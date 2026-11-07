//! The platform side of typed nodes: the `typed_nodes` rows (the state of the control loop,
//! section 8.1) and the hook delivery queue (`provider_deliveries`, section 7.1).
//!
//! [`TypedNodeStore`] is the border. [`MemoryTypedNodeStore`] implements it in memory, and
//! [`super::postgres::PgTypedNodeStore`] on the tables of `migrations/0004_databases.sql`. One
//! suite (the `tests` module of this file) runs on both.
//!
//! Rows are made in the caller's transaction, not through the trait: a typed node, its first
//! manifest version and its `typed_nodes` row are one unit (a deferred trigger checks it), and a
//! delivery is written with the journal entry it comes from. So each store offers its own
//! functions for that ([`MemoryTypedNodeStore::insert`], [`MemoryTypedNodeStore::enqueue`],
//! [`super::postgres::insert_typed_node`], [`super::postgres::enqueue`]). The trait holds what
//! the platform does with the rows afterwards:
//!
//! - [`TypedNodeStore::report`]: a status report updates the row only when its generation is
//!   greater than the stored one, in one step. An older or equal one is ignored.
//! - [`TypedNodeStore::reconcile_candidates`]: the rows that the reconciler
//!   ([`super::reconciler`]) must look at.
//! - [`TypedNodeStore::deliver_next`]: takes the oldest due delivery that is first in its
//!   queue, hands it to a [`Deliver`] while it holds it, and records the outcome. Each node has
//!   its own queue in `seq` order; the tenant hooks of a tenant have one queue of their own, in
//!   the order they were written. A delivery that waits for a retry blocks the later ones of its
//!   queue. Two workers never take the same delivery, and never two of one queue at a time.
//!
//! Every row is keyed by tenant: a node or a delivery ID of one tenant gives nothing in another.

use super::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ------------------------------------------------------------------------------- typed nodes

/// A new `typed_nodes` row. The node and its first manifest version are written with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewTypedNode {
    pub tenant_id: TenantId,
    pub node_id: NodeId,
    /// The provider ID, for example `ruum.database`.
    pub provider: String,
    /// The node type, for example `database`.
    pub type_name: String,
    /// The first manifest version.
    pub desired_version: VersionId,
}

impl NewTypedNode {
    /// The checks of the table: the patterns of the provider ID and the type.
    pub fn check(&self) -> Result<()> {
        if !name_ok(&self.provider, true) {
            return Err(bad_request(format!(
                "provider id {:?} does not match ^[a-z][a-z0-9_.-]{{1,62}}$",
                self.provider
            )));
        }
        if !name_ok(&self.type_name, false) {
            return Err(bad_request(format!(
                "type {:?} does not match ^[a-z][a-z0-9_.]{{1,62}}$",
                self.type_name
            )));
        }
        Ok(())
    }
}

fn name_ok(s: &str, dash: bool) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && (2..=63).contains(&s.len())
        && chars.all(|c| {
            c.is_ascii_lowercase()
                || c.is_ascii_digit()
                || c == '_'
                || c == '.'
                || (dash && c == '-')
        })
}

/// A `typed_nodes` row.
#[derive(Clone, Debug, PartialEq)]
pub struct TypedNodeRow {
    pub tenant_id: TenantId,
    pub node_id: NodeId,
    pub provider: String,
    pub type_name: String,
    /// The newest manifest version.
    pub desired_version: VersionId,
    /// The manifest version that the content matches, as the provider reported it.
    pub observed_version: Option<VersionId>,
    pub state: ProviderState,
    pub reason: Option<String>,
    pub message: Option<String>,
    pub detail: serde_json::Map<String, Value>,
    /// The generation of the stored report; 0 before the first report.
    pub generation: i64,
    /// When the stored state was written.
    pub state_at: OffsetDateTime,
    /// The provider's own small state (not content).
    pub provider_state: Value,
}

impl TypedNodeRow {
    /// True when the reconciler must look at the row: the state is `provisioning`,
    /// `updating` or `degraded`, or the observed version is behind the desired one. A row in
    /// `failed` or `deleted` waits for a new manifest version or a person (section 8.1: the
    /// control loop stops when the provider reports `failed`).
    pub fn needs_reconcile(&self) -> bool {
        let busy = matches!(
            self.state,
            ProviderState::Provisioning | ProviderState::Updating | ProviderState::Degraded
        );
        let behind = self.observed_version != Some(self.desired_version);
        (busy || behind) && !matches!(self.state, ProviderState::Failed | ProviderState::Deleted)
    }
}

/// What [`TypedNodeStore::report`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportOutcome {
    /// The report was newer than the stored one and is stored now.
    Stored,
    /// The report was not newer than the stored one; nothing changed (`409` in `reportStatus`).
    Ignored,
}

/// A row for the reconciler, with the manifest of its desired version and the principal pair
/// that wrote that version.
#[derive(Clone, Debug, PartialEq)]
pub struct ReconcileItem {
    pub row: TypedNodeRow,
    pub manifest: Value,
    pub by: ActorPair,
}

/// The checks of a status before it is stored: `reason` is snake_case and at most 63
/// characters, `message` at most 1000 characters (the checks of the table).
pub fn check_status(status: &ProviderStatus) -> Result<()> {
    if let Some(r) = &status.reason {
        let mut chars = r.chars();
        let ok = matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
            && r.len() <= 63
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !ok {
            return Err(bad_request(
                "reason is a snake_case word of at most 63 characters",
            ));
        }
    }
    if status
        .message
        .as_ref()
        .is_some_and(|m| m.chars().count() > 1000)
    {
        return Err(bad_request("message has at most 1000 characters"));
    }
    Ok(())
}

/// The name of a state in the database (`provider_state`).
pub(crate) fn state_name(s: ProviderState) -> &'static str {
    match s {
        ProviderState::Provisioning => "provisioning",
        ProviderState::Ready => "ready",
        ProviderState::Updating => "updating",
        ProviderState::Degraded => "degraded",
        ProviderState::Suspended => "suspended",
        ProviderState::Failed => "failed",
        ProviderState::Deleted => "deleted",
    }
}

/// The state of a database name.
pub(crate) fn parse_state(s: &str) -> Result<ProviderState> {
    Ok(match s {
        "provisioning" => ProviderState::Provisioning,
        "ready" => ProviderState::Ready,
        "updating" => ProviderState::Updating,
        "degraded" => ProviderState::Degraded,
        "suspended" => ProviderState::Suspended,
        "failed" => ProviderState::Failed,
        "deleted" => ProviderState::Deleted,
        other => return Err(Error::Internal(format!("unknown provider state {other}"))),
    })
}

/// The name of a hook in the database (`provider_hook`).
pub(crate) fn hook_name(h: HookKind) -> &'static str {
    match h {
        HookKind::Copied => "copied",
        HookKind::Moved => "moved",
        HookKind::Renamed => "renamed",
        HookKind::Trashed => "trashed",
        HookKind::Restored => "restored",
        HookKind::Purged => "purged",
        HookKind::AccessChanged => "access_changed",
        HookKind::TenantExported => "tenant_exported",
        HookKind::TenantDeleted => "tenant_deleted",
    }
}

// -------------------------------------------------------------------------------- deliveries

/// A new delivery of a lifecycle hook. The tenant, the node, `seq` and the target come from
/// the hook itself; the queue keeps the hook as the payload.
#[derive(Clone, Debug, PartialEq)]
pub struct NewDelivery {
    /// The `Ruum-Delivery-Id`. Unique per tenant: a second delivery with the same ID in the
    /// same tenant is the same delivery and is not written again.
    pub delivery_id: DeliveryId,
    /// The provider of the node's type.
    pub provider: String,
    pub hook: HookDelivery,
}

impl NewDelivery {
    /// The checks of the table: a tenant hook has no node and no `seq`, a node hook has both;
    /// only `copied` has a target.
    pub fn check(&self) -> Result<()> {
        let h = &self.hook;
        if h.hook.is_tenant_hook() {
            if h.node_id.is_some() || h.seq.is_some() {
                return Err(bad_request("a tenant hook has no node_id and no seq"));
            }
        } else if h.node_id.is_none() || h.seq.is_none() {
            return Err(bad_request("a node hook needs node_id and seq"));
        }
        if h.target_node_id.is_some() != (h.hook == HookKind::Copied) {
            return Err(bad_request(
                "only copied has a target_node_id, and it needs one",
            ));
        }
        Ok(())
    }

