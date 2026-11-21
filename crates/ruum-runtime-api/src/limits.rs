//! Fair share and limits of the runtime API (card R-3; backlog S-61, M-59).
//!
//! - **Concurrent requests**: at most [`Caps::per_app`] per app (its live and preview
//!   principals together), [`Caps::per_tenant`] per tenant and [`Caps::global`] in all. A
//!   request over a cap gets `429 rate_limited` with `Retry-After: 1` (not in the contract yet:
//!   an additive line for the panel). Who is refused first: an app at its own cap is refused
//!   before its tenant's other apps feel anything; a tenant at its cap is refused before other
//!   tenants feel anything; at the global cap every new request is refused until one ends, and
//!   no tenant holds more than `per_tenant / global` (1/8 by default) of it.
//!   The permit is taken right after the token check, before any call to the core, so the
//!   requests of one app reach the core at most `per_app` at a time, also when its principal
//!   cache entry is empty.
//! - **Streamed bytes** of `readBoundContent`: token buckets of [`Caps::bytes_per_app`],
//!   [`Caps::bytes_per_tenant`] and [`Caps::bytes_global`] per second. A stream over a bucket
//!   is slowed down, never cut: each chunk first reserves its bytes in its app's and tenant's
//!   buckets and waits for the slower of them ([`Limiter::reserve_own`]), and only then in the
//!   global bucket ([`Limiter::reserve_global`]), so a chunk that waits for its own tenant holds
//!   no global capacity. A reservation that is dropped before its chunk goes (the client went
//!   away) gives its bytes back, so cancelled streams leave no debt that other tenants wait for.
//!   One tenant's streams spend only its own and the global bucket; the per-tenant rate (1/8 of
//!   the global one by default) keeps the global bucket for at least 8 tenants at full speed.
//! - **Request bodies** (exchange, event) must arrive in full within [`Caps::body_timeout`],
//!   else `408`: a peer that trickles bytes holds its permit and connection no longer.
//! - **Connections** (see `crate::serve`; backlog S-71): a header read timeout (hyper also
//!   runs it while a keep-alive connection waits for its next request, so an idle connection
//!   closes after it), at most [`Caps::connections`] open connections in all and
//!   [`Caps::connections_per_peer`] per peer address. With a caller network lookup
//!   (`crate::caller`) also at most [`Caps::connections_per_tenant`] per tenant; the peers the
//!   lookup does not know share one such share. Who is refused first: a peer at its own cap, then
//!   a tenant at its cap (1/8 of the total by default), before any other tenant feels anything;
//!   at the total cap every new connection is closed, across tenants, until one ends, and no
//!   tenant holds more than its share of it. Without a lookup (tests and the laptop until card
//!   I-4) the listener cannot tell tenants apart: the per-peer cap is the only share, so 16
//!   peers of one tenant can fill the total (backlog R3-11: I-4 and I-6 wire the lookup).

use ruum_core::{NodeId, TenantId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The caps (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    pub per_app: usize,
    pub per_tenant: usize,
    pub global: usize,
    pub bytes_per_app: u64,
    pub bytes_per_tenant: u64,
    pub bytes_global: u64,
    /// Open connections of the listener in all.
    pub connections: usize,
    /// Open connections of one peer address.
    pub connections_per_peer: usize,
    /// Open connections of one tenant (with a caller network lookup).
    pub connections_per_tenant: usize,
    /// How long a client may take to send the request head.
    pub header_timeout: Duration,
    /// How long a client may take to send a whole request body.
    pub body_timeout: Duration,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            per_app: 32,
            per_tenant: 128,
            global: 1024,
            bytes_per_app: 8 << 20,
            bytes_per_tenant: 32 << 20,
            bytes_global: 256 << 20,
            connections: 4096,
            connections_per_peer: 256,
            connections_per_tenant: 512,
            header_timeout: Duration::from_secs(10),
            body_timeout: Duration::from_secs(10),
        }
    }
}

/// Which cap refused a request (for the log line; IDs only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    App,
    Tenant,
    Global,
}

impl Refused {
    pub fn as_str(self) -> &'static str {
        match self {
            Refused::App => "app",
            Refused::Tenant => "tenant",
            Refused::Global => "global",
        }
    }
}

#[derive(Default)]
struct Counts {
    global: usize,
    tenant: HashMap<TenantId, usize>,
    app: HashMap<(TenantId, NodeId), usize>,
}

struct Bucket {
    /// Bytes that may go now; below zero is a debt that later chunks wait for.
    tokens: f64,
    last: Instant,
}

impl Bucket {
    fn new(now: Instant, rate: u64) -> Self {
        Self {
            tokens: rate as f64,
            last: now,
        }
    }

    /// Gives back `n` bytes of a reservation that was not used.
    fn give_back(&mut self, n: u64, rate: u64) {
        self.tokens = (self.tokens + n as f64).min(rate.max(1) as f64);
    }

