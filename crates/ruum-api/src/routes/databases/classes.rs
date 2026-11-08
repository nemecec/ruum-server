//! Column classes (card K-1): `listDatabaseColumns`, `suggestColumnClasses` and
//! `confirmColumnClasses`. The logic is in `ruum_core::databases::classes`.

use crate::auth::{JsonBody, QueryParams, RequestContext};
use crate::error::ApiResult;
use crate::routes::nodes::{if_match, path_id, to_json};
use crate::state::AppState;
use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{Json, Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use ruum_core::databases::classes::{ConfirmRequest, SuggestRequest};
use ruum_core::{Cursor, PageRequest};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/databases/{node_id}/columns",
            get(list_database_columns),
        )
        .route(
            "/v1/databases/{node_id}/columns/suggestions",
            post(suggest_column_classes),
        )
        .route(
            "/v1/databases/{node_id}/columns/confirm",
            post(confirm_column_classes),
        )
}

/// The query of `listDatabaseColumns`.
#[derive(Debug, Default, Deserialize)]
pub struct ListColumnsQuery {
    pub unconfirmed: Option<bool>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// The data class of every column. `GET /v1/databases/{node_id}/columns` (operationId
/// `listDatabaseColumns`).
pub async fn list_database_columns(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    QueryParams(q): QueryParams<ListColumnsQuery>,
) -> ApiResult<Json<Value>> {
    let id = path_id(node)?;
    let page = state
        .services
        .databases
        .list_columns(
            &ctx,
            id,
            q.unconfirmed.unwrap_or(false),
            PageRequest {
                cursor: q.cursor.map(Cursor),
                limit: q.limit,
            },
        )
        .await?;
    Ok(Json(to_json(&page)?))
}

/// Suggest data classes for columns. `POST /v1/databases/{node_id}/columns/suggestions`
/// (operationId `suggestColumnClasses`).
pub async fn suggest_column_classes(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    JsonBody(b): JsonBody<SuggestRequest>,
) -> ApiResult<Json<Value>> {
    let id = path_id(node)?;
    let items = state
        .services
        .databases
        .suggest_columns(&ctx, &*state.services.audit, id, b)
        .await?;
    Ok(Json(json!({ "items": to_json(&items)? })))
}

/// Confirm the data classes of columns. `POST /v1/databases/{node_id}/columns/confirm`
/// (operationId `confirmColumnClasses`).
pub async fn confirm_column_classes(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    headers: HeaderMap,
    JsonBody(b): JsonBody<ConfirmRequest>,
) -> ApiResult<Json<Value>> {
    let id = path_id(node)?;
    let w = state
        .services
        .databases
        .confirm_columns(&ctx, &state.config.public_url, id, b, if_match(&headers)?)
        .await?;
    Ok(Json(
        json!({ "database": to_json(&w.value)?, "change": to_json(&w.change)? }),
    ))
}
