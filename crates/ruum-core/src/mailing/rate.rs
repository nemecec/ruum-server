//! Fixed-window counters (decision 0003-2 and 0003-3). The Postgres counters live in
//! `rate_counters` (migration 0006), under the tenant's row-level security, so every server
//! process shares them. One upsert per counted call. The mail worker deletes windows older than
//! a day ([`super::outbox::clean`]).
//!
//! Counters are kept per tenant, never across tenants: a counter across tenants would write
//! into another tenant (and maybe another cell). The per-tenant caps bound the total.

use crate::error::Result;
use crate::ids::TenantId;
use sqlx::PgConnection;
use std::collections::HashMap;
use std::sync::Mutex;

/// Counts one call of `key` in the current window of `window_seconds`, in the caller's
/// transaction (which has set `ruum.tenant_id`). Returns the count of the window, this call
/// included.
pub async fn hit(
    conn: &mut PgConnection,
    tenant: TenantId,
    key: &str,
    window_seconds: i64,
) -> Result<i64> {
    let count: i32 = sqlx::query_scalar(
        "insert into rate_counters (tenant_id, key, window_start, count) \
         values ($1, $2, to_timestamp(floor(extract(epoch from now()) / $3) * $3), 1) \
         on conflict (tenant_id, key, window_start) \
         do update set count = rate_counters.count + 1 returning count",
    )
    .bind(tenant.0)
    .bind(key)
    .bind(window_seconds as f64)
    .fetch_one(conn)
    .await?;
    Ok(i64::from(count))
}

/// The count of `key` in the current window, without counting.
pub async fn peek(
    conn: &mut PgConnection,
    tenant: TenantId,
    key: &str,
    window_seconds: i64,
) -> Result<i64> {
    let count: Option<i32> = sqlx::query_scalar(
        "select count from rate_counters where tenant_id = $1 and key = $2 \
         and window_start = to_timestamp(floor(extract(epoch from now()) / $3) * $3)",
    )
    .bind(tenant.0)
    .bind(key)
    .bind(window_seconds as f64)
    .fetch_optional(conn)
    .await?;
    Ok(count.map(i64::from).unwrap_or(0))
}

/// The in-memory counters of the in-memory stores (one process).
#[derive(Debug, Default)]
pub struct MemoryCounters {
    /// (tenant, key, window length, window number) to count.
    windows: Mutex<HashMap<(TenantId, String, i64, i64), i64>>,
}

impl MemoryCounters {
    fn window(window_seconds: i64) -> i64 {
        crate::now()
            .unix_timestamp()
            .div_euclid(window_seconds.max(1))
    }

    /// Counts one call; returns the count of the window, this call included.
    pub fn hit(&self, tenant: TenantId, key: &str, window_seconds: i64) -> i64 {
        let w = Self::window(window_seconds);
        let mut m = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|(_, _, len, start), _| *len != window_seconds || *start >= w - 1);
        let c = m
            .entry((tenant, key.to_string(), window_seconds, w))
            .or_insert(0);
        *c += 1;
        *c
    }

    /// The count of the current window.
    pub fn peek(&self, tenant: TenantId, key: &str, window_seconds: i64) -> i64 {
        let w = Self::window(window_seconds);
        self.windows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(tenant, key.to_string(), window_seconds, w))
            .copied()
            .unwrap_or(0)
    }
}