    /// Takes `n` bytes; returns how long the caller waits before it sends them.
    fn take(&mut self, n: u64, rate: u64, now: Instant) -> Duration {
        let rate = rate.max(1) as f64;
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * rate).min(rate);
        self.last = now;
        self.tokens -= n as f64;
        if self.tokens >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-self.tokens / rate)
        }
    }
}

#[derive(Default)]
struct Buckets {
    global: Option<Bucket>,
    tenant: HashMap<TenantId, Bucket>,
    app: HashMap<(TenantId, NodeId), Bucket>,
}

/// Buckets that went unused this long are dropped when the maps grow.
const BUCKET_IDLE: Duration = Duration::from_secs(60);
/// The maps of buckets are swept when they hold more than this.
const BUCKETS_SWEEP_AT: usize = 10_000;

/// The counters and buckets of the caps.
pub struct Limiter {
    pub caps: Caps,
    counts: Mutex<Counts>,
    buckets: Mutex<Buckets>,
}

/// A request in flight; dropping it frees its place.
pub struct Permit {
    limiter: Arc<Limiter>,
    tenant: TenantId,
    app: NodeId,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut c = self
            .limiter
            .counts
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        c.global = c.global.saturating_sub(1);
        if let Some(n) = c.tenant.get_mut(&self.tenant) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                c.tenant.remove(&self.tenant);
            }
        }
        let key = (self.tenant, self.app);
        if let Some(n) = c.app.get_mut(&key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                c.app.remove(&key);
            }
        }
    }
}

impl Limiter {
    pub fn new(caps: Caps) -> Arc<Self> {
        Arc::new(Self {
            caps,
            counts: Mutex::default(),
            buckets: Mutex::default(),
        })
    }

    /// A place for one request of `app` in `tenant`, or the cap that refused it.
    pub fn acquire(self: &Arc<Self>, tenant: TenantId, app: NodeId) -> Result<Permit, Refused> {
        let mut c = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        if c.app.get(&(tenant, app)).copied().unwrap_or(0) >= self.caps.per_app {
            return Err(Refused::App);
        }
        if c.tenant.get(&tenant).copied().unwrap_or(0) >= self.caps.per_tenant {
            return Err(Refused::Tenant);
        }
        if c.global >= self.caps.global {
            return Err(Refused::Global);
        }
        c.global += 1;
        *c.tenant.entry(tenant).or_default() += 1;
        *c.app.entry((tenant, app)).or_default() += 1;
        Ok(Permit {
            limiter: self.clone(),
            tenant,
            app,
        })
    }

    fn lock_buckets(&self) -> std::sync::MutexGuard<'_, Buckets> {
        self.buckets.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Reserves `n` bytes of `app` in its app's and its tenant's buckets; the reservation
    /// waits for the slower of the two.
    pub fn reserve_own(
        self: &Arc<Self>,
        tenant: TenantId,
        app: NodeId,
        n: u64,
        now: Instant,
    ) -> Reservation {
        let caps = self.caps;
        let mut b = self.lock_buckets();
        if b.app.len() > BUCKETS_SWEEP_AT || b.tenant.len() > BUCKETS_SWEEP_AT {
            b.app
                .retain(|_, x| now.saturating_duration_since(x.last) < BUCKET_IDLE);
            b.tenant
                .retain(|_, x| now.saturating_duration_since(x.last) < BUCKET_IDLE);
        }
        let a = b
            .app
            .entry((tenant, app))
            .or_insert_with(|| Bucket::new(now, caps.bytes_per_app))
            .take(n, caps.bytes_per_app, now);
        let t = b
            .tenant
            .entry(tenant)
            .or_insert_with(|| Bucket::new(now, caps.bytes_per_tenant))
            .take(n, caps.bytes_per_tenant, now);
        Reservation {
            limiter: self.clone(),
            of: Reserved::Own(tenant, app),
            n,
            delay: a.max(t),
            done: false,
        }
    }

    /// Reserves `n` bytes in the global bucket.
    pub fn reserve_global(self: &Arc<Self>, n: u64, now: Instant) -> Reservation {
        let rate = self.caps.bytes_global;
        let g = self
            .lock_buckets()
            .global
            .get_or_insert_with(|| Bucket::new(now, rate))
            .take(n, rate, now);
        Reservation {
            limiter: self.clone(),
            of: Reserved::Global,
            n,
            delay: g,
            done: false,
        }
    }

    /// How long a chunk of `n` bytes of `app` waits for all three buckets, taken at once and
    /// kept (tests).
    pub fn bytes(
        self: &Arc<Self>,
        tenant: TenantId,
        app: NodeId,
        n: u64,
        now: Instant,
    ) -> Duration {
        let own = self.reserve_own(tenant, app, n, now);
        let global = self.reserve_global(n, now);
        let d = own.delay().max(global.delay());
        own.commit();
        global.commit();
        d
    }

