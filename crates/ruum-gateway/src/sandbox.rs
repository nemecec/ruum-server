//! The border to the core for sandboxes, and the hold of cold starts (card G-1b; decision 0006 G
//! points 1 and 3; decision 25: a cold start within 3 s).
//!
//! [`Sandboxes`] asks the core where the instance of a deployment runs ([`HttpSandboxes`]: the
//! core's internal route `POST /internal/gateway/v1/upstream`, with the same base URL and client
//! rules as [`crate::core_api::HttpCore`]). **The upstream comes only from the core**: the query
//! holds the tenant, app, deployment and environment that the gateway resolved from the host
//! (never a header, the target or a value of the request), and the core checks them against its
//! own record. The gateway accepts only an answer of the form `http://<host>:<port>` (no path,
//! no user info, no query, a port in 1..=65535, a host of letters, digits, `.`, `-`, `_`, or an
//! IPv6 address in brackets); any other answer fails closed (the `502` page).
//!
//! [`SandboxHold`] holds a request while the core wakes or starts the instance:
//!
//! - **Single flight.** One question per deployment and environment is on its way to the core at
//!   a time; every request in the meantime waits for the same answer. The core coalesces the
//!   wake itself too, so many held requests cause one wake.
//! - **The hold limit** ([`HoldConfig::hold`], `RUUM_GATEWAY_WAKE_HOLD`, default 10 s): while the
//!   core answers `starting`, the request asks again every [`HoldConfig::retry`] until the limit;
//!   then the visitor gets the `503` page with `Retry-After`.
//! - **Fair share** (backlog S-61). A request that is not answered from the short cache of ready
//!   answers takes a hold slot: at most [`HoldConfig::per_app`] (`RUUM_GATEWAY_HOLD_PER_APP`,
//!   default 64) per deployment, [`HoldConfig::per_tenant`] (`RUUM_GATEWAY_HOLD_PER_TENANT`,
//!   default 256) per tenant and [`HoldConfig::global`] (`RUUM_GATEWAY_HOLD_GLOBAL`, default
//!   1024) in all. Over any of them, the visitor gets the `503` page at once, so the cold starts
//!   of one app, or of many deployments of one tenant, cannot take the slots of other tenants
//!   (both shares are far below the global cap). Open WebSockets have their own shares: per app
//!   ([`HoldConfig::ws_per_app`], all deployments of the app together), per tenant
//!   ([`HoldConfig::ws_per_tenant`]) and in all ([`HoldConfig::ws_global`]) (review-g1b 3).
//! - **A short cache of ready answers** ([`READY_TTL`]), so a busy app does not cost one core
//!   question per request. A request that cannot connect to a cached address forgets it
//!   ([`SandboxHold::forget`]): the instance may have scaled to zero.

use crate::core_api::{CoreError, CoreResult};
use async_trait::async_trait;
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use ruum_core::identity::app_sign_in::wire::{PREFIX, UpstreamAnswer, UpstreamQuery};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// How long a ready answer of the core is reused.
pub const READY_TTL: Duration = Duration::from_secs(2);

/// The question the gateway asks the core about sandboxes.
#[async_trait]
pub trait Sandboxes: Send + Sync {
    async fn upstream(&self, q: &UpstreamQuery) -> CoreResult<UpstreamAnswer>;
}

/// No sandbox directory: every deployment is `not_running` (the gateway built without one).
pub struct NoSandboxes;

#[async_trait]
impl Sandboxes for NoSandboxes {
    async fn upstream(&self, _: &UpstreamQuery) -> CoreResult<UpstreamAnswer> {
        Ok(UpstreamAnswer::NotRunning)
    }
}

/// The core's internal listener over HTTP (the client rules of `HttpCore`: a 5 s timeout, no
/// redirects).
pub struct HttpSandboxes {
    base: String,
    client: reqwest::Client,
}

impl HttpSandboxes {
    /// A client of the internal listener at `base` (`http://ruum-server:8081`).
    pub fn new(base: &str) -> CoreResult<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|e| CoreError(e.to_string()))?;
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            client,
        })
    }
}

#[async_trait]
impl Sandboxes for HttpSandboxes {
    async fn upstream(&self, q: &UpstreamQuery) -> CoreResult<UpstreamAnswer> {
        let r = self
            .client
            .post(format!("{}{PREFIX}/upstream", self.base))
            .json(q)
            .send()
            .await
            .map_err(|e| CoreError(format!("/upstream: {}", e.without_url())))?;
        if !r.status().is_success() {
            return Err(CoreError(format!("/upstream: status {}", r.status())));
        }
        r.json::<UpstreamAnswer>()
            .await
            .map_err(|e| CoreError(format!("/upstream: {}", e.without_url())))
    }
}

