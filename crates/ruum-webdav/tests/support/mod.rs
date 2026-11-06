//! A small in-memory node tree and transfer store for the WebDAV tests, and the services around
//! it. The real in-memory stores belong to the nodes and blobs areas; until they are merged, the
//! WebDAV tests run on this fake. It does not check permissions (the WebDAV layer does, with
//! `authz::require`), but it writes the relationship tuples of the tree (`parent`, `tenant`) and
//! one journal entry per change, like the real stores.

#![allow(dead_code)]

use async_trait::async_trait;
use bytes::Bytes;
use ruum_api::{ApiConfig, AppState};
use ruum_core::actor::Context;
use ruum_core::agents::memory::MemoryAgentStore;
use ruum_core::agents::oauth::MemoryOAuthServer;
use ruum_core::audit::memory::MemoryAuditLog;
use ruum_core::authz::memory::MemoryAuthz;
use ruum_core::authz::{Authz, RelationshipUpdate, tuples};
use ruum_core::blobs::memory::MemoryBlobStore;
use ruum_core::blobs::{
    ByteRange, Content, Hash, NewUpload, Transfers, Upload, UploadStatus, UploadTarget,
    chunking_by_id,
};
use ruum_core::error::{Error, ErrorCode, Result};
use ruum_core::identity::TokenIssuer;
use ruum_core::identity::idempotency::MemoryIdempotencyStore;
use ruum_core::identity::memory::{
    MemoryArea, MemoryDeps, MemoryIdentityStore, MemoryOidcClient, MemoryTokenIssuer,
};
use ruum_core::journal::memory::MemoryJournal;
use ruum_core::journal::{ChangeInfo, EventType, NewEntry, Written};
use ruum_core::nodes::*;
use ruum_core::paging::{Page, PageRequest};
use ruum_core::search::memory::MemorySearch;
use ruum_core::sharing::memory::MemorySharingStore;
use ruum_core::tags::memory::MemoryTagStore;
use ruum_core::tenants::memory::MemoryTenantStore;
use ruum_core::tenants::{RootCreated, Tenant};
use ruum_core::versions::Version;
use ruum_core::versions::memory::{MemoryDiffService, MemoryVersionStore};
use ruum_core::{NodeId, Services, TenantId, UploadId, UserId, VersionId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

struct StoredVersion {
    version: Version,
    chunks: Vec<Hash>,
}

#[derive(Default)]
struct State {
    roots: HashMap<TenantId, NodeId>,
    nodes: HashMap<NodeId, Node>,
    versions: HashMap<VersionId, StoredVersion>,
    chunks: HashMap<(TenantId, Hash), Bytes>,
    uploads: HashMap<UploadId, Upload>,
}

/// The fake node tree and transfer store.
pub struct FakeTree {
    authz: Arc<MemoryAuthz>,
    journal: Arc<MemoryJournal>,
    st: Mutex<State>,
}

fn not_found(what: &str) -> Error {
    Error::not_found(what)
}

fn api(code: ErrorCode, msg: &str) -> Error {
    Error::api(code, msg)
}

impl FakeTree {
    pub fn new(authz: Arc<MemoryAuthz>, journal: Arc<MemoryJournal>) -> Self {
        Self {
            authz,
            journal,
            st: Mutex::new(State::default()),
        }
    }

    /// A node by ID, also trashed ones, in any tenant (for test checks).
    pub fn node(&self, id: NodeId) -> Option<Node> {
        self.st.lock().unwrap().nodes.get(&id).cloned()
    }

    /// The versions of a node, oldest first.
    pub fn versions_of(&self, id: NodeId) -> Vec<Version> {
        let st = self.st.lock().unwrap();
        let mut v: Vec<Version> = st
            .versions
            .values()
            .filter(|v| v.version.node_id == id)
            .map(|v| v.version.clone())
            .collect();
        v.sort_by_key(|v| v.id);
        v
    }

    /// The live child of `parent` with this name (case-insensitive).
    pub fn child(&self, parent: NodeId, name: &str) -> Option<Node> {
        let st = self.st.lock().unwrap();
        live_child(&st, parent, name).cloned()
    }

    fn change(&self, ctx: &Context, node: &Node, event: EventType) -> ChangeInfo {
        let e = self
            .journal
            .append(
                ctx,
                NewEntry {
                    node_id: Some(node.id),
                    revision: Some(node.revision),
                    event,
                    data: serde_json::json!({ "name": node.name }),
                },
            )
            .expect("journal append");
        ChangeInfo {
            seq: Some(e.seq),
            actor: ctx.actor,
            at: e.at,
        }
    }

    async fn relate(&self, updates: Vec<RelationshipUpdate>) {
        self.authz.write(updates).await.expect("authz write");
    }
}

fn live_child<'a>(st: &'a State, parent: NodeId, name: &str) -> Option<&'a Node> {
    st.nodes.values().find(|n| {
        n.parent_id == Some(parent) && n.trashed_at.is_none() && n.name.eq_ignore_ascii_case(name)
    })
}

