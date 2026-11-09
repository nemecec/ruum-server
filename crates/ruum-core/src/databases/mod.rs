//! Database nodes (design section 8; `docs/STEP2.md`, area 3): `createDatabase`, `getDatabase`
//! and `updateDatabaseManifest` (card D-1).
//!
//! A database is a typed node of type `database`, owned by the first-party provider
//! `ruum.database` ([`provider::DatabaseProvider`]). The platform keeps the node, its manifest
//! versions and the `typed_nodes` row (the control loop of section 8.1); the provider makes the
//! database in an engine (`ruum-dbengine`: Postgres on the shared data cluster, SQLite with card
//! E-3, the memory engine for tests) and keeps the `databases` and `database_roles` rows.
//!
//! [`DatabaseService`] is what the API and MCP faces call:
//!
//! - `create`: `edit` on the parent folder (agents: `max_role` `edit`); a guest, or an agent of
//!   a guest, only while the tenant policy `guests_create_resources` is true (decision 0004-10);
//!   a valid manifest ([`manifest`]); an engine that this server has; room in the plan
//!   (`plan_limit_reached`, C-21). Then the node, its first manifest version and its
//!   `typed_nodes` row in one transaction ([`crate::nodes::NodeStore::create_typed`]), and the
//!   provider starts provisioning. The answer is `202` with status `provisioning`; the provider
//!   reports `ready` when the engine has the database.
//! - `get`: `view`; the status, the generation, the size from the engine, the plan limits and
//!   what the caller may do. No engine error text reaches a caller: the provider stores fixed
//!   texts only (decision 0004-8).
//! - `update_manifest`: `manage`; the manifest rules ([`manifest::check`]); a new manifest
//!   version, then the provider applies it. A node in the trash is refused (`409 conflict`), so
//!   the reconciler never applies a manifest to a suspended node (review-P-2 note 7).
//!
//! Engines: an engine kind that the server has no engine for fails with `422
//! validation_failed` before anything is written (no node). The server never uses the memory
//! engine: without a data cluster (`RUUM_DBHOST_URL`) every `createDatabase` fails so, and
//! `engine: sqlite` fails so until card E-3 (`ruum-server`, `wiring.rs`). Only
//! [`Services::in_memory`](crate::Services::in_memory) and the test kit use the memory engine.
//!
//! The submodules of later cards are declared here, so they never edit this file:
//! `migrations` (D-2), `branches` (D-3), `backups` (D-4), `classes` (K-1), `query` (Q-1) and
//! `access` (A-1).

pub mod access;
pub mod backups;
pub mod branches;
pub mod classes;
pub mod manifest;
pub mod migrations;
pub mod provider;
pub mod query;

/// The engines (`ruum-dbengine`), for the crates that wire the services.
pub use ruum_dbengine as engine;

use crate::actor::{Context, PrincipalKind};
use crate::authz::{self, Authz, Role};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{NodeId, TenantId, VersionId};
use crate::journal::{ChangeInfo, EventType, NewEntry, Written};
use crate::nodes::{NewTyped, Node, NodeKind, NodeStore, OnConflict};
use crate::paging::{Page, PageRequest};
use crate::providers::store::{ReportOutcome, TypedNodeRow, TypedNodeStore};
use crate::providers::{
    ApplyManifestRequest, CreateTypedNodeRequest, DataCopy, ProviderState, ProviderStatus, Source,
};
use crate::tenants::{DatabaseUsage, MemberRole, TenantStore, database_limits};
use crate::versions::{Version, VersionFilter};
use async_trait::async_trait;
use ruum_dbengine::{DatabaseEngine, DatabaseRef, EngineKind, EngineRole};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use time::OffsetDateTime;

/// The provider ID of databases.
pub const PROVIDER_ID: &str = "ruum.database";
/// The node type of a database.
pub const NODE_TYPE: &str = "database";
/// The `$id` of the manifest schema.
pub const MANIFEST_SCHEMA_ID: &str = "https://ruum.space/contracts/manifests/database.schema.json";

// ------------------------------------------------------------------------------- engines

/// The engines of this server, by the engine of the manifest. A missing engine refuses
/// `createDatabase` with `422` before a node exists.
#[derive(Clone, Default)]
pub struct Engines {
    pub postgres: Option<Arc<dyn DatabaseEngine>>,
    pub sqlite: Option<Arc<dyn DatabaseEngine>>,
}

impl Engines {
    /// No engine: every `createDatabase` fails with `422` (a server without a data cluster).
    pub fn none() -> Self {
        Self::default()
    }

    /// One memory engine for both kinds. For tests and `Services::in_memory` only: it has no
    /// lens (decision 0004-3 is not met), so a server never uses it.
    pub fn memory() -> Self {
        let e: Arc<dyn DatabaseEngine> = Arc::new(ruum_dbengine::memory::MemoryEngine::new());
        Self {
            postgres: Some(e.clone()),
            sqlite: Some(e),
        }
    }

    pub fn get(&self, kind: EngineKind) -> Option<&Arc<dyn DatabaseEngine>> {
        match kind {
            EngineKind::Postgres => self.postgres.as_ref(),
            EngineKind::Sqlite => self.sqlite.as_ref(),
        }
    }
}

/// The `422` of an engine that this server does not have.
pub fn engine_not_available(kind: EngineKind) -> Error {
    let (message, hint) = match kind {
        EngineKind::Sqlite => (
            "the sqlite engine is not available on this server yet",
            "/engine: use engine postgres (version 17); sqlite comes with a later release",
        ),
        EngineKind::Postgres => (
            "the postgres engine is not available on this server",
            "/engine: this server has no database cluster; ask the operator",
        ),
    };
    Error::Api(ApiError::new(ErrorCode::ValidationFailed, message).with_hint(hint))
}

// ----------------------------------------------------------------------------- settings

/// The limits of one database that are not in the plan table (`DatabaseLimits`):
/// configuration, not code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DatabaseSettings {
    pub statement_timeout_ms: u64,
    pub max_rows: u64,
    pub max_result_bytes: i64,
    /// The largest database that can be branched (decision 18, small databases).
    pub max_branch_source_bytes: i64,
}

impl Default for DatabaseSettings {
    fn default() -> Self {
        Self {
            statement_timeout_ms: 30_000,
            max_rows: 10_000,
            max_result_bytes: 4 * 1024 * 1024,
            max_branch_source_bytes: 1024 * 1024 * 1024,
        }
    }
}

// --------------------------------------------------------------------------- the answers

/// `DatabaseStatus`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DatabaseStatus {
    pub state: ProviderState,
    pub reason: Option<String>,
    /// A fixed text of the provider; never row values and never engine text (decision 0004-8).
    pub message: Option<String>,
    pub observed_version_id: Option<VersionId>,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

impl DatabaseStatus {
    /// The status of a `typed_nodes` row. A row that is `ready` but behind its newest manifest
    /// version shows `updating` (`updateDatabaseManifest`: "the status shows `updating` until
    /// it is applied"). A node in the trash shows `suspended` (reason `in_trash`; `DatabaseState`:
    /// "in the trash ... answers no query"), whatever the provider reported last.
    pub fn of(row: &TypedNodeRow, trashed: bool) -> Self {
        let behind = row.observed_version != Some(row.desired_version);
        let serving = !matches!(
            row.state,
            ProviderState::Failed | ProviderState::Deleted | ProviderState::Suspended
        );
        if trashed && serving {
            return Self {
                state: ProviderState::Suspended,
                reason: Some("in_trash".into()),
                message: None,
                observed_version_id: row.observed_version,
                at: row.state_at,
            };
        }
        let state = match row.state {
            ProviderState::Ready if behind => ProviderState::Updating,
            s => s,
        };
        Self {
            state,
            reason: row.reason.clone(),
            message: row.message.clone(),
            observed_version_id: row.observed_version,
            at: row.state_at,
        }
    }
}

/// `DatabaseLimits`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct DatabaseLimits {
    /// The largest size of the database; `i64::MAX` when the plan has no limit.
    pub max_size_bytes: i64,
    pub statement_timeout_ms: u64,
    pub max_rows: u64,
    pub max_result_bytes: i64,
    pub max_branch_source_bytes: i64,
}

/// `DatabasePermissions`: what the caller may do now (after the scope; no grant of A-1 exists
/// yet, so an agent has no `write_live` and no `read_personal` on a live database).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct DatabasePermissions {
    pub query_read: bool,
    pub query_write: bool,
    pub change_schema: bool,
    pub write_live: bool,
    pub read_personal: bool,
    pub read_secret: bool,
}

impl DatabasePermissions {
    /// The permissions of a role on a live database, for a person or an agent.
    pub fn of(role: Option<Role>, agent: bool) -> Self {
        let at = |r: Role| role.is_some_and(|x| x >= r);
        Self {
            query_read: at(Role::View),
            // An agent writes on a live database only with a `write_live` grant (A-1).
            query_write: at(Role::Edit) && !agent,
            change_schema: at(Role::Manage),
            write_live: at(Role::Edit) && !agent,
            read_personal: at(Role::View) && !agent,
            read_secret: at(Role::Manage) && !agent,
        }
    }
}

/// `Database`.
#[derive(Clone, Debug, Serialize)]
pub struct Database {
    pub node: Node,
    pub manifest: Value,
    pub manifest_version_id: VersionId,
    pub status: DatabaseStatus,
    pub size_bytes: i64,
    pub limits: DatabaseLimits,
    pub branch_of: Option<NodeId>,
    pub branch_data: Option<DataCopy>,
    /// `PitrWindow`, or `null` while there is no backup (card D-4).
    pub pitr_window: Option<Value>,
    /// Columns that wait for a person (card K-1 syncs the catalog; 0 until then).
    pub unconfirmed_columns: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub my_permissions: Option<DatabasePermissions>,
}

// ------------------------------------------------------------------------------ requests

/// `CreateDatabaseRequest.source`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct CreateSource {
    pub kind: String,
    #[serde(default)]
    pub node_id: Option<NodeId>,
}

/// `CreateDatabaseRequest`.
#[derive(Clone, Debug, Deserialize)]
pub struct CreateDatabase {
    pub parent_id: NodeId,
    pub name: String,
    pub manifest: Value,
    #[serde(default)]
    pub source: Option<CreateSource>,
    #[serde(default)]
    pub on_conflict: OnConflict,
}

/// `UpdateDatabaseManifestRequest`.
#[derive(Clone, Debug, Deserialize)]
pub struct UpdateManifest {
    pub manifest: Value,
    #[serde(default)]
    pub label: Option<String>,
}

// ------------------------------------------------------------------------------- the store

/// A `databases` row: where the data of a database node lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatabaseRow {
    pub tenant_id: TenantId,
    pub node_id: NodeId,
    pub engine: EngineKind,
    pub engine_version: String,
    pub cluster: String,
    pub engine_name: String,
    pub branch_of: Option<NodeId>,
    pub branch_data: Option<DataCopy>,
    /// `NULL`: the tenant key (decision 0004-25).
    pub key_node_id: Option<NodeId>,
    pub extensions: Vec<String>,
    pub size_bytes: i64,
}

impl DatabaseRow {
    /// The engine's reference of the database.
    pub fn database_ref(&self) -> DatabaseRef {
        DatabaseRef {
            tenant_id: self.tenant_id.0,
            node_id: self.node_id.0,
            cluster: self.cluster.clone(),
            engine_name: self.engine_name.clone(),
        }
    }
}

/// The platform side of database nodes: the manifest versions and the `databases` and
/// `database_roles` rows. Every method reads and writes the rows of one tenant.
#[async_trait]
pub trait DatabaseStore: Send + Sync {
    /// The manifest of a version of a typed node of the tenant.
    async fn manifest(&self, tenant: TenantId, version: VersionId) -> Result<Option<Value>>;

