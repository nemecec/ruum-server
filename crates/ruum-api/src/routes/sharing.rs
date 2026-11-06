//! Routes of the `sharing` area. See `docs/STEP1.md` for the owner of this file.

use crate::error::{HttpError, not_implemented};
use crate::state::AppState;
use axum::Router;
use axum::routing::{delete, get, patch, post};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/nodes/{node_id}/grants",
            get(list_grants).post(add_grant),
        )
        .route(
            "/v1/nodes/{node_id}/grants/{grant_id}",
            patch(update_grant).delete(remove_grant),
        )
        .route(
            "/v1/nodes/{node_id}/share-links",
            get(list_share_links).post(create_share_link),
        )
        .route("/v1/nodes/{node_id}/sharing", patch(update_node_sharing))
        .route("/v1/share-links/{link_id}", delete(revoke_share_link))
        .route("/v1/share-links/resolve", post(resolve_share_link))
        .route("/v1/shared-with-me", get(list_shared_with_me))
}

/// List grants. `GET /v1/nodes/{node_id}/grants` (operationId `listGrants`).
pub async fn list_grants() -> HttpError {
    not_implemented("listGrants")
}

/// Add a grant. `POST /v1/nodes/{node_id}/grants` (operationId `addGrant`).
pub async fn add_grant() -> HttpError {
    not_implemented("addGrant")
}

/// Change the role of a grant. `PATCH /v1/nodes/{node_id}/grants/{grant_id}` (operationId
/// `updateGrant`).
pub async fn update_grant() -> HttpError {
    not_implemented("updateGrant")
}

/// Remove a grant. `DELETE /v1/nodes/{node_id}/grants/{grant_id}` (operationId `removeGrant`).
pub async fn remove_grant() -> HttpError {
    not_implemented("removeGrant")
}

/// Change the sharing settings of a node (`editors_can_share`). `PATCH /v1/nodes/{node_id}/sharing`
/// (operationId `updateNodeSharing`).
pub async fn update_node_sharing() -> HttpError {
    not_implemented("updateNodeSharing")
}

/// List share links. `GET /v1/nodes/{node_id}/share-links` (operationId `listShareLinks`).
pub async fn list_share_links() -> HttpError {
    not_implemented("listShareLinks")
}

/// Create a share link. `POST /v1/nodes/{node_id}/share-links` (operationId `createShareLink`).
pub async fn create_share_link() -> HttpError {
    not_implemented("createShareLink")
}

/// Revoke a share link. `DELETE /v1/share-links/{link_id}` (operationId `revokeShareLink`).
pub async fn revoke_share_link() -> HttpError {
    not_implemented("revokeShareLink")
}

/// Open a share link. `POST /v1/share-links/resolve` (operationId `resolveShareLink`).
pub async fn resolve_share_link() -> HttpError {
    not_implemented("resolveShareLink")
}

/// Shared with me. `GET /v1/shared-with-me` (operationId `listSharedWithMe`).
pub async fn list_shared_with_me() -> HttpError {
    not_implemented("listSharedWithMe")
}