/// Whether the node and all its ancestors are live.
fn is_live(st: &State, id: NodeId) -> bool {
    let mut cur = st.nodes.get(&id);
    while let Some(n) = cur {
        if n.trashed_at.is_some() {
            return false;
        }
        cur = n.parent_id.and_then(|p| st.nodes.get(&p));
    }
    true
}

fn get_in(st: &State, ctx: &Context, id: NodeId) -> Result<Node> {
    st.nodes
        .get(&id)
        .filter(|n| n.tenant_id == ctx.tenant)
        .cloned()
        .ok_or_else(|| not_found("node"))
}

fn new_node(ctx: &Context, parent: NodeId, name: String, kind: NodeKind) -> Node {
    let now = ruum_core::now();
    Node {
        id: NodeId::new(),
        tenant_id: ctx.tenant,
        parent_id: Some(parent),
        name,
        kind,
        node_type: None,
        link_kind: None,
        link_target: None,
        head_version: None,
        owner_id: ctx
            .actor
            .effective_user()
            .unwrap_or(UserId(uuid::Uuid::nil())),
        size_bytes: 0,
        revision: 1,
        trashed_at: None,
        created_at: now,
        updated_at: now,
        created_by: ctx.actor,
        updated_by: ctx.actor,
        my_role: None,
    }
}

fn check_free(st: &State, parent: NodeId, name: &str, except: Option<NodeId>) -> Result<()> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(api(ErrorCode::ValidationFailed, "bad name"));
    }
    match live_child(st, parent, name) {
        Some(n) if Some(n.id) != except => Err(api(ErrorCode::NameTaken, "name taken")),
        _ => Ok(()),
    }
}

fn live_folder(st: &State, ctx: &Context, id: NodeId) -> Result<Node> {
    let n = get_in(st, ctx, id)?;
    if n.kind != NodeKind::Folder || !is_live(st, id) {
        return Err(not_found("folder"));
    }
    Ok(n)
}

impl RootCreated for FakeTree {
    fn root_created(&self, tenant: &Tenant, owner: UserId) {
        let now = ruum_core::now();
        let mut st = self.st.lock().unwrap();
        st.roots.insert(tenant.id, tenant.root_node_id);
        st.nodes.insert(
            tenant.root_node_id,
            Node {
                id: tenant.root_node_id,
                tenant_id: tenant.id,
                parent_id: None,
                name: "root".into(),
                kind: NodeKind::Folder,
                node_type: None,
                link_kind: None,
                link_target: None,
                head_version: None,
                owner_id: owner,
                size_bytes: 0,
                revision: 1,
                trashed_at: None,
                created_at: now,
                updated_at: now,
                created_by: ruum_core::Actor::user(owner),
                updated_by: ruum_core::Actor::user(owner),
                my_role: None,
            },
        );
    }
}

