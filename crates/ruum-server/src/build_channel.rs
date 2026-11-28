//! The build channel (card I6-13; `ruum_runtime::builder::kube`): the build pods of the
//! Kubernetes builder fetch their source here once (`GET /v1/builds/<build>/source`) and send
//! their output tar once (`PUT /v1/builds/<build>/output`), with the build's own token as the
//! bearer. Its own port, only these two routes, the peer address of the connection (never a
//! header). The builder makes every check (`KubeBuilder::channel_source`, `channel_output`):
//! the token, the caller's address against the build pod's, once only, the output cap. This
//! module only maps the answer: `200` with the tar, `204`, or `403`, `404`, `409`, `413`, `503`
//! without a detail.
//!
//! TLS 1.3 with the supervisor listener's identity (derived from the record key, backlog
//! I620-2); the build pods pin its certificate. The callers run untrusted code, so the
//! connections have the supervisor listener's caps ([`crate::supervisor::serve`]) with a longer
//! lifetime for the transfer of an output up to the builder's cap.

use crate::supervisor::Caps;
use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use ruum_runtime::builder::kube::KubeBuilder;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// The connection caps of the build channel: fewer connections than the supervisor listener
/// (a build holds at most one at a time), and a lifetime long enough for an output of the cap.
pub fn caps() -> Caps {
    Caps {
        connections: 256,
        per_peer: 2,
        header_timeout: Duration::from_secs(5),
        lifetime: Duration::from_secs(600),
    }
}

pub fn router(builder: Arc<KubeBuilder>) -> Router {
    Router::new()
        .route("/v1/builds/{build}/source", get(source))
        .route("/v1/builds/{build}/output", put(output))
        // The builder reads the output under its own cap.
        .layer(DefaultBodyLimit::disable())
        .with_state(builder)
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

fn refused(status: u16) -> Response {
    StatusCode::from_u16(status)
        .unwrap_or(StatusCode::FORBIDDEN)
        .into_response()
}

async fn source(
    State(builder): State<Arc<KubeBuilder>>,
    Path(build): Path<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    match builder
        .channel_source(&build, &bearer(&headers), peer.ip())
        .await
    {
        Ok(tar) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/x-tar"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            tar,
        )
            .into_response(),
        Err(r) => refused(r.status()),
    }
}

async fn output(
    State(builder): State<Arc<KubeBuilder>>,
    Path(build): Path<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    match builder
        .channel_output(
            &build,
            &bearer(&headers),
            peer.ip(),
            body.into_data_stream(),
        )
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(r) => refused(r.status()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruum_core::runtime::MemoryUsage;
    use ruum_runtime::NoEgress;
    use ruum_runtime::builder::kube::{ChannelTls, KubeBuilderConfig};
    use tower::ServiceExt as _;

    fn builder() -> Arc<KubeBuilder> {
        let cfg = KubeBuilderConfig::new(
            "https://127.0.0.1:1",
            "registry.ruum.internal:5000",
            "http://127.0.0.1:1",
            "10.0.0.2",
            8092,
            ChannelTls {
                cert_der: vec![1, 2, 3],
                pin: format!("sha256:{}", "0".repeat(64)),
            },
        );
        Arc::new(
            KubeBuilder::new(cfg, Arc::new(MemoryUsage::new()), Arc::new(NoEgress))
                .expect("builder"),
        )
    }

    async fn call(method: &str, path: &str, auth: Option<&str>) -> StatusCode {
        let mut req = axum::http::Request::builder().method(method).uri(path);
        if let Some(a) = auth {
            req = req.header(header::AUTHORIZATION, a);
        }
        let mut req = req.body(Body::empty()).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo::<SocketAddr>("10.0.0.9:4000".parse().unwrap()));
        router(builder()).oneshot(req).await.unwrap().status()
    }

    #[tokio::test]
    async fn a_build_that_does_not_run_is_not_found_and_other_routes_do_not_exist() {
        let id = uuid::Uuid::now_v7();
        for (m, p) in [
            ("GET", format!("/v1/builds/{id}/source")),
            ("PUT", format!("/v1/builds/{id}/output")),
            ("GET", "/v1/builds/not-a-uuid/source".to_owned()),
        ] {
            assert_eq!(
                call(m, &p, Some("Bearer x")).await,
                StatusCode::NOT_FOUND,
                "{m} {p}"
            );
            assert_eq!(call(m, &p, None).await, StatusCode::NOT_FOUND, "{m} {p}");
        }
        // Only the two routes and their methods.
        assert_eq!(
            call("POST", &format!("/v1/builds/{id}/source"), None).await,
            StatusCode::METHOD_NOT_ALLOWED
        );
        assert_eq!(call("GET", "/v1/launch", None).await, StatusCode::NOT_FOUND);
    }
}
