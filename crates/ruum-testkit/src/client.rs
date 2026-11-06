//! An HTTP test client for the API router: builds an authenticated request for any principal and
//! sends it through the router in memory (no network).

use crate::fixtures::{Credentials, Principal};
use axum::Router;
use axum::body::Body as AxumBody;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use http_body_util::BodyExt;
use ruum_api::AppState;
use ruum_core::TenantId;
use serde_json::Value;
use tower::ServiceExt;
use tower_http::catch_panic::CatchPanicLayer;

/// The body of a request.
#[derive(Clone, Debug, Default)]
pub enum Body {
    #[default]
    Empty,
    Json(Value),
    /// `application/x-www-form-urlencoded`, already encoded.
    Form(String),
    Bytes(Vec<u8>),
}

/// A request before it gets the credentials of a principal.
#[derive(Clone, Debug)]
pub struct RequestSpec {
    pub method: Method,
    /// The path with the query, for example `/v1/nodes/<id>/children?limit=10`.
    pub uri: String,
    /// The value of the `Ruum-Tenant` header, if any.
    pub tenant: Option<TenantId>,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

impl RequestSpec {
    pub fn new(method: Method, uri: impl Into<String>) -> Self {
        Self {
            method,
            uri: uri.into(),
            tenant: None,
            headers: Vec::new(),
            body: Body::Empty,
        }
    }

    pub fn get(uri: impl Into<String>) -> Self {
        Self::new(Method::GET, uri)
    }

    pub fn post(uri: impl Into<String>) -> Self {
        Self::new(Method::POST, uri)
    }

    pub fn tenant(mut self, t: TenantId) -> Self {
        self.tenant = Some(t);
        self
    }

    /// Sets or removes the `Ruum-Tenant` header.
    pub fn with_tenant(mut self, t: Option<TenantId>) -> Self {
        self.tenant = t;
        self
    }

    pub fn json(mut self, v: Value) -> Self {
        self.body = Body::Json(v);
        self
    }

    pub fn form(mut self, s: impl Into<String>) -> Self {
        self.body = Body::Form(s.into());
        self
    }

    pub fn bytes(mut self, b: Vec<u8>) -> Self {
        self.body = Body::Bytes(b);
        self
    }

    pub fn header(mut self, k: &str, v: impl Into<String>) -> Self {
        self.headers.push((k.to_string(), v.into()));
        self
    }

    /// All the text that the caller sends: the URI, header values and body. The leak checks do
    /// not count a marker that the caller sent itself.
    pub fn sent_text(&self) -> String {
        let mut s = self.uri.clone();
        if let Some(t) = self.tenant {
            s.push(' ');
            s.push_str(&t.to_string());
        }
        for (_, v) in &self.headers {
            s.push(' ');
            s.push_str(v);
        }
        match &self.body {
            Body::Empty => {}
            Body::Json(v) => s.push_str(&v.to_string()),
            Body::Form(f) => s.push_str(f),
            Body::Bytes(b) => s.push_str(&String::from_utf8_lossy(b)),
        }
        s
    }

    /// The method and path, for reports.
    pub fn label(&self) -> String {
        format!("{} {}", self.method, self.uri)
    }
}

/// A response, fully read.
#[derive(Clone, Debug)]
pub struct Response {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Response {
    pub fn json(&self) -> Option<Value> {
        serde_json::from_slice(&self.body).ok()
    }

    /// The `error.code` of the standard error body, if there is one.
    pub fn error_code(&self) -> Option<String> {
        self.json()
            .and_then(|v| v["error"]["code"].as_str().map(str::to_string))
    }

    /// The body and all header values as text, for the leak checks.
    pub fn text(&self) -> String {
        let mut s = String::from_utf8_lossy(&self.body).into_owned();
        for v in self.headers.values() {
            s.push('\n');
            s.push_str(&String::from_utf8_lossy(v.as_bytes()));
        }
        s
    }

    /// True for `501 not_implemented`: the handler is not built yet.
    pub fn is_not_implemented(&self) -> bool {
        self.status == StatusCode::NOT_IMPLEMENTED
    }
}

/// The router of `ruum-api` with a guard that turns a panic into `500`, so one broken handler
/// fails its case instead of the whole suite.
pub fn app(state: AppState) -> Router {
    ruum_api::router(state).layer(CatchPanicLayer::new())
}

/// Builds the HTTP request for `spec` with the credentials of `who`.
pub fn build(spec: &RequestSpec, who: &Principal) -> Request<AxumBody> {
    let mut b = Request::builder()
        .method(spec.method.clone())
        .uri(&spec.uri);
    if let Some(t) = spec.tenant {
        b = b.header(ruum_api::TENANT_HEADER, t.to_string());
    }
    match &who.credentials {
        Credentials::None => {}
        Credentials::Bearer(t) => b = b.header("authorization", format!("Bearer {t}")),
        Credentials::Session { session, csrf } => {
            b = b
                .header(
                    "cookie",
                    format!("ruum_session={session}; ruum_csrf={csrf}"),
                )
                .header("ruum-csrf", csrf.as_str());
        }
    }
    for (k, v) in &spec.headers {
        b = b.header(k.as_str(), v.as_str());
    }
    let body = match &spec.body {
        Body::Empty => AxumBody::empty(),
        Body::Json(v) => {
            b = b.header("content-type", "application/json");
            AxumBody::from(v.to_string())
        }
        Body::Form(f) => {
            b = b.header("content-type", "application/x-www-form-urlencoded");
            AxumBody::from(f.clone())
        }
        Body::Bytes(bytes) => {
            if !spec
                .headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            {
                b = b.header("content-type", "application/octet-stream");
            }
            AxumBody::from(bytes.clone())
        }
    };
    b.body(body).expect("a valid test request")
}

/// Sends `spec` as `who` through `app`.
pub async fn send(app: &Router, spec: &RequestSpec, who: &Principal) -> Response {
    let res = app
        .clone()
        .oneshot(build(spec, who))
        .await
        .expect("the router is infallible");
    let status = res.status();
    let headers = res.headers().clone();
    let body = res
        .into_body()
        .collect()
        .await
        .map(|b| b.to_bytes().to_vec())
        .unwrap_or_default();
    Response {
        status,
        headers,
        body,
    }
}

/// The answer of the router to a route that does not exist. A probe that gets the same answer hit
/// no handler, so the route of its operation is missing.
pub async fn fallback_fingerprint(app: &Router, anyone: &Principal) -> Response {
    send(
        app,
        &RequestSpec::get("/__ruum_testkit__/no/such/route"),
        anyone,
    )
    .await
}

/// Whether `res` is the router's fallback (or `405`): no handler for this method and path.
pub fn is_unrouted(res: &Response, fallback: &Response) -> bool {
    res.status == StatusCode::METHOD_NOT_ALLOWED
        || (res.status == fallback.status && res.body == fallback.body)
}
