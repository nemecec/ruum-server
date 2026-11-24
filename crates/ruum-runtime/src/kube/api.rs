//! A small client of the Kubernetes API for the driver: pods, ConfigMaps and Secrets of one
//! namespace, and pod logs. Plain JSON over `reqwest` (no Kubernetes client crate). The verbs it
//! uses are exactly the verbs of the driver's Role (`deploy/staging/charts/platform/templates/
//! runtime-rbac.yaml`); the fake API server of the tests refuses every other one.
//!
//! The answers of Secret writes hold the Secret's data, so they are never parsed, logged or put
//! into an error text: the client reads only their status code.

use ruum_core::runtime::RuntimeError;
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

/// The object kinds the driver touches. Each one is namespaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Pod,
    ConfigMap,
    Secret,
}

impl Kind {
    fn plural(self) -> &'static str {
        match self {
            Kind::Pod => "pods",
            Kind::ConfigMap => "configmaps",
            Kind::Secret => "secrets",
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

    /// An error for an answer that is not one of the expected ones. Secret answers never get
    /// their body read.
    async fn refused(&self, what: &str, kind: Kind, r: reqwest::Response) -> RuntimeError {
        let status = r.status();
        let msg = if kind == Kind::Secret {
            String::new()
        } else {
            status_message(&r.text().await.unwrap_or_default())
        };
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

    /// `GET` one object; `None` when it does not exist. Never called for Secrets.
    pub(crate) async fn get(&self, kind: Kind, name: &str) -> Result<Option<Value>, RuntimeError> {
        debug_assert!(kind != Kind::Secret, "the driver never reads a Secret");
        let what = format!("get {} {name}", kind.plural());
        let r = self
            .send(&what, self.client.get(self.url(kind, Some(name))))
            .await?;
        match r.status().as_u16() {
            200 => Ok(Some(self.json(&what, r).await?)),
            404 => Ok(None),
            _ => Err(self.refused(&what, kind, r).await),
        }
    }

    /// `GET` the objects with all of `labels`. Never called for Secrets.
    pub(crate) async fn list(
        &self,
        kind: Kind,
        labels: &[(&str, &str)],
    ) -> Result<Vec<Value>, RuntimeError> {
        debug_assert!(kind != Kind::Secret, "the driver never lists Secrets");
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
            return Err(self.refused(&what, kind, r).await);
        }
        let v = self.json(&what, r).await?;
        Ok(v.get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// `POST` a new object. The answer of a Secret is not read: `Done(Value::Null)`.
    pub(crate) async fn create(&self, kind: Kind, body: &Value) -> Result<Write, RuntimeError> {
        let what = format!("create {}", kind.plural());
        let r = self
            .send(&what, self.client.post(self.url(kind, None)).json(body))
            .await?;
        match r.status().as_u16() {
            200..=202 if kind == Kind::Secret => Ok(Write::Done(Value::Null)),
            200..=202 => Ok(Write::Done(self.json(&what, r).await?)),
            409 => Ok(Write::Conflict),
            _ => Err(self.refused(&what, kind, r).await),
        }
    }

    /// `PUT` an object with its `metadata.resourceVersion`: `Conflict` when another writer was
    /// first. Never called for Secrets.
    pub(crate) async fn replace(
        &self,
        kind: Kind,
        name: &str,
        body: &Value,
    ) -> Result<Write, RuntimeError> {
        debug_assert!(kind != Kind::Secret);
        let what = format!("update {} {name}", kind.plural());
        let r = self
            .send(
                &what,
                self.client.put(self.url(kind, Some(name))).json(body),
            )
            .await?;
        match r.status().as_u16() {
            200 | 201 => Ok(Write::Done(self.json(&what, r).await?)),
            409 => Ok(Write::Conflict),
            404 => Ok(Write::Gone),
            _ => Err(self.refused(&what, kind, r).await),
        }
    }

    /// A JSON merge patch. The answer is not read (it is a Secret's in the driver's one use).
    pub(crate) async fn merge_patch(
        &self,
        kind: Kind,
        name: &str,
        body: &Value,
    ) -> Result<Write, RuntimeError> {
        let what = format!("patch {} {name}", kind.plural());
        let r = self
            .send(
                &what,
                self.client
                    .patch(self.url(kind, Some(name)))
                    .header(
                        reqwest::header::CONTENT_TYPE,
                        "application/merge-patch+json",
                    )
                    .body(body.to_string()),
            )
            .await?;
        match r.status().as_u16() {
            200 | 201 => Ok(Write::Done(Value::Null)),
            404 => Ok(Write::Gone),
            409 => Ok(Write::Conflict),
            _ => Err(self.refused(&what, kind, r).await),
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
        let mut req = self.client.delete(self.url(kind, Some(name)));
        if let Some(g) = grace {
            req = req.json(&serde_json::json!({
                "apiVersion": "v1",
                "kind": "DeleteOptions",
                "gracePeriodSeconds": g,
                "propagationPolicy": "Background",
            }));
        }
        let r = self.send(&what, req).await?;
        match r.status().as_u16() {
            200 | 202 | 404 => Ok(()),
            _ => Err(self.refused(&what, kind, r).await),
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
            _ => Err(self.refused(&what, Kind::Pod, r).await),
        }
    }
}
