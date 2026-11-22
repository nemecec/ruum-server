//! The storage quota and the garbage collection of static sites (backlog W2S-1).
//!
//! # Quota
//!
//! The objects of a tenant's static sites ([`super::static_site`]: file objects and indexes)
//! count against the tenant's storage quota, with its files (the plan's `storage_bytes`, per
//! member for a team). A deployment whose site needs more new bytes than the tenant has free
//! is refused: before the `202` of `createAppDeployment` (a direct `403 quota_exceeded`, no
//! deployment row), and again in the image stage, under the site lock, just before the objects
//! are stored (the stage fails with `quota_exceeded`). Content that the tenant stores already
//! costs nothing (objects are content-addressed), but every deployment writes a new index,
//! which counts: a deployment needs the bytes of its index, and a publish or rollback, which
//! copies an index, is refused with `403 quota_exceeded` when the copy does not fit.
//!
//! Writers hold the site lock shared, so two deployments of one tenant that run at the same
//! time can each pass the check, and the quota can be exceeded by about one site per
//! concurrent writer of the tenant. The upload of files has the same bound (its check reads
//! the usage without a lock); a strict quota needs a recorded byte total (backlog W2S-3).
//!
//! # Garbage collection
//!
//! [`AppService::collect_static`] runs on one tenant under its site lock, taken exclusively
//! (deployments take it shared while they write a site or copy an index). It keeps the index
//! of every deployment whose site may still be served or deployed again (the live deployment,
//! every earlier release, which a rollback deploys again, and previews that have not ended:
//! [`super::store::StaticDeployment::reachable`]), deletes the indexes of deployments that
//! ended for good (failed, expired and superseded previews), deletes an index that no
//! deployment row names once it is older than the grace period (a publish writes the index of
//! its new deployment just before the row; what is left of a purged app), and then deletes every file object
//! that no kept index names and that is older than the grace period.
//!
//! No file is deleted while a deployment of the tenant that started in the last
//! [`WRITER_WINDOW`] is pending or running and has no index yet: a server of the previous
//! release (during a rolling upgrade) writes sites without the site lock, and may have found a
//! file that no index names and skipped its upload (Kimi W2S-1, 1). A deployment that stopped
//! with its process (it stays `pending` or `running`) holds the files back for that window
//! only.
//!
//! That guard sees only the rows that exist when the collection reads them. A server of the
//! previous release can insert its row after that read, or copy an index for a publish (no
//! row until the index is written), while the collection deletes files. So a server makes its
//! first collection only [`STATIC_GC_FIRST_DELAY`] after its start, when the rolling upgrade
//! that started it has ended and no server without the site lock writes sites any more (Kimi
//! run 12, W2S-1 finding 1). A rollout that takes longer than that delay must not run this
//! release next to a release before W2S-1.
//!
//! It runs in a task of its own that the preview sweeper of the server starts, first after
//! [`STATIC_GC_FIRST_DELAY`] and then every [`STATIC_GC_EVERY`], for every tenant with static
//! objects ([`AppService::collect_static_all`]). At the purge of an app, [`StaticPurge`] (a
//! purge hook of the provider) deletes the app's indexes, in more than one delivery when they
//! do not fit into [`PURGE_BUDGET`]; its files go at the next run. It pages through the app's
//! own deployment rows in ID order, deletes their indexes by key, and stores a cursor before
//! each `503`, so no delivery repeats the work of an earlier one and no other app's rows or
//! objects slow it down (backlog W2S-4).

use super::AppService;
use super::static_site::{IndexFate, SiteGcReport, SiteNeed};
use super::store::{LiveSwitchLock, StorageUsage};
use crate::error::{Error, ErrorCode, Result};
use crate::ids::{NodeId, TenantId};
use async_trait::async_trait;
use futures::{FutureExt, StreamExt};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::time::Duration;

/// How old an object without a reason to stay must be before the collection deletes it.
pub const STATIC_GC_GRACE: time::Duration = time::Duration::hours(1);

/// How often the server collects the static objects of every tenant.
pub const STATIC_GC_EVERY: Duration = Duration::from_secs(3600);

/// How long a server waits after its start before its first collection: longer than a rolling
/// upgrade, so that no server of a release without the site lock still writes sites when it
/// runs (Kimi run 12, W2S-1 finding 1).
pub const STATIC_GC_FIRST_DELAY: Duration = STATIC_GC_EVERY;