/// The address of an instance as the gateway accepts it: exactly `http://<host>:<port>` (see
/// the module docs). Returns the same text, or `None`.
pub fn upstream_url(raw: &str) -> Option<String> {
    let rest = raw.strip_prefix("http://")?;
    if rest.is_empty()
        || rest
            .bytes()
            .any(|b| matches!(b, b'/' | b'?' | b'#' | b'@' | b'\\' | b'%') || !b.is_ascii_graphic())
    {
        return None;
    }
    let (host, port) = rest.rsplit_once(':')?;
    if port.is_empty() || port.len() > 5 || !port.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let port: u16 = port.parse().ok()?;
    if port == 0 {
        return None;
    }
    let host_ok = if let Some(inner) = host.strip_prefix('[') {
        let inner = inner.strip_suffix(']')?;
        !inner.is_empty() && inner.parse::<std::net::Ipv6Addr>().is_ok()
    } else {
        !host.is_empty()
            && host.len() <= 253
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    };
    host_ok.then(|| format!("http://{host}:{port}"))
}

/// The limits of the hold (see the module docs).
#[derive(Clone, Debug)]
pub struct HoldConfig {
    /// The longest hold of one request (`RUUM_GATEWAY_WAKE_HOLD`).
    pub hold: Duration,
    /// Held requests per deployment (`RUUM_GATEWAY_HOLD_PER_APP`).
    pub per_app: usize,
    /// Held requests per tenant (`RUUM_GATEWAY_HOLD_PER_TENANT`).
    pub per_tenant: usize,
    /// Held requests in all (`RUUM_GATEWAY_HOLD_GLOBAL`).
    pub global: usize,
    /// Open WebSockets per app, all its deployments together (`RUUM_GATEWAY_WS_PER_APP`).
    pub ws_per_app: usize,
    /// Open WebSockets per tenant (`RUUM_GATEWAY_WS_PER_TENANT`).
    pub ws_per_tenant: usize,
    /// Open WebSockets in all (`RUUM_GATEWAY_WS_GLOBAL`).
    pub ws_global: usize,
    /// How long a held request waits before it asks again after `starting`.
    pub retry: Duration,
}

impl Default for HoldConfig {
    fn default() -> Self {
        Self {
            hold: Duration::from_secs(10),
            per_app: 64,
            per_tenant: 256,
            global: 1024,
            ws_per_app: 256,
            ws_per_tenant: 2048,
            ws_global: 8192,
            retry: Duration::from_millis(100),
        }
    }
}

/// Where a request of a sandbox deployment goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// The instance's address (`http://<host>:<port>`).
    Ready(String),
    /// Over the hold share of the app or the global cap: `503` at once.
    Busy,
    /// Still starting at the end of the hold: `503`.
    Starting,
    /// No instance runs, or the core's address was refused: `502`.
    NotRunning,
    /// The instance ended by itself: `502`.
    Crashed,
    /// The core could not answer: `503` (fail closed).
    Unavailable,
}

type Key = (
    ruum_core::TenantId,
    Uuid,
    ruum_core::identity::app_sign_in::AppEnv,
);

fn key_of(q: &UpstreamQuery) -> Key {
    (q.tenant_id, q.deployment_id, q.environment)
}

type Asked = Shared<BoxFuture<'static, Result<UpstreamAnswer, String>>>;

/// The limits of one kind of slot: per share key (a deployment or an app), per tenant and in
/// all.
#[derive(Clone, Copy, Debug)]
struct Limits {
    per_key: usize,
    per_tenant: usize,
    global: usize,
}

#[derive(Default)]
struct Counts {
    per_key: HashMap<Uuid, usize>,
    per_tenant: HashMap<ruum_core::TenantId, usize>,
    total: usize,
}

/// Counts slots per share key, per tenant and in all.
#[derive(Default)]
struct Counter {
    state: Mutex<Counts>,
}

fn take_one<K: std::hash::Hash + Eq>(m: &mut HashMap<K, usize>, k: &K) {
    if let Some(n) = m.get_mut(k) {
        *n -= 1;
        if *n == 0 {
            m.remove(k);
        }
    }
}

