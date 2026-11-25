//! The supervisor listener (backlog I6-20, S-89; `docs/STEP3.md` "The sandbox supervisor" 4):
//! the sandbox supervisor of each Kubernetes pod fetches its launch data here once
//! (`POST /v1/launch`) and the current workload token every 30 s (`POST /v1/token`), with its
//! projected service account token as the bearer. Its own port, only these two routes, no
//! request body, the peer address of the connection (never a header). The Kubernetes driver
//! makes every check (`ruum_runtime::KubeDriver::supervisor_launch`); this module only maps its
//! answer: `200` with the JSON, or `403`, `503`, `429` without a detail.

use axum::Router;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use ruum_runtime::KubeDriver;
use ruum_runtime::kube::LaunchRefusal;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// The longest a request may take (a `TokenReview` and a few reads of the API).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

pub fn router(driver: Arc<KubeDriver>) -> Router {
    Router::new()
        .route("/v1/launch", post(launch))
        .route("/v1/token", post(token))
        .layer(DefaultBodyLimit::max(1024))
        .with_state(driver)
}

fn bearer(headers: &HeaderMap) -> String {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default()
        .trim()
        .to_owned()
}

fn answer(r: Result<Result<Vec<u8>, LaunchRefusal>, tokio::time::error::Elapsed>) -> Response {
    match r {
        Ok(Ok(body)) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            body,
        )
            .into_response(),
        Ok(Err(LaunchRefusal::Forbidden)) => StatusCode::FORBIDDEN.into_response(),
        Ok(Err(LaunchRefusal::TooMany)) => StatusCode::TOO_MANY_REQUESTS.into_response(),
        Ok(Err(LaunchRefusal::NotReady)) | Err(_) => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

async fn launch(
    State(d): State<Arc<KubeDriver>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let t = bearer(&headers);
    answer(tokio::time::timeout(REQUEST_TIMEOUT, d.supervisor_launch(&t, peer.ip())).await)
}

async fn token(
    State(d): State<Arc<KubeDriver>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let t = bearer(&headers);
    answer(tokio::time::timeout(REQUEST_TIMEOUT, d.supervisor_token(&t, peer.ip())).await)
}