/// How long the purge hook of static sites works in one delivery before it answers
/// `503 unavailable` and goes on at the next one (below `RegistryDeliver::TIMEOUT`, 30 s).
pub const PURGE_BUDGET: Duration = Duration::from_secs(20);

/// A pending or running deployment younger than this may still write its site: while one has
/// no index, the collection deletes no file of its tenant.
pub const WRITER_WINDOW: time::Duration = time::Duration::hours(1);

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

    /// Refuses a site whose new objects (its file objects that do not exist yet and its index:
    /// `need`) do not fit into the tenant's free storage (`403 quota_exceeded`). When the
    /// whole site fits, no file object is looked up.
    pub(crate) async fn check_static_quota(&self, tenant: TenantId, need: &SiteNeed) -> Result<()> {
        let free = self.static_free_bytes(tenant).await?;
        if need.total_bytes() <= free {
            return Ok(());
        }
        let needed = self
            .deps
            .sites
            .missing_bytes(tenant, &need.contents)
            .await?
            .saturating_add(need.index_bytes);
        if needed > free {
            return Err(super::static_site::quota_exceeded(needed));
        }
        Ok(())
    }

    /// The garbage collection of one tenant's static objects (see the module docs). Objects
    /// written in the last `grace` stay unless a row says they ended. Waits at most `wait` for
    /// the site lock.
    pub async fn collect_static(
        &self,
        tenant: TenantId,
        grace: time::Duration,
        wait: Duration,
    ) -> Result<SiteGcReport> {
        let _lock = self.lock_sites(tenant, true, wait).await?;
        // Read under the lock: a deployment that writes its site holds the lock shared, so its
        // row (inserted before its stages run) is in this list.
        let now = crate::now();
        let deployments = self.deps.store.static_deployments(tenant).await?;
        let in_flight: HashSet<uuid::Uuid> = deployments
            .iter()
            .filter(|d| d.in_flight_since.is_some_and(|t| t > now - WRITER_WINDOW))
            .map(|d| d.id)
            .collect();
        let rows: HashMap<uuid::Uuid, bool> = deployments
            .into_iter()
            .map(|d| (d.id, d.reachable))
            .collect();
        let fate = move |id: uuid::Uuid| match rows.get(&id) {
            Some(true) => IndexFate::Keep,
            Some(false) => IndexFate::Delete,
            None => IndexFate::DeleteIfOld,
        };
        let report = self
            .deps
            .sites
            .collect(tenant, &fate, &in_flight, now - grace)
            .await?;
        if report.indexes_deleted > 0 || report.files_deleted > 0 {
            tracing::info!(%tenant, indexes = report.indexes_deleted, files = report.files_deleted,
                bytes = report.bytes_freed, "collected static site objects");
        }
        Ok(report)
    }

    /// One pass of the move of static sites out of the tenant blob bucket `legacy` into the
    /// bucket of static sites (decision 0008-4; Kimi reviews of k14r3): each tenant's objects
    /// move under its site lock, shared like a writer of a site, so the collection (exclusive)
    /// never runs in the middle of a tenant's pass and never deletes a file whose index is
    /// still on its way (`StaticSites::migrate_tenant`). At the end the writes of new sites open
    /// for every tenant except those whose pass failed or whose lock stayed taken
    /// (`StaticSites::end_move_pass`); the pass then answers an error, and the next pass tries
    /// again.
    pub async fn move_static_sites(
        &self,
        legacy: Arc<dyn object_store::ObjectStore>,
        delete_after: Duration,
    ) -> Result<super::static_site::SiteMigration> {
        let sites = &self.deps.sites;
        let mut total = super::static_site::SiteMigration::default();
        let mut failed = std::collections::HashSet::new();
        for (tenant, metas) in sites.list_moving(legacy.as_ref()).await? {
            let pass = async {
                let _lock = self.lock_sites(tenant, false, SITE_LOCK_WAIT).await?;
                sites
                    .migrate_tenant(legacy.clone(), &metas, delete_after)
                    .await
            };
            match pass.await {
                Ok(r) => total += r,
                Err(e) => {
                    failed.insert(tenant);
                    tracing::warn!(%tenant, error = %e, "the move of a tenant's static sites failed");
                }
            }
        }
        let count = failed.len();
        sites.end_move_pass(failed);
        if count > 0 {
            return Err(Error::Internal(format!(
                "the move of static sites failed for {count} tenants; their writes stay closed and \
                 the next pass tries again"
            )));
        }
        Ok(total)
    }

    /// [`AppService::move_static_sites`] out of the blob bucket of `blob`, with
    /// `static_site::MOVE_DELETE_AFTER`.
    pub async fn move_static_sites_from_blob_bucket(
        &self,
        blob: &crate::blobs::object::S3Settings,
    ) -> Result<super::static_site::SiteMigration> {
        self.move_static_sites(
            Arc::new(blob.build()?),
            super::static_site::MOVE_DELETE_AFTER,
        )
        .await
    }

    /// [`AppService::collect_static`] for every tenant with static objects. A tenant whose
    /// site lock stays taken, or whose collection fails, is left for the next run (logged).
    pub async fn collect_static_all(&self, grace: time::Duration) -> Result<SiteGcReport> {
        let mut total = SiteGcReport::default();
        for tenant in self.deps.sites.tenants().await? {
            match self.collect_static(tenant, grace, GC_LOCK_WAIT).await {
                Ok(r) => total += r,
                Err(e) => {
                    tracing::warn!(%tenant, error = %e, "the collection of static sites failed");
                }
            }
        }
        Ok(total)
    }

    /// Runs [`AppService::collect_static_all`] after [`STATIC_GC_FIRST_DELAY`] and then every
    /// [`STATIC_GC_EVERY`] (the server: [`AppService::run_sweeper`] starts it in a task of its
    /// own, so a long collection never holds up the expiry of previews). The first run waits
    /// for the end of a rolling upgrade (see the module docs).
    pub async fn run_static_gc(self: Arc<Self>) {
        self.run_static_gc_after(STATIC_GC_FIRST_DELAY).await
    }

    /// [`AppService::run_static_gc`] with the first run after `first`.
    pub(crate) async fn run_static_gc_after(self: Arc<Self>, first: Duration) {
        tokio::time::sleep(first).await;
        loop {
            if let Err(e) = self.collect_static_all(STATIC_GC_GRACE).await {
                tracing::warn!(error = %e, "the collection of static sites failed");
            }
            tokio::time::sleep(STATIC_GC_EVERY).await;
        }
    }

    /// Deletes the indexes of every deployment of `app` (the purge of the app). Takes no site
    /// lock: no deployment of a purged app writes, and deleting an index never frees a file
    /// under a writer. The file objects go at the next collection.
    ///
    /// It stops after [`PURGE_BUDGET`], below the delivery timeout of the hook, and then
    /// answers `503 unavailable`: the platform delivers the hook again, and the next call goes
    /// on where this one stopped, so a purge of any number of deployments ends (Kimi run 12,
    /// W2S-1 finding 2; backlog W2S-4).
    pub async fn delete_app_sites(&self, tenant: TenantId, app: NodeId) -> Result<u64> {
        self.delete_app_sites_within(tenant, app, PURGE_BUDGET)
            .await
    }

    /// The page size of the purge of an app's sites.
    fn purge_page(&self) -> u32 {
        #[cfg(test)]
        if let Some(page) = *self
            .purge_page_override
            .lock()
            .unwrap_or_else(|e| e.into_inner())
        {
            return page;
        }
        super::static_site::DELETE_BATCH as u32
    }

    /// Sets the page size of the purge of an app's sites (tests; `None`: the default).
    #[cfg(test)]
    pub(crate) fn set_purge_page(&self, page: Option<u32>) {
        *self
            .purge_page_override
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = page;
    }

    /// [`AppService::delete_app_sites`] with a time budget of `budget`.
    ///
    /// The work is app-scoped and keyset-paged (backlog W2S-4): it reads one page of the app's
    /// deployment IDs in ID order after the stored cursor ([`AppStore::static_purge_cursor`]),
    /// deletes their indexes by key (no listing of the tenant's objects), and stores the last
    /// ID whose index is gone and before which every index is gone. It stores that cursor
    /// before every wait on the object store, and so before it answers `503`; a delivery that
    /// is cut from outside loses at most the deletes it was waiting for. Neither the
    /// deployments of other apps nor the pages already done cost a later delivery anything.
    ///
    /// The deletes of a page go in requests that start small: the first request of a delivery
    /// holds one key, and each finished request doubles the next one, up to [`DELETE_BATCH`].
    /// A bulk request answers only when all its keys are done (S3 `DeleteObjects`), so one
    /// request of a whole page that takes longer than a delivery would never move the cursor.
    /// With the small first requests, every delivery in which one delete fits stores progress
    /// (Kimi review of W2S-4, finding 1).
    ///
    /// [`AppStore::static_purge_cursor`]: super::store::AppStore::static_purge_cursor
    /// [`DELETE_BATCH`]: super::static_site::DELETE_BATCH
    pub(crate) async fn delete_app_sites_within(
        &self,
        tenant: TenantId,
        app: NodeId,
        budget: Duration,
    ) -> Result<u64> {
        let deadline = tokio::time::Instant::now() + budget;
        let store = &self.deps.store;
        let unfinished = |deleted: u64| {
            tracing::info!(%tenant, %app, indexes = deleted,
                "the purge of an app's site indexes continues at the next delivery");
            Error::api(
                ErrorCode::Unavailable,
                "the purge of the app's sites is not finished; it continues at the next delivery",
            )
        };
        let page = self.purge_page();
        let mut deleted = 0u64;
        // The number of keys of the next delete request.
        let mut chunk = 1;
        let mut after = tokio::time::timeout_at(deadline, store.static_purge_cursor(tenant, app))
            .await
            .map_err(|_| unfinished(deleted))??;
        loop {
            let ids = tokio::time::timeout_at(
                deadline,
                store.app_deployment_ids_after(tenant, app, after, page),
            )
            .await
            .map_err(|_| unfinished(deleted))??;
            let Some(&last) = ids.last() else {
                return Ok(deleted);
            };
            // `ids` is in ID order: `done` is the count of its first IDs whose indexes are all
            // gone, `saved` the count the stored cursor covers.
            let position: HashMap<uuid::Uuid, usize> =
                ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
            let mut gone = vec![false; ids.len()];
            let (mut done, mut saved) = (0usize, 0usize);
            let mut start = 0;
            while start < ids.len() {
                let end = (start + chunk).min(ids.len());
                let mut s = self
                    .deps
                    .sites
                    .delete_indexes(tenant, ids[start..end].to_vec());
                loop {
                    // `now_or_never` polls once with a no-op waker. When that poll is pending,
                    // the `timeout_at` below polls the stream again with the real waker, so no
                    // wake-up is lost.
                    let next = match s.next().now_or_never() {
                        Some(next) => next,
                        None => {
                            // About to wait: store the progress first.
                            if done > saved {
                                tokio::time::timeout_at(
                                    deadline,
                                    store.set_static_purge_cursor(tenant, app, ids[done - 1]),
                                )
                                .await
                                .map_err(|_| unfinished(deleted))??;
                                saved = done;
                            }
                            tokio::time::timeout_at(deadline, s.next())
                                .await
                                .map_err(|_| unfinished(deleted))?
                        }
                    };
                    let Some(r) = next else { break };
                    let (id, removed) = r?;
                    deleted += u64::from(removed);
                    if let Some(&i) = position.get(&id) {
                        gone[i] = true;
                    }
                    while done < gone.len() && gone[done] {
                        done += 1;
                    }
                }
                start = end;
                chunk = (chunk * 2).min(super::static_site::DELETE_BATCH);
            }
            if done < ids.len() {
                return Err(Error::Internal(
                    "the object store did not answer for every site index of a purge".into(),
                ));
            }
            if saved < done {
                tokio::time::timeout_at(deadline, store.set_static_purge_cursor(tenant, app, last))
                    .await
                    .map_err(|_| unfinished(deleted))??;
            }
            after = Some(last);
        }
    }

    /// The purge hook of static sites (register it with the provider's purge hooks).
    pub fn static_purge(self: &Arc<Self>) -> StaticPurge {
        StaticPurge {
            service: Arc::downgrade(self),
        }
    }
}

/// The purge of an app (`super::provider::PurgeHook`): deletes the indexes of all its
/// deployments, so the next collection frees the file objects that only the app used. It does
/// not run the collection itself: that waits for the site lock and lists every object of the
/// tenant, which can take longer than the delivery timeout of the hook (Kimi W2S-1, 7). An
/// index that the app wrote without a row (a publish that failed before its release) goes at a
/// collection after the grace period. An error keeps the app rows, and the platform delivers
/// the hook again.
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
        let n = service.delete_app_sites(tenant, app).await?;
        tracing::debug!(%tenant, %app, indexes = n, "deleted the site indexes of a purged app");
        Ok(())
    }
}
