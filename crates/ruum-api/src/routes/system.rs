//! System routes: health and the public keys of the token issuer.

use crate::error::{HttpError, not_implemented};
use crate::state::AppState;
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
pub async fn get_protected_resource_metadata() -> HttpError {
    not_implemented("getProtectedResourceMetadata")
}

/// OAuth authorization server metadata (RFC 8414). `GET /.well-known/oauth-authorization-server`
/// (operationId `getAuthorizationServerMetadata`). Owned by the identity + tenants + agents area.
pub async fn get_authorization_server_metadata() -> HttpError {
    not_implemented("getAuthorizationServerMetadata")
}

/// Liveness check. `GET /healthz` (operationId `health`).
pub async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

/// Public keys of the Ruum token issuer. `GET /.well-known/jwks.json` (operationId `getJwks`).
/// Owned by the identity + tenants + agents area.
pub async fn get_jwks() -> HttpError {
    not_implemented("getJwks")
}
