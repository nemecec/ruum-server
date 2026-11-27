//! The supervisor listener (backlog I6-20, S-89; `docs/STEP3.md` "The sandbox supervisor" 4):
//! the sandbox supervisor of each Kubernetes pod fetches its launch data here once
//! (`POST /v1/launch`) and the current workload token every 30 s (`POST /v1/token`), with its
//! projected service account token as the bearer. Its own port, only these two routes, no
//! request body, the peer address of the connection (never a header). The Kubernetes driver
//! makes every check (`ruum_runtime::KubeDriver::supervisor_launch`); this module only maps its
//! answer: `200` with the JSON, or `403`, `503`, `429` without a detail.
//!
//! With `RUUM_KUBE_SUPERVISOR_NETWORK=tls` (backlog I620-2) the listener speaks TLS 1.3 with the
//! key every replica derives from the record key ([`tls_acceptor`],
//! `ruum_runtime::kube::tls`); the supervisor pins its certificate. The TLS handshake counts to
//! the header time.
//!
//! The sandboxes that reach this port run untrusted code, so the connections have their own
//! caps ([`serve`]; Kimi re-check of I6-20 in run 16, defect 4): at most [`MAX_CONNECTIONS`] at
//! once and [`MAX_PEER_CONNECTIONS`] per peer address, the request headers within
//! [`HEADER_TIMEOUT`], and every connection closed after [`CONNECTION_LIFETIME`]. A sandbox
//! that opens connections and sends its headers slowly, or never, holds no more than its share
//! and only for a bounded time.

use axum::Router;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use hyper_util::rt::{TokioIo, TokioTimer};
use ruum_runtime::KubeDriver;
use ruum_runtime::kube::LaunchRefusal;
use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The longest a request may take (a `TokenReview` and a few reads of the API).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// The most open connections of the listener. A supervisor holds one at a time (its client
/// sends `Connection: close`), so this is many pods' worth.
pub const MAX_CONNECTIONS: usize = 1024;
/// The most open connections of one peer address (one pod).
pub const MAX_PEER_CONNECTIONS: usize = 4;
/// The time a connection has to send its request headers.
pub const HEADER_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest a connection stays open: the request timeout and room for the answer.
pub const CONNECTION_LIFETIME: Duration = Duration::from_secs(30);

/// The connection caps of [`serve`] (tests use smaller ones).
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    pub connections: usize,
    pub per_peer: usize,
    pub header_timeout: Duration,
    pub lifetime: Duration,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            connections: MAX_CONNECTIONS,
            per_peer: MAX_PEER_CONNECTIONS,
            header_timeout: HEADER_TIMEOUT,
            lifetime: CONNECTION_LIFETIME,
        }
    }
}

/// Open connections per peer address.
#[derive(Default)]
struct Peers(Mutex<HashMap<IpAddr, usize>>);

/// One open connection of a peer; dropping it frees the place.
struct PeerSlot {
    peers: Arc<Peers>,
    ip: IpAddr,
}

impl Peers {
    fn take(self: &Arc<Self>, ip: IpAddr, cap: usize) -> Option<PeerSlot> {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let n = m.entry(ip).or_default();
        if *n >= cap {
            if *n == 0 {
                m.remove(&ip);
            }
            return None;
        }
        *n += 1;
        Some(PeerSlot {
            peers: self.clone(),
            ip,
        })
    }
}

impl Drop for PeerSlot {
    fn drop(&mut self) {
        let mut m = self.peers.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = m.get_mut(&self.ip) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.remove(&self.ip);
            }
        }
    }
}

/// An IPv4-mapped IPv6 address as IPv4 (one peer, one count).
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

