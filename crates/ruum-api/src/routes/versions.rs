//! Routes of the `versions` area. See `docs/STEP1.md` for the owner of this file.

use crate::error::{HttpError, not_implemented};
use crate::state::AppState;
use axum::Router;
use axum::routing::{get, post, put};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/nodes/{node_id}/versions", get(list_versions))
        .route(
            "/v1/nodes/{node_id}/versions/{version_id}",
            get(get_version),
        )
        .route(
            "/v1/nodes/{node_id}/versions/{version_id}/restore",
            post(restore_version),
        )
        .route(
            "/v1/nodes/{node_id}/versions/{version_id}/label",
            put(set_version_label),
        )
        .route("/v1/nodes/{node_id}/diff", get(diff_versions))
        .route("/v1/nodes/{node_id}/diff/plugins", get(list_diff_plugins))
}

/// List versions. `GET /v1/nodes/{node_id}/versions` (operationId `listVersions`).
pub async fn list_versions() -> HttpError {
    not_implemented("listVersions")
}

/// Get a version. `GET /v1/nodes/{node_id}/versions/{version_id}` (operationId `getVersion`).
pub async fn get_version() -> HttpError {
    not_implemented("getVersion")
}

/// Restore a version. `POST /v1/nodes/{node_id}/versions/{version_id}/restore` (operationId `restoreVersion`).
pub async fn restore_version() -> HttpError {
    not_implemented("restoreVersion")
}

/// Name a version. `PUT /v1/nodes/{node_id}/versions/{version_id}/label` (operationId `setVersionLabel`).
pub async fn set_version_label() -> HttpError {
    not_implemented("setVersionLabel")
}

/// Compare two versions. `GET /v1/nodes/{node_id}/diff` (operationId `diffVersions`).
pub async fn diff_versions() -> HttpError {
    not_implemented("diffVersions")
}

/// Diff plugins that match a node. `GET /v1/nodes/{node_id}/diff/plugins` (operationId `listDiffPlugins`).
pub async fn list_diff_plugins() -> HttpError {
    not_implemented("listDiffPlugins")
}
