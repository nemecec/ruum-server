//! The six operations of `contracts/apps/runtime.openapi.yaml` (card R-3).
//!
//! Every operation first finds the caller ([`authenticate`]): exactly one `Authorization:
//! Bearer` header with a workload token (or, for `readBoundContent` and `listBoundChildren`, a
//! delegated token) that passes its check (`crate::token`), from a peer whose network identity
//! matches the token when a lookup is configured (`crate::caller`), of a principal that still
//! serves (the core's answer, kept for at most 3 s). Then the caps (`crate::limits`). Errors
//! have the REST shape `{ "error": { "code", "message", "hint" } }` with existing codes; no
//! answer and no log line holds a token or file content (log fields are IDs and statuses).
//!
//! - `getAppSelf`: the core's `AppSelf`.
//! - `listAppSelfUsers`: the holders of a role as pairwise IDs (a `tenant_members` grant as one
//!   entry with `everyone: true`). `name` and `email` are never sent in step 3: the tenant
//!   policy for claims does not exist yet, and the gateway's user tokens carry neither.
//! - `exchangeAppUserToken`: the user token is checked against the gateway's key set (`401`
//!   for a bad one, also for a replayed one: each user token is exchanged at most once), its
//!   `aud`, `env` and `tenant` must be the caller's (`401`), its `actor` `user` or `agent`
//!   (`403`; `delegate` comes with B-3 and G-3, so every other actor is `403` in step 3), and a
//!   binding must have `on_behalf_of_user` (`403`). The delegated token names only those
//!   bindings and lives 300 s.
//! - `readBoundContent`, `listBoundChildren`: the core's checks (bound content only, the
//!   user's right for a delegated token); streamed with `Content-Length`, under the byte rates.
//! - `publishAppEvent`: the topic pattern; `403` unless the live manifest declares the topic;
//!   the body read with its cap (`413` past 256 KiB, before any parsing); a JSON object of at
//!   most 200 properties; then `501 not_implemented`, hint `event_delivery_not_ready`, until
//!   card R-6 delivers events (never a `202` for an event nobody delivers).

use crate::RuntimeApi;
use crate::core_api::CoreContent;
use crate::token::{self, Delegated, Workload};
use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use ruum_core::TenantId;
use ruum_core::apps::runtime_api::wire::{
    AppSelf, Caller, Delegation, ExchangeAnswer, ExchangeQuery, HoldersAnswer, HoldersQuery,
    PrincipalAnswer, ReadQuery, Refusal, TopicAnswer, TopicQuery,
};
use ruum_core::identity::app_sign_in::AppEnv;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use uuid::Uuid;

/// The largest event body (the contract: 256 KiB).
pub const MAX_EVENT_BYTES: usize = 256 * 1024;
/// The most properties of an event (the contract's `maxProperties`).
pub const MAX_EVENT_PROPERTIES: usize = 200;
/// The largest body of an exchange request.
pub const MAX_EXCHANGE_BYTES: usize = 16 * 1024;
/// The longest cursor the runtime API passes on.
const MAX_CURSOR_CHARS: usize = 1024;

/// An error answer of the REST shape.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub hint: Option<&'static str>,
    pub retry_after: Option<u64>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            hint: None,
            retry_after: None,
        }
    }

    fn hint(mut self, hint: &'static str) -> Self {
        self.hint = Some(hint);
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut r = (
            self.status,
            Json(json!({ "error": {
                "code": self.code,
                "message": self.message,
                "hint": self.hint,
            }})),
        )
            .into_response();
        if let Some(s) = self.retry_after
            && let Ok(v) = HeaderValue::from_str(&s.to_string())
        {
            r.headers_mut().insert(header::RETRY_AFTER, v);
        }
        r
    }
}

fn unauthenticated(message: &str) -> ApiError {
    ApiError::new(StatusCode::UNAUTHORIZED, "unauthenticated", message)
        .hint("send the app's workload token as Authorization: Bearer")
}

fn denied(message: &str) -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "permission_denied", message)
}

fn not_found(message: &str) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "not_found", message)
}

fn bad_request(message: &str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", message)
}

fn unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "unavailable",
        "the platform could not answer; try again",
    )
}