/// Serves `app` on `listener` (HTTP/1) with the connection caps until `stop` resolves: a
/// connection over a cap is closed at once, one without its headers after
/// [`Caps::header_timeout`], and every one after [`Caps::lifetime`]. The TCP peer of each
/// request goes to the handlers as `ConnectInfo`. After `stop` no connection is accepted, and
/// the open ones may finish (at most [`Caps::lifetime`]) before this returns: a launch answer
/// is sent after its pod is sealed as launched, so it must not be cut off (the shutdown drain,
/// `shutdown.rs`, bounds the wait).
pub async fn serve(
    listener: tokio::net::TcpListener,
    app: Router,
    caps: Caps,
    tls: Option<tokio_rustls::TlsAcceptor>,
    stop: impl Future<Output = ()>,
) -> std::io::Result<()> {
    use tower::ServiceExt as _;
    let all = Arc::new(tokio::sync::Semaphore::new(caps.connections));
    let peers: Arc<Peers> = Arc::default();
    tokio::pin!(stop);
    loop {
        let accepted = tokio::select! {
            () = &mut stop => None,
            r = listener.accept() => Some(r),
        };
        let (stream, peer) = match accepted {
            None => break,
            Some(Ok(x)) => x,
            Some(Err(e)) => {
                tracing::warn!(error = %e, "supervisor listener: accept failed");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let Ok(permit) = all.clone().try_acquire_owned() else {
            tracing::warn!("supervisor listener: a connection was closed at the connection cap");
            continue;
        };
        let Some(slot) = peers.take(canonical(peer.ip()), caps.per_peer) else {
            tracing::warn!(peer = %peer.ip(), "supervisor listener: a connection was closed at the peer's cap");
            continue;
        };
        let app = app.clone();
        let tls = tls.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _slot = slot;
            let service = hyper::service::service_fn(
                move |mut req: hyper::Request<hyper::body::Incoming>| {
                    req.extensions_mut()
                        .insert(axum::extract::ConnectInfo::<SocketAddr>(peer));
                    let app = app.clone();
                    async move { app.oneshot(req.map(axum::body::Body::new)).await }
                },
            );
            let mut b = hyper::server::conn::http1::Builder::new();
            b.timer(TokioTimer::new())
                .header_read_timeout(caps.header_timeout)
                .keep_alive(false);
            let started = tokio::time::Instant::now();
            match tls {
                None => {
                    let conn = b.serve_connection(TokioIo::new(stream), service);
                    let _ = tokio::time::timeout(caps.lifetime, conn).await;
                }
                Some(acceptor) => {
                    // The handshake within the header time; the lifetime counts from the accept.
                    let Ok(Ok(stream)) =
                        tokio::time::timeout(caps.header_timeout, acceptor.accept(stream)).await
                    else {
                        return;
                    };
                    let conn = b.serve_connection(TokioIo::new(stream), service);
                    let left = caps.lifetime.saturating_sub(started.elapsed());
                    let _ = tokio::time::timeout(left, conn).await;
                }
            }
        });
    }
    drop(listener);
    // Every open connection holds a permit: all of them back means all are closed.
    let n = u32::try_from(caps.connections).unwrap_or(u32::MAX);
    let _ = all.acquire_many(n).await;
    Ok(())
}

