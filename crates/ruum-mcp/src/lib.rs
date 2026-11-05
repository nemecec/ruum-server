//! The Ruum MCP server (`docs/DESIGN.md`, section 11): a remote MCP endpoint at `/mcp` with
//! OAuth, built with `rmcp` (the official Rust MCP SDK). Each tool maps to one REST operation of
//! `contracts/openapi.yaml`, and calls the same core services, so the MCP face never has rights
//! that the REST face does not have.
//!
//! Owned by the MCP server + CLI area. This is a stub: `/mcp` answers `501`.

use axum::Router;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::any;
use ruum_core::Services;
use serde_json::json;

/// The path of the MCP endpoint.
pub const MCP_PATH: &str = "/mcp";

/// The MCP router, to merge into the server.
pub fn router(_services: Services) -> Router {
    Router::new().route(MCP_PATH, any(not_implemented))
}

async fn not_implemented() -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        axum::Json(json!({ "error": {
            "code": "not_implemented",
            "message": "the MCP server is not implemented yet",
            "hint": "use the REST API at /v1 for now"
        }})),
    )
}