#[async_trait]
impl NodeStore for FakeTree {
    async fn root(&self, ctx: &Context) -> Result<Node> {
        let root = {
            let st = self.st.lock().unwrap();
            let id = st.roots.get(&ctx.tenant).ok_or_else(|| not_found("root"))?;
            get_in(&st, ctx, *id)?
        };
        // As the nodes area: the root is not secret, so a caller who cannot see it gets 403.
        match ruum_core::authz::require(
            &*self.authz,
            self,
            ctx,
            root.id,
            ruum_core::authz::Permission::View,
        )
        .await
        {
            Err(Error::Api(e)) if e.code == ErrorCode::NotFound => Err(Error::permission_denied()),
            other => other.map(|()| root),
        }
    }

    async fn create(&self, ctx: &Context, req: NewNode) -> Result<Written<Node>> {
        let node = {
            let mut st = self.st.lock().unwrap();
            live_folder(&st, ctx, req.parent_id)?;
            check_free(&st, req.parent_id, &req.name, None)?;
            let mut n = new_node(ctx, req.parent_id, req.name, req.kind);
            n.node_type = req.node_type;
            st.nodes.insert(n.id, n.clone());
            n
        };
        self.relate(vec![
            RelationshipUpdate::Touch(tuples::node_parent(node.id, req.parent_id)),
            RelationshipUpdate::Touch(tuples::node_tenant(node.id, ctx.tenant)),
        ])
        .await;
        let change = self.change(ctx, &node, EventType::NodeCreated);
        Ok(Written {
            value: node,
            change,
        })
    }

    async fn get(&self, ctx: &Context, id: NodeId) -> Result<Node> {
        get_in(&self.st.lock().unwrap(), ctx, id)
    }

    async fn get_by_path(&self, ctx: &Context, path: &str) -> Result<Node> {
        let st = self.st.lock().unwrap();
        let mut cur = *st.roots.get(&ctx.tenant).ok_or_else(|| not_found("root"))?;
        for name in path.split('/').filter(|s| !s.is_empty()) {
            cur = live_child(&st, cur, name)
                .ok_or_else(|| not_found("node"))?
                .id;
        }
        get_in(&st, ctx, cur)
    }