    fn give_back(&self, of: Reserved, n: u64) {
        let caps = self.caps;
        let mut b = self.lock_buckets();
        match of {
            Reserved::Own(tenant, app) => {
                if let Some(x) = b.app.get_mut(&(tenant, app)) {
                    x.give_back(n, caps.bytes_per_app);
                }
                if let Some(x) = b.tenant.get_mut(&tenant) {
                    x.give_back(n, caps.bytes_per_tenant);
                }
            }
            Reserved::Global => {
                if let Some(x) = b.global.as_mut() {
                    x.give_back(n, caps.bytes_global);
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Reserved {
    Own(TenantId, NodeId),
    Global,
}

/// Bytes taken from buckets for one chunk. Dropped before [`Reservation::commit`] (the stream
/// was cancelled while it waited), it gives the bytes back.
pub struct Reservation {
    limiter: Arc<Limiter>,
    of: Reserved,
    n: u64,
    delay: Duration,
    done: bool,
}

impl Reservation {
    /// How long the chunk waits before it goes.
    pub fn delay(&self) -> Duration {
        self.delay
    }

    /// Waits until the chunk may go.
    pub async fn wait(&self) {
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
    }

    /// The chunk went: the bytes stay spent.
    pub fn commit(mut self) {
        self.done = true;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.done {
            self.limiter.give_back(self.of, self.n);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn ids() -> (TenantId, NodeId) {
        (TenantId(Uuid::now_v7()), NodeId(Uuid::now_v7()))
    }

    #[test]
    fn each_cap_refuses_its_own_first() {
        let l = Limiter::new(Caps {
            per_app: 2,
            per_tenant: 3,
            global: 4,
            ..Caps::default()
        });
        let (t, a) = ids();
        let b = NodeId(Uuid::now_v7());
        let p1 = l.acquire(t, a).unwrap();
        let _p2 = l.acquire(t, a).unwrap();
        assert_eq!(l.acquire(t, a).err(), Some(Refused::App));
        // Another app of the tenant still runs, up to the tenant cap.
        let _p3 = l.acquire(t, b).unwrap();
        assert_eq!(l.acquire(t, b).err(), Some(Refused::Tenant));
        // Another tenant is not touched by the first tenant's cap.
        let (u, c) = ids();
        let _p4 = l.acquire(u, c).unwrap();
        assert_eq!(l.acquire(u, c).err(), Some(Refused::Global));
        drop(p1);
        assert!(l.acquire(u, c).is_ok());
    }

    #[test]
    fn a_stream_over_its_rate_waits() {
        let l = Limiter::new(Caps {
            bytes_per_app: 1000,
            bytes_per_tenant: 10_000,
            bytes_global: 100_000,
            ..Caps::default()
        });
        let (t, a) = ids();
        let now = Instant::now();
        assert_eq!(l.bytes(t, a, 1000, now), Duration::ZERO);
        let w = l.bytes(t, a, 500, now);
        assert!(
            w >= Duration::from_millis(450) && w <= Duration::from_millis(550),
            "{w:?}"
        );
        // Another app of another tenant is not slowed by it.
        let (u, b) = ids();
        assert_eq!(l.bytes(u, b, 1000, now), Duration::ZERO);
        // The bucket fills again with time.
        assert_eq!(
            l.bytes(t, a, 100, now + Duration::from_secs(2)),
            Duration::ZERO
        );
    }

    /// Kimi review 3: a stream that is cancelled while it waits leaves no debt; and a chunk
    /// that waits for its own tenant holds no global capacity.
    #[test]
    fn a_cancelled_stream_leaves_no_debt() {
        let l = Limiter::new(Caps {
            bytes_per_app: 1000,
            bytes_per_tenant: 1000,
            bytes_global: 2000,
            ..Caps::default()
        });
        let (t, a) = ids();
        let (u, b) = ids();
        let now = Instant::now();
        // Tenant t starts and cancels many big reads: each reservation is dropped unsent.
        for _ in 0..50 {
            let own = l.reserve_own(t, a, 5000, now);
            assert!(own.delay() > Duration::ZERO);
            drop(own);
            let g = l.reserve_global(5000, now);
            assert!(g.delay() > Duration::ZERO);
            drop(g);
        }
        // Tenant u is not slowed by any of it, and neither is t once it reads normally.
        assert_eq!(l.bytes(u, b, 1000, now), Duration::ZERO);
        assert_eq!(l.reserve_own(t, a, 1000, now).delay(), Duration::ZERO);
        // Positive control: bytes that went stay spent.
        l.reserve_own(t, a, 1000, now).commit();
        assert!(l.reserve_own(t, a, 1, now).delay() > Duration::ZERO);
    }
}
