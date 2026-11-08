//! Access grants and approvals (cards A-1, A-2). The three grant operations are card A-1
//! (`ruum_core::databases::access::grants`); the approval operations are `501` stubs until A-2.

use super::stub;
use crate::auth::{JsonBody, Paging, QueryParams, RequestContext};
use crate::error::ApiResult;
use crate::routes::nodes::{path_id, to_json};
use crate::state::AppState;
use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use ruum_core::databases::access::grants::{CreateGrant, DatabaseAccessGrant};
use ruum_core::journal::Written;
use ruum_core::{ApiError, Error, ErrorCode, NodeId};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/databases/{node_id}/access-grants",
            get(list_database_access_grants).post(create_database_access_grant),
        )
        .route(
            "/v1/databases/{node_id}/access-grants/{grant_id}",
            delete(revoke_database_access_grant),
        )
        .route(
            "/v1/databases/{node_id}/access-grant-requests",
            post(request_database_access_grant),
        )
        .route(
            "/v1/database-approvals",
            get(list_pending_database_approvals),
        )
        .route(
            "/v1/databases/{node_id}/approvals",
            get(list_database_approvals),
        )
        .route(
            "/v1/databases/{node_id}/approvals/{approval_id}",
            get(get_database_approval),
        )
        .route(
            "/v1/databases/{node_id}/approvals/{approval_id}/decision",
            post(decide_database_approval),
        )
}

/// `{ "grant": ..., "change": ... }` (`DatabaseAccessGrantWrite`).
fn grant_write(w: &Written<DatabaseAccessGrant>) -> Result<Json<Value>, Error> {
    Ok(Json(
        json!({ "grant": to_json(&w.value)?, "change": to_json(&w.change)? }),
    ))
}

/// The query of `listDatabaseAccessGrants`. Not `#[serde(flatten)]`: a flattened query reads
/// every value as a string.
#[derive(Debug, Default, Deserialize)]
pub struct ListGrantsQuery {
    pub active: Option<bool>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// The grants of agents on a database. `GET /v1/databases/{node_id}/access-grants`
/// (operationId `listDatabaseAccessGrants`).
pub async fn list_database_access_grants(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    QueryParams(q): QueryParams<ListGrantsQuery>,
) -> ApiResult<Json<Value>> {
    let id = path_id(node)?;
    let paging = Paging {
        cursor: q.cursor,
        limit: q.limit,
    };
    let page = state
        .services
        .databases
        .list_grants(&ctx, id, q.active.unwrap_or(false), paging.page()?)
        .await?;
    Ok(Json(to_json(&page)?))
}

/// Give an agent a time-limited grant. `POST /v1/databases/{node_id}/access-grants`
/// (operationId `createDatabaseAccessGrant`, users only).
pub async fn create_database_access_grant(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    JsonBody(b): JsonBody<CreateGrant>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let id = path_id(node)?;
    let w = state
        .services
        .databases
        .create_grant(&ctx, &*state.services.agents, id, b)
        .await?;
    Ok((StatusCode::CREATED, grant_write(&w)?))
}

/// End a grant now. `DELETE /v1/databases/{node_id}/access-grants/{grant_id}` (operationId
/// `revokeDatabaseAccessGrant`).
pub async fn revoke_database_access_grant(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    ids: Result<Path<(Uuid, Uuid)>, PathRejection>,
) -> ApiResult<Json<Value>> {
    let Path((node, grant)) = ids.map_err(|e| {
        Error::Api(
            ApiError::new(ErrorCode::BadRequest, format!("bad path: {e}"))
                .with_hint("IDs are UUIDs"),
        )
    })?;
    let w = state
        .services
        .databases
        .revoke_grant(&ctx, NodeId(node), grant)
        .await?;
    Ok(grant_write(&w)?)
}

stub!(request_database_access_grant, "requestDatabaseAccessGrant");
stub!(
    list_pending_database_approvals,
    "listPendingDatabaseApprovals"
);
stub!(list_database_approvals, "listDatabaseApprovals");
stub!(get_database_approval, "getDatabaseApproval");
stub!(decide_database_approval, "decideDatabaseApproval");