    /// Called after [`NodeStore::create_typed`] made a database node with its first manifest.
    /// The Postgres store has the version row already and does nothing.
    fn created(&self, node: &Node, version: VersionId, manifest: &Value) {
        let _ = (node, version, manifest);
    }

    /// Writes a new manifest version of a database node as its head (revision + 1), makes it
    /// the desired version of the `typed_nodes` row and writes `version.created`, in one
    /// transaction. `checked` is the desired version that the caller checked the new manifest
    /// against ([`manifest::check`]); when the desired version is another one under the row
    /// lock, someone changed the manifest in between, and the call is `409 conflict` (so an
    /// agent's write never reverts a person's change of `data_classes`). `if_revision` is the
    /// `If-Match` revision (`412`). A node in the trash is `409 conflict`; a node that is not a
    /// database is `404`.
    async fn put_manifest(
        &self,
        ctx: &Context,
        node: NodeId,
        manifest: &Value,
        label: Option<&str>,
        checked: VersionId,
        if_revision: Option<i64>,
    ) -> Result<Written<VersionId>>;

    /// The manifest versions of a database node, newest first (`listVersions` of a database
    /// node): the label, the actor and the time of each. No permission check; the caller
    /// checks `view`.
    async fn list_versions(
        &self,
        tenant: TenantId,
        node: NodeId,
        filter: VersionFilter,
        page: PageRequest,
    ) -> Result<Page<Version>>;

    /// The `databases` row of a node.
    async fn get(&self, tenant: TenantId, node: NodeId) -> Result<Option<DatabaseRow>>;

    /// Writes the `databases` row and its `database_roles` rows (the provider, when the engine
    /// made the database). `conflict` when the node has a row.
    async fn insert(&self, row: &DatabaseRow, roles: &[EngineRole]) -> Result<()>;

    /// Deletes the `databases` row and its roles; returns the row.
    async fn delete(&self, tenant: TenantId, node: NodeId) -> Result<Option<DatabaseRow>>;

    /// The `databases` rows of a tenant.
    async fn list_tenant(&self, tenant: TenantId) -> Result<Vec<DatabaseRow>>;

    /// The database nodes of the tenant that are not purged, per engine (also those in the
    /// trash and those still provisioning), and the branches apart (C-21).
    async fn usage(&self, tenant: TenantId) -> Result<DatabaseUsage>;
}

// ---------------------------------------------------------------------------- the memory

/// The in-memory store. It writes manifest versions through the in-memory node store
/// (`set_head`), journal and typed node store.
pub struct MemoryDatabaseStore {
    nodes: Arc<crate::nodes::memory::MemoryNodeStore>,
    journal: Arc<crate::journal::memory::MemoryJournal>,
    typed: Arc<crate::providers::store::MemoryTypedNodeStore>,
    inner: Mutex<MemDbInner>,
}

#[derive(Default)]
struct MemDbInner {
    manifests: HashMap<(TenantId, VersionId), Value>,
    /// The manifest versions of each database node, oldest first.
    versions: HashMap<(TenantId, NodeId), Vec<Version>>,
    /// The database nodes and their engine (until purged).
    nodes: HashMap<(TenantId, NodeId), EngineKind>,
    rows: HashMap<(TenantId, NodeId), (DatabaseRow, Vec<EngineRole>)>,
}