fn core_failed(e: crate::core_api::CoreError) -> ApiError {
    tracing::error!(target: "ruum::runtime_api", error = %e, "a core call failed");
    unavailable()
}

/// Any path this API does not have.
pub async fn fallback() -> ApiError {
    not_found("no such operation")
}

/// Only the configured host (`ruum.internal`) reaches the operations; the internal gateway's
/// hosts come with card G-3.
pub async fn host_only(State(rt): State<Arc<RuntimeApi>>, req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|h| {
            h.rsplit_once(':').map_or(h, |(n, p)| {
                if p.bytes().all(|b| b.is_ascii_digit()) {
                    n
                } else {
                    h
                }
            })
        })
        .map(|h| h.trim_end_matches('.').to_ascii_lowercase());
    if host.as_deref() != Some(rt.config.internal_host.as_str()) {
        return not_found("no such host").into_response();
    }
    next.run(req).await
}

// ------------------------------------------------------------------------------ the caller

/// A verified credential.
#[derive(Clone, Debug)]
pub enum Credential {
    Workload(Workload),
    Delegated(Delegated),
}

impl Credential {
    fn workload(&self) -> &Workload {
        match self {
            Credential::Workload(w) => w,
            Credential::Delegated(d) => &d.caller,
        }
    }

    fn delegation(&self) -> Option<Delegation> {
        match self {
            Credential::Workload(_) => None,
            Credential::Delegated(d) => Some(Delegation {
                bindings: d.bindings.clone(),
                user: d.user,
                agent: d.agent,
            }),
        }
    }
}

/// The caller of a request.
pub struct Authed {
    pub credential: Credential,
    pub caller: Caller,
    pub app: AppSelf,
    _permit: crate::limits::Permit,
}

fn caller_of(w: &Workload) -> Caller {
    Caller {
        tenant_id: w.tenant,
        app_id: w.app,
        principal: w.principal,
        environment: w.environment,
    }
}

/// The one bearer value of `Authorization` (two headers, another scheme or none: `401`).
fn bearer(headers: &HeaderMap) -> Result<&str, ApiError> {
    let mut all = headers.get_all(header::AUTHORIZATION).iter();
    let (Some(v), None) = (all.next(), all.next()) else {
        return Err(unauthenticated(
            "exactly one Authorization header is needed",
        ));
    };
    let v = v
        .to_str()
        .map_err(|_| unauthenticated("the Authorization header is not text"))?;
    let (scheme, token) = v
        .split_once(' ')
        .ok_or_else(|| unauthenticated("the Authorization header is not a bearer token"))?;
    let token = token.trim();
    if !scheme.eq_ignore_ascii_case("bearer") || token.is_empty() {
        return Err(unauthenticated(
            "the Authorization header is not a bearer token",
        ));
    }
    Ok(token)
}

fn refused(reason: &str) -> ApiError {
    tracing::info!(target: "ruum::runtime_api", status = 401, reason, "a credential was refused");
    unauthenticated("the token is not a valid workload token for this call")
}

/// Finds the caller (see the module docs). `delegated`: whether the operation also takes a
/// delegated token.
pub async fn authenticate(
    rt: &Arc<RuntimeApi>,
    headers: &HeaderMap,
    peer: SocketAddr,
    delegated: bool,
) -> Result<Authed, ApiError> {
    let token = bearer(headers)?;
    let credential = match token::peek_typ(token).as_deref() {
        Some(ruum_core::apps::workload_tokens::WORKLOAD_TYP) => Credential::Workload(
            token::verify_workload(token, &rt.workload_keys, &rt.config.issuer)
                .map_err(|e| refused(&e.0))?,
        ),
        Some(ruum_core::apps::workload_tokens::DELEGATED_TYP) if delegated => {
            Credential::Delegated(rt.delegated.verify(token).map_err(|e| refused(&e.0))?)
        }
        _ => return Err(refused("not a token of this call")),
    };
    let w = credential.workload().clone();
    if let Some(net) = &rt.network {
        match net.identify(peer.ip()).await {
            Some(n) if n.admits(w.tenant, w.app, w.environment) => {}
            _ => return Err(refused("the token does not belong to the caller's network")),
        }
    }
    let caller = caller_of(&w);
    let app = match principal(rt, &caller).await? {
        PrincipalAnswer::Serving { app } => app,
        PrincipalAnswer::Gone => return Err(refused("the principal no longer serves")),
    };
    let permit = rt.limiter.acquire(w.tenant, w.app).map_err(|which| {
        tracing::warn!(
            target: "ruum::runtime_api",
            tenant = %w.tenant,
            app = %w.app,
            cap = which.as_str(),
            "a request was refused at a cap"
        );
        let mut e = ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many requests at once; try again",
        );
        e.retry_after = Some(1);
        e
    })?;
    Ok(Authed {
        credential,
        caller,
        app,
        _permit: permit,
    })
}

