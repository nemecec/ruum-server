//! The push of a build (card R-2b, "The push"): the OCI distribution API over `reqwest`, to the
//! repository of the build's tenant and app only ([`crate::repository`]).
//!
//! A [`RepositoryPush`] is made for one build and one repository: its methods take no
//! repository, so no caller can name another one. It writes blobs (a cross-repository mount of
//! the base layers first, an upload when the registry refuses the mount), then the manifest **by
//! digest**, and it refuses a second manifest (push once). An upload address (`Location`) that
//! the registry gives must stay on the registry and under the build's own repository, or the
//! push stops. It reads the base image from the base repository (pull only).
//!
//! Credentials: with a [`RegistryCredential`], a registry that answers `401` with a `Bearer`
//! challenge (token auth, I-4) gets one token request for the scopes
//! `repository:<repo>:push,pull` and `repository:<base>:pull`, with the credential as HTTP
//! basic auth; a `Basic` challenge gets the credential directly. The credential lives only in
//! this client: never in the build job (no env, no file, no label).

use bytes::Bytes;
use ruum_core::runtime::{OciDigest, RuntimeError, SecretValue};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The media types the builder reads and writes.
pub const OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";
pub const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
pub const DOCKER_LIST: &str = "application/vnd.docker.distribution.manifest.list.v2+json";
pub const DOCKER_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";

/// The largest manifest or config the builder reads (an OCI manifest is a few KiB).
pub const MAX_MANIFEST_BYTES: usize = 4 * 1024 * 1024;

/// The builder's registry credential (I-4 issues one scoped to pushes).
#[derive(Clone)]
pub struct RegistryCredential {
    pub username: String,
    pub password: SecretValue,
}

impl std::fmt::Debug for RegistryCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryCredential")
            .field("username", &self.username)
            .field("password", &"<hidden>")
            .finish()
    }
}

#[derive(Clone)]
enum Auth {
    None,
    Basic,
    Bearer(String),
}

fn unavailable(what: &str, e: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Unavailable {
        reason: format!("the registry ({what}): {e}"),
    }
}

/// The push of one build to one repository.
pub struct RepositoryPush {
    http: reqwest::Client,
    /// `http://host:port`, no trailing slash.
    base: String,
    repo: String,
    base_repo: String,
    credential: Option<RegistryCredential>,
    auth: Mutex<Auth>,
    manifest_pushed: AtomicBool,
}

