//! Byte counters per identity and hour (decision 0006-18: never per visitor), drained by
//! `GET /v1/usage`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use time::OffsetDateTime;

use crate::api::{ByteCounts, UsageEntry, UsageReport, WorkloadIdentity};

/// The most counters between two drains. New identity-hours beyond it count into `overflow`.
pub const MAX_ENTRIES: usize = 65_536;

#[derive(Default, Clone, Copy)]
struct Counts {
    up: u64,
    down: u64,
    tunnels: u64,
    denied: u64,
}

#[derive(Default)]
pub struct Usage {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    map: HashMap<(WorkloadIdentity, i64), Counts>,
    overflow: ByteCounts,
}

fn hour_now() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    secs - secs.rem_euclid(3600)
}

impl Usage {
    pub fn new() -> Usage {
        Usage::default()
    }

    fn with(&self, id: &WorkloadIdentity, f: impl FnOnce(Option<&mut Counts>, &mut ByteCounts)) {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let Inner { map, overflow } = &mut *g;
        let key = (*id, hour_now());
        if !map.contains_key(&key) && map.len() >= MAX_ENTRIES {
            f(None, overflow);
            return;
        }
        f(Some(map.entry(key).or_default()), overflow);
    }

    pub fn add_bytes(&self, id: &WorkloadIdentity, up: u64, down: u64) {
        self.with(id, |c, overflow| match c {
            Some(c) => {
                c.up = c.up.saturating_add(up);
                c.down = c.down.saturating_add(down);
            }
            None => {
                overflow.bytes_up = overflow.bytes_up.saturating_add(up);
                overflow.bytes_down = overflow.bytes_down.saturating_add(down);
            }
        });
    }

    pub fn add_tunnel(&self, id: &WorkloadIdentity) {
        self.with(id, |c, _| {
            if let Some(c) = c {
                c.tunnels += 1;
            }
        });
    }

    pub fn add_denied(&self, id: &WorkloadIdentity) {
        self.with(id, |c, _| {
            if let Some(c) = c {
                c.denied += 1;
            }
        });
    }

    /// Returns every counter and starts again from zero.
    pub fn drain(&self) -> UsageReport {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let inner = std::mem::take(&mut *g);
        drop(g);
        let mut entries: Vec<UsageEntry> = inner
            .map
            .into_iter()
            .map(|((identity, hour), c)| UsageEntry {
                identity,
                hour: OffsetDateTime::from_unix_timestamp(hour)
                    .unwrap_or(OffsetDateTime::UNIX_EPOCH),
                bytes_up: c.up,
                bytes_down: c.down,
                tunnels: c.tunnels,
                denied: c.denied,
            })
            .collect();
        entries.sort_by_key(|a| (a.hour, a.identity));
        UsageReport {
            entries,
            overflow: inner.overflow,
        }
    }
}