impl Counter {
    fn acquire(
        self: &Arc<Self>,
        tenant: ruum_core::TenantId,
        key: Uuid,
        lim: Limits,
    ) -> Option<Slot> {
        let mut st = self.state.lock().expect("slot lock");
        let mine = st.per_key.get(&key).copied().unwrap_or(0);
        let tenants = st.per_tenant.get(&tenant).copied().unwrap_or(0);
        if mine >= lim.per_key || tenants >= lim.per_tenant || st.total >= lim.global {
            return None;
        }
        st.per_key.insert(key, mine + 1);
        st.per_tenant.insert(tenant, tenants + 1);
        st.total += 1;
        Some(Slot {
            counter: self.clone(),
            tenant,
            key,
        })
    }

    fn release(&self, tenant: ruum_core::TenantId, key: Uuid) {
        let mut st = self.state.lock().expect("slot lock");
        take_one(&mut st.per_key, &key);
        take_one(&mut st.per_tenant, &tenant);
        st.total = st.total.saturating_sub(1);
    }

    fn total(&self) -> usize {
        self.state.lock().expect("slot lock").total
    }
}

/// A taken slot (a held request or an open WebSocket); dropped, it is free again.
pub struct Slot {
    counter: Arc<Counter>,
    tenant: ruum_core::TenantId,
    key: Uuid,
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.counter.release(self.tenant, self.key);
    }
}

/// The hold of sandbox requests (see the module docs).
pub struct SandboxHold {
    inner: Arc<dyn Sandboxes>,
    pub config: HoldConfig,
    ready: Mutex<HashMap<Key, (String, Instant)>>,
    asked: Arc<Mutex<HashMap<Key, Asked>>>,
    held: Arc<Counter>,
    sockets: Arc<Counter>,
}

impl SandboxHold {
    pub fn new(inner: Arc<dyn Sandboxes>, config: HoldConfig) -> Self {
        Self {
            inner,
            config,
            ready: Mutex::new(HashMap::new()),
            asked: Arc::default(),
            held: Arc::default(),
            sockets: Arc::default(),
        }
    }

    /// Held requests now (for tests).
    pub fn held(&self) -> usize {
        self.held.total()
    }

    /// Open WebSockets now (for tests).
    pub fn open_sockets(&self) -> usize {
        self.sockets.total()
    }

    /// Takes a WebSocket slot of the app of `tenant`, or `None` over the app's share, the
    /// tenant's share or the global cap.
    pub fn open_socket(&self, tenant: ruum_core::TenantId, app: ruum_core::NodeId) -> Option<Slot> {
        self.sockets.acquire(
            tenant,
            app.0,
            Limits {
                per_key: self.config.ws_per_app,
                per_tenant: self.config.ws_per_tenant,
                global: self.config.ws_global,
            },
        )
    }

    /// Forgets the cached address of a query (the instance did not take a connection).
    pub fn forget(&self, q: &UpstreamQuery) {
        self.ready.lock().expect("ready lock").remove(&key_of(q));
    }

    fn cached(&self, k: &Key) -> Option<String> {
        let mut m = self.ready.lock().expect("ready lock");
        match m.get(k) {
            Some((url, at)) if at.elapsed() < READY_TTL => Some(url.clone()),
            Some(_) => {
                m.remove(k);
                None
            }
            None => None,
        }
    }

    /// The core's answer for `q`: the question on its way, or a new one.
    fn ask(&self, q: &UpstreamQuery) -> Asked {
        let k = key_of(q);
        let mut m = self.asked.lock().expect("asked lock");
        if let Some(a) = m.get(&k) {
            return a.clone();
        }
        let inner = self.inner.clone();
        let asked = self.asked.clone();
        let query = q.clone();
        let task = tokio::spawn(async move {
            let r = inner.upstream(&query).await.map_err(|e| e.to_string());
            asked.lock().expect("asked lock").remove(&key_of(&query));
            r
        });
        let shared = async move {
            task.await
                .unwrap_or_else(|_| Err("the core question ended early".into()))
        }
        .boxed()
        .shared();
        m.insert(k, shared.clone());
        shared
    }

