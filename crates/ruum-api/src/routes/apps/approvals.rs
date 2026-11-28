//! Card W-3b: `listAppApprovals`, `getAppApproval` (with `wait`) and `decideAppApproval`. The
//! rules are in `ruum_core::apps::approvals`.

use crate::auth::{JsonBody, Paging, QueryParams, RequestContext};
use crate::error::ApiResult;
use crate::routes::nodes::{path_id, to_json};
use crate::state::AppState;
use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{Json, Path, State};
use axum::routing::{get, post};
use ruum_core::apps::MAX_WAIT;
use ruum_core::apps::approvals::{DecideAppApproval, parse_status};
use ruum_core::{ApiError, Error, ErrorCode, NodeId};
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;
use uuid::Uuid;

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

fn invalid(message: &str, pointer: &str) -> Error {
    Error::Api(ApiError::new(ErrorCode::ValidationFailed, message).with_hint(pointer))
}

fn ids(ids: Result<Path<(Uuid, Uuid)>, PathRejection>) -> Result<(NodeId, Uuid), Error> {
    ids.map(|Path((n, a))| (NodeId(n), a)).map_err(|e| {
        Error::Api(
            ApiError::new(ErrorCode::BadRequest, format!("bad path: {e}"))
                .with_hint("IDs are UUIDs"),
        )
    })
}

/// The query of `listAppApprovals`.
#[derive(Debug, Deserialize)]
pub struct ApprovalsQuery {
    pub status: Option<String>,
    // Not flattened: a flattened query reads every value as a string.
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// The publishes of agents that wait for a person, newest first. `GET
/// /v1/apps/{node_id}/approvals` (operationId `listAppApprovals`).
pub async fn list_app_approvals(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    QueryParams(q): QueryParams<ApprovalsQuery>,
) -> ApiResult<Json<Value>> {
    let id = path_id(node)?;
    let status = match q.status.as_deref() {
        None => None,
        Some(s) => Some(parse_status(s).ok_or_else(|| {
            invalid(
                "status is pending, approved, denied, expired or used",
                "status",
            )
        })?),
    };
    let page = Paging {
        cursor: q.cursor,
        limit: q.limit,
    }
    .page()?;
    let p = state
        .services
        .apps
        .list_approvals(&ctx, id, status, page, &state.config.public_url)
        .await?;
    Ok(Json(to_json(&p)?))
}

/// The query of `getAppApproval`.
#[derive(Debug, Deserialize)]
pub struct WaitQuery {
    pub wait: Option<i64>,
}

/// One publish request. `GET /v1/apps/{node_id}/approvals/{approval_id}` (operationId
/// `getAppApproval`). `wait` holds the answer up to that many seconds (0 to 30, else `422`)
/// until the status changes.
pub async fn get_app_approval(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    path: Result<Path<(Uuid, Uuid)>, PathRejection>,
    QueryParams(q): QueryParams<WaitQuery>,
) -> ApiResult<Json<Value>> {
    let (node, approval) = ids(path)?;
    let wait = match q.wait {
        None => Duration::ZERO,
        // `Wait` is 0 to 30 in the contract: a value outside is a bad parameter (422).
        Some(w) if !(0..=MAX_WAIT.as_secs() as i64).contains(&w) => {
            return Err(invalid("wait is 0 to 30 seconds", "wait").into());
        }
        Some(w) => Duration::from_secs(w as u64),
    };
    let a = state
        .services
        .apps
        .get_approval(&ctx, node, approval, wait, &state.config.public_url)
        .await?;
    Ok(Json(a))
}

/// Approve or deny an agent's publish. `POST
/// /v1/apps/{node_id}/approvals/{approval_id}/decision` (operationId `decideAppApproval`):
/// `200` with the decided approval.
pub async fn decide_app_approval(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    path: Result<Path<(Uuid, Uuid)>, PathRejection>,
    JsonBody(b): JsonBody<DecideAppApproval>,
) -> ApiResult<Json<Value>> {
    let (node, approval) = ids(path)?;
    let w = state
        .services
        .apps
        .decide_approval(
            &state.services,
            &ctx,
            node,
            approval,
            b,
            &state.config.public_url,
        )
        .await?;
    // `AppApproval` (the contract's answer); the change is in the journal.
    Ok(Json(w.value))
}