/// The core's answer about a principal, kept for at most `CHECK_TTL`.
async fn principal(rt: &RuntimeApi, c: &Caller) -> Result<PrincipalAnswer, ApiError> {
    let now = Instant::now();
    if let Some(a) = rt.principals.get(c, now) {
        return Ok(a);
    }
    let a = rt.core.principal(c).await.map_err(core_failed)?;
    rt.principals.put(c, a.clone(), now);
    Ok(a)
}

// ------------------------------------------------------------------------------ replays

/// The user tokens already exchanged, per tenant, each until its `exp` (backlog S-61: per
/// tenant at most [`ReplaySet::PER_TENANT`]; when all tenants together hold
/// [`ReplaySet::TOTAL`], a tenant at or over its fair share of that is refused first).
#[derive(Default)]
pub struct ReplaySet {
    inner: Mutex<HashMap<TenantId, HashMap<String, i64>>>,
}

/// Whether a user token may be exchanged now.
#[derive(Debug, PartialEq, Eq)]
pub enum FirstUse {
    Yes,
    Replayed,
    Full,
}

impl ReplaySet {
    pub const PER_TENANT: usize = 10_000;
    pub const TOTAL: usize = 200_000;

    /// Records the first use of `jti` (valid until `exp`) in `tenant`.
    pub fn first_use(&self, tenant: TenantId, jti: &str, exp: i64, now: i64) -> FirstUse {
        let mut m = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let total: usize = m.values().map(HashMap::len).sum();
        let tenants = m.len().max(1);
        let set = m.entry(tenant).or_default();
        if set.contains_key(jti) {
            return FirstUse::Replayed;
        }
        if set.len() >= Self::PER_TENANT || total >= Self::TOTAL {
            set.retain(|_, e| *e > now);
        }
        let share = Self::TOTAL / tenants;
        if set.len() >= Self::PER_TENANT || (total >= Self::TOTAL && set.len() >= share) {
            return FirstUse::Full;
        }
        set.insert(jti.to_string(), exp);
        if total >= Self::TOTAL {
            // Every tenant drops its ended tokens, so the total shrinks again.
            for s in m.values_mut() {
                s.retain(|_, e| *e > now);
            }
            m.retain(|_, s| !s.is_empty());
        }
        FirstUse::Yes
    }
}

// ------------------------------------------------------------------------------ operations

/// `GET /v1/self` (`getAppSelf`).
pub async fn get_app_self(
    State(rt): State<Arc<RuntimeApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<AppSelf>, ApiError> {
    let a = authenticate(&rt, &headers, peer, false).await?;
    Ok(Json(a.app))
}

#[derive(Deserialize)]
pub struct UsersQuery {
    role: Option<String>,
    cursor: Option<String>,
    limit: Option<String>,
}

fn valid_role(s: &str) -> bool {
    token::valid_binding_name(s)
}

/// `GET /v1/self/users` (`listAppSelfUsers`).
pub async fn list_app_self_users(
    State(rt): State<Arc<RuntimeApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<UsersQuery>,
) -> Result<Json<Value>, ApiError> {
    let a = authenticate(&rt, &headers, peer, false).await?;
    let role = q
        .role
        .filter(|r| valid_role(r))
        .ok_or_else(|| bad_request("role must be an app role name"))?;
    let limit = match q.limit {
        None => 100,
        Some(l) => l
            .parse::<u32>()
            .ok()
            .filter(|l| (1..=200).contains(l))
            .ok_or_else(|| bad_request("limit must be 1 to 200"))?,
    };
    if q.cursor
        .as_ref()
        .is_some_and(|c| c.len() > MAX_CURSOR_CHARS)
    {
        return Err(bad_request("the cursor is not valid"));
    }
    let answer = rt
        .core
        .holders(&HoldersQuery {
            caller: a.caller.clone(),
            role,
            cursor: q.cursor,
            limit,
        })
        .await
        .map_err(core_failed)?;
    match answer {
        HoldersAnswer::Page { items, next_cursor } => {
            Ok(Json(json!({ "items": items, "next_cursor": next_cursor })))
        }
        HoldersAnswer::NoRole => Err(not_found("the role is not in the app's manifest")),
        HoldersAnswer::BadCursor => Err(bad_request("the cursor is not valid")),
        HoldersAnswer::Gone => Err(refused("the principal no longer serves")),
    }
}

fn too_large(max: usize) -> ApiError {
    ApiError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "payload_too_large",
        format!("the body is larger than {max} bytes"),
    )
}

