//! The two routers of the server (decision 0002-3):
//!
//! - the **platform router** on `RUUM_BIND`: the REST API (`/v1`), MCP (`/mcp`), the content
//!   domain and the health check. It has no WebDAV routes, so `/dav` is `404` there: Basic
//!   credentials never go to the origin of the web UI.
//! - the **WebDAV router** on `RUUM_DAV_BIND`, for the WebDAV host (`https://dav.<domain>/`). It
//!   has only WebDAV routes, at `/`, and never reads or sets cookies.

use ruum_api::AppState;
use std::time::Duration;

/// A request that has no answer after the timeout gets `408`. The longest long poll
/// (`listChanges` with `wait`) is 30 seconds; streamed bodies are not cut (the timeout ends
/// when the answer starts).
fn timeout(t: Duration) -> tower_http::timeout::TimeoutLayer {
    tower_http::timeout::TimeoutLayer::with_status_code(axum::http::StatusCode::REQUEST_TIMEOUT, t)
}

/// The router of the platform origin: REST API and MCP, without WebDAV.
pub fn platform(state: AppState, request_timeout: Duration) -> axum::Router {
    ruum_api::router(state.clone())
        .merge(ruum_mcp::router(state))
        .layer(timeout(request_timeout))
}

/// The router of the WebDAV host: WebDAV only.
pub fn dav(state: &AppState, request_timeout: Duration) -> axum::Router {
    ruum_webdav::router(state.services.clone()).layer(timeout(request_timeout))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    async fn status(app: &axum::Router, method: &str, path: &str) -> StatusCode {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", "Basic YUBiLmM6cnV1bV9hcHBfeA==")
            .header("depth", "0")
            .body(Body::empty())
            .unwrap();
        app.clone().oneshot(req).await.unwrap().status()
    }

    /// The platform origin does not serve WebDAV any more: `/dav` is `404`, not a redirect, so
    /// a WebDAV client never sends Basic credentials to it again.
    #[tokio::test]
    async fn the_platform_origin_has_no_webdav() {
        let app = platform(AppState::in_memory(), Duration::from_secs(60));
        for (method, path) in [
            ("PROPFIND", "/dav/"),
            ("PROPFIND", "/dav"),
            ("GET", "/dav/Projects/plan.txt"),
            ("PUT", "/dav/x.txt"),
            ("OPTIONS", "/dav/"),
        ] {
            assert_eq!(
                status(&app, method, path).await,
                StatusCode::NOT_FOUND,
                "{method} {path}"
            );
        }
        // The API is still there.
        assert_eq!(status(&app, "GET", "/healthz").await, StatusCode::OK);
    }

    /// The WebDAV host has no REST API and no MCP: without valid credentials every path asks
    /// for WebDAV sign-in (Basic or an agent token), and nothing answers as the API.
    #[tokio::test]
    async fn the_dav_host_has_no_api() {
        let app = dav(&AppState::in_memory(), Duration::from_secs(60));
        for (method, path) in [("GET", "/v1/me"), ("POST", "/mcp"), ("GET", "/healthz")] {
            assert_eq!(
                status(&app, method, path).await,
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
        }
    }
}
