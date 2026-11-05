//! Routes of the `tags` area. See `docs/STEP1.md` for the owner of this file.

use crate::error::{HttpError, not_implemented};
use crate::state::AppState;
use axum::Router;
use axum::routing::{get, put};

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

/// List tags. `GET /v1/tags` (operationId `listTags`).
pub async fn list_tags() -> HttpError {
    not_implemented("listTags")
}

/// Create a tag. `POST /v1/tags` (operationId `createTag`).
pub async fn create_tag() -> HttpError {
    not_implemented("createTag")
}

/// Get a tag. `GET /v1/tags/{tag_id}` (operationId `getTag`).
pub async fn get_tag() -> HttpError {
    not_implemented("getTag")
}

/// Change a tag. `PATCH /v1/tags/{tag_id}` (operationId `updateTag`).
pub async fn update_tag() -> HttpError {
    not_implemented("updateTag")
}

/// Delete a tag. `DELETE /v1/tags/{tag_id}` (operationId `deleteTag`).
pub async fn delete_tag() -> HttpError {
    not_implemented("deleteTag")
}

/// Nodes with a tag. `GET /v1/tags/{tag_id}/nodes` (operationId `listNodesByTag`).
pub async fn list_nodes_by_tag() -> HttpError {
    not_implemented("listNodesByTag")
}

/// Tags of a node. `GET /v1/nodes/{node_id}/tags` (operationId `listNodeTags`).
pub async fn list_node_tags() -> HttpError {
    not_implemented("listNodeTags")
}

/// Attach a tag. `PUT /v1/nodes/{node_id}/tags/{tag_id}` (operationId `attachTag`).
pub async fn attach_tag() -> HttpError {
    not_implemented("attachTag")
}

/// Detach a tag. `DELETE /v1/nodes/{node_id}/tags/{tag_id}` (operationId `detachTag`).
pub async fn detach_tag() -> HttpError {
    not_implemented("detachTag")
}
