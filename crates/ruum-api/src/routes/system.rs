//! System routes: health and the public keys of the token issuer.

use crate::state::AppState;
use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(health))
        .route("/.well-known/jwks.json", get(get_jwks))
        .route(
            "/.well-known/oauth-protected-resource",
            get(get_protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(get_authorization_server_metadata),
        )
}

/// OAuth protected resource metadata (RFC 9728). `GET /.well-known/oauth-protected-resource`
/// (operationId `getProtectedResourceMetadata`). Owned by the identity + tenants + agents area.
pub async fn get_protected_resource_metadata(State(state): State<AppState>) -> Json<Value> {
    let origin = state.config.public_url.trim_end_matches('/');
    Json(json!({
        "resource": origin,
        "authorization_servers": [state.services.tokens.issuer()],
        "bearer_methods_supported": ["header"],
        "resource_documentation": format!("{origin}/docs/api"),
    }))
}

/// OAuth authorization server metadata (RFC 8414). `GET /.well-known/oauth-authorization-server`
/// (operationId `getAuthorizationServerMetadata`). Owned by the identity + tenants + agents area.
pub async fn get_authorization_server_metadata(State(state): State<AppState>) -> Json<Value> {
    let origin = state.config.public_url.trim_end_matches('/');
    Json(json!({
        "issuer": state.services.tokens.issuer(),
        "authorization_endpoint": format!("{origin}/v1/oauth/authorize"),
        "token_endpoint": format!("{origin}/v1/oauth/token"),
        "registration_endpoint": format!("{origin}/v1/oauth/register"),
        "jwks_uri": format!("{origin}/.well-known/jwks.json"),
        "response_types_supported": ["code"],
        "grant_types_supported": [
            "authorization_code",
            "refresh_token",
            "urn:ietf:params:oauth:grant-type:token-exchange",
        ],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": ["offline_access"],
    }))
}

/// Liveness check. `GET /healthz` (operationId `health`).
pub async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

/// Public keys of the Ruum token issuer. `GET /.well-known/jwks.json` (operationId `getJwks`).
/// Owned by the identity + tenants + agents area.
pub async fn get_jwks(State(state): State<AppState>) -> Json<Value> {
    Json(state.services.tokens.jwks())
}
