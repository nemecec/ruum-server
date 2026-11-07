//! The conformance suite of providers: the same cases for every [`Provider`]. Today it runs on
//! the in-memory test provider; the database provider (step 2, `databases/provider.rs`) and the
//! HTTP adapter (step 6) run it through their own [`Fixture`].
//!
//! The cases follow the operations of `contracts/providers/provider.openapi.yaml`: descriptor,
//! validate, create, apply, status, hooks, provider versions, restore and export. Each case
//! makes its own tenants, so one fixture can run all of them, also in parallel. Each case also
//! checks tenant isolation: a node of one tenant is `not_found` for another.
//!
//! The suite sees content only through the fixture: it writes three markers (a `normal`, a
//! `personal` and a `secret` value) and reads a text dump of the content back. A masked copy
//! holds the normal marker only; a full copy holds all three; a schema-only copy holds none.
//!
//! The end of `contract` checks that the wire types serialize with the required fields and the
//! enum values of the contract.

use super::memory::{MemoryPlatform, MemoryProvider, Row, RowClass};
use super::registry::check_descriptor;
use super::*;
use crate::ids::UserId;
use crate::paging::Cursor;
use std::sync::Arc;
use std::time::Duration;

/// What the suite needs from a provider beyond the trait.
#[async_trait]
pub(crate) trait Fixture: Send + Sync {
    fn provider(&self) -> Arc<dyn Provider>;
    /// The platform double that the provider reports to.
    fn platform(&self) -> Arc<MemoryPlatform>;
    /// The node type the cases create.
    fn type_name(&self) -> String;
    /// The child type made by `Source::Branch`, if the type has branches.
    fn branch_type(&self) -> Option<String>;
    /// A valid first manifest.
    fn manifest(&self) -> Value;
    /// A valid change of [`Fixture::manifest`].
    fn changed_manifest(&self) -> Value;
    /// A change of [`Fixture::manifest`] that the provider refuses, and the pointer of the
    /// problem.
    fn refused_change(&self) -> (Value, String);
    /// A first manifest that is valid against the type's JSON Schema but that the provider
    /// refuses.
    fn refused_manifest(&self) -> Value;
    /// A valid export format of the type.
    fn export_format(&self) -> String;
    /// The `data` of a `tenant_exported` hook that writes to `upload`.
    fn tenant_export_data(&self, upload: &UploadTarget) -> serde_json::Map<String, Value>;
    /// Writes content with the three markers into a ready node (replaces earlier content).
    async fn seed(&self, tenant: TenantId, node: NodeId, m: &Markers);
    /// A text dump of the content, or `None` when the provider holds no content for the node.
    async fn content(&self, tenant: TenantId, node: NodeId) -> Option<String>;
    /// Makes a provider version (a named restore point) and returns its ID.
    async fn make_version(&self, tenant: TenantId, node: NodeId, label: &str) -> String;
    /// Values that only the platform may know (keys); no export may hold them.
    async fn secrets(&self, tenant: TenantId, node: NodeId) -> Vec<String>;
}

/// Three values, one per data class.
pub(crate) struct Markers {
    pub normal: String,
    pub personal: String,
    pub secret: String,
}

impl Markers {
    pub fn new() -> Self {
        let id = Uuid::now_v7().simple().to_string();
        Self {
            normal: format!("normal{id}"),
            personal: format!("personal{id}"),
            secret: format!("secret{id}"),
        }
    }
}

// ------------------------------------------------------------------------------ helpers

fn user() -> ActorPair {
    Actor::user(UserId::new()).into()
}

fn agent() -> ActorPair {
    ActorPair {
        actor: ProviderPrincipalRef {
            kind: ProviderPrincipalKind::Agent,
            id: Uuid::now_v7(),
        },
        on_behalf_of: Some(Uuid::now_v7()),
    }
}

fn status_code(e: &Error) -> u16 {
    e.http_status()
}

