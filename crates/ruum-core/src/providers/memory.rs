//! The in-memory test provider ([`MemoryProvider`]) and a platform double ([`MemoryPlatform`]).
//!
//! `MemoryProvider` (`ruum.test`) owns the types `test.note` and `test.branch` (the only child
//! type of `test.note`, made by `Source::Branch`). Its content is a list of rows, each with a
//! data class like the columns of a database: `normal`, `personal` (a pseudonym in a masked
//! copy) and `secret` (NULL in a masked copy). Each node also holds a key that only the
//! platform may know (like the pseudonym key of a database): it is never in a status, a copy
//! or an export.
//!
//! The manifest of both types (its JSON Schema is
//! `https://ruum.space/contracts/manifests/test-note.schema.json`, a test type only):
//!
//! | Field | Rule |
//! |---|---|
//! | `title` | a non-empty string, required |
//! | `kind` | `plain` or `rich`, default `plain`; cannot change after create (`kind_change_not_allowed`) |
//! | `limit` | an integer from 1, default 10; more than 100 on the plan `free` is `plan_limit_reached` |
//! | `fail` | a boolean, default false; `true` makes `applyManifest` fail (`apply_failed`), for tests of the failed state |
//!
//! `tenant_exported` takes `data.upload` (`{ "upload_id", "token" }`): the provider writes one
//! JSON file with every node of the tenant there. `copied` takes `data.data` (a `DataCopy`,
//! default `masked`) and an optional `data.version_id` (the manifest version of the new node).
//! `copied` refuses a target that already has content, and an agent gets no `full` data
//! (copy, branch, `copied` or export).
//!
//! Both are safe to share between tasks. Neither holds a lock across an `await`.

use super::*;
use crate::error::ApiError;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

/// The data class of a row of the test provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowClass {
    Normal,
    Personal,
    Secret,
}

/// One row of the content of a test node. `value` is `None` for NULL.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub key: String,
    pub value: Option<String>,
    pub class: RowClass,
}

impl Row {
    pub fn new(key: &str, value: &str, class: RowClass) -> Self {
        Self {
            key: key.into(),
            value: Some(value.into()),
            class,
        }
    }
}

/// The test provider.
pub struct MemoryProvider {
    platform: Arc<dyn PlatformApi>,
    descriptor: ProviderDescriptor,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    nodes: HashMap<(TenantId, NodeId), Node>,
    /// The answer of each delivery, so that a second call with the same ID does nothing. Keyed
    /// by tenant: a delivery ID of one tenant tells nothing about another.
    deliveries: HashMap<(TenantId, DeliveryId), Recorded>,
}

#[derive(Clone)]
enum Recorded {
    Status(Reply<ProviderStatus>),
    Hook(Reply<HookResult>),
}

struct Node {
    type_name: String,
    manifest: Value,
    observed: Option<VersionId>,
    state: ProviderState,
    reason: Option<String>,
    generation: i64,
    /// The key that only the platform knows; never leaves the provider.
    key: String,
    rows: Vec<Row>,
    /// Restore points and branches, oldest first.
    versions: Vec<StoredVersion>,
    /// Every content state with its time, oldest first (the point-in-time window).
    history: Vec<(OffsetDateTime, Vec<Row>)>,
    created_at: OffsetDateTime,
    /// Open sessions (principal, permission), closed by `access_changed` when access ends.
    sessions: Vec<(ActorPair, String)>,
}

struct StoredVersion {
    version: ProviderVersion,
    rows: Vec<Row>,
}

impl Node {
    fn new(type_name: &str, manifest: Value, observed: Option<VersionId>) -> Self {
        let now = crate::now();
        Self {
            type_name: type_name.into(),
            manifest,
            observed,
            state: ProviderState::Provisioning,
            reason: None,
            generation: 0,
            key: hex::encode(Uuid::now_v7().as_bytes()),
            rows: Vec::new(),
            versions: Vec::new(),
            history: vec![(now, Vec::new())],
            created_at: now,
            sessions: Vec::new(),
        }
    }

    /// Moves to a new state and returns the status with the next generation.
    fn set(&mut self, node: NodeId, state: ProviderState, reason: Option<&str>) -> ProviderStatus {
        self.state = state;
        self.reason = reason.map(str::to_owned);
        self.generation += 1;
        self.status(node)
    }

    fn status(&self, node: NodeId) -> ProviderStatus {
        let mut detail = serde_json::Map::new();
        detail.insert("rows".into(), Value::from(self.rows.len()));
        ProviderStatus {
            node_id: node,
            state: self.state,
            generation: self.generation,
            observed_version_id: self.observed,
            reason: self.reason.clone(),
            message: None,
            detail: Some(detail),
            export: None,
            at: crate::now(),
        }
    }

    fn set_rows(&mut self, rows: Vec<Row>) {
        self.rows = rows;
        self.history.push((crate::now(), self.rows.clone()));
    }

    /// The rows as a copy with `data` takes them.
    fn copy_rows(&self, data: DataCopy) -> Vec<Row> {
        match data {
            DataCopy::Full => self.rows.clone(),
            DataCopy::SchemaOnly => Vec::new(),
            DataCopy::Masked => self
                .rows
                .iter()
                .map(|r| Row {
                    key: r.key.clone(),
                    value: match r.class {
                        RowClass::Normal => r.value.clone(),
                        RowClass::Personal => r.value.as_deref().map(|v| pseudonym(&self.key, v)),
                        RowClass::Secret => None,
                    },
                    class: r.class,
                })
                .collect(),
        }
    }

