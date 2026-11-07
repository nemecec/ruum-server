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
//! - Copies: card D-1 makes no copy. A `Source::Copy`, `Source::Branch` or `Source::Template`
//!   and the hook `copied` are refused with `422` (so an agent never gets real data through
//!   them, review-P-1 note 2). Cards P-3 and D-3 build them.
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
use ruum_dbengine::{DatabaseEngine, EngineError, NewDatabase};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The `reason` of a database that could not be made.
pub const REASON_PROVISIONING_FAILED: &str = "provisioning_failed";
/// The text of a database that could not be made. No engine text (decision 0004-8).
pub const MESSAGE_PROVISIONING_FAILED: &str =
    "The database could not be made. Try again later, or ask for help.";

/// What the provider works with.
pub struct ProviderDeps {
    pub engines: Engines,
    pub keys: Arc<TenantKeys>,
    pub store: Arc<dyn DatabaseStore>,
    /// Read only: the generation and the desired version of a node after a restart.
    pub typed: Arc<dyn TypedNodeStore>,
    /// Where the provider reports (`reportStatus`).
    pub platform: Arc<dyn PlatformApi>,
}

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
            hooks: vec![HookKind::Copied, HookKind::Purged, HookKind::TenantDeleted],
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
        // D-1 makes no copy: refuse, so no data (real or masked) goes anywhere.
        if !matches!(req.source, Source::Empty) {
            return Err(copies_not_available());
        }
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
        tokio::spawn(self.inner.clone().provision(delivery, req, spec));
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
        _delivery: DeliveryId,
        hook: HookDelivery,
    ) -> Result<Reply<HookResult>> {
        let tenant = hook.tenant_id;
        match hook.hook {
            HookKind::Copied => return Err(copies_not_available()),
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
