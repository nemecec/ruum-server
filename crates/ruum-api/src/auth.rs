//! Authentication of requests: the session cookie of the web UI (with the CSRF double submit),
//! or a bearer token (a Ruum access token of a user, an agent or a share link). The extractors
//! turn this, the `Ruum-Tenant` header and the `Idempotency-Key` header into a
//! [`ruum_core::Context`].
//!
//! - [`RequestContext`]: the caller in one tenant (the tenant of the token, or the `Ruum-Tenant`
//!   header of a user who is a member or guest there). Use it for node, upload, tag, agent,
//!   change, search and app password calls.
//! - [`Principal`]: the caller without a tenant (`getMe`, `listTenants`, ...). Use
//!   [`tenant_context`] for the `/v1/tenants/{tenant_id}` routes.
//! - [`MaybePrincipal`]: the caller if signed in, for public routes that act on a session.
//!
//! A missing or invalid credential is `401 unauthenticated` with
//! `WWW-Authenticate: Bearer resource_metadata="<origin>/.well-known/oauth-protected-resource"`,
//! so MCP clients find the authorization server (RFC 9728).
//!
//! Owned by the identity + tenants + agents area.

use crate::error::render;
use crate::state::AppState;
use crate::{IDEMPOTENCY_HEADER, TENANT_HEADER};
use axum::extract::{FromRequestParts, OptionalFromRequestParts};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, header};
use axum::response::{IntoResponse, Response};
use ruum_core::identity::{
    Authenticated, CSRF_COOKIE, CSRF_HEADER, SESSION_COOKIE, auth, unauthenticated,
};
use ruum_core::{ApiError, Context, Error, ErrorCode, TenantId};

/// The context of an authenticated request in one tenant.
pub struct RequestContext(pub Context);

/// The authenticated caller, before a tenant is chosen.
pub struct Principal(pub Authenticated);

/// The caller if the request has valid credentials; `None` without credentials. Invalid
/// credentials are still refused with `401`.
pub struct MaybePrincipal(pub Option<Authenticated>);

/// The rejection of the extractors: the standard error JSON, with `WWW-Authenticate` on `401`.
pub struct AuthRejection {
    error: Error,
    resource_metadata: String,
}

impl IntoResponse for AuthRejection {
    fn into_response(self) -> Response {
        let api = self.error.to_api();
        let unauthorized = api.code.http_status() == 401;
        let mut res = render(api);
        if unauthorized
            && let Ok(v) = HeaderValue::from_str(&www_authenticate(&self.resource_metadata))
        {
            res.headers_mut().insert(header::WWW_AUTHENTICATE, v);
        }
        res
    }
}

/// The `WWW-Authenticate` value of a `401`.
pub fn www_authenticate(resource_metadata: &str) -> String {
    format!("Bearer resource_metadata=\"{resource_metadata}\"")
}

/// The URL of the protected resource metadata of this API.
pub fn resource_metadata_url(state: &AppState) -> String {
    format!(
        "{}/.well-known/oauth-protected-resource",
        state.config.public_url.trim_end_matches('/')
    )
}

impl AuthRejection {
    fn new(state: &AppState, error: Error) -> Self {
        Self {
            error,
            resource_metadata: resource_metadata_url(state),
        }
    }
}

/// The value of one cookie of the request.
pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.trim().to_string())
}

/// The `Ruum-Tenant` header, if any. A value that is not a UUID is `bad_request`.
pub fn tenant_header(headers: &HeaderMap) -> Result<Option<TenantId>, Error> {
    match headers.get(TENANT_HEADER) {
        None => Ok(None),
        Some(v) => v
            .to_str()
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .map(Some)
            .ok_or_else(|| {
                Error::Api(
                    ApiError::new(
                        ErrorCode::BadRequest,
                        "the Ruum-Tenant header is not a UUID",
                    )
                    .with_hint("send the tenant ID from GET /v1/tenants"),
                )
            }),
    }
}

