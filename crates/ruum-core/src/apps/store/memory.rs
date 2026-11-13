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
    /// The release versions of each app node, oldest first, with their deployment.
    versions: HashMap<(TenantId, NodeId), Vec<(Version, Uuid)>>,
    approvals: HashMap<(TenantId, Uuid), ApprovalRow>,
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

    /// Appends the entries to the journal (when the store has one); the change of the first.
    fn journal(
        &self,
        ctx: &Context,
        node: NodeId,
        entries: Vec<(EventType, Value)>,
        at: OffsetDateTime,
    ) -> Result<ChangeInfo> {
        let mut first = None;
        for (event, data) in entries {
            let change = match &self.journal {
                Some(j) => {
                    let e = j.append(
                        ctx,
                        NewEntry {
                            node_id: Some(node),
                            revision: None,
                            event,
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
                    at,
                },
            };
            first.get_or_insert(change);
        }
        Ok(first.unwrap_or(ChangeInfo {
            seq: None,
            actor: ctx.actor,
            at,
        }))
    }
}

/// Inserts a deployment row under the lock (the checks of [`AppStore::insert_deployment`]).
fn insert_row(
    st: &mut Inner,
    ctx: &Context,
    d: &NewDeployment,
    version: Option<VersionId>,
    now: OffsetDateTime,
) -> Result<DeploymentRow> {
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
    let release = d.is_release();
    Ok(DeploymentRow {
        tenant_id: tenant,
        id: d.id,
        app_node_id: d.app_node_id,
        environment: d.environment,
        state: d.state,
        short_id: d.short_id.clone(),
        version_id: version,
        label: d.label.clone(),
        draft: d.draft.clone(),
        source: d.source.clone(),
        archive: d.archive.clone(),
        manifest: d.manifest.clone(),
        stages: d.stages.clone(),
        check_result: None,
        published_from: d.published_from,
        approval_id: None,
        created_by: ctx.actor,
        created_at: now,
        published_at: release.then_some(now),
        expires_at: d.expires_at,
    })
}

/// Makes `row` the live deployment of its app (the old one `superseded`) and fixes the slug.
fn switch_live(st: &mut Inner, tenant: TenantId, row: &DeploymentRow, now: OffsetDateTime) {
    let Some(app) = st.apps.get_mut(&(tenant, row.app_node_id)) else {
        return;
    };
    let old = app.live_deployment_id.replace(row.id);
    app.slug_fixed_at.get_or_insert(now);
    app.updated_at = now;
    if let Some(old) = old
        && let Some(prev) = st.deployments.get_mut(&(tenant, old))
    {
        prev.state = DeploymentState::Superseded;
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
        expected: &AppSettings,
        settings: &AppSettings,
        data: Value,
    ) -> Result<Option<(AppRow, ChangeInfo)>> {
        // The compare, the write and the journal entry happen under one lock, so two updates
        // cannot write in one order and journal in the other; a failed append puts the old row
        // back (as the rollback of the Postgres transaction).
        let mut st = self.st();
        let row = st
            .apps
            .get_mut(&(ctx.tenant, node))
            .ok_or_else(|| Error::not_found("app"))?;
        if row.settings != *expected {
            return Ok(None);
        }
        let before = row.clone();
        row.settings = *settings;
        row.updated_at = crate::now();
        let row = row.clone();
        let change = match &self.journal {
            Some(j) => {
                let appended = j.append(
                    ctx,
                    NewEntry {
                        node_id: Some(node),
                        revision: None,
                        event: EventType::AppSettingsChanged,
                        data,
                    },
                );
                let e = match appended {
                    Ok(e) => e,
                    Err(err) => {
                        st.apps.insert((ctx.tenant, node), before);
                        return Err(err);
                    }
                };
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
        Ok(Some((row, change)))
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
        let now = crate::now();
        let tenant = ctx.tenant;
        let release = d.is_release().then(|| release_version(ctx, &d, None, now));
        let row = insert_row(&mut st, ctx, &d, release.as_ref().map(|(v, _)| v.id), now)?;
        if let Some((v, _)) = release {
            switch_live(&mut st, tenant, &row, now);
            st.versions
                .entry((tenant, d.app_node_id))
                .or_default()
                .push((v, row.id));
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

    async fn update_stages(&self, tenant: TenantId, id: Uuid, stages: &Value) -> Result<bool> {
        Ok(match self.st().deployments.get_mut(&(tenant, id)) {
            Some(d) if d.state.is_moving() => {
                d.state = DeploymentState::Running;
                d.stages = stages.clone();
                true
            }
            _ => false,
        })
    }

    async fn finish_preview(
        &self,
        ctx: &Context,
        id: Uuid,
        stages: &Value,
        check: Option<&Value>,
    ) -> Result<DeploymentState> {
        let mut st = self.st();
        let tenant = ctx.tenant;
        let Some(me) = st.deployments.get(&(tenant, id)).cloned() else {
            return Err(Error::not_found("deployment"));
        };
        if me.environment != Environment::Preview || !me.state.is_moving() {
            return Err(Error::not_found("deployment"));
        }
        let mut state = DeploymentState::Ready;
        if let Some(draft) = &me.draft {
            let same: Vec<Uuid> = st
                .deployments
                .values()
                .filter(|x| {
                    x.tenant_id == tenant
                        && x.app_node_id == me.app_node_id
                        && x.environment == Environment::Preview
                        && x.state == DeploymentState::Ready
                        && x.draft.as_deref() == Some(draft.as_str())
                        && x.id != id
                })
                .map(|x| x.id)
                .collect();
            for other in same {
                let o = st
                    .deployments
                    .get_mut(&(tenant, other))
                    .expect("listed above");
                if (o.created_at, o.id) < (me.created_at, me.id) {
                    o.state = DeploymentState::Superseded;
                } else {
                    state = DeploymentState::Superseded;
                }
            }
        }
        let row = st.deployments.get_mut(&(tenant, id)).expect("read above");
        row.state = state;
        row.stages = stages.clone();
        row.check_result = check.cloned();
        let node = row.app_node_id;
        if state == DeploymentState::Ready {
            self.journal(
                ctx,
                node,
                vec![(EventType::AppDeployed, preview_deployed_data(id))],
                crate::now(),
            )?;
        }
        Ok(state)
    }

    async fn fail_deployment(
        &self,
        ctx: &Context,
        id: Uuid,
        stages: &Value,
        check: Option<&Value>,
        stage: &str,
        reason: &str,
    ) -> Result<()> {
        let mut st = self.st();
        let row = st
            .deployments
            .get_mut(&(ctx.tenant, id))
            .ok_or_else(|| Error::not_found("deployment"))?;
        row.state = DeploymentState::Failed;
        row.stages = stages.clone();
        row.check_result = check.cloned();
        let (node, env) = (row.app_node_id, row.environment);
        self.journal(
            ctx,
            node,
            vec![(
                EventType::AppDeploymentFailed,
                deployment_failed_data(id, env, stage, reason),
            )],
            crate::now(),
        )?;
        Ok(())
    }

    async fn release(&self, ctx: &Context, d: NewDeployment, r: Release) -> Result<Released> {
        if !d.is_release() {
            return Err(Error::Internal(
                "release() of a deployment that is not live".into(),
            ));
        }
        let mut st = self.st();
        let tenant = ctx.tenant;
        let live = st
            .apps
            .get(&(tenant, d.app_node_id))
            .ok_or_else(|| Error::not_found("app"))?
            .live_deployment_id;
        if live != r.expected_live {
            return Err(live_changed());
        }
        let previous = live.map(|p| {
            let v = st.deployments.get(&(tenant, p)).and_then(|x| x.version_id);
            (p, v)
        });
        let now = crate::now();
        let (version, _) = release_version(ctx, &d, r.restored_from, now);
        let row = insert_row(&mut st, ctx, &d, Some(version.id), now)?;
        let entries = release_entries(&d, &version, previous, &r);
        // The journal first: a failed append leaves nothing behind (as the Postgres rollback).
        let change = self.journal(ctx, d.app_node_id, entries, now)?;
        switch_live(&mut st, tenant, &row, now);
        st.versions
            .entry((tenant, d.app_node_id))
            .or_default()
            .push((version.clone(), row.id));
        st.deployments.insert((tenant, row.id), row.clone());
        Ok(Released {
            deployment: row,
            version,
            change,
        })
    }

    async fn insert_approval(&self, ctx: &Context, a: NewApproval) -> Result<ApprovalRow> {
        let mut st = self.st();
        let tenant = ctx.tenant;
        if !st.deployments.contains_key(&(tenant, a.deployment_id)) {
            return Err(Error::not_found("deployment"));
        }
        let row = ApprovalRow {
            tenant_id: tenant,
            id: a.id,
            app_node_id: a.app_node_id,
            deployment_id: a.deployment_id,
            status: "pending".into(),
            changes: a.changes,
            bound_archive_hash: a.bound_archive_hash,
            bound_manifest_hash: a.bound_manifest_hash,
            bound_bindings: a.bound_bindings,
            requested_by: ctx.actor,
            decided_by: None,
            decided_at: None,
            comment: None,
            expires_at: a.expires_at,
            created_at: crate::now(),
        };
        st.approvals.insert((tenant, row.id), row.clone());
        Ok(row)
    }

    async fn list_versions(
        &self,
        tenant: TenantId,
        node: NodeId,
        filter: VersionFilter,
        page: PageRequest,
    ) -> Result<Page<Version>> {
        let after = page
            .cursor
            .as_ref()
            .map(crate::blobs::logic::decode_version_cursor)
            .transpose()?;
        let limit = page.limit() as usize;
        let mut all: Vec<Version> = self
            .st()
            .versions
            .get(&(tenant, node))
            .map(|vs| {
                vs.iter()
                    .map(|(v, _)| v)
                    .filter(|v| filter.matches(v))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        all.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
        let mut items: Vec<Version> = all
            .into_iter()
            .filter(|v| after.is_none_or(|(at, id)| (v.created_at, v.id) < (at, id)))
            .take(limit + 1)
            .collect();
        Ok(version_page(&mut items, limit))
    }

    async fn release_of(
        &self,
        tenant: TenantId,
        node: NodeId,
        version: VersionId,
    ) -> Result<Option<(Version, Uuid)>> {
        Ok(self.st().versions.get(&(tenant, node)).and_then(|vs| {
            vs.iter()
                .find(|(v, _)| v.id == version)
                .map(|(v, d)| (v.clone(), *d))
        }))
    }

    async fn expire_previews(
        &self,
        now: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<ExpiredPreview>> {
        let mut st = self.st();
        let mut out = Vec::new();
        for d in st.deployments.values_mut() {
            if out.len() >= limit as usize {
                break;
            }
            if d.environment == Environment::Preview
                && d.state == DeploymentState::Ready
                && d.expires_at.is_some_and(|t| t <= now)
            {
                d.state = DeploymentState::Expired;
                out.push(ExpiredPreview {
                    tenant_id: d.tenant_id,
                    app_node_id: d.app_node_id,
                    id: d.id,
                });
            }
        }
        Ok(out)
    }
}