/// The TLS 1.3 acceptor of the listener's identity (backlog I620-2): the certificate and key
/// derived from the record key, the aws-lc-rs provider, no client certificate.
pub fn tls_acceptor(
    t: &ruum_runtime::kube::tls::SupervisorTls,
) -> anyhow::Result<tokio_rustls::TlsAcceptor> {
    use tokio_rustls::rustls;
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let cert = rustls::pki_types::CertificateDer::from(t.cert_der.clone());
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(t.key_pkcs8.clone().into());
    let cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| anyhow::anyhow!("the supervisor listener's TLS versions: {e}"))?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .map_err(|e| anyhow::anyhow!("the supervisor listener's TLS key: {e}"))?;
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(cfg)))
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn start(caps: Caps) -> SocketAddr {
        let app = Router::new().route(
            "/v1/token",
            post(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move { peer.ip().to_string() }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(serve(l, app, caps, None, std::future::pending()));
        addr
    }

    /// Reads until the server closes the connection (`true`) or `wait` passes (`false`).
    async fn closed_within(s: &mut tokio::net::TcpStream, wait: Duration) -> bool {
        let mut buf = [0u8; 256];
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            match tokio::time::timeout_at(deadline, s.read(&mut buf)).await {
                Err(_) => return false,
                Ok(Ok(0)) | Ok(Err(_)) => return true,
                Ok(Ok(_)) => {}
            }
        }
    }

    /// One request and its whole answer. `Err` when the server closed or reset the connection
    /// (a connection over a cap): the caller decides whether that is expected.
    async fn post_token(addr: SocketAddr) -> std::io::Result<String> {
        let mut s = tokio::net::TcpStream::connect(addr).await?;
        s.write_all(
            b"POST /v1/token HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await?;
        let mut out = String::new();
        s.read_to_string(&mut out).await?;
        Ok(out)
    }

    /// Kimi re-check of I6-20 in run 16, defect 4: a peer that opens connections and sends no
    /// headers holds at most its cap; one more is closed at once, and a request still gets its
    /// answer once a place is free.
    #[tokio::test]
    async fn a_peer_holds_at_most_its_connections() {
        let addr = start(Caps {
            connections: 64,
            per_peer: 2,
            header_timeout: Duration::from_secs(30),
            lifetime: Duration::from_secs(60),
        })
        .await;
        let mut held = Vec::new();
        for _ in 0..2 {
            held.push(tokio::net::TcpStream::connect(addr).await.unwrap());
        }
        let mut third = tokio::net::TcpStream::connect(addr).await.unwrap();
        assert!(
            closed_within(&mut third, Duration::from_secs(5)).await,
            "a third connection of one peer stays open"
        );
        drop(held.pop());
        // The freed place is taken again (the drop reaches the server shortly after).
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            // A try before the freed place is visible is closed at the cap: an empty answer, or
            // a reset when the request bytes were still unread (Kimi review of this fix).
            let answer = post_token(addr).await.unwrap_or_default();
            if answer.starts_with("HTTP/1.1 200") {
                assert!(answer.ends_with("127.0.0.1"), "{answer}");
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no place after a close"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// All peers together hold at most the listener's cap.
    #[tokio::test]
    async fn the_listener_holds_at_most_its_connections() {
        let addr = start(Caps {
            connections: 2,
            per_peer: 8,
            header_timeout: Duration::from_secs(30),
            lifetime: Duration::from_secs(60),
        })
        .await;
        let _a = tokio::net::TcpStream::connect(addr).await.unwrap();
        let _b = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
        assert!(
            closed_within(&mut c, Duration::from_secs(5)).await,
            "a connection over the listener's cap stays open"
        );
    }

    /// A connection that sends part of its headers and then nothing is closed after the header
    /// timeout.
    #[tokio::test]
    async fn a_connection_without_its_headers_is_closed() {
        let addr = start(Caps {
            connections: 64,
            per_peer: 8,
            header_timeout: Duration::from_millis(300),
            lifetime: Duration::from_secs(60),
        })
        .await;
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        s.write_all(b"POST /v1/token HTTP/1.1\r\nHost: x\r\n")
            .await
            .unwrap();
        assert!(
            closed_within(&mut s, Duration::from_secs(5)).await,
            "a connection without its headers stays open"
        );
    }

    /// A request in flight at the stop gets its answer, and `serve` returns after it (a launch
    /// answer is sent after the pod is sealed as launched). No connection is accepted after the
    /// stop.
    #[tokio::test]
    async fn a_request_in_flight_at_the_stop_gets_its_answer() {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
        let entered = Arc::new(Mutex::new(Some(entered_tx)));
        let app = Router::new().route(
            "/v1/token",
            post(move || {
                let entered = entered.clone();
                async move {
                    if let Some(tx) = entered.lock().unwrap().take() {
                        let _ = tx.send(());
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    "answer"
                }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve(l, app, Caps::default(), None, async move {
            let _ = stop_rx.await;
        }));
        let client = tokio::spawn(post_token(addr));
        entered_rx.await.unwrap();
        stop_tx.send(()).unwrap();
        // The handler takes 500 ms: `serve` waits for it (the process would exit after it).
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            !server.is_finished(),
            "serve returned with a request in flight"
        );
        let answer = tokio::time::timeout(Duration::from_secs(5), client)
            .await
            .expect("no answer")
            .unwrap()
            .expect("the answer");
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        assert!(answer.ends_with("answer"), "{answer}");
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("serve did not return after the stop")
            .unwrap()
            .unwrap();
        assert!(
            tokio::net::TcpStream::connect(addr).await.is_err(),
            "a connection is accepted after the stop"
        );
    }

    /// Every connection is closed at its lifetime, whatever the header timeout.
    #[tokio::test]
    async fn a_connection_is_closed_at_its_lifetime() {
        let addr = start(Caps {
            connections: 64,
            per_peer: 8,
            header_timeout: Duration::from_secs(60),
            lifetime: Duration::from_millis(500),
        })
        .await;
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        s.write_all(b"POST /v1/token HTTP/1.1\r\n").await.unwrap();
        assert!(
            closed_within(&mut s, Duration::from_secs(5)).await,
            "a connection stays open past its lifetime"
        );
    }

    /// Backlog I620-2: the listener speaks TLS with the certificate derived from the record
    /// key; the supervisor's client takes the answer with that pin, refuses another pin, and a
    /// plain HTTP request gets no answer.
    #[tokio::test]
    async fn the_supervisor_reaches_the_tls_listener_with_its_pin_only() {
        let key = ruum_runtime::kube::RecordKey::new([9u8; 32]);
        let t = ruum_runtime::kube::tls::SupervisorTls::derive(&key).unwrap();
        let app = Router::new().route(
            "/v1/token",
            post(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move { peer.ip().to_string() }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let caps = Caps {
            header_timeout: Duration::from_secs(2),
            ..Caps::default()
        };
        tokio::spawn(serve(
            l,
            app,
            caps,
            Some(tls_acceptor(&t).unwrap()),
            std::future::pending(),
        ));
        let pin = ruum_supervisor::http::parse_pin(&t.pin).unwrap();
        let ask = move |pin: Option<[u8; 32]>| {
            tokio::task::spawn_blocking(move || {
                ruum_supervisor::http::post(
                    addr,
                    pin,
                    "/v1/token",
                    "tok",
                    1024,
                    Duration::from_secs(5),
                )
            })
        };
        let body = ask(Some(pin)).await.unwrap().expect("the pinned answer");
        assert_eq!(body, b"127.0.0.1");
        let other = ruum_runtime::kube::tls::SupervisorTls::derive(
            &ruum_runtime::kube::RecordKey::new([10u8; 32]),
        )
        .unwrap();
        let other_pin = ruum_supervisor::http::parse_pin(&other.pin).unwrap();
        assert!(
            ask(Some(other_pin)).await.unwrap().is_err(),
            "another pin was accepted"
        );
        assert!(
            ask(None).await.unwrap().is_err(),
            "a plain HTTP request was answered"
        );
        // A connection that starts no handshake is closed after the header time.
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        assert!(
            closed_within(&mut s, Duration::from_secs(5)).await,
            "a connection without a handshake stays open"
        );
    }
}
