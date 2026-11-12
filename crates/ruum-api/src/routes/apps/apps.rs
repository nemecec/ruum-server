//! Card W-1: `createApp`, `getApp`, `updateAppSettings`, `listAppDeployments` and
//! `getAppDeployment` (with `wait`).

use crate::auth::{JsonBody, Paging, QueryParams, RequestContext};
use crate::error::ApiResult;
use crate::routes::nodes::{if_match, path_id, to_json};
use crate::state::AppState;
use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{Json, Path, State};
use axum::http::header::ETAG;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use ruum_core::apps::settings::SettingsInput;
use ruum_core::apps::store::Environment;
use ruum_core::apps::{App, CreateApp, MAX_WAIT};
use ruum_core::journal::Written;
use ruum_core::{ApiError, Error, ErrorCode, NodeId};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/apps", post(create_app))
        .route("/v1/apps/{node_id}", get(get_app))
        .route("/v1/apps/{node_id}/settings", patch(update_app_settings))
        .route("/v1/apps/{node_id}/deployments", get(list_app_deployments))
        .route(
            "/v1/apps/{node_id}/deployments/{deployment_id}",
            get(get_app_deployment),
        )
}

/// `{ "app": ..., "change": ... }` (`AppWrite`).
fn app_write(w: &Written<App>) -> Result<Json<Value>, Error> {
    Ok(Json(
        json!({ "app": to_json(&w.value)?, "change": to_json(&w.change)? }),
    ))
}

fn invalid(message: &str, pointer: &str) -> Error {
    Error::Api(ApiError::new(ErrorCode::ValidationFailed, message).with_hint(pointer))
}

/// Create a web app node. `POST /v1/apps` (operationId `createApp`).
pub async fn create_app(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    JsonBody(b): JsonBody<CreateApp>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let w = state.services.apps.create(&ctx, b).await?;
    Ok((StatusCode::ACCEPTED, app_write(&w)?))
}

/// Get a web app with its status. `GET /v1/apps/{node_id}` (operationId `getApp`).
pub async fn get_app(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
) -> ApiResult<Response> {
    let id = path_id(node)?;
    let app = state.services.apps.get(&ctx, id).await?;
    let etag = app.node.etag();
    let mut res = Json(to_json(&app)?).into_response();
    if let Ok(v) = HeaderValue::from_str(&etag) {
        res.headers_mut().insert(ETAG, v);
    }
    Ok(res)
}

/// Change the settings of a web app. `PATCH /v1/apps/{node_id}/settings` (operationId
/// `updateAppSettings`).
pub async fn update_app_settings(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    headers: HeaderMap,
    JsonBody(b): JsonBody<SettingsInput>,
) -> ApiResult<Json<Value>> {
    let id = path_id(node)?;
    let w = state
        .services
        .apps
        .update_settings(&ctx, id, b, if_match(&headers)?)
        .await?;
    Ok(app_write(&w)?)
}

/// The query of `listAppDeployments`.
#[derive(Debug, Deserialize)]
pub struct DeploymentsQuery {
    pub environment: Option<String>,
    // Not flattened: a flattened query reads every value as a string.
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// The deployments of a web app, newest first. `GET /v1/apps/{node_id}/deployments`
/// (operationId `listAppDeployments`).
pub async fn list_app_deployments(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    QueryParams(q): QueryParams<DeploymentsQuery>,
) -> ApiResult<Json<Value>> {
    let id = path_id(node)?;
    let environment = match q.environment.as_deref() {
        None => None,
        Some(e) => Some(
            Environment::parse(e)
                .ok_or_else(|| invalid("environment is live or preview", "environment"))?,
        ),
    };
    let page = Paging {
        cursor: q.cursor,
        limit: q.limit,
    }
    .page()?;
    let p = state
        .services
        .apps
        .list_deployments(&ctx, id, environment, page)
        .await?;
    Ok(Json(to_json(&p)?))
}

/// The query of `getAppDeployment`.
#[derive(Debug, Deserialize)]
pub struct WaitQuery {
    pub wait: Option<i64>,
}

/// One deployment. `GET /v1/apps/{node_id}/deployments/{deployment_id}` (operationId
/// `getAppDeployment`). `wait` holds the answer up to that many seconds (0 to 30, else `422`;
/// the service caps the hold at 30 s too) until the state changes.
pub async fn get_app_deployment(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    ids: Result<Path<(Uuid, Uuid)>, PathRejection>,
    QueryParams(q): QueryParams<WaitQuery>,
) -> ApiResult<Json<Value>> {
    let (node, deployment) = ids.map(|Path(v)| v).map_err(|e| {
        Error::Api(
            ApiError::new(ErrorCode::BadRequest, format!("bad path: {e}"))
                .with_hint("IDs are UUIDs"),
        )
    })?;
    let wait = match q.wait {
        None => Duration::ZERO,
        // `Wait` is 0 to 30 in the contract: a value outside is a bad parameter (422).
        Some(w) if !(0..=MAX_WAIT.as_secs() as i64).contains(&w) => {
            return Err(invalid("wait is 0 to 30 seconds", "wait").into());
        }
        Some(w) => Duration::from_secs(w as u64),
    };
    let d = state
        .services
        .apps
        .get_deployment(&ctx, NodeId(node), deployment, wait)
        .await?;
    Ok(Json(to_json(&d)?))
}
