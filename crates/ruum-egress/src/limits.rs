//! Per-identity limits (decision 23 "logs and limits"): tunnels at once, a connection rate (a
//! token bucket), an idle timeout and a maximum lifetime.
//!
//! The rate counts every client connection of an identity at accept, before its request head is
//! read, so a refused request (a wrong method, port or host) costs a token too (Kimi review of
//! run 11, finding 1). A connection over the rate is closed at once; its refusals are logged as
//! one line per [`RATE_REPORT_INTERVAL`] with their number, not one line each.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::api::Limits;

/// Client connections an identity may hold open beyond its tunnels: connections that are still
/// sending their request head (or are being refused). With the tunnels, this bounds what one app
/// can take from the proxy's global connection cap, so connections that send nothing (slowloris)
/// cannot stop the egress of other apps.
pub const PENDING_ALLOWANCE: u32 = 16;

/// At most one log line per identity in this time for connections refused over the rate.
pub const RATE_REPORT_INTERVAL: Duration = Duration::from_secs(10);

/// The limits of one identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LimitConfig {
    pub max_tunnels: u32,
    pub per_minute: u32,
    pub burst: u32,
    pub idle_timeout: Duration,
    pub max_lifetime: Duration,
}

impl Default for LimitConfig {
    /// The defaults: 64 tunnels at once, 120 new connections per minute with a burst of 30,
    /// five minutes idle, one hour at most.
    fn default() -> Self {
        LimitConfig {
            max_tunnels: 64,
            per_minute: 120,
            burst: 30,
            idle_timeout: Duration::from_secs(300),
            max_lifetime: Duration::from_secs(3600),
        }
    }
}

impl LimitConfig {
    /// The limits of a registration: each value of `lower` may only lower the configured one.
    pub fn lowered_by(&self, lower: Option<&Limits>) -> LimitConfig {
        let Some(l) = lower else { return *self };
        let min_u32 = |cfg: u32, v: Option<u32>| v.map_or(cfg, |v| v.min(cfg));
        let min_dur =
            |cfg: Duration, v: Option<u64>| v.map_or(cfg, |v| Duration::from_secs(v).min(cfg));
        LimitConfig {
            max_tunnels: min_u32(self.max_tunnels, l.max_tunnels),
            per_minute: min_u32(self.per_minute, l.per_minute),
            burst: min_u32(self.burst, l.burst),
            idle_timeout: min_dur(self.idle_timeout, l.idle_timeout_secs),
            max_lifetime: min_dur(self.max_lifetime, l.max_lifetime_secs),
        }
    }
}

/// The counters of one identity: its open tunnels, its open connections and its token bucket. Shared by the
/// registrations of the same identity; dropped with the last of them (and its last tunnel), so
/// the proxy keeps no state for sources it does not know.
#[derive(Debug)]
pub struct LimitState {
    open: AtomicU32,
    /// Client connections from the identity's networks, from accept to close.
    connections: AtomicU32,
    bucket: Mutex<Bucket>,
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
    /// The bucket size the tokens were last capped at (a lower registration limit applies at
    /// the next take).
    size: u32,
    /// Connections refused over the rate and not yet logged.
    refused: u64,
    /// When the last log line for refused connections was written.
    reported: Option<Instant>,
}

impl LimitState {
    pub fn new(cfg: &LimitConfig) -> Arc<LimitState> {
        Arc::new(LimitState {
            open: AtomicU32::new(0),
            connections: AtomicU32::new(0),
            bucket: Mutex::new(Bucket {
                tokens: f64::from(cfg.burst),
                last: Instant::now(),
                size: cfg.burst,
                refused: 0,
                reported: None,
            }),
        })
    }

