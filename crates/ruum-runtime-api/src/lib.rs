//! The runtime API (card R-3; `contracts/apps/runtime.openapi.yaml`, `docs/STEP3.md`
//! "Processes" and "How an app runs"): what a running app may call at `http://ruum.internal`.
//!
//! Like the gateway (decision 0006-1), the binary holds **no tenant keys, no signing key and no
//! database credential**. It holds the public key set of workload tokens (which also checks the
//! delegated tokens that the core signs), the public key set of the gateway's user tokens and
//! the address of the core's internal listener; every question about a tenant goes to the core
//! ([`core_api`]), with the app's own token, which the core checks again.
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
use token::KeySet;

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
        core: Arc<dyn CoreApi>,
    ) -> Self {
        let limiter = Limiter::new(config.caps);
        Self {
            config,
            workload_keys,
            user_keys,
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

/// Open connections by key (a peer address, a tenant).
struct Slots<K>(Mutex<HashMap<K, usize>>);

impl<K: std::hash::Hash + Eq + Copy> Slots<K> {
    fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(HashMap::new())))
    }

    /// One more connection of `key`, unless it has `cap` already.
    fn take(self: &Arc<Self>, key: K, cap: usize) -> Option<Slot<K>> {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let n = m.entry(key).or_default();
        if *n >= cap {
            if *n == 0 {
                m.remove(&key);
            }
            return None;
        }
        *n += 1;
        Some(Slot {
            slots: self.clone(),
            key,
        })
    }
}

/// An open connection of a key; dropping it frees the place.
struct Slot<K: std::hash::Hash + Eq + Copy> {
    slots: Arc<Slots<K>>,
    key: K,
}

impl<K: std::hash::Hash + Eq + Copy> Drop for Slot<K> {
    fn drop(&mut self) {
        let mut m = self.slots.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = m.get_mut(&self.key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.remove(&self.key);
            }
        }
    }
}

/// Serves the API on `listener` (HTTP/1): a header read timeout, at most
/// [`Caps::connections`] open connections and [`Caps::connections_per_peer`] per peer address,
/// and with a caller network lookup [`Caps::connections_per_tenant`] per tenant (the peers the
/// lookup does not know share one such share); a connection over a cap is closed at once (who is
/// refused first: `crate::limits`). The TCP peer of each request goes to [`caller`]. Runs until
/// the listener fails.
pub async fn serve(listener: tokio::net::TcpListener, rt: Arc<RuntimeApi>) -> std::io::Result<()> {
    use tower::ServiceExt as _;
    let caps = rt.config.caps;
    let network = rt.network.clone();
    let app = router(rt);
    let all = Arc::new(tokio::sync::Semaphore::new(caps.connections));
    let peers: Arc<Slots<IpAddr>> = Slots::new();
    let tenants: Arc<Slots<Option<ruum_core::TenantId>>> = Slots::new();
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
        let Some(slot) = peers.take(peer.ip(), caps.connections_per_peer) else {
            tracing::warn!(target: "ruum::runtime_api", "a connection was closed at the peer's connection cap");
            continue;
        };
        let app = app.clone();
        let network = network.clone();
        let tenants = tenants.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _slot = slot;
            // With a lookup, the tenant's share of the connections (the lookup runs here, not
            // in the accept loop, so a slow lookup never stops other peers' accepts).
            // Peers the lookup does not know share one such share (their requests get `401`).
            let _tenant_slot = match &network {
                None => None,
                Some(net) => {
                    let tenant = net.identify(peer.ip()).await.map(|id| id.tenant);
                    match tenants.take(tenant, caps.connections_per_tenant) {
                        Some(s) => Some(s),
                        None => {
                            tracing::warn!(
                                target: "ruum::runtime_api",
                                tenant = ?tenant,
                                "a connection was closed at the tenant's connection cap"
                            );
                            return;
                        }
                    }
                }
            };
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
