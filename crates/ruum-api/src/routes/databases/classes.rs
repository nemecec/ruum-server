//! Column classes (card K-1): `501` stubs until it is built.

use super::stub;
use crate::state::AppState;
use axum::Router;
use axum::routing::{get, post};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/databases/{node_id}/columns",
            get(list_database_columns),
        )
        .route(
            "/v1/databases/{node_id}/columns/suggestions",
            post(suggest_column_classes),
        )
        .route(
            "/v1/databases/{node_id}/columns/confirm",
            post(confirm_column_classes),
        )
}

stub!(list_database_columns, "listDatabaseColumns");
stub!(suggest_column_classes, "suggestColumnClasses");
stub!(confirm_column_classes, "confirmColumnClasses");
