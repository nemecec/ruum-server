//! The in-memory approval store. One async lock serializes every write, so two requests of one
//! key make one row and two takes of one approval cannot both succeed. An approved grant
//! request writes the grant through the grant store of the services ([`GrantStore::create`])
//! under that lock, after every check of the decision; nothing after it can fail, so the
//! decision and the grant are one step as on Postgres.

use super::{
    ApprovalFilter, ApprovalStatus, ApprovalStore, ApprovalSubject, DatabaseApproval, Decision,
    NewApproval, PENDING_HOURS, TEXT_MAX_CHARS, Take, approval_not_found, check_take, decided,
    decided_data, micros, not_pending, page_of, parse_cursor,
};
use crate::actor::Context;
use crate::authz::Authz;
use crate::databases::access::grants::GrantStore;
use crate::error::{Error, ErrorCode, Result};
use crate::ids::{NodeId, TenantId};
use crate::journal::{ChangeInfo, EventType, NewEntry, Written};
use crate::paging::{Page, PageRequest};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use time::OffsetDateTime;
use uuid::Uuid;

/// The in-memory store. It writes the journal entries through the in-memory journal and the
/// grants of approved grant requests through the in-memory grant store.
pub struct MemoryApprovalStore {
    nodes: Arc<crate::nodes::memory::MemoryNodeStore>,
    journal: Arc<crate::journal::memory::MemoryJournal>,
    grants: Arc<dyn GrantStore>,
    rows: Mutex<Vec<DatabaseApproval>>,
    /// Held across a whole write (the grant write is async).
    writes: tokio::sync::Mutex<()>,
}

