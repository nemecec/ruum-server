//! A small client of the Kubernetes API for the driver: pods and ConfigMaps of one namespace,
//! pod logs, and `TokenReview`s. Plain JSON over `reqwest` (no Kubernetes client crate). The
//! verbs it uses are exactly the verbs of the driver's Role and ClusterRole
//! (`deploy/staging/charts/platform/templates/runtime-rbac.yaml`); the fake API server of the
//! tests refuses every other one. No Secret: the driver has no verb on Secrets (backlog S-89).
//! Deletes always say `propagationPolicy: Background`.

use ruum_core::runtime::RuntimeError;
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

/// The object kinds the driver touches. Each one is namespaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Pod,
    ConfigMap,
}

impl Kind {
    fn plural(self) -> &'static str {
        match self {
            Kind::Pod => "pods",
            Kind::ConfigMap => "configmaps",
        }
    }
}

/// The answer to a write that can lose a race.
pub(crate) enum Write {
    Done(Value),
    /// `409`: the object exists (create) or its resourceVersion moved on (replace).
    Conflict,
    /// `404`: the object is gone.
    Gone,
}

/// A failed [`Api::replace_checked`].
pub(crate) struct ReplaceError {
    pub error: RuntimeError,
    /// The object surely was not written.
    pub not_written: bool,
}

/// The answer to a log request.
pub(crate) enum LogAnswer {
    Stream(reqwest::Response),
    /// `400`: the container has not started yet (or the pod is still pending).
    NotReady,
    /// `404`: the pod is gone.
    Gone,
}

pub(crate) struct Api {
    base: String,
    namespace: String,
    token_file: Option<PathBuf>,
    client: reqwest::Client,
    /// A client without a total timeout, for followed log streams.
    stream_client: reqwest::Client,
}

fn unavailable(what: &str, e: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Unavailable {
        reason: format!("the Kubernetes API ({what}): {e}"),
    }
}

/// The `message` of a Kubernetes `Status` answer, cut short; never used for Secret answers.
fn status_message(body: &str) -> String {
    let msg = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    let mut m: String = msg.chars().take(300).collect();
    if m.is_empty() {
        m.push_str("no message");
    }
    m
}

impl Api {
    pub(crate) fn new(
        base: &str,
        namespace: &str,
        token_file: Option<PathBuf>,
        ca_file: Option<&std::path::Path>,
        timeout: Duration,
    ) -> Result<Self, RuntimeError> {
        let mk = |total: Option<Duration>| -> Result<reqwest::Client, RuntimeError> {
            // No proxy of the environment: the API calls carry the server's bearer token.
            let mut b = reqwest::Client::builder()
                .no_proxy()
                .connect_timeout(Duration::from_secs(10));
            if let Some(t) = total {
                b = b.timeout(t);
            }
            if let Some(ca) = ca_file {
                let pem = std::fs::read(ca).map_err(|e| RuntimeError::Internal {
                    reason: format!("the Kubernetes CA file {}: {e}", ca.display()),
                })?;
                let cert =
                    reqwest::Certificate::from_pem(&pem).map_err(|e| RuntimeError::Internal {
                        reason: format!("the Kubernetes CA file {}: {e}", ca.display()),
                    })?;
                b = b.add_root_certificate(cert);
            }
            b.build().map_err(|e| RuntimeError::Internal {
                reason: format!("the Kubernetes client: {e}"),
            })
        };
        Ok(Self {
            base: base.trim_end_matches('/').to_owned(),
            namespace: namespace.to_owned(),
            token_file,
            client: mk(Some(timeout))?,
            stream_client: mk(None)?,
        })
    }

    fn url(&self, kind: Kind, name: Option<&str>) -> String {
        let mut u = format!(
            "{}/api/v1/namespaces/{}/{}",
            self.base,
            self.namespace,
            kind.plural()
        );
        if let Some(n) = name {
            u.push('/');
            u.push_str(n);
        }
        u
    }

