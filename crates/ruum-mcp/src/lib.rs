//! The Ruum MCP server (`docs/DESIGN.md`, section 11): a remote MCP endpoint at `/mcp` with
//! OAuth, built with `rmcp` (the official Rust MCP SDK) over streamable HTTP.
//!
//! - Every request needs a Ruum access token (`Authorization: Bearer ...`). Without one, or with
//!   an invalid one, `/mcp` answers `401` with
//!   `WWW-Authenticate: Bearer resource_metadata="<origin>/.well-known/oauth-protected-resource"`,
//!   so an MCP client finds the authorization server (RFC 9728) and runs the OAuth 2.1 flow of
//!   the identity area.
//! - There is one tool per entry of `contracts/mcp/tools.yaml` ([`catalog`]). A tool call runs
//!   the REST operation it maps to through the in-process API router ([`dispatch`]), with the
//!   caller's token. So the MCP face has exactly the rights, checks, journal entries and error
//!   codes of the REST face, and never more.
//! - Tool errors are tool results with `isError: true` that carry the Ruum error body
//!   (`{"error": {"code", "message", "hint"}}`) and the HTTP status.
//!
//! The server is stateless (no MCP session): each HTTP request carries its token, so any
//! instance can answer it.
//!
//! Owned by the MCP server + CLI area.

pub mod catalog;
pub mod dispatch;
mod server;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::never::NeverSessionManager,
};
use ruum_api::AppState;
use ruum_core::identity::{TokenType, auth, unauthenticated};
use ruum_core::{ApiError, Error, ErrorCode};
use std::sync::Arc;

pub use catalog::{Catalog, ToolSpec};
pub use dispatch::Caller;

/// The path of the MCP endpoint.
pub const MCP_PATH: &str = "/mcp";

/// The MCP router, to merge into the server. `state` is the state of the REST API; tool calls
/// run through `ruum_api::router(state)` in process.
pub fn router(state: AppState) -> Router {
    // A handler that panics answers 500 to the tool call instead of ending the MCP request.
    let api =
        ruum_api::router(state.clone()).layer(tower_http::catch_panic::CatchPanicLayer::new());
    let catalog = Arc::new(Catalog::load());
    let handler = server::RuumMcp::new(catalog, api);
    let service = StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(NeverSessionManager::default()),
        http_config(&state.config.public_url),
    );
    Router::new()
        .route_service(MCP_PATH, service)
        .layer(axum::middleware::from_fn_with_state(state, authenticate))
}

/// The settings of the streamable HTTP transport: stateless, JSON answers where possible, and
/// the `Host` header limited to the public origin and loopback names.
fn http_config(public_url: &str) -> StreamableHttpServerConfig {
    let mut hosts = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];
    if let Ok(url) = url::Url::parse(public_url)
        && let Some(host) = url.host_str()
    {
        hosts.push(host.trim_matches(['[', ']']).to_string());
    }
    StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None)
        .with_allowed_hosts(hosts)
}

/// Checks the bearer token of every `/mcp` request and puts the [`Caller`] into the request
/// extensions, where the tool handler finds it.
async fn authenticate(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(|t| t.trim().to_string());
    let Some(token) = token.filter(|t| !t.is_empty()) else {
        return reject(
            &state,
            unauthenticated(
                "the MCP endpoint needs an access token",
                "sign in with OAuth (see the resource_metadata URL in WWW-Authenticate), \
                 or exchange an agent API key at POST /v1/oauth/token",
            ),
        );
    };
    let who = match auth::authenticate_mcp_bearer(&state.services, &token).await {
        Ok(who) => who,
        Err(e) => return reject(&state, e),
    };
    let api_token = match api_token(&state, &token) {
        Ok(t) => t,
        Err(e) => return reject(&state, e),
    };
    // A user token is bound to no tenant; such a caller may name one with `Ruum-Tenant`.
    let tenant = req
        .headers()
        .get(ruum_api::TENANT_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    req.extensions_mut().insert(Caller {
        token: api_token,
        tenant: if who.bound_tenant.is_none() {
            tenant
        } else {
            None
        },
    });
    next.run(req).await
}

/// The token for the in-process REST call. The REST API takes only `aud = <origin>`, while MCP
/// clients may hold tokens for `<origin>/mcp` (RFC 8707). Such a token is signed again for the
/// origin with the same claims and expiry, so the call has the same principal and no more time.
fn api_token(state: &AppState, token: &str) -> Result<String, Error> {
    let tokens = &state.services.tokens;
    let origin = tokens.audience().trim_end_matches('/');
    let mut claims = tokens.verify(token)?;
    if claims.aud == origin {
        return Ok(token.to_string());
    }
    if !matches!(claims.typ, TokenType::Agent | TokenType::User) {
        return Err(unauthenticated(
            "this token does not work for the MCP endpoint",
            "use an agent access token",
        ));
    }
    claims.aud = origin.to_string();
    tokens.issue(&claims)
}

/// The error answer of the endpoint itself: the standard error JSON, and on `401` the
/// `WWW-Authenticate` header with the protected resource metadata.
fn reject(state: &AppState, error: Error) -> Response {
    let api: ApiError = error.to_api();
    let status = StatusCode::from_u16(api.code.http_status()).unwrap_or(StatusCode::UNAUTHORIZED);
    let unauthorized = api.code.http_status() == 401
        || matches!(
            api.code,
            ErrorCode::Unauthenticated | ErrorCode::TokenExpired
        );
    let mut res = (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        Body::from(serde_json::json!({ "error": api }).to_string()),
    )
        .into_response();
    if unauthorized
        && let Ok(v) = HeaderValue::from_str(&ruum_api::auth::www_authenticate(
            &ruum_api::auth::resource_metadata_url(state),
        ))
    {
        res.headers_mut().insert(header::WWW_AUTHENTICATE, v);
    }
    res
}
