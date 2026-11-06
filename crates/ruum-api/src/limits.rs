//! Rate limits per client IP for the operations that need no sign-in and write or check a
//! secret: `registerOAuthClient` (`POST /v1/oauth/register`, one stored row per call),
//! `exchangeToken` (`POST /v1/oauth/token`) and `resolveShareLink`
//! (`POST /v1/share-links/resolve`, link passwords). The contract gives these three a `429`
//! answer (`RateLimited`).
//!
//! Each operation has a fixed window of one minute per client IP. The client IP is the peer
//! address of the connection (`ConnectInfo<SocketAddr>`, which `ruum-server` sets). When the peer
//! is a proxy on a loopback or private address (Caddy), the last address of `X-Forwarded-For`
//! (the one that proxy added) is used. A request without a known peer (tests that call the
//! router directly) is not limited.

use crate::error::render;
use axum::extract::{ConnectInfo, Request};
use axum::http::{HeaderMap, HeaderValue, Method, header};
use axum::middleware::Next;
use axum::response::Response;
use ruum_core::{ApiError, ErrorCode};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The length of a window.
pub const WINDOW: Duration = Duration::from_secs(60);

/// The limited operations and their requests per client IP and window.
pub const LIMITS: &[(&str, u32)] = &[
    ("/v1/oauth/register", 10),
    ("/v1/oauth/token", 60),
    ("/v1/share-links/resolve", 30),
];

/// When the table holds this many windows, the ended ones are dropped.
const PRUNE_AT: usize = 10_000;

#[derive(Clone, Copy)]
struct Window {
    start: Instant,
    count: u32,
}

/// The counters of the limited operations.
#[derive(Default)]
pub struct RateLimiter {
    windows: Mutex<HashMap<(&'static str, IpAddr), Window>>,
}

impl RateLimiter {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Counts a request; returns the seconds to wait when the limit is reached.
    fn hit(&self, path: &'static str, limit: u32, ip: IpAddr, now: Instant) -> Option<u64> {
        let mut w = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        if w.len() >= PRUNE_AT {
            w.retain(|_, x| now.duration_since(x.start) < WINDOW);
        }
        let x = w.entry((path, ip)).or_insert(Window {
            start: now,
            count: 0,
        });
        if now.duration_since(x.start) >= WINDOW {
            *x = Window {
                start: now,
                count: 0,
            };
        }
        if x.count >= limit {
            let left = WINDOW.saturating_sub(now.duration_since(x.start));
            return Some(left.as_secs().max(1));
        }
        x.count += 1;
        None
    }
}

fn is_proxy(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private(),
        IpAddr::V6(v6) => v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00,
    }
}

/// The client IP of a request whose peer is `peer`.
pub fn client_ip(peer: IpAddr, headers: &HeaderMap) -> IpAddr {
    if !is_proxy(peer) {
        return peer;
    }
    headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|s| s.trim().parse::<IpAddr>().ok())
        .next_back()
        .unwrap_or(peer)
}

/// The middleware. Add it with `axum::middleware::from_fn_with_state(limiter, rate_limit)`.
pub async fn rate_limit(
    axum::extract::State(limiter): axum::extract::State<Arc<RateLimiter>>,
    req: Request,
    next: Next,
) -> Response {
    if req.method() != Method::POST {
        return next.run(req).await;
    }
    let Some(&(path, limit)) = LIMITS.iter().find(|(p, _)| *p == req.uri().path()) else {
        return next.run(req).await;
    };
    let Some(peer) = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
    else {
        return next.run(req).await;
    };
    let ip = client_ip(peer, req.headers());
    match limiter.hit(path, limit, ip, Instant::now()) {
        None => next.run(req).await,
        Some(wait) => {
            let mut res = render(
                ApiError::new(
                    ErrorCode::RateLimited,
                    "too many requests to this operation from your address",
                )
                .with_hint(format!("wait {wait} seconds and try again")),
            );
            if let Ok(v) = HeaderValue::from_str(&wait.to_string()) {
                res.headers_mut().insert(header::RETRY_AFTER, v);
            }
            res
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::StatusCode;
    use axum::routing::post;
    use tower::ServiceExt;

    fn app() -> Router {
        Router::new()
            .route("/v1/oauth/register", post(|| async { "ok" }))
            .route("/v1/nodes", post(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                RateLimiter::new(),
                rate_limit,
            ))
    }

    async fn send(app: &Router, path: &str, peer: &str, xff: Option<&str>) -> StatusCode {
        let mut req = Request::builder().method(Method::POST).uri(path);
        if let Some(x) = xff {
            req = req.header("x-forwarded-for", x);
        }
        let mut req = req.body(Body::empty()).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo::<SocketAddr>(peer.parse().unwrap()));
        app.clone().oneshot(req).await.unwrap().status()
    }

    /// Unauthenticated registration is limited per client IP; other IPs and other operations
    /// are not affected.
    #[tokio::test]
    async fn registration_is_limited_per_client_ip() {
        let app = app();
        let limit = LIMITS[0].1;
        for _ in 0..limit {
            assert_eq!(
                send(&app, "/v1/oauth/register", "203.0.113.7:5000", None).await,
                StatusCode::OK
            );
        }
        assert_eq!(
            send(&app, "/v1/oauth/register", "203.0.113.7:5001", None).await,
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            send(&app, "/v1/oauth/register", "203.0.113.8:5000", None).await,
            StatusCode::OK
        );
        for _ in 0..limit + 5 {
            assert_eq!(
                send(&app, "/v1/nodes", "203.0.113.7:5000", None).await,
                StatusCode::OK
            );
        }
        // Behind the proxy, the address the proxy added counts, not the proxy's.
        for _ in 0..limit {
            assert_eq!(
                send(
                    &app,
                    "/v1/oauth/register",
                    "127.0.0.1:443",
                    Some("198.51.100.1, 198.51.100.9")
                )
                .await,
                StatusCode::OK
            );
        }
        assert_eq!(
            send(
                &app,
                "/v1/oauth/register",
                "127.0.0.1:443",
                Some("198.51.100.9")
            )
            .await,
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            send(
                &app,
                "/v1/oauth/register",
                "127.0.0.1:443",
                Some("198.51.100.10")
            )
            .await,
            StatusCode::OK
        );
    }

    #[test]
    fn a_window_ends() {
        let l = RateLimiter::default();
        let ip: IpAddr = "203.0.113.1".parse().unwrap();
        let t0 = Instant::now();
        assert!(l.hit("/x", 1, ip, t0).is_none());
        assert!(l.hit("/x", 1, ip, t0).is_some());
        assert!(l.hit("/x", 1, ip, t0 + WINDOW).is_none());
    }

    #[test]
    fn a_direct_peer_cannot_choose_its_address() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "10.0.0.1".parse().unwrap());
        let peer: IpAddr = "203.0.113.5".parse().unwrap();
        assert_eq!(client_ip(peer, &h), peer);
    }
}
