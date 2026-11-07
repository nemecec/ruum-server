//! Branches (card D-3): `501` stubs until it is built.

use super::stub;
use crate::state::AppState;
use axum::Router;
use axum::routing::get;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/v1/databases/{node_id}/branches",
        get(list_database_branches).post(create_database_branch),
    )
}

stub!(list_database_branches, "listDatabaseBranches");
stub!(create_database_branch, "createDatabaseBranch");
