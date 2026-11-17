//! The storage quota and the garbage collection of static sites (backlog W2S-1).
//!
//! # Quota
//!
//! The objects of a tenant's static sites ([`super::static_site`]: file objects and indexes)
//! count against the tenant's storage quota, with its files (the plan's `storage_bytes`, per
//! member for a team). A deployment whose site needs more new bytes than the tenant has free
//! is refused: before the `202` of `createAppDeployment` (a direct `403 quota_exceeded`, no
//! deployment row), and again in the image stage, under the site lock, just before the objects
//! are stored (the stage fails with `quota_exceeded`; two deployments of one tenant that run at
//! the same time can each pass the first check). Content that the tenant stores already costs
//! nothing (objects are content-addressed).
//!
//! # Garbage collection
//!
//! [`AppService::collect_static`] runs on one tenant under its site lock, taken exclusively
//! (deployments take it shared while they write a site or copy an index). It keeps the index
//! of every deployment whose site may still be served or deployed again (the live deployment,
//! every earlier release, which a rollback deploys again, and previews that have not ended:
//! [`super::store::StaticDeployment::reachable`]), deletes the indexes of deployments that
//! ended for good (failed, expired and superseded previews) and of a purged app, deletes an
//! index that no deployment row names once it is older than the grace period (a publish writes
//! the index of its new deployment just before the row), and then deletes every file object
//! that no kept index names and that is older than the grace period.
//!
//! It runs from the preview sweeper of the server every [`STATIC_GC_EVERY`] for every tenant
//! with static objects ([`AppService::collect_static_all`]), and at the purge of an app
//! ([`StaticPurge`], a purge hook of the provider), which deletes the app's indexes at once.

use super::AppService;
use super::static_site::{IndexFate, SiteGcReport};
use super::store::{LiveSwitchLock, StorageUsage};
use crate::error::Result;
use crate::ids::{NodeId, TenantId};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::Duration;

/// How old an object without a reason to stay must be before the collection deletes it.
pub const STATIC_GC_GRACE: time::Duration = time::Duration::hours(1);

/// How often the server collects the static objects of every tenant.
pub const STATIC_GC_EVERY: Duration = Duration::from_secs(3600);

/// How long a deployment or a publish waits for the site lock of its tenant (the collection of
/// one tenant holds it), then `409 conflict`.
pub const SITE_LOCK_WAIT: Duration = Duration::from_secs(60);

/// How long the periodic collection waits for the site lock of one tenant before it leaves
/// that tenant for the next run.
const GC_LOCK_WAIT: Duration = Duration::from_secs(5);

/// The site locks of one process, one per tenant (only tenants with a lock held or awaited
/// keep an entry); the store's lock serializes the processes.
#[derive(Default)]
pub(crate) struct SiteLocks(std::sync::Mutex<HashMap<TenantId, Weak<tokio::sync::RwLock<()>>>>);

impl SiteLocks {
    fn of(&self, tenant: TenantId) -> Arc<tokio::sync::RwLock<()>> {
        let mut locks = self.0.lock().unwrap_or_else(|e| e.into_inner());
        locks.retain(|_, l| l.strong_count() > 0);
        if let Some(l) = locks.get(&tenant).and_then(Weak::upgrade) {
            return l;
        }
        let l = Arc::new(tokio::sync::RwLock::new(()));
        locks.insert(tenant, Arc::downgrade(&l));
        l
    }
}

