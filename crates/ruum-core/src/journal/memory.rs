//! In-memory journal, for unit tests and for running without Docker.
//!
//! The in-memory stores share one `MemoryJournal` (in an `Arc`) and call
//! [`MemoryJournal::append`] for each change.

use super::{EventType, Journal, JournalEntry, NewEntry, check_cursor, encode_cursor};
use crate::actor::Context;
use crate::error::Result;
use crate::ids::{NodeId, TenantId};
use crate::paging::Page;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::Notify;

#[derive(Debug, Default)]
struct TenantJournal {
    /// The newest `seq` given out.
    last_seq: i64,
    /// The kept entries, oldest first.
    entries: Vec<JournalEntry>,
}

#[derive(Debug, Default)]
pub struct MemoryJournal {
    tenants: Mutex<HashMap<TenantId, TenantJournal>>,
    appended: Notify,
}

impl MemoryJournal {
    /// Appends one entry at once and returns it with its `seq`. The tenant and the actor come
    /// from `ctx`.
    pub fn append(&self, ctx: &Context, entry: NewEntry) -> Result<JournalEntry> {
        let written = {
            let mut tenants = self.tenants.lock().expect("journal lock");
            let t = tenants.entry(ctx.tenant).or_default();
            t.last_seq += 1;
            let written = JournalEntry {
                tenant_id: ctx.tenant,
                seq: t.last_seq,
                node_id: entry.node_id,
                revision: entry.revision,
                event: entry.event,
                actor: ctx.actor,
                at: crate::now(),
                data: entry.data,
            };
            t.entries.push(written.clone());
            written
        };
        self.appended.notify_waiters();
        Ok(written)
    }

    /// Drops the entries of the tenant with a `seq` below `seq`, as the journal retention does.
    /// For tests of `cursor_expired`.
    pub fn prune_before(&self, tenant: TenantId, seq: i64) {
        let mut tenants = self.tenants.lock().expect("journal lock");
        if let Some(t) = tenants.get_mut(&tenant) {
            t.entries.retain(|e| e.seq >= seq);
        }
    }

    /// All kept entries of the tenant, oldest first. For the in-memory audit log.
    pub fn entries(&self, tenant: TenantId) -> Vec<JournalEntry> {
        let tenants = self.tenants.lock().expect("journal lock");
        tenants
            .get(&tenant)
            .map(|t| t.entries.clone())
            .unwrap_or_default()
    }

    fn last_seq(&self, tenant: TenantId) -> i64 {
        let tenants = self.tenants.lock().expect("journal lock");
        tenants.get(&tenant).map_or(0, |t| t.last_seq)
    }
}

#[async_trait]
impl Journal for MemoryJournal {
    async fn read(
        &self,
        ctx: &Context,
        after: Option<i64>,
        limit: u32,
    ) -> Result<Page<JournalEntry>> {
        let tenants = self.tenants.lock().expect("journal lock");
        let (latest, oldest, entries) = match tenants.get(&ctx.tenant) {
            Some(t) => (
                t.last_seq,
                t.entries.first().map_or(t.last_seq + 1, |e| e.seq),
                t.entries.as_slice(),
            ),
            None => (0, 1, &[][..]),
        };
        let after = check_cursor(after, oldest, latest)?;
        let items: Vec<JournalEntry> = entries
            .iter()
            .filter(|e| e.seq > after)
            .take(limit.max(1) as usize)
            .cloned()
            .collect();
        let next_cursor = Some(encode_cursor(items.last().map_or(after, |e| e.seq)));
        Ok(Page { items, next_cursor })
    }

    async fn latest_seq(&self, ctx: &Context) -> Result<i64> {
        Ok(self.last_seq(ctx.tenant))
    }

    async fn node_entries(
        &self,
        ctx: &Context,
        node: NodeId,
        before: i64,
        events: &[EventType],
        limit: u32,
    ) -> Result<Vec<JournalEntry>> {
        let tenants = self.tenants.lock().expect("journal lock");
        Ok(tenants
            .get(&ctx.tenant)
            .map(|t| {
                t.entries
                    .iter()
                    .rev()
                    .filter(|e| {
                        e.seq < before && e.node_id == Some(node) && events.contains(&e.event)
                    })
                    .take(limit as usize)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn wait_for(&self, ctx: &Context, after: i64, timeout: Duration) -> Result<bool> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Register before the check, so an append between the check and the wait is seen.
            let notified = self.appended.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.last_seq(ctx.tenant) > after {
                return Ok(true);
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Ok(self.last_seq(ctx.tenant) > after);
            }
        }
    }
}