    async fn list_children(
        &self,
        ctx: &Context,
        parent: NodeId,
        _order: ChildOrder,
        _page: PageRequest,
    ) -> Result<Page<Node>> {
        let st = self.st.lock().unwrap();
        live_folder(&st, ctx, parent)?;
        let mut items: Vec<Node> = st
            .nodes
            .values()
            .filter(|n| n.parent_id == Some(parent) && n.trashed_at.is_none())
            .cloned()
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Page {
            items,
            next_cursor: None,
        })
    }

    async fn rename(
        &self,
        ctx: &Context,
        id: NodeId,
        name: String,
        _on_conflict: OnConflict,
        _if_revision: Option<i64>,
    ) -> Result<Written<Node>> {
        let node = {
            let mut st = self.st.lock().unwrap();
            let n = get_in(&st, ctx, id)?;
            let parent = n
                .parent_id
                .ok_or_else(|| api(ErrorCode::Conflict, "root"))?;
            check_free(&st, parent, &name, Some(id))?;
            let n = st.nodes.get_mut(&id).expect("node");
            n.name = name;
            n.revision += 1;
            n.updated_by = ctx.actor;
            n.clone()
        };
        let change = self.change(ctx, &node, EventType::NodeRenamed);
        Ok(Written {
            value: node,
            change,
        })
    }

    async fn move_node(
        &self,
        ctx: &Context,
        id: NodeId,
        req: MoveRequest,
    ) -> Result<Written<Node>> {
        let (node, old_parent) = {
            let mut st = self.st.lock().unwrap();
            let n = get_in(&st, ctx, id)?;
            live_folder(&st, ctx, req.parent_id)?;
            // Not into itself or its subtree.
            let mut cur = Some(req.parent_id);
            while let Some(c) = cur {
                if c == id {
                    return Err(api(ErrorCode::Conflict, "into its own subtree"));
                }
                cur = st.nodes.get(&c).and_then(|n| n.parent_id);
            }
            let name = req.name.unwrap_or(n.name.clone());
            check_free(&st, req.parent_id, &name, Some(id))?;
            let old = n
                .parent_id
                .ok_or_else(|| api(ErrorCode::Conflict, "root"))?;
            let n = st.nodes.get_mut(&id).expect("node");
            n.parent_id = Some(req.parent_id);
            n.name = name;
            n.revision += 1;
            n.updated_by = ctx.actor;
            (n.clone(), old)
        };
        self.relate(vec![
            RelationshipUpdate::Delete(tuples::node_parent(id, old_parent)),
            RelationshipUpdate::Touch(tuples::node_parent(id, req.parent_id)),
        ])
        .await;
        let change = self.change(ctx, &node, EventType::NodeMoved);
        Ok(Written {
            value: node,
            change,
        })
    }

    async fn copy(&self, ctx: &Context, id: NodeId, req: CopyRequest) -> Result<Written<Node>> {
        // Copies the node and its live subtree.
        let mut made = Vec::new();
        let root = {
            let mut st = self.st.lock().unwrap();
            let src = get_in(&st, ctx, id)?;
            live_folder(&st, ctx, req.parent_id)?;
            let name = req.name.unwrap_or(src.name.clone());
            check_free(&st, req.parent_id, &name, None)?;
            let mut stack = vec![(id, req.parent_id, Some(name))];
            let mut root = None;
            while let Some((from, to, rename)) = stack.pop() {
                let s = st.nodes.get(&from).cloned().expect("node");
                let mut n = new_node(ctx, to, rename.unwrap_or(s.name.clone()), s.kind);
                n.node_type = s.node_type.clone();
                n.size_bytes = s.size_bytes;
                if let Some(v) = s.head_version {
                    let old = &st.versions[&v];
                    let mut version = old.version.clone();
                    version.id = VersionId::new();
                    version.node_id = n.id;
                    version.actor = ctx.actor;
                    let chunks = old.chunks.clone();
                    n.head_version = Some(version.id);
                    st.versions
                        .insert(version.id, StoredVersion { version, chunks });
                }
                if root.is_none() {
                    root = Some(n.id);
                }
                for c in st
                    .nodes
                    .values()
                    .filter(|c| c.parent_id == Some(from) && c.trashed_at.is_none())
                {
                    stack.push((c.id, n.id, None));
                }
                made.push((n.id, to));
                st.nodes.insert(n.id, n);
            }
            root.expect("a root")
        };
        let mut ups = Vec::new();
        for (n, p) in &made {
            ups.push(RelationshipUpdate::Touch(tuples::node_parent(*n, *p)));
            ups.push(RelationshipUpdate::Touch(tuples::node_tenant(
                *n, ctx.tenant,
            )));
        }
        self.relate(ups).await;
        let node = self.node(root).expect("copy");
        let change = self.change(ctx, &node, EventType::NodeCopied);
        Ok(Written {
            value: node,
            change,
        })
    }

    async fn trash(
        &self,
        ctx: &Context,
        id: NodeId,
        _if_revision: Option<i64>,
    ) -> Result<Written<Node>> {
        let node = {
            let mut st = self.st.lock().unwrap();
            let n = get_in(&st, ctx, id)?;
            if n.parent_id.is_none() {
                return Err(api(ErrorCode::Conflict, "the root cannot be trashed"));
            }
            let n = st.nodes.get_mut(&id).expect("node");
            n.trashed_at = Some(ruum_core::now());
            n.revision += 1;
            n.clone()
        };
        let change = self.change(ctx, &node, EventType::NodeTrashed);
        Ok(Written {
            value: node,
            change,
        })
    }

    async fn restore(
        &self,
        ctx: &Context,
        id: NodeId,
        _req: RestoreRequest,
    ) -> Result<Written<Node>> {
        let node = {
            let mut st = self.st.lock().unwrap();
            get_in(&st, ctx, id)?;
            let n = st.nodes.get_mut(&id).expect("node");
            n.trashed_at = None;
            n.revision += 1;
            n.clone()
        };
        let change = self.change(ctx, &node, EventType::NodeRestored);
        Ok(Written {
            value: node,
            change,
        })
    }

    async fn list_trash(&self, _: &Context, _: bool, _: PageRequest) -> Result<Page<Node>> {
        Ok(Page::empty())
    }

    async fn purge(&self, _: &Context, _: NodeId) -> Result<Written<()>> {
        Err(api(ErrorCode::NotImplemented, "fake"))
    }

    async fn empty_trash(&self, _: &Context) -> Result<(u32, Option<ChangeInfo>)> {
        Ok((0, None))
    }

    async fn path_of(&self, ctx: &Context, id: NodeId) -> Result<String> {
        let st = self.st.lock().unwrap();
        let mut names = Vec::new();
        let mut cur = get_in(&st, ctx, id)?;
        while let Some(p) = cur.parent_id {
            names.push(cur.name.clone());
            cur = get_in(&st, ctx, p)?;
        }
        names.reverse();
        Ok(format!("/{}", names.join("/")))
    }

    async fn ancestors(&self, ctx: &Context, id: NodeId) -> Result<Vec<NodeId>> {
        let st = self.st.lock().unwrap();
        let mut out = Vec::new();
        let mut cur = get_in(&st, ctx, id)?;
        while let Some(p) = cur.parent_id {
            out.push(p);
            cur = get_in(&st, ctx, p)?;
        }
        out.reverse();
        Ok(out)
    }
}