/// The `Idempotency-Key` header, if any.
pub fn idempotency_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get(IDEMPOTENCY_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn is_safe(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// Authenticates the request: a bearer token first, then the session cookie. `Ok(None)` when
/// the request has no credentials.
pub async fn authenticate(
    state: &AppState,
    method: &Method,
    headers: &HeaderMap,
) -> Result<Option<Authenticated>, Error> {
    if let Some(value) = headers.get(header::AUTHORIZATION) {
        let value = value.to_str().unwrap_or_default().trim();
        let token = value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| {
                unauthenticated(
                    "the Authorization header must be `Bearer <token>`",
                    "send a Ruum access token from POST /v1/oauth/token",
                )
            })?;
        let audience = state.services.tokens.audience().to_string();
        return auth::authenticate_bearer(&state.services, token, &audience)
            .await
            .map(Some);
    }
    let Some(secret) = cookie(headers, SESSION_COOKIE).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let who = auth::authenticate_session(&state.services, &secret).await?;
    if !is_safe(method) {
        let header_value = headers
            .get(CSRF_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        let cookie_value = cookie(headers, CSRF_COOKIE).unwrap_or_default();
        if header_value.is_empty()
            || !ruum_core::identity::secret::constant_time_eq(
                header_value.as_bytes(),
                cookie_value.as_bytes(),
            )
        {
            return Err(Error::Api(
                ApiError::new(ErrorCode::PermissionDenied, "the CSRF check failed")
                    .with_hint("send the value of the ruum_csrf cookie in the Ruum-Csrf header"),
            ));
        }
    }
    Ok(Some(who))
}

/// The context of `who` in `tenant` (a path parameter such as `/v1/tenants/{tenant_id}`).
/// Credentials bound to another tenant are refused; a user must be a member or guest there.
pub async fn tenant_context(
    state: &AppState,
    who: &Authenticated,
    tenant: TenantId,
    headers: &HeaderMap,
) -> Result<Context, Error> {
    if who.bound_tenant.is_some_and(|b| b != tenant) {
        return Err(Error::not_found("tenant"));
    }
    auth::context_for(&state.services, who, Some(tenant), idempotency_key(headers)).await
}

impl FromRequestParts<AppState> for Principal {
    type Rejection = AuthRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        match authenticate(state, &parts.method, &parts.headers).await {
            Ok(Some(who)) => Ok(Principal(who)),
            Ok(None) => Err(AuthRejection::new(
                state,
                unauthenticated(
                    "this endpoint needs sign-in",
                    "sign in at /v1/auth/login, or send a bearer access token",
                ),
            )),
            Err(e) => Err(AuthRejection::new(state, e)),
        }
    }
}

impl OptionalFromRequestParts<AppState> for Principal {
    type Rejection = AuthRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Option<Self>, Self::Rejection> {
        authenticate(state, &parts.method, &parts.headers)
            .await
            .map(|o| o.map(Principal))
            .map_err(|e| AuthRejection::new(state, e))
    }
}

impl FromRequestParts<AppState> for MaybePrincipal {
    type Rejection = AuthRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        authenticate(state, &parts.method, &parts.headers)
            .await
            .map(MaybePrincipal)
            .map_err(|e| AuthRejection::new(state, e))
    }
}

impl FromRequestParts<AppState> for RequestContext {
    type Rejection = AuthRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let Principal(who) =
            <Principal as FromRequestParts<AppState>>::from_request_parts(parts, state).await?;
        let tenant = tenant_header(&parts.headers).map_err(|e| AuthRejection::new(state, e))?;
        auth::context_for(
            &state.services,
            &who,
            tenant,
            idempotency_key(&parts.headers),
        )
        .await
        .map(RequestContext)
        .map_err(|e| AuthRejection::new(state, e))
    }
}

/// Refuses share-link bearers with `user_required`: they can call only the node reads.
pub fn refuse_share_link(who: &Authenticated) -> Result<(), Error> {
    if who.actor.actor.kind == ruum_core::PrincipalKind::ShareLink {
        Err(Error::user_required())
    } else {
        Ok(())
    }
}

/// The signed-in user of an operation marked `x-ruum-user-only`: agents, apps and share-link
/// bearers get `user_required`.
pub fn require_user(who: &Authenticated) -> Result<ruum_core::UserId, Error> {
    if who.actor.is_user() && who.scope.is_none() {
        Ok(ruum_core::UserId(who.actor.actor.id.0))
    } else {
        Err(Error::user_required())
    }
}

/// A JSON request body. A body that is not valid JSON is `400 bad_request`; JSON that does not
/// fit the type is `422 validation_failed`; both with the standard error body. Any area may use
/// it in place of `axum::Json` for request bodies.
pub struct JsonBody<T>(pub T);

impl<T, S> axum::extract::FromRequest<S> for JsonBody<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = axum::body::Bytes::from_request(req, state)
            .await
            .map_err(|e| e.into_response())?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| {
            render(
                ApiError::new(
                    ErrorCode::BadRequest,
                    format!("the body is not valid JSON: {e}"),
                )
                .with_hint("send a JSON object with Content-Type: application/json"),
            )
        })?;
        serde_json::from_value(value).map(JsonBody).map_err(|e| {
            render(
                ApiError::new(
                    ErrorCode::ValidationFailed,
                    format!("the body is not valid: {e}"),
                )
                .with_hint("see the request schema in contracts/openapi.yaml"),
            )
        })
    }
}

