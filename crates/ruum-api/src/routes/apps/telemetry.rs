//! The area `telemetry` (card O-2): `searchAppLogs`, `getAppMetrics`, `getAppTrace`,
//! `compareAppDeployment`, `listAppTelemetryFields`, `confirmAppTelemetryFields` (users only)
//! and `computeAppPseudonym` (users only). The rules are in `ruum_core::telemetry::query`
//! (`Services::telemetry`); these handlers parse the request and render the JSON of
//! `contracts/openapi.yaml`.
//!
//! Every query value is parsed here into a typed value (a UUID, a trace ID, a time, a level);
//! a value that does not parse is `422 validation_failed` with the parameter's name, before
//! anything is read. No value reaches a store query as text except `q`, whose words become
//! escaped line filters in the store.

use crate::auth::{JsonBody, Paging, QueryParams, RequestContext};
use crate::error::ApiResult;
use crate::routes::nodes::{path_id, to_json};
use crate::state::AppState;
use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{Json, Path, State};
use axum::routing::{get, post};
use ruum_core::telemetry::query::{ConfirmItem, LogQuery, MetricsQuery};
use ruum_core::telemetry::{LogOrigin, Severity};
use ruum_core::{ApiError, Error, ErrorCode, NodeId};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/apps/{node_id}/logs", get(search_app_logs))
        .route("/v1/apps/{node_id}/metrics", get(get_app_metrics))
        .route("/v1/apps/{node_id}/traces/{trace_id}", get(get_app_trace))
        .route(
            "/v1/apps/{node_id}/deployments/{deployment_id}/comparison",
            get(compare_app_deployment),
        )
        .route(
            "/v1/apps/{node_id}/telemetry-fields",
            get(list_app_telemetry_fields),
        )
        .route(
            "/v1/apps/{node_id}/telemetry-fields/confirm",
            post(confirm_app_telemetry_fields),
        )
        .route("/v1/apps/{node_id}/pseudonyms", post(compute_app_pseudonym))
}

fn invalid(message: &str, name: &str) -> Error {
    Error::Api(
        ApiError::new(ErrorCode::ValidationFailed, message)
            .with_hint(name.to_owned())
            .with_details(json!({ "pointer": name })),
    )
}

fn uuid_param(v: Option<&str>, name: &str) -> Result<Option<Uuid>, Error> {
    v.map(|s| {
        s.parse::<Uuid>()
            .map_err(|_| invalid(&format!("{name} is not a UUID"), name))
    })
    .transpose()
}

fn time_param(v: Option<&str>, name: &str) -> Result<Option<OffsetDateTime>, Error> {
    v.map(|s| {
        OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
            .map_err(|_| invalid(&format!("{name} is not an RFC 3339 time"), name))
    })
    .transpose()
}

fn int_param(v: Option<&str>, name: &str) -> Result<Option<i64>, Error> {
    v.map(|s| {
        s.parse::<i64>()
            .map_err(|_| invalid(&format!("{name} is not an integer"), name))
    })
    .transpose()
}

fn bool_param(v: Option<&str>, name: &str) -> Result<bool, Error> {
    match v {
        None | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(invalid(&format!("{name} is true or false"), name)),
    }
}

fn level_param(v: Option<&str>) -> Result<Option<Severity>, Error> {
    v.map(|s| match s {
        "trace" => Ok(Severity::Trace),
        "debug" => Ok(Severity::Debug),
        "info" => Ok(Severity::Info),
        "warn" => Ok(Severity::Warn),
        "error" => Ok(Severity::Error),
        "fatal" => Ok(Severity::Fatal),
        _ => Err(invalid(
            "level is trace, debug, info, warn, error or fatal",
            "level",
        )),
    })
    .transpose()
}

fn origin_param(v: Option<&str>) -> Result<Option<LogOrigin>, Error> {
    v.map(|s| match s {
        "backend" => Ok(LogOrigin::Backend),
        "frontend" => Ok(LogOrigin::Frontend),
        "platform" => Ok(LogOrigin::Platform),
        _ => Err(invalid("origin is backend, frontend or platform", "origin")),
    })
    .transpose()
}

/// The query of `searchAppLogs`: every value as text, parsed by the handler.
#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    pub q: Option<String>,
    pub level: Option<String>,
    pub origin: Option<String>,
    pub deployment_id: Option<String>,
    pub trace_id: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// Search the logs of a web app, newest first. `GET /v1/apps/{node_id}/logs` (operationId
/// `searchAppLogs`).
pub async fn search_app_logs(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    QueryParams(q): QueryParams<LogsQuery>,
) -> ApiResult<Json<Value>> {
    let id = path_id(node)?;
    let query = LogQuery {
        q: q.q,
        level: level_param(q.level.as_deref())?,
        origin: origin_param(q.origin.as_deref())?,
        deployment: uuid_param(q.deployment_id.as_deref(), "deployment_id")?,
        trace_id: q.trace_id,
        since: time_param(q.since.as_deref(), "since")?,
        until: time_param(q.until.as_deref(), "until")?,
        page: Paging {
            cursor: q.cursor,
            limit: q.limit,
        }
        .page()?,
    };
    let page = state
        .services
        .telemetry
        .search_logs(&state.services, &ctx, id, query)
        .await?;
    Ok(Json(to_json(&page)?))
}