impl MemoryDatabaseStore {
    pub fn new(
        nodes: Arc<crate::nodes::memory::MemoryNodeStore>,
        journal: Arc<crate::journal::memory::MemoryJournal>,
        typed: Arc<crate::providers::store::MemoryTypedNodeStore>,
    ) -> Self {
        Self {
            nodes,
            journal,
            typed,
            inner: Mutex::new(MemDbInner::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MemDbInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The roles of a database, for tests.
    pub fn roles(&self, tenant: TenantId, node: NodeId) -> Vec<EngineRole> {
        self.lock()
            .rows
            .get(&(tenant, node))
            .map(|(_, r)| r.clone())
            .unwrap_or_default()
    }
}

fn engine_of(manifest: &Value) -> Option<EngineKind> {
    manifest::EngineSpec::of(manifest).ok().map(|s| s.engine)
}

#[async_trait]
impl DatabaseStore for MemoryDatabaseStore {
    async fn manifest(&self, tenant: TenantId, version: VersionId) -> Result<Option<Value>> {
        // A copy's first manifest version is written by the node store (card P-3) into the
        // typed node store only.
        let own = self.lock().manifests.get(&(tenant, version)).cloned();
        Ok(own.or_else(|| self.typed.manifest(tenant, version)))
    }

    fn created(&self, node: &Node, version: VersionId, manifest: &Value) {
        let tenant = node.tenant_id;
        let mut inner = self.lock();
        inner.manifests.insert((tenant, version), manifest.clone());
        inner.versions.insert(
            (tenant, node.id),
            vec![manifest_version(
                tenant,
                node.id,
                version,
                manifest,
                None,
                node.created_by,
                node.created_at,
            )],
        );
        if let Some(e) = engine_of(manifest) {
            inner.nodes.insert((tenant, node.id), e);
        }
    }

    async fn put_manifest(
        &self,
        ctx: &Context,
        node: NodeId,
        manifest: &Value,
        label: Option<&str>,
        checked: VersionId,
        if_revision: Option<i64>,
    ) -> Result<Written<VersionId>> {
        let current = self
            .nodes
            .peek(node)
            .filter(|n| n.tenant_id == ctx.tenant)
            .filter(|n| n.kind == NodeKind::Typed && n.node_type.as_deref() == Some(NODE_TYPE))
            .ok_or_else(|| Error::not_found("database"))?;
        if current.trashed_at.is_some() {
            return Err(in_trash());
        }
        if if_revision.is_some_and(|r| r != current.revision) {
            return Err(crate::nodes::rules::precondition_failed());
        }
        let desired = TypedNodeStore::get(&*self.typed, ctx.tenant, node)
            .await?
            .ok_or_else(|| Error::not_found("database"))?
            .desired_version;
        if desired != checked {
            return Err(manifest_changed());
        }
        let (bytes, hash) = crate::nodes::typed_manifest_bytes(manifest);
        let version = VersionId::new();
        let previous = current.head_version;
        self.typed.set_desired(
            ctx.tenant,
            node,
            version,
            manifest.clone(),
            ctx.actor.into(),
        )?;
        let updated = self.nodes.set_head(ctx, node, Some(version), 0)?;
        {
            let mut inner = self.lock();
            inner
                .manifests
                .insert((ctx.tenant, version), manifest.clone());
            inner
                .versions
                .entry((ctx.tenant, node))
                .or_default()
                .push(manifest_version(
                    ctx.tenant,
                    node,
                    version,
                    manifest,
                    label,
                    ctx.actor,
                    updated.updated_at,
                ));
        }
        let e = self.journal.append(
            ctx,
            NewEntry {
                node_id: Some(node),
                revision: Some(updated.revision),
                event: EventType::VersionCreated,
                data: crate::nodes::typed_version_created_data(
                    version,
                    previous,
                    bytes.len(),
                    &hash,
                ),
            },
        )?;
        Ok(Written {
            value: version,
            change: ChangeInfo {
                seq: Some(e.seq),
                actor: e.actor,
                at: e.at,
            },
        })
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
            .lock()
            .versions
            .get(&(tenant, node))
            .map(|vs| {
                vs.iter()
                    .filter(|v| v.tenant_id == tenant && filter.matches(v))
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
        Ok(page_of(&mut items, limit))
    }

    async fn get(&self, tenant: TenantId, node: NodeId) -> Result<Option<DatabaseRow>> {
        Ok(self
            .lock()
            .rows
            .get(&(tenant, node))
            .map(|(r, _)| r.clone()))
    }

    async fn insert(&self, row: &DatabaseRow, roles: &[EngineRole]) -> Result<()> {
        let mut inner = self.lock();
        let key = (row.tenant_id, row.node_id);
        if inner.rows.contains_key(&key) {
            return Err(Error::api(
                ErrorCode::Conflict,
                format!("database {} has a row", row.node_id),
            ));
        }
        inner.rows.insert(key, (row.clone(), roles.to_vec()));
        inner.nodes.entry(key).or_insert(row.engine);
        Ok(())
    }

    async fn delete(&self, tenant: TenantId, node: NodeId) -> Result<Option<DatabaseRow>> {
        let mut inner = self.lock();
        inner.nodes.remove(&(tenant, node));
        Ok(inner.rows.remove(&(tenant, node)).map(|(r, _)| r))
    }

    async fn list_tenant(&self, tenant: TenantId) -> Result<Vec<DatabaseRow>> {
        let mut rows: Vec<DatabaseRow> = self
            .lock()
            .rows
            .iter()
            .filter(|((t, _), _)| *t == tenant)
            .map(|(_, (r, _))| r.clone())
            .collect();
        rows.sort_by_key(|r| r.node_id);
        Ok(rows)
    }

    async fn usage(&self, tenant: TenantId) -> Result<DatabaseUsage> {
        let inner = self.lock();
        let mut u = DatabaseUsage::default();
        for ((t, n), engine) in &inner.nodes {
            if *t != tenant {
                continue;
            }
            let row = inner.rows.get(&(*t, *n)).map(|(r, _)| r);
            if row.is_some_and(|r| r.branch_of.is_some()) {
                u.branches += 1;
                continue;
            }
            let e = match engine {
                EngineKind::Postgres => &mut u.postgres,
                EngineKind::Sqlite => &mut u.sqlite,
            };
            e.count += 1;
            e.size_bytes += row.map(|r| r.size_bytes).unwrap_or(0);
        }
        Ok(u)
    }
}

/// `409 conflict` when the manifest changed while a new one was checked against it.
pub fn manifest_changed() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::Conflict,
            "the manifest changed while this change was checked",
        )
        .with_hint("read the database again (getDatabase) and send the change again"),
    )
}

/// The `Version` of a manifest version of a typed node.
fn manifest_version(
    tenant: TenantId,
    node: NodeId,
    version: VersionId,
    manifest: &Value,
    label: Option<&str>,
    actor: crate::actor::Actor,
    at: OffsetDateTime,
) -> Version {
    let (bytes, hash) = crate::nodes::typed_manifest_bytes(manifest);
    Version {
        id: version,
        tenant_id: tenant,
        node_id: node,
        size_bytes: bytes.len() as i64,
        content_hash: crate::blobs::Hash(hash),
        content_type: Some("application/json".into()),
        restored_from: None,
        label: label.map(str::to_owned),
        actor,
        created_at: at,
    }
}

/// One page of `items` (at most `limit + 1` of them, newest first).
fn page_of(items: &mut Vec<Version>, limit: usize) -> Page<Version> {
    let next_cursor = if items.len() > limit {
        items.truncate(limit);
        items.last().map(crate::blobs::logic::version_cursor)
    } else {
        None
    };
    Page {
        items: std::mem::take(items),
        next_cursor,
    }
}

/// `409 conflict` for a database node in the trash.
pub fn in_trash() -> Error {
    Error::Api(
        ApiError::new(ErrorCode::Conflict, "the database is in the trash")
            .with_hint("restore the node first (restoreNode)"),
    )
}

// ---------------------------------------------------------------------------- the postgres

/// The Postgres store (`versions`, `nodes`, `typed_nodes`, `databases`, `database_roles` of
/// migrations 0001 and 0004). Every statement filters by `tenant_id` and runs in a transaction
/// of the tenant (row-level security).
#[derive(Clone, Debug)]
pub struct PgDatabaseStore {
    pool: sqlx::PgPool,
}

impl PgDatabaseStore {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

fn engine_from(s: &str) -> Result<EngineKind> {
    match s {
        "postgres" => Ok(EngineKind::Postgres),
        "sqlite" => Ok(EngineKind::Sqlite),
        other => Err(Error::Internal(format!("unknown database engine {other}"))),
    }
}

fn data_copy_from(s: Option<String>) -> Result<Option<DataCopy>> {
    Ok(match s.as_deref() {
        None => None,
        Some("masked") => Some(DataCopy::Masked),
        Some("schema_only") => Some(DataCopy::SchemaOnly),
        Some("full") => Some(DataCopy::Full),
        Some(other) => return Err(Error::Internal(format!("unknown data copy {other}"))),
    })
}

fn data_copy_name(d: DataCopy) -> &'static str {
    match d {
        DataCopy::Masked => "masked",
        DataCopy::SchemaOnly => "schema_only",
        DataCopy::Full => "full",
    }
}

/// The columns that [`database_row`] reads.
const DATABASE_COLUMNS: &str = "tenant_id, node_id, engine::text as engine, engine_version, \
    cluster, engine_name, branch_of, branch_data::text as branch_data, key_node_id, extensions, \
    size_bytes";

fn database_row(r: &sqlx::postgres::PgRow) -> Result<DatabaseRow> {
    use sqlx::Row;
    Ok(DatabaseRow {
        tenant_id: r.try_get("tenant_id")?,
        node_id: r.try_get("node_id")?,
        engine: engine_from(&r.try_get::<String, _>("engine")?)?,
        engine_version: r.try_get("engine_version")?,
        cluster: r.try_get("cluster")?,
        engine_name: r.try_get("engine_name")?,
        branch_of: r.try_get("branch_of")?,
        branch_data: data_copy_from(r.try_get("branch_data")?)?,
        key_node_id: r.try_get("key_node_id")?,
        extensions: r.try_get("extensions")?,
        size_bytes: r.try_get("size_bytes")?,
    })
}

#[async_trait]
impl DatabaseStore for PgDatabaseStore {
    async fn manifest(&self, tenant: TenantId, version: VersionId) -> Result<Option<Value>> {
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        let bytes: Option<Vec<u8>> = sqlx::query_scalar(
            "select manifest from versions where tenant_id = $1 and id = $2 \
             and encoding = 'typed_manifest_v1'",
        )
        .bind(tenant)
        .bind(version)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        bytes
            .map(|b| {
                serde_json::from_slice(&b)
                    .map_err(|e| Error::Internal(format!("typed manifest: {e}")))
            })
            .transpose()
    }

    async fn put_manifest(
        &self,
        ctx: &Context,
        node: NodeId,
        manifest: &Value,
        label: Option<&str>,
        checked: VersionId,
        if_revision: Option<i64>,
    ) -> Result<Written<VersionId>> {
        use sqlx::Row;
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, ctx.tenant).await?;
        let row = sqlx::query(
            "select kind::text as kind, type, revision, head_version, trashed_at \
             from nodes where tenant_id = $1 and id = $2 for update",
        )
        .bind(ctx.tenant)
        .bind(node)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::not_found("database"))?;
        let kind: String = row.try_get("kind")?;
        let node_type: Option<String> = row.try_get("type")?;
        if kind != "typed" || node_type.as_deref() != Some(NODE_TYPE) {
            return Err(Error::not_found("database"));
        }
        if row
            .try_get::<Option<OffsetDateTime>, _>("trashed_at")?
            .is_some()
        {
            return Err(in_trash());
        }
        let revision: i64 = row.try_get("revision")?;
        if if_revision.is_some_and(|r| r != revision) {
            return Err(crate::nodes::rules::precondition_failed());
        }
        // The `nodes` row lock above serializes every manifest write of the node, so the
        // desired version read here stays until this transaction ends.
        let desired: VersionId = sqlx::query_scalar(
            "select desired_version from typed_nodes where tenant_id = $1 and node_id = $2",
        )
        .bind(ctx.tenant)
        .bind(node)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::not_found("database"))?;
        if desired != checked {
            return Err(manifest_changed());
        }
        let previous: Option<VersionId> = row.try_get("head_version")?;
        let (bytes, hash) = crate::nodes::typed_manifest_bytes(manifest);
        let version = VersionId::new();
        let now = crate::now();
        sqlx::query(
            "insert into versions (id, tenant_id, node_id, manifest, encoding, size_bytes, \
             content_hash, content_type, label, actor_kind, actor, on_behalf_of, created_at, \
             updated_at) values ($1, $2, $3, $4, 'typed_manifest_v1', $5, $6, \
             'application/json', $7, $8, $9, $10, $11, $11)",
        )
        .bind(version)
        .bind(ctx.tenant)
        .bind(node)
        .bind(&bytes)
        .bind(bytes.len() as i64)
        .bind(hash.to_vec())
        .bind(label)
        .bind(ctx.actor.actor.kind)
        .bind(ctx.actor.actor.id.0)
        .bind(ctx.actor.on_behalf_of.map(|u| u.0))
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "update nodes set head_version = $3, revision = revision + 1, updated_at = $4, \
             updated_kind = $5, updated_by = $6, updated_obo = $7 \
             where tenant_id = $1 and id = $2",
        )
        .bind(ctx.tenant)
        .bind(node)
        .bind(version)
        .bind(now)
        .bind(ctx.actor.actor.kind)
        .bind(ctx.actor.actor.id.0)
        .bind(ctx.actor.on_behalf_of.map(|u| u.0))
        .execute(&mut *tx)
        .await?;
        crate::providers::postgres::set_desired_version(&mut tx, ctx.tenant, node, version).await?;
        let e = crate::journal::postgres::append(
            &mut tx,
            ctx,
            NewEntry {
                node_id: Some(node),
                revision: Some(revision + 1),
                event: EventType::VersionCreated,
                data: crate::nodes::typed_version_created_data(
                    version,
                    previous,
                    bytes.len(),
                    &hash,
                ),
            },
        )
        .await?;
        tx.commit().await?;
        Ok(Written {
            value: version,
            change: ChangeInfo {
                seq: Some(e.seq),
                actor: ctx.actor,
                at: e.at,
            },
        })
    }

    async fn list_versions(
        &self,
        tenant: TenantId,
        node: NodeId,
        filter: VersionFilter,
        page: PageRequest,
    ) -> Result<Page<Version>> {
        use sqlx::Row;
        let after = page
            .cursor
            .as_ref()
            .map(crate::blobs::logic::decode_version_cursor)
            .transpose()?;
        let limit = page.limit() as usize;
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        let rows = sqlx::query(
            "select id, size_bytes, content_hash, content_type, restored_from, label, \
             actor_kind, actor, on_behalf_of, created_at from versions \
             where tenant_id = $1 and node_id = $2 and encoding = 'typed_manifest_v1' \
             and ($3::principal_kind is null or actor_kind = $3) \
             and ($4::boolean is null or (label is not null) = $4) \
             and ($5::timestamptz is null or (created_at, id) < ($5, $6)) \
             order by created_at desc, id desc limit $7",
        )
        .bind(tenant)
        .bind(node)
        .bind(filter.actor_kind)
        .bind(filter.labeled)
        .bind(after.map(|(at, _)| at))
        .bind(after.map(|(_, id)| id))
        .bind(limit as i64 + 1)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let mut items = rows
            .iter()
            .map(|r| {
                let hash: Vec<u8> = r.try_get("content_hash")?;
                let hash: [u8; 32] = hash
                    .try_into()
                    .map_err(|_| Error::Internal("a stored hash is not 32 bytes".into()))?;
                Ok(Version {
                    id: r.try_get("id")?,
                    tenant_id: tenant,
                    node_id: node,
                    size_bytes: r.try_get("size_bytes")?,
                    content_hash: crate::blobs::Hash(hash),
                    content_type: r.try_get("content_type")?,
                    restored_from: r.try_get("restored_from")?,
                    label: r.try_get("label")?,
                    actor: crate::actor::Actor {
                        actor: crate::actor::PrincipalRef {
                            kind: r.try_get("actor_kind")?,
                            id: crate::ids::PrincipalId(r.try_get("actor")?),
                        },
                        on_behalf_of: r
                            .try_get::<Option<uuid::Uuid>, _>("on_behalf_of")?
                            .map(crate::ids::UserId),
                    },
                    created_at: r.try_get("created_at")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(page_of(&mut items, limit))
    }

    async fn get(&self, tenant: TenantId, node: NodeId) -> Result<Option<DatabaseRow>> {
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {DATABASE_COLUMNS} from databases where tenant_id = $1 and node_id = $2"
        )))
        .bind(tenant)
        .bind(node)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.as_ref().map(database_row).transpose()
    }

    async fn insert(&self, row: &DatabaseRow, roles: &[EngineRole]) -> Result<()> {
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, row.tenant_id).await?;
        let done = sqlx::query(
            "insert into databases (tenant_id, node_id, engine, engine_version, cluster, \
             engine_name, branch_of, branch_data, key_node_id, extensions, size_bytes) \
             values ($1, $2, $3::database_engine, $4, $5, $6, $7, $8::data_copy, $9, $10, $11) \
             on conflict do nothing",
        )
        .bind(row.tenant_id)
        .bind(row.node_id)
        .bind(manifest::engine_name(row.engine))
        .bind(&row.engine_version)
        .bind(&row.cluster)
        .bind(&row.engine_name)
        .bind(row.branch_of)
        .bind(row.branch_data.map(data_copy_name))
        .bind(row.key_node_id)
        .bind(&row.extensions)
        .bind(row.size_bytes)
        .execute(&mut *tx)
        .await?;
        if done.rows_affected() == 0 {
            return Err(Error::api(
                ErrorCode::Conflict,
                format!("database {} has a row", row.node_id),
            ));
        }
        for role in roles {
            sqlx::query(
                "insert into database_roles (id, tenant_id, node_id, kind, binding_id, role_name) \
                 values ($1, $2, $3, $4, $5, $6)",
            )
            .bind(uuid::Uuid::now_v7())
            .bind(row.tenant_id)
            .bind(row.node_id)
            .bind(role.kind.as_str())
            .bind(role.binding)
            .bind(&role.role_name)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn delete(&self, tenant: TenantId, node: NodeId) -> Result<Option<DatabaseRow>> {
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        // The rows that refer to the database go first, in the order of their foreign keys
        // (card P-3: a purge of a database with columns, grants or a query log deletes it).
        for stmt in [
            "delete from database_queries where tenant_id = $1 and node_id = $2",
            "delete from database_approvals where tenant_id = $1 and node_id = $2",
            "delete from database_access_grants where tenant_id = $1 and node_id = $2",
            "delete from database_restores where tenant_id = $1 \
             and (node_id = $2 or target_node_id = $2)",
            "delete from database_migrations where tenant_id = $1 and node_id = $2",
            "delete from database_restore_points where tenant_id = $1 and node_id = $2",
            "delete from database_columns where tenant_id = $1 and node_id = $2",
            "delete from database_roles where tenant_id = $1 and node_id = $2",
        ] {
            sqlx::query(stmt)
                .bind(tenant)
                .bind(node)
                .execute(&mut *tx)
                .await?;
        }
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "delete from databases where tenant_id = $1 and node_id = $2 \
             returning {DATABASE_COLUMNS}"
        )))
        .bind(tenant)
        .bind(node)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.as_ref().map(database_row).transpose()
    }

    async fn list_tenant(&self, tenant: TenantId) -> Result<Vec<DatabaseRow>> {
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {DATABASE_COLUMNS} from databases where tenant_id = $1 order by node_id"
        )))
        .bind(tenant)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter().map(database_row).collect()
    }

    async fn usage(&self, tenant: TenantId) -> Result<DatabaseUsage> {
        use sqlx::Row;
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        // The engine of a node that is still provisioning comes from its newest manifest.
        let rows = sqlx::query(
            "select coalesce(d.engine::text, convert_from(v.manifest, 'UTF8')::jsonb ->> 'engine') \
             as engine, d.branch_of is not null as branch, count(*) as n, \
             coalesce(sum(d.size_bytes), 0)::bigint as size \
             from typed_nodes t \
             join versions v on v.tenant_id = t.tenant_id and v.id = t.desired_version \
             left join databases d on d.tenant_id = t.tenant_id and d.node_id = t.node_id \
             where t.tenant_id = $1 and t.type = 'database' and t.state <> 'deleted' \
             group by 1, 2",
        )
        .bind(tenant)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let mut u = DatabaseUsage::default();
        for r in rows {
            let n = r.try_get::<i64, _>("n")?.max(0) as u32;
            if r.try_get::<bool, _>("branch")? {
                u.branches += n;
                continue;
            }
            let e = match r.try_get::<Option<String>, _>("engine")?.as_deref() {
                Some("postgres") => &mut u.postgres,
                Some("sqlite") => &mut u.sqlite,
                _ => continue,
            };
            e.count += n;
            e.size_bytes += r.try_get::<i64, _>("size")?;
        }
        Ok(u)
    }
}

