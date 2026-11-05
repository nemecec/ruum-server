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
