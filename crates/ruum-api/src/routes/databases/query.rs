//! The query API (card Q-1): `getDatabaseSchema`, `queryDatabase` and `listDatabaseQueries`.
//! The logic is in `ruum_core::databases::query`; these handlers parse the request and render
//! the JSON of `contracts/openapi.yaml`.
//!
//! The step 2 error codes of the query API (`query_failed`, `read_only_query`,
//! `statement_not_allowed`, `database_not_ready`, `statement_timeout`, `write_grant_required`)
//! are codes of the shared `ErrorCode`; they render with [`crate::error::render`]: the standard
//! error body, the `sqlstate` of a failed statement and the `Retry-After` header of `503
//! database_not_ready`.

use crate::auth::{JsonBody, RequestContext};
use crate::error::{ApiResult, HttpError};
use crate::routes::nodes::{path_id, to_json};
use crate::state::AppState;
use axum::Router;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Json, Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use ruum_core::databases::engine::StatementMode;
use ruum_core::databases::query::{self, ListQueries, QueryError, QueryRequest};
use ruum_core::{ApiError, Cursor, Error, ErrorCode, PageRequest};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/databases/{node_id}/schema", get(get_database_schema))
        .route("/v1/databases/{node_id}/query", post(query_database))
        .route(
            "/v1/databases/{node_id}/queries",
            get(list_database_queries),
        )
}

fn answer(e: QueryError) -> Response {
    match e {
        QueryError::Core(e) => HttpError(e).into_response(),
        QueryError::Problem(p) => crate::error::render(p),
    }
}

/// The catalog of a database. `GET /v1/databases/{node_id}/schema` (operationId
/// `getDatabaseSchema`).
pub async fn get_database_schema(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
) -> ApiResult<Response> {
    let id = path_id(node)?;
    Ok(
        match query::schema(&state.services.databases, &ctx, id).await {
            Ok(s) => Json(to_json(&s)?).into_response(),
            Err(e) => answer(e),
        },
    )
}

/// Runs one statement. `POST /v1/databases/{node_id}/query` (operationId `queryDatabase`).
pub async fn query_database(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    JsonBody(b): JsonBody<QueryRequest>,
) -> ApiResult<Response> {
    let id = path_id(node)?;
    Ok(
        match query::query(&state.services.databases, &ctx, id, b).await {
            Ok(r) => Json(to_json(&r)?).into_response(),
            Err(e) => answer(e),
        },
    )
}

/// The query parameters of `listDatabaseQueries`.
#[derive(Debug, Default, Deserialize)]
pub struct QueriesParams {
    pub actor: Option<Uuid>,
    pub mode: Option<StatementMode>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// The query log of a database. `GET /v1/databases/{node_id}/queries` (operationId
/// `listDatabaseQueries`).
pub async fn list_database_queries(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    params: Result<Query<QueriesParams>, QueryRejection>,
) -> ApiResult<Json<serde_json::Value>> {
    let id = path_id(node)?;
    let Query(p) = params.map_err(|e| {
        Error::Api(
            ApiError::new(ErrorCode::BadRequest, format!("bad query parameters: {e}"))
                .with_hint("actor is a UUID, mode is read or write, limit is 1 to 1000"),
        )
    })?;
    if p.limit.is_some_and(|l| !(1..=1000).contains(&l)) {
        return Err(Error::Api(
            ApiError::new(ErrorCode::ValidationFailed, "limit is 1 to 1000").with_hint("/limit"),
        )
        .into());
    }
    let page = query::list_queries(
        &state.services.databases,
        &ctx,
        id,
        ListQueries {
            actor: p.actor,
            mode: p.mode,
        },
        PageRequest {
            cursor: p.cursor.filter(|c| !c.is_empty()).map(Cursor),
            limit: p.limit,
        },
    )
    .await?;
    let items = page
        .items
        .iter()
        .map(|e| to_json(&e.view()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(
        json!({ "items": items, "next_cursor": page.next_cursor }),
    ))
}
