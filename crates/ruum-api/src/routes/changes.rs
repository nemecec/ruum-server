//! Routes of the `changes` area. See `docs/STEP1.md` for the owner of this file.

use crate::error::{HttpError, not_implemented};
use crate::state::AppState;
use axum::Router;
use axum::routing::get;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/changes", get(list_changes))
        .route("/v1/changes/latest", get(get_latest_cursor))
}

/// The changes feed. `GET /v1/changes` (operationId `listChanges`).
pub async fn list_changes() -> HttpError {
    not_implemented("listChanges")
}

/// The newest cursor. `GET /v1/changes/latest` (operationId `getLatestCursor`).
pub async fn get_latest_cursor() -> HttpError {
    not_implemented("getLatestCursor")
}