/// Reads a body with a cap, while reading: `413` past `max` bytes.
async fn capped_body(body: Body, max: usize) -> Result<Bytes, ApiError> {
    axum::body::to_bytes(body, max)
        .await
        .map_err(|_| too_large(max))
}

fn content_length_over(headers: &HeaderMap, max: usize) -> bool {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|n| n > max as u64)
}

/// `POST /v1/token/exchange` (`exchangeAppUserToken`).
pub async fn exchange(
    State(rt): State<Arc<RuntimeApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
) -> Result<Json<Value>, ApiError> {
    let (parts, body) = req.into_parts();
    let a = authenticate(&rt, &parts.headers, peer, false).await?;
    let Credential::Workload(w) = &a.credential else {
        return Err(refused("an exchange takes the workload token"));
    };
    if content_length_over(&parts.headers, MAX_EXCHANGE_BYTES) {
        return Err(too_large(MAX_EXCHANGE_BYTES));
    }
    let bytes = capped_body(body, MAX_EXCHANGE_BYTES).await?;
    let v: Value =
        serde_json::from_slice(&bytes).map_err(|_| bad_request("the body is not JSON"))?;
    let Some(user_token) = v.get("user_token").and_then(Value::as_str) else {
        return Err(bad_request("user_token is required"));
    };
    let user = token::verify_user(user_token, &rt.user_keys, &rt.config.issuer).map_err(|e| {
        tracing::info!(target: "ruum::runtime_api", status = 401, reason = %e.0, "a user token was refused");
        unauthenticated("the user token is not valid")
    })?;
    // The token was made for this caller: its audience, environment and tenant.
    let aud = token::expected_audience(w.app, w.environment, w.principal);
    if user.aud != aud || user.env != w.environment.as_str() || user.tenant != a.app.tenant_slug {
        tracing::info!(target: "ruum::runtime_api", status = 401, app = %w.app, "a user token of another caller was refused");
        return Err(unauthenticated("the user token was not made for this app"));
    }
    // Once per user token (until its exp).
    match rt.replay.first_use(
        w.tenant,
        &user.jti,
        user.exp,
        ruum_core::now().unix_timestamp(),
    ) {
        FirstUse::Yes => {}
        FirstUse::Replayed => {
            tracing::info!(target: "ruum::runtime_api", status = 401, app = %w.app, "a replayed user token was refused");
            return Err(unauthenticated("the user token was exchanged before"));
        }
        FirstUse::Full => {
            let mut e = ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "too many exchanges; try again",
            );
            e.retry_after = Some(1);
            return Err(e);
        }
    }
    // `user` and `agent` act for a person; every other actor needs `delegate` (B-3, G-3).
    if user.actor != "user" && user.actor != "agent" {
        return Err(denied("this actor cannot be exchanged").hint("delegate_not_ready"));
    }
    let answer = rt
        .core
        .exchange(&ExchangeQuery {
            caller: a.caller.clone(),
            sub: user.sub.clone(),
            actor: user.actor.clone(),
            on_behalf_of: user.on_behalf_of.clone(),
        })
        .await
        .map_err(core_failed)?;
    let (bindings, sealed_user, agent) = match answer {
        ExchangeAnswer::Granted {
            bindings,
            user,
            agent,
        } => (bindings, user, agent),
        ExchangeAnswer::NoBinding => {
            return Err(denied("no binding of this app acts for the user")
                .hint("bindings need on_behalf_of_user"));
        }
        ExchangeAnswer::UnknownUser => {
            return Err(unauthenticated("the user token names nobody of this space"));
        }
        ExchangeAnswer::Gone => return Err(refused("the principal no longer serves")),
    };
    let t = rt
        .delegated
        .sign(w, bindings, sealed_user, agent)
        .map_err(|e| {
            tracing::error!(target: "ruum::runtime_api", error = %e, "a delegated token could not be signed");
            unavailable()
        })?;
    tracing::info!(target: "ruum::runtime_api", tenant = %w.tenant, app = %w.app, "a user token was exchanged");
    Ok(Json(json!({
        "access_token": t,
        "expires_in": token::DELEGATED_SECONDS,
    })))
}

