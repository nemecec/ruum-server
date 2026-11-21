//! Fair share and limits of the runtime API (card R-3; backlog S-61, M-59).
//!
//! - **Concurrent requests**: at most [`Caps::per_app`] per app (its live and preview
//!   principals together), [`Caps::per_tenant`] per tenant and [`Caps::global`] in all. A
//!   request over a cap gets `429 rate_limited` with `Retry-After: 1` (not in the contract yet:
//!   an additive line for the panel). Who is refused first: an app at its own cap is refused
//!   before its tenant's other apps feel anything; a tenant at its cap is refused before other
//!   tenants feel anything; at the global cap every new request is refused until one ends, and
//!   no tenant holds more than `per_tenant / global` (1/8 by default) of it.
//! - **Streamed bytes** of `readBoundContent`: token buckets of [`Caps::bytes_per_app`],
//!   [`Caps::bytes_per_tenant`] and [`Caps::bytes_global`] per second. A stream over a bucket
//!   is slowed down (each chunk waits for the slowest of its three buckets), never cut. One
//!   tenant's streams spend only its own and the global bucket; the per-tenant rate (1/8 of the
//!   global one by default) keeps the global bucket for at least 8 tenants at full speed.
//! - The listener has a header read timeout and caps on connections in all and per peer address
//!   (see `crate::serve`; backlog S-71).

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
    /// How long a client may take to send the request head.
    pub header_timeout: Duration,
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
            header_timeout: Duration::from_secs(10),
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

    /// How long a chunk of `n` bytes of `app` waits (the slowest of its three buckets).
    pub fn bytes(&self, tenant: TenantId, app: NodeId, n: u64, now: Instant) -> Duration {
        let caps = self.caps;
        let mut b = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
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
        let g = b
            .global
            .get_or_insert_with(|| Bucket::new(now, caps.bytes_global))
            .take(n, caps.bytes_global, now);
        a.max(t).max(g)
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
}