/// Query parameters. Parameters that do not fit the type are `400 bad_request` with the
/// standard error body.
pub struct QueryParams<T>(pub T);

impl<T, S> FromRequestParts<S> for QueryParams<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let q = parts.uri.query().unwrap_or_default();
        axum::extract::Query::<T>::try_from_uri(&parts.uri)
            .map(|axum::extract::Query(v)| QueryParams(v))
            .map_err(|e| {
                render(
                    ApiError::new(
                        ErrorCode::BadRequest,
                        format!("the query parameters are not valid: {}", e.body_text()),
                    )
                    .with_hint(if q.is_empty() {
                        "add the required query parameters".to_string()
                    } else {
                        "see the parameters in contracts/openapi.yaml".to_string()
                    }),
                )
            })
    }
}

/// The paging parameters of lists (`cursor`, `limit`).
#[derive(Debug, Default, serde::Deserialize)]
pub struct Paging {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

impl Paging {
    /// The page request; `limit` must be 1 to 1000.
    pub fn page(&self) -> Result<ruum_core::PageRequest, Error> {
        if self
            .limit
            .is_some_and(|l| l == 0 || l > ruum_core::paging::MAX_PAGE_SIZE)
        {
            return Err(Error::Api(ApiError::new(
                ErrorCode::BadRequest,
                "limit must be 1 to 1000",
            )));
        }
        Ok(ruum_core::PageRequest {
            cursor: self.cursor.clone().map(ruum_core::Cursor),
            limit: self.limit,
        })
    }
}

/// The largest request body that the idempotency layer keeps a key for. Larger writes (content
/// uploads) run without the replay guard; chunk uploads are idempotent by their hash.
const IDEMPOTENT_BODY_LIMIT: usize = 16 * 1024 * 1024;

/// The response headers that a replay sends again.
const REPLAYED_HEADERS: &[&str] = &["content-type", "location", "etag", "cache-control"];

/// The tenant of a `/v1/tenants/{tenant_id}/...` path, if the path is one.
fn path_tenant(path: &str) -> Option<TenantId> {
    path.strip_prefix("/v1/tenants/")?
        .split('/')
        .next()?
        .parse()
        .ok()
}

/// Gives the key up when the request does not finish: the handler panicked or the client went
/// away (the future is dropped). Without it the key would stay "running" and every retry would
/// get `409 conflict` until it expires.
struct AbandonOnDrop {
    store: std::sync::Arc<dyn ruum_core::identity::idempotency::IdempotencyStore>,
    key: Option<ruum_core::identity::idempotency::KeyRef>,
}

impl AbandonOnDrop {
    /// The request finished; the caller stores or abandons the key itself.
    fn disarm(&mut self) {
        self.key = None;
    }
}

impl Drop for AbandonOnDrop {
    fn drop(&mut self) {
        if let Some(key) = self.key.take()
            && let Ok(rt) = tokio::runtime::Handle::try_current()
        {
            let store = self.store.clone();
            rt.spawn(async move {
                if let Err(e) = store.abandon(&key).await {
                    tracing::warn!(error = %e, "could not give up an idempotency key");
                }
            });
        }
    }
}

/// The idempotency layer (`Idempotency-Key`, table `idempotency_keys`). For a write with the
/// header from an authenticated caller it claims the key, runs the request and stores the
/// response; a retry with the same request gets the stored response, another request with the
/// same key gets `409 idempotency_key_reused`. Requests without the header pass through.
///
/// - A request whose size is not known to be at most 16 MiB (no `Content-Length`, or a larger
///   one) runs without the replay guard, so a large chunked upload is never refused for its key.
/// - A response that holds a secret shown once is not kept (see
///   `ruum_core::identity::idempotency::storable`); a retry gets `409 conflict`.
/// - A handler that panics, or a client that goes away, gives the key up.
pub async fn idempotency(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    use axum::body::HttpBody as _;
    use ruum_core::identity::idempotency::{
        Begin, KeyRef, StoredResponse, check_key, request_hash,
    };
    let Some(key) = idempotency_key(req.headers()) else {
        return next.run(req).await;
    };
    if is_safe(req.method()) {
        return next.run(req).await;
    }
    let declared = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let size = declared.or_else(|| req.body().size_hint().exact());
    if size.is_none_or(|n| n > IDEMPOTENT_BODY_LIMIT as u64) {
        return next.run(req).await;
    }
    let who = match authenticate(&state, req.method(), req.headers()).await {
        Ok(Some(w)) => w,
        Ok(None) => return next.run(req).await,
        Err(e) => return AuthRejection::new(&state, e).into_response(),
    };
    if let Err(e) = check_key(&key) {
        return render(e.to_api());
    }
    let requested = match tenant_header(req.headers()) {
        Ok(t) => t.or_else(|| path_tenant(req.uri().path())),
        // The handler answers the bad header.
        Err(_) => return next.run(req).await,
    };
    let tenant = match who.bound_tenant {
        Some(t) => t,
        None => {
            let Some(u) = who.user() else {
                return next.run(req).await;
            };
            // The tenant of the request when the caller is in it; else the home tenant. A
            // tenant the caller is not in is answered by the handler, and keeps no key there.
            let in_requested = match requested {
                Some(t) => auth::context_for(&state.services, &who, Some(t), None)
                    .await
                    .ok()
                    .map(|c| c.tenant),
                None => None,
            };
            match in_requested {
                Some(t) => t,
                None if requested.is_some() => return next.run(req).await,
                None => match state.services.identity.get_user(u).await {
                    Ok(user) => user.home_tenant_id,
                    Err(e) => return render(e.to_api()),
                },
            }
        }
    };
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, IDEMPOTENT_BODY_LIMIT).await {
        Ok(b) => b,
        Err(_) => {
            return render(ApiError::new(
                ErrorCode::PayloadTooLarge,
                "the request body is larger than its Content-Length",
            ));
        }
    };
    let uri = parts.uri.to_string();
    let tenant_value = parts
        .headers
        .get(TENANT_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let hash = request_hash(parts.method.as_str(), &uri, tenant_value.as_deref(), &bytes);
    let key_ref = KeyRef {
        tenant,
        actor: who.actor.actor.id.0,
        key,
    };
    let store = state.services.idempotency.clone();
    match store
        .begin(&key_ref, parts.method.as_str(), parts.uri.path(), hash)
        .await
    {
        Ok(Begin::Replay(stored)) => {
            let mut res = Response::new(axum::body::Body::from(stored.body));
            *res.status_mut() = axum::http::StatusCode::from_u16(stored.status).unwrap_or_default();
            for (k, v) in stored.headers {
                if let (Ok(k), Ok(v)) = (
                    axum::http::HeaderName::from_bytes(k.as_bytes()),
                    HeaderValue::from_str(&v),
                ) {
                    res.headers_mut().insert(k, v);
                }
            }
            res.headers_mut()
                .insert("idempotent-replayed", HeaderValue::from_static("true"));
            return res;
        }
        Ok(Begin::Fresh) => {}
        Err(e) => return render(e.to_api()),
    }
    let mut guard = AbandonOnDrop {
        store: store.clone(),
        key: Some(key_ref.clone()),
    };
    let req = axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes));
    let res = next.run(req).await;
    let (rparts, rbody) = res.into_parts();
    let rbytes = match axum::body::to_bytes(rbody, usize::MAX).await {
        Ok(b) => b,
        // The body failed: the guard gives the key up.
        Err(_) => return render(ruum_core::Error::Internal("the response failed".into()).to_api()),
    };
    guard.disarm();
    if rparts.status.is_server_error() {
        if let Err(e) = store.abandon(&key_ref).await {
            tracing::warn!(error = %e, "could not give up an idempotency key");
        }
    } else {
        let stored = StoredResponse {
            status: rparts.status.as_u16(),
            headers: rparts
                .headers
                .iter()
                .filter(|(k, _)| REPLAYED_HEADERS.contains(&k.as_str()))
                .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.to_string(), v.to_string())))
                .collect(),
            body: rbytes.to_vec(),
        };
        // The store keeps no secret: a response with one is kept as `409 conflict`.
        if let Err(e) = store.finish(&key_ref, &stored).await {
            tracing::warn!(error = %e, "could not store an idempotent response");
        }
    }
    Response::from_parts(rparts, axum::body::Body::from(rbytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookies_are_parsed() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; ruum_session=abc; ruum_csrf=x"),
        );
        assert_eq!(cookie(&h, "ruum_session").as_deref(), Some("abc"));
        assert_eq!(cookie(&h, "ruum_csrf").as_deref(), Some("x"));
        assert_eq!(cookie(&h, "nope"), None);
    }

    #[test]
    fn the_path_tenant_is_found() {
        let t = TenantId::new();
        assert_eq!(path_tenant(&format!("/v1/tenants/{t}/members")), Some(t));
        assert_eq!(path_tenant(&format!("/v1/tenants/{t}")), Some(t));
        assert_eq!(path_tenant("/v1/tenants"), None);
        assert_eq!(path_tenant("/v1/agents"), None);
    }

    #[test]
    fn a_bad_tenant_header_is_bad_request() {
        let mut h = HeaderMap::new();
        h.insert(TENANT_HEADER, HeaderValue::from_static("not-a-uuid"));
        let e = tenant_header(&h).unwrap_err();
        assert_eq!(e.to_api().code, ErrorCode::BadRequest);
    }
}