    fn add_version(
        &mut self,
        kind: ProviderVersionKind,
        label: Option<&str>,
        reason: &str,
        by: Option<ActorPair>,
        node_id: Option<NodeId>,
    ) -> String {
        let id = Uuid::now_v7().to_string();
        let restore_modes = match kind {
            ProviderVersionKind::Branch => Vec::new(),
            _ => vec![RestoreTarget::InPlace, RestoreTarget::NewNode],
        };
        self.versions.push(StoredVersion {
            version: ProviderVersion {
                id: id.clone(),
                kind,
                label: label.map(str::to_owned),
                reason: Some(reason.into()),
                created_at: crate::now(),
                size_bytes: Some(self.rows.len() as i64),
                node_id,
                by,
                restore_modes,
            },
            rows: self.rows.clone(),
        });
        id
    }
}

/// A pseudonym of a value: `p_` and 12 hex characters of SHA-256(key, value). The test
/// provider's stand-in for the HMAC of the privacy module.
fn pseudonym(key: &str, value: &str) -> String {
    let mut h = Sha256::new();
    h.update(key.as_bytes());
    h.update([0]);
    h.update(value.as_bytes());
    format!("p_{}", &hex::encode(h.finalize())[..12])
}

fn lock(m: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl MemoryProvider {
    pub const ID: &'static str = "ruum.test";
    pub const TYPE: &'static str = "test.note";
    pub const CHILD_TYPE: &'static str = "test.branch";
    pub const SCHEMA: &'static str = "https://ruum.space/contracts/manifests/test-note.schema.json";
    /// The only export format.
    pub const FORMAT: &'static str = "json";

    /// A test provider that reports to `platform`.
    pub fn new(platform: Arc<dyn PlatformApi>) -> Self {
        Self::with_descriptor(platform, Self::default_descriptor())
    }

    /// A test provider with another descriptor (for tests of the registry).
    pub fn with_descriptor(platform: Arc<dyn PlatformApi>, descriptor: ProviderDescriptor) -> Self {
        Self {
            platform,
            descriptor,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// The descriptor of the test provider: every hook and the features of a database.
    pub fn default_descriptor() -> ProviderDescriptor {
        let roles = TypeRoles {
            view: "read the rows".into(),
            comment: None,
            edit: "change the rows".into(),
            manage: "change the manifest, restore, export".into(),
        };
        ProviderDescriptor {
            id: Self::ID.into(),
            name: "Ruum test provider".into(),
            base_url: None,
            open_url: None,
            types: vec![
                TypeDescriptor {
                    type_name: Self::TYPE.into(),
                    display_name: Some("Test note".into()),
                    icon: Some("note".into()),
                    extension: Some(".json".into()),
                    manifest_schema: Self::SCHEMA.into(),
                    roles: roles.clone(),
                    child_types: vec![Self::CHILD_TYPE.into()],
                },
                TypeDescriptor {
                    type_name: Self::CHILD_TYPE.into(),
                    display_name: Some("Test branch".into()),
                    icon: Some("branch".into()),
                    extension: Some(".json".into()),
                    manifest_schema: Self::SCHEMA.into(),
                    roles,
                    child_types: Vec::new(),
                },
            ],
            hooks: HookKind::ALL.to_vec(),
            features: vec![
                Feature::ProviderVersions,
                Feature::RestoreInPlace,
                Feature::RestoreNewNode,
                Feature::Export,
            ],
        }
    }

    // ------------------------------------------------------------------ test helpers

    /// Replaces the rows of a node (a stand-in for writes by queries).
    pub fn put_rows(&self, tenant: TenantId, node: NodeId, rows: Vec<Row>) -> Result<()> {
        let mut inner = lock(&self.inner);
        let n = live_mut(&mut inner, tenant, node)?;
        n.set_rows(rows);
        Ok(())
    }

    /// The rows of a node, or `None` when the provider does not hold it.
    pub fn rows(&self, tenant: TenantId, node: NodeId) -> Option<Vec<Row>> {
        let inner = lock(&self.inner);
        inner
            .nodes
            .get(&(tenant, node))
            .filter(|n| n.state != ProviderState::Deleted)
            .map(|n| n.rows.clone())
    }

    /// The key of a node that only the platform may know.
    pub fn key(&self, tenant: TenantId, node: NodeId) -> Option<String> {
        lock(&self.inner)
            .nodes
            .get(&(tenant, node))
            .map(|n| n.key.clone())
    }

    /// Makes a named restore point (`manual`) and returns its ID.
    pub fn create_restore_point(
        &self,
        tenant: TenantId,
        node: NodeId,
        label: &str,
        by: Option<ActorPair>,
    ) -> Result<String> {
        let mut inner = lock(&self.inner);
        let n = live_mut(&mut inner, tenant, node)?;
        Ok(n.add_version(
            ProviderVersionKind::RestorePoint,
            Some(label),
            "manual",
            by,
            None,
        ))
    }

    /// Opens a session of `principal` with `permission` on a node (like a database
    /// connection). `access_changed` checks each session again.
    pub fn open_session(
        &self,
        tenant: TenantId,
        node: NodeId,
        principal: ActorPair,
        permission: &str,
    ) -> Result<()> {
        let mut inner = lock(&self.inner);
        let n = live_mut(&mut inner, tenant, node)?;
        n.sessions.push((principal, permission.into()));
        Ok(())
    }

    /// The open sessions of a node.
    pub fn sessions(&self, tenant: TenantId, node: NodeId) -> Vec<(ActorPair, String)> {
        lock(&self.inner)
            .nodes
            .get(&(tenant, node))
            .map(|n| n.sessions.clone())
            .unwrap_or_default()
    }

    // ---------------------------------------------------------------------- internals

    fn owns(&self, type_name: &str) -> bool {
        self.descriptor.type_descriptor(type_name).is_some()
    }

    fn recorded(&self, tenant: TenantId, delivery: DeliveryId) -> Option<Recorded> {
        lock(&self.inner)
            .deliveries
            .get(&(tenant, delivery))
            .cloned()
    }

    async fn report(&self, tenant: TenantId, delivery: Option<DeliveryId>, status: ProviderStatus) {
        let report = StatusReport {
            status,
            tenant_id: tenant,
            delivery_id: delivery,
        };
        if let Err(e) = self.platform.report_status(report).await {
            tracing::warn!(error = %e, "test provider: status report refused");
        }
    }

    async fn report_all(
        &self,
        tenant: TenantId,
        delivery: Option<DeliveryId>,
        statuses: Vec<ProviderStatus>,
    ) {
        for s in statuses {
            self.report(tenant, delivery, s).await;
        }
    }
}

fn live_mut(inner: &mut Inner, tenant: TenantId, node: NodeId) -> Result<&mut Node> {
    inner
        .nodes
        .get_mut(&(tenant, node))
        .filter(|n| n.state != ProviderState::Deleted)
        .ok_or_else(|| node_not_found(node))
}

/// The rules of the test manifest. Returns the normalized manifest and the problems.
fn check_manifest(
    manifest: &Value,
    current: Option<&Value>,
    plan: Option<&str>,
) -> Result<(Value, Vec<Problem>)> {
    let Some(obj) = manifest.as_object() else {
        return Err(bad_request("the manifest is not a JSON object"));
    };
    let mut problems = Vec::new();
    let mut problem = |pointer: &str, code: &str, message: &str, hint: Option<&str>| {
        problems.push(Problem {
            pointer: pointer.into(),
            code: code.into(),
            message: message.into(),
            hint: hint.map(str::to_owned),
        })
    };
    let mut normalized = obj.clone();
    match obj.get("title").and_then(Value::as_str) {
        Some(t) if !t.trim().is_empty() => {}
        _ => problem(
            "/title",
            "title_required",
            "the title is missing or empty",
            None,
        ),
    }
    let kind = obj.get("kind").cloned().unwrap_or(Value::from("plain"));
    if !matches!(kind.as_str(), Some("plain" | "rich")) {
        problem("/kind", "unknown_kind", "kind is plain or rich", None);
    }
    if let Some(cur) = current {
        let cur_kind = cur.get("kind").cloned().unwrap_or(Value::from("plain"));
        if cur_kind != kind {
            problem(
                "/kind",
                "kind_change_not_allowed",
                "the kind of a node cannot change",
                Some("make a new node with the other kind"),
            );
        }
    }
    normalized.insert("kind".into(), kind);
    let limit = obj.get("limit").cloned().unwrap_or(Value::from(10));
    match limit.as_i64() {
        Some(l) if l >= 1 => {
            if plan == Some("free") && l > 100 {
                problem(
                    "/limit",
                    "plan_limit_reached",
                    "the plan free allows a limit of 100",
                    Some("choose a limit up to 100, or a larger plan"),
                );
            }
        }
        _ => problem(
            "/limit",
            "invalid_limit",
            "limit is an integer from 1",
            None,
        ),
    }
    normalized.insert("limit".into(), limit);
    let fail = obj.get("fail").cloned().unwrap_or(Value::Bool(false));
    if !fail.is_boolean() {
        problem("/fail", "invalid_fail", "fail is a boolean", None);
    }
    normalized.insert("fail".into(), fail);
    Ok((Value::Object(normalized), problems))
}

fn problems_error(problems: &[Problem]) -> Error {
    let first = &problems[0];
    Error::Api(
        ApiError::new(
            ErrorCode::ValidationFailed,
            format!(
                "the manifest is not valid: {} at {}",
                first.code, first.pointer
            ),
        )
        .with_hint("call validateManifest for the list of problems"),
    )
}

#[async_trait]
impl Provider for MemoryProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        self.descriptor.clone()
    }

    async fn validate_manifest(
        &self,
        _delivery: DeliveryId,
        req: ValidateRequest,
    ) -> Result<ValidationResult> {
        if !self.owns(&req.type_name) {
            return Err(bad_request(format!(
                "this provider has no type {}",
                req.type_name
            )));
        }
        let (normalized, problems) =
            check_manifest(&req.manifest, req.current.as_ref(), req.plan.as_deref())?;
        Ok(ValidationResult {
            valid: problems.is_empty(),
            normalized: problems.is_empty().then_some(normalized),
            problems,
        })
    }

    async fn create_typed_node(
        &self,
        delivery: DeliveryId,
        req: CreateTypedNodeRequest,
    ) -> Result<Reply<ProviderStatus>> {
        if let Some(Recorded::Status(r)) = self.recorded(req.tenant_id, delivery) {
            return Ok(r);
        }
        if !self.owns(&req.type_name) {
            return Err(invalid(
                format!("this provider has no type {}", req.type_name),
                "send the node to the provider of its type",
            ));
        }
        let (manifest, problems) = check_manifest(&req.manifest, None, None)?;
        if !problems.is_empty() {
            return Err(problems_error(&problems));
        }
        let full = matches!(
            req.source,
            Source::Copy {
                data: Some(DataCopy::Full),
                ..
            } | Source::Branch {
                data: DataCopy::Full,
                ..
            }
        );
        if full && req.by.is_agent() {
            return Err(agent_full());
        }
        let tenant = req.tenant_id;
        let (reply, later) = {
            let mut inner = lock(&self.inner);
            if inner.nodes.contains_key(&(tenant, req.node_id)) {
                return Err(conflict(format!(
                    "typed node {} already exists",
                    req.node_id
                )));
            }
            let mut node = Node::new(&req.type_name, manifest, Some(req.version_id));
            let source_rows = match &req.source {
                Source::Empty => None,
                Source::Copy { node_id, data } => {
                    let src = live_source(&inner, tenant, *node_id)?;
                    Some(src.copy_rows(data.unwrap_or_default()))
                }
                Source::Template { node_id, .. } => {
                    let src = live_source(&inner, tenant, *node_id)?;
                    Some(src.copy_rows(DataCopy::Masked))
                }
                Source::Branch { node_id, data } => {
                    let src = live_source(&inner, tenant, *node_id)?;
                    if !self
                        .descriptor
                        .type_descriptor(&src.type_name)
                        .is_some_and(|t| t.child_types.contains(&req.type_name))
                    {
                        return Err(invalid(
                            format!(
                                "a {} cannot be a branch of a {}",
                                req.type_name, src.type_name
                            ),
                            "a branch of a branch is refused; branch the source",
                        ));
                    }
                    if req.parent_id.is_some_and(|p| p != *node_id) {
                        return Err(invalid(
                            "a branch is a child of its source",
                            "set parent_id to the source node",
                        ));
                    }
                    Some(src.copy_rows(*data))
                }
            };
            let reply = match source_rows {
                None => Reply::Done(node.set(req.node_id, ProviderState::Ready, None)),
                Some(rows) => {
                    node.set_rows(rows);
                    let started = node.set(req.node_id, ProviderState::Provisioning, None);
                    Reply::Started(started)
                }
            };
            let later = if reply.is_done() {
                None
            } else {
                Some(node.set(req.node_id, ProviderState::Ready, None))
            };
            if let Source::Branch { node_id, .. } = &req.source {
                let label = req
                    .manifest
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if let Some(src) = inner.nodes.get_mut(&(tenant, *node_id)) {
                    src.add_version(
                        ProviderVersionKind::Branch,
                        label.as_deref(),
                        "branch",
                        Some(req.by),
                        Some(req.node_id),
                    );
                }
            }
            inner.nodes.insert((tenant, req.node_id), node);
            inner
                .deliveries
                .insert((tenant, delivery), Recorded::Status(reply.clone()));
            (reply, later)
        };
        if let Some(s) = later {
            self.report(tenant, Some(delivery), s).await;
        }
        Ok(reply)
    }

    async fn apply_manifest(
        &self,
        delivery: DeliveryId,
        node: NodeId,
        req: ApplyManifestRequest,
    ) -> Result<Reply<ProviderStatus>> {
        if let Some(Recorded::Status(r)) = self.recorded(req.tenant_id, delivery) {
            return Ok(r);
        }
        let mut inner = lock(&self.inner);
        let n = live_mut(&mut inner, req.tenant_id, node)?;
        let (manifest, problems) = check_manifest(&req.manifest, Some(&n.manifest), None)?;
        if !problems.is_empty() {
            return Err(problems_error(&problems));
        }
        let fail = manifest.get("fail").and_then(Value::as_bool) == Some(true);
        let status = if fail {
            n.set(node, ProviderState::Failed, Some("apply_failed"))
        } else {
            n.manifest = manifest;
            n.observed = Some(req.version_id);
            // A node in the trash keeps its state; the new manifest serves after `restored`.
            let state = if n.state == ProviderState::Suspended {
                ProviderState::Suspended
            } else {
                ProviderState::Ready
            };
            let reason = n
                .reason
                .clone()
                .filter(|_| state == ProviderState::Suspended);
            n.set(node, state, reason.as_deref())
        };
        let reply = Reply::Done(status);
        inner
            .deliveries
            .insert((req.tenant_id, delivery), Recorded::Status(reply.clone()));
        Ok(reply)
    }

    async fn get_typed_node_status(
        &self,
        tenant: TenantId,
        node: NodeId,
    ) -> Result<ProviderStatus> {
        lock(&self.inner)
            .nodes
            .get(&(tenant, node))
            .map(|n| n.status(node))
            .ok_or_else(|| node_not_found(node))
    }

    async fn deliver_hook(
        &self,
        delivery: DeliveryId,
        hook: HookDelivery,
    ) -> Result<Reply<HookResult>> {
        let tenant = hook.tenant_id;
        if let Some(Recorded::Hook(r)) = self.recorded(tenant, delivery) {
            return Ok(r);
        }
        let done = Reply::Done(HookResult { done: true });
        let mut reports = Vec::new();
        let mut export: Option<(UploadTarget, Vec<u8>)> = None;
        let mut checks: Vec<(NodeId, ActorPair, String)> = Vec::new();
        {
            let mut inner = lock(&self.inner);
            if hook.hook.is_tenant_hook() {
                match hook.hook {
                    HookKind::TenantDeleted => inner.nodes.retain(|(t, _), _| *t != tenant),
                    _ => {
                        let upload = hook
                            .data
                            .as_ref()
                            .and_then(|d| d.get("upload"))
                            .cloned()
                            .ok_or_else(|| bad_request("tenant_exported needs data.upload"))?;
                        let upload: UploadTarget = serde_json::from_value(upload)
                            .map_err(|e| bad_request(format!("data.upload: {e}")))?;
                        let mut nodes: Vec<Value> = inner
                            .nodes
                            .iter()
                            .filter(|((t, _), n)| *t == tenant && n.state != ProviderState::Deleted)
                            .map(|((_, id), n)| export_value(*id, n, DataCopy::Full))
                            .collect();
                        nodes.sort_by_key(|v| v["node_id"].as_str().map(str::to_owned));
                        let bytes = serde_json::to_vec(&serde_json::json!({ "nodes": nodes }))
                            .map_err(|e| Error::Internal(e.to_string()))?;
                        export = Some((upload, bytes));
                    }
                }
            } else {
                let node = hook
                    .node_id
                    .ok_or_else(|| bad_request("a node hook needs node_id"))?;
                let known = inner
                    .nodes
                    .get(&(tenant, node))
                    .is_some_and(|n| n.state != ProviderState::Deleted);
                if !known {
                    if hook.hook.is_idempotent_delete() {
                        inner
                            .deliveries
                            .insert((tenant, delivery), Recorded::Hook(done.clone()));
                        return Ok(done);
                    }
                    return Err(node_not_found(node));
                }
                match hook.hook {
                    HookKind::Copied => {
                        let target = hook
                            .target_node_id
                            .ok_or_else(|| bad_request("copied needs target_node_id"))?;
                        let data: DataCopy = hook
                            .data
                            .as_ref()
                            .and_then(|d| d.get("data"))
                            .map(|v| serde_json::from_value(v.clone()))
                            .transpose()
                            .map_err(|e| bad_request(format!("data.data: {e}")))?
                            .unwrap_or_default();
                        // The same guard as an export: an agent never gets real data.
                        if data == DataCopy::Full && hook.by.is_agent() {
                            return Err(agent_full());
                        }
                        // A copy makes a new node; it never writes over content (also not
                        // over the source itself).
                        if inner.nodes.contains_key(&(tenant, target)) {
                            return Err(invalid(
                                format!("typed node {target} already has content"),
                                "copy into a new node",
                            ));
                        }
                        let version: Option<VersionId> = hook
                            .data
                            .as_ref()
                            .and_then(|d| d.get("version_id"))
                            .and_then(|v| serde_json::from_value(v.clone()).ok());
                        let src = &inner.nodes[&(tenant, node)];
                        let rows = src.copy_rows(data);
                        let type_name = src.type_name.clone();
                        let manifest = src.manifest.clone();
                        let copy = inner
                            .nodes
                            .entry((tenant, target))
                            .or_insert_with(|| Node::new(&type_name, manifest, version));
                        copy.set_rows(rows);
                        reports.push(copy.set(target, ProviderState::Ready, None));
                    }
                    HookKind::Moved | HookKind::Renamed => {}
                    HookKind::Trashed => {
                        let n = live_mut(&mut inner, tenant, node)?;
                        n.sessions.clear();
                        reports.push(n.set(node, ProviderState::Suspended, Some("trashed")));
                    }
                    HookKind::Restored => {
                        let n = live_mut(&mut inner, tenant, node)?;
                        if n.state == ProviderState::Suspended {
                            reports.push(n.set(node, ProviderState::Ready, None));
                        }
                    }
                    HookKind::Purged => {
                        let n = live_mut(&mut inner, tenant, node)?;
                        n.rows.clear();
                        n.versions.clear();
                        n.history.clear();
                        n.sessions.clear();
                        n.manifest = Value::Null;
                        reports.push(n.set(node, ProviderState::Deleted, None));
                    }
                    HookKind::AccessChanged => {
                        let n = live_mut(&mut inner, tenant, node)?;
                        checks = n
                            .sessions
                            .iter()
                            .map(|(p, perm)| (node, *p, perm.clone()))
                            .collect();
                    }
                    HookKind::TenantExported | HookKind::TenantDeleted => unreachable!(),
                }
            }
        }
        if let Some((upload, bytes)) = export {
            self.platform.write_upload(tenant, &upload, bytes).await?;
        }
        // `access_changed`: ask the check API for each open session and close the ones that
        // lost access.
        let mut closed = Vec::new();
        for (node, principal, permission) in checks {
            let res = self
                .platform
                .check_access(CheckRequest {
                    tenant_id: tenant,
                    node_id: node,
                    principal,
                    permission: permission.clone(),
                })
                .await?;
            if !res.allowed {
                closed.push((principal, permission));
            }
        }
        {
            let mut inner = lock(&self.inner);
            if !closed.is_empty()
                && let Some(n) = hook
                    .node_id
                    .and_then(|id| inner.nodes.get_mut(&(tenant, id)))
            {
                n.sessions.retain(|s| !closed.contains(s));
            }
            inner
                .deliveries
                .insert((tenant, delivery), Recorded::Hook(done.clone()));
        }
        self.report_all(tenant, Some(delivery), reports).await;
        Ok(done)
    }

    async fn list_provider_versions(
        &self,
        tenant: TenantId,
        node: NodeId,
        page: PageRequest,
    ) -> Result<ProviderVersionPage> {
        let inner = lock(&self.inner);
        let n = inner
            .nodes
            .get(&(tenant, node))
            .filter(|n| n.state != ProviderState::Deleted)
            .ok_or_else(|| node_not_found(node))?;
        let start = match &page.cursor {
            None => 0,
            Some(c) => {
                c.0.parse::<usize>()
                    .map_err(|_| bad_request("the cursor is not valid"))?
            }
        };
        let limit = page.limit() as usize;
        let newest_first: Vec<&StoredVersion> = n.versions.iter().rev().collect();
        let items: Vec<ProviderVersion> = newest_first
            .iter()
            .skip(start)
            .take(limit)
            .map(|v| v.version.clone())
            .collect();
        let next = start + items.len();
        Ok(ProviderVersionPage {
            items,
            next_cursor: (next < newest_first.len()).then(|| next.to_string()),
            window: Some(VersionWindow {
                earliest: n.created_at,
                latest: crate::now(),
            }),
        })
    }

    async fn restore_provider_version(
        &self,
        delivery: DeliveryId,
        node: NodeId,
        req: RestoreRequest,
    ) -> Result<ProviderStatus> {
        let tenant = req.tenant_id;
        if let Some(Recorded::Status(r)) = self.recorded(tenant, delivery) {
            return Ok(r.into_inner());
        }
        let (started, later) = {
            let mut inner = lock(&self.inner);
            let n = live_mut(&mut inner, tenant, node)?;
            if n.state == ProviderState::Suspended {
                return Err(invalid(
                    "the node is in the trash",
                    "restore the node from the trash first",
                ));
            }
            let rows = match (&req.version, req.at) {
                (Some(v), None) => {
                    let sv = n
                        .versions
                        .iter()
                        .find(|sv| &sv.version.id == v)
                        .ok_or_else(|| Error::not_found(format!("provider version {v}")))?;
                    if !sv.version.restore_modes.contains(&req.target) {
                        return Err(invalid(
                            "this version cannot be restored this way",
                            "a branch is a node of its own; open it instead",
                        ));
                    }
                    sv.rows.clone()
                }
                (None, Some(at)) => {
                    if at < n.created_at || at > crate::now() {
                        return Err(invalid(
                            "the time is outside the window",
                            "list the provider versions for the window",
                        ));
                    }
                    n.history
                        .iter()
                        .rev()
                        .find(|(t, _)| *t <= at)
                        .map(|(_, rows)| rows.clone())
                        .unwrap_or_default()
                }
                _ => {
                    return Err(invalid(
                        "give version or at, not both and not neither",
                        "set exactly one of version and at",
                    ));
                }
            };
            match req.target {
                RestoreTarget::InPlace => {
                    n.add_version(
                        ProviderVersionKind::RestorePoint,
                        Some("Before the restore"),
                        "before_restore",
                        Some(req.by),
                        None,
                    );
                    n.set_rows(rows);
                    let started = n.set(node, ProviderState::Updating, None);
                    let later = n.set(node, ProviderState::Ready, None);
                    (started, later)
                }
                RestoreTarget::NewNode => {
                    let target = req.target_node_id.ok_or_else(|| {
                        invalid(
                            "a restore into a new node needs target_node_id",
                            "create the node first, then restore into it",
                        )
                    })?;
                    let type_name = n.type_name.clone();
                    let manifest = n.manifest.clone();
                    let observed = n.observed;
                    if inner.nodes.contains_key(&(tenant, target)) {
                        return Err(invalid(
                            format!("typed node {target} already has content"),
                            "restore into a new node",
                        ));
                    }
                    let mut t = Node::new(&type_name, manifest, observed);
                    t.set_rows(rows);
                    let started = t.set(target, ProviderState::Provisioning, None);
                    let later = t.set(target, ProviderState::Ready, None);
                    inner.nodes.insert((tenant, target), t);
                    (started, later)
                }
            }
        };
        lock(&self.inner).deliveries.insert(
            (tenant, delivery),
            Recorded::Status(Reply::Started(started.clone())),
        );
        self.report(tenant, Some(delivery), later).await;
        Ok(started)
    }

    async fn export_typed_node(
        &self,
        delivery: DeliveryId,
        node: NodeId,
        req: ExportRequest,
    ) -> Result<ProviderStatus> {
        if let Some(Recorded::Status(r)) = self.recorded(req.tenant_id, delivery) {
            return Ok(r.into_inner());
        }
        if req.format != Self::FORMAT {
            return Err(invalid(
                format!("unknown export format {}", req.format),
                format!("the format of this type is {}", Self::FORMAT),
            ));
        }
        let data = req.data.unwrap_or_default();
        if data == DataCopy::Full && req.by.is_agent() {
            return Err(agent_full());
        }
        let tenant = req.tenant_id;
        let (started, bytes) = {
            let inner = lock(&self.inner);
            let n = inner
                .nodes
                .get(&(tenant, node))
                .filter(|n| n.state != ProviderState::Deleted)
                .ok_or_else(|| node_not_found(node))?;
            let bytes = serde_json::to_vec(&export_value(node, n, data))
                .map_err(|e| Error::Internal(e.to_string()))?;
            (n.status(node), bytes)
        };
        let file = self
            .platform
            .write_upload(tenant, &req.upload, bytes)
            .await?;
        let later = {
            let mut inner = lock(&self.inner);
            inner.deliveries.insert(
                (tenant, delivery),
                Recorded::Status(Reply::Started(started.clone())),
            );
            inner.nodes.get_mut(&(tenant, node)).map(|n| {
                let (state, reason) = (n.state, n.reason.clone());
                let mut s = n.set(node, state, reason.as_deref());
                s.export = Some(ExportRef { node_id: file });
                s
            })
        };
        if let Some(s) = later {
            self.report(tenant, Some(delivery), s).await;
        }
        Ok(started)
    }
}

/// `validation_failed` for `data: full` from an agent: an agent gets masked copies, branches
/// and exports only (`docs/DESIGN.md`, section 8.5).
fn agent_full() -> Error {
    invalid(
        "an agent gets masked data only",
        "copy, branch or export with data masked",
    )
}

fn live_source(inner: &Inner, tenant: TenantId, node: NodeId) -> Result<&Node> {
    inner
        .nodes
        .get(&(tenant, node))
        .filter(|n| n.state != ProviderState::Deleted)
        .ok_or_else(|| {
            invalid(
                format!("the source node {node} does not exist"),
                "copy from a node of this tenant",
            )
        })
}

/// The export of one node: the content only, never the key.
fn export_value(id: NodeId, n: &Node, data: DataCopy) -> Value {
    serde_json::json!({
        "node_id": id,
        "type": n.type_name,
        "manifest": n.manifest,
        "rows": n.copy_rows(data),
    })
}

// ------------------------------------------------------------------------------ platform

/// A platform double for provider tests: it keeps the newest status per node by generation,
/// records every report, holds uploads granted with [`MemoryPlatform::grant_upload`], and
/// answers the check API from a table set with [`MemoryPlatform::allow`] (deny by default).
#[derive(Default)]
pub struct MemoryPlatform {
    inner: Mutex<PlatformInner>,
}

#[derive(Default)]
struct PlatformInner {
    statuses: HashMap<(TenantId, NodeId), ProviderStatus>,
    reports: Vec<StatusReport>,
    refused: HashMap<TenantId, usize>,
    grants: HashMap<UploadId, (TenantId, String)>,
    uploads: HashMap<UploadId, (TenantId, NodeId, Vec<u8>)>,
    access: HashMap<(TenantId, NodeId, Uuid, String), CheckRole>,
    checks: Vec<CheckRequest>,
}

impl MemoryPlatform {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, PlatformInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The newest stored status of a node.
    pub fn status(&self, tenant: TenantId, node: NodeId) -> Option<ProviderStatus> {
        self.lock().statuses.get(&(tenant, node)).cloned()
    }

    /// Every accepted report, in order.
    pub fn reports(&self) -> Vec<StatusReport> {
        self.lock().reports.clone()
    }

    /// The number of refused (older) reports, in all tenants.
    pub fn refused(&self) -> usize {
        self.lock().refused.values().sum()
    }

    /// The number of refused (older) reports of one tenant. Tests that share one platform
    /// with other tests use this count, not [`MemoryPlatform::refused`].
    pub fn refused_for(&self, tenant: TenantId) -> usize {
        self.lock().refused.get(&tenant).copied().unwrap_or(0)
    }

    /// Grants a one-upload token for an export into `tenant`.
    pub fn grant_upload(&self, tenant: TenantId) -> UploadTarget {
        let target = UploadTarget {
            upload_id: UploadId::new(),
            token: hex::encode(Uuid::now_v7().as_bytes()),
        };
        self.lock()
            .grants
            .insert(target.upload_id, (tenant, target.token.clone()));
        target
    }

    /// The bytes written to an upload and the file node that holds them.
    pub fn upload(&self, id: UploadId) -> Option<(NodeId, Vec<u8>)> {
        self.lock()
            .uploads
            .get(&id)
            .map(|(_, file, bytes)| (*file, bytes.clone()))
    }

    /// The number of finished uploads, in all tenants.
    pub fn upload_count(&self) -> usize {
        self.lock().uploads.len()
    }

    /// The number of finished uploads of one tenant. Tests that share one platform with
    /// other tests use this count, not [`MemoryPlatform::upload_count`].
    pub fn upload_count_for(&self, tenant: TenantId) -> usize {
        self.lock()
            .uploads
            .values()
            .filter(|(t, _, _)| *t == tenant)
            .count()
    }

    /// Lets `principal` (the actor's ID) have `permission` on a node with `role`.
    pub fn allow(
        &self,
        tenant: TenantId,
        node: NodeId,
        principal: Uuid,
        permission: &str,
        role: CheckRole,
    ) {
        self.lock()
            .access
            .insert((tenant, node, principal, permission.into()), role);
    }

    /// Takes the permission away again.
    pub fn revoke(&self, tenant: TenantId, node: NodeId, principal: Uuid, permission: &str) {
        self.lock()
            .access
            .remove(&(tenant, node, principal, permission.into()));
    }

    /// Every check request, in order.
    pub fn checks(&self) -> Vec<CheckRequest> {
        self.lock().checks.clone()
    }
}

#[async_trait]
impl PlatformApi for MemoryPlatform {
    async fn report_status(&self, report: StatusReport) -> Result<()> {
        let mut inner = self.lock();
        let key = (report.tenant_id, report.status.node_id);
        if let Some(stored) = inner.statuses.get(&key).map(|s| s.generation)
            && stored >= report.status.generation
        {
            *inner.refused.entry(report.tenant_id).or_default() += 1;
            return Err(conflict(format!(
                "generation {} is not newer than the stored generation {stored}",
                report.status.generation
            )));
        }
        inner.statuses.insert(key, report.status.clone());
        inner.reports.push(report);
        Ok(())
    }

    async fn check_access(&self, req: CheckRequest) -> Result<CheckResult> {
        let mut inner = self.lock();
        inner.checks.push(req.clone());
        let role = inner
            .access
            .get(&(
                req.tenant_id,
                req.node_id,
                req.principal.actor.id,
                req.permission.clone(),
            ))
            .copied();
        Ok(CheckResult {
            allowed: role.is_some(),
            role,
            expires_at: None,
        })
    }

    async fn write_upload(
        &self,
        tenant: TenantId,
        upload: &UploadTarget,
        bytes: Vec<u8>,
    ) -> Result<NodeId> {
        let mut inner = self.lock();
        match inner.grants.get(&upload.upload_id) {
            Some((t, token)) if *t == tenant && *token == upload.token => {}
            _ => return Err(Error::permission_denied()),
        }
        if inner.uploads.contains_key(&upload.upload_id) {
            return Err(conflict("the upload is already written"));
        }
        let file = NodeId::new();
        inner
            .uploads
            .insert(upload.upload_id, (tenant, file, bytes));
        Ok(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::UserId;

    fn setup() -> (Arc<MemoryPlatform>, MemoryProvider, TenantId, NodeId) {
        let platform = Arc::new(MemoryPlatform::new());
        let provider = MemoryProvider::new(platform.clone());
        (platform, provider, TenantId::new(), NodeId::new())
    }

    fn user() -> ActorPair {
        Actor::user(UserId::new()).into()
    }

    async fn create(p: &MemoryProvider, tenant: TenantId, node: NodeId) {
        p.create_typed_node(
            DeliveryId::new(),
            CreateTypedNodeRequest {
                tenant_id: tenant,
                node_id: node,
                parent_id: None,
                type_name: MemoryProvider::TYPE.into(),
                version_id: VersionId::new(),
                manifest: serde_json::json!({ "title": "t" }),
                source: Source::Empty,
                by: user(),
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn access_changed_closes_the_sessions_that_lost_access() {
        let (platform, p, tenant, node) = setup();
        create(&p, tenant, node).await;
        let (stays, goes) = (user(), user());
        platform.allow(tenant, node, stays.actor.id, "edit", CheckRole::Edit);
        platform.allow(tenant, node, goes.actor.id, "view", CheckRole::View);
        p.open_session(tenant, node, stays, "edit").unwrap();
        p.open_session(tenant, node, goes, "view").unwrap();
        platform.revoke(tenant, node, goes.actor.id, "view");

        let r = p
            .deliver_hook(
                DeliveryId::new(),
                HookDelivery {
                    hook: HookKind::AccessChanged,
                    tenant_id: tenant,
                    node_id: Some(node),
                    seq: Some(7),
                    target_node_id: None,
                    data: None,
                    by: user(),
                    at: crate::now(),
                },
            )
            .await
            .unwrap();
        assert_eq!(r, Reply::Done(HookResult { done: true }));
        assert_eq!(p.sessions(tenant, node), vec![(stays, "edit".to_string())]);
        assert_eq!(platform.checks().len(), 2);
        assert!(platform.checks().iter().all(|c| c.tenant_id == tenant));
    }

    #[tokio::test]
    async fn trash_closes_every_session() {
        let (_platform, p, tenant, node) = setup();
        create(&p, tenant, node).await;
        p.open_session(tenant, node, user(), "view").unwrap();
        p.deliver_hook(
            DeliveryId::new(),
            HookDelivery {
                hook: HookKind::Trashed,
                tenant_id: tenant,
                node_id: Some(node),
                seq: Some(1),
                target_node_id: None,
                data: None,
                by: user(),
                at: crate::now(),
            },
        )
        .await
        .unwrap();
        assert!(p.sessions(tenant, node).is_empty());
    }

    #[tokio::test]
    async fn platform_refuses_an_older_generation_and_a_foreign_upload() {
        let platform = MemoryPlatform::new();
        let (tenant, node) = (TenantId::new(), NodeId::new());
        let status = |generation| StatusReport {
            status: ProviderStatus {
                node_id: node,
                state: ProviderState::Ready,
                generation,
                observed_version_id: None,
                reason: None,
                message: None,
                detail: None,
                export: None,
                at: crate::now(),
            },
            tenant_id: tenant,
            delivery_id: None,
        };
        platform.report_status(status(2)).await.unwrap();
        assert!(platform.report_status(status(1)).await.is_err());
        assert!(platform.report_status(status(2)).await.is_err());
        assert_eq!(platform.status(tenant, node).unwrap().generation, 2);
        assert_eq!(platform.refused(), 2);

        let up = platform.grant_upload(tenant);
        let err = platform
            .write_upload(TenantId::new(), &up, b"x".to_vec())
            .await
            .unwrap_err();
        assert_eq!(err.http_status(), 403);
        let mut wrong = up.clone();
        wrong.token = "guess".into();
        assert!(platform.write_upload(tenant, &wrong, vec![]).await.is_err());
        platform
            .write_upload(tenant, &up, b"x".to_vec())
            .await
            .unwrap();
        assert!(platform.write_upload(tenant, &up, vec![]).await.is_err());
    }

    #[test]
    fn pseudonyms_depend_on_the_key() {
        assert_eq!(pseudonym("k", "v"), pseudonym("k", "v"));
        assert_ne!(pseudonym("k", "v"), pseudonym("k2", "v"));
        assert!(pseudonym("k", "v").starts_with("p_"));
        assert_eq!(pseudonym("k", "v").len(), 14);
    }
}