    /// The bearer token, read again for every request: a projected token is renewed in place.
    fn auth(&self, req: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder, RuntimeError> {
        match &self.token_file {
            None => Ok(req),
            Some(p) => {
                let t = std::fs::read_to_string(p).map_err(|e| RuntimeError::Internal {
                    reason: format!("the Kubernetes token file {}: {e}", p.display()),
                })?;
                Ok(req.bearer_auth(t.trim()))
            }
        }
    }

    async fn send(
        &self,
        what: &str,
        req: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, RuntimeError> {
        self.auth(req)?
            .send()
            .await
            .map_err(|e| unavailable(what, e.without_url()))
    }

    /// An error for an answer that is not one of the expected ones.
    async fn refused(&self, what: &str, r: reqwest::Response) -> RuntimeError {
        let status = r.status();
        let msg = status_message(&r.text().await.unwrap_or_default());
        match status.as_u16() {
            401 | 403 => RuntimeError::Internal {
                reason: format!(
                    "the Kubernetes API refused {what} ({status}; the driver's Role) {msg}"
                ),
            },
            400 | 422 => RuntimeError::Internal {
                reason: format!("the Kubernetes API refused {what} ({status}) {msg}"),
            },
            _ => unavailable(what, format!("{status} {msg}")),
        }
    }

    async fn json(&self, what: &str, r: reqwest::Response) -> Result<Value, RuntimeError> {
        r.json::<Value>()
            .await
            .map_err(|e| unavailable(what, e.without_url()))
    }

    /// `GET` one object; `None` when it does not exist.
    pub(crate) async fn get(&self, kind: Kind, name: &str) -> Result<Option<Value>, RuntimeError> {
        let what = format!("get {} {name}", kind.plural());
        let r = self
            .send(&what, self.client.get(self.url(kind, Some(name))))
            .await?;
        match r.status().as_u16() {
            200 => Ok(Some(self.json(&what, r).await?)),
            404 => Ok(None),
            _ => Err(self.refused(&what, r).await),
        }
    }

    /// `GET` the objects with all of `labels`.
    pub(crate) async fn list(
        &self,
        kind: Kind,
        labels: &[(&str, &str)],
    ) -> Result<Vec<Value>, RuntimeError> {
        let what = format!("list {}", kind.plural());
        let selector: Vec<String> = labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let r = self
            .send(
                &what,
                self.client
                    .get(self.url(kind, None))
                    .query(&[("labelSelector", selector.join(","))]),
            )
            .await?;
        if r.status().as_u16() != 200 {
            return Err(self.refused(&what, r).await);
        }
        let v = self.json(&what, r).await?;
        Ok(v.get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// `POST` a new object.
    pub(crate) async fn create(&self, kind: Kind, body: &Value) -> Result<Write, RuntimeError> {
        let what = format!("create {}", kind.plural());
        let r = self
            .send(&what, self.client.post(self.url(kind, None)).json(body))
            .await?;
        match r.status().as_u16() {
            200..=202 => Ok(Write::Done(self.json(&what, r).await?)),
            409 => Ok(Write::Conflict),
            _ => Err(self.refused(&what, r).await),
        }
    }

    /// `PUT` an object with its `metadata.resourceVersion`: `Conflict` when another writer was
    /// first.
    pub(crate) async fn replace(
        &self,
        kind: Kind,
        name: &str,
        body: &Value,
    ) -> Result<Write, RuntimeError> {
        self.replace_checked(kind, name, body)
            .await
            .map_err(|e| e.error)
    }

    /// [`Api::replace`], and on an error whether the object surely was not written: the request
    /// never reached the API server (a connect error), or the API server answered with a status
    /// that writes nothing (`400`, `401`, `403`, `413`, `415`, `422`, `429`, `503`). A timeout,
    /// a cut answer, `500`, `502` and `504` may hide a write that went through.
    pub(crate) async fn replace_checked(
        &self,
        kind: Kind,
        name: &str,
        body: &Value,
    ) -> Result<Write, ReplaceError> {
        let what = format!("update {} {name}", kind.plural());
        let req = self
            .auth(self.client.put(self.url(kind, Some(name))).json(body))
            .map_err(|error| ReplaceError {
                error,
                not_written: true,
            })?;
        let r = req.send().await.map_err(|e| ReplaceError {
            not_written: e.is_connect(),
            error: unavailable(&what, e.without_url()),
        })?;
        match r.status().as_u16() {
            200 | 201 => self
                .json(&what, r)
                .await
                .map(Write::Done)
                .map_err(|error| ReplaceError {
                    error,
                    not_written: false,
                }),
            409 => Ok(Write::Conflict),
            404 => Ok(Write::Gone),
            code => Err(ReplaceError {
                not_written: matches!(code, 400 | 401 | 403 | 413 | 415 | 422 | 429 | 503),
                error: self.refused(&what, r).await,
            }),
        }
    }

    /// `DELETE` an object; a missing one is fine. `grace` is the pod's termination grace.
    pub(crate) async fn delete(
        &self,
        kind: Kind,
        name: &str,
        grace: Option<u64>,
    ) -> Result<(), RuntimeError> {
        let what = format!("delete {} {name}", kind.plural());
        let mut opts = serde_json::json!({
            "apiVersion": "v1",
            "kind": "DeleteOptions",
            "propagationPolicy": "Background",
        });
        if let Some(g) = grace {
            opts["gracePeriodSeconds"] = serde_json::json!(g);
        }
        let req = self.client.delete(self.url(kind, Some(name))).json(&opts);
        let r = self.send(&what, req).await?;
        match r.status().as_u16() {
            200 | 202 | 404 => Ok(()),
            _ => Err(self.refused(&what, r).await),
        }
    }

    /// `POST /apis/authentication.k8s.io/v1/tokenreviews`: asks the API server whether `token`
    /// is valid for `audience`, and whose it is. The answer's `status`; the token is never
    /// logged or put into an error text.
    pub(crate) async fn token_review(
        &self,
        token: &str,
        audience: &str,
    ) -> Result<Value, RuntimeError> {
        let what = "create tokenreviews";
        let body = serde_json::json!({
            "apiVersion": "authentication.k8s.io/v1",
            "kind": "TokenReview",
            "spec": { "token": token, "audiences": [audience] },
        });
        let url = format!("{}/apis/authentication.k8s.io/v1/tokenreviews", self.base);
        let r = self.send(what, self.client.post(url).json(&body)).await?;
        match r.status().as_u16() {
            200 | 201 => Ok(self
                .json(what, r)
                .await?
                .get("status")
                .cloned()
                .unwrap_or_default()),
            _ => Err(self.refused(what, r).await),
        }
    }

    /// Follows the log of the container `app` of a pod: one stream (`Stdout` or `Stderr`, which
    /// needs the API server's split streams) or both together (`None`). `from_now` skips what was
    /// written before (a reconnect).
    pub(crate) async fn follow_log(
        &self,
        pod: &str,
        stream: Option<&str>,
        from_now: bool,
    ) -> Result<LogAnswer, RuntimeError> {
        let what = format!("log of {pod}");
        let mut q: Vec<(&str, String)> = vec![
            ("container", "app".to_owned()),
            ("follow", "true".to_owned()),
        ];
        if let Some(s) = stream {
            q.push(("stream", s.to_owned()));
        }
        if from_now {
            q.push(("tailLines", "0".to_owned()));
        }
        let url = format!("{}/log", self.url(Kind::Pod, Some(pod)));
        let r = self
            .send(&what, self.stream_client.get(url).query(&q))
            .await?;
        match r.status().as_u16() {
            200 => Ok(LogAnswer::Stream(r)),
            400 => Ok(LogAnswer::NotReady),
            404 => Ok(LogAnswer::Gone),
            _ => Err(self.refused(&what, r).await),
        }
    }
}
