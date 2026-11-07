//! Restore points and restores (card D-4): `501` stubs until it is built.

use super::stub;
use crate::state::AppState;
use axum::Router;
use axum::routing::{get, post};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/databases/{node_id}/restore-points",
            get(list_database_restore_points).post(create_database_restore_point),
        )
        .route("/v1/databases/{node_id}/restore", post(restore_database))
}

stub!(list_database_restore_points, "listDatabaseRestorePoints");
stub!(create_database_restore_point, "createDatabaseRestorePoint");
stub!(restore_database, "restoreDatabase");
