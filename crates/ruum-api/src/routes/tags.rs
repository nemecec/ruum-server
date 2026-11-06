//! Routes of the `tags` area (`docs/STEP1.md`, area 1). The logic is in `ruum_core::tags`.

use super::nodes::{change_only, path_id, refuse_share_link, to_json};
use crate::auth::{JsonBody, Paging, QueryParams, RequestContext};
use crate::error::ApiResult;
use crate::state::AppState;
use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use ruum_core::tags::TagInput;
use ruum_core::{ApiError, Error, ErrorCode, TagId};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/tags", get(list_tags).post(create_tag))
        .route(
            "/v1/tags/{tag_id}",
            get(get_tag).patch(update_tag).delete(delete_tag),
        )
        .route("/v1/tags/{tag_id}/nodes", get(list_nodes_by_tag))
        .route("/v1/nodes/{node_id}/tags", get(list_node_tags))
        .route(
            "/v1/nodes/{node_id}/tags/{tag_id}",
            put(attach_tag).delete(detach_tag),
        )
}

fn bad_path(e: PathRejection) -> Error {
    Error::Api(
        ApiError::new(ErrorCode::BadRequest, format!("bad path: {e}")).with_hint("IDs are UUIDs"),
    )
}

fn tag_id(p: Result<Path<Uuid>, PathRejection>) -> Result<TagId, Error> {
    p.map(|Path(v)| TagId(v)).map_err(bad_path)
}

fn tag_write(w: &ruum_core::journal::Written<ruum_core::tags::Tag>) -> Result<Json<Value>, Error> {
    Ok(Json(
        json!({ "tag": to_json(&w.value)?, "change": to_json(&w.change)? }),
    ))
}

/// List tags. `GET /v1/tags` (operationId `listTags`).
pub async fn list_tags(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    QueryParams(q): QueryParams<Paging>,
) -> ApiResult<Json<Value>> {
    refuse_share_link(&ctx)?;
    let p = state.services.tags.list(&ctx, q.page()?).await?;
    Ok(Json(to_json(&p)?))
}

/// The body of `createTag`.
#[derive(Debug, Deserialize)]
pub struct CreateTagBody {
    pub name: String,
    pub color: Option<String>,
}

/// Create a tag. `POST /v1/tags` (operationId `createTag`).
pub async fn create_tag(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    JsonBody(b): JsonBody<CreateTagBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    refuse_share_link(&ctx)?;
    let w = state.services.tags.create(&ctx, b.name, b.color).await?;
    Ok((StatusCode::CREATED, tag_write(&w)?))
}

/// Get a tag. `GET /v1/tags/{tag_id}` (operationId `getTag`).
pub async fn get_tag(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    tag: Result<Path<Uuid>, PathRejection>,
) -> ApiResult<Json<Value>> {
    refuse_share_link(&ctx)?;
    let t = state.services.tags.get(&ctx, tag_id(tag)?).await?;
    Ok(Json(to_json(&t)?))
}

/// Change a tag. `PATCH /v1/tags/{tag_id}` (operationId `updateTag`).
pub async fn update_tag(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    tag: Result<Path<Uuid>, PathRejection>,
    JsonBody(b): JsonBody<TagInput>,
) -> ApiResult<Json<Value>> {
    refuse_share_link(&ctx)?;
    let id = tag_id(tag)?;
    if b.is_empty() {
        return Err(Error::Api(
            ApiError::new(
                ErrorCode::ValidationFailed,
                "send at least one of name and color",
            )
            .with_hint("send name and/or color (null removes the color)"),
        )
        .into());
    }
    let w = state.services.tags.update(&ctx, id, b).await?;
    Ok(tag_write(&w)?)
}

/// Delete a tag. `DELETE /v1/tags/{tag_id}` (operationId `deleteTag`).
pub async fn delete_tag(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    tag: Result<Path<Uuid>, PathRejection>,
) -> ApiResult<StatusCode> {
    refuse_share_link(&ctx)?;
    state.services.tags.delete(&ctx, tag_id(tag)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Nodes with a tag. `GET /v1/tags/{tag_id}/nodes` (operationId `listNodesByTag`).
pub async fn list_nodes_by_tag(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    tag: Result<Path<Uuid>, PathRejection>,
    QueryParams(q): QueryParams<Paging>,
) -> ApiResult<Json<Value>> {
    refuse_share_link(&ctx)?;
    let p = state
        .services
        .tags
        .nodes_with(&ctx, tag_id(tag)?, q.page()?)
        .await?;
    Ok(Json(to_json(&p)?))
}

/// Tags of a node. `GET /v1/nodes/{node_id}/tags` (operationId `listNodeTags`).
pub async fn list_node_tags(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
) -> ApiResult<Json<Value>> {
    refuse_share_link(&ctx)?;
    let tags = state.services.tags.tags_of(&ctx, path_id(node)?).await?;
    Ok(Json(json!({ "items": to_json(&tags)? })))
}

fn node_and_tag(
    p: Result<Path<(Uuid, Uuid)>, PathRejection>,
) -> Result<(ruum_core::NodeId, TagId), Error> {
    p.map(|Path((n, t))| (ruum_core::NodeId(n), TagId(t)))
        .map_err(bad_path)
}

/// Attach a tag. `PUT /v1/nodes/{node_id}/tags/{tag_id}` (operationId `attachTag`).
pub async fn attach_tag(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    ids: Result<Path<(Uuid, Uuid)>, PathRejection>,
) -> ApiResult<Json<Value>> {
    refuse_share_link(&ctx)?;
    let (node, tag) = node_and_tag(ids)?;
    let w = state.services.tags.attach(&ctx, node, tag).await?;
    Ok(change_only(&w.change)?)
}

/// Detach a tag. `DELETE /v1/nodes/{node_id}/tags/{tag_id}` (operationId `detachTag`).
pub async fn detach_tag(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    ids: Result<Path<(Uuid, Uuid)>, PathRejection>,
) -> ApiResult<Json<Value>> {
    refuse_share_link(&ctx)?;
    let (node, tag) = node_and_tag(ids)?;
    let w = state.services.tags.detach(&ctx, node, tag).await?;
    Ok(change_only(&w.change)?)
}
