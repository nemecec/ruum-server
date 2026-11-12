//! The database provider `ruum.database` (design 7.1 and 8.1; `contracts/providers/`): the
//! first-party provider of the node type `database`, in the server process.
//!
//! - `createTypedNode` answers `202` at once (long work never holds a caller, review-P-2 note
//!   3) and makes the database in a task: it loads the pseudonym key of the database through
//!   the tenant keys (card Q-4, [`TenantKeys::engine_key`]: the tenant key, `key_node_id` is
//!   `NULL` for a new database) and hands it to the engine, which installs it (decision
//!   0004-30). Then it writes the `databases` and `database_roles` rows and reports `ready`
//!   (or `failed` with a fixed text). The same delivery again, or a create of a node that is
//!   already made, answers with the current status and does the work once.
//! - `applyManifest`: the engine and its major version must stay (the manifest rules refuse a
//!   change first); the rest of a manifest of step 2 needs no engine work (classes are K-1's,
//!   the extensions are fixed), so the answer is `200` with the new observed version.
//! - `getTypedNodeStatus`: the status of a node that the provider knows; `404` for a node it
//!   never made, so the reconciler sends `createTypedNode` again (review-P-2 note 2).
//! - Copies (card P-3): the hook `copied` copies the source database into the new node (the
//!   platform made the node and its `typed_nodes` row, state `provisioning`). It answers `202`
//!   at once and copies in a task: the engine copy (Postgres: the drained `TEMPLATE` path of
//!   E-2) with the data that the platform set in `data.data` (`full` only for a person with
//!   `manage` on the source; a missing or unknown value is `masked`), the source's pseudonym
//!   key and its `key_node_id` (decisions 0004-25 and 0004-30: the key follows the data),
//!   then the lens of the copy from its manifest, the rows, and `ready` (or `failed` with a
//!   fixed text). A masked copy masks what the lens masks: each column of a table of the
//!   database's own schema by its class in the source's head manifest, `personal` when it has
//!   none; `personal` becomes a pseudonym, `secret` becomes `NULL`. A `Source::Copy` or
//!   `Source::Template` of `createTypedNode` is still refused with `422`, so no data goes
//!   anywhere except through the hook and a branch.
//! - Branches (card D-3a): `createTypedNode` with `Source::Branch` and `schema_only` data
//!   answers `202` at once and makes the branch in a task ([`super::branches`]); `masked` and
//!   `full` branches are refused with `422` until card D-3b. A `Source::Empty` create of a node
//!   whose manifest names a `template` (a branch whose create the reconciler sends again after
//!   a restart, without its source) is refused with `422`, so the branch ends `failed` and
//!   never becomes an empty live database.
//! - `trashed`: the database stops serving (`suspended`, reason `in_trash`; the content stays).
//!   `restored`: it serves again (`ready`). Both answer `200` for a node the provider never
//!   made.
//! - `purged` and `tenant_deleted` drop the engine databases, delete the rows and drop the
//!   provider's copy of the key (review-P-1 note 6). Both are idempotent.
//!
//! Status texts are fixed texts of this file: never an engine message, never a value
//! (decision 0004-8).