/// The local part of a held site lock.
enum LocalGuard {
    Shared(#[allow(dead_code)] tokio::sync::OwnedRwLockReadGuard<()>),
    Exclusive(#[allow(dead_code)] tokio::sync::OwnedRwLockWriteGuard<()>),
}

/// A held site lock of one tenant: the lock of this process and the store's lock across
/// processes. Dropping it frees both.
pub(crate) struct SiteLock {
    _store: LiveSwitchLock,
    _local: LocalGuard,
}

impl AppService {
    /// Takes the site lock of `tenant`: shared for a writer of a site, exclusive for the
    /// collection. Waits at most `wait`, then `409 conflict`.
    pub(crate) async fn lock_sites(
        &self,
        tenant: TenantId,
        exclusive: bool,
        wait: Duration,
    ) -> Result<SiteLock> {
        let deadline = tokio::time::Instant::now() + wait;
        let lock = self.site_locks.of(tenant);
        let local = if exclusive {
            tokio::time::timeout_at(deadline, lock.write_owned())
                .await
                .map(LocalGuard::Exclusive)
        } else {
            tokio::time::timeout_at(deadline, lock.read_owned())
                .await
                .map(LocalGuard::Shared)
        }
        .map_err(|_| super::store::sites_busy())?;
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let store = self
            .deps
            .store
            .lock_static_sites(tenant, exclusive, left)
            .await?;
        Ok(SiteLock {
            _store: store,
            _local: local,
        })
    }

    /// The bytes of the tenant's storage quota that are free for new static objects: the
    /// plan's storage (per member for a team) less the bytes of its files and of its static
    /// objects. Never below zero.
    pub async fn static_free_bytes(&self, tenant: TenantId) -> Result<u64> {
        let plan = self.deps.tenants.find(tenant).await?.plan;
        let StorageUsage {
            file_bytes,
            members,
        } = self.deps.store.storage_usage(tenant).await?;
        let limit = self.storage_limit(plan, members);
        let stored = i64::try_from(self.deps.sites.stored_bytes(tenant).await?).unwrap_or(i64::MAX);
        let free = limit.saturating_sub(file_bytes).saturating_sub(stored);
        Ok(u64::try_from(free).unwrap_or(0))
    }

    /// The storage limit of a tenant on `plan` with `members` members.
    fn storage_limit(&self, plan: crate::tenants::Plan, members: u32) -> i64 {
        #[cfg(test)]
        if let Some(limit) = *self
            .storage_limit_override
            .lock()
            .unwrap_or_else(|e| e.into_inner())
        {
            return limit;
        }
        crate::tenants::limits::tenant_limits(plan, members).storage_bytes
    }

    /// Sets the storage limit of every tenant (the tests of the quota; `None`: the plan's).
    #[cfg(test)]
    pub(crate) fn set_storage_limit(&self, limit: Option<i64>) {
        *self
            .storage_limit_override
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = limit;
    }

    /// Refuses a site whose new objects (`contents`: [`super::static_site::site_contents`]) do
    /// not fit into the tenant's free storage (`403 quota_exceeded`).
    pub(crate) async fn check_static_quota(
        &self,
        tenant: TenantId,
        contents: &[(crate::blobs::Hash, u64)],
    ) -> Result<()> {
        let needed = self.deps.sites.missing_bytes(tenant, contents).await?;
        if needed == 0 {
            return Ok(());
        }
        let free = self.static_free_bytes(tenant).await?;
        if needed > free {
            return Err(super::static_site::quota_exceeded(needed, free));
        }
        Ok(())
    }

    /// The garbage collection of one tenant's static objects (see the module docs). `purged`:
    /// an app whose indexes go now, whatever its rows say (the purge hook runs before the rows
    /// go). Objects written in the last `grace` stay unless a row says they ended. Waits at
    /// most `wait` for the site lock.
    pub async fn collect_static(
        &self,
        tenant: TenantId,
        grace: time::Duration,
        purged: Option<NodeId>,
        wait: Duration,
    ) -> Result<SiteGcReport> {
        let _lock = self.lock_sites(tenant, true, wait).await?;
        // Read under the lock: a deployment that writes its site holds the lock shared, so its
        // row (inserted before its stages run) is in this list.
        let rows: HashMap<uuid::Uuid, (NodeId, bool)> = self
            .deps
            .store
            .static_deployments(tenant)
            .await?
            .into_iter()
            .map(|d| (d.id, (d.app_node_id, d.reachable)))
            .collect();
        let fate = move |id: uuid::Uuid| match rows.get(&id) {
            Some((app, _)) if Some(*app) == purged => IndexFate::Delete,
            Some((_, true)) => IndexFate::Keep,
            Some((_, false)) => IndexFate::Delete,
            None => IndexFate::DeleteIfOld,
        };
        let cutoff = crate::now() - grace;
        let report = self.deps.sites.collect(tenant, &fate, cutoff).await?;
        if report.indexes_deleted > 0 || report.files_deleted > 0 {
            tracing::info!(%tenant, indexes = report.indexes_deleted, files = report.files_deleted,
                bytes = report.bytes_freed, "collected static site objects");
        }
        Ok(report)
    }

    /// [`AppService::collect_static`] for every tenant with static objects. A tenant whose
    /// site lock stays taken, or whose collection fails, is left for the next run (logged).
    pub async fn collect_static_all(&self, grace: time::Duration) -> Result<SiteGcReport> {
        let mut total = SiteGcReport::default();
        for tenant in self.deps.sites.tenants().await? {
            match self.collect_static(tenant, grace, None, GC_LOCK_WAIT).await {
                Ok(r) => total += r,
                Err(e) => {
                    tracing::warn!(%tenant, error = %e, "the collection of static sites failed");
                }
            }
        }
        Ok(total)
    }

    /// The purge hook of static sites (register it with the provider's purge hooks).
    pub fn static_purge(self: &Arc<Self>) -> StaticPurge {
        StaticPurge {
            service: Arc::downgrade(self),
        }
    }
}

/// The purge of an app (`super::provider::PurgeHook`): deletes the indexes of all its
/// deployments and collects the tenant's static objects, so the file objects that only the app
/// used go (those written in the last [`STATIC_GC_GRACE`] go at the next run). An error keeps
/// the app rows, and the platform delivers the hook again.
pub struct StaticPurge {
    service: Weak<AppService>,
}

#[async_trait]
impl super::provider::PurgeHook for StaticPurge {
    async fn app_purged(&self, tenant: TenantId, app: NodeId) -> Result<()> {
        // The service owns the provider that holds this hook: it outlives every delivery.
        let Some(service) = self.service.upgrade() else {
            return Ok(());
        };
        service
            .collect_static(tenant, STATIC_GC_GRACE, Some(app), SITE_LOCK_WAIT)
            .await?;
        Ok(())
    }
}
