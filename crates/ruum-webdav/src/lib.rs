//! WebDAV face of the node tree (decision 12), built on `dav-server`. It maps WebDAV paths to
//! nodes of one tenant and uses the same core services as the REST API, so permissions, versions
//! and the journal work the same way. Mounted at `/dav`.
//!
//! Owned by the WebDAV area. This is a stub: `/dav` answers `501`.

use axum::Router;
use axum::http::StatusCode;
use axum::routing::any;
use ruum_core::Services;

/// The path prefix of the WebDAV endpoint.
pub const DAV_PATH: &str = "/dav";

/// The WebDAV router, to merge into the server.
pub fn router(_services: Services) -> Router {
    Router::new()
        .route(DAV_PATH, any(not_implemented))
        .route("/dav/{*path}", any(not_implemented))
}

async fn not_implemented() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}
