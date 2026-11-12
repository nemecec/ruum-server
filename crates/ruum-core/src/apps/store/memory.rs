//! The in-memory [`AppStore`]. One lock over all rows; each method reads and writes only the
//! rows of its tenant (the tenant slug indexes span tenants, as in Postgres).

use super::*;
use crate::journal::{EventType, NewEntry, memory::MemoryJournal};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Inner {
    tenants: HashMap<TenantId, TenantAppSettings>,
    /// Every tenant slug ever given, and its last tenant (spans tenants).
    tenant_slugs: HashMap<String, TenantId>,
    apps: HashMap<(TenantId, NodeId), AppRow>,
    /// Every app slug ever given in a tenant, and its app node.
    app_slugs: HashMap<(TenantId, String), NodeId>,
    deployments: HashMap<(TenantId, Uuid), DeploymentRow>,
}

/// The in-memory store. With a journal, `update_settings` appends its entry there.
pub struct MemoryAppStore {
    journal: Option<Arc<MemoryJournal>>,
    inner: Mutex<Inner>,
}

impl MemoryAppStore {
    pub fn new(journal: Option<Arc<MemoryJournal>>) -> Self {
        Self {
            journal,
            inner: Mutex::new(Inner::default()),
        }
    }

    fn st(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("app store lock")
    }
}

fn ensure(st: &mut Inner, tenant: TenantId, candidates: &[String]) -> Result<TenantAppSettings> {
    if let Some(t) = st.tenants.get(&tenant) {
        return Ok(t.clone());
    }
    let slug = candidates
        .iter()
        .find(|c| !st.tenant_slugs.contains_key(c.as_str()))
        .ok_or_else(no_free_slug)?
        .clone();
    st.tenant_slugs.insert(slug.clone(), tenant);
    let row = TenantAppSettings {
        tenant_id: tenant,
        slug,
        public_apps_approved: false,
        custom_domains_approved: false,
    };
    st.tenants.insert(tenant, row.clone());
    Ok(row)
}

#[async_trait]
impl AppStore for MemoryAppStore {
    async fn tenant_settings(&self, tenant: TenantId) -> Result<Option<TenantAppSettings>> {
        Ok(self.st().tenants.get(&tenant).cloned())
    }

    async fn ensure_tenant_settings(
        &self,
        tenant: TenantId,
        candidates: &[String],
    ) -> Result<TenantAppSettings> {
        ensure(&mut self.st(), tenant, candidates)
    }

    async fn set_approval(
        &self,
        tenant: TenantId,
        candidates: &[String],
        change: ApprovalChange,
    ) -> Result<TenantAppSettings> {
        let mut st = self.st();
        ensure(&mut st, tenant, candidates)?;
        let row = st.tenants.get_mut(&tenant).expect("ensured above");
        if let Some(p) = change.public_apps {
            row.public_apps_approved = p;
        }
        if let Some(d) = change.custom_domains {
            row.custom_domains_approved = d;
        }
        Ok(row.clone())
    }

    async fn slug_state(&self, tenant: TenantId, slug: &str, node: NodeId) -> Result<SlugState> {
        let st = self.st();
        if let Some(a) = st
            .apps
            .values()
            .find(|a| a.tenant_id == tenant && a.slug == slug && a.node_id != node)
        {
            return Ok(SlugState::InUse(a.node_id));
        }
        Ok(match st.app_slugs.get(&(tenant, slug.to_string())) {
            Some(n) if *n != node => SlugState::Retired,
            _ => SlugState::Free,
        })
    }

    async fn insert_app(&self, ctx: &Context, app: NewApp) -> Result<Option<AppRow>> {
        let mut st = self.st();
        let tenant = ctx.tenant;
        if st.apps.contains_key(&(tenant, app.node_id)) {
            return Err(Error::api(
                crate::error::ErrorCode::Conflict,
                "the app row exists",
            ));
        }
        let key = (tenant, app.slug.clone());
        let taken_now = st
            .apps
            .values()
            .any(|a| a.tenant_id == tenant && a.slug == app.slug);
        let used_by_other = st.app_slugs.get(&key).is_some_and(|n| *n != app.node_id);
        if taken_now || used_by_other {
            return Ok(None);
        }
        let now = crate::now();
        let row = AppRow {
            tenant_id: tenant,
            node_id: app.node_id,
            slug: app.slug,
            settings: app.settings,
            live_deployment_id: None,
            slug_fixed_at: None,
            created_at: now,
            updated_at: now,
        };
        st.app_slugs.insert(key, app.node_id);
        st.apps.insert((tenant, app.node_id), row.clone());
        Ok(Some(row))
    }

    async fn get_app(&self, tenant: TenantId, node: NodeId) -> Result<Option<AppRow>> {
        Ok(self.st().apps.get(&(tenant, node)).cloned())
    }

