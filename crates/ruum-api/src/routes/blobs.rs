//! Routes of the `blobs` area. See `docs/STEP1.md` for the owner of this file.

use crate::error::{HttpError, not_implemented};
use crate::state::AppState;
use axum::Router;
use axum::routing::{get, post, put};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/uploads", post(create_upload))
        .route(
            "/v1/uploads/{upload_id}",
            get(get_upload).delete(abort_upload),
        )
        .route("/v1/uploads/{upload_id}/chunks/{hash}", put(put_chunk))
        .route("/v1/uploads/{upload_id}/complete", post(complete_upload))
        .route(
            "/v1/nodes/{node_id}/content",
            get(download_content).put(put_content),
        )
}

/// Start a resumable upload. `POST /v1/uploads` (operationId `createUpload`).
pub async fn create_upload() -> HttpError {
    not_implemented("createUpload")
}

/// Get an upload and its missing chunks. `GET /v1/uploads/{upload_id}` (operationId `getUpload`).
pub async fn get_upload() -> HttpError {
    not_implemented("getUpload")
}

/// Abort an upload. `DELETE /v1/uploads/{upload_id}` (operationId `abortUpload`).
pub async fn abort_upload() -> HttpError {
    not_implemented("abortUpload")
}

/// Upload one chunk. `PUT /v1/uploads/{upload_id}/chunks/{hash}` (operationId `putChunk`).
pub async fn put_chunk() -> HttpError {
    not_implemented("putChunk")
}

/// Complete an upload. `POST /v1/uploads/{upload_id}/complete` (operationId `completeUpload`).
pub async fn complete_upload() -> HttpError {
    not_implemented("completeUpload")
}

/// Download file content. `GET /v1/nodes/{node_id}/content` (operationId `downloadContent`).
pub async fn download_content() -> HttpError {
    not_implemented("downloadContent")
}

/// Replace file content in one request. `PUT /v1/nodes/{node_id}/content` (operationId `putContent`).
pub async fn put_content() -> HttpError {
    not_implemented("putContent")
}
