//! Routes of the `search` area. See `docs/STEP1.md` for the owner of this file.

use crate::error::{HttpError, not_implemented};
use crate::state::AppState;
use axum::Router;
use axum::routing::get;

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/search", get(search_nodes))
}

/// Search by name. `GET /v1/search` (operationId `searchNodes`).
pub async fn search_nodes() -> HttpError {
    not_implemented("searchNodes")
}