impl MemoryApprovalStore {
    pub fn new(
        nodes: Arc<crate::nodes::memory::MemoryNodeStore>,
        journal: Arc<crate::journal::memory::MemoryJournal>,
        grants: Arc<dyn GrantStore>,
    ) -> Self {
        Self {
            nodes,
            journal,
            grants,
            rows: Mutex::new(Vec::new()),
            writes: tokio::sync::Mutex::new(()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<DatabaseApproval>> {
        self.rows.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn revision(&self, tenant: TenantId, node: NodeId) -> Result<i64> {
        self.nodes
            .peek(node)
            .filter(|n| n.tenant_id == tenant)
            .map(|n| n.revision)
            .ok_or_else(|| Error::not_found("database"))
    }

    fn find(&self, tenant: TenantId, node: NodeId, id: Uuid) -> Option<DatabaseApproval> {
        self.lock()
            .iter()
            .find(|a| a.tenant_id == tenant && a.node_id == node && a.id == id)
            .cloned()
    }

    fn replace(&self, a: &DatabaseApproval) {
        if let Some(row) = self
            .lock()
            .iter_mut()
            .find(|r| r.tenant_id == a.tenant_id && r.id == a.id)
        {
            *row = a.clone();
        }
    }
}

/// Whether `a` has the key of `new` from the agent `agent` (see [`ApprovalStore::request`]).
fn same_key(a: &DatabaseApproval, ctx: &Context, new: &NewApproval) -> bool {
    a.tenant_id == ctx.tenant
        && a.node_id == new.node
        && a.requested_by.actor == ctx.actor.actor
        && a.subject == new.subject
        && match new.subject {
            ApprovalSubject::Grant => a.grant_kind == new.grant_kind,
            _ => a.statement_hash == new.statement_hash,
        }
}

#[async_trait]
impl ApprovalStore for MemoryApprovalStore {
    async fn request(
        &self,
        ctx: &Context,
        new: NewApproval,
        at: OffsetDateTime,
    ) -> Result<(DatabaseApproval, bool)> {
        let _w = self.writes.lock().await;
        self.revision(ctx.tenant, new.node)?;
        let at = micros(at);
        {
            let mut rows = self.lock();
            for a in rows
                .iter_mut()
                .filter(|a| same_key(a, ctx, &new) && a.status == ApprovalStatus::Pending)
            {
                if a.status_at(at) == ApprovalStatus::Pending {
                    return Ok((a.clone(), false));
                }
                // A pending row past its time: mark it, so one pending row of a key exists.
                a.status = ApprovalStatus::Expired;
            }
        }
        let a = DatabaseApproval {
            id: Uuid::now_v7(),
            tenant_id: ctx.tenant,
            node_id: new.node,
            subject: new.subject,
            status: ApprovalStatus::Pending,
            migration_name: new.migration_name,
            statement: new.statement,
            planned_columns: new.planned_columns,
            grant_kind: new.grant_kind,
            grant_minutes: new.grant_minutes,
            grant_reason: new.grant_reason,
            grant_id: None,
            statement_hash: new.statement_hash,
            params: new.params,
            reasons: new.reasons,
            estimated_rows: new.estimated_rows,
            requested_by: ctx.actor,
            decided_by: None,
            decided_by_display_name: None,
            decided_at: None,
            comment: None,
            expires_at: at + time::Duration::hours(PENDING_HOURS),
            used_at: None,
            created_at: at,
        };
        self.lock().push(a.clone());
        Ok((a, true))
    }

    async fn get(
        &self,
        tenant: TenantId,
        node: NodeId,
        id: Uuid,
    ) -> Result<Option<DatabaseApproval>> {
        Ok(self.find(tenant, node, id))
    }

    async fn list(
        &self,
        tenant: TenantId,
        filter: ApprovalFilter,
        page: PageRequest,
        at: OffsetDateTime,
    ) -> Result<Page<DatabaseApproval>> {
        let after = parse_cursor(&page)?;
        let limit = page.limit() as usize;
        let mut items: Vec<DatabaseApproval> = self
            .lock()
            .iter()
            .filter(|a| a.tenant_id == tenant && filter.allows(a, at))
            .filter(|a| after.is_none_or(|c| a.id < c))
            .cloned()
            .collect();
        items.sort_by_key(|x| std::cmp::Reverse(x.id));
        items.truncate(limit + 1);
        Ok(page_of(items, limit))
    }

    async fn decide(
        &self,
        ctx: &Context,
        authz: &dyn Authz,
        node: NodeId,
        id: Uuid,
        decision: Decision,
        at: OffsetDateTime,
    ) -> Result<Written<DatabaseApproval>> {
        let user = ctx
            .actor
            .effective_user()
            .ok_or_else(Error::user_required)?;
        let _w = self.writes.lock().await;
        let a = self
            .find(ctx.tenant, node, id)
            .ok_or_else(approval_not_found)?;
        if a.status_at(at) != ApprovalStatus::Pending {
            return Err(not_pending());
        }
        // The checks of the row (the constraints of `database_approvals`) before any write, so
        // a refused decision writes no grant.
        if decision
            .comment
            .as_ref()
            .is_some_and(|c| c.chars().count() > TEXT_MAX_CHARS)
        {
            return Err(Error::api(
                ErrorCode::ValidationFailed,
                "comment has at most 500 characters",
            ));
        }
        let revision = self.revision(ctx.tenant, node)?;
        let grant_id = match (&decision.grant, decision.approve, a.subject) {
            (Some(g), true, ApprovalSubject::Grant) => {
                Some(self.grants.create(ctx, authz, g.clone()).await?.value.id)
            }
            (None, true, ApprovalSubject::Grant) => {
                return Err(Error::Internal(
                    "an approved grant request needs its grant".into(),
                ));
            }
            _ => None,
        };
        let (a, classes) = decided(a, user, &decision, grant_id, at);
        self.replace(&a);
        let e = self.journal.append(
            ctx,
            NewEntry {
                node_id: Some(node),
                revision: Some(revision),
                event: EventType::DatabaseApprovalDecided,
                data: decided_data(&a, decision.approve, classes),
            },
        )?;
        Ok(Written {
            value: a,
            change: ChangeInfo {
                seq: Some(e.seq),
                actor: e.actor,
                at: e.at,
            },
        })
    }

    async fn take(
        &self,
        tenant: TenantId,
        node: NodeId,
        id: Uuid,
        take: &Take,
        at: OffsetDateTime,
    ) -> Result<DatabaseApproval> {
        let _w = self.writes.lock().await;
        let mut a = self.find(tenant, node, id).ok_or_else(approval_not_found)?;
        check_take(&a, take, at)?;
        a.status = ApprovalStatus::Used;
        a.used_at = Some(micros(at));
        self.replace(&a);
        Ok(a)
    }
}
