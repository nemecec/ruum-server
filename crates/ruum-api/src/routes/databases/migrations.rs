//! Migrations (card D-2): `listDatabaseMigrations` and `applyDatabaseMigration`. The logic is in
//! `ruum_core::databases::migrations`; these handlers parse the request and render the JSON of
//! `contracts/openapi.yaml`. Errors with a SQLSTATE (`migration_failed`) or a `Retry-After`
//! (`503 database_not_ready`) render with [`crate::error::render`].

use crate::auth::{JsonBody, Paging, QueryParams, RequestContext};
use crate::error::{ApiResult, HttpError};
use crate::routes::nodes::{path_id, to_json};
use crate::state::AppState;
use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use ruum_core::Error;
use ruum_core::databases::migrations::{Applied, ApplyMigration};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/v1/databases/{node_id}/migrations",
        get(list_database_migrations).post(apply_database_migration),
    )
}

fn answer(e: Error) -> Response {
    match e {
        Error::Api(p) => crate::error::render(p),
        e => HttpError(e).into_response(),
    }
}

/// The query of `listDatabaseMigrations`.
#[derive(Debug, Default, Deserialize)]
pub struct ListMigrationsQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// The migrations of a database. `GET /v1/databases/{node_id}/migrations` (operationId
/// `listDatabaseMigrations`).
pub async fn list_database_migrations(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    QueryParams(q): QueryParams<ListMigrationsQuery>,
) -> ApiResult<Json<Value>> {
    let id = path_id(node)?;
    let paging = Paging {
        cursor: q.cursor.filter(|c| !c.is_empty()),
        limit: q.limit,
    };
    let page = state
        .services
        .databases
        .list_migrations(&ctx, id, paging.page()?)
        .await?;
    Ok(Json(json!({
        "items": to_json(&page.items)?,
        "next_cursor": page.next_cursor.map(|c| c.0),
    })))
}

/// Applies a migration. `POST /v1/databases/{node_id}/migrations` (operationId
/// `applyDatabaseMigration`): `200` with the migration, or `202` with the approval an agent's
/// migration on real data waits for.
pub async fn apply_database_migration(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    JsonBody(b): JsonBody<ApplyMigration>,
) -> ApiResult<Response> {
    let id = path_id(node)?;
    let done = state
        .services
        .databases
        .apply_migration(&ctx, id, b, &state.config.public_url)
        .await;
    Ok(match done {
        Ok(Applied::Ran(w)) => Json(json!({
            "migration": to_json(&w.value)?,
            "change": to_json(&w.change)?,
        }))
        .into_response(),
        Ok(Applied::Approval(a)) => (
            StatusCode::ACCEPTED,
            Json(json!({
                "approval": a.view(&state.config.public_url, ruum_core::now()),
            })),
        )
            .into_response(),
        Err(e) => answer(e),
    })
}
