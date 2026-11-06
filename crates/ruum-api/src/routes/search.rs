//! Routes of the `search` area (`docs/STEP1.md`, area 1). The logic is in `ruum_core::search`.

use super::nodes::{parse_kind, refuse_share_link, to_json};
use crate::auth::{Paging, QueryParams, RequestContext};
use crate::error::ApiResult;
use crate::state::AppState;
use axum::Router;
use axum::extract::{Json, State};
use axum::routing::get;
use ruum_core::search::SearchQuery;
use ruum_core::{ApiError, Error, ErrorCode, NodeId, TagId};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/search", get(search_nodes))
}

/// The query of `searchNodes`.
#[derive(Debug, Default, Deserialize)]
pub struct SearchParams {
    pub q: Option<String>,
    pub kind: Option<String>,
    pub tag: Option<Uuid>,
    pub within: Option<Uuid>,
    // Not `#[serde(flatten)]`: a flattened query reads every value as a string, so a number
    // (`limit=10`) or a bool would fail.
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// Search by name. `GET /v1/search` (operationId `searchNodes`).
pub async fn search_nodes(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    QueryParams(p): QueryParams<SearchParams>,
) -> ApiResult<Json<Value>> {
    refuse_share_link(&ctx)?;
    let Some(q) = p.q else {
        return Err(Error::Api(
            ApiError::new(ErrorCode::ValidationFailed, "q is required")
                .with_hint("send part of a node name in q"),
        )
        .into());
    };
    let query = SearchQuery {
        q,
        kind: parse_kind(p.kind.as_deref())?,
        tag: p.tag.map(TagId),
        within: p.within.map(NodeId),
        page: Paging {
            cursor: p.cursor.clone(),
            limit: p.limit,
        }
        .page()?,
    };
    let page = state.services.search.by_name(&ctx, query).await?;
    Ok(Json(to_json(&page)?))
}
