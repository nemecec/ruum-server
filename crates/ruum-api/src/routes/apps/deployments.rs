//! Card W-2s (W-2 static and the static part of W-3a): `createAppDeployment` (previews) and
//! `publishAppDeployment` (publish and rollback). The rules are in `ruum_core::apps::deployments`.

use crate::auth::{JsonBody, RequestContext};
use crate::error::ApiResult;
use crate::routes::nodes::{path_id, to_json};
use crate::state::AppState;
use axum::Router;
use axum::body::Bytes;
use axum::extract::rejection::PathRejection;
use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::routing::post;
use ruum_core::apps::deployments::{CreateDeployment, PublishDeployment, Published};
use ruum_core::{ApiError, Error, ErrorCode, NodeId};
use serde_json::Value;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/apps/{node_id}/deployments",
            post(create_app_deployment),
        )
        .route(
            "/v1/apps/{node_id}/deployments/{deployment_id}/publish",
            post(publish_app_deployment),
        )
}

/// Deploy an archive as a preview. `POST /v1/apps/{node_id}/deployments` (operationId
/// `createAppDeployment`): `202` with the deployment; the cheap stages answer directly.
pub async fn create_app_deployment(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    JsonBody(b): JsonBody<CreateDeployment>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let id = path_id(node)?;
    let d = state.services.apps.create_deployment(&ctx, id, b).await?;
    Ok((StatusCode::ACCEPTED, Json(to_json(&d)?)))
}

/// Make a deployment live (publish or roll back). `POST
/// /v1/apps/{node_id}/deployments/{deployment_id}/publish` (operationId
/// `publishAppDeployment`): `200` with the live deployment for a person, `202` with a pending
/// approval for an agent. The body is optional.
pub async fn publish_app_deployment(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    ids: Result<Path<(Uuid, Uuid)>, PathRejection>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (node, deployment) = ids.map(|Path(v)| v).map_err(|e| {
        Error::Api(
            ApiError::new(ErrorCode::BadRequest, format!("bad path: {e}"))
                .with_hint("IDs are UUIDs"),
        )
    })?;
    let req: PublishDeployment = if body.iter().all(|b| b.is_ascii_whitespace()) {
        PublishDeployment::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| {
            Error::Api(
                ApiError::new(
                    ErrorCode::ValidationFailed,
                    format!("the body is not a PublishAppDeploymentRequest: {e}"),
                )
                .with_hint("send {} or {\"force\": true}"),
            )
        })?
    };
    let public_url = state.config.public_url.clone();
    match state
        .services
        .apps
        .publish_deployment(&ctx, NodeId(node), deployment, req, &public_url)
        .await?
    {
        Published::Live(d) => Ok((StatusCode::OK, Json(to_json(&d)?))),
        Published::ApprovalRequired(v) => Ok((StatusCode::ACCEPTED, Json(v))),
    }
}