/// Waits until the status of a node meets `pred` (async work ends with a report).
async fn wait_for(
    f: &dyn Fixture,
    tenant: TenantId,
    node: NodeId,
    what: &str,
    pred: impl Fn(&ProviderStatus) -> bool,
) -> ProviderStatus {
    let p = f.provider();
    for _ in 0..200 {
        if let Ok(s) = p.get_typed_node_status(tenant, node).await
            && pred(&s)
        {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "node {node} never became {what}: {:?}",
        p.get_typed_node_status(tenant, node).await
    );
}

async fn wait_ready(f: &dyn Fixture, tenant: TenantId, node: NodeId) -> ProviderStatus {
    wait_for(f, tenant, node, "ready", |s| {
        s.state == ProviderState::Ready
    })
    .await
}

fn create_request(f: &dyn Fixture, tenant: TenantId, node: NodeId) -> CreateTypedNodeRequest {
    CreateTypedNodeRequest {
        tenant_id: tenant,
        node_id: node,
        parent_id: None,
        type_name: f.type_name(),
        version_id: VersionId::new(),
        manifest: f.manifest(),
        source: Source::Empty,
        by: user(),
    }
}

/// Creates a ready node with an empty source.
async fn create(f: &dyn Fixture, tenant: TenantId) -> (NodeId, VersionId) {
    let node = NodeId::new();
    let req = create_request(f, tenant, node);
    let version = req.version_id;
    f.provider()
        .create_typed_node(DeliveryId::new(), req)
        .await
        .expect("create");
    wait_ready(f, tenant, node).await;
    (node, version)
}

/// Creates a ready node with the three markers.
async fn create_seeded(f: &dyn Fixture, tenant: TenantId) -> (NodeId, Markers) {
    let (node, _) = create(f, tenant).await;
    let m = Markers::new();
    f.seed(tenant, node, &m).await;
    (node, m)
}

fn hook(kind: HookKind, tenant: TenantId, node: Option<NodeId>) -> HookDelivery {
    HookDelivery {
        hook: kind,
        tenant_id: tenant,
        node_id: node,
        seq: node.map(|_| 1),
        target_node_id: None,
        data: None,
        by: user(),
        at: crate::now(),
    }
}

fn holds_all(content: &str, m: &Markers) -> bool {
    content.contains(&m.normal) && content.contains(&m.personal) && content.contains(&m.secret)
}

fn holds_masked_only(content: &str, m: &Markers) -> bool {
    content.contains(&m.normal) && !content.contains(&m.personal) && !content.contains(&m.secret)
}

fn holds_none(content: &str, m: &Markers) -> bool {
    !content.contains(&m.normal) && !content.contains(&m.personal) && !content.contains(&m.secret)
}

fn is_snake_case(s: &str) -> bool {
    !s.is_empty()
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

// -------------------------------------------------------------------------------- cases

/// The descriptor follows the rules of the contract and names the fixture's types.
pub(crate) async fn descriptor(f: &dyn Fixture) {
    let d = f.provider().descriptor();
    check_descriptor(&d).expect("the descriptor follows the contract");
    let t = d
        .type_descriptor(&f.type_name())
        .expect("the provider owns the fixture type");
    if let Some(b) = f.branch_type() {
        assert!(t.child_types.contains(&b), "branches are child types");
    }
    let back: ProviderDescriptor =
        serde_json::from_value(serde_json::to_value(&d).unwrap()).unwrap();
    assert_eq!(back, d, "the descriptor survives JSON");
    let mut r = ProviderRegistry::new();
    r.register(f.provider()).expect("the registry takes it");
    assert!(r.for_type(&f.type_name()).is_some());
}

/// `validateManifest`: valid and refused manifests, normalization, no side effects.
pub(crate) async fn validate(f: &dyn Fixture) {
    let p = f.provider();
    let tenant = TenantId::new();
    let node = NodeId::new();
    let req = |manifest: Value, current: Option<Value>| ValidateRequest {
        tenant_id: tenant,
        node_id: Some(node),
        type_name: f.type_name(),
        manifest,
        current,
        plan: Some("team".into()),
        by: Some(user()),
    };

    let ok = p
        .validate_manifest(DeliveryId::new(), req(f.manifest(), None))
        .await
        .unwrap();
    assert!(ok.valid, "{:?}", ok.problems);
    assert!(ok.problems.is_empty());
    let normalized = ok
        .normalized
        .expect("a valid manifest comes back normalized");
    let again = p
        .validate_manifest(DeliveryId::new(), req(normalized.clone(), None))
        .await
        .unwrap();
    assert!(again.valid);
    assert_eq!(
        again.normalized.as_ref(),
        Some(&normalized),
        "normalizing is stable"
    );

    let change = p
        .validate_manifest(
            DeliveryId::new(),
            req(f.changed_manifest(), Some(normalized.clone())),
        )
        .await
        .unwrap();
    assert!(change.valid, "{:?}", change.problems);

    let (refused, pointer) = f.refused_change();
    let bad = p
        .validate_manifest(DeliveryId::new(), req(refused, Some(normalized)))
        .await
        .unwrap();
    assert!(!bad.valid);
    assert!(!bad.problems.is_empty());
    assert!(bad.problems.iter().any(|pr| pr.pointer == pointer));
    for pr in &bad.problems {
        assert!(
            pr.pointer.is_empty() || pr.pointer.starts_with('/'),
            "a JSON pointer"
        );
        assert!(is_snake_case(&pr.code), "snake_case code: {}", pr.code);
        assert!(!pr.message.is_empty());
    }

    // Pure: no node exists after the calls.
    let err = p.get_typed_node_status(tenant, node).await.unwrap_err();
    assert_eq!(status_code(&err), 404);

    let mut unknown = req(f.manifest(), None);
    unknown.type_name = "no.such.type".into();
    let err = p
        .validate_manifest(DeliveryId::new(), unknown)
        .await
        .unwrap_err();
    assert!(matches!(status_code(&err), 400 | 422), "{err}");
}

/// `createTypedNode`: empty, idempotent, isolated, conflicts, refused manifests, copies and
/// branches.
pub(crate) async fn create_node(f: &dyn Fixture) {
    let p = f.provider();
    let tenant = TenantId::new();
    let other = TenantId::new();

    let node = NodeId::new();
    let req = create_request(f, tenant, node);
    let version = req.version_id;
    let delivery = DeliveryId::new();
    let first = p.create_typed_node(delivery, req.clone()).await.unwrap();
    assert_eq!(first.body().node_id, node);
    assert!(first.body().generation >= 1);
    let ready = wait_ready(f, tenant, node).await;
    assert_eq!(ready.observed_version_id, Some(version));
    assert!(ready.reason.is_none());

    // The same delivery again does the work once.
    let replay = p.create_typed_node(delivery, req.clone()).await.unwrap();
    assert_eq!(replay, first);
    let after = p.get_typed_node_status(tenant, node).await.unwrap();
    assert_eq!(after.generation, ready.generation);

    // Another tenant does not see the node.
    let err = p.get_typed_node_status(other, node).await.unwrap_err();
    assert_eq!(status_code(&err), 404);

    // The same node with a new delivery is a conflict.
    let err = p
        .create_typed_node(DeliveryId::new(), req.clone())
        .await
        .unwrap_err();
    assert_eq!(status_code(&err), 409);

    // The same node ID in another tenant is another node.
    let mut in_other = req.clone();
    in_other.tenant_id = other;
    p.create_typed_node(DeliveryId::new(), in_other)
        .await
        .unwrap();
    wait_ready(f, other, node).await;
    let mine = p.get_typed_node_status(tenant, node).await.unwrap();
    assert_eq!(
        (mine.generation, mine.observed_version_id),
        (after.generation, after.observed_version_id),
        "a node of another tenant does not change this one"
    );

    // A manifest the provider refuses makes no node.
    let bad_node = NodeId::new();
    let mut bad = create_request(f, tenant, bad_node);
    bad.manifest = f.refused_manifest();
    let err = p
        .create_typed_node(DeliveryId::new(), bad)
        .await
        .unwrap_err();
    assert_eq!(status_code(&err), 422);
    assert_eq!(
        status_code(&p.get_typed_node_status(tenant, bad_node).await.unwrap_err()),
        404
    );

    // A type of another provider.
    let mut foreign = create_request(f, tenant, NodeId::new());
    foreign.type_name = "no.such.type".into();
    let err = p
        .create_typed_node(DeliveryId::new(), foreign)
        .await
        .unwrap_err();
    assert_eq!(status_code(&err), 422);

    // Copies: the source's data as `data` says; a source of another tenant is refused.
    let (src, m) = create_seeded(f, tenant).await;
    for (data, check) in [
        (DataCopy::Full, holds_all as fn(&str, &Markers) -> bool),
        (DataCopy::Masked, holds_masked_only),
        (DataCopy::SchemaOnly, holds_none),
    ] {
        let copy = NodeId::new();
        let mut req = create_request(f, tenant, copy);
        req.source = Source::Copy {
            node_id: src,
            data: Some(data),
        };
        p.create_typed_node(DeliveryId::new(), req).await.unwrap();
        wait_ready(f, tenant, copy).await;
        let content = f.content(tenant, copy).await.expect("the copy has content");
        assert!(check(&content, &m), "{data:?}: {content}");
    }
    let stolen = NodeId::new();
    let mut req = create_request(f, other, stolen);
    req.source = Source::Copy {
        node_id: src,
        data: Some(DataCopy::Full),
    };
    let err = p
        .create_typed_node(DeliveryId::new(), req)
        .await
        .unwrap_err();
    assert!(matches!(status_code(&err), 404 | 422), "{err}");
    assert!(f.content(other, stolen).await.is_none());

    // Branches: a child type, masked by default, listed as a provider version of the source;
    // a branch of a branch is refused.
    if let Some(branch_type) = f.branch_type() {
        let branch = NodeId::new();
        let mut req = create_request(f, tenant, branch);
        req.type_name = branch_type.clone();
        req.parent_id = Some(src);
        req.source = Source::Branch {
            node_id: src,
            data: DataCopy::Masked,
        };
        p.create_typed_node(DeliveryId::new(), req).await.unwrap();
        wait_ready(f, tenant, branch).await;
        let content = f.content(tenant, branch).await.unwrap();
        assert!(holds_masked_only(&content, &m), "{content}");
        let versions = p
            .list_provider_versions(tenant, src, PageRequest::default())
            .await
            .unwrap();
        assert!(
            versions
                .items
                .iter()
                .any(|v| v.kind == ProviderVersionKind::Branch && v.node_id == Some(branch))
        );

        let mut req = create_request(f, tenant, NodeId::new());
        req.type_name = branch_type;
        req.parent_id = Some(branch);
        req.source = Source::Branch {
            node_id: branch,
            data: DataCopy::Masked,
        };
        let err = p
            .create_typed_node(DeliveryId::new(), req)
            .await
            .unwrap_err();
        assert_eq!(status_code(&err), 422);
    }
}

/// `applyManifest`: a new version, idempotent, isolated, refused changes, content kept.
pub(crate) async fn apply(f: &dyn Fixture) {
    let p = f.provider();
    let tenant = TenantId::new();
    let (node, m) = create_seeded(f, tenant).await;
    let before = p.get_typed_node_status(tenant, node).await.unwrap();
    let v1 = before.observed_version_id.expect("an applied version");

    let v2 = VersionId::new();
    let req = ApplyManifestRequest {
        tenant_id: tenant,
        version_id: v2,
        manifest: f.changed_manifest(),
        previous_version_id: v1,
        by: user(),
    };
    let delivery = DeliveryId::new();
    let reply = p.apply_manifest(delivery, node, req.clone()).await.unwrap();
    let applied = wait_for(f, tenant, node, "applied", |s| {
        s.observed_version_id == Some(v2) && s.state == ProviderState::Ready
    })
    .await;
    assert!(applied.generation > before.generation);
    assert!(reply.body().generation > before.generation);

    let replay = p.apply_manifest(delivery, node, req.clone()).await.unwrap();
    assert_eq!(replay, reply);
    assert_eq!(
        p.get_typed_node_status(tenant, node)
            .await
            .unwrap()
            .generation,
        applied.generation
    );

    // A manifest version changes the configuration only, never the data.
    let content = f.content(tenant, node).await.unwrap();
    assert!(holds_all(&content, &m), "{content}");

    // Unknown node and another tenant.
    let err = p
        .apply_manifest(DeliveryId::new(), NodeId::new(), req.clone())
        .await
        .unwrap_err();
    assert_eq!(status_code(&err), 404);
    let mut foreign = req.clone();
    foreign.tenant_id = TenantId::new();
    let err = p
        .apply_manifest(DeliveryId::new(), node, foreign)
        .await
        .unwrap_err();
    assert_eq!(status_code(&err), 404);

    // A refused change keeps the applied version.
    let (refused, _) = f.refused_change();
    let err = p
        .apply_manifest(
            DeliveryId::new(),
            node,
            ApplyManifestRequest {
                tenant_id: tenant,
                version_id: VersionId::new(),
                manifest: refused,
                previous_version_id: v2,
                by: user(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(status_code(&err), 422);
    let now = p.get_typed_node_status(tenant, node).await.unwrap();
    assert_eq!(now.observed_version_id, Some(v2));
}

/// `getTypedNodeStatus`: not found, isolation, growing generations, no content values.
pub(crate) async fn status(f: &dyn Fixture) {
    let p = f.provider();
    let tenant = TenantId::new();
    let err = p
        .get_typed_node_status(tenant, NodeId::new())
        .await
        .unwrap_err();
    assert_eq!(status_code(&err), 404);

    let (node, m) = create_seeded(f, tenant).await;
    let s1 = p.get_typed_node_status(tenant, node).await.unwrap();
    assert!(s1.state.serves());
    assert_eq!(s1.node_id, node);
    let err = p
        .get_typed_node_status(TenantId::new(), node)
        .await
        .unwrap_err();
    assert_eq!(status_code(&err), 404);

    // Generations never go back.
    let v2 = VersionId::new();
    p.apply_manifest(
        DeliveryId::new(),
        node,
        ApplyManifestRequest {
            tenant_id: tenant,
            version_id: v2,
            manifest: f.changed_manifest(),
            previous_version_id: s1.observed_version_id.unwrap(),
            by: user(),
        },
    )
    .await
    .unwrap();
    let s2 = wait_for(f, tenant, node, "applied", |s| {
        s.observed_version_id == Some(v2)
    })
    .await;
    assert!(s2.generation > s1.generation);

    // Status, message and detail never hold content values.
    for s in [&s1, &s2] {
        let text = serde_json::to_string(s).unwrap();
        assert!(holds_none(&text, &m), "{text}");
        if let Some(r) = &s.reason {
            assert!(is_snake_case(r));
        }
    }

    // The platform's stored status is never newer than the provider's, and no report of the
    // provider was refused as older.
    if let Some(stored) = f.platform().status(tenant, node) {
        assert!(stored.generation <= s2.generation);
    }
    for r in f.platform().reports() {
        let text = serde_json::to_string(&r).unwrap();
        assert!(holds_none(&text, &m), "{text}");
    }
    // Count only this tenant's refusals: other cases may share the platform.
    assert_eq!(
        f.platform().refused_for(tenant),
        0,
        "a report went backwards"
    );
}

/// `deliverHook`: every hook kind, unknown nodes, idempotency and tenant isolation.
pub(crate) async fn hooks(f: &dyn Fixture) {
    let p = f.provider();
    let tenant = TenantId::new();
    let other = TenantId::new();
    let (node, m) = create_seeded(f, tenant).await;
    let (theirs, their_m) = create_seeded(f, other).await;

    // copied: `data` decides what the copy holds.
    for (data, check) in [
        (DataCopy::Full, holds_all as fn(&str, &Markers) -> bool),
        (DataCopy::Masked, holds_masked_only),
        (DataCopy::SchemaOnly, holds_none),
    ] {
        let target = NodeId::new();
        let mut h = hook(HookKind::Copied, tenant, Some(node));
        h.target_node_id = Some(target);
        let mut d = serde_json::Map::new();
        d.insert("data".into(), serde_json::to_value(data).unwrap());
        h.data = Some(d);
        let delivery = DeliveryId::new();
        let r = p.deliver_hook(delivery, h.clone()).await.unwrap();
        assert_eq!(r.body().done, r.is_done());
        let s = wait_ready(f, tenant, target).await;
        let content = f.content(tenant, target).await.unwrap();
        assert!(check(&content, &m), "{data:?}: {content}");
        // Again: no second copy.
        assert_eq!(p.deliver_hook(delivery, h).await.unwrap(), r);
        assert_eq!(
            p.get_typed_node_status(tenant, target)
                .await
                .unwrap()
                .generation,
            s.generation
        );
    }
    // A copy without `data` is masked.
    let target = NodeId::new();
    let mut h = hook(HookKind::Copied, tenant, Some(node));
    h.target_node_id = Some(target);
    p.deliver_hook(DeliveryId::new(), h).await.unwrap();
    wait_ready(f, tenant, target).await;
    assert!(holds_masked_only(
        &f.content(tenant, target).await.unwrap(),
        &m
    ));

    // moved and renamed change nothing of the content.
    for kind in [HookKind::Moved, HookKind::Renamed] {
        p.deliver_hook(DeliveryId::new(), hook(kind, tenant, Some(node)))
            .await
            .unwrap();
        assert!(holds_all(&f.content(tenant, node).await.unwrap(), &m));
        assert!(
            p.get_typed_node_status(tenant, node)
                .await
                .unwrap()
                .state
                .serves()
        );
    }

    // trashed: stop serving, keep the content; restored: serve again.
    p.deliver_hook(
        DeliveryId::new(),
        hook(HookKind::Trashed, tenant, Some(node)),
    )
    .await
    .unwrap();
    wait_for(f, tenant, node, "suspended", |s| {
        s.state == ProviderState::Suspended
    })
    .await;
    assert!(holds_all(&f.content(tenant, node).await.unwrap(), &m));
    p.deliver_hook(
        DeliveryId::new(),
        hook(HookKind::Restored, tenant, Some(node)),
    )
    .await
    .unwrap();
    wait_ready(f, tenant, node).await;

    // access_changed on a known node.
    p.deliver_hook(
        DeliveryId::new(),
        hook(HookKind::AccessChanged, tenant, Some(node)),
    )
    .await
    .unwrap();

    // Hooks for a node the provider does not know: 200 for trashed and purged, else 404.
    let unknown = NodeId::new();
    for kind in [HookKind::Trashed, HookKind::Purged] {
        p.deliver_hook(DeliveryId::new(), hook(kind, tenant, Some(unknown)))
            .await
            .unwrap_or_else(|e| panic!("{kind:?} of an unknown node: {e}"));
    }
    for kind in [
        HookKind::Moved,
        HookKind::Renamed,
        HookKind::Restored,
        HookKind::AccessChanged,
        HookKind::Copied,
    ] {
        let mut h = hook(kind, tenant, Some(unknown));
        h.target_node_id = Some(NodeId::new());
        let err = p.deliver_hook(DeliveryId::new(), h).await.unwrap_err();
        assert_eq!(status_code(&err), 404, "{kind:?}");
    }
    // A node of another tenant is unknown here: a hook in this tenant does not touch it.
    let mut h = hook(HookKind::Copied, tenant, Some(theirs));
    h.target_node_id = Some(NodeId::new());
    assert_eq!(
        status_code(&p.deliver_hook(DeliveryId::new(), h).await.unwrap_err()),
        404
    );
    p.deliver_hook(
        DeliveryId::new(),
        hook(HookKind::Purged, tenant, Some(theirs)),
    )
    .await
    .unwrap();
    assert!(holds_all(
        &f.content(other, theirs).await.unwrap(),
        &their_m
    ));

    // tenant_exported: one export with the tenant's content, nothing of another tenant, no key.
    let upload = f.platform().grant_upload(tenant);
    let mut h = hook(HookKind::TenantExported, tenant, None);
    h.data = Some(f.tenant_export_data(&upload));
    p.deliver_hook(DeliveryId::new(), h).await.unwrap();
    let bytes = wait_upload(f, &upload).await;
    let text = String::from_utf8_lossy(&bytes);
    assert!(holds_all(&text, &m), "{text}");
    assert!(holds_none(&text, &their_m), "{text}");
    for s in f.secrets(tenant, node).await {
        assert!(!text.contains(&s), "the export holds a platform secret");
    }

    // purged: the content is gone; a second purge is fine.
    p.deliver_hook(
        DeliveryId::new(),
        hook(HookKind::Purged, tenant, Some(node)),
    )
    .await
    .unwrap();
    wait_for(f, tenant, node, "deleted", |s| {
        s.state == ProviderState::Deleted
    })
    .await;
    assert!(f.content(tenant, node).await.is_none());
    p.deliver_hook(
        DeliveryId::new(),
        hook(HookKind::Purged, tenant, Some(node)),
    )
    .await
    .unwrap();

    // tenant_deleted: everything of the tenant goes, nothing of another tenant.
    let (a, _) = create_seeded(f, tenant).await;
    p.deliver_hook(
        DeliveryId::new(),
        hook(HookKind::TenantDeleted, tenant, None),
    )
    .await
    .unwrap();
    for _ in 0..200 {
        if f.content(tenant, a).await.is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(f.content(tenant, a).await.is_none());
    assert!(f.content(tenant, target).await.is_none());
    assert!(holds_all(
        &f.content(other, theirs).await.unwrap(),
        &their_m
    ));
    assert!(
        p.get_typed_node_status(other, theirs)
            .await
            .unwrap()
            .state
            .serves()
    );
    // Again: idempotent.
    p.deliver_hook(
        DeliveryId::new(),
        hook(HookKind::TenantDeleted, tenant, None),
    )
    .await
    .unwrap();
}

async fn wait_upload(f: &dyn Fixture, upload: &UploadTarget) -> Vec<u8> {
    for _ in 0..200 {
        if let Some((_, bytes)) = f.platform().upload(upload.upload_id) {
            return bytes;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the upload {} was never written", upload.upload_id.0);
}

/// `listProviderVersions`: newest first, paging, isolation, the window.
pub(crate) async fn provider_versions(f: &dyn Fixture) {
    let p = f.provider();
    assert!(
        p.descriptor().has_feature(Feature::ProviderVersions),
        "the suite runs on providers with provider versions"
    );
    let tenant = TenantId::new();
    let (node, _) = create_seeded(f, tenant).await;
    let mut ids = Vec::new();
    for label in ["first", "second", "third"] {
        ids.push(f.make_version(tenant, node, label).await);
    }

    let all = p
        .list_provider_versions(tenant, node, PageRequest::default())
        .await
        .unwrap();
    let listed: Vec<&str> = all
        .items
        .iter()
        .filter(|v| ids.contains(&v.id))
        .map(|v| v.id.as_str())
        .collect();
    let newest_first: Vec<&str> = ids.iter().rev().map(String::as_str).collect();
    assert_eq!(listed, newest_first);
    for w in all.items.windows(2) {
        assert!(w[0].created_at >= w[1].created_at, "newest first");
    }
    for v in all.items.iter().filter(|v| ids.contains(&v.id)) {
        assert_eq!(v.kind, ProviderVersionKind::RestorePoint);
        assert!(!v.restore_modes.is_empty());
    }
    if let Some(w) = all.window {
        assert!(w.earliest <= w.latest);
    }

    // Paging with a limit of 2.
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = p
            .list_provider_versions(
                tenant,
                node,
                PageRequest {
                    cursor: cursor.clone(),
                    limit: Some(2),
                },
            )
            .await
            .unwrap();
        assert!(page.items.len() <= 2);
        seen.extend(page.items.iter().map(|v| v.id.clone()));
        match page.next_cursor {
            Some(c) => cursor = Some(Cursor(c)),
            None => break,
        }
    }
    let all_ids: Vec<String> = all.items.iter().map(|v| v.id.clone()).collect();
    assert_eq!(seen, all_ids, "pages give the same list");

    // Isolation and unknown nodes.
    for (t, n) in [(TenantId::new(), node), (tenant, NodeId::new())] {
        let err = p
            .list_provider_versions(t, n, PageRequest::default())
            .await
            .unwrap_err();
        assert_eq!(status_code(&err), 404);
    }
}

/// `restoreProviderVersion`: in place (undoable), into a new node, at a time, refusals,
/// idempotency and isolation.
pub(crate) async fn restore(f: &dyn Fixture) {
    let p = f.provider();
    let tenant = TenantId::new();
    let (node, old) = create_seeded(f, tenant).await;
    let version = f.make_version(tenant, node, "before the change").await;
    let new = Markers::new();
    f.seed(tenant, node, &new).await;

    let req = |version: Option<String>, target: RestoreTarget| RestoreRequest {
        tenant_id: tenant,
        version,
        at: None,
        target,
        target_node_id: None,
        by: user(),
    };
    let count = |page: &ProviderVersionPage| page.items.len();
    let before = p
        .list_provider_versions(tenant, node, PageRequest::default())
        .await
        .unwrap();

    // In place: the old content, and a restore point of the current state.
    let delivery = DeliveryId::new();
    let in_place = req(Some(version.clone()), RestoreTarget::InPlace);
    let started = p
        .restore_provider_version(delivery, node, in_place.clone())
        .await
        .unwrap();
    assert_eq!(started.node_id, node);
    let done = wait_for(f, tenant, node, "restored", |s| {
        s.state == ProviderState::Ready && s.generation > started.generation
    })
    .await;
    let content = f.content(tenant, node).await.unwrap();
    assert!(holds_all(&content, &old) && holds_none(&content, &new));
    let after = p
        .list_provider_versions(tenant, node, PageRequest::default())
        .await
        .unwrap();
    assert_eq!(count(&after), count(&before) + 1, "one restore point more");
    let undo = after
        .items
        .iter()
        .find(|v| !before.items.iter().any(|b| b.id == v.id))
        .expect("the restore point of the current state")
        .clone();
    assert!(undo.restore_modes.contains(&RestoreTarget::InPlace));

    // The same delivery again: no second restore.
    let replay = p
        .restore_provider_version(delivery, node, in_place)
        .await
        .unwrap();
    assert_eq!(replay, started);
    let again = p
        .list_provider_versions(tenant, node, PageRequest::default())
        .await
        .unwrap();
    assert_eq!(count(&again), count(&after));
    assert_eq!(
        p.get_typed_node_status(tenant, node)
            .await
            .unwrap()
            .generation,
        done.generation
    );

    // Undo: restore the point made before the restore.
    p.restore_provider_version(
        DeliveryId::new(),
        node,
        req(Some(undo.id.clone()), RestoreTarget::InPlace),
    )
    .await
    .unwrap();
    wait_for(f, tenant, node, "undone", |s| {
        s.state == ProviderState::Ready && s.generation > done.generation
    })
    .await;
    assert!(holds_all(&f.content(tenant, node).await.unwrap(), &new));

    // Into a new node: the old content there, the source unchanged.
    let target = NodeId::new();
    let mut into_new = req(Some(version.clone()), RestoreTarget::NewNode);
    into_new.target_node_id = Some(target);
    p.restore_provider_version(DeliveryId::new(), node, into_new)
        .await
        .unwrap();
    wait_ready(f, tenant, target).await;
    let content = f.content(tenant, target).await.unwrap();
    assert!(holds_all(&content, &old) && holds_none(&content, &new));
    assert!(holds_all(&f.content(tenant, node).await.unwrap(), &new));

    // A point in time inside the window.
    let window = p
        .list_provider_versions(tenant, node, PageRequest::default())
        .await
        .unwrap()
        .window;
    if window.is_some() {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let at = crate::now();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let newest = Markers::new();
        f.seed(tenant, node, &newest).await;
        let mut by_time = req(None, RestoreTarget::InPlace);
        by_time.at = Some(at);
        let s = p
            .restore_provider_version(DeliveryId::new(), node, by_time)
            .await
            .unwrap();
        wait_for(f, tenant, node, "restored to a time", |x| {
            x.state == ProviderState::Ready && x.generation > s.generation
        })
        .await;
        let content = f.content(tenant, node).await.unwrap();
        assert!(holds_all(&content, &new) && holds_none(&content, &newest));

        let mut outside = req(None, RestoreTarget::InPlace);
        outside.at = Some(crate::now() + time::Duration::days(1));
        let err = p
            .restore_provider_version(DeliveryId::new(), node, outside)
            .await
            .unwrap_err();
        assert_eq!(status_code(&err), 422);
    }

    // Refusals: no target node, both or neither of version and at, an unknown version.
    let no_target = req(Some(version.clone()), RestoreTarget::NewNode);
    let mut both = req(Some(version.clone()), RestoreTarget::InPlace);
    both.at = Some(crate::now());
    let neither = req(None, RestoreTarget::InPlace);
    for bad in [no_target, both, neither] {
        let err = p
            .restore_provider_version(DeliveryId::new(), node, bad)
            .await
            .unwrap_err();
        assert_eq!(status_code(&err), 422);
    }
    let err = p
        .restore_provider_version(
            DeliveryId::new(),
            node,
            req(Some("no-such-version".into()), RestoreTarget::InPlace),
        )
        .await
        .unwrap_err();
    assert!(matches!(status_code(&err), 404 | 422), "{err}");

    // Another tenant and an unknown node.
    let mut foreign = req(Some(version.clone()), RestoreTarget::InPlace);
    foreign.tenant_id = TenantId::new();
    let err = p
        .restore_provider_version(DeliveryId::new(), node, foreign)
        .await
        .unwrap_err();
    assert_eq!(status_code(&err), 404);
    let err = p
        .restore_provider_version(
            DeliveryId::new(),
            NodeId::new(),
            req(Some(version), RestoreTarget::InPlace),
        )
        .await
        .unwrap_err();
    assert_eq!(status_code(&err), 404);
}

/// `exportTypedNode`: full and masked data, never a platform secret, idempotent, refusals and
/// isolation.
pub(crate) async fn export(f: &dyn Fixture) {
    let p = f.provider();
    assert!(
        p.descriptor().has_feature(Feature::Export),
        "the suite runs on providers with export"
    );
    let tenant = TenantId::new();
    let (node, m) = create_seeded(f, tenant).await;
    let secrets = f.secrets(tenant, node).await;
    let platform = f.platform();

    let req = |data: DataCopy, by: ActorPair| ExportRequest {
        tenant_id: tenant,
        format: f.export_format(),
        data: Some(data),
        upload: platform.grant_upload(tenant),
        by,
    };

    for (data, check) in [
        (DataCopy::Full, holds_all as fn(&str, &Markers) -> bool),
        (DataCopy::Masked, holds_masked_only),
    ] {
        let r = req(data, user());
        let delivery = DeliveryId::new();
        let started = p
            .export_typed_node(delivery, node, r.clone())
            .await
            .unwrap();
        assert_eq!(started.node_id, node);
        let bytes = wait_upload(f, &r.upload).await;
        let text = String::from_utf8_lossy(&bytes);
        assert!(check(&text, &m), "{data:?}: {text}");
        for s in &secrets {
            assert!(
                !text.contains(s.as_str()),
                "the export holds a platform secret"
            );
        }
        // The report names the file node that holds the export.
        let (file, _) = platform.upload(r.upload.upload_id).unwrap();
        let mut reported = false;
        for _ in 0..200 {
            reported = platform.reports().iter().any(|rep| {
                rep.tenant_id == tenant
                    && rep.status.node_id == node
                    && rep.status.export == Some(ExportRef { node_id: file })
            });
            if reported {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(reported, "a report with the export file");

        // The same delivery again writes nothing more. Count only this tenant's uploads:
        // other cases may share the platform and export at the same time.
        let uploads = platform.upload_count_for(tenant);
        assert_eq!(
            p.export_typed_node(delivery, node, r).await.unwrap(),
            started
        );
        assert_eq!(platform.upload_count_for(tenant), uploads);
    }

    // An agent never gets full data.
    let r = req(DataCopy::Full, agent());
    match p
        .export_typed_node(DeliveryId::new(), node, r.clone())
        .await
    {
        Err(e) => assert!(matches!(status_code(&e), 403 | 422), "{e}"),
        Ok(_) => {
            let bytes = wait_upload(f, &r.upload).await;
            let text = String::from_utf8_lossy(&bytes);
            assert!(!text.contains(&m.personal) && !text.contains(&m.secret));
        }
    }

    // An unknown format, another tenant, an unknown node.
    let mut bad = req(DataCopy::Masked, user());
    bad.format = "no-such-format".into();
    let err = p
        .export_typed_node(DeliveryId::new(), node, bad.clone())
        .await
        .unwrap_err();
    assert_eq!(status_code(&err), 422);
    assert!(platform.upload(bad.upload.upload_id).is_none());

    let other = TenantId::new();
    let foreign = ExportRequest {
        tenant_id: other,
        format: f.export_format(),
        data: Some(DataCopy::Full),
        upload: platform.grant_upload(other),
        by: user(),
    };
    let err = p
        .export_typed_node(DeliveryId::new(), node, foreign.clone())
        .await
        .unwrap_err();
    assert_eq!(status_code(&err), 404);
    assert!(platform.upload(foreign.upload.upload_id).is_none());
    let err = p
        .export_typed_node(
            DeliveryId::new(),
            NodeId::new(),
            req(DataCopy::Masked, user()),
        )
        .await
        .unwrap_err();
    assert_eq!(status_code(&err), 404);
}

/// Every case, in order.
pub(crate) async fn run_all(f: &dyn Fixture) {
    descriptor(f).await;
    validate(f).await;
    create_node(f).await;
    apply(f).await;
    status(f).await;
    hooks(f).await;
    provider_versions(f).await;
    restore(f).await;
    export(f).await;
}

// ------------------------------------------------------------------- the memory fixture

struct MemoryFixture {
    platform: Arc<MemoryPlatform>,
    provider: Arc<MemoryProvider>,
}

impl MemoryFixture {
    fn new() -> Self {
        let platform = Arc::new(MemoryPlatform::new());
        let provider = Arc::new(MemoryProvider::new(platform.clone()));
        Self { platform, provider }
    }
}

#[async_trait]
impl Fixture for MemoryFixture {
    fn provider(&self) -> Arc<dyn Provider> {
        self.provider.clone()
    }
    fn platform(&self) -> Arc<MemoryPlatform> {
        self.platform.clone()
    }
    fn type_name(&self) -> String {
        MemoryProvider::TYPE.into()
    }
    fn branch_type(&self) -> Option<String> {
        Some(MemoryProvider::CHILD_TYPE.into())
    }
    fn manifest(&self) -> Value {
        serde_json::json!({ "title": "Notes" })
    }
    fn changed_manifest(&self) -> Value {
        serde_json::json!({ "title": "Notes, renamed", "limit": 20 })
    }
    fn refused_change(&self) -> (Value, String) {
        (
            serde_json::json!({ "title": "Notes", "kind": "rich" }),
            "/kind".into(),
        )
    }
    fn refused_manifest(&self) -> Value {
        serde_json::json!({ "title": " " })
    }
    fn export_format(&self) -> String {
        MemoryProvider::FORMAT.into()
    }
    fn tenant_export_data(&self, upload: &UploadTarget) -> serde_json::Map<String, Value> {
        let mut d = serde_json::Map::new();
        d.insert("upload".into(), serde_json::to_value(upload).unwrap());
        d
    }
    async fn seed(&self, tenant: TenantId, node: NodeId, m: &Markers) {
        self.provider
            .put_rows(
                tenant,
                node,
                vec![
                    Row::new("n", &m.normal, RowClass::Normal),
                    Row::new("p", &m.personal, RowClass::Personal),
                    Row::new("s", &m.secret, RowClass::Secret),
                ],
            )
            .unwrap();
    }
    async fn content(&self, tenant: TenantId, node: NodeId) -> Option<String> {
        self.provider
            .rows(tenant, node)
            .map(|rows| serde_json::to_string(&rows).unwrap())
    }
    async fn make_version(&self, tenant: TenantId, node: NodeId, label: &str) -> String {
        self.provider
            .create_restore_point(tenant, node, label, Some(user()))
            .unwrap()
    }
    async fn secrets(&self, tenant: TenantId, node: NodeId) -> Vec<String> {
        self.provider.key(tenant, node).into_iter().collect()
    }
}

mod memory_runs {
    use super::*;

    #[tokio::test]
    async fn descriptor() {
        super::descriptor(&MemoryFixture::new()).await;
    }
    #[tokio::test]
    async fn validate() {
        super::validate(&MemoryFixture::new()).await;
    }
    #[tokio::test]
    async fn create() {
        super::create_node(&MemoryFixture::new()).await;
    }
    #[tokio::test]
    async fn apply() {
        super::apply(&MemoryFixture::new()).await;
    }
    #[tokio::test]
    async fn status() {
        super::status(&MemoryFixture::new()).await;
    }
    #[tokio::test]
    async fn hooks() {
        super::hooks(&MemoryFixture::new()).await;
    }
    #[tokio::test]
    async fn provider_versions() {
        super::provider_versions(&MemoryFixture::new()).await;
    }
    #[tokio::test]
    async fn restore() {
        super::restore(&MemoryFixture::new()).await;
    }
    #[tokio::test]
    async fn export() {
        super::export(&MemoryFixture::new()).await;
    }
    /// All cases on one fixture, in parallel: cases do not share tenants.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn all_cases_share_one_provider() {
        let f = Arc::new(MemoryFixture::new());
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let f = f.clone();
            tasks.push(tokio::spawn(async move { run_all(f.as_ref()).await }));
        }
        for t in tasks {
            t.await.unwrap();
        }
    }
    /// A failed apply reports `failed` with a reason and keeps the applied version.
    #[tokio::test]
    async fn failed_apply() {
        let f = MemoryFixture::new();
        let p = f.provider();
        let tenant = TenantId::new();
        let (node, v1) = super::create(&f, tenant).await;
        let r = p
            .apply_manifest(
                DeliveryId::new(),
                node,
                ApplyManifestRequest {
                    tenant_id: tenant,
                    version_id: VersionId::new(),
                    manifest: serde_json::json!({ "title": "x", "fail": true }),
                    previous_version_id: v1,
                    by: user(),
                },
            )
            .await
            .unwrap();
        let s = r.into_inner();
        assert_eq!(s.state, ProviderState::Failed);
        assert_eq!(s.reason.as_deref(), Some("apply_failed"));
        assert_eq!(s.observed_version_id, Some(v1));
    }
}

// ------------------------------------------------------------------------ the contract

/// The wire types against `contracts/providers/provider.openapi.yaml`: each serialized value
/// holds the `required` fields of its schema, and each Rust enum has the values of the
/// contract's `enum`.
mod contract {
    use super::*;

    const SPEC: &str = include_str!("../../../../contracts/providers/provider.openapi.yaml");

    /// The lines of `components.schemas.<name>` (4 spaces of indent for the name).
    fn schema_block(name: &str) -> Vec<&'static str> {
        let head = format!("    {name}:");
        let start = SPEC
            .lines()
            .position(|l| l == head)
            .unwrap_or_else(|| panic!("schema {name} is in the contract"));
        SPEC.lines()
            .skip(start + 1)
            .take_while(|l| l.is_empty() || l.starts_with("      "))
            .collect()
    }

    /// The items of the first `<key>: [a, b]` line at 6 spaces of indent.
    fn list(name: &str, key: &str) -> Vec<String> {
        let prefix = format!("      {key}: [");
        let line = schema_block(name)
            .into_iter()
            .find(|l| l.starts_with(&prefix))
            .unwrap_or_else(|| panic!("{name} has {key}"));
        line[prefix.len()..line.len() - 1]
            .split(',')
            .map(|s| s.trim().to_string())
            .collect()
    }

    fn assert_required(name: &str, value: &Value) {
        let obj = value.as_object().expect("an object");
        for field in list(name, "required") {
            assert!(
                obj.contains_key(&field),
                "{name}.{field} is required: {value}"
            );
        }
    }

    fn assert_enum<T: Serialize>(name: &str, all: &[T]) {
        let ours: Vec<String> = all
            .iter()
            .map(|v| {
                serde_json::to_value(v)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(ours, list(name, "enum"), "{name}");
    }

    fn status() -> ProviderStatus {
        ProviderStatus {
            node_id: NodeId::new(),
            state: ProviderState::Ready,
            generation: 1,
            observed_version_id: None,
            reason: None,
            message: None,
            detail: None,
            export: None,
            at: crate::now(),
        }
    }

    #[test]
    fn enums_match() {
        assert_enum("HookKind", HookKind::ALL);
        assert_enum(
            "ProviderState",
            &[
                ProviderState::Provisioning,
                ProviderState::Ready,
                ProviderState::Updating,
                ProviderState::Degraded,
                ProviderState::Suspended,
                ProviderState::Failed,
                ProviderState::Deleted,
            ],
        );
        assert_enum(
            "DataCopy",
            &[DataCopy::Masked, DataCopy::SchemaOnly, DataCopy::Full],
        );
        assert_enum(
            "ProviderVersionKind",
            &[
                ProviderVersionKind::RestorePoint,
                ProviderVersionKind::Branch,
                ProviderVersionKind::Snapshot,
                ProviderVersionKind::Commit,
                ProviderVersionKind::Deployment,
            ],
        );
        let features: Vec<String> = [
            Feature::ProviderVersions,
            Feature::RestoreInPlace,
            Feature::RestoreNewNode,
            Feature::Export,
            Feature::Thumbnail,
            Feature::Preview,
            Feature::SearchText,
        ]
        .iter()
        .map(|v| {
            serde_json::to_value(v)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
        let line = schema_block("ProviderDescriptor")
            .into_iter()
            .find(|l| l.trim_start().starts_with("enum: [provider_versions"))
            .unwrap();
        assert!(
            line.ends_with(&format!("[{}]", features.join(", "))),
            "{line}"
        );
        let principals: Vec<String> = [
            ProviderPrincipalKind::User,
            ProviderPrincipalKind::Group,
            ProviderPrincipalKind::App,
            ProviderPrincipalKind::Agent,
            ProviderPrincipalKind::ShareLink,
            ProviderPrincipalKind::Provider,
        ]
        .iter()
        .map(|v| {
            serde_json::to_value(v)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
        let line = schema_block("PrincipalRef")
            .into_iter()
            .find(|l| l.trim_start().starts_with("enum: ["))
            .unwrap();
        assert!(
            line.ends_with(&format!("[{}]", principals.join(", "))),
            "{line}"
        );
    }

    #[test]
    fn required_fields_are_written() {
        let by = user();
        assert_required("ActorPair", &serde_json::to_value(by).unwrap());
        assert_required("PrincipalRef", &serde_json::to_value(by.actor).unwrap());
        let d = MemoryProvider::default_descriptor();
        let dv = serde_json::to_value(&d).unwrap();
        assert_required("ProviderDescriptor", &dv);
        assert_required("TypeDescriptor", &dv["types"][0]);
        assert_eq!(dv["base_url"], Value::Null, "base_url is written as null");
        assert_required(
            "ValidateRequest",
            &serde_json::to_value(ValidateRequest {
                tenant_id: TenantId::new(),
                node_id: None,
                type_name: "t".into(),
                manifest: serde_json::json!({}),
                current: None,
                plan: None,
                by: None,
            })
            .unwrap(),
        );
        assert_required(
            "ValidationResult",
            &serde_json::to_value(ValidationResult {
                valid: true,
                normalized: None,
                problems: vec![],
            })
            .unwrap(),
        );
        assert_required(
            "CreateTypedNodeRequest",
            &serde_json::to_value(CreateTypedNodeRequest {
                tenant_id: TenantId::new(),
                node_id: NodeId::new(),
                parent_id: None,
                type_name: "t".into(),
                version_id: VersionId::new(),
                manifest: serde_json::json!({}),
                source: Source::Empty,
                by,
            })
            .unwrap(),
        );
        assert_required(
            "ApplyManifestRequest",
            &serde_json::to_value(ApplyManifestRequest {
                tenant_id: TenantId::new(),
                version_id: VersionId::new(),
                manifest: serde_json::json!({}),
                previous_version_id: VersionId::new(),
                by,
            })
            .unwrap(),
        );
        let sv = serde_json::to_value(status()).unwrap();
        assert_required("ProviderStatus", &sv);
        let report = serde_json::to_value(StatusReport {
            status: status(),
            tenant_id: TenantId::new(),
            delivery_id: None,
        })
        .unwrap();
        assert_required("ProviderStatus", &report);
        assert_eq!(
            report["tenant_id"].as_str().map(str::len),
            Some(36),
            "StatusReport adds tenant_id at the top level"
        );
        assert_required(
            "HookDelivery",
            &serde_json::to_value(hook(HookKind::TenantDeleted, TenantId::new(), None)).unwrap(),
        );
        assert_required(
            "HookResult",
            &serde_json::to_value(HookResult { done: true }).unwrap(),
        );
        let version = ProviderVersion {
            id: "v".into(),
            kind: ProviderVersionKind::RestorePoint,
            label: None,
            reason: None,
            created_at: crate::now(),
            size_bytes: None,
            node_id: None,
            by: None,
            restore_modes: vec![RestoreTarget::InPlace],
        };
        assert_required("ProviderVersion", &serde_json::to_value(&version).unwrap());
        assert_required(
            "ProviderVersionPage",
            &serde_json::to_value(ProviderVersionPage {
                items: vec![version],
                next_cursor: None,
                window: None,
            })
            .unwrap(),
        );
        assert_required(
            "RestoreRequest",
            &serde_json::to_value(RestoreRequest {
                tenant_id: TenantId::new(),
                version: None,
                at: None,
                target: RestoreTarget::NewNode,
                target_node_id: None,
                by,
            })
            .unwrap(),
        );
        assert_required(
            "ExportRequest",
            &serde_json::to_value(ExportRequest {
                tenant_id: TenantId::new(),
                format: "json".into(),
                data: None,
                upload: UploadTarget {
                    upload_id: UploadId::new(),
                    token: "t".into(),
                },
                by,
            })
            .unwrap(),
        );
        assert_required(
            "CheckRequest",
            &serde_json::to_value(CheckRequest {
                tenant_id: TenantId::new(),
                node_id: NodeId::new(),
                principal: by,
                permission: "view".into(),
            })
            .unwrap(),
        );
        assert_required(
            "CheckResult",
            &serde_json::to_value(CheckResult {
                allowed: false,
                role: None,
                expires_at: None,
            })
            .unwrap(),
        );
    }

    #[test]
    fn sources_are_tagged_by_kind() {
        let n = NodeId::new();
        assert_eq!(
            serde_json::to_value(Source::Empty).unwrap(),
            serde_json::json!({ "kind": "empty" })
        );
        assert_eq!(
            serde_json::to_value(Source::Branch {
                node_id: n,
                data: DataCopy::SchemaOnly
            })
            .unwrap(),
            serde_json::json!({ "kind": "branch", "node_id": n, "data": "schema_only" })
        );
        let copy: Source =
            serde_json::from_value(serde_json::json!({ "kind": "copy", "node_id": n })).unwrap();
        assert_eq!(
            copy,
            Source::Copy {
                node_id: n,
                data: None
            }
        );
        let t: Source =
            serde_json::from_value(serde_json::json!({ "kind": "template", "node_id": n }))
                .unwrap();
        assert!(matches!(t, Source::Template { .. }));
        // A branch needs `data` (required in the contract).
        assert!(
            serde_json::from_value::<Source>(serde_json::json!({ "kind": "branch", "node_id": n }))
                .is_err()
        );
    }

    #[test]
    fn actor_pairs_come_from_core_actors() {
        let u = UserId::new();
        let pair: ActorPair = Actor::user(u).into();
        assert_eq!(pair.actor.kind, ProviderPrincipalKind::User);
        assert_eq!(pair.actor.id, u.0);
        assert_eq!(pair.on_behalf_of, None);
        assert_eq!(
            serde_json::to_value(pair).unwrap()["on_behalf_of"],
            Value::Null
        );
    }
}
