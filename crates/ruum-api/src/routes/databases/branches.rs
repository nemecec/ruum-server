//! Branches (cards D-3a and D-3b): `listDatabaseBranches` and `createDatabaseBranch`
//! (`masked`, `schema_only` or `full` data). The logic is in `ruum_core::databases::branches`.

use super::database_write;
use crate::auth::{JsonBody, QueryParams, RequestContext};
use crate::error::ApiResult;
use crate::routes::nodes::{path_id, to_json};
use crate::state::AppState;
use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use ruum_core::databases::branches::CreateBranch;
use ruum_core::{Cursor, PageRequest};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/v1/databases/{node_id}/branches",
        get(list_database_branches).post(create_database_branch),
    )
}

/// The query of `listDatabaseBranches`.
#[derive(Debug, Default, Deserialize)]
pub struct ListBranchesQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// The branches of a database. `GET /v1/databases/{node_id}/branches` (operationId
/// `listDatabaseBranches`).
pub async fn list_database_branches(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    QueryParams(q): QueryParams<ListBranchesQuery>,
) -> ApiResult<Json<Value>> {
    let id = path_id(node)?;
    let page = state
        .services
        .databases
        .list_branches(
            &ctx,
            id,
            PageRequest {
                cursor: q.cursor.map(Cursor),
                limit: q.limit,
            },
        )
        .await?;
    Ok(Json(to_json(&page)?))
}

/// Make a branch of a database. `POST /v1/databases/{node_id}/branches` (operationId
/// `createDatabaseBranch`).
///
/// The call writes `node.created` and `version.created` for the branch node at once, and
/// `database.branch_created` on the source when the branch is made (card D-3b).
pub async fn create_database_branch(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    JsonBody(b): JsonBody<CreateBranch>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let id = path_id(node)?;
    let w = state.services.databases.create_branch(&ctx, id, b).await?;
    Ok((StatusCode::ACCEPTED, database_write(&w)?))
}
