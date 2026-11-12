//! Card W-3b: `listAppApprovals`, `getAppApproval`, `decideAppApproval`.
//!
//! `501` stubs until the card builds them (made by card W-1).

use super::stub;
use crate::state::AppState;
use axum::Router;
use axum::routing::{get, post};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/apps/{node_id}/approvals", get(list_app_approvals))
        .route(
            "/v1/apps/{node_id}/approvals/{approval_id}",
            get(get_app_approval),
        )
        .route(
            "/v1/apps/{node_id}/approvals/{approval_id}/decision",
            post(decide_app_approval),
        )
}

stub!(list_app_approvals, "listAppApprovals");
stub!(get_app_approval, "getAppApproval");
stub!(decide_app_approval, "decideAppApproval");