    async fn count_apps(&self, tenant: TenantId) -> Result<u32> {
        Ok(self.st().apps.keys().filter(|(t, _)| *t == tenant).count() as u32)
    }

    async fn update_settings(
        &self,
        ctx: &Context,
        node: NodeId,
        settings: &AppSettings,
        data: Value,
    ) -> Result<(AppRow, ChangeInfo)> {
        let row = {
            let mut st = self.st();
            let row = st
                .apps
                .get_mut(&(ctx.tenant, node))
                .ok_or_else(|| Error::not_found("app"))?;
            row.settings = *settings;
            row.updated_at = crate::now();
            row.clone()
        };
        let change = match &self.journal {
            Some(j) => {
                let e = j.append(
                    ctx,
                    NewEntry {
                        node_id: Some(node),
                        revision: None,
                        event: EventType::AppSettingsChanged,
                        data,
                    },
                )?;
                ChangeInfo {
                    seq: Some(e.seq),
                    actor: ctx.actor,
                    at: e.at,
                }
            }
            None => ChangeInfo {
                seq: None,
                actor: ctx.actor,
                at: row.updated_at,
            },
        };
        Ok((row, change))
    }

    async fn remove_app(&self, tenant: TenantId, node: NodeId) -> Result<()> {
        let mut st = self.st();
        st.deployments
            .retain(|(t, _), d| !(*t == tenant && d.app_node_id == node));
        st.apps.remove(&(tenant, node));
        Ok(())
    }

    async fn insert_deployment(&self, ctx: &Context, d: NewDeployment) -> Result<DeploymentRow> {
        let mut st = self.st();
        let tenant = ctx.tenant;
        if !st.apps.contains_key(&(tenant, d.app_node_id)) {
            return Err(Error::not_found("app"));
        }
        if st
            .deployments
            .values()
            .any(|x| x.tenant_id == tenant && (x.id == d.id || x.short_id == d.short_id))
        {
            return Err(Error::api(
                crate::error::ErrorCode::Conflict,
                "the deployment exists",
            ));
        }
        let now = crate::now();
        let release = d.is_release();
        let row = DeploymentRow {
            tenant_id: tenant,
            id: d.id,
            app_node_id: d.app_node_id,
            environment: d.environment,
            state: d.state,
            short_id: d.short_id,
            version_id: release.then(VersionId::new),
            label: d.label,
            draft: d.draft,
            source: d.source,
            archive: d.archive,
            manifest: d.manifest,
            stages: d.stages,
            check_result: None,
            published_from: d.published_from,
            approval_id: None,
            created_by: ctx.actor,
            created_at: now,
            published_at: release.then_some(now),
            expires_at: d.expires_at,
        };
        if release {
            let app = st
                .apps
                .get_mut(&(tenant, d.app_node_id))
                .expect("checked above");
            if let Some(old) = app.live_deployment_id.replace(row.id)
                && let Some(prev) = st.deployments.get_mut(&(tenant, old))
            {
                prev.state = DeploymentState::Superseded;
            }
            let app = st
                .apps
                .get_mut(&(tenant, d.app_node_id))
                .expect("checked above");
            app.slug_fixed_at.get_or_insert(now);
        }
        st.deployments.insert((tenant, row.id), row.clone());
        Ok(row)
    }

    async fn get_deployment(
        &self,
        tenant: TenantId,
        node: NodeId,
        id: Uuid,
    ) -> Result<Option<DeploymentRow>> {
        Ok(self
            .st()
            .deployments
            .get(&(tenant, id))
            .filter(|d| d.app_node_id == node)
            .cloned())
    }

    async fn list_deployments(
        &self,
        tenant: TenantId,
        node: NodeId,
        environment: Option<Environment>,
        page: PageRequest,
    ) -> Result<Page<DeploymentRow>> {
        let after = page
            .cursor
            .as_ref()
            .map(decode_deployment_cursor)
            .transpose()?;
        let limit = page.limit() as usize;
        let mut items: Vec<DeploymentRow> = self
            .st()
            .deployments
            .values()
            .filter(|d| d.tenant_id == tenant && d.app_node_id == node)
            .filter(|d| environment.is_none_or(|e| d.environment == e))
            .filter(|d| after.is_none_or(|(at, id)| (d.created_at, d.id) < (at, id)))
            .cloned()
            .collect();
        items.sort_by_key(|d| std::cmp::Reverse((d.created_at, d.id)));
        items.truncate(limit + 1);
        Ok(page_of(items, limit))
    }

    async fn set_deployment_state(
        &self,
        tenant: TenantId,
        id: Uuid,
        state: DeploymentState,
    ) -> Result<bool> {
        Ok(match self.st().deployments.get_mut(&(tenant, id)) {
            Some(d) => {
                d.state = state;
                true
            }
            None => false,
        })
    }
}
