//! Routes of the `audit` area. See `docs/STEP1.md` for the owner of this file.

use crate::error::{HttpError, not_implemented};
use crate::state::AppState;
use axum::Router;
use axum::routing::get;

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/tenants/{tenant_id}/audit", get(list_audit))
}

/// The audit log of a tenant. `GET /v1/tenants/{tenant_id}/audit` (operationId `listAudit`).
pub async fn list_audit() -> HttpError {
    not_implemented("listAudit")
}
