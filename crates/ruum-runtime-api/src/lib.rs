//! The runtime API (card R-3; `contracts/apps/runtime.openapi.yaml`, `docs/STEP3.md`
//! "Processes" and "How an app runs"): what a running app may call at `http://ruum.internal`.
//!
//! Like the gateway (decision 0006-1), the binary holds **no tenant keys and no database
//! credential**. It holds the public key set of workload tokens, the public key set of the
//! gateway's user tokens, its own delegated-token signing key and the address of the core's
//! internal listener; every question about a tenant goes to the core ([`core_api`]).
//!
//! Modules (`docs/STEP3.md` "Shared files"):
//!
//! - [`api`]: the six operations of the contract (this card).
//! - [`token`]: workload, user and delegated tokens.
//! - [`core_api`]: the core's internal API and the principal cache.
//! - [`caller`]: the caller's network identity.
//! - [`limits`]: fair share and limits.
//! - [`internal`]: the internal gateway for app-to-app calls (card G-3).
//! - [`scheduler`]: the job scheduler (card R-5).
//! - [`events`]: the event delivery to subscribers (card R-6).
//!
//! The runtime API container is the only hop between tenant networks in production (decision
//! 0006 G): it listens only on its service port of each app network and never forwards a request
//! to an address that an app named.

pub mod api;
pub mod caller;
pub mod core_api;
pub mod events;
pub mod internal;
pub mod limits;
pub mod scheduler;
pub mod token;

use axum::Router;
use axum::routing::{get, post};
use caller::CallerNetwork;
use core_api::{CoreApi, PrincipalCache};
use hyper_util::rt::{TokioIo, TokioTimer};
use limits::{Caps, Limiter};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use token::{DelegatedTokens, KeySet};

/// The settings of the runtime API.
#[derive(Clone, Debug)]
pub struct Config {
    /// The instance's origin: `iss` of every token it checks and makes.
    pub issuer: String,
    /// The host name of the API (`ruum.internal`), lower case.
    pub internal_host: String,
    pub caps: Caps,
}

impl Config {
    pub fn new(issuer: &str, internal_host: &str) -> Self {
        Self {
            issuer: issuer.trim_end_matches('/').to_string(),
            internal_host: internal_host.trim_end_matches('.').to_ascii_lowercase(),
            caps: Caps::default(),
        }
    }
}

/// The runtime API: its keys, the core client and its state.
pub struct RuntimeApi {
    pub config: Config,
    pub workload_keys: KeySet,
    pub user_keys: KeySet,
    pub delegated: DelegatedTokens,
    pub core: Arc<dyn CoreApi>,
    /// The caller's network identity; `None`: no network checks (see [`caller`]).
    pub network: Option<Arc<dyn CallerNetwork>>,
    pub principals: PrincipalCache,
    pub replay: api::ReplaySet,
    pub limiter: Arc<Limiter>,
}

impl RuntimeApi {
    pub fn new(
        config: Config,
        workload_keys: KeySet,
        user_keys: KeySet,
        delegated: DelegatedTokens,
        core: Arc<dyn CoreApi>,
    ) -> Self {
        let limiter = Limiter::new(config.caps);
        Self {
            config,
            workload_keys,
            user_keys,
            delegated,
            core,
            network: None,
            principals: PrincipalCache::default(),
            replay: api::ReplaySet::default(),
            limiter,
        }
    }

    /// Checks every token against the network identity of its caller.
    pub fn with_network(mut self, network: Arc<dyn CallerNetwork>) -> Self {
        self.network = Some(network);
        self
    }
}

/// The router of the contract's operations.
pub fn router(rt: Arc<RuntimeApi>) -> Router {
    Router::new()
        .route("/v1/self", get(api::get_app_self))
        .route("/v1/self/users", get(api::list_app_self_users))
        .route("/v1/token/exchange", post(api::exchange))
        .route("/v1/nodes/{node_id}/content", get(api::read_bound_content))
        .route(
            "/v1/nodes/{node_id}/children",
            get(api::list_bound_children),
        )
        .route("/v1/events/{topic}", post(api::publish_app_event))
        .fallback(api::fallback)
        .layer(axum::middleware::from_fn_with_state(
            rt.clone(),
            api::host_only,
        ))
        .with_state(rt)
}

/// The open connections of each peer address.
#[derive(Default)]
struct Peers(Mutex<HashMap<IpAddr, usize>>);

struct PeerSlot {
    peers: Arc<Peers>,
    ip: IpAddr,
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

/// Serves the API on `listener` (HTTP/1): a header read timeout, at most
/// [`Caps::connections`] open connections and [`Caps::connections_per_peer`] per peer address
/// (a connection over a cap is closed at once), the TCP peer of each request for
/// [`caller`]. Runs until the listener fails.
pub async fn serve(listener: tokio::net::TcpListener, rt: Arc<RuntimeApi>) -> std::io::Result<()> {
    use tower::ServiceExt as _;
    let caps = rt.config.caps;
    let app = router(rt);
    let all = Arc::new(tokio::sync::Semaphore::new(caps.connections));
    let peers = Arc::new(Peers::default());
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(target: "ruum::runtime_api", error = %e, "accept failed");
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                continue;
            }
        };
        let Ok(permit) = all.clone().try_acquire_owned() else {
            tracing::warn!(target: "ruum::runtime_api", "a connection was closed at the connection cap");
            continue;
        };
        let slot = {
            let mut m = peers.0.lock().unwrap_or_else(|e| e.into_inner());
            let n = m.entry(peer.ip()).or_default();
            if *n >= caps.connections_per_peer {
                tracing::warn!(target: "ruum::runtime_api", "a connection was closed at the peer's connection cap");
                continue;
            }
            *n += 1;
            PeerSlot {
                peers: peers.clone(),
                ip: peer.ip(),
            }
        };
        let app = app.clone();
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
                .header_read_timeout(caps.header_timeout);
            let _ = b.serve_connection(TokioIo::new(stream), service).await;
        });
    }
}