// ---------------------------------------------------------------------------- the service

/// What [`DatabaseService`] works with.
#[derive(Clone)]
pub struct DatabaseDeps {
    pub nodes: Arc<dyn NodeStore>,
    pub authz: Arc<dyn Authz>,
    pub tenants: Arc<dyn TenantStore>,
    pub store: Arc<dyn DatabaseStore>,
    pub typed: Arc<dyn TypedNodeStore>,
    pub provider: Arc<provider::DatabaseProvider>,
    pub engines: Engines,
    pub settings: DatabaseSettings,
    pub columns: Arc<dyn classes::ColumnStore>,
    pub masking: Arc<dyn crate::privacy::MaskingRules>,
    pub grants: Arc<dyn access::grants::GrantStore>,
    pub queries: query::QueryDeps,
}

/// `createDatabase`, `getDatabase`, `updateDatabaseManifest` and the usage of `getLimits`.
#[derive(Clone)]
pub struct DatabaseService {
    deps: DatabaseDeps,
}

/// `403 permission_denied` with the hint `guests_create_resources` (decision 0004-10).
pub fn guests_may_not_create() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::PermissionDenied,
            "guests may not create databases in this space",
        )
        .with_hint("guests_create_resources"),
    )
}

/// Share-link bearers call no database operation (`403 user_required`).
fn refuse_share_link(ctx: &Context) -> Result<()> {
    if ctx.actor.actor.kind == PrincipalKind::ShareLink {
        Err(Error::user_required())
    } else {
        Ok(())
    }
}

impl DatabaseService {
    pub fn new(deps: DatabaseDeps) -> Self {
        Self { deps }
    }

    pub fn deps(&self) -> &DatabaseDeps {
        &self.deps
    }

    /// `createDatabase`. See the module docs for the order of the checks.
    pub async fn create(&self, ctx: &Context, req: CreateDatabase) -> Result<Written<Database>> {
        refuse_share_link(ctx)?;
        let d = &self.deps;
        authz::require(&*d.authz, &*d.nodes, ctx, req.parent_id, Role::Edit).await?;
        self.check_guest_policy(ctx).await?;
        if let Some(source) = &req.source {
            match source.kind.as_str() {
                "empty" => {}
                // A template is a copy (decision 0004-27), taken on the path of a branch (D-3).
                "template" => {
                    return Err(Error::Api(
                        ApiError::new(
                            ErrorCode::ValidationFailed,
                            "a database from a template is not available yet",
                        )
                        .with_hint("/source/kind: use empty"),
                    ));
                }
                _ => {
                    return Err(Error::Api(
                        ApiError::new(ErrorCode::ValidationFailed, "source.kind is not valid")
                            .with_hint("/source/kind: empty or template"),
                    ));
                }
            }
        }
        let manifest = manifest::check(&req.manifest, None, ctx.actor.is_agent())?;
        let spec = manifest::EngineSpec::of(&manifest)?;
        if d.engines.get(spec.engine).is_none() {
            return Err(engine_not_available(spec.engine));
        }
        self.check_plan(ctx.tenant, spec.engine).await?;
        let written = d
            .nodes
            .create_typed(
                ctx,
                NewTyped {
                    parent_id: req.parent_id,
                    name: req.name,
                    node_type: NODE_TYPE.into(),
                    provider: PROVIDER_ID.into(),
                    manifest: manifest.clone(),
                    on_conflict: req.on_conflict,
                },
            )
            .await?;
        let node = written.value.id;
        let version = written
            .value
            .head_version
            .ok_or_else(|| Error::Internal("a new typed node has no version".into()))?;
        d.store.created(&written.value, version, &manifest);
        // Start provisioning. A failure here leaves the row `provisioning`; the reconciler
        // sends `createTypedNode` again with the same delivery ID.
        let req = CreateTypedNodeRequest {
            tenant_id: ctx.tenant,
            node_id: node,
            parent_id: written.value.parent_id,
            type_name: NODE_TYPE.into(),
            version_id: version,
            manifest,
            source: Source::Empty,
            by: ctx.actor.into(),
        };
        let delivery = crate::providers::reconciler::create_delivery_id(ctx.tenant, node, version);
        use crate::providers::Provider;
        match d.provider.create_typed_node(delivery, req).await {
            Ok(reply) => self.store_status(ctx.tenant, reply.into_inner()).await,
            Err(e) => tracing::warn!(
                tenant = %ctx.tenant,
                node = %node,
                error = %crate::providers::store::error_text(&e),
                "createTypedNode failed; the reconciler tries again"
            ),
        }
        let db = self.view(ctx, written.value).await?;
        Ok(Written {
            value: db,
            change: written.change,
        })
    }

    /// `getDatabase`.
    pub async fn get(&self, ctx: &Context, id: NodeId) -> Result<Database> {
        refuse_share_link(ctx)?;
        let node = self.database_node(ctx, id).await?;
        self.view(ctx, node).await
    }

    /// `updateDatabaseManifest`.
    pub async fn update_manifest(
        &self,
        ctx: &Context,
        id: NodeId,
        req: UpdateManifest,
        if_revision: Option<i64>,
    ) -> Result<Written<Database>> {
        refuse_share_link(ctx)?;
        let d = &self.deps;
        let node = self.database_node(ctx, id).await?;
        authz::require(&*d.authz, &*d.nodes, ctx, id, Role::Manage).await?;
        if node.trashed_at.is_some() {
            return Err(in_trash());
        }
        if let Some(l) = &req.label
            && !(1..=200).contains(&l.chars().count())
        {
            return Err(Error::Api(
                ApiError::new(ErrorCode::ValidationFailed, "label has 1 to 200 characters")
                    .with_hint("/label"),
            ));
        }
        let row = self.typed_row(ctx.tenant, id).await?;
        let current = d
            .store
            .manifest(ctx.tenant, row.desired_version)
            .await?
            .ok_or_else(|| Error::Internal(format!("database {id} has no manifest")))?;
        let manifest = manifest::check(&req.manifest, Some(&current), ctx.actor.is_agent())?;
        let written = d
            .store
            .put_manifest(
                ctx,
                id,
                &manifest,
                req.label.as_deref(),
                row.desired_version,
                if_revision,
            )
            .await?;
        let version = written.value;
        let apply = ApplyManifestRequest {
            tenant_id: ctx.tenant,
            version_id: version,
            manifest,
            previous_version_id: row.observed_version.unwrap_or(row.desired_version),
            by: ctx.actor.into(),
        };
        let delivery = crate::providers::reconciler::apply_delivery_id(ctx.tenant, id, version);
        use crate::providers::Provider;
        match d.provider.apply_manifest(delivery, id, apply).await {
            Ok(reply) => self.store_status(ctx.tenant, reply.into_inner()).await,
            Err(e) => tracing::warn!(
                tenant = %ctx.tenant,
                node = %id,
                error = %crate::providers::store::error_text(&e),
                "applyManifest failed; the reconciler tries again"
            ),
        }
        let node = d.nodes.get(ctx, id).await?;
        let db = self.view(ctx, node).await?;
        Ok(Written {
            value: db,
            change: written.change,
        })
    }

    /// `restoreVersion` of a database node (card P-3, decision 0004-6): a new manifest version
    /// with the content of an older one, through the rules of [`Self::update_manifest`]
    /// (`manage`, the manifest rules, an agent may not change `data_classes`, not in the
    /// trash), then the provider applies it. It never changes data. Returns the new version.
    pub async fn restore_version(
        &self,
        ctx: &Context,
        id: NodeId,
        version: VersionId,
    ) -> Result<Written<Version>> {
        refuse_share_link(ctx)?;
        let d = &self.deps;
        self.database_node(ctx, id).await?;
        authz::require(&*d.authz, &*d.nodes, ctx, id, Role::Manage).await?;
        self.find_version(ctx.tenant, id, version)
            .await?
            .ok_or_else(|| Error::not_found("version"))?;
        let old = d
            .store
            .manifest(ctx.tenant, version)
            .await?
            .ok_or_else(|| Error::not_found("version"))?;
        let written = self
            .update_manifest(
                ctx,
                id,
                UpdateManifest {
                    manifest: old,
                    label: None,
                },
                None,
            )
            .await?;
        let new = written.value.manifest_version_id;
        let v = self
            .find_version(ctx.tenant, id, new)
            .await?
            .ok_or_else(|| Error::Internal(format!("database {id}: the new version is missing")))?;
        Ok(Written {
            value: v,
            change: written.change,
        })
    }

    /// A manifest version of a database node, by its ID.
    async fn find_version(
        &self,
        tenant: TenantId,
        id: NodeId,
        version: VersionId,
    ) -> Result<Option<Version>> {
        let mut page = PageRequest {
            cursor: None,
            limit: Some(200),
        };
        for _ in 0..1000 {
            let got = self
                .deps
                .store
                .list_versions(tenant, id, VersionFilter::default(), page.clone())
                .await?;
            if let Some(v) = got.items.into_iter().find(|v| v.id == version) {
                return Ok(Some(v));
            }
            match got.next_cursor {
                Some(c) => page.cursor = Some(c),
                None => return Ok(None),
            }
        }
        Ok(None)
    }

    /// Whether a node of the tenant is a database node (its `typed_nodes` row names the
    /// type `database`). No permission check: the API uses it to send `listVersions` of a
    /// database node to [`Self::list_versions`], which checks `view`.
    pub async fn is_database(&self, tenant: TenantId, id: NodeId) -> Result<bool> {
        Ok(self
            .deps
            .typed
            .get(tenant, id)
            .await?
            .is_some_and(|r| r.type_name == NODE_TYPE))
    }

    /// `listVersions` of a database node: its manifest versions, newest first, with the label
    /// and the actor of each (read only). Needs `view`, as `getDatabase` does (a node in the
    /// trash needs `manage`).
    pub async fn list_versions(
        &self,
        ctx: &Context,
        id: NodeId,
        filter: VersionFilter,
        page: PageRequest,
    ) -> Result<Page<Version>> {
        refuse_share_link(ctx)?;
        self.database_node(ctx, id).await?;
        self.deps
            .store
            .list_versions(ctx.tenant, id, filter, page)
            .await
    }

    /// The databases of the tenant (`Usage.databases`, C-21).
    pub async fn usage(&self, tenant: TenantId) -> Result<DatabaseUsage> {
        self.deps.store.usage(tenant).await
    }