/// The query of `getAppMetrics`.
#[derive(Debug, Deserialize)]
pub struct MetricsParams {
    pub since: Option<String>,
    pub until: Option<String>,
    pub step_seconds: Option<String>,
    pub deployment_id: Option<String>,
}

/// Metric series of a web app. `GET /v1/apps/{node_id}/metrics` (operationId `getAppMetrics`).
pub async fn get_app_metrics(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    QueryParams(q): QueryParams<MetricsParams>,
) -> ApiResult<Json<Value>> {
    let id = path_id(node)?;
    let query = MetricsQuery {
        since: time_param(q.since.as_deref(), "since")?,
        until: time_param(q.until.as_deref(), "until")?,
        step: int_param(q.step_seconds.as_deref(), "step_seconds")?,
        deployment: uuid_param(q.deployment_id.as_deref(), "deployment_id")?,
    };
    let m = state
        .services
        .telemetry
        .metrics(&state.services, &ctx, id, query)
        .await?;
    Ok(Json(to_json(&m)?))
}

fn bad_path(e: PathRejection) -> Error {
    Error::Api(
        ApiError::new(ErrorCode::BadRequest, format!("bad path: {e}")).with_hint("IDs are UUIDs"),
    )
}

/// One trace across the app and the apps it called. `GET /v1/apps/{node_id}/traces/{trace_id}`
/// (operationId `getAppTrace`).
pub async fn get_app_trace(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    ids: Result<Path<(Uuid, String)>, PathRejection>,
) -> ApiResult<Json<Value>> {
    let (node, trace) = ids.map(|Path(v)| v).map_err(bad_path)?;
    let t = state
        .services
        .telemetry
        .trace(&state.services, &ctx, NodeId(node), &trace)
        .await?;
    Ok(Json(to_json(&t)?))
}

/// The query of `compareAppDeployment`.
#[derive(Debug, Deserialize)]
pub struct CompareQuery {
    pub window_minutes: Option<String>,
}

/// Compare a deployment with the one before it. `GET
/// /v1/apps/{node_id}/deployments/{deployment_id}/comparison` (operationId
/// `compareAppDeployment`).
pub async fn compare_app_deployment(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    ids: Result<Path<(Uuid, Uuid)>, PathRejection>,
    QueryParams(q): QueryParams<CompareQuery>,
) -> ApiResult<Json<Value>> {
    let (node, dep) = ids.map(|Path(v)| v).map_err(bad_path)?;
    let window = int_param(q.window_minutes.as_deref(), "window_minutes")?;
    let c = state
        .services
        .telemetry
        .compare(&state.services, &ctx, NodeId(node), dep, window)
        .await?;
    Ok(Json(to_json(&c)?))
}

/// The query of `listAppTelemetryFields`.
#[derive(Debug, Deserialize)]
pub struct FieldsQuery {
    pub unconfirmed: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// The telemetry fields with their classes. `GET /v1/apps/{node_id}/telemetry-fields`
/// (operationId `listAppTelemetryFields`).
pub async fn list_app_telemetry_fields(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    QueryParams(q): QueryParams<FieldsQuery>,
) -> ApiResult<Json<Value>> {
    let id = path_id(node)?;
    let unconfirmed = bool_param(q.unconfirmed.as_deref(), "unconfirmed")?;
    let page = Paging {
        cursor: q.cursor,
        limit: q.limit,
    }
    .page()?;
    let p = state
        .services
        .telemetry
        .list_fields(&state.services, &ctx, id, unconfirmed, page)
        .await?;
    Ok(Json(to_json(&p)?))
}

/// The body of `confirmAppTelemetryFields`.
#[derive(Debug, Deserialize)]
pub struct ConfirmBody {
    pub fields: Vec<ConfirmItem>,
}

/// A person confirms the classes of telemetry fields. `POST
/// /v1/apps/{node_id}/telemetry-fields/confirm` (operationId `confirmAppTelemetryFields`).
pub async fn confirm_app_telemetry_fields(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    JsonBody(b): JsonBody<ConfirmBody>,
) -> ApiResult<Json<Value>> {
    let id = path_id(node)?;
    let items = state
        .services
        .telemetry
        .confirm_fields(&state.services, &ctx, id, b.fields)
        .await?;
    Ok(Json(
        json!({ "items": to_json(&items)?, "next_cursor": null }),
    ))
}

/// The body of `computeAppPseudonym`. The value has no `Debug`, so it is never logged.
#[derive(Deserialize)]
pub struct PseudonymBody {
    pub field: String,
    pub value: String,
}

/// The telemetry pseudonym of a known value. `POST /v1/apps/{node_id}/pseudonyms`
/// (operationId `computeAppPseudonym`).
pub async fn compute_app_pseudonym(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    JsonBody(b): JsonBody<PseudonymBody>,
) -> ApiResult<Json<Value>> {
    let id = path_id(node)?;
    let p = state
        .services
        .telemetry
        .pseudonym(&state.services, &ctx, id, &b.field, &b.value)
        .await?;
    Ok(Json(p))
}
