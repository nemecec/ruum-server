//! Migrations (card D-2): `501` stubs until it is built.

use super::stub;
use crate::state::AppState;
use axum::Router;
use axum::routing::get;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/v1/databases/{node_id}/migrations",
        get(list_database_migrations).post(apply_database_migration),
    )
}

stub!(list_database_migrations, "listDatabaseMigrations");
stub!(apply_database_migration, "applyDatabaseMigration");