impl RepositoryPush {
    /// A push to `repo` (always [`crate::repository`] of the build's identity) that reads the
    /// base image from `base_repo`.
    pub fn new(
        registry_url: &str,
        repo: String,
        base_repo: String,
        credential: Option<RegistryCredential>,
    ) -> Result<Self, RuntimeError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .connect_timeout(Duration::from_secs(10))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| RuntimeError::Internal {
                reason: format!("the registry client: {e}"),
            })?;
        Ok(Self {
            http,
            base: registry_url.trim_end_matches('/').to_owned(),
            repo,
            base_repo,
            credential,
            auth: Mutex::new(Auth::None),
            manifest_pushed: AtomicBool::new(false),
        })
    }

    /// The repository this push writes to.
    pub fn repository(&self) -> &str {
        &self.repo
    }

    fn url(&self, repo: &str, rest: &str) -> String {
        format!("{}/v2/{repo}/{rest}", self.base)
    }

    fn authorize(&self, b: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let auth = self.auth.lock().unwrap_or_else(|p| p.into_inner()).clone();
        match (auth, &self.credential) {
            (Auth::Bearer(t), _) => b.bearer_auth(t),
            (Auth::Basic, Some(c)) => b.basic_auth(&c.username, Some(c.password.expose())),
            _ => b,
        }
    }

    /// Sends a request; on a `401` with a challenge it authenticates once and sends it again.
    /// `make` builds the request (it may be called twice).
    async fn send(
        &self,
        what: &str,
        make: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, RuntimeError> {
        let r = self
            .authorize(make())
            .send()
            .await
            .map_err(|e| unavailable(what, e.without_url()))?;
        if r.status() != reqwest::StatusCode::UNAUTHORIZED {
            return Ok(r);
        }
        let challenge = r
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        self.login(&challenge).await?;
        self.authorize(make())
            .send()
            .await
            .map_err(|e| unavailable(what, e.without_url()))
    }

    async fn login(&self, challenge: &str) -> Result<(), RuntimeError> {
        let Some(cred) = &self.credential else {
            return Err(unavailable("auth", "the registry wants a credential"));
        };
        let lower = challenge.to_ascii_lowercase();
        if lower.starts_with("basic") {
            *self.auth.lock().unwrap_or_else(|p| p.into_inner()) = Auth::Basic;
            return Ok(());
        }
        if !lower.starts_with("bearer") {
            return Err(unavailable("auth", "an unknown challenge"));
        }
        let param = |name: &str| -> Option<String> {
            let key = format!("{name}=\"");
            let at = challenge.find(&key)? + key.len();
            let end = challenge[at..].find('"')?;
            Some(challenge[at..at + end].to_owned())
        };
        let realm = param("realm").ok_or_else(|| unavailable("auth", "no realm"))?;
        // The token service must be the registry's own origin or an https service; a realm that
        // points elsewhere over plain http would receive the credential in clear.
        if !(realm.starts_with("https://") || realm.starts_with(&format!("{}/", self.base))) {
            return Err(unavailable("auth", "the token realm is not trusted"));
        }
        let mut query: Vec<(&str, String)> = vec![
            ("scope", format!("repository:{}:push,pull", self.repo)),
            ("scope", format!("repository:{}:pull", self.base_repo)),
        ];
        if let Some(s) = param("service") {
            query.push(("service", s));
        }
        #[derive(serde::Deserialize)]
        struct Token {
            token: Option<String>,
            access_token: Option<String>,
        }
        let t: Token = self
            .http
            .get(&realm)
            .query(&query)
            .basic_auth(&cred.username, Some(cred.password.expose()))
            .send()
            .await
            .map_err(|e| unavailable("token", e.without_url()))?
            .error_for_status()
            .map_err(|e| unavailable("token", e.without_url()))?
            .json()
            .await
            .map_err(|e| unavailable("token", e.without_url()))?;
        let token = t
            .token
            .or(t.access_token)
            .ok_or_else(|| unavailable("token", "no token in the answer"))?;
        *self.auth.lock().unwrap_or_else(|p| p.into_inner()) = Auth::Bearer(token);
        Ok(())
    }

    /// Reads a manifest (or index) of the base repository by digest, checked against it.
    pub async fn base_manifest(&self, digest: &OciDigest) -> Result<(String, Bytes), RuntimeError> {
        let url = self.url(&self.base_repo, &format!("manifests/{digest}"));
        let accept = format!("{OCI_INDEX}, {OCI_MANIFEST}, {DOCKER_LIST}, {DOCKER_MANIFEST}");
        let r = self
            .send("base manifest", || {
                self.http.get(&url).header(reqwest::header::ACCEPT, &accept)
            })
            .await?;
        if r.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(RuntimeError::Unavailable {
                reason: format!(
                    "the base image {}@{digest} is not in the local registry (mirror it there)",
                    self.base_repo
                ),
            });
        }
        if !r.status().is_success() {
            return Err(unavailable("base manifest", r.status()));
        }
        let media = r
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let body = read_capped(r, MAX_MANIFEST_BYTES).await?;
        if OciDigest::of(&body) != *digest {
            return Err(unavailable("base manifest", "the digest does not match"));
        }
        Ok((media, body))
    }

    /// Reads a small blob (the base config) of the base repository, checked against its digest.
    pub async fn base_blob(&self, digest: &OciDigest) -> Result<Bytes, RuntimeError> {
        let url = self.url(&self.base_repo, &format!("blobs/{digest}"));
        let r = self.send("base blob", || self.http.get(&url)).await?;
        if !r.status().is_success() {
            return Err(unavailable("base blob", r.status()));
        }
        let body = read_capped(r, MAX_MANIFEST_BYTES).await?;
        if OciDigest::of(&body) != *digest {
            return Err(unavailable("base blob", "the digest does not match"));
        }
        Ok(body)
    }

    /// Whether the build's repository has the blob.
    async fn has_blob(&self, digest: &OciDigest) -> Result<bool, RuntimeError> {
        let url = self.url(&self.repo, &format!("blobs/{digest}"));
        let r = self.send("blob check", || self.http.head(&url)).await?;
        Ok(r.status().is_success())
    }

    /// Checks an upload address: on the registry, under `/v2/<repo>/blobs/uploads/`. A
    /// relative address is joined to the registry.
    fn upload_url(&self, location: &str) -> Result<String, RuntimeError> {
        let abs = if location.starts_with('/') {
            format!("{}{location}", self.base)
        } else {
            location.to_owned()
        };
        let prefix = format!("{}/v2/{}/blobs/uploads/", self.base, self.repo);
        if !abs.starts_with(&prefix) || abs.contains("/../") || abs.contains('#') {
            return Err(RuntimeError::Internal {
                reason: "the registry gave an upload address outside the build's repository"
                    .to_owned(),
            });
        }
        Ok(abs)
    }

    fn location(r: &reqwest::Response) -> Result<String, RuntimeError> {
        r.headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .ok_or_else(|| unavailable("upload", "no upload address"))
    }

    /// Finishes an upload session with the whole blob.
    async fn finish_upload(
        &self,
        location: &str,
        digest: &OciDigest,
        body: impl Fn() -> reqwest::Body,
    ) -> Result<(), RuntimeError> {
        let upload = self.upload_url(location)?;
        let sep = if upload.contains('?') { '&' } else { '?' };
        let url = format!("{upload}{sep}digest={digest}");
        let r = self
            .send("blob upload", || {
                self.http
                    .put(&url)
                    .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                    .body(body())
            })
            .await?;
        if r.status() != reqwest::StatusCode::CREATED {
            return Err(unavailable("blob upload", r.status()));
        }
        Ok(())
    }

    /// Writes a blob of the build (its layer or config) to the build's repository.
    pub async fn push_blob(&self, digest: &OciDigest, data: Bytes) -> Result<(), RuntimeError> {
        if self.has_blob(digest).await? {
            return Ok(());
        }
        let url = self.url(&self.repo, "blobs/uploads/");
        let r = self.send("blob upload", || self.http.post(&url)).await?;
        if r.status() != reqwest::StatusCode::ACCEPTED {
            return Err(unavailable("blob upload", r.status()));
        }
        let location = Self::location(&r)?;
        self.finish_upload(&location, digest, || reqwest::Body::from(data.clone()))
            .await
    }

    /// Makes a base layer available in the build's repository: a cross-repository mount from
    /// the base repository, or (when the registry refuses the mount) a copy of the blob.
    pub async fn bring_base_blob(&self, digest: &OciDigest) -> Result<(), RuntimeError> {
        if self.has_blob(digest).await? {
            return Ok(());
        }
        let url = self.url(&self.repo, "blobs/uploads/");
        let query = [
            ("mount", digest.to_string()),
            ("from", self.base_repo.clone()),
        ];
        let r = self
            .send("blob mount", || self.http.post(&url).query(&query))
            .await?;
        match r.status() {
            reqwest::StatusCode::CREATED => Ok(()),
            reqwest::StatusCode::ACCEPTED => {
                let location = Self::location(&r)?;
                let src = self.url(&self.base_repo, &format!("blobs/{digest}"));
                let got = self.send("base layer", || self.http.get(&src)).await?;
                if !got.status().is_success() {
                    return Err(unavailable("base layer", got.status()));
                }
                // A base layer is the platform's own (tens of MiB): read it, check it, write it.
                let data = got
                    .bytes()
                    .await
                    .map_err(|e| unavailable("base layer", e.without_url()))?;
                if OciDigest::of(&data) != *digest {
                    return Err(unavailable("base layer", "the digest does not match"));
                }
                self.finish_upload(&location, digest, || reqwest::Body::from(data.clone()))
                    .await
            }
            s => Err(unavailable("blob mount", s)),
        }
    }

    /// Writes the image manifest by its digest. Once per build: a second call is refused.
    pub async fn push_manifest(
        &self,
        media_type: &str,
        manifest: Bytes,
    ) -> Result<OciDigest, RuntimeError> {
        if self.manifest_pushed.swap(true, Ordering::SeqCst) {
            return Err(RuntimeError::Internal {
                reason: "the build already pushed its image (a build pushes once)".to_owned(),
            });
        }
        let digest = OciDigest::of(&manifest);
        let url = self.url(&self.repo, &format!("manifests/{digest}"));
        let r = self
            .send("manifest", || {
                self.http
                    .put(&url)
                    .header(reqwest::header::CONTENT_TYPE, media_type)
                    .body(manifest.clone())
            })
            .await?;
        if r.status() != reqwest::StatusCode::CREATED {
            return Err(unavailable("manifest", r.status()));
        }
        Ok(digest)
    }
}

