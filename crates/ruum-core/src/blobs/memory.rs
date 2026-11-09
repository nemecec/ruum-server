//! In-memory blob store and content store (transfers and versions), for unit tests and for
//! running without Docker.
//!
//! [`MemoryContentStore`] implements both [`Transfers`] and [`VersionStore`] over one state, as
//! the Postgres store does over its tables. It reads nodes through [`NodeStore`], makes new
//! files with [`NodeStore::create`], and sets the head of a file through [`HeadUpdate`] (the
//! in-memory node store implements it). Without a [`HeadUpdate`] it only patches the node it
//! returns.

use super::gc::GcReport;
use super::logic::{self, check_link_may_write, version_not_found};
use super::*;
use crate::authz::{self, Authz, Role};
use crate::journal::memory::MemoryJournal;
use crate::journal::{ChangeInfo, EventType, NewEntry, Written};
use crate::nodes::{NewNode, NodeKind, NodeStore};
use crate::paging::{Page, PageRequest};
use crate::tenants::TenantStore;
use crate::tenants::limits::{Limits, plan_limits};
use crate::versions::policy::{self, HeadInfo, VersionMeta};
use crate::versions::{HeadUpdate, VersionFilter, VersionPolicy, VersionStore};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use time::Duration;

/// Chunks in a map, per tenant.
#[derive(Debug, Default)]
pub struct MemoryBlobStore {
    chunks: Mutex<HashMap<(TenantId, Hash), Bytes>>,
}

impl MemoryBlobStore {
    /// The number of stored chunks of the tenant (for tests).
    pub fn count(&self, tenant: TenantId) -> usize {
        self.chunks
            .lock()
            .expect("blob lock")
            .keys()
            .filter(|(t, _)| *t == tenant)
            .count()
    }
}

#[async_trait]
impl BlobStore for MemoryBlobStore {
    async fn put_chunk(&self, tenant: TenantId, hash: Hash, data: Bytes) -> Result<()> {
        self.chunks
            .lock()
            .expect("blob lock")
            .entry((tenant, hash))
            .or_insert(data);
        Ok(())
    }
    async fn get_chunk(&self, tenant: TenantId, hash: Hash) -> Result<Bytes> {
        self.chunks
            .lock()
            .expect("blob lock")
            .get(&(tenant, hash))
            .cloned()
            .ok_or_else(|| Error::not_found("chunk"))
    }
    async fn has_chunks(&self, tenant: TenantId, hashes: &[Hash]) -> Result<Vec<bool>> {
        let chunks = self.chunks.lock().expect("blob lock");
        Ok(hashes
            .iter()
            .map(|h| chunks.contains_key(&(tenant, *h)))
            .collect())
    }
    async fn delete_chunk(&self, tenant: TenantId, hash: Hash) -> Result<()> {
        self.chunks
            .lock()
            .expect("blob lock")
            .remove(&(tenant, hash));
        Ok(())
    }
}

/// What the content store uses from other areas.
#[derive(Clone)]
pub struct ContentDeps {
    pub nodes: Arc<dyn NodeStore>,
    pub authz: Arc<dyn Authz>,
    pub journal: Arc<MemoryJournal>,
    pub blobs: Arc<dyn BlobStore>,
    /// The plan of the tenant (limits and version history). `None`: the free plan.
    pub tenants: Option<Arc<dyn TenantStore>>,
    /// Sets the head of a file in the node store. `None`: only the returned node is patched.
    pub heads: Option<Arc<dyn HeadUpdate>>,
}

#[derive(Clone, Debug)]
struct Stored {
    version: Version,
    manifest: Manifest,
    /// The last save that went into the version (coalescing).
    updated_at: OffsetDateTime,
}

#[derive(Clone, Copy, Debug)]
struct ChunkMeta {
    created_at: OffsetDateTime,
    last_marked_at: OffsetDateTime,
}

#[derive(Default)]
struct State {
    uploads: HashMap<UploadId, Upload>,
    versions: HashMap<VersionId, Stored>,
    /// The versions of each node, oldest first. The last one is the head.
    by_node: HashMap<NodeId, Vec<VersionId>>,
    chunks: HashMap<(TenantId, Hash), ChunkMeta>,
}