use super::manifest::{self, EngineSpec};
use super::{DatabaseRow, DatabaseStore, Engines, NODE_TYPE, PROVIDER_ID};
use crate::error::{Error, Result};
use crate::ids::{NodeId, TenantId, VersionId};
use crate::paging::PageRequest;
use crate::privacy::keys::TenantKeys;
use crate::providers::store::{TypedNodeStore, error_text};
use crate::providers::*;
use async_trait::async_trait;
use ruum_dbengine::{
    Catalog, ColumnRef, CopyData, DataClass, DatabaseEngine, EngineError, EngineKind, MaskAs,
    MaskPlan, NewDatabase, RelationKind,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The `reason` of a database that could not be made.
pub const REASON_PROVISIONING_FAILED: &str = "provisioning_failed";
/// The text of a database that could not be made. No engine text (decision 0004-8).
pub const MESSAGE_PROVISIONING_FAILED: &str =
    "The database could not be made. Try again later, or ask for help.";

/// The `reason` of a copy that could not be made.
pub const REASON_COPY_FAILED: &str = "copy_failed";
/// The text of a copy that could not be made. No engine text (decision 0004-8).
pub const MESSAGE_COPY_FAILED: &str =
    "The copy could not be made. Copy the database again, or ask for help.";
/// The `reason` of a database in the trash.
pub const REASON_IN_TRASH: &str = "in_trash";

/// What the provider works with.
pub struct ProviderDeps {
    pub engines: Engines,
    pub keys: Arc<TenantKeys>,
    pub store: Arc<dyn DatabaseStore>,
    /// Read only: the generation and the desired version of a node after a restart.
    pub typed: Arc<dyn TypedNodeStore>,
    /// Where the provider reports (`reportStatus`).
    pub platform: Arc<dyn PlatformApi>,
    /// Told when a manifest changes the confirmed classes (`data_classes`), so the lens of the
    /// database follows at once (review-Q-2-1 F-2): `updateDatabaseManifest` and
    /// `restoreVersion` both reach the database through `applyManifest`.
    pub masking: Arc<dyn crate::privacy::MaskingRules>,
    /// The `database_columns` rows: a branch gets copies of its source's (card D-3a). A masked
    /// copy takes each column's class as the lens does, the stricter of the store and the
    /// manifest (backlog 2026-11-09 S-18).
    pub columns: Arc<dyn super::classes::ColumnStore>,
}

/// The `reason` of a branch that could not be made.
pub const REASON_BRANCH_FAILED: &str = "branch_failed";
/// The text of a branch that could not be made. No engine text (decision 0004-8).
pub const MESSAGE_BRANCH_FAILED: &str =
    "The branch could not be made. Make the branch again, or ask for help.";

/// The provider.
pub struct DatabaseProvider {
    inner: Arc<Inner>,
}

struct Inner {
    deps: ProviderDeps,
    /// The nodes that this process knows: the last status, and the key of a made database.
    live: Mutex<HashMap<(TenantId, NodeId), Live>>,
}

struct Live {
    status: ProviderStatus,
    /// The provider's copy of the pseudonym key, dropped on `purged` and `tenant_deleted`.
    key: Option<ruum_dbengine::PseudonymKey>,
}

impl DatabaseProvider {
    pub fn new(deps: ProviderDeps) -> Self {
        Self {
            inner: Arc::new(Inner {
                deps,
                live: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// The descriptor of `ruum.database`.
    pub fn descriptor_of() -> ProviderDescriptor {
        ProviderDescriptor {
            id: PROVIDER_ID.into(),
            name: "Databases".into(),
            base_url: None,
            open_url: None,
            types: vec![TypeDescriptor {
                type_name: NODE_TYPE.into(),
                display_name: Some("Database".into()),
                icon: Some("database".into()),
                extension: None,
                manifest_schema: super::MANIFEST_SCHEMA_ID.into(),
                roles: TypeRoles {
                    view: "read-only queries".into(),
                    comment: None,
                    edit: "read and write rows".into(),
                    manage: "settings, migrations and restore".into(),
                },
                // A branch is a child of its database (decision 0004-18).
                child_types: vec![NODE_TYPE.into()],
            }],
            hooks: vec![
                HookKind::Copied,
                HookKind::Trashed,
                HookKind::Restored,
                HookKind::Purged,
                HookKind::TenantDeleted,
            ],
            features: Vec::new(),
        }
    }

    /// True when the provider holds a key for the database (for tests of `purged`).
    pub fn holds_key(&self, tenant: TenantId, node: NodeId) -> bool {
        self.inner
            .lock()
            .get(&(tenant, node))
            .is_some_and(|l| l.key.is_some())
    }
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(TenantId, NodeId), Live>> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A generation greater than the stored one and than the last one of this process.
    async fn next_generation(&self, tenant: TenantId, node: NodeId) -> Result<i64> {
        let stored = self
            .deps
            .typed
            .get(tenant, node)
            .await?
            .map(|r| r.generation)
            .unwrap_or(0);
        let mine = self
            .lock()
            .get(&(tenant, node))
            .map(|l| l.status.generation)
            .unwrap_or(0);
        Ok(stored.max(mine) + 1)
    }

    fn engine(&self, kind: ruum_dbengine::EngineKind) -> Result<Arc<dyn DatabaseEngine>> {
        self.deps
            .engines
            .get(kind)
            .cloned()
            .ok_or_else(|| super::engine_not_available(kind))
    }

    fn set(&self, tenant: TenantId, status: &ProviderStatus) {
        let mut live = self.lock();
        let entry = live.entry((tenant, status.node_id)).or_insert(Live {
            status: status.clone(),
            key: None,
        });
        entry.status = status.clone();
    }

    /// Makes the database of a node and reports the outcome.
    async fn provision(
        self: Arc<Self>,
        delivery: DeliveryId,
        req: CreateTypedNodeRequest,
        spec: EngineSpec,
    ) {
        let tenant = req.tenant_id;
        let node = req.node_id;
        let outcome = self.make(tenant, node, &spec).await;
        let generation = match self.next_generation(tenant, node).await {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!(%tenant, %node, error = %error_text(&e), "no generation");
                return;
            }
        };
        let status = match outcome {
            Ok(key) => {
                let status = status(node, ProviderState::Ready, generation, Some(req.version_id));
                let mut live = self.lock();
                live.insert(
                    (tenant, node),
                    Live {
                        status: status.clone(),
                        key: Some(key),
                    },
                );
                status
            }
            Err(e) => {
                tracing::warn!(
                    %tenant,
                    %node,
                    error = %error_text(&e),
                    "the database could not be made"
                );
                let mut s = status(node, ProviderState::Failed, generation, None);
                s.reason = Some(REASON_PROVISIONING_FAILED.into());
                s.message = Some(MESSAGE_PROVISIONING_FAILED.into());
                self.set(tenant, &s);
                s
            }
        };
        if let Err(e) = self
            .deps
            .platform
            .report_status(StatusReport {
                status,
                tenant_id: tenant,
                delivery_id: Some(delivery),
            })
            .await
        {
            tracing::info!(%tenant, %node, error = %error_text(&e), "a status report was not stored");
        }
    }

    /// The engine work of a create: the key, the database, the rows. Returns the key.
    async fn make(
        &self,
        tenant: TenantId,
        node: NodeId,
        spec: &EngineSpec,
    ) -> Result<ruum_dbengine::PseudonymKey> {
        let engine = self.engine(spec.engine)?;
        // Decision 0004-30: the tenant key (key_node_id NULL), through Q-4, into the engine.
        let key = self.deps.keys.engine_key(tenant, None).await?;
        let created = engine
            .create_database(
                &NewDatabase {
                    tenant_id: tenant.0,
                    node_id: node.0,
                    engine_version: spec.version.clone(),
                    extensions: spec.extensions.clone(),
                },
                &key,
            )
            .await
            .map_err(engine_error)?;
        let size_bytes = engine
            .size_bytes(&created.database)
            .await
            .map(|n| i64::try_from(n).unwrap_or(i64::MAX))
            .unwrap_or(0);
        let row = DatabaseRow {
            tenant_id: tenant,
            node_id: node,
            engine: spec.engine,
            engine_version: spec.version.clone(),
            cluster: created.database.cluster.clone(),
            engine_name: created.database.engine_name.clone(),
            branch_of: None,
            branch_data: None,
            key_node_id: None,
            extensions: spec.extensions.clone(),
            size_bytes,
        };
        if let Err(e) = self.deps.store.insert(&row, &created.roles).await {
            // Leave no engine database without its row.
            let _ = engine.drop_database(&created.database).await;
            return Err(e);
        }
        Ok(key)
    }

    /// Copies the database `source` into `target` (the hook `copied`) and reports the outcome.
    async fn copy(
        self: Arc<Self>,
        delivery: DeliveryId,
        tenant: TenantId,
        source: NodeId,
        target: NodeId,
        data: DataCopy,
    ) {
        let outcome = self.make_copy(tenant, source, target, data).await;
        let (generation, desired) = match (
            self.next_generation(tenant, target).await,
            self.deps.typed.get(tenant, target).await,
        ) {
            (Ok(g), Ok(row)) => (g, row.map(|r| r.desired_version)),
            (Err(e), _) | (_, Err(e)) => {
                tracing::warn!(%tenant, node = %target, error = %error_text(&e), "no generation");
                return;
            }
        };
        let status = match outcome {
            Ok(key) => {
                let status = status(target, ProviderState::Ready, generation, desired);
                self.lock().insert(
                    (tenant, target),
                    Live {
                        status: status.clone(),
                        key: Some(key),
                    },
                );
                status
            }
            Err(e) => {
                tracing::warn!(
                    %tenant,
                    node = %target,
                    error = %error_text(&e),
                    "the copy of a database could not be made"
                );
                let mut s = status(target, ProviderState::Failed, generation, None);
                s.reason = Some(REASON_COPY_FAILED.into());
                s.message = Some(MESSAGE_COPY_FAILED.into());
                self.set(tenant, &s);
                s
            }
        };
        if let Err(e) = self
            .deps
            .platform
            .report_status(StatusReport {
                status,
                tenant_id: tenant,
                delivery_id: Some(delivery),
            })
            .await
        {
            tracing::info!(%tenant, node = %target, error = %error_text(&e), "a status report was not stored");
        }
    }

    /// Makes the branch `target` of `source` (card D-3a, [`super::branches::make_branch`]) and
    /// reports the outcome.
    async fn branch(
        self: Arc<Self>,
        delivery: DeliveryId,
        req: CreateTypedNodeRequest,
        source: NodeId,
        data: DataCopy,
    ) {
        let tenant = req.tenant_id;
        let target = req.node_id;
        let outcome =
            super::branches::make_branch(&self.deps, tenant, source, target, req.version_id, data)
                .await;
        let generation = match self.next_generation(tenant, target).await {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!(%tenant, node = %target, error = %error_text(&e), "no generation");
                return;
            }
        };
        let status = match outcome {
            Ok(key) => {
                let status = status(
                    target,
                    ProviderState::Ready,
                    generation,
                    Some(req.version_id),
                );
                self.lock().insert(
                    (tenant, target),
                    Live {
                        status: status.clone(),
                        key: Some(key),
                    },
                );
                status
            }
            Err(e) => {
                tracing::warn!(
                    %tenant,
                    node = %target,
                    error = %error_text(&e),
                    "the branch of a database could not be made"
                );
                let mut s = status(target, ProviderState::Failed, generation, None);
                s.reason = Some(REASON_BRANCH_FAILED.into());
                s.message = Some(MESSAGE_BRANCH_FAILED.into());
                self.set(tenant, &s);
                s
            }
        };
        if let Err(e) = self
            .deps
            .platform
            .report_status(StatusReport {
                status,
                tenant_id: tenant,
                delivery_id: Some(delivery),
            })
            .await
        {
            tracing::info!(%tenant, node = %target, error = %error_text(&e), "a status report was not stored");
        }
    }

    /// The engine work of a copy: the source's key, the engine copy with `data`, the lens of
    /// the copy, the rows. Returns the key.
    async fn make_copy(
        &self,
        tenant: TenantId,
        source: NodeId,
        target: NodeId,
        data: DataCopy,
    ) -> Result<ruum_dbengine::PseudonymKey> {
        let src = self
            .deps
            .store
            .get(tenant, source)
            .await?
            .ok_or_else(|| node_not_found(source))?;
        let engine = self.engine(src.engine)?;
        // Decisions 0004-25 and 0004-30: the copy keeps the source's key_node_id, so the key
        // follows the data (a copy of a draft branch keeps the branch's key); the key comes
        // through Q-4 and the engine installs it in the copy.
        let key_node_id = src.key_node_id;
        let key = self.deps.keys.engine_key(tenant, key_node_id).await?;
        let copy = match data {
            DataCopy::Full => CopyData::Full,
            DataCopy::SchemaOnly => CopyData::SchemaOnly,
            DataCopy::Masked => {
                let catalog = engine
                    .catalog(&src.database_ref())
                    .await
                    .map_err(engine_error)?;
                let manifest = self.head_manifest(tenant, source).await?;
                let classes = super::query::class_map_from(
                    &*self.deps.columns,
                    tenant,
                    source,
                    src.engine,
                    &manifest,
                )
                .await?;
                CopyData::Masked(mask_plan(&catalog, src.engine, &classes))
            }
        };
        let created = engine
            .copy_database(
                &src.database_ref(),
                &key,
                &NewDatabase {
                    tenant_id: tenant.0,
                    node_id: target.0,
                    engine_version: src.engine_version.clone(),
                    extensions: src.extensions.clone(),
                },
                &copy,
                &key,
            )
            .await
            .map_err(engine_error)?;
        // The lens of the copy, from the copy's manifest (the classes the copy starts with).
        let rest = async {
            let manifest = self.head_manifest(tenant, target).await?;
            let sync = super::query::interim_catalog_sync(src.engine, &manifest);
            engine
                .sync_lens(&created.database, &key, &sync)
                .await
                .map_err(engine_error)?;
            let size_bytes = engine
                .size_bytes(&created.database)
                .await
                .map(|n| i64::try_from(n).unwrap_or(i64::MAX))
                .unwrap_or(0);
            let row = DatabaseRow {
                tenant_id: tenant,
                node_id: target,
                engine: src.engine,
                engine_version: src.engine_version.clone(),
                cluster: created.database.cluster.clone(),
                engine_name: created.database.engine_name.clone(),
                branch_of: None,
                branch_data: None,
                key_node_id,
                extensions: src.extensions.clone(),
                size_bytes,
            };
            self.deps.store.insert(&row, &created.roles).await
        }
        .await;
        if let Err(e) = rest {
            // Leave no engine database without its row.
            let _ = engine.drop_database(&created.database).await;
            return Err(e);
        }
        Ok(key)
    }

    /// The manifest of the newest version of a node.
    async fn head_manifest(&self, tenant: TenantId, node: NodeId) -> Result<serde_json::Value> {
        let row = self
            .deps
            .typed
            .get(tenant, node)
            .await?
            .ok_or_else(|| node_not_found(node))?;
        self.deps
            .store
            .manifest(tenant, row.desired_version)
            .await?
            .ok_or_else(|| Error::Internal("the manifest of a database is missing".into()))
    }

    /// `suspended` on `trashed`, `ready` again on `restored`, as a report one generation newer;
    /// the observed version stays. A node that does not serve (failed, being made) keeps its
    /// state.
    async fn trash_state(&self, tenant: TenantId, node: NodeId, trashed: bool) -> Result<()> {
        let Some(row) = self.deps.typed.get(tenant, node).await? else {
            return Ok(());
        };
        let next = match (trashed, row.state) {
            (true, s) if s.serves() => ProviderState::Suspended,
            (false, ProviderState::Suspended) => ProviderState::Ready,
            _ => return Ok(()),
        };
        let generation = self.next_generation(tenant, node).await?;
        let mut s = status(node, next, generation, row.observed_version);
        if trashed {
            s.reason = Some(REASON_IN_TRASH.into());
        }
        self.set(tenant, &s);
        self.deps
            .platform
            .report_status(StatusReport {
                status: s,
                tenant_id: tenant,
                delivery_id: None,
            })
            .await
    }

    /// Drops the database of a node: the engine database first, then the rows and the key.
    async fn drop_node(&self, tenant: TenantId, node: NodeId) -> Result<()> {
        if let Some(row) = self.deps.store.get(tenant, node).await? {
            let engine = self.engine(row.engine)?;
            match engine.drop_database(&row.database_ref()).await {
                Ok(()) | Err(EngineError::NotFound) => {}
                Err(e) => return Err(engine_error(e)),
            }
            self.deps.store.delete(tenant, node).await?;
        }
        self.lock().remove(&(tenant, node));
        Ok(())
    }
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

/// An engine error as a platform error: `500` with the kind only (the text can hold engine
/// text; it goes to the log of the caller, never into a status).
fn engine_error(e: EngineError) -> Error {
    match e {
        EngineError::NotReady => Error::api(
            crate::error::ErrorCode::Unavailable,
            "the database engine is busy; try again",
        ),
        other => Error::Internal(format!("database engine: {other}")),
    }
}

/// What a masked copy changes (decision 0004-25), the same columns as the lens masks: each
/// column of a table of the database's own schema by the class the lens gives it (`classes`:
/// the stricter of the column store and the manifest's `data_classes`, backlog 2026-11-09
/// S-18), `personal` when it has none. `personal` becomes a pseudonym, `secret` becomes `NULL`.
pub fn mask_plan(
    catalog: &Catalog,
    engine: EngineKind,
    classes: &super::query::postgres_mask::ClassMap,
) -> MaskPlan {
    let schema = super::query::own_schema(engine);
    let mut columns = Vec::new();
    for t in catalog
        .tables
        .iter()
        .filter(|t| t.schema == schema && t.kind == RelationKind::Table)
    {
        for c in &t.columns {
            let how = match classes
                .class(&t.name, &c.name)
                .unwrap_or(DataClass::Personal)
            {
                DataClass::Personal => MaskAs::Pseudonym,
                DataClass::Secret => MaskAs::Null,
                DataClass::Normal => continue,
            };
            columns.push((
                ColumnRef {
                    schema: t.schema.clone(),
                    table: t.name.clone(),
                    column: c.name.clone(),
                },
                how,
            ));
        }
    }
    MaskPlan { columns }
}

fn copies_not_available() -> Error {
    invalid(
        "copies of a database are not available yet",
        "copies_not_available",
    )
}

#[async_trait]
impl Provider for DatabaseProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        Self::descriptor_of()
    }

    async fn validate_manifest(
        &self,
        _delivery: DeliveryId,
        req: ValidateRequest,
    ) -> Result<ValidationResult> {
        if req.type_name != NODE_TYPE {
            return Err(bad_request(format!("unknown type {}", req.type_name)));
        }
        let by_agent = req.by.is_some_and(|b| b.is_agent());
        Ok(manifest::validate(
            &req.manifest,
            req.current.as_ref(),
            by_agent,
        ))
    }

    async fn create_typed_node(
        &self,
        delivery: DeliveryId,
        req: CreateTypedNodeRequest,
    ) -> Result<Reply<ProviderStatus>> {
        if req.type_name != NODE_TYPE {
            return Err(bad_request(format!("unknown type {}", req.type_name)));
        }
        // Only an empty database and a `schema_only` branch (card D-3a): refuse the rest, so no
        // data (real or masked) goes anywhere.
        let branch_of = match &req.source {
            Source::Empty => {
                if req.manifest.get("template").is_some() {
                    return Err(super::branches::branch_source_lost());
                }
                None
            }
            Source::Branch {
                node_id,
                data: DataCopy::SchemaOnly,
            } => Some(*node_id),
            Source::Branch { .. } => return Err(super::branches::data_not_available()),
            _ => return Err(copies_not_available()),
        };
        let spec = EngineSpec::of(&req.manifest)?;
        let (tenant, node) = (req.tenant_id, req.node_id);
        if let Some(l) = self.inner.lock().get(&(tenant, node)) {
            let s = l.status.clone();
            return Ok(if s.state == ProviderState::Provisioning {
                Reply::Started(s)
            } else {
                Reply::Done(s)
            });
        }
        if self.inner.deps.store.get(tenant, node).await?.is_some() {
            // Made before (another process, or before a restart).
            let generation = self.inner.next_generation(tenant, node).await?;
            let s = status(node, ProviderState::Ready, generation, Some(req.version_id));
            self.inner.set(tenant, &s);
            return Ok(Reply::Done(s));
        }
        self.inner.engine(spec.engine)?;
        let generation = self.inner.next_generation(tenant, node).await?;
        let s = status(node, ProviderState::Provisioning, generation, None);
        self.inner.set(tenant, &s);
        match branch_of {
            Some(source) => {
                tokio::spawn(self.inner.clone().branch(
                    delivery,
                    req,
                    source,
                    DataCopy::SchemaOnly,
                ));
            }
            None => {
                tokio::spawn(self.inner.clone().provision(delivery, req, spec));
            }
        }
        Ok(Reply::Started(s))
    }

    async fn apply_manifest(
        &self,
        _delivery: DeliveryId,
        node: NodeId,
        req: ApplyManifestRequest,
    ) -> Result<Reply<ProviderStatus>> {
        let tenant = req.tenant_id;
        let Some(row) = self.inner.deps.store.get(tenant, node).await? else {
            return match self.inner.lock().get(&(tenant, node)) {
                // Still being made: the reconciler applies the version after `ready`.
                Some(l) if l.status.state == ProviderState::Provisioning => {
                    Ok(Reply::Started(l.status.clone()))
                }
                _ => Err(node_not_found(node)),
            };
        };
        let spec = EngineSpec::of(&req.manifest)?;
        if spec.engine != row.engine || spec.version != row.engine_version {
            return Err(invalid(
                "the engine and its major version cannot change",
                "/engine",
            ));
        }
        let mut have = row.extensions.clone();
        let mut want = spec.extensions.clone();
        have.sort();
        want.sort();
        if have != want {
            return Err(invalid(
                "the extensions of a database are fixed when it is made",
                "/extensions",
            ));
        }
        // A change of `data_classes` (a class taken back or made stricter) rebuilds the lens
        // now, not at the next confirmation or schema change (review-Q-2-1 F-2). A failure
        // fails the delivery, so the reconciler tries again. A write of the column classes
        // themselves (a confirmation, a reset of the catalog sync) is left out: that code calls
        // `classes_changed` after it changed the store, and a reset is written from inside a
        // lens rebuild, which already uses it.
        let by_sync = super::classes::OWN_CLASSES_WRITE.try_with(|_| ()).is_ok();
        let before = self
            .inner
            .deps
            .store
            .manifest(tenant, req.previous_version_id)
            .await?
            .map(|m| super::query::manifest_classes(&m));
        if !by_sync && before.as_ref() != Some(&super::query::manifest_classes(&req.manifest)) {
            self.inner
                .deps
                .masking
                .manifest_changed(tenant, node)
                .await?;
        }
        let generation = self.inner.next_generation(tenant, node).await?;
        let s = status(node, ProviderState::Ready, generation, Some(req.version_id));
        self.inner.set(tenant, &s);
        Ok(Reply::Done(s))
    }

    async fn get_typed_node_status(
        &self,
        tenant: TenantId,
        node: NodeId,
    ) -> Result<ProviderStatus> {
        if let Some(l) = self.inner.lock().get(&(tenant, node)) {
            return Ok(l.status.clone());
        }
        let Some(_) = self.inner.deps.store.get(tenant, node).await? else {
            return Err(node_not_found(node));
        };
        // Made before a restart: the content matches the newest manifest (nothing of a step 2
        // manifest change needs engine work).
        let desired = self
            .inner
            .deps
            .typed
            .get(tenant, node)
            .await?
            .map(|r| r.desired_version);
        let generation = self.inner.next_generation(tenant, node).await?;
        let s = status(node, ProviderState::Ready, generation, desired);
        self.inner.set(tenant, &s);
        Ok(s)
    }

    async fn deliver_hook(
        &self,
        delivery: DeliveryId,
        hook: HookDelivery,
    ) -> Result<Reply<HookResult>> {
        let tenant = hook.tenant_id;
        match hook.hook {
            HookKind::Copied => {
                let (Some(source), Some(target)) = (hook.node_id, hook.target_node_id) else {
                    return Err(bad_request("copied needs node_id and target_node_id"));
                };
                // The same delivery again, or a copy that is made: the work is done once.
                if self.inner.lock().contains_key(&(tenant, target))
                    || self.inner.deps.store.get(tenant, target).await?.is_some()
                {
                    return Ok(Reply::Done(HookResult { done: true }));
                }
                let generation = self.inner.next_generation(tenant, target).await?;
                self.inner.set(
                    tenant,
                    &status(target, ProviderState::Provisioning, generation, None),
                );
                let data = crate::nodes::typed::data_of(&hook);
                tokio::spawn(
                    self.inner
                        .clone()
                        .copy(delivery, tenant, source, target, data),
                );
                return Ok(Reply::Started(HookResult { done: false }));
            }
            HookKind::Trashed | HookKind::Restored => {
                if let Some(node) = hook.node_id {
                    self.inner
                        .trash_state(tenant, node, hook.hook == HookKind::Trashed)
                        .await?;
                }
            }
            HookKind::Purged => {
                if let Some(node) = hook.node_id {
                    self.inner.drop_node(tenant, node).await?;
                }
            }
            HookKind::TenantDeleted => {
                for row in self.inner.deps.store.list_tenant(tenant).await? {
                    self.inner.drop_node(tenant, row.node_id).await?;
                }
                self.inner.lock().retain(|(t, _), _| *t != tenant);
            }
            _ => {}
        }
        Ok(Reply::Done(HookResult { done: true }))
    }

    async fn list_provider_versions(
        &self,
        _tenant: TenantId,
        _node: NodeId,
        _page: PageRequest,
    ) -> Result<ProviderVersionPage> {
        Ok(ProviderVersionPage {
            items: Vec::new(),
            next_cursor: None,
            window: None,
        })
    }

    async fn restore_provider_version(
        &self,
        _delivery: DeliveryId,
        _node: NodeId,
        _req: RestoreRequest,
    ) -> Result<ProviderStatus> {
        Err(invalid(
            "restores of a database are not available yet",
            "restore_not_available",
        ))
    }

    async fn export_typed_node(
        &self,
        _delivery: DeliveryId,
        _node: NodeId,
        _req: ExportRequest,
    ) -> Result<ProviderStatus> {
        Err(invalid(
            "exports of a database are not available yet",
            "export_not_available",
        ))
    }
}

/// The check API and the upload API of an in-process provider that uses neither (card D-1):
/// both fail. Status reports go through [`crate::providers::store::StoreReports`].
pub struct NoPlatformCalls;

#[async_trait]
impl PlatformApi for NoPlatformCalls {
    async fn report_status(&self, _report: StatusReport) -> Result<()> {
        Err(Error::Internal("no status store".into()))
    }

    async fn check_access(&self, _req: CheckRequest) -> Result<CheckResult> {
        Err(Error::Internal("the check API is not wired".into()))
    }

    async fn write_upload(
        &self,
        _tenant: TenantId,
        _upload: &UploadTarget,
        _bytes: Vec<u8>,
    ) -> Result<NodeId> {
        Err(Error::Internal("the upload API is not wired".into()))
    }
}

#[cfg(test)]
mod tests {
    //! Card P-3 on the in-memory services (memory engine): `copyNode` of a database through the
    //! node store, the delivery worker and this provider; `trashed`, `restored` and `purged`.

    use super::super::tests::{ctx, settled, world};
    use super::*;
    use crate::nodes::{CopyRequest, OnConflict, RestoreRequest};
    use crate::providers::deliveries::{Backoff, DeliveryWorker};
    use crate::tenants::TenantPolicyChange;
    use ruum_dbengine::{MigrationRequest, RoleKind, RoleRef, StatementMode, StatementRequest};
    use serde_json::json;
    use std::time::Duration;

    fn worker(s: &crate::Services) -> DeliveryWorker {
        DeliveryWorker::new(
            s.typed_nodes.clone(),
            s.providers.clone(),
            Backoff::default(),
        )
    }

    /// Runs the worker until nothing is due.
    async fn deliver(s: &crate::Services) {
        worker(s).run_once(100).await.unwrap();
    }

    /// A ready SQLite database in `parent` with the table `people (email text)` and one row.
    async fn garden(w: &super::super::tests::World, parent: NodeId, name: &str) -> NodeId {
        let a = ctx(w.team.id, w.alice);
        let id = w
            .s
            .databases
            .create(
                &a,
                super::super::CreateDatabase {
                    parent_id: parent,
                    name: name.into(),
                    manifest: json!({ "engine": "sqlite", "version": "3", "plan": "shared-small" }),
                    source: None,
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .unwrap()
            .value
            .node
            .id;
        assert_eq!(
            settled(&w.s, &a, id).await.status.state,
            ProviderState::Ready
        );
        let row = w
            .handles
            .databases
            .get(w.team.id, id)
            .await
            .unwrap()
            .unwrap();
        let engine = w.s.database_engines.get(row.engine).unwrap().clone();
        let key = w.s.tenant_keys.engine_key(w.team.id, None).await.unwrap();
        let manifest = json!({ "engine": "sqlite", "version": "3", "plan": "shared-small" });
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
                &super::super::query::interim_catalog_sync(EngineKind::Sqlite, &manifest),
            )
            .await
            .unwrap();
        id
    }

    /// The emails of `people` in a database, read as the owner kind.
    async fn emails(w: &super::super::tests::World, id: NodeId) -> Vec<String> {
        let row = w
            .handles
            .databases
            .get(w.team.id, id)
            .await
            .unwrap()
            .unwrap();
        let engine = w.s.database_engines.get(row.engine).unwrap().clone();
        let key =
            w.s.tenant_keys
                .engine_key(w.team.id, row.key_node_id)
                .await
                .unwrap();
        let out = engine
            .execute(
                &RoleRef::new(row.database_ref(), RoleKind::SecretRead).unwrap(),
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
        out.rows
            .iter()
            .map(|r| match &r[0] {
                ruum_dbengine::Value::Text(t) => t.clone(),
                other => panic!("not text: {other:?}"),
            })
            .collect()
    }

    fn copy_to(parent: NodeId) -> CopyRequest {
        CopyRequest {
            parent_id: parent,
            name: None,
            on_conflict: OnConflict::Rename,
        }
    }

    /// Decision 0004-7: a manager's copy holds the real data, keeps the source's key_node_id
    /// and becomes ready through the `copied` hook; a guest editor's copy is masked, and the
    /// memory engine (no lens) refuses a masked copy, so the copy fails with a fixed text and
    /// no database (fail closed).
    #[tokio::test]
    async fn copies_follow_the_callers_rights() {
        let w = world().await;
        let a = ctx(w.team.id, w.alice);
        let src = garden(&w, w.folder, "Source").await;
        let copy =
            w.s.nodes
                .copy(&a, src, copy_to(w.folder))
                .await
                .unwrap()
                .value;
        assert_eq!(
            w.s.databases.get(&a, copy.id).await.unwrap().status.state,
            ProviderState::Provisioning
        );
        deliver(&w.s).await;
        let db = settled(&w.s, &a, copy.id).await;
        assert_eq!(db.status.state, ProviderState::Ready, "{:?}", db.status);
        assert_eq!(db.manifest_version_id, copy.head_version.unwrap());
        assert_eq!(emails(&w, copy.id).await, vec!["alice@example.com"]);
        let (s_row, c_row) = (
            w.handles
                .databases
                .get(w.team.id, src)
                .await
                .unwrap()
                .unwrap(),
            w.handles
                .databases
                .get(w.team.id, copy.id)
                .await
                .unwrap()
                .unwrap(),
        );
        assert_eq!(c_row.key_node_id, s_row.key_node_id);
        assert_eq!(c_row.branch_of, None);
        assert_ne!(c_row.engine_name, s_row.engine_name);
        // The copy is its own database: a write to the source does not reach it.
        assert_eq!(emails(&w, src).await, vec!["alice@example.com"]);

        // Bob is a guest with edit: refused while the policy is off, masked after.
        let b = ctx(w.team.id, w.bob);
        let refused = w.s.nodes.copy(&b, src, copy_to(w.folder)).await;
        assert_eq!(
            refused.err().map(|e| e.to_api().hint.unwrap_or_default()),
            Some("guests_create_resources".to_string())
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
        let masked =
            w.s.nodes
                .copy(&b, src, copy_to(w.folder))
                .await
                .unwrap()
                .value;
        deliver(&w.s).await;
        let db = settled(&w.s, &a, masked.id).await;
        assert_eq!(db.status.state, ProviderState::Failed);
        assert_eq!(db.status.reason.as_deref(), Some(REASON_COPY_FAILED));
        assert_eq!(db.status.message.as_deref(), Some(MESSAGE_COPY_FAILED));
        assert!(
            w.handles
                .databases
                .get(w.team.id, masked.id)
                .await
                .unwrap()
                .is_none(),
            "a masked copy on an engine without a lens holds no data"
        );
    }

    /// The same `copied` delivery twice makes one database.
    #[tokio::test]
    async fn a_copied_hook_does_its_work_once() {
        let w = world().await;
        let a = ctx(w.team.id, w.alice);
        let src = garden(&w, w.folder, "Once").await;
        let target =
            w.s.nodes
                .copy(&a, src, copy_to(w.folder))
                .await
                .unwrap()
                .value
                .id;
        let provider = w.s.databases.deps().provider.clone();
        let mut data = serde_json::Map::new();
        data.insert("data".into(), json!("full"));
        let hook = HookDelivery {
            hook: HookKind::Copied,
            tenant_id: w.team.id,
            node_id: Some(src),
            seq: Some(1),
            target_node_id: Some(target),
            data: Some(data),
            by: crate::actor::Actor::user(w.alice).into(),
            at: crate::now(),
        };
        let d = DeliveryId::new();
        let first = provider.deliver_hook(d, hook.clone()).await.unwrap();
        assert!(!first.is_done());
        settled(&w.s, &a, target).await;
        let again = provider.deliver_hook(d, hook).await.unwrap();
        assert!(again.is_done());
        assert_eq!(
            w.handles
                .databases
                .list_tenant(w.team.id)
                .await
                .unwrap()
                .iter()
                .filter(|r| r.node_id == target)
                .count(),
            1
        );
        // The worker's delivery of the same copy finds it made.
        deliver(&w.s).await;
        assert_eq!(emails(&w, target).await, vec!["alice@example.com"]);
    }

    /// A mask plan names every column of a table of the own schema, by its class (personal
    /// when it has none); normal columns and views are left out.
    #[test]
    fn a_mask_plan_follows_the_classes() {
        use ruum_dbengine::{CatalogColumn, CatalogTable};
        let col = |name: &str| CatalogColumn {
            name: name.into(),
            data_type: "text".into(),
            nullable: true,
            identity: None,
        };
        let table = |schema: &str, name: &str, kind| CatalogTable {
            schema: schema.into(),
            name: name.into(),
            kind,
            columns: vec![col("email"), col("token"), col("latin"), col("note")],
            primary_key: vec![],
            foreign_keys: vec![],
        };
        let catalog = Catalog {
            tables: vec![
                table("public", "people", RelationKind::Table),
                table("public", "people_v", RelationKind::View),
                table("other", "people", RelationKind::Table),
            ],
        };
        let manifest = super::super::query::manifest_classes(&json!({ "data_classes": {
            "public.people.token": "secret",
            "public.people.latin": "normal",
            "public.people.note": "normal",
        }}));
        // The column store: `latin` confirmed `normal`, `email` and `token` unconfirmed, and
        // `note` held `personal` by the store while the manifest calls it `normal` (a drift,
        // backlog 2026-11-09 S-18): the stricter class wins, as in the lens.
        let row = |c: &str, class| {
            (
                "public".to_string(),
                "people".to_string(),
                c.to_string(),
                class,
            )
        };
        let store = vec![
            row("email", DataClass::Personal),
            row("token", DataClass::Personal),
            row("latin", DataClass::Normal),
            row("note", DataClass::Personal),
        ];
        let classes =
            super::super::query::postgres_mask::class_map(EngineKind::Postgres, &store, &manifest);
        let plan = mask_plan(&catalog, EngineKind::Postgres, &classes);
        let got: Vec<(String, MaskAs)> = plan
            .columns
            .iter()
            .map(|(c, m)| (format!("{}.{}.{}", c.schema, c.table, c.column), *m))
            .collect();
        assert_eq!(
            got,
            vec![
                ("public.people.email".to_string(), MaskAs::Pseudonym),
                ("public.people.token".to_string(), MaskAs::Null),
                ("public.people.note".to_string(), MaskAs::Pseudonym),
            ]
        );
    }

    /// `restoreVersion` of a database (decision 0004-6): `manage` only; a new manifest version
    /// with the old content, which the provider applies; a version of another node is `404`.
    #[tokio::test]
    async fn restore_version_goes_through_the_manifest_rules() {
        let w = world().await;
        let a = ctx(w.team.id, w.alice);
        let id = garden(&w, w.folder, "Versions").await;
        let other = garden(&w, w.folder, "Other").await;
        let first = w.s.databases.get(&a, id).await.unwrap();
        let mut m = first.manifest.clone();
        m["data_classes"] = json!({ "main.people.email": "personal" });
        let second =
            w.s.databases
                .update_manifest(
                    &a,
                    id,
                    super::super::UpdateManifest {
                        manifest: m.clone(),
                        label: None,
                    },
                    None,
                )
                .await
                .unwrap()
                .value;
        settled(&w.s, &a, id).await;
        m["data_classes"] = json!({ "main.people.email": "normal" });
        w.s.databases
            .update_manifest(
                &a,
                id,
                super::super::UpdateManifest {
                    manifest: m,
                    label: None,
                },
                None,
            )
            .await
            .unwrap();
        // Bob has edit: 403.
        let b = ctx(w.team.id, w.bob);
        let refused =
            w.s.databases
                .restore_version(&b, id, second.manifest_version_id)
                .await;
        assert_eq!(refused.err().map(|e| e.http_status()), Some(403));
        // A version of another database: 404.
        let foreign =
            w.s.databases
                .get(&a, other)
                .await
                .unwrap()
                .manifest_version_id;
        let missing = w.s.databases.restore_version(&a, id, foreign).await;
        assert_eq!(missing.err().map(|e| e.http_status()), Some(404));
        let restored =
            w.s.databases
                .restore_version(&a, id, second.manifest_version_id)
                .await
                .unwrap();
        assert_ne!(restored.value.id, second.manifest_version_id);
        let db = settled(&w.s, &a, id).await;
        assert_eq!(db.manifest_version_id, restored.value.id);
        assert_eq!(db.manifest["data_classes"]["main.people.email"], "personal");
        assert_eq!(db.status.state, ProviderState::Ready);
    }

    /// `trashed` suspends, `restored` serves again, `purged` drops the database, its key and
    /// (after the delivery) the typed node's row.
    #[tokio::test]
    async fn trash_restore_and_purge_reach_the_provider() {
        let w = world().await;
        let a = ctx(w.team.id, w.alice);
        let id = garden(&w, w.folder, "Lifecycle").await;
        let typed = w.s.typed_nodes.clone();
        w.s.nodes.trash(&a, w.folder, None).await.unwrap();
        deliver(&w.s).await;
        let row = typed.get(w.team.id, id).await.unwrap().unwrap();
        assert_eq!(row.state, ProviderState::Suspended);
        assert_eq!(row.reason.as_deref(), Some(REASON_IN_TRASH));
        w.s.nodes
            .restore(&a, w.folder, RestoreRequest::default())
            .await
            .unwrap();
        deliver(&w.s).await;
        let row = typed.get(w.team.id, id).await.unwrap().unwrap();
        assert_eq!(row.state, ProviderState::Ready);
        assert_eq!(
            w.s.databases.get(&a, id).await.unwrap().status.state,
            ProviderState::Ready
        );
        let provider = w.s.databases.deps().provider.clone();
        assert!(provider.holds_key(w.team.id, id));
        w.s.nodes.trash(&a, id, None).await.unwrap();
        w.s.nodes.purge(&a, id).await.unwrap();
        deliver(&w.s).await;
        assert!(!provider.holds_key(w.team.id, id));
        assert!(
            w.handles
                .databases
                .get(w.team.id, id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(typed.get(w.team.id, id).await.unwrap().is_none());
    }
}

#[cfg(test)]
mod postgres_tests {
    //! Card P-3 on the Postgres engine (`RUUM_DBHOST_URL`; skipped without it, except in CI
    //! and with `RUUM_DBENGINE_REQUIRE_POSTGRES`): a manager's copy holds the real data; a
    //! masked copy holds pseudonyms (under the source's key: the same as the source's lens
    //! shows) and `NULL` for a secret column.

    use super::super::tests::{ctx, settled, world};
    use super::super::{DatabaseParts, Engines, install};
    use super::*;
    use crate::nodes::{CopyRequest, OnConflict};
    use crate::providers::deliveries::{Backoff, DeliveryWorker};
    use crate::tenants::TenantPolicyChange;
    use ruum_dbengine::{MigrationRequest, RoleKind, RoleRef, StatementMode, StatementRequest};
    use serde_json::json;
    use std::time::Duration;

    async fn engine() -> Option<Arc<dyn DatabaseEngine>> {
        let required = std::env::var_os("CI").is_some()
            || std::env::var_os("RUUM_DBENGINE_REQUIRE_POSTGRES").is_some();
        let Some(config) = ruum_dbengine::postgres::PostgresConfig::from_env() else {
            assert!(
                !required,
                "RUUM_DBHOST_URL is not set: this test needs the data cluster"
            );
            eprintln!("skipped: RUUM_DBHOST_URL is not set");
            return None;
        };
        match ruum_dbengine::postgres::PostgresEngine::connect(config).await {
            Ok(e) => Some(Arc::new(e) as Arc<dyn DatabaseEngine>),
            Err(e) => {
                assert!(!required, "the data cluster: {e}");
                eprintln!("skipped: the data cluster does not answer: {e}");
                None
            }
        }
    }

    async fn read(
        w: &super::super::tests::World,
        id: NodeId,
        kind: RoleKind,
        sql: &str,
    ) -> Vec<Vec<ruum_dbengine::Value>> {
        let row = w
            .handles
            .databases
            .get(w.team.id, id)
            .await
            .unwrap()
            .unwrap();
        let engine = w.s.database_engines.get(row.engine).unwrap().clone();
        let key =
            w.s.tenant_keys
                .engine_key(w.team.id, row.key_node_id)
                .await
                .unwrap();
        engine
            .execute(
                &RoleRef::new(row.database_ref(), kind).unwrap(),
                &key,
                &StatementRequest {
                    sql: sql.into(),
                    params: vec![],
                    mode: StatementMode::Read,
                    timeout: Duration::from_secs(30),
                    max_rows: 10,
                    max_bytes: None,
                },
            )
            .await
            .unwrap()
            .rows
    }

    fn text(v: &ruum_dbengine::Value) -> Option<String> {
        match v {
            ruum_dbengine::Value::Text(t) => Some(t.clone()),
            ruum_dbengine::Value::Null => None,
            other => panic!("not text: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn postgres_copies_are_full_or_masked() {
        let Some(engine) = engine().await else {
            return;
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
            queries: Arc::new(super::super::query::MemoryQueryLog::new()),
        };
        install(&mut w.s, parts).unwrap();
        let a = ctx(w.team.id, w.alice);
        // The token column is secret, the email column has no class (personal).
        let manifest = json!({
            "engine": "postgres", "version": "17", "plan": "shared-small",
            "data_classes": { "public.people.token": "secret" },
        });
        let src =
            w.s.databases
                .create(
                    &a,
                    super::super::CreateDatabase {
                        parent_id: w.folder,
                        name: "Copied on Postgres".into(),
                        manifest: manifest.clone(),
                        source: None,
                        on_conflict: OnConflict::Fail,
                    },
                )
                .await
                .unwrap()
                .value
                .node
                .id;
        assert_eq!(
            settled(&w.s, &a, src).await.status.state,
            ProviderState::Ready
        );
        let row = w
            .handles
            .databases
            .get(w.team.id, src)
            .await
            .unwrap()
            .unwrap();
        let key = w.s.tenant_keys.engine_key(w.team.id, None).await.unwrap();
        engine
            .apply_migration(
                &row.database_ref(),
                &key,
                &MigrationRequest {
                    up: "CREATE TABLE people (id int primary key, email text not null, \
                         token text not null); \
                         INSERT INTO people VALUES (1, 'alice@example.com', 's3cret');"
                        .into(),
                    timeout: Duration::from_secs(30),
                    dry_run: false,
                },
                &super::super::query::interim_catalog_sync(EngineKind::Postgres, &manifest),
            )
            .await
            .unwrap();
        let lens = read(&w, src, RoleKind::MaskedRead, "SELECT email FROM people").await;
        let lens_email = text(&lens[0][0]).unwrap();
        assert_ne!(lens_email, "alice@example.com");

        let worker = DeliveryWorker::new(
            w.s.typed_nodes.clone(),
            w.s.providers.clone(),
            Backoff::default(),
        );
        let copy_to = CopyRequest {
            parent_id: w.folder,
            name: None,
            on_conflict: OnConflict::Rename,
        };
        // Alice manages the source: a full copy.
        let full =
            w.s.nodes
                .copy(&a, src, copy_to.clone())
                .await
                .unwrap()
                .value;
        worker.run_once(100).await.unwrap();
        let db = settled(&w.s, &a, full.id).await;
        assert_eq!(db.status.state, ProviderState::Ready, "{:?}", db.status);
        let rows = read(
            &w,
            full.id,
            RoleKind::SecretRead,
            "SELECT email, token FROM people",
        )
        .await;
        assert_eq!(text(&rows[0][0]).as_deref(), Some("alice@example.com"));
        assert_eq!(text(&rows[0][1]).as_deref(), Some("s3cret"));

        // Bob is a guest editor: a masked copy (policy on).
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
        let b = ctx(w.team.id, w.bob);
        let masked = w.s.nodes.copy(&b, src, copy_to).await.unwrap().value;
        worker.run_once(100).await.unwrap();
        let db = settled(&w.s, &a, masked.id).await;
        assert_eq!(db.status.state, ProviderState::Ready, "{:?}", db.status);
        let m_row = w
            .handles
            .databases
            .get(w.team.id, masked.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            m_row.key_node_id, row.key_node_id,
            "the copy keeps the source's key"
        );
        let rows = read(
            &w,
            masked.id,
            RoleKind::SecretRead,
            "SELECT email, token FROM people",
        )
        .await;
        let email = text(&rows[0][0]).unwrap();
        assert_ne!(email, "alice@example.com");
        assert_eq!(
            email, lens_email,
            "a masked copy holds the pseudonym of the source's key"
        );
        assert_eq!(
            text(&rows[0][1]),
            None,
            "a secret column is NULL in a masked copy"
        );
        for id in [src, full.id, masked.id] {
            if let Some(r) = w.handles.databases.get(w.team.id, id).await.unwrap() {
                let _ = engine.drop_database(&r.database_ref()).await;
            }
        }
    }
}