    /// Where a request of `q` goes (see the module docs).
    pub async fn route(&self, q: &UpstreamQuery) -> Route {
        let k = key_of(q);
        if let Some(url) = self.cached(&k) {
            return Route::Ready(url);
        }
        let limits = Limits {
            per_key: self.config.per_app,
            per_tenant: self.config.per_tenant,
            global: self.config.global,
        };
        let Some(_slot) = self.held.acquire(q.tenant_id, q.deployment_id, limits) else {
            tracing::warn!(target: "ruum::gateway", "the hold share of an app or a tenant is full");
            return Route::Busy;
        };
        let deadline = tokio::time::Instant::now() + self.config.hold;
        loop {
            let answer = match tokio::time::timeout_at(deadline, self.ask(q)).await {
                Err(_) => return Route::Starting,
                Ok(Err(e)) => {
                    tracing::warn!(target: "ruum::gateway", error = %e, "the upstream question failed");
                    return Route::Unavailable;
                }
                Ok(Ok(a)) => a,
            };
            match answer {
                UpstreamAnswer::Ready { url } => {
                    return match upstream_url(&url) {
                        Some(u) => {
                            self.ready
                                .lock()
                                .expect("ready lock")
                                .insert(k, (u.clone(), Instant::now()));
                            Route::Ready(u)
                        }
                        None => {
                            tracing::warn!(
                                target: "ruum::security",
                                event = "gateway.upstream_refused",
                                "the core gave an upstream that is not http://host:port"
                            );
                            Route::NotRunning
                        }
                    };
                }
                UpstreamAnswer::NotRunning => return Route::NotRunning,
                UpstreamAnswer::Crashed => return Route::Crashed,
                UpstreamAnswer::Starting => {
                    if tokio::time::Instant::now() + self.config.retry >= deadline {
                        return Route::Starting;
                    }
                    tokio::time::sleep(self.config.retry).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruum_core::identity::app_sign_in::AppEnv;
    use ruum_core::{NodeId, TenantId};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn only_http_host_port_is_an_upstream() {
        for ok in [
            "http://10.89.0.7:3000",
            "http://sandbox-1.internal:8080",
            "http://[fd00::1]:80",
            "http://a_b:1",
        ] {
            assert_eq!(upstream_url(ok).as_deref(), Some(ok), "{ok}");
        }
        for bad in [
            "https://10.89.0.7:3000",
            "http://10.89.0.7",
            "http://10.89.0.7:",
            "http://10.89.0.7:0",
            "http://10.89.0.7:65536",
            "http://10.89.0.7:3000/",
            "http://10.89.0.7:3000/x",
            "http://10.89.0.7:3000?x",
            "http://10.89.0.7:3000#x",
            "http://user@10.89.0.7:3000",
            "http://user:pw@10.89.0.7:3000",
            "http://:3000",
            "http://[zz]:3000",
            "http://a b:3000",
            "http://a%2fb:3000",
            "http://a\\b:3000",
            "file:///etc/passwd",
            "",
            "http://",
        ] {
            assert_eq!(upstream_url(bad), None, "{bad}");
        }
    }

    /// A core that answers `starting` until `ready_after` questions, counting them.
    struct Slow {
        asks: AtomicUsize,
        ready_after: usize,
        delay: Duration,
    }

    #[async_trait]
    impl Sandboxes for Slow {
        async fn upstream(&self, _: &UpstreamQuery) -> CoreResult<UpstreamAnswer> {
            tokio::time::sleep(self.delay).await;
            let n = self.asks.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(if n >= self.ready_after {
                UpstreamAnswer::Ready {
                    url: "http://127.0.0.1:9".into(),
                }
            } else {
                UpstreamAnswer::Starting
            })
        }
    }

    fn q() -> UpstreamQuery {
        UpstreamQuery {
            tenant_id: TenantId::new(),
            app_id: NodeId::new(),
            deployment_id: Uuid::now_v7(),
            environment: AppEnv::Live,
        }
    }

    #[tokio::test]
    async fn many_requests_share_one_question_and_the_answer_is_cached() {
        let core = Arc::new(Slow {
            asks: AtomicUsize::new(0),
            ready_after: 1,
            delay: Duration::from_millis(200),
        });
        let hold = Arc::new(SandboxHold::new(core.clone(), HoldConfig::default()));
        let query = q();
        let routes = futures::future::join_all((0..20).map(|_| {
            let h = hold.clone();
            let q = query.clone();
            async move { h.route(&q).await }
        }))
        .await;
        assert!(
            routes
                .iter()
                .all(|r| *r == Route::Ready("http://127.0.0.1:9".into()))
        );
        assert_eq!(core.asks.load(Ordering::SeqCst), 1);
        assert_eq!(
            hold.route(&query).await,
            Route::Ready("http://127.0.0.1:9".into())
        );
        assert_eq!(
            core.asks.load(Ordering::SeqCst),
            1,
            "the ready answer is cached"
        );
        hold.forget(&query);
        hold.route(&query).await;
        assert_eq!(core.asks.load(Ordering::SeqCst), 2);
        assert_eq!(hold.held(), 0);
    }

    #[tokio::test]
    async fn a_hold_ends_at_its_limit_and_shares_are_kept() {
        let core = Arc::new(Slow {
            asks: AtomicUsize::new(0),
            ready_after: usize::MAX,
            delay: Duration::from_millis(10),
        });
        let config = HoldConfig {
            hold: Duration::from_millis(400),
            per_app: 2,
            global: 3,
            ..HoldConfig::default()
        };
        let hold = Arc::new(SandboxHold::new(core, config));
        let a = q();
        let b = q();
        let c = q();
        let started = Instant::now();
        let (ra1, ra2, ra3, rb, rc) = tokio::join!(
            hold.route(&a),
            hold.route(&a),
            async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let t = Instant::now();
                (hold.route(&a).await, t.elapsed())
            },
            async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                hold.route(&b).await
            },
            async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let t = Instant::now();
                (hold.route(&c).await, t.elapsed())
            },
        );
        assert_eq!((ra1, ra2), (Route::Starting, Route::Starting));
        // The third request of app A is over A's share: at once, while B is still held.
        assert_eq!(ra3.0, Route::Busy);
        assert!(ra3.1 < Duration::from_millis(100), "{:?}", ra3.1);
        assert_eq!(rb, Route::Starting);
        // App C is over the global cap (A 2 + B 1).
        assert_eq!(rc.0, Route::Busy);
        assert!(rc.1 < Duration::from_millis(100));
        // A hold ends at its limit, or one retry interval before it when no retry fits.
        assert!(
            started.elapsed() >= Duration::from_millis(300),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(hold.held(), 0);
        // WebSocket shares: per app (all its deployments), per tenant, in all.
        let hold = SandboxHold::new(
            Arc::new(NoSandboxes),
            HoldConfig {
                ws_per_app: 1,
                ws_per_tenant: 2,
                ws_global: 3,
                ..HoldConfig::default()
            },
        );
        let (t1, t2, t3) = (TenantId::new(), TenantId::new(), TenantId::new());
        let app1 = NodeId::new();
        let s1 = hold.open_socket(t1, app1).unwrap();
        assert!(hold.open_socket(t1, app1).is_none(), "over the app's share");
        let _s2 = hold.open_socket(t1, NodeId::new()).unwrap();
        assert!(
            hold.open_socket(t1, NodeId::new()).is_none(),
            "over the tenant's share"
        );
        let _s3 = hold.open_socket(t2, NodeId::new()).unwrap();
        assert!(
            hold.open_socket(t3, NodeId::new()).is_none(),
            "over the global cap"
        );
        drop(s1);
        assert!(hold.open_socket(t1, app1).is_some());
    }

    #[tokio::test]
    async fn one_tenants_deployments_cannot_take_the_global_hold() {
        let core = Arc::new(Slow {
            asks: AtomicUsize::new(0),
            ready_after: usize::MAX,
            delay: Duration::from_millis(10),
        });
        let config = HoldConfig {
            hold: Duration::from_millis(400),
            per_app: 2,
            per_tenant: 3,
            global: 10,
            ..HoldConfig::default()
        };
        let hold = Arc::new(SandboxHold::new(core, config));
        let tenant = TenantId::new();
        let of_tenant = || UpstreamQuery {
            tenant_id: tenant,
            ..q()
        };
        // Deployment A holds 2 (its share), B 1: the tenant's share of 3 is full.
        let (a, b, c) = (of_tenant(), of_tenant(), of_tenant());
        let other = q();
        let (ra1, ra2, rb, rc, rother) = tokio::join!(
            hold.route(&a),
            hold.route(&a),
            hold.route(&b),
            async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let t = Instant::now();
                (hold.route(&c).await, t.elapsed())
            },
            async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                hold.route(&other).await
            },
        );
        assert_eq!(
            (ra1, ra2, rb),
            (Route::Starting, Route::Starting, Route::Starting)
        );
        // A third deployment of the same tenant is within its own share, but over the tenant's.
        assert_eq!(rc.0, Route::Busy);
        assert!(rc.1 < Duration::from_millis(100), "{:?}", rc.1);
        // Another tenant is still held (not busy), far below the global cap.
        assert_eq!(rother, Route::Starting);
        assert_eq!(hold.held(), 0);
    }
}
