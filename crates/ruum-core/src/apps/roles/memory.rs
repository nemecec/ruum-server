//! The in-memory [`RoleStore`]. One lock over all rows; each method reads and writes only the
//! rows of its tenant. The engine is written after the row; when the engine refuses, the row
//! and the journal entry are not kept (the rollback of the Postgres transaction).

use super::*;
use crate::journal::{ChangeInfo, EventType, NewEntry, memory::MemoryJournal};
use std::sync::Mutex;

/// The in-memory store. With a journal, writes append their entries there.
pub struct MemoryRoleStore {
    journal: Option<Arc<MemoryJournal>>,
    rows: Mutex<BTreeMap<(TenantId, Uuid), RoleGrant>>,
}

impl MemoryRoleStore {
    pub fn new(journal: Option<Arc<MemoryJournal>>) -> Self {
        Self {
            journal,
            rows: Mutex::new(BTreeMap::new()),
        }
    }

    fn rows(&self) -> std::sync::MutexGuard<'_, BTreeMap<(TenantId, Uuid), RoleGrant>> {
        self.rows.lock().expect("role store lock")
    }

    fn change(
        &self,
        ctx: &Context,
        app: NodeId,
        event: EventType,
        data: Value,
    ) -> Result<ChangeInfo> {
        match &self.journal {
            Some(j) => {
                let e = j.append(
                    ctx,
                    NewEntry {
                        node_id: Some(app),
                        revision: None,
                        event,
                        data,
                    },
                )?;
                Ok(ChangeInfo {
                    seq: Some(e.seq),
                    actor: ctx.actor,
                    at: e.at,
                })
            }
            None => Ok(ChangeInfo {
                seq: None,
                actor: ctx.actor,
                at: crate::now(),
            }),
        }
    }
}

/// The same grantee (the unique index of `app_role_grants`; emails case-insensitive).
fn same_grantee(a: &Grantee, b: &Grantee) -> bool {
    match (a, b) {
        (Grantee::User { id: x }, Grantee::User { id: y }) => x == y,
        (Grantee::Email { email: x }, Grantee::Email { email: y }) => {
            x.to_lowercase() == y.to_lowercase()
        }
        (Grantee::TenantMembers, Grantee::TenantMembers) => true,
        _ => false,
    }
}

#[async_trait]
impl RoleStore for MemoryRoleStore {
    async fn add(
        &self,
        ctx: &Context,
        authz: &dyn Authz,
        grant: NewRoleGrant,
    ) -> Result<Written<RoleGrant>> {
        let grantee = grant.stored_grantee();
        let row = RoleGrant {
            tenant_id: ctx.tenant,
            id: grant.id,
            app_id: grant.app_id,
            role: grant.role,
            grantee,
            granted_by: ctx.actor,
            created_at: crate::now(),
        };
        {
            // Check and reserve under one lock (the unique index of the table).
            let mut rows = self.rows();
            if rows.values().any(|g| {
                g.tenant_id == ctx.tenant
                    && g.app_id == row.app_id
                    && g.role == row.role
                    && same_grantee(&g.grantee, &row.grantee)
            }) {
                return Err(duplicate());
            }
            rows.insert((ctx.tenant, row.id), row.clone());
        }
        let written = async {
            if let Some(t) = row.tuple() {
                authz.write(vec![RelationshipUpdate::Touch(t)]).await?;
            }
            self.change(ctx, row.app_id, EventType::AppRoleGranted, event_data(&row))
        }
        .await;
        let change = match written {
            Ok(c) => c,
            Err(e) => {
                self.rows().remove(&(ctx.tenant, row.id));
                return Err(e);
            }
        };
        Ok(Written { value: row, change })
    }

    async fn get(&self, tenant: TenantId, app: NodeId, id: Uuid) -> Result<Option<RoleGrant>> {
        Ok(self
            .rows()
            .get(&(tenant, id))
            .filter(|g| g.app_id == app)
            .cloned())
    }

    async fn remove(
        &self,
        ctx: &Context,
        authz: &dyn Authz,
        app: NodeId,
        id: Uuid,
    ) -> Result<Option<Written<RoleGrant>>> {
        let Some(row) = self.get(ctx.tenant, app, id).await? else {
            return Ok(None);
        };
        if let Some(t) = row.tuple() {
            authz.write(vec![RelationshipUpdate::Delete(t)]).await?;
        }
        let change = self.change(ctx, app, EventType::AppRoleRevoked, event_data(&row))?;
        self.rows().remove(&(ctx.tenant, id));
        Ok(Some(Written { value: row, change }))
    }

    async fn list(
        &self,
        tenant: TenantId,
        app: NodeId,
        role: Option<&str>,
        page: PageRequest,
    ) -> Result<Page<RoleGrant>> {
        let after = page.cursor.as_ref().map(parse_grant_cursor).transpose()?;
        let limit = page.limit() as usize;
        let mut items: Vec<RoleGrant> = self
            .rows()
            .values()
            .filter(|g| {
                g.tenant_id == tenant
                    && g.app_id == app
                    && role.is_none_or(|r| g.role == r)
                    && after.is_none_or(|a| g.id > a)
            })
            .cloned()
            .collect();
        items.sort_by_key(|g| g.id);
        let next_cursor = if items.len() > limit {
            items.truncate(limit);
            items.last().map(|g| grant_cursor(g.id))
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }

    async fn counts(&self, tenant: TenantId, app: NodeId) -> Result<BTreeMap<String, u32>> {
        let mut out = BTreeMap::new();
        for g in self.rows().values() {
            if g.tenant_id == tenant && g.app_id == app {
                *out.entry(g.role.clone()).or_insert(0) += 1;
            }
        }
        Ok(out)
    }

    async fn bind_email(
        &self,
        ctx: &Context,
        authz: &dyn Authz,
        email: &str,
        user: UserId,
    ) -> Result<usize> {
        let email_grants: Vec<RoleGrant> = self
            .rows()
            .values()
            .filter(|g| {
                g.tenant_id == ctx.tenant
                    && matches!(&g.grantee, Grantee::Email { email: e }
                        if e.to_lowercase() == email.to_lowercase())
            })
            .cloned()
            .collect();
        let mut bound = 0;
        for g in email_grants {
            let held = self.rows().values().any(|o| {
                o.tenant_id == ctx.tenant
                    && o.app_id == g.app_id
                    && o.role == g.role
                    && o.grantee == Grantee::User { id: user }
            });
            if held {
                self.change(ctx, g.app_id, EventType::AppRoleRevoked, event_data(&g))?;
                self.rows().remove(&(ctx.tenant, g.id));
            } else {
                let mut b = g.clone();
                b.grantee = Grantee::User { id: user };
                if let Some(t) = b.tuple() {
                    authz.write(vec![RelationshipUpdate::Touch(t)]).await?;
                }
                self.change(ctx, g.app_id, EventType::AppRoleRevoked, event_data(&g))?;
                self.change(ctx, g.app_id, EventType::AppRoleGranted, event_data(&b))?;
                self.rows().insert((ctx.tenant, b.id), b);
            }
            bound += 1;
        }
        Ok(bound)
    }
}