#[async_trait]
impl Transfers for FakeTree {
    async fn create_upload(&self, ctx: &Context, req: NewUpload) -> Result<Upload> {
        let params = chunking_by_id(&req.chunking)
            .ok_or_else(|| api(ErrorCode::ValidationFailed, "unknown chunking"))?;
        if req.chunks.iter().map(|c| c.size as u64).sum::<u64>() != req.size {
            return Err(api(ErrorCode::ValidationFailed, "sizes do not add up"));
        }
        let mut st = self.st.lock().unwrap();
        match &req.target {
            UploadTarget::NewVersion { node_id, .. } => {
                let n = get_in(&st, ctx, *node_id)?;
                if n.kind != NodeKind::File {
                    return Err(api(ErrorCode::ValidationFailed, "not a file"));
                }
            }
            UploadTarget::NewFile {
                parent_id, name, ..
            } => {
                live_folder(&st, ctx, *parent_id)?;
                check_free(&st, *parent_id, name, None)?;
            }
        }
        let mut missing: Vec<Hash> = req
            .chunks
            .iter()
            .map(|c| c.hash)
            .filter(|h| !st.chunks.contains_key(&(ctx.tenant, *h)))
            .collect();
        missing.dedup();
        let up = Upload {
            id: UploadId::new(),
            tenant_id: ctx.tenant,
            target: req.target,
            content_type: req.content_type,
            size: req.size,
            content_hash: req.content_hash,
            chunks: req.chunks,
            missing,
            status: UploadStatus::Open,
            chunking: params,
            expires_at: ruum_core::now() + time::Duration::hours(1),
            created_by: ctx.actor,
        };
        st.uploads.insert(up.id, up.clone());
        Ok(up)
    }

    async fn get_upload(&self, ctx: &Context, id: UploadId) -> Result<Upload> {
        let st = self.st.lock().unwrap();
        st.uploads
            .get(&id)
            .filter(|u| u.tenant_id == ctx.tenant)
            .cloned()
            .ok_or_else(|| not_found("upload"))
    }

