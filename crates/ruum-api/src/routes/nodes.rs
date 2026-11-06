//! Routes of the `nodes` area. See `docs/STEP1.md` for the owner of this file.

use crate::error::{HttpError, not_implemented};
use crate::state::AppState;
use axum::Router;
use axum::routing::{get, post};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/nodes/root", get(get_root))
        .route("/v1/nodes", post(create_node))
        .route("/v1/nodes/by-path/{*path}", get(get_node_by_path))
        .route("/v1/nodes/{node_id}", get(get_node).delete(purge_node))
        .route("/v1/nodes/{node_id}/children", get(list_children))
        .route("/v1/nodes/{node_id}/rename", post(rename_node))
        .route("/v1/nodes/{node_id}/move", post(move_node))
        .route("/v1/nodes/{node_id}/copy", post(copy_node))
        .route("/v1/nodes/{node_id}/trash", post(trash_node))
        .route("/v1/nodes/{node_id}/restore", post(restore_node))
        .route("/v1/trash", get(list_trash))
        .route("/v1/trash/empty", post(empty_trash))
}

/// Delete a trashed node permanently. `DELETE /v1/nodes/{node_id}` (operationId `purgeNode`).
pub async fn purge_node() -> HttpError {
    not_implemented("purgeNode")
}

/// Empty the trash. `POST /v1/trash/empty` (operationId `emptyTrash`).
pub async fn empty_trash() -> HttpError {
    not_implemented("emptyTrash")
}

/// The root folder of the tenant. `GET /v1/nodes/root` (operationId `getRoot`).
pub async fn get_root() -> HttpError {
    not_implemented("getRoot")
}

/// Create a folder, an empty file or a link. `POST /v1/nodes` (operationId `createNode`).
pub async fn create_node() -> HttpError {
    not_implemented("createNode")
}

/// Get a node by its path. `GET /v1/nodes/by-path/{path}` (operationId `getNodeByPath`).
pub async fn get_node_by_path() -> HttpError {
    not_implemented("getNodeByPath")
}

/// Get a node. `GET /v1/nodes/{node_id}` (operationId `getNode`).
pub async fn get_node() -> HttpError {
    not_implemented("getNode")
}

/// List the children of a folder. `GET /v1/nodes/{node_id}/children` (operationId `listChildren`).
pub async fn list_children() -> HttpError {
    not_implemented("listChildren")
}

/// Rename a node. `POST /v1/nodes/{node_id}/rename` (operationId `renameNode`).
pub async fn rename_node() -> HttpError {
    not_implemented("renameNode")
}

/// Move a node. `POST /v1/nodes/{node_id}/move` (operationId `moveNode`).
pub async fn move_node() -> HttpError {
    not_implemented("moveNode")
}

/// Copy a node. `POST /v1/nodes/{node_id}/copy` (operationId `copyNode`).
pub async fn copy_node() -> HttpError {
    not_implemented("copyNode")
}

/// Move a node to the trash. `POST /v1/nodes/{node_id}/trash` (operationId `trashNode`).
pub async fn trash_node() -> HttpError {
    not_implemented("trashNode")
}

/// Restore a node from the trash. `POST /v1/nodes/{node_id}/restore` (operationId `restoreNode`).
pub async fn restore_node() -> HttpError {
    not_implemented("restoreNode")
}

/// List the trash. `GET /v1/trash` (operationId `listTrash`).
pub async fn list_trash() -> HttpError {
    not_implemented("listTrash")
}
