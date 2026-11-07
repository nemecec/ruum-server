//! The query API (card Q-1): `501` stubs until it is built.

use super::stub;
use crate::state::AppState;
use axum::Router;
use axum::routing::{get, post};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/databases/{node_id}/schema", get(get_database_schema))
        .route("/v1/databases/{node_id}/query", post(query_database))
        .route(
            "/v1/databases/{node_id}/queries",
            get(list_database_queries),
        )
}

stub!(get_database_schema, "getDatabaseSchema");
stub!(query_database, "queryDatabase");
stub!(list_database_queries, "listDatabaseQueries");