async fn read_capped(r: reqwest::Response, cap: usize) -> Result<Bytes, RuntimeError> {
    use futures::StreamExt;
    if r.content_length().is_some_and(|n| n > cap as u64) {
        return Err(unavailable("read", "the answer is too large"));
    }
    let mut out = Vec::new();
    let mut s = r.bytes_stream();
    while let Some(chunk) = s.next().await {
        let chunk = chunk.map_err(|e| unavailable("read", e.without_url()))?;
        if out.len() + chunk.len() > cap {
            return Err(unavailable("read", "the answer is too large"));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(out))
}

#[cfg(test)]
mod tests {
    //! Acceptance 5 of R-2b against a recording stand-in registry (`wiremock`): the push writes
    //! only under `/v2/<repo>/`, pushes its manifest once, and refuses an upload address outside
    //! its repository.
    use super::*;
    use wiremock::matchers::{method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const REPO: &str =
        "ruum/11111111-1111-1111-1111-111111111111/22222222-2222-2222-2222-222222222222";

    async fn registry(location: &str) -> MockServer {
        let s = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path_regex("^/v2/.*/blobs/.*$"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&s)
            .await;
        Mock::given(method("POST"))
            .and(path_regex("^/v2/.*/blobs/uploads/$"))
            .respond_with(ResponseTemplate::new(202).insert_header("Location", location))
            .mount(&s)
            .await;
        Mock::given(method("PUT"))
            .and(path_regex("^/v2/.*/blobs/uploads/.*$"))
            .respond_with(ResponseTemplate::new(201))
            .mount(&s)
            .await;
        Mock::given(method("PUT"))
            .and(path_regex("^/v2/.*/manifests/.*$"))
            .respond_with(ResponseTemplate::new(201))
            .mount(&s)
            .await;
        Mock::given(method("GET"))
            .and(path_regex("^/v2/ruum-base/node22/blobs/.*$"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"base layer".to_vec()))
            .mount(&s)
            .await;
        s
    }

    fn push(s: &MockServer) -> RepositoryPush {
        RepositoryPush::new(
            &s.uri(),
            REPO.to_owned(),
            "ruum-base/node22".to_owned(),
            Some(RegistryCredential {
                username: "builder".to_owned(),
                password: SecretValue::new("canary-push-password".to_owned()),
            }),
        )
        .expect("client")
    }

    #[tokio::test]
    async fn the_push_writes_only_its_repository_and_once() {
        let s = registry(&format!("/v2/{REPO}/blobs/uploads/abc")).await;
        let p = push(&s);
        let layer = Bytes::from_static(b"layer");
        p.push_blob(&OciDigest::of(&layer), layer)
            .await
            .expect("blob");
        p.bring_base_blob(&OciDigest::of(b"base layer"))
            .await
            .expect("base blob (mount refused, copied)");
        p.push_manifest(OCI_MANIFEST, Bytes::from_static(b"{}"))
            .await
            .expect("manifest");
        let again = p
            .push_manifest(OCI_MANIFEST, Bytes::from_static(b"{}"))
            .await;
        assert!(again.is_err(), "a second manifest push is refused");
        let reqs = s.received_requests().await.expect("recorded");
        let writes: Vec<_> = reqs
            .iter()
            .filter(|r| r.method.as_str() != "GET" && r.method.as_str() != "HEAD")
            .collect();
        assert!(!writes.is_empty());
        for r in &writes {
            assert!(
                r.url.path().starts_with(&format!("/v2/{REPO}/")),
                "a write outside the build's repository: {} {}",
                r.method,
                r.url.path()
            );
        }
        let manifests = writes
            .iter()
            .filter(|r| r.url.path().contains("/manifests/"))
            .count();
        assert_eq!(manifests, 1, "one manifest push");
        // Reads go to the build's repository or the base repository only.
        for r in &reqs {
            let p = r.url.path();
            assert!(
                p.starts_with(&format!("/v2/{REPO}/")) || p.starts_with("/v2/ruum-base/node22/"),
                "{p}"
            );
        }
    }

    #[tokio::test]
    async fn an_upload_address_outside_the_repository_is_refused() {
        let s = registry("/v2/ruum/other/app/blobs/uploads/abc").await;
        let p = push(&s);
        let layer = Bytes::from_static(b"layer");
        let r = p.push_blob(&OciDigest::of(&layer), layer).await;
        assert!(r.is_err(), "the push follows the registry elsewhere");
        let reqs = s.received_requests().await.expect("recorded");
        assert!(
            !reqs.iter().any(|r| r.url.path().contains("/ruum/other/")),
            "a request went to another repository"
        );
    }

    #[tokio::test]
    async fn a_token_is_scoped_to_the_repository() {
        let s = MockServer::start().await;
        let realm = format!("{}/token", s.uri());
        Mock::given(method("GET"))
            .and(path_regex("^/token$"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"token":"t0k"}"#))
            .mount(&s)
            .await;
        Mock::given(method("HEAD"))
            .and(path_regex("^/v2/.*/blobs/.*$"))
            .respond_with(ResponseTemplate::new(401).insert_header(
                "WWW-Authenticate",
                format!("Bearer realm=\"{realm}\",service=\"reg\"").as_str(),
            ))
            .mount(&s)
            .await;
        let p = push(&s);
        let _ = p.has_blob(&OciDigest::of(b"x")).await;
        let reqs = s.received_requests().await.expect("recorded");
        let token = reqs
            .iter()
            .find(|r| r.url.path() == "/token")
            .expect("a token request");
        let scopes: Vec<String> = token
            .url
            .query_pairs()
            .filter(|(k, _)| k == "scope")
            .map(|(_, v)| v.into_owned())
            .collect();
        assert_eq!(
            scopes,
            vec![
                format!("repository:{REPO}:push,pull"),
                "repository:ruum-base/node22:pull".to_owned()
            ]
        );
    }
}
