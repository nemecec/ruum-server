//! In-memory audit log, for unit tests and for running without Docker.

use super::*;
use crate::ids::TenantId;
use crate::journal::memory::MemoryJournal;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
pub struct MemoryAuditLog {
    entries: Mutex<HashMap<TenantId, Vec<AuditEntry>>>,
    /// The (tenant, session) pairs of [`AuditLog::record_first_use`].
    first_uses: Mutex<HashSet<(TenantId, Uuid)>>,
    /// The journal whose entries [`AuditLog::list`] merges in. `None`: only recorded events.
    journal: Option<Arc<MemoryJournal>>,
}

impl MemoryAuditLog {
    /// An audit log that also lists the node changes of `journal`.
    pub fn with_journal(journal: Arc<MemoryJournal>) -> Self {
        Self {
            entries: Mutex::default(),
            first_uses: Mutex::default(),
            journal: Some(journal),
        }
    }
}

#[async_trait]
impl AuditLog for MemoryAuditLog {
    async fn record(&self, ctx: &Context, entry: NewAuditEntry) -> Result<()> {
        let entry = entry.normalized()?;
        let row = AuditEntry {
            id: Uuid::now_v7(),
            action: entry.action,
            actor: ctx.actor,
            target_type: entry.target_type,
            target_id: entry.target_id,
            ip: entry.ip,
            user_agent: entry.user_agent,
            data: entry.data,
            at: crate::now(),
        };
        self.entries
            .lock()
            .expect("audit lock")
            .entry(ctx.tenant)
            .or_default()
            .push(row);
        Ok(())
    }

    async fn record_first_use(
        &self,
        ctx: &Context,
        session: Uuid,
        entry: NewAuditEntry,
    ) -> Result<bool> {
        let entry = entry.normalized()?;
        if !self
            .first_uses
            .lock()
            .expect("audit lock")
            .insert((ctx.tenant, session))
        {
            return Ok(false);
        }
        self.record(ctx, entry).await?;
        Ok(true)
    }

    async fn list(&self, ctx: &Context, query: AuditQuery) -> Result<Page<AuditEntry>> {
        let after = query.page.cursor.as_ref().map(decode_cursor).transpose()?;
        let limit = query.page.limit() as usize;
        let mut all: Vec<AuditEntry> = self
            .entries
            .lock()
            .expect("audit lock")
            .get(&ctx.tenant)
            .cloned()
            .unwrap_or_default();
        if let Some(j) = &self.journal {
            all.extend(j.entries(ctx.tenant).iter().map(from_journal));
        }
        all.retain(|e| matches(e, &query) && after.is_none_or(|c| (e.at, e.id) < c));
        all.sort_by_key(|e| std::cmp::Reverse((e.at, e.id)));
        let more = all.len() > limit;
        all.truncate(limit);
        let next_cursor = if more {
            all.last().map(|e| encode_cursor(e.at, e.id))
        } else {
            None
        };
        Ok(Page {
            items: all,
            next_cursor,
        })
    }
}