    /// The payload as it is stored.
    pub(crate) fn payload(&self) -> Result<Value> {
        serde_json::to_value(&self.hook).map_err(|e| Error::Internal(e.to_string()))
    }
}

/// A delivery that a worker holds.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingDelivery {
    pub tenant_id: TenantId,
    pub delivery_id: DeliveryId,
    pub provider: String,
    pub hook: HookDelivery,
    /// The failed attempts before this one.
    pub attempts: i32,
}

/// The outcome of one attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// The provider took the hook (`200` or `202`).
    Delivered,
    /// The attempt failed (`5xx`, `408`, `429`, no answer); try again after `after`.
    /// `error` is a status and a code, never a value of the content.
    Retry { error: String, after: Duration },
    /// The provider refused the hook for good (a `4xx` other than `408` and `429`). The
    /// delivery ends (`delivered_at` is set) and `error` says why, so the queue goes on.
    Refused { error: String },
}

/// The state of one delivery, for tests and operators.
#[derive(Clone, Debug, PartialEq)]
pub struct DeliveryState {
    pub tenant_id: TenantId,
    pub delivery_id: DeliveryId,
    pub provider: String,
    pub hook: HookKind,
    pub node_id: Option<NodeId>,
    pub target_node_id: Option<NodeId>,
    pub seq: Option<i64>,
    pub attempts: i32,
    pub next_attempt_at: OffsetDateTime,
    /// The end of the delivery: delivered, or refused for good (then `last_error` is set).
    pub delivered_at: Option<OffsetDateTime>,
    pub last_error: Option<String>,
}

/// Hands one delivery to its provider. [`super::deliveries::RegistryDeliver`] is the real one.
#[async_trait]
pub trait Deliver: Send + Sync {
    async fn deliver(&self, d: &PendingDelivery) -> DeliveryOutcome;
}

/// What `last_error` holds for an error: the status and the code, never the message (a
/// provider's message may quote values).
pub fn error_text(e: &Error) -> String {
    format!("{} {}", e.http_status(), e.to_api().code.as_str())
}

// ------------------------------------------------------------------------------------ trait

/// The platform side of typed nodes. Every method that names a tenant reads and writes only
/// that tenant's rows; the two scans of the background tasks
/// ([`TypedNodeStore::reconcile_candidates`], [`TypedNodeStore::deliver_next`]) read every
/// tenant and hand each row on with its own tenant.
#[async_trait]
pub trait TypedNodeStore: Send + Sync {
    /// The row of a typed node.
    async fn get(&self, tenant: TenantId, node: NodeId) -> Result<Option<TypedNodeRow>>;

    /// Stores a status report when its generation is greater than the stored one. A node
    /// without a row in this tenant is `not_found`.
    async fn report(&self, tenant: TenantId, status: &ProviderStatus) -> Result<ReportOutcome>;

    /// The rows that [`TypedNodeRow::needs_reconcile`] picks and whose state is older than
    /// `settled`, oldest state first, at most `limit`, from every tenant.
    async fn reconcile_candidates(
        &self,
        settled: Duration,
        limit: usize,
    ) -> Result<Vec<ReconcileItem>>;

    /// The state of one delivery of this tenant.
    async fn delivery(
        &self,
        tenant: TenantId,
        delivery: DeliveryId,
    ) -> Result<Option<DeliveryState>>;

    /// Takes the oldest due delivery that is first in its queue (from any tenant), calls
    /// `deliver` while it holds it, and records the outcome. `None` when nothing is due.
    async fn deliver_next(
        &self,
        deliver: &dyn Deliver,
    ) -> Result<Option<(PendingDelivery, DeliveryOutcome)>>;

    /// Makes a waiting delivery due now (an operator's "retry now"). False when the delivery
    /// does not exist in this tenant or has ended.
    async fn retry_now(&self, tenant: TenantId, delivery: DeliveryId) -> Result<bool>;
}

// ----------------------------------------------------------------------------- the platform

/// The [`PlatformApi`] of an in-process provider: status reports go into the store
/// (`reportStatus`: `409` for an older generation, `404` for an unknown node); the check API
/// and uploads go to `inner`.
pub struct StoreReports {
    store: Arc<dyn TypedNodeStore>,
    inner: Arc<dyn PlatformApi>,
}

impl StoreReports {
    pub fn new(store: Arc<dyn TypedNodeStore>, inner: Arc<dyn PlatformApi>) -> Self {
        Self { store, inner }
    }
}

#[async_trait]
impl PlatformApi for StoreReports {
    async fn report_status(&self, report: StatusReport) -> Result<()> {
        match self.store.report(report.tenant_id, &report.status).await? {
            ReportOutcome::Stored => Ok(()),
            ReportOutcome::Ignored => Err(conflict(format!(
                "generation {} is not newer than the stored one",
                report.status.generation
            ))),
        }
    }

    async fn check_access(&self, req: CheckRequest) -> Result<CheckResult> {
        self.inner.check_access(req).await
    }

    async fn write_upload(
        &self,
        tenant: TenantId,
        upload: &UploadTarget,
        bytes: Vec<u8>,
    ) -> Result<NodeId> {
        self.inner.write_upload(tenant, upload, bytes).await
    }
}

// ------------------------------------------------------------------------------- the memory

/// The in-memory store. It also stands in for the manifest versions (`versions` rows) that the
/// reconciler reads.
#[derive(Default)]
pub struct MemoryTypedNodeStore {
    inner: Mutex<MemInner>,
}

#[derive(Default)]
struct MemInner {
    rows: HashMap<(TenantId, NodeId), TypedNodeRow>,
    /// (tenant, version) -> (node, manifest, author)
    versions: HashMap<(TenantId, VersionId), (NodeId, Value, ActorPair)>,
    deliveries: BTreeMap<(TenantId, DeliveryId), MemDelivery>,
    /// Deliveries that a worker holds (the row lock of Postgres).
    held: HashSet<(TenantId, DeliveryId)>,
    /// The write order of deliveries (`created_at`).
    written: u64,
}

#[derive(Clone)]
struct MemDelivery {
    provider: String,
    hook: HookDelivery,
    attempts: i32,
    next_attempt_at: OffsetDateTime,
    delivered_at: Option<OffsetDateTime>,
    last_error: Option<String>,
    written: u64,
}

impl MemDelivery {
    /// The queue of the delivery: its node, or the tenant hooks (`None`).
    fn queue(&self) -> Option<NodeId> {
        self.hook.node_id
    }

    /// The order within the queue: `seq` for a node, the delivery ID (a UUIDv7) for the tenant
    /// hooks.
    fn order(&self, id: DeliveryId) -> (i64, DeliveryId) {
        (self.hook.seq.unwrap_or(0), id)
    }

    fn state(&self, tenant: TenantId, id: DeliveryId) -> DeliveryState {
        DeliveryState {
            tenant_id: tenant,
            delivery_id: id,
            provider: self.provider.clone(),
            hook: self.hook.hook,
            node_id: self.hook.node_id,
            target_node_id: self.hook.target_node_id,
            seq: self.hook.seq,
            attempts: self.attempts,
            next_attempt_at: self.next_attempt_at,
            delivered_at: self.delivered_at,
            last_error: self.last_error.clone(),
        }
    }
}