    /// A node that the caller sees and that is a database; else `404`.
    async fn database_node(&self, ctx: &Context, id: NodeId) -> Result<Node> {
        let node = self.deps.nodes.get(ctx, id).await?;
        if node.kind != NodeKind::Typed || node.node_type.as_deref() != Some(NODE_TYPE) {
            return Err(Error::not_found("database"));
        }
        Ok(node)
    }

    async fn typed_row(&self, tenant: TenantId, id: NodeId) -> Result<TypedNodeRow> {
        self.deps
            .typed
            .get(tenant, id)
            .await?
            .ok_or_else(|| Error::not_found("database"))
    }

    async fn store_status(&self, tenant: TenantId, status: ProviderStatus) {
        match self.deps.typed.report(tenant, &status).await {
            Ok(ReportOutcome::Stored | ReportOutcome::Ignored) => {}
            Err(e) => tracing::warn!(
                %tenant,
                node = %status.node_id,
                error = %crate::providers::store::error_text(&e),
                "could not store a status of the database provider"
            ),
        }
    }

    /// Guests and agents of guests need `guests_create_resources` (decision 0004-10).
    async fn check_guest_policy(&self, ctx: &Context) -> Result<()> {
        let d = &self.deps;
        let role = match ctx.actor.effective_user() {
            Some(user) => d.tenants.role_of(ctx.tenant, user).await?,
            None => None,
        };
        if matches!(
            role,
            Some(MemberRole::Member | MemberRole::Admin | MemberRole::Owner)
        ) {
            return Ok(());
        }
        let tenant = d.tenants.find(ctx.tenant).await?;
        if tenant.policy.guests_create_resources {
            Ok(())
        } else {
            Err(guests_may_not_create())
        }
    }

    /// `plan_limit_reached` when the engine's count of the plan is reached (C-21).
    async fn check_plan(&self, tenant: TenantId, engine: EngineKind) -> Result<()> {
        let d = &self.deps;
        let t = d.tenants.find(tenant).await?;
        let members = d.tenants.member_count(tenant).await?;
        let limits = database_limits(t.plan, members);
        let usage = d.store.usage(tenant).await?;
        let (limit, used) = match engine {
            EngineKind::Postgres => (limits.postgres.count, usage.postgres.count),
            EngineKind::Sqlite => (limits.sqlite.count, usage.sqlite.count),
        };
        match limit {
            Some(max) if used >= max => Err(Error::Api(
                ApiError::new(
                    ErrorCode::PlanLimitReached,
                    format!(
                        "the plan allows {max} {} database(s); the space has {used}",
                        manifest::engine_name(engine)
                    ),
                )
                .with_hint("delete a database or move to a larger plan"),
            )),
            _ => Ok(()),
        }
    }

    /// The `Database` answer of a database node the caller sees.
    async fn view(&self, ctx: &Context, node: Node) -> Result<Database> {
        let d = &self.deps;
        let row = self.typed_row(ctx.tenant, node.id).await?;
        let manifest = d
            .store
            .manifest(ctx.tenant, row.desired_version)
            .await?
            .ok_or_else(|| Error::Internal(format!("database {} has no manifest", node.id)))?;
        let spec = manifest::EngineSpec::of(&manifest)?;
        let db = d.store.get(ctx.tenant, node.id).await?;
        let size_bytes = match &db {
            Some(r) => match d.engines.get(r.engine) {
                Some(engine) => match engine.size_bytes(&r.database_ref()).await {
                    Ok(n) => i64::try_from(n).unwrap_or(i64::MAX),
                    Err(_) => r.size_bytes,
                },
                None => r.size_bytes,
            },
            None => 0,
        };
        let t = d.tenants.find(ctx.tenant).await?;
        let members = d.tenants.member_count(ctx.tenant).await?;
        let plan = database_limits(t.plan, members);
        let max_size = match spec.engine {
            EngineKind::Postgres => plan.postgres.size_bytes,
            EngineKind::Sqlite => plan.sqlite.size_bytes,
        };
        let s = d.settings;
        let my_permissions = Some(DatabasePermissions::of(node.my_role, ctx.actor.is_agent()));
        Ok(Database {
            manifest_version_id: row.desired_version,
            status: DatabaseStatus::of(&row, node.trashed_at.is_some()),
            size_bytes,
            limits: DatabaseLimits {
                max_size_bytes: max_size.unwrap_or(i64::MAX),
                statement_timeout_ms: s.statement_timeout_ms,
                max_rows: s.max_rows,
                max_result_bytes: s.max_result_bytes,
                max_branch_source_bytes: s.max_branch_source_bytes,
            },
            branch_of: db.as_ref().and_then(|r| r.branch_of),
            branch_data: db.as_ref().and_then(|r| r.branch_data),
            pitr_window: None,
            unconfirmed_columns: self.unconfirmed_columns(ctx.tenant, node.id).await?,
            my_permissions,
            manifest,
            node,
        })
    }
}

/// The parts of the database area for [`install`].
pub struct DatabaseParts {
    pub engines: Engines,
    pub keys: Arc<crate::privacy::keys::TenantKeys>,
    pub store: Arc<dyn DatabaseStore>,
    pub typed: Arc<dyn TypedNodeStore>,
    pub columns: Arc<dyn classes::ColumnStore>,
    pub masking: Arc<dyn crate::privacy::MaskingRules>,
    pub grants: Arc<dyn access::grants::GrantStore>,
    pub queries: Arc<dyn query::QueryLogStore>,
}

/// A key-encryption key that lives in memory only, for a process that makes no tenant key (no
/// engine) or for tests.
pub fn throwaway_kek() -> Arc<dyn crate::privacy::keys::KeyWrapper> {
    Arc::new(crate::privacy::keys::LocalKekWrapper::from_bytes(
        &rand::random(),
    ))
}

/// The provider registry (with `ruum.database`) and the service over these stores. Hands the
/// typed-node hooks of the registry to `nodes` (card P-3; only the first call counts).
pub fn build(
    nodes: Arc<dyn NodeStore>,
    authz: Arc<dyn Authz>,
    tenants: Arc<dyn TenantStore>,
    parts: &DatabaseParts,
) -> Result<(
    Arc<crate::providers::ProviderRegistry>,
    Arc<DatabaseService>,
)> {
    let platform: Arc<dyn crate::providers::PlatformApi> =
        Arc::new(crate::providers::store::StoreReports::new(
            parts.typed.clone(),
            Arc::new(provider::NoPlatformCalls),
        ));
    let provider = Arc::new(provider::DatabaseProvider::new(provider::ProviderDeps {
        engines: parts.engines.clone(),
        keys: parts.keys.clone(),
        store: parts.store.clone(),
        typed: parts.typed.clone(),
        platform,
    }));
    let mut registry = crate::providers::ProviderRegistry::new();
    registry.register(provider.clone())?;
    let registry = Arc::new(registry);
    // The node store writes the lifecycle hooks of typed nodes to these providers (card P-3).
    nodes.set_typed_hooks(Arc::new(crate::providers::hooks::RegistryHooks::new(
        registry.clone(),
        tenants.clone(),
    )));
    let service = DatabaseService::new(DatabaseDeps {
        nodes,
        authz,
        tenants,
        store: parts.store.clone(),
        typed: parts.typed.clone(),
        provider,
        engines: parts.engines.clone(),
        settings: DatabaseSettings::default(),
        columns: parts.columns.clone(),
        masking: parts.masking.clone(),
        grants: parts.grants.clone(),
        queries: query::QueryDeps::new(parts.queries.clone(), parts.keys.clone()),
    });
    Ok((registry, Arc::new(service)))
}

/// Builds the database provider, the provider registry and the service over the current
/// stores of `services` (nodes, authz, tenants), and puts them into `services` with `parts`.
/// Call it after the node and tenant stores are chosen.
pub fn install(services: &mut crate::Services, parts: DatabaseParts) -> Result<()> {
    let (registry, service) = build(
        services.nodes.clone(),
        services.authz.clone(),
        services.tenants.clone(),
        &parts,
    )?;
    services.database_engines = parts.engines;
    services.tenant_keys = parts.keys;
    services.typed_nodes = parts.typed;
    services.providers = registry;
    services.databases = service;
    Ok(())
}

#[cfg(test)]
mod tests {
    //! The service and the provider on the in-memory services (memory engine), and the key
    //! path on the Postgres engine (`RUUM_DBHOST_URL`; skipped without it, except in CI).

    use super::*;
    use crate::actor::{Actor, PrincipalRef};
    use crate::agents::{AgentScope, Budget, NewAgent};
    use crate::identity::VerifiedLogin;
    use crate::providers::{HookDelivery, HookKind, Provider};
    use crate::sharing::Grantee;
    use crate::tenants::{Tenant, TenantPolicyChange};
    use crate::{Services, UserId};
    use serde_json::json;
    use std::time::Duration;

    pub(crate) struct World {
        pub s: Services,
        pub handles: crate::services::MemoryHandles,
        pub team: Tenant,
        pub alice: UserId,
        pub bob: UserId,
        pub folder: NodeId,
    }

    pub(crate) fn ctx(tenant: TenantId, user: UserId) -> Context {
        Context {
            tenant,
            actor: Actor::user(user),
            scope: None,
            idempotency_key: None,
        }
    }

    fn login(name: &str) -> VerifiedLogin {
        let run = uuid::Uuid::now_v7().simple().to_string();
        VerifiedLogin {
            issuer: "https://test.ruum.localhost".into(),
            subject: format!("{name}-{run}"),
            email: Some(format!("{name}-{run}@example.test")),
            email_verified: true,
            name: Some(name.into()),
            id_token: None,
        }
    }