    /// Takes a tunnel slot, or nothing. The slot is given back when the permit is dropped. The
    /// connection paid its token at accept ([`LimitState::take_connection_token`]).
    pub fn try_acquire(self: &Arc<Self>, cfg: &LimitConfig) -> Option<TunnelPermit> {
        self.open
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < cfg.max_tunnels).then_some(n + 1)
            })
            .ok()
            .map(|_| TunnelPermit(self.clone()))
    }

    /// Takes a token of the bucket for a new client connection, at accept and before anything
    /// is read. `Err(report)` when the bucket is empty: `report` is the number of refused
    /// connections to log now (this one and those since the last line), at most once per
    /// [`RATE_REPORT_INTERVAL`]; `None` means: count it, log nothing now.
    pub fn take_connection_token(&self, cfg: &LimitConfig) -> Result<(), Option<u64>> {
        let mut b = self.bucket.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        let refill = now.duration_since(b.last).as_secs_f64() * f64::from(cfg.per_minute) / 60.0;
        b.size = cfg.burst;
        b.tokens = (b.tokens + refill).min(f64::from(b.size));
        b.last = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            return Ok(());
        }
        b.refused = b.refused.saturating_add(1);
        let due = b
            .reported
            .is_none_or(|t| now.duration_since(t) >= RATE_REPORT_INTERVAL);
        if !due {
            return Err(None);
        }
        b.reported = Some(now);
        Err(Some(std::mem::take(&mut b.refused)))
    }

    pub fn open_tunnels(&self) -> u32 {
        self.open.load(Ordering::Acquire)
    }

    /// Takes a connection slot at accept, before the request head is read: at most
    /// `max_tunnels + PENDING_ALLOWANCE` client connections of the identity at once. The slot is
    /// given back when the permit is dropped (the connection closes).
    pub fn try_open_connection(self: &Arc<Self>, cfg: &LimitConfig) -> Option<ConnectionPermit> {
        let max = cfg.max_tunnels.saturating_add(PENDING_ALLOWANCE);
        self.connections
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < max).then_some(n + 1)
            })
            .ok()
            .map(|_| ConnectionPermit(self.clone()))
    }

    pub fn open_connections(&self) -> u32 {
        self.connections.load(Ordering::Acquire)
    }
}

/// One open client connection of an identity.
#[derive(Debug)]
pub struct ConnectionPermit(Arc<LimitState>);

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::AcqRel);
    }
}

/// One open tunnel of an identity.
#[derive(Debug)]
pub struct TunnelPermit(Arc<LimitState>);

impl Drop for TunnelPermit {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrency() {
        let cfg = LimitConfig {
            max_tunnels: 2,
            per_minute: 0,
            burst: 3,
            ..LimitConfig::default()
        };
        let s = LimitState::new(&cfg);
        let a = s.try_acquire(&cfg).unwrap();
        let _b = s.try_acquire(&cfg).unwrap();
        assert!(s.try_acquire(&cfg).is_none(), "third at once");
        drop(a);
        let _c = s.try_acquire(&cfg).unwrap();
        assert_eq!(s.open_tunnels(), 2);
    }

    #[test]
    fn rate_with_sampled_reports() {
        let cfg = LimitConfig {
            per_minute: 0,
            burst: 3,
            ..LimitConfig::default()
        };
        let s = LimitState::new(&cfg);
        for i in 0..3 {
            assert_eq!(s.take_connection_token(&cfg), Ok(()), "burst {i}");
        }
        // The first refusal is logged at once, the next ones only counted.
        assert_eq!(s.take_connection_token(&cfg), Err(Some(1)));
        for _ in 0..1000 {
            assert_eq!(s.take_connection_token(&cfg), Err(None));
        }
        assert_eq!(s.bucket.lock().unwrap().refused, 1000);
        // After the interval, one line carries all of them.
        s.bucket.lock().unwrap().reported = Some(Instant::now() - RATE_REPORT_INTERVAL);
        assert_eq!(s.take_connection_token(&cfg), Err(Some(1001)));
        assert_eq!(s.take_connection_token(&cfg), Err(None));
    }

    #[test]
    fn connections_beyond_tunnels() {
        let cfg = LimitConfig {
            max_tunnels: 2,
            ..LimitConfig::default()
        };
        let s = LimitState::new(&cfg);
        let held: Vec<_> = (0..2 + PENDING_ALLOWANCE)
            .map(|_| s.try_open_connection(&cfg).unwrap())
            .collect();
        assert!(s.try_open_connection(&cfg).is_none(), "over the cap");
        assert_eq!(s.open_connections(), 2 + PENDING_ALLOWANCE);
        drop(held);
        assert_eq!(s.open_connections(), 0);
        assert!(s.try_open_connection(&cfg).is_some(), "a slot came back");
    }

    #[test]
    fn registration_limits_only_lower() {
        let cfg = LimitConfig::default();
        let l = Limits {
            max_tunnels: Some(1_000_000),
            per_minute: Some(5),
            burst: None,
            idle_timeout_secs: Some(10),
            max_lifetime_secs: Some(1_000_000),
        };
        let got = cfg.lowered_by(Some(&l));
        assert_eq!(got.max_tunnels, cfg.max_tunnels);
        assert_eq!(got.per_minute, 5);
        assert_eq!(got.burst, cfg.burst);
        assert_eq!(got.idle_timeout, Duration::from_secs(10));
        assert_eq!(got.max_lifetime, cfg.max_lifetime);
    }
}