impl MemoryTypedNodeStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MemInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Writes a typed node's row and its first manifest version (`manifest`, written by `by`).
    /// `conflict` when the node has a row.
    pub fn insert(
        &self,
        new: NewTypedNode,
        manifest: Value,
        by: ActorPair,
    ) -> Result<TypedNodeRow> {
        new.check()?;
        let mut inner = self.lock();
        let key = (new.tenant_id, new.node_id);
        if inner.rows.contains_key(&key) {
            return Err(conflict(format!("typed node {} has a row", new.node_id)));
        }
        let now = crate::now();
        let row = TypedNodeRow {
            tenant_id: new.tenant_id,
            node_id: new.node_id,
            provider: new.provider,
            type_name: new.type_name,
            desired_version: new.desired_version,
            observed_version: None,
            state: ProviderState::Provisioning,
            reason: None,
            message: None,
            detail: serde_json::Map::new(),
            generation: 0,
            state_at: now,
            provider_state: Value::Object(Default::default()),
        };
        inner.versions.insert(
            (new.tenant_id, new.desired_version),
            (new.node_id, manifest, by),
        );
        inner.rows.insert(key, row.clone());
        Ok(row)
    }

    /// Writes a new manifest version of a typed node and makes it the desired one.
    pub fn set_desired(
        &self,
        tenant: TenantId,
        node: NodeId,
        version: VersionId,
        manifest: Value,
        by: ActorPair,
    ) -> Result<()> {
        let mut inner = self.lock();
        let row = inner
            .rows
            .get_mut(&(tenant, node))
            .ok_or_else(|| node_not_found(node))?;
        row.desired_version = version;
        inner
            .versions
            .insert((tenant, version), (node, manifest, by));
        Ok(())
    }

    /// Writes a delivery. False when the tenant has a delivery with this ID already (nothing
    /// changes).
    pub fn enqueue(&self, d: NewDelivery) -> Result<bool> {
        d.check()?;
        let mut inner = self.lock();
        let key = (d.hook.tenant_id, d.delivery_id);
        if inner.deliveries.contains_key(&key) {
            return Ok(false);
        }
        inner.written += 1;
        let written = inner.written;
        inner.deliveries.insert(
            key,
            MemDelivery {
                provider: d.provider,
                hook: d.hook,
                attempts: 0,
                next_attempt_at: crate::now(),
                delivered_at: None,
                last_error: None,
                written,
            },
        );
        Ok(true)
    }
}

#[async_trait]
impl TypedNodeStore for MemoryTypedNodeStore {
    async fn get(&self, tenant: TenantId, node: NodeId) -> Result<Option<TypedNodeRow>> {
        Ok(self.lock().rows.get(&(tenant, node)).cloned())
    }

    async fn report(&self, tenant: TenantId, status: &ProviderStatus) -> Result<ReportOutcome> {
        check_status(status)?;
        let mut inner = self.lock();
        let row = inner
            .rows
            .get_mut(&(tenant, status.node_id))
            .ok_or_else(|| node_not_found(status.node_id))?;
        if status.generation <= row.generation {
            return Ok(ReportOutcome::Ignored);
        }
        row.state = status.state;
        row.reason = status.reason.clone();
        row.message = status.message.clone();
        row.detail = status.detail.clone().unwrap_or_default();
        row.generation = status.generation;
        row.observed_version = status.observed_version_id;
        row.state_at = crate::now();
        Ok(ReportOutcome::Stored)
    }

    async fn reconcile_candidates(
        &self,
        settled: Duration,
        limit: usize,
    ) -> Result<Vec<ReconcileItem>> {
        let inner = self.lock();
        let before = crate::now() - settled;
        let mut rows: Vec<&TypedNodeRow> = inner
            .rows
            .values()
            .filter(|r| r.needs_reconcile() && r.state_at <= before)
            .collect();
        rows.sort_by_key(|r| (r.state_at, r.tenant_id, r.node_id));
        rows.into_iter()
            .take(limit)
            .map(|r| {
                let (_, manifest, by) = inner
                    .versions
                    .get(&(r.tenant_id, r.desired_version))
                    .ok_or_else(|| Error::Internal("the desired version is missing".into()))?;
                Ok(ReconcileItem {
                    row: r.clone(),
                    manifest: manifest.clone(),
                    by: *by,
                })
            })
            .collect()
    }

    async fn delivery(
        &self,
        tenant: TenantId,
        delivery: DeliveryId,
    ) -> Result<Option<DeliveryState>> {
        Ok(self
            .lock()
            .deliveries
            .get(&(tenant, delivery))
            .map(|d| d.state(tenant, delivery)))
    }

    async fn deliver_next(
        &self,
        deliver: &dyn Deliver,
    ) -> Result<Option<(PendingDelivery, DeliveryOutcome)>> {
        let pending = {
            let mut inner = self.lock();
            let now = crate::now();
            let open: Vec<(&(TenantId, DeliveryId), &MemDelivery)> = inner
                .deliveries
                .iter()
                .filter(|(_, d)| d.delivered_at.is_none())
                .collect();
            let first_in_queue = |key: &(TenantId, DeliveryId), d: &MemDelivery| {
                !open.iter().any(|(k, e)| {
                    k.0 == key.0 && e.queue() == d.queue() && e.order(k.1) < d.order(key.1)
                })
            };
            let pick = open
                .iter()
                .filter(|(k, d)| {
                    d.next_attempt_at <= now && !inner.held.contains(k) && first_in_queue(k, d)
                })
                .min_by_key(|(k, d)| (d.next_attempt_at, d.written, k.1))
                .map(|(k, d)| (**k, (*d).clone()));
            let Some((key, d)) = pick else {
                return Ok(None);
            };
            inner.held.insert(key);
            PendingDelivery {
                tenant_id: key.0,
                delivery_id: key.1,
                provider: d.provider,
                hook: d.hook,
                attempts: d.attempts,
            }
        };
        let outcome = deliver.deliver(&pending).await;
        let mut inner = self.lock();
        let key = (pending.tenant_id, pending.delivery_id);
        inner.held.remove(&key);
        if let Some(d) = inner.deliveries.get_mut(&key) {
            let now = crate::now();
            d.attempts += 1;
            match &outcome {
                DeliveryOutcome::Delivered => {
                    d.delivered_at = Some(now);
                    d.last_error = None;
                }
                DeliveryOutcome::Retry { error, after } => {
                    d.last_error = Some(error.clone());
                    d.next_attempt_at = now + *after;
                }
                DeliveryOutcome::Refused { error } => {
                    d.delivered_at = Some(now);
                    d.last_error = Some(error.clone());
                }
            }
        }
        Ok(Some((pending, outcome)))
    }