fn read_refused(r: Refusal) -> ApiError {
    match r {
        Refusal::NotFound => not_found("no such node, or it is not visible"),
        Refusal::Denied => denied("the node is not inside a binding of this app"),
        Refusal::Gone => refused("the principal no longer serves"),
        Refusal::BadCursor => bad_request("the cursor is not valid"),
    }
}

fn node_of(id: &str) -> Result<ruum_core::NodeId, ApiError> {
    Uuid::parse_str(id)
        .map(ruum_core::NodeId)
        .map_err(|_| not_found("no such node, or it is not visible"))
}

/// `GET /v1/nodes/{node_id}/content` (`readBoundContent`).
pub async fn read_bound_content(
    State(rt): State<Arc<RuntimeApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(node_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let a = authenticate(&rt, &headers, peer, true).await?;
    let node = node_of(&node_id)?;
    let q = ReadQuery {
        caller: a.caller.clone(),
        node_id: node,
        delegation: a.credential.delegation(),
        cursor: None,
        limit: None,
    };
    let CoreContent { length, body } = match rt.core.content(&q).await.map_err(core_failed)? {
        Ok(c) => c,
        Err(r) => return Err(read_refused(r)),
    };
    let (tenant, app) = (a.caller.tenant_id, a.caller.app_id);
    let limiter = rt.limiter.clone();
    // The permit lives as long as the stream.
    let permit = a._permit;
    let stream = body.then(move |chunk| {
        let limiter = limiter.clone();
        let _keep = &permit;
        let wait = match &chunk {
            Ok(b) => limiter.bytes(tenant, app, b.len() as u64, Instant::now()),
            Err(_) => std::time::Duration::ZERO,
        };
        async move {
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }
            chunk
        }
    });
    let mut resp = Response::new(Body::from_stream(stream));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    if let Some(n) = length
        && let Ok(v) = HeaderValue::from_str(&n.to_string())
    {
        h.insert(header::CONTENT_LENGTH, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(resp)
}

#[derive(Deserialize)]
pub struct ChildrenQuery {
    cursor: Option<String>,
    limit: Option<String>,
}

/// `GET /v1/nodes/{node_id}/children` (`listBoundChildren`).
pub async fn list_bound_children(
    State(rt): State<Arc<RuntimeApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(node_id): Path<String>,
    headers: HeaderMap,
    Query(q): Query<ChildrenQuery>,
) -> Result<Json<Value>, ApiError> {
    let a = authenticate(&rt, &headers, peer, true).await?;
    let node = node_of(&node_id)?;
    let limit = match q.limit {
        None => 100,
        Some(l) => l
            .parse::<u32>()
            .ok()
            .filter(|l| (1..=200).contains(l))
            .ok_or_else(|| bad_request("limit must be 1 to 200"))?,
    };
    if q.cursor
        .as_ref()
        .is_some_and(|c| c.len() > MAX_CURSOR_CHARS)
    {
        return Err(bad_request("the cursor is not valid"));
    }
    let r = ReadQuery {
        caller: a.caller.clone(),
        node_id: node,
        delegation: a.credential.delegation(),
        cursor: q.cursor,
        limit: Some(limit),
    };
    match rt.core.children(&r).await.map_err(core_failed)? {
        Ok(p) => Ok(Json(
            json!({ "items": p.items, "next_cursor": p.next_cursor }),
        )),
        Err(r) => Err(read_refused(r)),
    }
}

/// The topic pattern of the contract (`^[a-z][a-z0-9_.]{0,99}$`).
pub fn valid_topic(t: &str) -> bool {
    let b = t.as_bytes();
    !b.is_empty()
        && b.len() <= 100
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'.')
}

/// `POST /v1/events/{topic}` (`publishAppEvent`).
pub async fn publish_app_event(
    State(rt): State<Arc<RuntimeApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(topic): Path<String>,
    req: Request,
) -> Result<Response, ApiError> {
    let (parts, body) = req.into_parts();
    let a = authenticate(&rt, &parts.headers, peer, false).await?;
    if !valid_topic(&topic) {
        return Err(bad_request(
            "the topic does not match ^[a-z][a-z0-9_.]{0,99}$",
        ));
    }
    match rt
        .core
        .topic(&TopicQuery {
            caller: a.caller.clone(),
            topic: topic.clone(),
        })
        .await
        .map_err(core_failed)?
    {
        TopicAnswer::Declared => {}
        TopicAnswer::Undeclared => {
            return Err(denied(
                "the topic is not declared by this app's live manifest",
            ));
        }
        TopicAnswer::Gone => return Err(refused("the principal no longer serves")),
    }
    if content_length_over(&parts.headers, MAX_EVENT_BYTES) {
        return Err(too_large(MAX_EVENT_BYTES));
    }
    let bytes = capped_body(body, MAX_EVENT_BYTES).await?;
    let v: Value =
        serde_json::from_slice(&bytes).map_err(|_| bad_request("the event is not JSON"))?;
    let Some(o) = v.as_object() else {
        return Err(bad_request("the event is a JSON object"));
    };
    if o.len() > MAX_EVENT_PROPERTIES {
        return Err(bad_request("the event has more than 200 properties"));
    }
    if a.caller.environment != AppEnv::Live {
        return Err(denied("a preview publishes no events"));
    }
    Err(ApiError::new(
        StatusCode::NOT_IMPLEMENTED,
        "not_implemented",
        "event delivery comes with a later release",
    )
    .hint("event_delivery_not_ready"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_token_is_exchanged_once() {
        let r = ReplaySet::default();
        let t = TenantId(Uuid::now_v7());
        assert_eq!(r.first_use(t, "j1", 100, 50), FirstUse::Yes);
        assert_eq!(r.first_use(t, "j1", 100, 60), FirstUse::Replayed);
        // The same jti in another tenant is another token.
        assert_eq!(
            r.first_use(TenantId(Uuid::now_v7()), "j1", 100, 60),
            FirstUse::Yes
        );
    }

    #[test]
    fn a_tenant_fills_only_its_own_share() {
        let r = ReplaySet::default();
        let a = TenantId(Uuid::now_v7());
        let b = TenantId(Uuid::now_v7());
        for i in 0..ReplaySet::PER_TENANT {
            assert_eq!(r.first_use(a, &format!("j{i}"), 1000, 10), FirstUse::Yes);
        }
        assert_eq!(r.first_use(a, "one more", 1000, 10), FirstUse::Full);
        assert_eq!(r.first_use(b, "j0", 1000, 10), FirstUse::Yes);
        // Ended tokens make room again.
        assert_eq!(r.first_use(a, "later", 2000, 1001), FirstUse::Yes);
    }

    #[test]
    fn bearer_needs_one_header() {
        let mut h = HeaderMap::new();
        assert!(bearer(&h).is_err());
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer abc"),
        );
        assert_eq!(bearer(&h).unwrap(), "abc");
        h.append(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer def"),
        );
        assert!(bearer(&h).is_err());
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Basic abc"));
        assert!(bearer(&h).is_err());
    }

    #[test]
    fn topics_follow_the_pattern() {
        assert!(valid_topic("orders.created"));
        assert!(valid_topic("a"));
        for bad in ["", "Orders", "1x", "a-b", "a/b", &"a".repeat(101)] {
            assert!(!valid_topic(bad), "{bad}");
        }
    }
}