    async fn put_chunk(&self, ctx: &Context, id: UploadId, hash: Hash, data: Bytes) -> Result<()> {
        let mut st = self.st.lock().unwrap();
        let up = st
            .uploads
            .get(&id)
            .filter(|u| u.tenant_id == ctx.tenant && u.status == UploadStatus::Open)
            .ok_or_else(|| not_found("upload"))?;
        if !up.chunks.iter().any(|c| c.hash == hash) {
            return Err(api(
                ErrorCode::ValidationFailed,
                "not a chunk of the upload",
            ));
        }
        if *blake3::hash(&data).as_bytes() != hash.0 {
            return Err(api(ErrorCode::HashMismatch, "hash mismatch"));
        }
        st.chunks.insert((ctx.tenant, hash), data);
        Ok(())
    }

    async fn complete_upload(
        &self,
        ctx: &Context,
        id: UploadId,
    ) -> Result<Written<(Node, Version)>> {
        let (node, version, created) = {
            let mut st = self.st.lock().unwrap();
            let up = st
                .uploads
                .get(&id)
                .filter(|u| u.tenant_id == ctx.tenant && u.status == UploadStatus::Open)
                .cloned()
                .ok_or_else(|| not_found("upload"))?;
            let mut whole = blake3::Hasher::new();
            for c in &up.chunks {
                let data = st
                    .chunks
                    .get(&(ctx.tenant, c.hash))
                    .ok_or_else(|| api(ErrorCode::ChunkMissing, "chunk missing"))?;
                whole.update(data);
            }
            if *whole.finalize().as_bytes() != up.content_hash.0 {
                return Err(api(ErrorCode::HashMismatch, "content hash mismatch"));
            }
            let (node_id, created) = match &up.target {
                UploadTarget::NewVersion { node_id, .. } => (*node_id, false),
                UploadTarget::NewFile {
                    parent_id, name, ..
                } => {
                    check_free(&st, *parent_id, name, None)?;
                    let n = new_node(ctx, *parent_id, name.clone(), NodeKind::File);
                    let id = n.id;
                    st.nodes.insert(id, n);
                    (id, true)
                }
            };
            let version = Version {
                id: VersionId::new(),
                tenant_id: ctx.tenant,
                node_id,
                size_bytes: up.size as i64,
                content_hash: up.content_hash,
                content_type: up.content_type.clone(),
                restored_from: None,
                label: None,
                actor: ctx.actor,
                created_at: ruum_core::now(),
            };
            st.versions.insert(
                version.id,
                StoredVersion {
                    version: version.clone(),
                    chunks: up.chunks.iter().map(|c| c.hash).collect(),
                },
            );
            let n = st.nodes.get_mut(&node_id).expect("node");
            n.head_version = Some(version.id);
            n.size_bytes = up.size as i64;
            n.node_type = up.content_type.clone();
            if !created {
                n.revision += 1;
            }
            n.updated_at = ruum_core::now();
            n.updated_by = ctx.actor;
            let node = n.clone();
            st.uploads.get_mut(&id).expect("upload").status = UploadStatus::Completed;
            (node, version, created)
        };
        if created {
            self.relate(vec![
                RelationshipUpdate::Touch(tuples::node_parent(
                    node.id,
                    node.parent_id.expect("a parent"),
                )),
                RelationshipUpdate::Touch(tuples::node_tenant(node.id, ctx.tenant)),
            ])
            .await;
            self.change(ctx, &node, EventType::NodeCreated);
        }
        let change = self.change(ctx, &node, EventType::VersionCreated);
        Ok(Written {
            value: (node, version),
            change,
        })
    }

    async fn abort_upload(&self, ctx: &Context, id: UploadId) -> Result<()> {
        let mut st = self.st.lock().unwrap();
        let up = st
            .uploads
            .get_mut(&id)
            .filter(|u| u.tenant_id == ctx.tenant)
            .ok_or_else(|| not_found("upload"))?;
        up.status = UploadStatus::Aborted;
        Ok(())
    }