    /// Alice's team with a folder `Data`; Bob is a guest with `edit` on it.
    pub(crate) async fn world() -> World {
        let (s, handles) = Services::in_memory_with_handles();
        let alice = s.identity.upsert_from_login(&login("alice")).await.unwrap();
        let bob = s.identity.upsert_from_login(&login("bob")).await.unwrap();
        let team = s
            .tenants
            .create_team(&Actor::user(alice.id), "Garden".into())
            .await
            .unwrap();
        let a = ctx(team.id, alice.id);
        let (_, token) = s
            .tenants
            .invite(&a, bob.email.clone(), MemberRole::Guest, None)
            .await
            .unwrap();
        s.tenants
            .accept_invitation(&Actor::user(bob.id), &token)
            .await
            .unwrap();
        let folder = s
            .nodes
            .create(
                &a,
                crate::nodes::NewNode {
                    parent_id: team.root_node_id,
                    name: "Data".into(),
                    kind: NodeKind::Folder,
                    node_type: None,
                    link: None,
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .unwrap()
            .value
            .id;
        s.sharing
            .add_grant(&a, folder, Grantee::User { id: bob.id }, Role::Edit)
            .await
            .unwrap();
        World {
            s,
            handles,
            team,
            alice: alice.id,
            bob: bob.id,
            folder,
        }
    }

    fn sqlite() -> Value {
        json!({ "engine": "sqlite", "version": "3", "plan": "shared-small" })
    }

    fn pg() -> Value {
        json!({ "engine": "postgres", "version": "17", "plan": "shared-small" })
    }

    fn create(parent: NodeId, name: &str, manifest: Value) -> CreateDatabase {
        CreateDatabase {
            parent_id: parent,
            name: name.into(),
            manifest,
            source: None,
            on_conflict: OnConflict::Fail,
        }
    }

    fn code<T>(r: Result<T>) -> (u16, String) {
        match r {
            Ok(_) => (200, String::new()),
            Err(e) => (e.http_status(), e.to_api().hint.unwrap_or_default()),
        }
    }

    /// Polls `getDatabase` until the state is not `provisioning` (at most 5 s).
    pub(crate) async fn settled(s: &Services, c: &Context, id: NodeId) -> Database {
        for _ in 0..200 {
            let db = s.databases.get(c, id).await.unwrap();
            if db.status.state != ProviderState::Provisioning {
                return db;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("database {id} is still provisioning after 5 s");
    }

    async fn agent_ctx(w: &World, user: UserId, max_role: Role) -> Context {
        let agent =
            w.s.agents
                .register(
                    &ctx(w.team.id, user),
                    NewAgent {
                        name: format!("agent {}", uuid::Uuid::now_v7().simple()),
                        description: None,
                        scope: AgentScope {
                            subtree: None,
                            max_role,
                            expires_at: None,
                            budget: Budget::default(),
                        },
                    },
                )
                .await
                .unwrap();
        Context {
            tenant: w.team.id,
            actor: Actor {
                actor: PrincipalRef {
                    kind: PrincipalKind::Agent,
                    id: agent.id.into(),
                },
                on_behalf_of: Some(user),
            },
            scope: Some(agent.scope.as_scope()),
            idempotency_key: None,
        }
    }

    #[tokio::test]
    async fn create_provisions_and_becomes_ready() {
        let w = world().await;
        let a = ctx(w.team.id, w.alice);
        let written =
            w.s.databases
                .create(&a, create(w.folder, "Plants", pg()))
                .await
                .unwrap();
        let db = written.value;
        assert_eq!(db.node.kind, NodeKind::Typed);
        assert_eq!(db.node.node_type.as_deref(), Some(NODE_TYPE));
        assert_eq!(db.manifest["extensions"], json!([]));
        assert_eq!(db.manifest["backups"], json!({ "pitr_days": 7 }));
        assert!(matches!(
            db.status.state,
            ProviderState::Provisioning | ProviderState::Ready
        ));
        assert!(written.change.seq.is_some());
        let id = db.node.id;
        let db = settled(&w.s, &a, id).await;
        assert_eq!(db.status.state, ProviderState::Ready);
        assert_eq!(db.status.observed_version_id, Some(db.manifest_version_id));
        assert!(db.status.message.is_none());
        let p = db.my_permissions.unwrap();
        assert!(p.change_schema && p.query_write && p.read_secret);
        // Team plan: 25 per member, 10 GB each.
        assert_eq!(db.limits.max_size_bytes, 10 * 1024 * 1024 * 1024);
        // The provider wrote the databases row and one role per kind.
        let row = w
            .handles
            .databases
            .get(w.team.id, id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.engine, EngineKind::Postgres);
        assert_eq!(row.key_node_id, None);
        assert!(row.engine_name.starts_with("d_"), "{}", row.engine_name);
        assert_eq!(
            w.handles.databases.roles(w.team.id, id).len(),
            ruum_dbengine::RoleKind::ALL.len()
        );
        assert!(
            !format!("{:?}", w.handles.databases.roles(w.team.id, id)).contains(&id.to_string()),
            "role names are neutral"
        );
        // The journal: node.created and version.created.
        let events: Vec<String> = w
            .handles
            .journal
            .entries(w.team.id)
            .into_iter()
            .filter(|e| e.node_id == Some(id))
            .map(|e| e.event.as_str().to_string())
            .collect();
        assert_eq!(events, ["node.created", "version.created"]);
        // The usage counts it per engine.
        let u = w.s.databases.usage(w.team.id).await.unwrap();
        assert_eq!((u.postgres.count, u.sqlite.count, u.branches), (1, 0, 0));
        // Another tenant does not find it.
        let other =
            w.s.identity
                .upsert_from_login(&login("carol"))
                .await
                .unwrap();
        let carol = ctx(other.home_tenant_id, other.id);
        assert_eq!(code(w.s.databases.get(&carol, id).await).0, 404);
        // A node that is not a database is 404.
        assert_eq!(code(w.s.databases.get(&a, w.folder).await).0, 404);
    }

    #[tokio::test]
    async fn the_manifest_changes_with_the_rules() {
        let w = world().await;
        let a = ctx(w.team.id, w.alice);
        let id =
            w.s.databases
                .create(&a, create(w.folder, "Plants", sqlite()))
                .await
                .unwrap()
                .value
                .node
                .id;
        let db = settled(&w.s, &a, id).await;
        let mut m = db.manifest.clone();
        m["data_classes"] = json!({ "main.plants.name": "normal" });
        // A stale If-Match: 412.
        let stale = Some(db.node.revision + 5);
        let req = |m: &Value| UpdateManifest {
            manifest: m.clone(),
            label: Some("classes".into()),
        };
        assert_eq!(
            code(w.s.databases.update_manifest(&a, id, req(&m), stale).await).0,
            412
        );
        let written =
            w.s.databases
                .update_manifest(&a, id, req(&m), Some(db.node.revision))
                .await
                .unwrap();
        let after = written.value;
        assert_ne!(after.manifest_version_id, db.manifest_version_id);
        assert_eq!(after.node.revision, db.node.revision + 1);
        // `listVersions` of the database: the manifest versions with the label, newest first
        // (review-D-1 B-1). Bob (edit on the folder) sees them too.
        let versions =
            w.s.databases
                .list_versions(&a, id, VersionFilter::default(), PageRequest::default())
                .await
                .unwrap();
        let listed: Vec<(VersionId, Option<&str>)> = versions
            .items
            .iter()
            .map(|v| (v.id, v.label.as_deref()))
            .collect();
        assert_eq!(
            listed,
            vec![
                (after.manifest_version_id, Some("classes")),
                (db.manifest_version_id, None)
            ]
        );
        assert!(
            versions
                .items
                .iter()
                .all(|v| v.actor.actor.id == w.alice.into())
        );
        let b = ctx(w.team.id, w.bob);
        assert_eq!(
            w.s.databases
                .list_versions(&b, id, VersionFilter::default(), PageRequest::default())
                .await
                .unwrap()
                .items
                .len(),
            2
        );
        assert!(w.s.databases.is_database(w.team.id, id).await.unwrap());
        assert!(
            !w.s.databases
                .is_database(w.team.id, w.folder)
                .await
                .unwrap()
        );
        assert_eq!(
            code(
                w.s.databases
                    .list_versions(
                        &a,
                        w.folder,
                        VersionFilter::default(),
                        PageRequest::default()
                    )
                    .await
            )
            .0,
            404
        );
        // A manifest checked against the old version, written after the new one came, is
        // refused (review-D-1 N-1): an agent without If-Match cannot revert a person's change.
        assert_eq!(
            code(
                w.handles
                    .databases
                    .put_manifest(&a, id, &db.manifest, None, db.manifest_version_id, None)
                    .await
            )
            .0,
            409
        );
        assert_eq!(
            w.s.databases.get(&a, id).await.unwrap().manifest_version_id,
            after.manifest_version_id
        );
        assert_eq!(after.manifest["data_classes"]["main.plants.name"], "normal");
        assert_eq!(after.status.state, ProviderState::Ready);
        assert_eq!(
            after.status.observed_version_id,
            Some(after.manifest_version_id)
        );
        // The engine and its version do not change; nor do the extensions.
        assert_eq!(
            code(
                w.s.databases
                    .update_manifest(&a, id, req(&pg()), None)
                    .await
            ),
            (422, "/engine: keep \"sqlite\"".into())
        );
        // An agent (max_role manage) may resend the manifest, not change the classes.
        let agent = agent_ctx(&w, w.alice, Role::Manage).await;
        assert!(
            w.s.databases
                .update_manifest(&agent, id, req(&after.manifest), None)
                .await
                .is_ok()
        );
        let mut other = after.manifest.clone();
        other["data_classes"] = json!({ "main.plants.name": "secret" });
        assert_eq!(
            code(
                w.s.databases
                    .update_manifest(&agent, id, req(&other), None)
                    .await
            ),
            (422, manifest::HINT_SUGGEST_CLASSES.into())
        );
        // An agent cannot create a database with classes either.
        let mut with = sqlite();
        with["data_classes"] = json!({ "main.t.c": "normal" });
        let edit_agent = agent_ctx(&w, w.alice, Role::Edit).await;
        assert_eq!(
            code(
                w.s.databases
                    .create(&edit_agent, create(w.folder, "Agent db", with))
                    .await
            ),
            (422, manifest::HINT_SUGGEST_CLASSES.into())
        );
        // Bob (edit) may not change the settings.
        let b = ctx(w.team.id, w.bob);
        assert_eq!(
            code(
                w.s.databases
                    .update_manifest(&b, id, req(&after.manifest), None)
                    .await
            )
            .0,
            403
        );
        // In the trash: suspended, and a manifest change is 409.
        w.s.nodes.trash(&a, id, None).await.unwrap();
        let trashed = w.s.databases.get(&a, id).await.unwrap();
        assert_eq!(trashed.status.state, ProviderState::Suspended);
        assert_eq!(trashed.status.reason.as_deref(), Some("in_trash"));
        assert_eq!(
            code(
                w.s.databases
                    .update_manifest(&a, id, req(&after.manifest), None)
                    .await
            )
            .0,
            409
        );
    }

    #[tokio::test]
    async fn guests_need_the_policy_and_agents_need_edit() {
        let w = world().await;
        let a = ctx(w.team.id, w.alice);
        let b = ctx(w.team.id, w.bob);
        assert_eq!(
            code(
                w.s.databases
                    .create(&b, create(w.folder, "Bob's", sqlite()))
                    .await
            ),
            (403, "guests_create_resources".into())
        );
        let bob_agent = agent_ctx(&w, w.bob, Role::Edit).await;
        assert_eq!(
            code(
                w.s.databases
                    .create(&bob_agent, create(w.folder, "Bob's agent", sqlite()))
                    .await
            ),
            (403, "guests_create_resources".into())
        );
        // Nothing was made.
        assert_eq!(
            w.s.databases.usage(w.team.id).await.unwrap().sqlite.count,
            0
        );
        w.s.tenants
            .update(
                &a,
                None,
                Some(TenantPolicyChange {
                    guests_create_resources: Some(true),
                    ..TenantPolicyChange::default()
                }),
            )
            .await
            .unwrap();
        assert!(
            w.s.databases
                .create(&b, create(w.folder, "Bob's", sqlite()))
                .await
                .is_ok()
        );
        assert!(
            w.s.databases
                .create(&bob_agent, create(w.folder, "Bob's agent", sqlite()))
                .await
                .is_ok()
        );
        // An agent with max_role view gets 403.
        let viewer = agent_ctx(&w, w.alice, Role::View).await;
        assert_eq!(
            code(
                w.s.databases
                    .create(&viewer, create(w.folder, "V", sqlite()))
                    .await
            )
            .0,
            403
        );
        // A template is not available yet: 422, no node.
        let mut req = create(w.folder, "From template", sqlite());
        req.source = Some(CreateSource {
            kind: "template".into(),
            node_id: Some(NodeId::new()),
        });
        assert_eq!(code(w.s.databases.create(&a, req).await).0, 422);
        assert_eq!(
            w.s.databases.usage(w.team.id).await.unwrap().sqlite.count,
            2
        );
    }

    #[tokio::test]
    async fn the_plan_limits_the_count_per_engine() {
        let w = world().await;
        // A personal space is on the free plan: 1 SQLite and 1 Postgres.
        let dora =
            w.s.identity
                .upsert_from_login(&login("dora"))
                .await
                .unwrap();
        let c = ctx(dora.home_tenant_id, dora.id);
        let root = w.s.nodes.root(&c).await.unwrap().id;
        assert!(
            w.s.databases
                .create(&c, create(root, "one", sqlite()))
                .await
                .is_ok()
        );
        assert_eq!(
            code(
                w.s.databases
                    .create(&c, create(root, "two", sqlite()))
                    .await
            )
            .0,
            403
        );
        let e =
            w.s.databases
                .create(&c, create(root, "two", sqlite()))
                .await
                .unwrap_err();
        assert_eq!(e.to_api().code, ErrorCode::PlanLimitReached);
        assert!(
            w.s.databases
                .create(&c, create(root, "pg", pg()))
                .await
                .is_ok()
        );
        let u = w.s.databases.usage(dora.home_tenant_id).await.unwrap();
        assert_eq!((u.postgres.count, u.sqlite.count), (1, 1));
    }

    #[tokio::test]
    async fn an_engine_that_the_server_lacks_makes_no_node() {
        let mut w = world().await;
        let parts = DatabaseParts {
            engines: Engines::none(),
            keys: w.s.tenant_keys.clone(),
            store: w.handles.databases.clone(),
            typed: w.handles.typed.clone(),
            columns: w.s.databases.deps().columns.clone(),
            masking: w.s.databases.deps().masking.clone(),
            grants: w.s.databases.deps().grants.clone(),
            queries: Arc::new(query::MemoryQueryLog::new()),
        };
        install(&mut w.s, parts).unwrap();
        let a = ctx(w.team.id, w.alice);
        for (m, hint) in [
            (sqlite(), "/engine: use engine postgres"),
            (pg(), "/engine: this server"),
        ] {
            let (status, h) = code(w.s.databases.create(&a, create(w.folder, "X", m)).await);
            assert_eq!(status, 422);
            assert!(h.starts_with(hint), "{h}");
        }
        let children =
            w.s.nodes
                .list_children(&a, w.folder, Default::default(), Default::default())
                .await
                .unwrap();
        assert!(children.items.is_empty(), "no node was made");
    }

    #[tokio::test]
    async fn purged_drops_the_database_and_the_key() {
        let w = world().await;
        let a = ctx(w.team.id, w.alice);
        let id =
            w.s.databases
                .create(&a, create(w.folder, "Short lived", sqlite()))
                .await
                .unwrap()
                .value
                .node
                .id;
        settled(&w.s, &a, id).await;
        let provider = w.s.databases.deps().provider.clone();
        assert!(provider.holds_key(w.team.id, id));
        let row = w
            .handles
            .databases
            .get(w.team.id, id)
            .await
            .unwrap()
            .unwrap();
        let engine = w.s.database_engines.get(row.engine).unwrap().clone();
        assert!(engine.size_bytes(&row.database_ref()).await.is_ok());
        let hook = |kind| HookDelivery {
            hook: kind,
            tenant_id: w.team.id,
            node_id: Some(id),
            seq: Some(1),
            target_node_id: None,
            data: None,
            by: Actor::user(w.alice).into(),
            at: crate::now(),
        };
        provider
            .deliver_hook(crate::providers::DeliveryId::new(), hook(HookKind::Purged))
            .await
            .unwrap();
        assert!(!provider.holds_key(w.team.id, id));
        assert!(
            w.handles
                .databases
                .get(w.team.id, id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            engine.size_bytes(&row.database_ref()).await,
            Err(ruum_dbengine::EngineError::NotFound)
        ));
        // Again: idempotent.
        assert!(
            provider
                .deliver_hook(crate::providers::DeliveryId::new(), hook(HookKind::Purged))
                .await
                .is_ok()
        );
        // A copy of the purged database makes nothing (card P-3 builds copies: the hook starts
        // the work, and the copy fails without a source).
        let mut copied = hook(HookKind::Copied);
        let target = NodeId::new();
        copied.target_node_id = Some(target);
        assert!(
            !provider
                .deliver_hook(crate::providers::DeliveryId::new(), copied)
                .await
                .unwrap()
                .is_done()
        );
        for _ in 0..100 {
            if provider
                .get_typed_node_status(w.team.id, target)
                .await
                .is_ok_and(|s| s.state != ProviderState::Provisioning)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            provider
                .get_typed_node_status(w.team.id, target)
                .await
                .unwrap()
                .state,
            ProviderState::Failed
        );
        assert!(
            w.handles
                .databases
                .get(w.team.id, target)
                .await
                .unwrap()
                .is_none()
        );
        for source in [
            Source::Copy {
                node_id: id,
                data: Some(DataCopy::Full),
            },
            Source::Branch {
                node_id: id,
                data: DataCopy::Full,
            },
        ] {
            let r = provider
                .create_typed_node(
                    crate::providers::DeliveryId::new(),
                    CreateTypedNodeRequest {
                        tenant_id: w.team.id,
                        node_id: NodeId::new(),
                        parent_id: None,
                        type_name: NODE_TYPE.into(),
                        version_id: VersionId::new(),
                        manifest: sqlite(),
                        source,
                        by: Actor::user(w.alice).into(),
                    },
                )
                .await;
            assert_eq!(code(r).0, 422);
        }
    }

    #[tokio::test]
    async fn the_reconciler_sends_a_lost_create_again() {
        let w = world().await;
        let a = ctx(w.team.id, w.alice);
        // A node whose create never reached the provider: made through the node store only.
        let manifest = manifest::check(&sqlite(), None, false).unwrap();
        let node =
            w.s.nodes
                .create_typed(
                    &a,
                    NewTyped {
                        parent_id: w.folder,
                        name: "Lost".into(),
                        node_type: NODE_TYPE.into(),
                        provider: PROVIDER_ID.into(),
                        manifest: manifest.clone(),
                        on_conflict: OnConflict::Fail,
                    },
                )
                .await
                .unwrap()
                .value;
        w.handles
            .databases
            .created(&node, node.head_version.unwrap(), &manifest);
        let rec = crate::providers::reconciler::Reconciler::new(
            w.s.typed_nodes.clone(),
            w.s.providers.clone(),
        )
        .settled(Duration::ZERO);
        let round = rec.run_once().await.unwrap();
        assert_eq!(round.created, 1, "{round:?}");
        let db = settled(&w.s, &a, node.id).await;
        assert_eq!(db.status.state, ProviderState::Ready);
        assert!(
            w.handles
                .databases
                .get(w.team.id, node.id)
                .await
                .unwrap()
                .is_some()
        );
        // Nothing more to do.
        let round = rec.run_once().await.unwrap();
        assert_eq!((round.created, round.applied), (0, 0), "{round:?}");
    }

    // ------------------------------------------------------------ the Postgres store

    /// A tenant with a user, a root folder and a database node (`nodes`, `versions`,
    /// `typed_nodes`), written as the system. Returns (tenant, node, version).
    async fn seed_pg_database(pool: &sqlx::PgPool, engine: &str) -> (TenantId, NodeId, VersionId) {
        let (t, u, root, node, v) = (
            TenantId::new(),
            UserId::new(),
            NodeId::new(),
            NodeId::new(),
            VersionId::new(),
        );
        let manifest = json!({ "engine": engine, "version": if engine == "sqlite" { "3" } else { "17" }, "plan": "shared-small" });
        let (bytes, hash) = crate::nodes::typed_manifest_bytes(&manifest);
        let mut tx = pool.begin().await.unwrap();
        for sql in [
            "set local ruum.system = 'on'",
            "set constraints all deferred",
        ] {
            sqlx::query(sql).execute(&mut *tx).await.unwrap();
        }
        sqlx::query(
            "insert into tenants (id, name, kind, plan, root_node_id, created_kind, created_by) \
             values ($1, 't', 'team', 'team', $2, 'user', $3)",
        )
        .bind(t)
        .bind(root)
        .bind(u.0)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query(
            "insert into users (id, tenant_id, email, email_verified, display_name) \
             values ($1, $2, $3, true, 'x')",
        )
        .bind(u.0)
        .bind(t)
        .bind(format!("{}@example.test", u.0))
        .execute(&mut *tx)
        .await
        .unwrap();
        for (id, parent, kind, ty, head) in [
            (root, None, "folder", None, None),
            (node, Some(root), "typed", Some(NODE_TYPE), Some(v)),
        ] {
            sqlx::query(
                "insert into nodes (id, tenant_id, parent_id, name, kind, type, head_version, \
                 owner_id, created_kind, created_by, updated_kind, updated_by) \
                 values ($1, $2, $3, $4, $5::node_kind, $6, $7, $8, 'user', $8, 'user', $8)",
            )
            .bind(id)
            .bind(t)
            .bind(parent)
            .bind(id.to_string())
            .bind(kind)
            .bind(ty)
            .bind(head)
            .bind(u.0)
            .execute(&mut *tx)
            .await
            .unwrap();
        }
        sqlx::query(
            "insert into versions (id, tenant_id, node_id, manifest, encoding, size_bytes, \
             content_hash, actor_kind, actor) values ($1, $2, $3, $4, 'typed_manifest_v1', $5, $6, \
             'user', $7)",
        )
        .bind(v)
        .bind(t)
        .bind(node)
        .bind(&bytes)
        .bind(bytes.len() as i64)
        .bind(hash.to_vec())
        .bind(u.0)
        .execute(&mut *tx)
        .await
        .unwrap();
        crate::providers::postgres::insert_typed_node(
            &mut tx,
            &crate::providers::store::NewTypedNode {
                tenant_id: t,
                node_id: node,
                provider: PROVIDER_ID.into(),
                type_name: NODE_TYPE.into(),
                desired_version: v,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (t, node, v)
    }

    fn row(t: TenantId, node: NodeId, engine: EngineKind) -> DatabaseRow {
        DatabaseRow {
            tenant_id: t,
            node_id: node,
            engine,
            engine_version: if engine == EngineKind::Sqlite {
                "3"
            } else {
                "17"
            }
            .into(),
            cluster: "memory".into(),
            engine_name: format!("d_{}", &uuid::Uuid::now_v7().simple().to_string()[16..]),
            branch_of: None,
            branch_data: None,
            key_node_id: None,
            extensions: Vec::new(),
            size_bytes: 4096,
        }
    }

    /// The Postgres store keeps every row with its tenant, with row-level security off, so only
    /// the tenant filters of its statements hold (the mutation
    /// `databases-tenant-filter-postgres` breaks one).
    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_store_filters_by_tenant_without_rls(pool: sqlx::PgPool) {
        for table in [
            "databases",
            "database_roles",
            "typed_nodes",
            "versions",
            "nodes",
        ] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "alter table {table} no force row level security"
            )))
            .execute(&pool)
            .await
            .unwrap();
        }
        let store = PgDatabaseStore::new(pool.clone());
        let (a, na, va) = seed_pg_database(&pool, "postgres").await;
        let (b, nb, _) = seed_pg_database(&pool, "sqlite").await;
        let roles = vec![EngineRole {
            kind: ruum_dbengine::RoleKind::Read,
            binding: None,
            role_name: format!("r_{}", &uuid::Uuid::now_v7().simple().to_string()[16..]),
        }];
        // Before the provider writes the row, the node still counts (by its manifest).
        let u = store.usage(a).await.unwrap();
        assert_eq!((u.postgres.count, u.sqlite.count), (1, 0));
        store
            .insert(&row(a, na, EngineKind::Postgres), &roles)
            .await
            .unwrap();
        assert_eq!(
            code(store.insert(&row(a, na, EngineKind::Postgres), &[]).await).0,
            409
        );
        store
            .insert(&row(b, nb, EngineKind::Sqlite), &[])
            .await
            .unwrap();
        assert_eq!(
            store.get(a, na).await.unwrap().unwrap().engine,
            EngineKind::Postgres
        );
        // Tenant B never reads, lists, counts or deletes A's row, and the other way round.
        assert!(store.get(b, na).await.unwrap().is_none());
        assert!(store.get(a, nb).await.unwrap().is_none());
        assert_eq!(store.list_tenant(b).await.unwrap().len(), 1);
        assert_eq!(store.list_tenant(b).await.unwrap()[0].node_id, nb);
        let ua = store.usage(a).await.unwrap();
        assert_eq!(
            (ua.postgres.count, ua.postgres.size_bytes, ua.sqlite.count),
            (1, 4096, 0)
        );
        let ub = store.usage(b).await.unwrap();
        assert_eq!((ub.postgres.count, ub.sqlite.count), (0, 1));
        assert!(store.manifest(b, va).await.unwrap().is_none());
        assert_eq!(
            store.manifest(a, va).await.unwrap().unwrap()["engine"],
            "postgres"
        );
        assert!(store.delete(b, na).await.unwrap().is_none());
        assert!(store.get(a, na).await.unwrap().is_some());
        assert_eq!(store.delete(a, na).await.unwrap().unwrap().node_id, na);
        assert!(store.get(a, na).await.unwrap().is_none());
        let roles_left: i64 =
            sqlx::query_scalar("select count(*) from database_roles where node_id = $1")
                .bind(na)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(roles_left, 0);
        assert!(store.get(b, nb).await.unwrap().is_some());
    }

    /// `put_manifest` on Postgres: a new head version, the desired version of the row, the
    /// journal entry; `If-Match`, the trash and other tenants are refused.
    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_put_manifest_writes_one_unit(pool: sqlx::PgPool) {
        let store = PgDatabaseStore::new(pool.clone());
        let typed = crate::providers::postgres::PgTypedNodeStore::new(pool.clone());
        let (a, node, v1) = seed_pg_database(&pool, "sqlite").await;
        let (b, _, _) = seed_pg_database(&pool, "sqlite").await;
        let user: uuid::Uuid = {
            let mut tx = crate::journal::postgres::begin_tenant(&pool, a)
                .await
                .unwrap();
            sqlx::query_scalar("select owner_id from nodes where tenant_id = $1 and id = $2")
                .bind(a)
                .bind(node)
                .fetch_one(&mut *tx)
                .await
                .unwrap()
        };
        let c = ctx(a, UserId(user));
        let m = manifest::check(&sqlite(), None, false).unwrap();
        assert_eq!(
            code(store.put_manifest(&c, node, &m, None, v1, Some(9)).await).0,
            412
        );
        assert_eq!(
            code(
                store
                    .put_manifest(&ctx(b, UserId(user)), node, &m, None, v1, None)
                    .await
            )
            .0,
            404
        );
        let w = store
            .put_manifest(&c, node, &m, Some("same"), v1, Some(1))
            .await
            .unwrap();
        let v2 = w.value;
        assert!(w.change.seq.is_some());
        // A manifest checked against v1 after v2 came is refused, and writes nothing
        // (review-D-1 N-1).
        assert_eq!(
            code(store.put_manifest(&c, node, &m, None, v1, None).await).0,
            409
        );
        // `listVersions` of the node: both manifest versions, newest first, with the label;
        // other tenants see none.
        let page = store
            .list_versions(a, node, VersionFilter::default(), PageRequest::default())
            .await
            .unwrap();
        let listed: Vec<(VersionId, Option<&str>)> = page
            .items
            .iter()
            .map(|v| (v.id, v.label.as_deref()))
            .collect();
        assert_eq!(listed, vec![(v2, Some("same")), (v1, None)]);
        assert_eq!(page.items[0].actor.actor.id.0, user);
        let labeled = store
            .list_versions(
                a,
                node,
                VersionFilter {
                    actor_kind: None,
                    labeled: Some(true),
                },
                PageRequest {
                    cursor: None,
                    limit: Some(1),
                },
            )
            .await
            .unwrap();
        assert_eq!(labeled.items.len(), 1);
        assert_eq!(labeled.items[0].id, v2);
        let first = store
            .list_versions(
                a,
                node,
                VersionFilter::default(),
                PageRequest {
                    cursor: None,
                    limit: Some(1),
                },
            )
            .await
            .unwrap();
        assert_eq!(first.items[0].id, v2);
        let second = store
            .list_versions(
                a,
                node,
                VersionFilter::default(),
                PageRequest {
                    cursor: first.next_cursor.clone(),
                    limit: Some(1),
                },
            )
            .await
            .unwrap();
        assert_eq!(second.items[0].id, v1);
        assert!(second.next_cursor.is_none());
        assert!(
            store
                .list_versions(b, node, VersionFilter::default(), PageRequest::default())
                .await
                .unwrap()
                .items
                .is_empty()
        );
        let row = typed.get(a, node).await.unwrap().unwrap();
        assert_eq!(row.desired_version, v2);
        assert_eq!(store.manifest(a, v2).await.unwrap().unwrap(), m);
        let mut tx = crate::journal::postgres::begin_tenant(&pool, a)
            .await
            .unwrap();
        let (head, revision, label): (VersionId, i64, Option<String>) = sqlx::query_as(
            "select n.head_version, n.revision, v.label from nodes n \
             join versions v on v.tenant_id = n.tenant_id and v.id = n.head_version \
             where n.tenant_id = $1 and n.id = $2",
        )
        .bind(a)
        .bind(node)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        let data: Value = sqlx::query_scalar(
            "select data from journal where tenant_id = $1 and node_id = $2 \
             and type = 'version.created' order by seq desc limit 1",
        )
        .bind(a)
        .bind(node)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        sqlx::query(
            "update nodes set trashed_at = now(), trash_root = id where tenant_id = $1 and id = $2",
        )
        .bind(a)
        .bind(node)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!((head, revision, label.as_deref()), (v2, 2, Some("same")));
        assert_eq!(data["previous_version_id"], json!(v1));
        assert_eq!(data["version_id"], json!(v2));
        assert_eq!(
            code(store.put_manifest(&c, node, &m, None, v2, None).await).0,
            409
        );
    }

    // ------------------------------------------------------- the key on the Postgres engine

    struct AllPersonal;

    #[async_trait]
    impl ruum_dbengine::CatalogSync for AllPersonal {
        async fn sync(
            &self,
            _catalog: &ruum_dbengine::Catalog,
        ) -> std::result::Result<ruum_dbengine::LensSpec, ruum_dbengine::EngineError> {
            // No column named: every column counts as personal (fails safe).
            Ok(ruum_dbengine::LensSpec::default())
        }
    }

    /// The pseudonym of `alice@example.com` as an agent without `read_personal` reads it from a
    /// database that the provider made (masked lens).
    async fn pseudonym_in(s: &Services, tenant: TenantId, id: NodeId) -> String {
        use ruum_dbengine::*;
        let row = s
            .databases
            .deps()
            .store
            .get(tenant, id)
            .await
            .unwrap()
            .unwrap();
        let engine = s.database_engines.get(row.engine).unwrap().clone();
        let key = s.tenant_keys.engine_key(tenant, None).await.unwrap();
        engine
            .apply_migration(
                &row.database_ref(),
                &key,
                &MigrationRequest {
                    up: "CREATE TABLE people (email text); \
                         INSERT INTO people VALUES ('alice@example.com');"
                        .into(),
                    timeout: Duration::from_secs(30),
                    dry_run: false,
                },
                &AllPersonal,
            )
            .await
            .unwrap();
        let out = engine
            .execute(
                &RoleRef::new(row.database_ref(), RoleKind::MaskedRead).unwrap(),
                &key,
                &StatementRequest {
                    sql: "SELECT email FROM people".into(),
                    params: vec![],
                    mode: StatementMode::Read,
                    timeout: Duration::from_secs(30),
                    max_rows: 10,
                    max_bytes: None,
                },
            )
            .await
            .unwrap();
        match &out.rows[0][0] {
            ruum_dbengine::Value::Text(t) => t.clone(),
            other => panic!("not a pseudonym: {other:?}"),
        }
    }

    /// Decision 0004-30, card D-1: `createDatabase` loads the tenant key through Q-4 and the
    /// engine installs it. Two databases of one tenant give the same pseudonym for the same
    /// value; a database of another tenant gives another one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn databases_of_a_tenant_share_the_tenant_key() {
        // In CI, and with RUUM_DBENGINE_REQUIRE_POSTGRES (the mutation check), the cluster is
        // required: a skip would let a broken key path pass.
        let required = std::env::var_os("CI").is_some()
            || std::env::var_os("RUUM_DBENGINE_REQUIRE_POSTGRES").is_some();
        let Some(config) = ruum_dbengine::postgres::PostgresConfig::from_env() else {
            assert!(
                !required,
                "RUUM_DBHOST_URL is not set: this test needs the data cluster"
            );
            eprintln!("skipped: RUUM_DBHOST_URL is not set");
            return;
        };
        let engine = match ruum_dbengine::postgres::PostgresEngine::connect(config).await {
            Ok(e) => Arc::new(e) as Arc<dyn DatabaseEngine>,
            Err(e) => {
                assert!(!required, "the data cluster: {e}");
                eprintln!("skipped: the data cluster does not answer: {e}");
                return;
            }
        };
        let mut w = world().await;
        let parts = DatabaseParts {
            engines: Engines {
                postgres: Some(engine.clone()),
                sqlite: None,
            },
            keys: w.s.tenant_keys.clone(),
            store: w.handles.databases.clone(),
            typed: w.handles.typed.clone(),
            columns: w.s.databases.deps().columns.clone(),
            masking: w.s.databases.deps().masking.clone(),
            grants: w.s.databases.deps().grants.clone(),
            queries: Arc::new(query::MemoryQueryLog::new()),
        };
        install(&mut w.s, parts).unwrap();
        let a = ctx(w.team.id, w.alice);
        let mut ids = Vec::new();
        for name in ["One", "Two"] {
            let id =
                w.s.databases
                    .create(&a, create(w.folder, name, pg()))
                    .await
                    .unwrap()
                    .value
                    .node
                    .id;
            assert_eq!(
                settled(&w.s, &a, id).await.status.state,
                ProviderState::Ready
            );
            ids.push((w.team.id, id));
        }
        // Another tenant: Bob's personal space.
        let bob_home =
            w.s.identity
                .upsert_from_login(&login("frank"))
                .await
                .unwrap();
        let f = ctx(bob_home.home_tenant_id, bob_home.id);
        let root = w.s.nodes.root(&f).await.unwrap().id;
        let other =
            w.s.databases
                .create(&f, create(root, "Three", pg()))
                .await
                .unwrap()
                .value
                .node
                .id;
        assert_eq!(
            settled(&w.s, &f, other).await.status.state,
            ProviderState::Ready
        );
        ids.push((bob_home.home_tenant_id, other));
        let mut p = Vec::new();
        for (t, id) in &ids {
            p.push(pseudonym_in(&w.s, *t, *id).await);
        }
        assert!(
            crate::privacy::Pseudonym::parse(&p[0]).is_some(),
            "a masked read gives a pseudonym: {}",
            p[0]
        );
        assert_ne!(p[0], "alice@example.com");
        assert_eq!(
            p[0], p[1],
            "two databases of one tenant: the same pseudonym"
        );
        assert_ne!(p[0], p[2], "two tenants: different pseudonyms");
        // The pseudonym is the tenant key's (the Rust pseudonym of the same value).
        let key = w.s.tenant_keys.database_key(w.team.id, None).await.unwrap();
        let expected = crate::privacy::HmacPseudonymizer::new(key)
            .pseudonym_of_canonical("alice@example.com")
            .id()
            .to_string();
        assert_eq!(p[0], expected);
        for (t, id) in ids {
            let row = w.handles.databases.get(t, id).await.unwrap().unwrap();
            let _ = engine.drop_database(&row.database_ref()).await;
        }
    }
}
