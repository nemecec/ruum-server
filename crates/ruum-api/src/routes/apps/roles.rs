//! Card B-2 (area `roles`): `listAppRoles`, `listAppRoleGrants`, `addAppRoleGrant`,
//! `removeAppRoleGrant`. The rules are in `ruum_core::apps::roles`; these handlers parse the
//! request and render the JSON of `contracts/openapi.yaml`.

use crate::auth::{JsonBody, Paging, QueryParams, RequestContext};
use crate::error::ApiResult;
use crate::routes::nodes::{path_id, to_json};
use crate::state::AppState;
use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get};
use ruum_core::apps::roles::AddRoleGrant;
use ruum_core::{ApiError, Error, ErrorCode, NodeId};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/apps/{node_id}/roles", get(list_app_roles))
        .route(
            "/v1/apps/{node_id}/role-grants",
            get(list_app_role_grants).post(add_app_role_grant),
        )
        .route(
            "/v1/apps/{node_id}/role-grants/{role_grant_id}",
            delete(remove_app_role_grant),
        )
}

/// The roles of the live manifest, plus inactive roles that still have grants. `GET
/// /v1/apps/{node_id}/roles` (operationId `listAppRoles`).
pub async fn list_app_roles(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
) -> ApiResult<Json<Value>> {
    let id = path_id(node)?;
    let roles = state.services.apps.list_app_roles(&ctx, id).await?;
    Ok(Json(json!({ "items": to_json(&roles)? })))
}

/// The query of `listAppRoleGrants`.
#[derive(Debug, Deserialize)]
pub struct GrantsQuery {
    pub role: Option<String>,
    // Not flattened: a flattened query reads every value as a string.
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// Who holds which app role. `GET /v1/apps/{node_id}/role-grants` (operationId
/// `listAppRoleGrants`).
pub async fn list_app_role_grants(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    QueryParams(q): QueryParams<GrantsQuery>,
) -> ApiResult<Json<Value>> {
    let id = path_id(node)?;
    let page = Paging {
        cursor: q.cursor,
        limit: q.limit,
    }
    .page()?;
    let p = state
        .services
        .apps
        .list_app_role_grants(&ctx, id, q.role.as_deref(), page)
        .await?;
    Ok(Json(to_json(&p)?))
}

/// Give an app role. `POST /v1/apps/{node_id}/role-grants` (operationId `addAppRoleGrant`).
pub async fn add_app_role_grant(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    JsonBody(b): JsonBody<AddRoleGrant>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let id = path_id(node)?;
    let w = state.services.apps.add_app_role_grant(&ctx, id, b).await?;
    Ok((StatusCode::CREATED, Json(to_json(&w.value)?)))
}

/// Take an app role away. `DELETE /v1/apps/{node_id}/role-grants/{role_grant_id}`
/// (operationId `removeAppRoleGrant`).
pub async fn remove_app_role_grant(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    ids: Result<Path<(Uuid, Uuid)>, PathRejection>,
) -> ApiResult<StatusCode> {
    let (node, grant) = ids.map(|Path(v)| v).map_err(|e| {
        Error::Api(
            ApiError::new(ErrorCode::BadRequest, format!("bad path: {e}"))
                .with_hint("IDs are UUIDs"),
        )
    })?;
    state
        .services
        .apps
        .remove_app_role_grant(&ctx, NodeId(node), grant)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