    async fn open_content(
        &self,
        ctx: &Context,
        node: NodeId,
        version: Option<VersionId>,
        range: Option<ByteRange>,
    ) -> Result<Content> {
        let st = self.st.lock().unwrap();
        let n = get_in(&st, ctx, node)?;
        let vid = version
            .or(n.head_version)
            .ok_or_else(|| not_found("version"))?;
        let v = st
            .versions
            .get(&vid)
            .filter(|v| v.version.node_id == node)
            .ok_or_else(|| not_found("version"))?;
        let mut all = Vec::new();
        for h in &v.chunks {
            all.extend_from_slice(&st.chunks[&(ctx.tenant, *h)]);
        }
        let total = all.len() as u64;
        let (start, end) = match range {
            Some(r) if r.start <= r.end && r.end < total => (r.start, r.end),
            Some(_) => return Err(api(ErrorCode::RangeNotSatisfiable, "range")),
            None => (0, total.saturating_sub(1)),
        };
        let part = if total == 0 {
            Vec::new()
        } else {
            all[start as usize..=end as usize].to_vec()
        };
        // Small pieces, so the reader sees a stream of several parts.
        let pieces: Vec<Result<Bytes>> = part
            .chunks(1000)
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect();
        Ok(Content {
            node_id: node,
            version_id: vid,
            total_size: total,
            range,
            content_type: v
                .version
                .content_type
                .clone()
                .unwrap_or_else(|| "application/octet-stream".into()),
            content_hash: v.version.content_hash,
            body: Box::pin(futures::stream::iter(pieces)),
        })
    }
}

/// The services of a test: the in-memory stores of the merged areas and the fake tree.
pub struct Harness {
    pub services: Services,
    pub tree: Arc<FakeTree>,
    pub journal: Arc<MemoryJournal>,
    pub authz: Arc<MemoryAuthz>,
}

pub fn harness() -> Harness {
    let tokens: Arc<dyn TokenIssuer> = Arc::new(MemoryTokenIssuer::default());
    let authz = Arc::new(MemoryAuthz::default());
    let journal = Arc::new(MemoryJournal::default());
    let audit = Arc::new(MemoryAuditLog::with_journal(journal.clone()));
    let tree = Arc::new(FakeTree::new(authz.clone(), journal.clone()));
    let area = Arc::new(MemoryArea::new(MemoryDeps {
        tokens: tokens.clone(),
        authz: Some(authz.clone() as Arc<dyn Authz>),
        nodes: Some(tree.clone() as Arc<dyn NodeStore>),
        journal: Some(journal.clone() as _),
        audit: Some(audit.clone() as _),
        root_created: Some(tree.clone() as Arc<dyn RootCreated>),
        ..MemoryDeps::default()
    }));
    let tenants = Arc::new(MemoryTenantStore::new(area.clone()));
    let sharing = Arc::new(MemorySharingStore::new(
        authz.clone(),
        tree.clone(),
        tenants.clone(),
        journal.clone(),
    ));
    area.set_member_cleanup(sharing.clone());
    let services = Services {
        tenants,
        identity: Arc::new(MemoryIdentityStore::new(area.clone())),
        oidc: Arc::new(MemoryOidcClient::default()),
        tokens,
        agents: Arc::new(MemoryAgentStore::new(area.clone())),
        oauth: Arc::new(MemoryOAuthServer::new(area)),
        nodes: tree.clone(),
        blobs: Arc::new(MemoryBlobStore::default()),
        transfers: tree.clone(),
        versions: Arc::new(MemoryVersionStore::default()),
        diffs: Arc::new(MemoryDiffService::default()),
        tags: Arc::new(MemoryTagStore::default()),
        authz: authz.clone(),
        sharing,
        journal: journal.clone(),
        search: Arc::new(MemorySearch::default()),
        audit,
        idempotency: Arc::new(MemoryIdempotencyStore::default()),
    };
    Harness {
        services,
        tree,
        journal,
        authz,
    }
}

/// The REST API state over the same services (for `fixtures::seed` and REST checks).
pub fn app_state(s: &Services) -> AppState {
    AppState::new(s.clone(), ApiConfig::default())
}