impl State {
    fn head(&self, node: NodeId) -> Option<&Stored> {
        self.by_node
            .get(&node)
            .and_then(|v| v.last())
            .and_then(|id| self.versions.get(id))
    }

    fn used_bytes(&self, tenant: TenantId) -> i64 {
        self.by_node
            .values()
            .filter_map(|v| v.last())
            .filter_map(|id| self.versions.get(id))
            .filter(|s| s.version.tenant_id == tenant)
            .map(|s| s.version.size_bytes)
            .sum()
    }

    fn touch(&mut self, tenant: TenantId, hashes: &[Hash], now: OffsetDateTime) {
        for h in hashes {
            if let Some(m) = self.chunks.get_mut(&(tenant, *h)) {
                m.last_marked_at = now;
            }
        }
    }
}

/// The in-memory content store: uploads, downloads and versions.
pub struct MemoryContentStore {
    deps: ContentDeps,
    signer: ContentSigner,
    state: Mutex<State>,
}

/// The names of the earlier stubs.
pub type MemoryTransfers = MemoryContentStore;

/// One `(node, version)` content write and its journal entry.
type ContentWritten = Written<(Node, Version)>;

impl MemoryContentStore {
    pub fn new(deps: ContentDeps, signer: ContentSigner) -> Self {
        Self {
            deps,
            signer,
            state: Mutex::new(State::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("content lock")
    }

    async fn limits(&self, tenant: TenantId) -> Limits {
        match &self.deps.tenants {
            Some(t) => match t.find(tenant).await {
                Ok(t) => plan_limits(t.plan),
                Err(_) => plan_limits(crate::tenants::Plan::Free),
            },
            None => plan_limits(crate::tenants::Plan::Free),
        }
    }

    async fn policy(&self, tenant: TenantId) -> VersionPolicy {
        VersionPolicy::from_limits(&self.limits(tenant).await)
    }

    /// Loads a node after the permission check, in the tenant of `ctx`.
    async fn node(&self, ctx: &Context, id: NodeId, role: Role) -> Result<Node> {
        authz::require(&*self.deps.authz, &*self.deps.nodes, ctx, id, role).await?;
        let n = self.deps.nodes.get(ctx, id).await?;
        if n.tenant_id != ctx.tenant {
            return Err(Error::not_found("node"));
        }
        Ok(n)
    }

    /// Loads a file after the permission check: live for writes; a trashed file only for
    /// readers with `manage` (`logic::require_trashed_read`).
    async fn file(&self, ctx: &Context, id: NodeId, role: Role) -> Result<Node> {
        let n = self.node(ctx, id, role).await?;
        if n.kind != NodeKind::File {
            return Err(if role >= Role::Edit {
                logic::not_a_file_to_write(n.kind)
            } else {
                logic::not_a_file()
            });
        }
        if role >= Role::Edit && n.trashed_at.is_some() {
            return Err(Error::not_found("node"));
        }
        // Reads of a trashed file need `manage`, whatever the node store checks.
        logic::require_trashed_read(
            &*self.deps.authz,
            &*self.deps.nodes,
            ctx,
            id,
            n.trashed_at.is_some(),
        )
        .await?;
        Ok(n)
    }

    /// The node with the head of this store (when the node store has no [`HeadUpdate`]).
    fn patched(&self, mut n: Node) -> Node {
        if let Some(h) = self.lock().head(n.id) {
            n.head_version = Some(h.version.id);
            n.size_bytes = h.version.size_bytes;
        }
        n
    }

    /// The version `id` of the node, or the head.
    fn stored(&self, tenant: TenantId, node: NodeId, id: Option<VersionId>) -> Result<Stored> {
        let st = self.lock();
        let s = match id {
            Some(id) => st.versions.get(&id),
            None => st.head(node),
        };
        match s {
            Some(s) if s.version.node_id == node && s.version.tenant_id == tenant => Ok(s.clone()),
            Some(_) => Err(version_not_found()),
            None if id.is_none() => Err(logic::api(
                ErrorCode::NotFound,
                "the file has no content yet",
                "upload content first (PUT /v1/nodes/{node_id}/content)",
            )),
            None => Err(version_not_found()),
        }
    }

    /// Makes a new head version (or merges the save into the head, when `coalesce` and the
    /// version policy allow it) and writes the entry.
    async fn commit(
        &self,
        ctx: &Context,
        node: Node,
        manifest: Manifest,
        content_type: Option<String>,
        restored_from: Option<VersionId>,
        coalesce: bool,
    ) -> Result<ContentWritten> {
        let policy = self.policy(ctx.tenant).await;
        let now = crate::now();
        let (version, previous, coalesced) = {
            let mut st = self.lock();
            let ids = st.by_node.get(&node.id).cloned().unwrap_or_default();
            let head = ids.last().and_then(|id| st.versions.get(id)).cloned();
            let merge = coalesce
                && restored_from.is_none()
                && head.as_ref().is_some_and(|h| {
                    policy::may_coalesce(
                        &HeadInfo {
                            actor: h.version.actor,
                            label: h.version.label.clone(),
                            restored_from: h.version.restored_from,
                            updated_at: h.updated_at,
                        },
                        &ctx.actor,
                        now,
                        &policy,
                    )
                });
            match head {
                Some(h) if merge => {
                    let s = st.versions.get_mut(&h.version.id).expect("head");
                    s.version.size_bytes = manifest.size as i64;
                    s.version.content_hash = manifest.content_hash;
                    s.version.content_type = content_type;
                    s.manifest = manifest;
                    s.updated_at = now;
                    let previous = ids.len().checked_sub(2).map(|i| ids[i]);
                    (s.version.clone(), previous, true)
                }
                _ => {
                    let v = Version {
                        id: VersionId::new(),
                        tenant_id: ctx.tenant,
                        node_id: node.id,
                        size_bytes: manifest.size as i64,
                        content_hash: manifest.content_hash,
                        content_type,
                        restored_from,
                        label: None,
                        actor: ctx.actor,
                        created_at: now,
                    };
                    st.versions.insert(
                        v.id,
                        Stored {
                            version: v.clone(),
                            manifest,
                            updated_at: now,
                        },
                    );
                    st.by_node.entry(node.id).or_default().push(v.id);
                    (v, head.map(|h| h.version.id), false)
                }
            }
        };
        let node = match &self.deps.heads {
            Some(h) => {
                h.set_head(ctx, node.id, version.id, version.size_bytes)
                    .await?
            }
            None => self.patched(node),
        };
        let (event, data) = match restored_from {
            Some(_) => (
                EventType::VersionRestored,
                logic::version_restored_data(&version, previous),
            ),
            None => (
                EventType::VersionCreated,
                logic::version_created_data(&version, previous, coalesced),
            ),
        };
        let e = self.deps.journal.append(
            ctx,
            NewEntry {
                node_id: Some(node.id),
                revision: Some(node.revision),
                event,
                data,
            },
        )?;
        self.apply_policy(node.id, &policy, now);
        Ok(Written {
            value: (node, version),
            change: ChangeInfo {
                seq: Some(e.seq),
                actor: e.actor,
                at: e.at,
            },
        })
    }

    /// Thinning and retention of one node (section 5.3). Returns the removed versions.
    pub fn apply_policy(
        &self,
        node: NodeId,
        policy: &VersionPolicy,
        now: OffsetDateTime,
    ) -> Vec<VersionId> {
        let mut st = self.lock();
        let Some(ids) = st.by_node.get(&node).cloned() else {
            return vec![];
        };
        let head = ids.last().copied();
        let metas: Vec<VersionMeta> = ids
            .iter()
            .filter_map(|id| st.versions.get(id))
            .map(|s| VersionMeta {
                id: s.version.id,
                created_at: s.version.created_at,
                labeled: s.version.label.is_some(),
                is_head: Some(s.version.id) == head,
            })
            .collect();
        let gone = policy::prunable(&metas, policy, now);
        if gone.is_empty() {
            return gone;
        }
        let set: HashSet<VersionId> = gone.iter().copied().collect();
        for id in &gone {
            st.versions.remove(id);
        }
        for s in st.versions.values_mut() {
            if s.version.restored_from.is_some_and(|r| set.contains(&r)) {
                s.version.restored_from = None;
            }
        }
        if let Some(v) = st.by_node.get_mut(&node) {
            v.retain(|id| !set.contains(id));
        }
        gone
    }

    /// Gives the file `to`, a new copy of the file `from`, one version with the content of
    /// `from`'s head (the chunks are shared) and writes `version.created`. For the copy of the
    /// in-memory node store. `None` when `from` has no content.
    pub async fn copy_head(
        &self,
        ctx: &Context,
        from: NodeId,
        to: Node,
    ) -> Result<Option<Version>> {
        let Ok(s) = self.stored(ctx.tenant, from, None) else {
            return Ok(None);
        };
        let w = self
            .commit(ctx, to, s.manifest, s.version.content_type, None, true)
            .await?;
        Ok(Some(w.value.1))
    }

    /// The versions of a node, oldest first, without a permission check. For tests and
    /// inspection only.
    pub fn versions_of(&self, node: NodeId) -> Vec<Version> {
        let st = self.lock();
        st.by_node
            .get(&node)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| st.versions.get(id))
                    .map(|s| s.version.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Forgets the versions of a purged node (the node store calls it when it purges a file).
    pub fn forget_node(&self, node: NodeId) {
        let mut st = self.lock();
        if let Some(ids) = st.by_node.remove(&node) {
            for id in ids {
                st.versions.remove(&id);
            }
        }
    }

    /// Garbage collection of the tenant's chunks (mark and sweep, section 5.2): a chunk goes
    /// when no version and no open upload uses it, and it was not stored or named by an upload
    /// within `grace`.
    pub async fn collect_garbage(&self, tenant: TenantId, grace: Duration) -> Result<GcReport> {
        let now = crate::now();
        let cutoff = now - grace;
        let (sweep, live) = {
            let st = self.lock();
            let mut live: HashSet<Hash> = HashSet::new();
            for s in st
                .versions
                .values()
                .filter(|s| s.version.tenant_id == tenant)
            {
                live.extend(s.manifest.chunks.iter().map(|c| c.hash));
            }
            for u in st.uploads.values().filter(|u| {
                u.tenant_id == tenant && u.status == UploadStatus::Open && u.expires_at > now
            }) {
                live.extend(u.chunks.iter().map(|c| c.hash));
            }
            let sweep: Vec<Hash> = st
                .chunks
                .iter()
                .filter(|((t, h), m)| {
                    *t == tenant
                        && !live.contains(h)
                        && m.created_at < cutoff
                        && m.last_marked_at < cutoff
                })
                .map(|((_, h), _)| *h)
                .collect();
            let n_live = st
                .chunks
                .keys()
                .filter(|(t, h)| *t == tenant && live.contains(h))
                .count();
            (sweep, n_live)
        };
        self.sweep(tenant, sweep, live).await
    }

    async fn sweep(&self, tenant: TenantId, hashes: Vec<Hash>, live: usize) -> Result<GcReport> {
        let mut deleted = 0;
        for h in hashes {
            self.deps.blobs.delete_chunk(tenant, h).await?;
            self.lock().chunks.remove(&(tenant, h));
            deleted += 1;
        }
        Ok(GcReport { live, deleted })
    }

    /// The upload `id` of the caller (only the actor that started it sees it). An open upload
    /// past its expiry becomes `expired`.
    fn own_upload(&self, ctx: &Context, id: UploadId) -> Result<Upload> {
        let mut st = self.lock();
        let u = st
            .uploads
            .get_mut(&id)
            .filter(|u| u.tenant_id == ctx.tenant && u.created_by == ctx.actor)
            .ok_or_else(logic::upload_not_found)?;
        if u.status == UploadStatus::Open && u.expires_at <= crate::now() {
            u.status = UploadStatus::Expired;
        }
        Ok(u.clone())
    }

    async fn with_missing(&self, mut u: Upload) -> Result<Upload> {
        let hashes = logic::distinct(&u.chunks);
        let have = self.deps.blobs.has_chunks(u.tenant_id, &hashes).await?;
        u.missing = hashes
            .into_iter()
            .zip(have)
            .filter(|(_, h)| !h)
            .map(|(h, _)| h)
            .collect();
        Ok(u)
    }

    fn set_status(&self, id: UploadId, status: UploadStatus) {
        if let Some(u) = self.lock().uploads.get_mut(&id) {
            u.status = status;
        }
    }

    fn record_chunks(&self, tenant: TenantId, chunks: &[ChunkRef]) {
        let now = crate::now();
        let mut st = self.lock();
        for c in chunks {
            st.chunks
                .entry((tenant, c.hash))
                .and_modify(|m| m.last_marked_at = now)
                .or_insert(ChunkMeta {
                    created_at: now,
                    last_marked_at: now,
                });
        }
    }
}

/// The content copy of `copyNode` for the in-memory node store
/// ([`crate::nodes::memory::MemoryNodeStore::set_content_copy`]).
#[async_trait]
impl crate::nodes::ContentCopy for MemoryContentStore {
    async fn copy_head(
        &self,
        ctx: &Context,
        source: &Node,
        target: NodeId,
    ) -> Result<Option<VersionId>> {
        // The new node is not visible to the caller yet (its tuples follow), so the target is
        // the source with the new ID; the commit only reads the ID and sets the head through
        // `HeadUpdate`.
        let mut to = source.clone();
        to.id = target;
        to.head_version = None;
        Ok(MemoryContentStore::copy_head(self, ctx, source.id, to)
            .await?
            .map(|v| v.id))
    }

    fn purged(&self, nodes: &[NodeId]) {
        for n in nodes {
            self.forget_node(*n);
        }
    }
}

#[async_trait]
impl Transfers for MemoryContentStore {
    async fn create_upload(&self, ctx: &Context, req: NewUpload) -> Result<Upload> {
        check_link_may_write(ctx)?;
        let params = logic::check_new_upload(&req)?;
        let old_size = match &req.target {
            UploadTarget::NewVersion { node_id, .. } => {
                self.file(ctx, *node_id, Role::Edit).await?;
                self.lock()
                    .head(*node_id)
                    .map_or(0, |h| h.version.size_bytes)
            }
            UploadTarget::NewFile {
                parent_id, name, ..
            } => {
                let p = self.node(ctx, *parent_id, Role::Edit).await?;
                if p.kind != NodeKind::Folder || p.trashed_at.is_some() {
                    return Err(logic::not_a_folder_for(p.kind));
                }
                logic::check_file_name(name)?;
                0
            }
        };
        let limits = self.limits(ctx.tenant).await;
        let used = self.lock().used_bytes(ctx.tenant);
        logic::check_limits(&limits, used, old_size, req.size)?;
        let now = crate::now();
        let upload = Upload {
            id: UploadId::new(),
            tenant_id: ctx.tenant,
            target: req.target,
            content_type: req.content_type,
            size: req.size,
            content_hash: req.content_hash,
            missing: Vec::new(),
            chunks: req.chunks,
            status: UploadStatus::Open,
            chunking: params,
            expires_at: now + Duration::hours(UPLOAD_HOURS),
            created_by: ctx.actor,
        };
        {
            let mut st = self.lock();
            st.touch(ctx.tenant, &logic::distinct(&upload.chunks), now);
            st.uploads.insert(upload.id, upload.clone());
        }
        self.with_missing(upload).await
    }

    async fn get_upload(&self, ctx: &Context, id: UploadId) -> Result<Upload> {
        check_link_may_write(ctx)?;
        let u = self.own_upload(ctx, id)?;
        self.with_missing(u).await
    }

    async fn put_chunk(&self, ctx: &Context, id: UploadId, hash: Hash, data: Bytes) -> Result<()> {
        check_link_may_write(ctx)?;
        let u = self.own_upload(ctx, id)?;
        if u.status != UploadStatus::Open {
            return Err(logic::upload_not_open(u.status));
        }
        logic::check_chunk(&u.chunks, hash, &data)?;
        let size = data.len() as u32;
        self.deps.blobs.put_chunk(ctx.tenant, hash, data).await?;
        self.record_chunks(ctx.tenant, &[ChunkRef { hash, size }]);
        Ok(())
    }

    async fn complete_upload(&self, ctx: &Context, id: UploadId) -> Result<ContentWritten> {
        check_link_may_write(ctx)?;
        let u = self.own_upload(ctx, id)?;
        if u.status != UploadStatus::Open {
            return Err(logic::upload_not_open(u.status));
        }
        let manifest = Manifest {
            format: ManifestFormat::ChunksV1,
            chunking: u.chunking.id.to_string(),
            chunks: u.chunks.clone(),
            size: u.size,
            content_hash: u.content_hash,
        };
        if let Err((e, gone)) =
            logic::verify_content(&*self.deps.blobs, ctx.tenant, &manifest).await
        {
            let mut st = self.lock();
            for h in gone {
                st.chunks.remove(&(ctx.tenant, h));
            }
            return Err(e);
        }
        let written = match &u.target {
            UploadTarget::NewVersion {
                node_id,
                base_version,
                coalesce,
            } => {
                let node = self.file(ctx, *node_id, Role::Edit).await?;
                let head = self.lock().head(*node_id).map(|h| h.version.id);
                if base_version.is_some() && *base_version != head {
                    return Err(logic::version_mismatch());
                }
                self.commit(ctx, node, manifest, u.content_type.clone(), None, *coalesce)
                    .await?
            }
            UploadTarget::NewFile {
                parent_id,
                name,
                on_conflict,
            } => {
                let node = self
                    .deps
                    .nodes
                    .create(
                        ctx,
                        NewNode {
                            parent_id: *parent_id,
                            name: name.clone(),
                            kind: NodeKind::File,
                            node_type: u.content_type.clone(),
                            link: None,
                            on_conflict: *on_conflict,
                        },
                    )
                    .await?
                    .value;
                self.commit(ctx, node, manifest, u.content_type.clone(), None, true)
                    .await?
            }
        };
        self.set_status(id, UploadStatus::Completed);
        Ok(written)
    }

    async fn abort_upload(&self, ctx: &Context, id: UploadId) -> Result<()> {
        check_link_may_write(ctx)?;
        let u = self.own_upload(ctx, id)?;
        if u.status == UploadStatus::Open {
            self.set_status(id, UploadStatus::Aborted);
        }
        Ok(())
    }

    async fn open_content(
        &self,
        ctx: &Context,
        node: NodeId,
        version: Option<VersionId>,
        range: Option<ByteRange>,
    ) -> Result<Content> {
        let n = self.file(ctx, node, Role::View).await?;
        let s = self.stored(ctx.tenant, node, version)?;
        let range = logic::resolve_range(range, s.manifest.size)?;
        Ok(Content {
            node_id: node,
            name: n.name,
            version_id: s.version.id,
            total_size: s.manifest.size,
            range,
            content_type: s
                .version
                .content_type
                .clone()
                .unwrap_or_else(|| "application/octet-stream".into()),
            content_hash: s.manifest.content_hash,
            body: logic::stream_chunks(
                self.deps.blobs.clone(),
                ctx.tenant,
                s.manifest.chunks,
                range,
            ),
        })
    }

    async fn put_content_with(
        &self,
        ctx: &Context,
        node: NodeId,
        data: Bytes,
        content_type: Option<String>,
        if_version: Option<VersionId>,
        coalesce: bool,
    ) -> Result<ContentWritten> {
        check_link_may_write(ctx)?;
        let n = self.file(ctx, node, Role::Edit).await?;
        if data.len() > MAX_PUT_CONTENT_BYTES {
            return Err(logic::payload_too_large(MAX_PUT_CONTENT_BYTES as i64));
        }
        let (head, old_size, old_type) = {
            let st = self.lock();
            let h = st.head(node);
            (
                h.map(|h| h.version.id),
                h.map_or(0, |h| h.version.size_bytes),
                h.and_then(|h| h.version.content_type.clone()),
            )
        };
        if if_version.is_some() && if_version != head {
            return Err(logic::precondition_failed());
        }
        let limits = self.limits(ctx.tenant).await;
        let used = self.lock().used_bytes(ctx.tenant);
        logic::check_limits(&limits, used, old_size, data.len() as u64)?;
        let content_type = content_type.or(old_type).or(n.node_type.clone());
        let (manifest, stored) = logic::store_data(
            &*self.deps.blobs,
            ctx.tenant,
            &data,
            content_type.as_deref(),
        )
        .await?;
        self.record_chunks(ctx.tenant, &stored);
        self.commit(ctx, n, manifest, content_type, None, coalesce)
            .await
    }

    async fn content_grant(
        &self,
        ctx: &Context,
        node: NodeId,
        version: Option<VersionId>,
        disposition: Disposition,
    ) -> Result<(String, ContentClaims)> {
        let n = self.file(ctx, node, Role::View).await?;
        let s = self.stored(ctx.tenant, node, version)?;
        let claims = ContentSigner::claims(
            ctx.tenant,
            node,
            s.version.id,
            disposition,
            &n.name,
            crate::now(),
        );
        Ok((self.signer.sign(&claims), claims))
    }

    async fn open_granted(
        &self,
        token: &str,
        range: Option<ByteRange>,
    ) -> Result<(Content, ContentClaims)> {
        let claims = self.signer.verify(token, crate::now())?;
        let s = self.stored(claims.tenant, claims.node, Some(claims.version))?;
        let range = logic::resolve_range(range, s.manifest.size)?;
        let content = Content {
            node_id: claims.node,
            name: claims.name.clone(),
            version_id: s.version.id,
            total_size: s.manifest.size,
            range,
            content_type: s
                .version
                .content_type
                .clone()
                .unwrap_or_else(|| "application/octet-stream".into()),
            content_hash: s.manifest.content_hash,
            body: logic::stream_chunks(
                self.deps.blobs.clone(),
                claims.tenant,
                s.manifest.chunks,
                range,
            ),
        };
        Ok((content, claims))
    }
}

#[async_trait]
impl VersionStore for MemoryContentStore {
    async fn list_filtered(
        &self,
        ctx: &Context,
        node: NodeId,
        filter: VersionFilter,
        page: PageRequest,
    ) -> Result<Page<Version>> {
        self.file(ctx, node, Role::View).await?;
        let after = page
            .cursor
            .as_ref()
            .map(logic::decode_version_cursor)
            .transpose()?;
        let limit = page.limit() as usize;
        let st = self.lock();
        let mut all: Vec<&Version> = st
            .by_node
            .get(&node)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| st.versions.get(id))
                    .map(|s| &s.version)
                    .filter(|v| v.tenant_id == ctx.tenant && filter.matches(v))
                    .collect()
            })
            .unwrap_or_default();
        all.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
        let mut items: Vec<Version> = all
            .into_iter()
            .filter(|v| after.is_none_or(|(at, id)| (v.created_at, v.id) < (at, id)))
            .take(limit + 1)
            .cloned()
            .collect();
        let next_cursor = if items.len() > limit {
            items.truncate(limit);
            items.last().map(logic::version_cursor)
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }

    async fn get(&self, ctx: &Context, node: NodeId, version: VersionId) -> Result<Version> {
        self.file(ctx, node, Role::View).await?;
        Ok(self.stored(ctx.tenant, node, Some(version))?.version)
    }

    async fn restore(
        &self,
        ctx: &Context,
        node: NodeId,
        version: VersionId,
    ) -> Result<Written<Version>> {
        check_link_may_write(ctx)?;
        let n = self.file(ctx, node, Role::Edit).await?;
        let s = self.stored(ctx.tenant, node, Some(version))?;
        let w = self
            .commit(
                ctx,
                n,
                s.manifest,
                s.version.content_type,
                Some(version),
                true,
            )
            .await?;
        Ok(Written {
            value: w.value.1,
            change: w.change,
        })
    }

    async fn set_label(
        &self,
        ctx: &Context,
        node: NodeId,
        version: VersionId,
        label: Option<String>,
    ) -> Result<Written<Version>> {
        check_link_may_write(ctx)?;
        let n = self.file(ctx, node, Role::Edit).await?;
        let label = logic::check_label(label)?;
        let (v, old) = {
            let mut st = self.lock();
            let s = st
                .versions
                .get_mut(&version)
                .filter(|s| s.version.node_id == node && s.version.tenant_id == ctx.tenant)
                .ok_or_else(version_not_found)?;
            let old = std::mem::replace(&mut s.version.label, label.clone());
            (s.version.clone(), old)
        };
        if old == label {
            return Ok(Written {
                value: v,
                change: ChangeInfo {
                    seq: None,
                    actor: ctx.actor,
                    at: crate::now(),
                },
            });
        }
        let e = self.deps.journal.append(
            ctx,
            NewEntry {
                node_id: Some(node),
                revision: Some(n.revision),
                event: EventType::VersionLabeled,
                data: logic::version_labeled_data(version, old.as_deref(), label.as_deref()),
            },
        )?;
        Ok(Written {
            value: v,
            change: ChangeInfo {
                seq: Some(e.seq),
                actor: e.actor,
                at: e.at,
            },
        })
    }
}
