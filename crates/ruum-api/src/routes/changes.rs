//! Routes of the `changes` area (journal and audit, `docs/STEP1.md` area 5).
//!
//! The logic is in `ruum_core::journal::feed`; these handlers parse the parameters and render
//! the JSON.

use crate::auth::RequestContext;
use crate::error::ApiResult;
use crate::state::AppState;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use ruum_core::journal::Journal;
use ruum_core::journal::feed::{
    self, ChangesPage, ChangesQuery, FeedAccess, LatestCursor, ServicesAccess,
};
use ruum_core::{ApiError, Context, Error, ErrorCode, NodeId};
use serde::Deserialize;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/changes", get(list_changes))
        .route("/v1/changes/latest", get(get_latest_cursor))
}

/// The query parameters of `listChanges`.
#[derive(Debug, Default, Deserialize)]
pub struct ChangesParams {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
    pub wait: Option<u64>,
    pub node: Option<Uuid>,
}

/// A query string that does not parse: `400 bad_request` with the standard error JSON.
pub(crate) fn bad_query(e: QueryRejection) -> Error {
    Error::Api(
        ApiError::new(ErrorCode::BadRequest, format!("bad query parameters: {e}"))
            .with_hint("see the parameters of this operation in contracts/openapi.yaml"),
    )
}

/// The changes feed. `GET /v1/changes` (operationId `listChanges`).
pub async fn list_changes(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    params: Result<Query<ChangesParams>, QueryRejection>,
) -> ApiResult<Json<ChangesPage>> {
    let access = ServicesAccess {
        services: &state.services,
        ctx: &ctx,
    };
    changes_page(state.services.journal.as_ref(), &access, &ctx, params).await
}

/// The body of `listChanges` for a caller, with the access answers of `access`.
pub(crate) async fn changes_page(
    journal: &dyn Journal,
    access: &dyn FeedAccess,
    ctx: &Context,
    params: Result<Query<ChangesParams>, QueryRejection>,
) -> ApiResult<Json<ChangesPage>> {
    let Query(p) = params.map_err(bad_query)?;
    let query =
        ChangesQuery::from_params(p.cursor.as_deref(), p.limit, p.wait, p.node.map(NodeId))?;
    Ok(Json(feed::list_changes(journal, access, ctx, query).await?))
}

/// The newest cursor. `GET /v1/changes/latest` (operationId `getLatestCursor`).
pub async fn get_latest_cursor(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
) -> ApiResult<Json<LatestCursor>> {
    Ok(Json(
        feed::latest_cursor(state.services.journal.as_ref(), &ctx).await?,
    ))
}

#[cfg(test)]
mod tests;
