//! Per-identity limits (decision 23 "logs and limits"): tunnels at once, a connection rate (a
//! token bucket), an idle timeout and a maximum lifetime.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::api::Limits;

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
    /// The defaults: 64 tunnels at once, 120 new tunnels per minute with a burst of 30, five
    /// minutes idle, one hour at most.
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

/// The counters of one identity: its open tunnels and its token bucket. Shared by the
/// registrations of the same identity; dropped with the last of them (and its last tunnel), so
/// the proxy keeps no state for sources it does not know.
#[derive(Debug)]
pub struct LimitState {
    open: AtomicU32,
    bucket: Mutex<Bucket>,
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
    /// The bucket size the tokens were last capped at (a lower registration limit applies at
    /// the next take).
    size: u32,
}

impl LimitState {
    pub fn new(cfg: &LimitConfig) -> Arc<LimitState> {
        Arc::new(LimitState {
            open: AtomicU32::new(0),
            bucket: Mutex::new(Bucket {
                tokens: f64::from(cfg.burst),
                last: Instant::now(),
                size: cfg.burst,
            }),
        })
    }

    /// Takes a tunnel slot and a token, or nothing. The slot is given back when the permit is
    /// dropped; a refused request takes no token.
    pub fn try_acquire(self: &Arc<Self>, cfg: &LimitConfig) -> Option<TunnelPermit> {
        let taken = self
            .open
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < cfg.max_tunnels).then_some(n + 1)
            })
            .is_ok();
        if !taken {
            return None;
        }
        let mut b = self.bucket.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        let refill = now.duration_since(b.last).as_secs_f64() * f64::from(cfg.per_minute) / 60.0;
        b.size = cfg.burst;
        b.tokens = (b.tokens + refill).min(f64::from(b.size));
        b.last = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            Some(TunnelPermit(self.clone()))
        } else {
            self.open.fetch_sub(1, Ordering::AcqRel);
            None
        }
    }

    pub fn open_tunnels(&self) -> u32 {
        self.open.load(Ordering::Acquire)
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
    fn concurrency_and_rate() {
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
        drop(_c);
        assert!(s.try_acquire(&cfg).is_none(), "the bucket is empty");
        assert_eq!(s.open_tunnels(), 1);
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