    async fn retry_now(&self, tenant: TenantId, delivery: DeliveryId) -> Result<bool> {
        let mut inner = self.lock();
        match inner.deliveries.get_mut(&(tenant, delivery)) {
            Some(d) if d.delivered_at.is_none() => {
                d.next_attempt_at = crate::now();
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

// ------------------------------------------------------------------------------------ suite

#[cfg(test)]
mod tests {
    //! The suite of the store: the same cases on [`MemoryTypedNodeStore`] and on
    //! [`PgTypedNodeStore`] (`#[sqlx::test]`, one database per case, as the non-superuser role
    //! of `DATABASE_URL`, so row-level security is on). With `RUUM_SUITE_RLS=off` the Postgres
    //! harness turns `FORCE` off on both tables, so the cases test the tenant filters of the
    //! queries alone (the mutation check uses it).

    use super::*;
    use crate::actor::Actor;
    use crate::ids::UserId;
    use crate::providers::deliveries::{Backoff, DeliveryWorker, outcome_of};
    use crate::providers::memory::{MemoryPlatform, MemoryProvider};
    use crate::providers::postgres::{self as pg, PgTypedNodeStore};
    use crate::providers::reconciler::{Reconciler, apply_delivery_id};
    use sqlx::PgPool;
    use std::sync::atomic::{AtomicBool, Ordering};

    const PROVIDER: &str = MemoryProvider::ID;
    const TYPE: &str = MemoryProvider::TYPE;
    const HOUR: Duration = Duration::from_secs(3600);

    // ------------------------------------------------------------------------- harnesses

    /// What the cases need beyond the trait: the rows that the caller's transaction writes.
    #[async_trait]
    trait Harness: Send + Sync {
        fn store(&self) -> Arc<dyn TypedNodeStore>;
        /// A new tenant (Postgres: the tenant, a user and the root folder).
        async fn tenant(&self) -> TenantId;
        /// A typed node, its first manifest version and its `typed_nodes` row, in one
        /// transaction.
        async fn typed_node(
            &self,
            tenant: TenantId,
            node: NodeId,
            version: VersionId,
            manifest: &Value,
            by: Actor,
        );
        /// A new manifest version that becomes the desired one, in one transaction.
        async fn new_version(
            &self,
            tenant: TenantId,
            node: NodeId,
            version: VersionId,
            manifest: &Value,
            by: Actor,
        );
        /// For a node hook, a journal entry and the delivery with its `seq`, in one
        /// transaction; a tenant hook has no entry. Returns whether the delivery was written
        /// and the `seq`.
        async fn enqueue(&self, d: NewDelivery) -> (bool, Option<i64>);
    }

    #[derive(Default)]
    struct MemHarness {
        store: Arc<MemoryTypedNodeStore>,
        seqs: Mutex<HashMap<TenantId, i64>>,
    }

    #[async_trait]
    impl Harness for MemHarness {
        fn store(&self) -> Arc<dyn TypedNodeStore> {
            self.store.clone()
        }
        async fn tenant(&self) -> TenantId {
            TenantId::new()
        }
        async fn typed_node(
            &self,
            tenant: TenantId,
            node: NodeId,
            version: VersionId,
            manifest: &Value,
            by: Actor,
        ) {
            self.store
                .insert(new_row(tenant, node, version), manifest.clone(), by.into())
                .unwrap();
        }
        async fn new_version(
            &self,
            tenant: TenantId,
            node: NodeId,
            version: VersionId,
            manifest: &Value,
            by: Actor,
        ) {
            self.store
                .set_desired(tenant, node, version, manifest.clone(), by.into())
                .unwrap();
        }
        async fn enqueue(&self, mut d: NewDelivery) -> (bool, Option<i64>) {
            if !d.hook.hook.is_tenant_hook() {
                let mut seqs = self.seqs.lock().unwrap();
                let seq = seqs.entry(d.hook.tenant_id).or_default();
                *seq += 1;
                d.hook.seq = Some(*seq);
            }
            let seq = d.hook.seq;
            (self.store.enqueue(d).unwrap(), seq)
        }
    }

    struct PgHarness {
        pool: PgPool,
        store: Arc<PgTypedNodeStore>,
        /// tenant -> (owner, root)
        tenants: Mutex<HashMap<TenantId, (UserId, NodeId)>>,
    }

    impl PgHarness {
        async fn new(pool: PgPool) -> Self {
            if std::env::var("RUUM_SUITE_RLS").is_ok_and(|v| v.trim() == "off") {
                for q in [
                    "alter table typed_nodes no force row level security",
                    "alter table provider_deliveries no force row level security",
                ] {
                    sqlx::query(q).execute(&pool).await.unwrap();
                }
            }
            Self {
                store: Arc::new(PgTypedNodeStore::new(pool.clone())),
                pool,
                tenants: Mutex::default(),
            }
        }

        fn owner(&self, tenant: TenantId) -> (UserId, NodeId) {
            self.tenants.lock().unwrap()[&tenant]
        }

        async fn version_row(
            conn: &mut sqlx::PgConnection,
            tenant: TenantId,
            node: NodeId,
            version: VersionId,
            manifest: &Value,
            by: Actor,
        ) {
            let bytes = serde_json::to_vec(manifest).unwrap();
            sqlx::query(
                "insert into versions (id, tenant_id, node_id, manifest, encoding, size_bytes, \
                 content_hash, actor_kind, actor, on_behalf_of) \
                 values ($1, $2, $3, $4, 'typed_manifest_v1', $5, $6, $7, $8, $9)",
            )
            .bind(version)
            .bind(tenant)
            .bind(node)
            .bind(&bytes)
            .bind(bytes.len() as i64)
            .bind(&blake3::hash(&bytes).as_bytes()[..])
            .bind(by.actor.kind)
            .bind(by.actor.id.0)
            .bind(by.on_behalf_of.map(|u| u.0))
            .execute(&mut *conn)
            .await
            .unwrap();
            sqlx::query("update nodes set head_version = $3 where tenant_id = $1 and id = $2")
                .bind(tenant)
                .bind(node)
                .bind(version)
                .execute(&mut *conn)
                .await
                .unwrap();
        }
    }

    #[async_trait]
    impl Harness for PgHarness {
        fn store(&self) -> Arc<dyn TypedNodeStore> {
            self.store.clone()
        }
        async fn tenant(&self) -> TenantId {
            let (tenant, owner, root) = (TenantId::new(), UserId::new(), NodeId::new());
            let mut tx = self.pool.begin().await.unwrap();
            for q in [
                "set local ruum.system = 'on'",
                "set constraints all deferred",
            ] {
                sqlx::query(q).execute(&mut *tx).await.unwrap();
            }
            sqlx::query(
                "insert into tenants (id, name, kind, plan, root_node_id, created_kind, created_by) \
                 values ($1, 'team', 'team', 'team', $2, 'user', $3)",
            )
            .bind(tenant)
            .bind(root)
            .bind(owner.0)
            .execute(&mut *tx)
            .await
            .unwrap();
            sqlx::query(
                "insert into users (id, tenant_id, email, email_verified, display_name) \
                 values ($1, $2, $3, true, 'x')",
            )
            .bind(owner.0)
            .bind(tenant)
            .bind(format!("{}@example.test", owner.0))
            .execute(&mut *tx)
            .await
            .unwrap();
            sqlx::query(
                "insert into nodes (id, tenant_id, parent_id, name, kind, owner_id, created_kind, \
                 created_by, updated_kind, updated_by) \
                 values ($1, $2, null, 'root', 'folder', $3, 'user', $3, 'user', $3)",
            )
            .bind(root)
            .bind(tenant)
            .bind(owner.0)
            .execute(&mut *tx)
            .await
            .unwrap();
            tx.commit().await.unwrap();
            self.tenants.lock().unwrap().insert(tenant, (owner, root));
            tenant
        }
        async fn typed_node(
            &self,
            tenant: TenantId,
            node: NodeId,
            version: VersionId,
            manifest: &Value,
            by: Actor,
        ) {
            let (owner, root) = self.owner(tenant);
            let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant)
                .await
                .unwrap();
            sqlx::query(
                "insert into nodes (id, tenant_id, parent_id, name, kind, type, owner_id, \
                 created_kind, created_by, updated_kind, updated_by) \
                 values ($1, $2, $3, $4, 'typed', $5, $6, 'user', $6, 'user', $6)",
            )
            .bind(node)
            .bind(tenant)
            .bind(root)
            .bind(node.to_string())
            .bind(TYPE)
            .bind(owner.0)
            .execute(&mut *tx)
            .await
            .unwrap();
            Self::version_row(&mut tx, tenant, node, version, manifest, by).await;
            pg::insert_typed_node(&mut tx, &new_row(tenant, node, version))
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }
        async fn new_version(
            &self,
            tenant: TenantId,
            node: NodeId,
            version: VersionId,
            manifest: &Value,
            by: Actor,
        ) {
            let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant)
                .await
                .unwrap();
            Self::version_row(&mut tx, tenant, node, version, manifest, by).await;
            pg::set_desired_version(&mut tx, tenant, node, version)
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }
        async fn enqueue(&self, mut d: NewDelivery) -> (bool, Option<i64>) {
            let tenant = d.hook.tenant_id;
            let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant)
                .await
                .unwrap();
            if !d.hook.hook.is_tenant_hook() {
                sqlx::query(
                    "insert into journal_counters (tenant_id) values ($1) on conflict do nothing",
                )
                .bind(tenant)
                .execute(&mut *tx)
                .await
                .unwrap();
                let seq: i64 = sqlx::query_scalar("select ruum_next_seq($1)")
                    .bind(tenant)
                    .fetch_one(&mut *tx)
                    .await
                    .unwrap();
                sqlx::query(
                    "insert into journal (tenant_id, seq, node_id, type, actor_kind, actor) \
                     values ($1, $2, $3, 'node.moved', 'user', $4)",
                )
                .bind(tenant)
                .bind(seq)
                .bind(d.hook.node_id)
                .bind(d.hook.by.actor.id)
                .execute(&mut *tx)
                .await
                .unwrap();
                d.hook.seq = Some(seq);
            }
            let written = pg::enqueue(&mut tx, &d).await.unwrap();
            tx.commit().await.unwrap();
            (written, d.hook.seq)
        }
    }

    // --------------------------------------------------------------------------- helpers

    fn new_row(tenant: TenantId, node: NodeId, version: VersionId) -> NewTypedNode {
        NewTypedNode {
            tenant_id: tenant,
            node_id: node,
            provider: PROVIDER.into(),
            type_name: TYPE.into(),
            desired_version: version,
        }
    }

    fn person() -> Actor {
        Actor::user(UserId::new())
    }

    fn status(
        node: NodeId,
        state: ProviderState,
        generation: i64,
        observed: Option<VersionId>,
    ) -> ProviderStatus {
        ProviderStatus {
            node_id: node,
            state,
            generation,
            observed_version_id: observed,
            reason: None,
            message: None,
            detail: None,
            export: None,
            at: crate::now(),
        }
    }

    fn hook(kind: HookKind, tenant: TenantId, node: Option<NodeId>) -> HookDelivery {
        HookDelivery {
            hook: kind,
            tenant_id: tenant,
            node_id: node,
            seq: None,
            target_node_id: None,
            data: None,
            by: person().into(),
            at: crate::now(),
        }
    }

    fn delivery(kind: HookKind, tenant: TenantId, node: Option<NodeId>) -> NewDelivery {
        NewDelivery {
            delivery_id: DeliveryId::new(),
            provider: PROVIDER.into(),
            hook: hook(kind, tenant, node),
        }
    }

    /// A node with a row: the first manifest version written by a person.
    async fn node_with_row(h: &dyn Harness, tenant: TenantId) -> (NodeId, VersionId) {
        let (node, v1) = (NodeId::new(), VersionId::new());
        h.typed_node(
            tenant,
            node,
            v1,
            &serde_json::json!({ "title": "A" }),
            person(),
        )
        .await;
        (node, v1)
    }

    /// (tenant, delivery, node, seq) of a delivered hook.
    type Delivered = (TenantId, DeliveryId, Option<NodeId>, Option<i64>);

    /// A [`Deliver`] that records what it delivered, answers planned outcomes first, and
    /// notices two deliveries of one queue at the same time.
    #[derive(Default)]
    struct Recorder {
        log: Mutex<Vec<Delivered>>,
        plan: Mutex<HashMap<(TenantId, DeliveryId), Vec<DeliveryOutcome>>>,
        busy: Mutex<HashSet<(TenantId, Option<NodeId>)>>,
        overlap: AtomicBool,
        delay: Duration,
    }

    impl Recorder {
        fn with_delay(delay: Duration) -> Self {
            Self {
                delay,
                ..Default::default()
            }
        }
        fn plan(&self, tenant: TenantId, d: DeliveryId, outcomes: Vec<DeliveryOutcome>) {
            self.plan.lock().unwrap().insert((tenant, d), outcomes);
        }
        fn delivered(&self) -> Vec<(TenantId, DeliveryId)> {
            self.log
                .lock()
                .unwrap()
                .iter()
                .map(|(t, d, _, _)| (*t, *d))
                .collect()
        }
    }

    #[async_trait]
    impl Deliver for Recorder {
        async fn deliver(&self, d: &PendingDelivery) -> DeliveryOutcome {
            let queue = (d.tenant_id, d.hook.node_id);
            if !self.busy.lock().unwrap().insert(queue) {
                self.overlap.store(true, Ordering::SeqCst);
            }
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            self.busy.lock().unwrap().remove(&queue);
            let planned = self
                .plan
                .lock()
                .unwrap()
                .get_mut(&(d.tenant_id, d.delivery_id))
                .filter(|v| !v.is_empty())
                .map(|v| v.remove(0));
            if let Some(o) = planned {
                return o;
            }
            self.log
                .lock()
                .unwrap()
                .push((d.tenant_id, d.delivery_id, d.hook.node_id, d.hook.seq));
            DeliveryOutcome::Delivered
        }
    }

    fn retry_in_an_hour() -> DeliveryOutcome {
        DeliveryOutcome::Retry {
            error: "503 unavailable".into(),
            after: HOUR,
        }
    }

    fn worker(h: &dyn Harness, r: &Arc<Recorder>) -> DeliveryWorker {
        DeliveryWorker::with_deliver(h.store(), r.clone())
    }

    // ----------------------------------------------------------------------------- cases

    /// A new row: provisioning, generation 0, nothing observed; another tenant sees nothing.
    async fn typed_rows_are_kept_per_tenant(h: &dyn Harness) {
        let s = h.store();
        let (a, b) = (h.tenant().await, h.tenant().await);
        let (node, v1) = node_with_row(h, a).await;
        let row = s.get(a, node).await.unwrap().expect("the row");
        assert_eq!(row.tenant_id, a);
        assert_eq!(row.node_id, node);
        assert_eq!(row.provider, PROVIDER);
        assert_eq!(row.type_name, TYPE);
        assert_eq!(row.desired_version, v1);
        assert_eq!(row.observed_version, None);
        assert_eq!(row.state, ProviderState::Provisioning);
        assert_eq!(row.generation, 0);
        assert!(row.reason.is_none() && row.message.is_none() && row.detail.is_empty());
        assert!(row.needs_reconcile());
        assert!(
            s.get(b, node).await.unwrap().is_none(),
            "another tenant sees no row"
        );
        assert!(s.get(a, NodeId::new()).await.unwrap().is_none());

        let v2 = VersionId::new();
        h.new_version(a, node, v2, &serde_json::json!({ "title": "B" }), person())
            .await;
        assert_eq!(s.get(a, node).await.unwrap().unwrap().desired_version, v2);
    }

    /// A report with an older generation is ignored: 3, then 2, reads 3.
    async fn an_older_generation_is_ignored(h: &dyn Harness) {
        let s = h.store();
        let (a, b) = (h.tenant().await, h.tenant().await);
        let (node, v1) = node_with_row(h, a).await;

        let mut three = status(node, ProviderState::Ready, 3, Some(v1));
        three.message = Some("ready".into());
        let mut detail = serde_json::Map::new();
        detail.insert("rows".into(), Value::from(3));
        three.detail = Some(detail.clone());
        assert_eq!(s.report(a, &three).await.unwrap(), ReportOutcome::Stored);
        let mut two = status(node, ProviderState::Failed, 2, None);
        two.reason = Some("old_news".into());
        assert_eq!(s.report(a, &two).await.unwrap(), ReportOutcome::Ignored);
        let row = s.get(a, node).await.unwrap().unwrap();
        assert_eq!(row.generation, 3);
        assert_eq!(row.state, ProviderState::Ready);
        assert_eq!(row.observed_version, Some(v1));
        assert_eq!(row.reason, None);
        assert_eq!(row.message.as_deref(), Some("ready"));
        assert_eq!(row.detail, detail);
        assert!(!row.needs_reconcile());
        // The same generation again is ignored too.
        let same = status(node, ProviderState::Degraded, 3, Some(v1));
        assert_eq!(s.report(a, &same).await.unwrap(), ReportOutcome::Ignored);
        assert_eq!(
            s.get(a, node).await.unwrap().unwrap().state,
            ProviderState::Ready
        );

        // Another tenant cannot report on the node, and an unknown node is not found.
        let foreign = status(node, ProviderState::Failed, 9, None);
        let err = s.report(b, &foreign).await.unwrap_err();
        assert_eq!(err.http_status(), 404);
        assert_eq!(s.get(a, node).await.unwrap().unwrap().generation, 3);
        let err = s
            .report(a, &status(NodeId::new(), ProviderState::Ready, 1, None))
            .await
            .unwrap_err();
        assert_eq!(err.http_status(), 404);

        // The checks of the table: a snake_case reason, a short message.
        let mut bad = status(node, ProviderState::Failed, 4, None);
        bad.reason = Some("Not Snake".into());
        assert_eq!(s.report(a, &bad).await.unwrap_err().http_status(), 400);
        let mut long = status(node, ProviderState::Ready, 4, Some(v1));
        long.message = Some("x".repeat(1001));
        assert_eq!(s.report(a, &long).await.unwrap_err().http_status(), 400);
        assert_eq!(s.get(a, node).await.unwrap().unwrap().generation, 3);

        let mut four = status(node, ProviderState::Degraded, 4, Some(v1));
        four.reason = Some("disk_full".into());
        assert_eq!(s.report(a, &four).await.unwrap(), ReportOutcome::Stored);
        let row = s.get(a, node).await.unwrap().unwrap();
        assert_eq!((row.generation, row.state), (4, ProviderState::Degraded));
        assert_eq!(row.reason.as_deref(), Some("disk_full"));
        assert!(row.detail.is_empty(), "a report without detail clears it");
    }

    /// The reconciler's rows: the condition of typed_nodes_reconcile_idx without failed and
    /// deleted rows, from every tenant, with the manifest and the author of the desired version.
    async fn reconcile_candidates_follow_the_index(h: &dyn Harness) {
        let s = h.store();
        let (a, b) = (h.tenant().await, h.tenant().await);
        let author = person();
        let mut expect = HashSet::new();
        let mut skip = HashSet::new();
        // (tenant, state, observed behind)
        let cases = [
            (a, ProviderState::Ready, false, false),
            (a, ProviderState::Provisioning, false, true),
            (b, ProviderState::Updating, false, true),
            (a, ProviderState::Degraded, false, true),
            (b, ProviderState::Ready, true, true),
            (a, ProviderState::Suspended, false, false),
            (a, ProviderState::Suspended, true, true),
            (b, ProviderState::Failed, true, false),
            (a, ProviderState::Deleted, true, false),
        ];
        let mut manifests = HashMap::new();
        for (tenant, state, behind, picked) in cases {
            let (node, v1) = (NodeId::new(), VersionId::new());
            let m1 = serde_json::json!({ "title": format!("{node}") });
            h.typed_node(tenant, node, v1, &m1, author).await;
            s.report(tenant, &status(node, state, 1, Some(v1)))
                .await
                .unwrap();
            let mut desired = m1;
            if behind {
                let v2 = VersionId::new();
                desired = serde_json::json!({ "title": format!("{node}"), "limit": 20 });
                h.new_version(tenant, node, v2, &desired, author).await;
            }
            manifests.insert(node, desired);
            if picked {
                expect.insert((tenant, node));
            } else {
                skip.insert((tenant, node));
            }
        }
        let got = s.reconcile_candidates(Duration::ZERO, 100).await.unwrap();
        let keys: HashSet<(TenantId, NodeId)> = got
            .iter()
            .map(|i| (i.row.tenant_id, i.row.node_id))
            .collect();
        assert_eq!(keys, expect);
        for i in &got {
            assert_eq!(i.manifest, manifests[&i.row.node_id]);
            assert_eq!(i.by, ActorPair::from(author));
            assert!(i.row.needs_reconcile());
        }
        // Oldest state first, and the limit.
        let times: Vec<OffsetDateTime> = got.iter().map(|i| i.row.state_at).collect();
        assert!(times.windows(2).all(|w| w[0] <= w[1]), "{times:?}");
        assert_eq!(
            s.reconcile_candidates(Duration::ZERO, 2)
                .await
                .unwrap()
                .len(),
            2
        );
        // A row whose state just changed waits.
        assert!(s.reconcile_candidates(HOUR, 100).await.unwrap().is_empty());
    }

    /// The reconciler applies the newest manifest version through the registry, polls busy
    /// nodes, and stops at `failed`.
    async fn the_reconciler_applies_the_newest_version(h: &dyn Harness) {
        let s = h.store();
        let platform = Arc::new(StoreReports::new(
            s.clone(),
            Arc::new(MemoryPlatform::new()),
        ));
        let provider = Arc::new(MemoryProvider::new(platform));
        let mut registry = ProviderRegistry::new();
        registry.register(provider.clone()).unwrap();
        let rec = Reconciler::new(s.clone(), Arc::new(registry)).settled(Duration::ZERO);
        let (a, b) = (h.tenant().await, h.tenant().await);

        // A ready node at v1, as the create path leaves it.
        let (node, v1) = node_with_row(h, a).await;
        let created = provider
            .create_typed_node(
                DeliveryId::new(),
                CreateTypedNodeRequest {
                    tenant_id: a,
                    node_id: node,
                    parent_id: None,
                    type_name: TYPE.into(),
                    version_id: v1,
                    manifest: serde_json::json!({ "title": "A" }),
                    source: Source::Empty,
                    by: person().into(),
                },
            )
            .await
            .unwrap();
        s.report(a, created.body()).await.unwrap();
        assert_eq!(
            rec.run_once().await.unwrap(),
            Default::default(),
            "nothing to do"
        );

        // A new manifest version: the reconciler applies it.
        let v2 = VersionId::new();
        let author = person();
        h.new_version(
            a,
            node,
            v2,
            &serde_json::json!({ "title": "B", "limit": 20 }),
            author,
        )
        .await;
        let round = rec.run_once().await.unwrap();
        assert_eq!(
            (round.applied, round.stored, round.errors),
            (1, 1, 0),
            "{round:?}"
        );
        let row = s.get(a, node).await.unwrap().unwrap();
        assert_eq!(row.observed_version, Some(v2));
        assert_eq!(row.state, ProviderState::Ready);
        let at_provider = provider.get_typed_node_status(a, node).await.unwrap();
        assert_eq!(at_provider.observed_version_id, Some(v2));
        assert_eq!(at_provider.generation, row.generation);
        // The delivery was the reconciler's own ID: sending it again changes nothing.
        let again = provider
            .apply_manifest(
                apply_delivery_id(a, node, v2),
                node,
                ApplyManifestRequest {
                    tenant_id: a,
                    version_id: v2,
                    manifest: serde_json::json!({ "title": "B", "limit": 20 }),
                    previous_version_id: v1,
                    by: author.into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(again.body().generation, row.generation);
        assert_eq!(rec.run_once().await.unwrap(), Default::default());

        // A version the provider fails to apply: the row is failed and the loop stops.
        let v3 = VersionId::new();
        h.new_version(
            a,
            node,
            v3,
            &serde_json::json!({ "title": "C", "fail": true }),
            author,
        )
        .await;
        let round = rec.run_once().await.unwrap();
        assert_eq!((round.applied, round.stored), (1, 1));
        let row = s.get(a, node).await.unwrap().unwrap();
        assert_eq!(row.state, ProviderState::Failed);
        assert_eq!(row.reason.as_deref(), Some("apply_failed"));
        assert_eq!(row.observed_version, Some(v2));
        assert_eq!(
            rec.run_once().await.unwrap(),
            Default::default(),
            "failed stops the loop"
        );

        // A node that the provider made but never reported (another tenant): polled.
        let (polled, pv1) = node_with_row(h, b).await;
        provider
            .create_typed_node(
                DeliveryId::new(),
                CreateTypedNodeRequest {
                    tenant_id: b,
                    node_id: polled,
                    parent_id: None,
                    type_name: TYPE.into(),
                    version_id: pv1,
                    manifest: serde_json::json!({ "title": "P" }),
                    source: Source::Empty,
                    by: person().into(),
                },
            )
            .await
            .unwrap();
        // And a row the provider does not know: an error, tried again later.
        let (lost, _) = node_with_row(h, b).await;
        let round = rec.run_once().await.unwrap();
        assert_eq!(
            (round.polled, round.stored, round.errors),
            (2, 1, 1),
            "{round:?}"
        );
        let row = s.get(b, polled).await.unwrap().unwrap();
        assert_eq!(
            (row.state, row.observed_version),
            (ProviderState::Ready, Some(pv1))
        );
        let row = s.get(b, lost).await.unwrap().unwrap();
        assert_eq!(
            (row.state, row.generation),
            (ProviderState::Provisioning, 0)
        );
        // The failed node of tenant a did not move.
        assert_eq!(
            s.get(a, node).await.unwrap().unwrap().state,
            ProviderState::Failed
        );
    }

    /// A failed delivery is retried in seq order per node: a later seq never goes while an
    /// earlier one waits; other nodes go on.
    async fn a_failed_delivery_is_retried_in_seq_order(h: &dyn Harness) {
        let s = h.store();
        let t = h.tenant().await;
        let (n1, n2) = (NodeId::new(), NodeId::new());
        let first = delivery(HookKind::Moved, t, Some(n1));
        let second = delivery(HookKind::Renamed, t, Some(n1));
        let third = delivery(HookKind::Trashed, t, Some(n1));
        let other = delivery(HookKind::Moved, t, Some(n2));
        let mut seqs = Vec::new();
        for d in [&first, &second, &third, &other] {
            let (written, seq) = h.enqueue(d.clone()).await;
            assert!(written);
            seqs.push(seq.unwrap());
        }
        assert!(seqs.windows(2).all(|w| w[0] < w[1]));
        let r = Arc::new(Recorder::default());
        r.plan(t, first.delivery_id, vec![retry_in_an_hour()]);
        let w = worker(h, &r);

        let round = w.run_once(100).await.unwrap();
        assert_eq!((round.delivered, round.retried), (1, 1), "{round:?}");
        assert_eq!(r.delivered(), vec![(t, other.delivery_id)]);
        let st = s.delivery(t, first.delivery_id).await.unwrap().unwrap();
        assert_eq!(st.attempts, 1);
        assert_eq!(st.last_error.as_deref(), Some("503 unavailable"));
        assert!(st.delivered_at.is_none());
        assert!(st.next_attempt_at > crate::now() + Duration::from_secs(1800));
        assert_eq!(st.seq, Some(seqs[0]));
        assert_eq!(st.node_id, Some(n1));
        assert_eq!(st.hook, HookKind::Moved);
        let st = s.delivery(t, second.delivery_id).await.unwrap().unwrap();
        assert_eq!(
            (st.attempts, st.delivered_at),
            (0, None),
            "the later seq waits"
        );

        // Nothing is due while the first waits.
        assert_eq!(w.run_once(100).await.unwrap(), Default::default());

        // Retry now: the three go in seq order.
        assert!(
            !s.retry_now(h.tenant().await, first.delivery_id)
                .await
                .unwrap()
        );
        assert!(s.retry_now(t, first.delivery_id).await.unwrap());
        let round = w.run_once(100).await.unwrap();
        assert_eq!(round.delivered, 3, "{round:?}");
        assert_eq!(
            r.delivered(),
            vec![
                (t, other.delivery_id),
                (t, first.delivery_id),
                (t, second.delivery_id),
                (t, third.delivery_id)
            ]
        );
        let st = s.delivery(t, first.delivery_id).await.unwrap().unwrap();
        assert_eq!(st.attempts, 2);
        assert!(st.delivered_at.is_some() && st.last_error.is_none());
        assert!(
            !s.retry_now(t, first.delivery_id).await.unwrap(),
            "it has ended"
        );
        assert_eq!(w.run_once(100).await.unwrap(), Default::default());
    }

    /// A refusal (a 4xx other than 408 and 429) ends the delivery, and its queue goes on.
    async fn a_refused_delivery_ends_and_the_queue_goes_on(h: &dyn Harness) {
        let s = h.store();
        let t = h.tenant().await;
        let n = NodeId::new();
        let (first, second) = (
            delivery(HookKind::Moved, t, Some(n)),
            delivery(HookKind::Renamed, t, Some(n)),
        );
        h.enqueue(first.clone()).await;
        h.enqueue(second.clone()).await;
        let r = Arc::new(Recorder::default());
        r.plan(
            t,
            first.delivery_id,
            vec![DeliveryOutcome::Refused {
                error: "404 not_found".into(),
            }],
        );
        let round = worker(h, &r).run_once(100).await.unwrap();
        assert_eq!((round.refused, round.delivered), (1, 1));
        let st = s.delivery(t, first.delivery_id).await.unwrap().unwrap();
        assert!(st.delivered_at.is_some());
        assert_eq!(st.last_error.as_deref(), Some("404 not_found"));
        assert_eq!(r.delivered(), vec![(t, second.delivery_id)]);
    }

    /// The tenant hooks of a tenant have their own queue, apart from the nodes and from other
    /// tenants.
    async fn tenant_hooks_have_their_own_queue(h: &dyn Harness) {
        let s = h.store();
        let (a, b) = (h.tenant().await, h.tenant().await);
        let export = delivery(HookKind::TenantExported, a, None);
        let delete = delivery(HookKind::TenantDeleted, a, None);
        let node_hook = delivery(HookKind::Moved, a, Some(NodeId::new()));
        let theirs = delivery(HookKind::TenantExported, b, None);
        for d in [&export, &delete, &node_hook, &theirs] {
            let (written, seq) = h.enqueue(d.clone()).await;
            assert!(written);
            assert_eq!(seq.is_none(), d.hook.hook.is_tenant_hook());
        }
        let r = Arc::new(Recorder::default());
        r.plan(a, export.delivery_id, vec![retry_in_an_hour()]);
        let w = worker(h, &r);
        w.run_once(100).await.unwrap();
        let got: HashSet<(TenantId, DeliveryId)> = r.delivered().into_iter().collect();
        assert_eq!(
            got,
            HashSet::from([(a, node_hook.delivery_id), (b, theirs.delivery_id)]),
            "a waiting tenant hook blocks only the later tenant hooks of its tenant"
        );
        let st = s.delivery(a, delete.delivery_id).await.unwrap().unwrap();
        assert_eq!((st.attempts, st.node_id, st.seq), (0, None, None));
        s.retry_now(a, export.delivery_id).await.unwrap();
        w.run_once(100).await.unwrap();
        let log = r.delivered();
        assert_eq!(
            &log[2..],
            &[(a, export.delivery_id), (a, delete.delivery_id)]
        );
    }

    /// Deliveries are keyed by (tenant, delivery): the same ID twice in a tenant is written
    /// once, and the same ID in another tenant is another delivery that gives nothing of the
    /// first.
    async fn deliveries_are_keyed_by_tenant(h: &dyn Harness) {
        let s = h.store();
        let (a, b, c) = (h.tenant().await, h.tenant().await, h.tenant().await);
        let (na, nb) = (NodeId::new(), NodeId::new());
        let mine = delivery(HookKind::Moved, a, Some(na));
        assert!(h.enqueue(mine.clone()).await.0);
        assert!(
            !h.enqueue(mine.clone()).await.0,
            "the same delivery is written once"
        );
        let mut theirs = delivery(HookKind::Trashed, b, Some(nb));
        theirs.delivery_id = mine.delivery_id;
        assert!(
            h.enqueue(theirs.clone()).await.0,
            "another tenant, another delivery"
        );

        let st = s.delivery(a, mine.delivery_id).await.unwrap().unwrap();
        assert_eq!(
            (st.tenant_id, st.node_id, st.hook),
            (a, Some(na), HookKind::Moved)
        );
        let st = s.delivery(b, mine.delivery_id).await.unwrap().unwrap();
        assert_eq!(
            (st.tenant_id, st.node_id, st.hook),
            (b, Some(nb), HookKind::Trashed)
        );
        assert!(s.delivery(c, mine.delivery_id).await.unwrap().is_none());

        let r = Arc::new(Recorder::default());
        let round = worker(h, &r).run_once(100).await.unwrap();
        assert_eq!(round.delivered, 2);
        let mut got = r.delivered();
        got.sort();
        let mut want = vec![(a, mine.delivery_id), (b, mine.delivery_id)];
        want.sort();
        assert_eq!(got, want, "each tenant's delivery once");
        assert!(
            s.delivery(a, mine.delivery_id)
                .await
                .unwrap()
                .unwrap()
                .delivered_at
                .is_some()
        );
        assert!(
            s.delivery(b, mine.delivery_id)
                .await
                .unwrap()
                .unwrap()
                .delivered_at
                .is_some()
        );
        // Retry now in another tenant does not touch the delivery.
        let mut later = delivery(HookKind::Moved, a, Some(na));
        later.delivery_id = DeliveryId::new();
        h.enqueue(later.clone()).await;
        let r2 = Arc::new(Recorder::default());
        r2.plan(a, later.delivery_id, vec![retry_in_an_hour()]);
        worker(h, &r2).run_once(100).await.unwrap();
        assert!(!s.retry_now(b, later.delivery_id).await.unwrap());
        assert!(s.delivery(b, later.delivery_id).await.unwrap().is_none());
    }

    /// Several workers at the same time: every delivery once, each queue in order, never two
    /// deliveries of one queue at a time.
    async fn workers_never_deliver_one_row_twice(h: &dyn Harness) {
        let tenants = [h.tenant().await, h.tenant().await];
        let mut all = Vec::new();
        for t in tenants {
            let nodes = [NodeId::new(), NodeId::new(), NodeId::new()];
            for _ in 0..4 {
                for n in nodes {
                    let d = delivery(HookKind::Moved, t, Some(n));
                    h.enqueue(d.clone()).await;
                    all.push((t, d.delivery_id));
                }
            }
            for _ in 0..2 {
                let d = delivery(HookKind::TenantExported, t, None);
                h.enqueue(d.clone()).await;
                all.push((t, d.delivery_id));
            }
        }
        let r = Arc::new(Recorder::with_delay(Duration::from_millis(3)));
        let mut tasks = Vec::new();
        for _ in 0..4 {
            let w = worker(h, &r);
            tasks.push(tokio::spawn(async move {
                let mut n = 0;
                loop {
                    let round = w.run_once(5).await.unwrap();
                    n += round.delivered;
                    if round.delivered == 0 {
                        return n;
                    }
                }
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        // A worker may stop while another holds the last rows; finish them.
        worker(h, &r).run_once(1000).await.unwrap();
        let log = r.log.lock().unwrap().clone();
        let mut got: Vec<(TenantId, DeliveryId)> =
            log.iter().map(|(t, d, _, _)| (*t, *d)).collect();
        got.sort();
        all.sort();
        assert_eq!(got, all, "every delivery exactly once");
        assert!(
            !r.overlap.load(Ordering::SeqCst),
            "two deliveries of one queue at a time"
        );
        let mut last: HashMap<(TenantId, Option<NodeId>), (i64, DeliveryId)> = HashMap::new();
        for (t, d, n, seq) in log {
            let key = (seq.unwrap_or(0), d);
            if let Some(prev) = last.insert((t, n), key) {
                assert!(prev < key, "queue {n:?} went out of order");
            }
        }
    }

    /// The worker delivers through the registry; `last_error` holds a status and a code, never
    /// a value.
    async fn the_worker_delivers_through_the_registry(h: &dyn Harness) {
        let s = h.store();
        let platform = Arc::new(StoreReports::new(
            s.clone(),
            Arc::new(MemoryPlatform::new()),
        ));
        let provider = Arc::new(MemoryProvider::new(platform));
        let mut registry = ProviderRegistry::new();
        registry.register(provider.clone()).unwrap();
        let w = DeliveryWorker::new(
            s.clone(),
            Arc::new(registry),
            Backoff {
                first: HOUR,
                max: HOUR,
            },
        );
        let t = h.tenant().await;
        let (node, v1) = node_with_row(h, t).await;
        let created = provider
            .create_typed_node(
                DeliveryId::new(),
                CreateTypedNodeRequest {
                    tenant_id: t,
                    node_id: node,
                    parent_id: None,
                    type_name: TYPE.into(),
                    version_id: v1,
                    manifest: serde_json::json!({ "title": "A" }),
                    source: Source::Empty,
                    by: person().into(),
                },
            )
            .await
            .unwrap();
        s.report(t, created.body()).await.unwrap();

        // trashed: the provider suspends the node and reports into the store.
        let trash = delivery(HookKind::Trashed, t, Some(node));
        h.enqueue(trash.clone()).await;
        // moved of a node the provider does not know: 404, refused for good.
        let unknown = NodeId::new();
        let lost = delivery(HookKind::Moved, t, Some(unknown));
        h.enqueue(lost.clone()).await;
        // A provider that is not registered: retried.
        let mut nobody = delivery(HookKind::Moved, t, Some(NodeId::new()));
        nobody.provider = "no.such".into();
        h.enqueue(nobody.clone()).await;

        let round = w.run_once(100).await.unwrap();
        assert_eq!(
            (round.delivered, round.refused, round.retried),
            (1, 1, 1),
            "{round:?}"
        );
        assert_eq!(
            provider.get_typed_node_status(t, node).await.unwrap().state,
            ProviderState::Suspended
        );
        let row = s.get(t, node).await.unwrap().unwrap();
        assert_eq!(
            row.state,
            ProviderState::Suspended,
            "the report reached the store"
        );
        let st = s.delivery(t, lost.delivery_id).await.unwrap().unwrap();
        assert!(st.delivered_at.is_some());
        assert_eq!(st.last_error.as_deref(), Some("404 not_found"));
        let st = s.delivery(t, nobody.delivery_id).await.unwrap().unwrap();
        assert!(st.delivered_at.is_none());
        assert_eq!(st.last_error.as_deref(), Some("provider_not_registered"));
        assert!(st.next_attempt_at > crate::now() + Duration::from_secs(1800));

        // A provider error's message never reaches last_error.
        let marker = format!("personal{}", Uuid::now_v7().simple());
        let r = Arc::new(Recorder::default());
        let leaky = delivery(HookKind::Moved, t, Some(NodeId::new()));
        h.enqueue(leaky.clone()).await;
        r.plan(
            t,
            leaky.delivery_id,
            vec![outcome_of(
                &Error::Internal(marker.clone()),
                1,
                &Backoff::default(),
            )],
        );
        worker(h, &r).run_once(100).await.unwrap();
        let st = s.delivery(t, leaky.delivery_id).await.unwrap().unwrap();
        assert_eq!(st.last_error.as_deref(), Some("500 internal"));
        assert!(!format!("{st:?}").contains(&marker));
    }

    macro_rules! suite {
        ($($case:ident),* $(,)?) => {
            mod memory_runs {
                $(
                    #[tokio::test]
                    async fn $case() {
                        super::$case(&super::MemHarness::default()).await;
                    }
                )*
            }
            mod postgres_runs {
                $(
                    #[sqlx::test(migrations = "../../migrations")]
                    async fn $case(pool: sqlx::PgPool) {
                        super::$case(&super::PgHarness::new(pool).await).await;
                    }
                )*
            }
        };
    }

    suite!(
        typed_rows_are_kept_per_tenant,
        an_older_generation_is_ignored,
        reconcile_candidates_follow_the_index,
        the_reconciler_applies_the_newest_version,
        a_failed_delivery_is_retried_in_seq_order,
        a_refused_delivery_ends_and_the_queue_goes_on,
        tenant_hooks_have_their_own_queue,
        deliveries_are_keyed_by_tenant,
        workers_never_deliver_one_row_twice,
        the_worker_delivers_through_the_registry,
    );

    /// Several workers on several threads (memory): the same rules as above.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn memory_workers_on_threads() {
        workers_never_deliver_one_row_twice(&MemHarness::default()).await;
    }

    #[test]
    fn new_rows_and_deliveries_are_checked() {
        let (t, n, v) = (TenantId::new(), NodeId::new(), VersionId::new());
        assert!(new_row(t, n, v).check().is_ok());
        let mut bad = new_row(t, n, v);
        bad.provider = "Ruum".into();
        assert!(bad.check().is_err());
        let mut bad = new_row(t, n, v);
        bad.type_name = "has-dash".into();
        assert!(bad.check().is_err());

        let mut d = delivery(HookKind::Moved, t, Some(n));
        assert!(d.check().is_err(), "a node hook needs seq");
        d.hook.seq = Some(1);
        assert!(d.check().is_ok());
        let mut tenant_hook = delivery(HookKind::TenantDeleted, t, Some(n));
        assert!(tenant_hook.check().is_err(), "a tenant hook has no node");
        tenant_hook.hook.node_id = None;
        assert!(tenant_hook.check().is_ok());
        let mut copied = delivery(HookKind::Copied, t, Some(n));
        copied.hook.seq = Some(2);
        assert!(copied.check().is_err(), "copied needs a target");
        copied.hook.target_node_id = Some(NodeId::new());
        assert!(copied.check().is_ok());
    }
}
