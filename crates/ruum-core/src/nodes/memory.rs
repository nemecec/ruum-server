//! In-memory node tree, for unit tests and for running without Docker.
//!
//! It follows the same rules as the Postgres store ([`super::rules`]) and passes the same
//! conformance suite. Relationships go straight to the engine (no outbox in memory); journal
//! entries go to the shared [`MemoryJournal`].
//!
//! The tenant store is set after construction ([`MemoryNodeStore::set_tenants`]): it gives the
//! plan (trash windows) and the tenant role of the caller (admins). Without it the store uses the
//! `personal` plan and treats nobody as a tenant admin.
//!
//! File content: the blobs and versions area sets the head version of a file with
//! [`MemoryNodeStore::set_head`], and copies content for `copyNode` through
//! [`MemoryNodeStore::set_content_copy`] ([`ContentCopy`]).
//!
//! Typed nodes: the store writes their `typed_nodes` rows (and manifest versions) into the
//! [`MemoryTypedNodeStore`] set with [`MemoryNodeStore::set_typed`].

use super::rules::{self, ChildKey, TrashWindows};
use super::*;
use crate::authz::{self, Authz, ObjectRef, Permission, RelationshipUpdate, tuples};
use crate::error::Error;
use crate::journal::memory::MemoryJournal;
use crate::journal::{EventType, NewEntry};
use crate::providers::store::{MemoryTypedNodeStore, NewTypedNode};
use crate::tenants::{Plan, RootCreated, Tenant, TenantStore};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

#[derive(Clone, Debug)]
struct Row {
    /// The node without the caller fields.
    node: Node,
    trash_root: Option<NodeId>,
    purge_after: Option<OffsetDateTime>,
}

#[derive(Debug, Default)]
struct State {
    rows: HashMap<NodeId, Row>,
}

/// A change that a check must see before it is made ([`State::view_reaches`]).
#[derive(Default)]
struct Pending {
    /// New parents.
    parents: HashMap<NodeId, NodeId>,
    /// Trashed nodes that come back.
    revived: HashSet<NodeId>,
}

impl State {
    fn row(&self, tenant: TenantId, id: NodeId) -> Result<&Row> {
        self.rows
            .get(&id)
            .filter(|r| r.node.tenant_id == tenant)
            .ok_or_else(rules::node_not_found)
    }

    fn row_mut(&mut self, tenant: TenantId, id: NodeId) -> Result<&mut Row> {
        self.rows
            .get_mut(&id)
            .filter(|r| r.node.tenant_id == tenant)
            .ok_or_else(rules::node_not_found)
    }

    fn live(&self, tenant: TenantId, id: NodeId) -> Result<&Row> {
        self.row(tenant, id)
            .ok()
            .filter(|r| r.node.trashed_at.is_none())
            .ok_or_else(rules::node_not_found)
    }

    fn children(&self, tenant: TenantId, parent: NodeId) -> impl Iterator<Item = &Row> {
        self.rows
            .values()
            .filter(move |r| r.node.tenant_id == tenant && r.node.parent_id == Some(parent))
    }

    fn live_children(&self, tenant: TenantId, parent: NodeId) -> impl Iterator<Item = &Row> {
        self.children(tenant, parent)
            .filter(|r| r.node.trashed_at.is_none())
    }

    fn taken(&self, tenant: TenantId, parent: NodeId, except: Option<NodeId>) -> HashSet<String> {
        self.live_children(tenant, parent)
            .filter(|r| Some(r.node.id) != except)
            .map(|r| name_key(&r.node.name))
            .collect()
    }

    /// The node and its descendants, parents before children. With `live_only`, trashed
    /// descendants (and what is below them) are left out.
    fn subtree(&self, tenant: TenantId, root: NodeId, live_only: bool) -> Vec<NodeId> {
        let mut out = vec![root];
        let mut queue = VecDeque::from([root]);
        while let Some(p) = queue.pop_front() {
            let mut kids: Vec<&Row> = self
                .children(tenant, p)
                .filter(|r| !live_only || r.node.trashed_at.is_none())
                .collect();
            kids.sort_by_key(|r| r.node.id);
            for k in kids {
                out.push(k.node.id);
                queue.push_back(k.node.id);
            }
        }
        out
    }

    /// Whether a live member link in `folder` points at `target`.
    fn has_member_link(&self, tenant: TenantId, folder: NodeId, target: NodeId) -> bool {
        self.live_children(tenant, folder)
            .any(|r| rules::member_target(&r.node) == Some(target))
    }

    /// Whether `goal` can be reached from `from` along the edges that the graph follows for
    /// `view` (`parent->view`, `collection->view`): from a node to its parent, and from the
    /// target of a live member link to the folder that holds the link. `pending` describes a
    /// change that is not made yet.
    fn view_reaches(
        &self,
        tenant: TenantId,
        from: NodeId,
        goal: NodeId,
        pending: &Pending,
    ) -> bool {
        let parent_of = |r: &Row| {
            pending
                .parents
                .get(&r.node.id)
                .copied()
                .or(r.node.parent_id)
        };
        let mut folders: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        for r in self.rows.values().filter(|r| r.node.tenant_id == tenant) {
            let live = r.node.trashed_at.is_none() || pending.revived.contains(&r.node.id);
            if let (true, Some(t), Some(p)) = (live, rules::member_target(&r.node), parent_of(r)) {
                folders.entry(t).or_default().push(p);
            }
        }
        let mut seen = HashSet::new();
        let mut stack = vec![from];
        while let Some(x) = stack.pop() {
            if x == goal {
                return true;
            }
            if !seen.insert(x) {
                continue;
            }
            if let Some(r) = self.rows.get(&x) {
                stack.extend(parent_of(r));
            }
            if let Some(f) = folders.get(&x) {
                stack.extend(f.iter().copied());
            }
        }
        false
    }

    /// Whether the new edges (`(a, b)`: the view of `a` follows `b`) close a loop in the state
    /// after the change `pending` (see [`rules::member_link_loop`]). The state before has no
    /// loop, so a new loop goes through a new edge.
    fn closes_view_loop(
        &self,
        tenant: TenantId,
        edges: &[(NodeId, NodeId)],
        pending: &Pending,
    ) -> bool {
        edges
            .iter()
            .any(|&(a, b)| a == b || self.view_reaches(tenant, b, a, pending))
    }

    fn ancestors(&self, tenant: TenantId, id: NodeId) -> Result<Vec<NodeId>> {
        let mut out = Vec::new();
        let mut cur = self.row(tenant, id)?.node.parent_id;
        while let Some(p) = cur {
            if out.contains(&p) || out.len() > 10_000 {
                return Err(Error::Internal("nodes: a cycle in the tree".into()));
            }
            out.push(p);
            cur = self.rows.get(&p).and_then(|r| r.node.parent_id);
        }
        out.reverse();
        Ok(out)
    }
}

/// The in-memory store.
pub struct MemoryNodeStore {
    authz: Arc<dyn Authz>,
    journal: Arc<MemoryJournal>,
    tenants: OnceLock<Arc<dyn TenantStore>>,
    content: OnceLock<Arc<dyn ContentCopy>>,
    typed: OnceLock<Arc<MemoryTypedNodeStore>>,
    state: Mutex<State>,
    /// Held across a whole write, so the checks and the change of one write see one state.
    writes: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for MemoryNodeStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryNodeStore").finish_non_exhaustive()
    }
}

impl MemoryNodeStore {
    /// The store over the shared in-memory engine and journal (`Services::in_memory` builds
    /// them once).
    pub fn new(authz: Arc<dyn Authz>, journal: Arc<MemoryJournal>) -> Self {
        Self {
            authz,
            journal,
            tenants: OnceLock::new(),
            content: OnceLock::new(),
            typed: OnceLock::new(),
            state: Mutex::new(State::default()),
            writes: tokio::sync::Mutex::new(()),
        }
    }

    /// Sets the tenant store (plans and tenant roles). Only the first call counts.
    pub fn set_tenants(&self, tenants: Arc<dyn TenantStore>) {
        let _ = self.tenants.set(tenants);
    }

    /// Sets the content copy of the versions area, for copies of files with content. Only the
    /// first call counts.
    pub fn set_content_copy(&self, content: Arc<dyn ContentCopy>) {
        let _ = self.content.set(content);
    }

    /// Sets the store of the `typed_nodes` rows, for [`NodeStore::create_typed`]. Only the first
    /// call counts.
    pub fn set_typed(&self, typed: Arc<MemoryTypedNodeStore>) {
        let _ = self.typed.set(typed);
    }

    /// Sets the head version and the size of a file (for the blobs and versions area: a new
    /// version, a restore). Bumps the revision; writes no journal entry (the caller writes
    /// `version.created` with the returned revision). No permission check.
    pub fn set_head(
        &self,
        ctx: &Context,
        id: NodeId,
        head: Option<VersionId>,
        size: i64,
    ) -> Result<Node> {
        let mut st = self.st();
        let row = st.row_mut(ctx.tenant, id)?;
        if row.node.kind != NodeKind::File && row.node.kind != NodeKind::Typed {
            return Err(rules::invalid("only files and typed nodes have content"));
        }
        row.node.head_version = head;
        row.node.size_bytes = size.max(0);
        touch(&mut row.node, ctx);
        Ok(row.node.clone())
    }

    /// The node in any tenant, also a trashed one, without the caller fields and without a
    /// permission check. For tests and inspection only.
    pub fn peek(&self, id: NodeId) -> Option<Node> {
        self.st().rows.get(&id).map(|r| r.node.clone())
    }

    /// The live child of `parent` with this name (case-insensitive), without a permission
    /// check. For tests and inspection only.
    pub fn peek_child(&self, parent: NodeId, name: &str) -> Option<Node> {
        let name = name.to_lowercase();
        self.st()
            .rows
            .values()
            .find(|r| {
                r.node.parent_id == Some(parent)
                    && r.node.trashed_at.is_none()
                    && r.node.name.to_lowercase() == name
            })
            .map(|r| r.node.clone())
    }

    /// The live nodes of a tenant, without the caller fields (for search and tags).
    pub(crate) fn live_nodes(&self, tenant: TenantId) -> Vec<Node> {
        self.st()
            .rows
            .values()
            .filter(|r| r.node.tenant_id == tenant && r.node.trashed_at.is_none())
            .map(|r| r.node.clone())
            .collect()
    }

    /// The live node, without the caller fields and without a permission check.
    pub(crate) fn live_node(&self, tenant: TenantId, id: NodeId) -> Option<Node> {
        self.st().live(tenant, id).ok().map(|r| r.node.clone())
    }

    fn st(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("nodes lock")
    }

    fn row(&self, tenant: TenantId, id: NodeId) -> Result<Row> {
        self.st().row(tenant, id).cloned()
    }

    fn live(&self, tenant: TenantId, id: NodeId) -> Result<Row> {
        self.st().live(tenant, id).cloned()
    }

    async fn require(
        &self,
        ctx: &Context,
        id: NodeId,
        p: impl Into<Permission> + Send,
    ) -> Result<()> {
        authz::require(&*self.authz, self, ctx, id, p).await
    }

    async fn out(&self, ctx: &Context, node: Node) -> Result<Node> {
        let mut v = self.present(ctx, vec![node], None).await?;
        v.pop()
            .ok_or_else(|| Error::Internal("nodes: present".into()))
    }

    /// Fills the caller fields and `editors_can_share`, and keeps the nodes the caller sees with
    /// at least `min` (all of them when `min` is `None`). The in-memory sharing store keeps the
    /// node setting `editors_can_share` in the engine only, so it is read from there.
    pub(crate) async fn present(
        &self,
        ctx: &Context,
        mut items: Vec<Node>,
        min: Option<Role>,
    ) -> Result<Vec<Node>> {
        rules::decorate(&*self.authz, self, ctx, &mut items).await?;
        if let Some(min) = min {
            items.retain(|n| n.my_role.is_some_and(|r| r >= min));
        }
        for n in &mut items {
            let rels = self.authz.read(&ObjectRef::node(n.id)).await?;
            let has = |rel: &str| rels.iter().any(|r| r.relation == rel);
            n.editors_can_share = if has("editors_share_off") {
                Some(false)
            } else if has("editors_share_on") {
                Some(true)
            } else {
                None
            };
        }
        Ok(items)
    }

    async fn windows(&self, tenant: TenantId) -> Result<TrashWindows> {
        let plan = match self.tenants.get() {
            Some(t) => t.find(tenant).await?.plan,
            None => Plan::Personal,
        };
        Ok(TrashWindows::of_plan(plan))
    }

    async fn is_admin(&self, ctx: &Context) -> Result<bool> {
        let (Some(t), Some(user)) = (self.tenants.get(), ctx.actor.effective_user()) else {
            return Ok(false);
        };
        Ok(rules::is_admin(t.role_of(ctx.tenant, user).await?))
    }

    fn append(
        &self,
        ctx: &Context,
        node: NodeId,
        revision: Option<i64>,
        event: EventType,
        data: serde_json::Value,
    ) -> Result<ChangeInfo> {
        let e = self.journal.append(
            ctx,
            NewEntry {
                node_id: Some(node),
                revision,
                event,
                data,
            },
        )?;
        Ok(ChangeInfo {
            seq: Some(e.seq),
            actor: ctx.actor,
            at: e.at,
        })
    }

    async fn relate(&self, updates: Vec<RelationshipUpdate>) -> Result<()> {
        if !updates.is_empty() {
            self.authz.write(updates).await?;
        }
        Ok(())
    }

    /// The trashed root, with its permission checked (`manage`), for restore and purge.
    async fn trashed_root(&self, ctx: &Context, id: NodeId) -> Result<Row> {
        let row = self.row(ctx.tenant, id)?;
        self.require(ctx, id, Role::Manage).await?;
        if row.node.trashed_at.is_none() {
            return Err(rules::conflict(
                "the node is not in the trash",
                "trash it first",
            ));
        }
        if row.trash_root != Some(id) {
            return Err(rules::conflict(
                "the node was trashed with a folder above it",
                "restore or purge the folder that was trashed (see GET /v1/trash)",
            ));
        }
        Ok(row)
    }

    /// Purges a trashed root and everything below it.
    async fn purge_root(&self, ctx: &Context, id: NodeId, expired: bool) -> Result<ChangeInfo> {
        let (ids, name, links) = {
            let st = self.st();
            let row = st.row(ctx.tenant, id)?;
            let ids = st.subtree(ctx.tenant, id, false);
            let links: Vec<(NodeId, NodeId)> = ids
                .iter()
                .filter_map(|n| st.rows.get(n))
                .filter_map(|r| Some((rules::member_target(&r.node)?, r.node.parent_id?)))
                .collect();
            (ids, row.node.name.clone(), links)
        };
        let mut updates = Vec::new();
        for n in &ids {
            for r in self.authz.read(&ObjectRef::node(*n)).await? {
                updates.push(RelationshipUpdate::Delete(r));
            }
        }
        let change = {
            let mut st = self.st();
            for n in &ids {
                st.rows.remove(n);
            }
            for (target, folder) in links {
                if !st.has_member_link(ctx.tenant, folder, target) {
                    updates.push(RelationshipUpdate::Delete(tuples::member_link(
                        target, folder,
                    )));
                }
            }
            self.append(
                ctx,
                id,
                None,
                EventType::NodePurged,
                rules::purged_data(&name, ids.len(), expired),
            )?
        };
        self.relate(updates).await?;
        // The versions of purged files go too, so garbage collection can free their chunks.
        if let Some(content) = self.content.get() {
            content.purged(&ids);
        }
        Ok(change)
    }
}

/// Bumps the revision and the `updated_*` fields of a node.
fn touch(node: &mut Node, ctx: &Context) {
    node.revision += 1;
    node.updated_at = crate::now();
    node.updated_by = ctx.actor;
}

#[async_trait]
impl crate::versions::HeadUpdate for MemoryNodeStore {
    async fn set_head(
        &self,
        ctx: &Context,
        node: NodeId,
        version: VersionId,
        size_bytes: i64,
    ) -> Result<Node> {
        MemoryNodeStore::set_head(self, ctx, node, Some(version), size_bytes)
    }
}

impl RootCreated for MemoryNodeStore {
    fn root_created(&self, tenant: &Tenant, owner: UserId) {
        let actor = Actor::user(owner);
        let node = Node {
            id: tenant.root_node_id,
            tenant_id: tenant.id,
            parent_id: None,
            name: ROOT_NAME.to_string(),
            kind: NodeKind::Folder,
            node_type: None,
            link_kind: None,
            link_target: None,
            head_version: None,
            owner_id: owner,
            size_bytes: 0,
            revision: 1,
            trashed_at: None,
            created_at: tenant.created_at,
            updated_at: tenant.created_at,
            created_by: actor,
            updated_by: actor,
            my_role: None,
            my_can_share: None,
            editors_can_share: None,
            editors_can_share_effective: None,
        };
        self.st().rows.insert(
            node.id,
            Row {
                node,
                trash_root: None,
                purge_after: None,
            },
        );
    }
}

#[async_trait]
impl NodeStore for MemoryNodeStore {
    async fn root(&self, ctx: &Context) -> Result<Node> {
        let root = self
            .st()
            .rows
            .values()
            .find(|r| r.node.tenant_id == ctx.tenant && r.node.parent_id.is_none())
            .map(|r| r.node.clone())
            .ok_or_else(rules::node_not_found)?;
        // The root is not secret: a caller who cannot see it gets 403.
        match self.require(ctx, root.id, Role::View).await {
            Err(e) if rules::is_not_found(&e) => return Err(Error::permission_denied()),
            other => other?,
        }
        self.out(ctx, root).await
    }

    async fn create(&self, ctx: &Context, req: NewNode) -> Result<Written<Node>> {
        let name = validate_name(&req.name)?;
        let link = check_new_kind(&req)?;
        let owner = rules::owner_of(ctx)?;
        self.require(ctx, req.parent_id, Role::Edit).await?;
        let parent = self.live(ctx.tenant, req.parent_id)?;
        if parent.node.kind != NodeKind::Folder {
            return Err(rules::invalid("the parent is not a folder"));
        }
        if let Some(l) = &link {
            self.live(ctx.tenant, l.target)?;
            let p = match l.kind {
                LinkKind::Shortcut => Permission::View,
                LinkKind::Member => Permission::Share,
            };
            self.require(ctx, l.target, p).await?;
        }
        let _w = self.writes.lock().await;
        let (node, change) = {
            let mut st = self.st();
            st.live(ctx.tenant, req.parent_id)?;
            if let Some(l) = link.as_ref().filter(|l| l.kind == LinkKind::Member)
                && st.closes_view_loop(
                    ctx.tenant,
                    &[(l.target, req.parent_id)],
                    &Pending::default(),
                )
            {
                return Err(rules::member_link_loop());
            }
            let taken = st.taken(ctx.tenant, req.parent_id, None);
            let name = rules::pick_name(&name, req.kind, req.on_conflict, &taken)?;
            let now = crate::now();
            let node = Node {
                id: NodeId::new(),
                tenant_id: ctx.tenant,
                parent_id: Some(req.parent_id),
                name,
                kind: req.kind,
                node_type: req.node_type.clone(),
                link_kind: link.as_ref().map(|l| l.kind),
                link_target: link.as_ref().map(|l| l.target),
                head_version: None,
                owner_id: owner,
                size_bytes: 0,
                revision: 1,
                trashed_at: None,
                created_at: now,
                updated_at: now,
                created_by: ctx.actor,
                updated_by: ctx.actor,
                my_role: None,
                my_can_share: None,
                editors_can_share: None,
                editors_can_share_effective: None,
            };
            st.rows.insert(
                node.id,
                Row {
                    node: node.clone(),
                    trash_root: None,
                    purge_after: None,
                },
            );
            let change = self.append(
                ctx,
                node.id,
                Some(node.revision),
                EventType::NodeCreated,
                rules::created_data(&node, None),
            )?;
            (node, change)
        };
        self.relate(rules::created_tuples(&node)).await?;
        Ok(Written {
            value: self.out(ctx, node).await?,
            change,
        })
    }

    async fn create_typed(&self, ctx: &Context, req: NewTyped) -> Result<Written<Node>> {
        let name = validate_name(&req.name)?;
        let owner = rules::owner_of(ctx)?;
        let typed = self
            .typed
            .get()
            .cloned()
            .ok_or_else(|| Error::Internal("nodes: no typed node store".into()))?;
        self.require(ctx, req.parent_id, Role::Edit).await?;
        let parent = self.live(ctx.tenant, req.parent_id)?;
        if parent.node.kind != NodeKind::Folder {
            return Err(rules::invalid("the parent is not a folder"));
        }
        let (bytes, hash) = typed_manifest_bytes(&req.manifest);
        let _w = self.writes.lock().await;
        let (node, change) = {
            let mut st = self.st();
            st.live(ctx.tenant, req.parent_id)?;
            let taken = st.taken(ctx.tenant, req.parent_id, None);
            let name = rules::pick_name(&name, NodeKind::Typed, req.on_conflict, &taken)?;
            let now = crate::now();
            let version = VersionId::new();
            let node = Node {
                id: NodeId::new(),
                tenant_id: ctx.tenant,
                parent_id: Some(req.parent_id),
                name,
                kind: NodeKind::Typed,
                node_type: Some(req.node_type.clone()),
                link_kind: None,
                link_target: None,
                head_version: Some(version),
                owner_id: owner,
                size_bytes: 0,
                revision: 1,
                trashed_at: None,
                created_at: now,
                updated_at: now,
                created_by: ctx.actor,
                updated_by: ctx.actor,
                my_role: None,
                my_can_share: None,
                editors_can_share: None,
                editors_can_share_effective: None,
            };
            // The `typed_nodes` row and the first manifest version, then the node: one unit.
            typed.insert(
                NewTypedNode {
                    tenant_id: ctx.tenant,
                    node_id: node.id,
                    provider: req.provider.clone(),
                    type_name: req.node_type.clone(),
                    desired_version: version,
                },
                req.manifest.clone(),
                ctx.actor.into(),
            )?;
            st.rows.insert(
                node.id,
                Row {
                    node: node.clone(),
                    trash_root: None,
                    purge_after: None,
                },
            );
            self.append(
                ctx,
                node.id,
                Some(node.revision),
                EventType::NodeCreated,
                rules::created_data(&node, None),
            )?;
            let change = self.append(
                ctx,
                node.id,
                Some(node.revision),
                EventType::VersionCreated,
                typed_version_created_data(version, None, bytes.len(), &hash),
            )?;
            (node, change)
        };
        self.relate(rules::created_tuples(&node)).await?;
        Ok(Written {
            value: self.out(ctx, node).await?,
            change,
        })
    }

    async fn get(&self, ctx: &Context, id: NodeId) -> Result<Node> {
        let row = self.row(ctx.tenant, id)?;
        if row.node.trashed_at.is_some() {
            // The trash is for those who may restore.
            self.require(ctx, id, Role::Manage)
                .await
                .map_err(|e| match e {
                    Error::Api(_) => rules::node_not_found(),
                    other => other,
                })?;
        } else {
            self.require(ctx, id, Role::View).await?;
        }
        self.out(ctx, row.node).await
    }

    async fn get_by_path(&self, ctx: &Context, path: &str) -> Result<Node> {
        let names = split_path(path).map_err(|_| rules::node_not_found())?;
        let node = {
            let st = self.st();
            let mut cur = st
                .rows
                .values()
                .find(|r| r.node.tenant_id == ctx.tenant && r.node.parent_id.is_none())
                .ok_or_else(rules::node_not_found)?;
            for n in &names {
                let key = name_key(n);
                cur = st
                    .live_children(ctx.tenant, cur.node.id)
                    .find(|r| name_key(&r.node.name) == key)
                    .ok_or_else(rules::node_not_found)?;
            }
            cur.node.clone()
        };
        self.require(ctx, node.id, Role::View).await?;
        self.out(ctx, node).await
    }

    async fn list_children(
        &self,
        ctx: &Context,
        parent: NodeId,
        order: ChildOrder,
        page: PageRequest,
    ) -> Result<Page<Node>> {
        let after = page.cursor.as_ref().map(ChildKey::parse).transpose()?;
        self.require(ctx, parent, Role::View).await?;
        let limit = page.limit() as usize;
        let mut rows: Vec<(ChildKey, Node)> = {
            let st = self.st();
            let p = st.live(ctx.tenant, parent)?;
            if p.node.kind != NodeKind::Folder {
                return Err(rules::invalid("the node is not a folder"));
            }
            st.live_children(ctx.tenant, parent)
                .map(|r| (ChildKey::of(&r.node), r.node.clone()))
                .collect()
        };
        rows.sort_by(|a, b| ChildKey::cmp(order, &a.0, &b.0));
        if let Some(after) = &after {
            rows.retain(|(k, _)| ChildKey::cmp(order, k, after).is_gt());
        }
        let more = rows.len() > limit;
        rows.truncate(limit);
        let next_cursor = if more {
            rows.last().map(|(k, _)| k.cursor())
        } else {
            None
        };
        let items = self
            .present(
                ctx,
                rows.into_iter().map(|(_, n)| n).collect(),
                Some(Role::View),
            )
            .await?;
        Ok(Page { items, next_cursor })
    }

    async fn rename(
        &self,
        ctx: &Context,
        id: NodeId,
        name: String,
        on_conflict: OnConflict,
        if_revision: Option<i64>,
    ) -> Result<Written<Node>> {
        let name = validate_name(&name)?;
        self.live(ctx.tenant, id)?;
        self.require(ctx, id, Role::Edit).await?;
        let _w = self.writes.lock().await;
        let done = {
            let mut st = self.st();
            let row = st.live(ctx.tenant, id)?.clone();
            let Some(parent) = row.node.parent_id else {
                return Err(rules::invalid("the root folder cannot be renamed"));
            };
            rules::check_revision(&row.node, if_revision)?;
            if row.node.name == name {
                Err(row.node)
            } else {
                let taken = st.taken(ctx.tenant, parent, Some(id));
                let name = rules::pick_name(&name, row.node.kind, on_conflict, &taken)?;
                let r = st.row_mut(ctx.tenant, id)?;
                let old = std::mem::replace(&mut r.node.name, name.clone());
                touch(&mut r.node, ctx);
                let node = r.node.clone();
                let change = self.append(
                    ctx,
                    id,
                    Some(node.revision),
                    EventType::NodeRenamed,
                    rules::renamed_data(Some(parent), &old, &name),
                )?;
                Ok((node, change))
            }
        };
        let (node, change) = match done {
            Ok(v) => v,
            Err(node) => (node, no_change(ctx)),
        };
        Ok(Written {
            value: self.out(ctx, node).await?,
            change,
        })
    }

    async fn move_node(
        &self,
        ctx: &Context,
        id: NodeId,
        req: MoveRequest,
    ) -> Result<Written<Node>> {
        let wanted = req.name.as_deref().map(validate_name).transpose()?;
        let row = self.live(ctx.tenant, id)?;
        self.require(ctx, id, Role::Manage).await?;
        let Some(old_parent) = row.node.parent_id else {
            return Err(rules::invalid("the root folder cannot be moved"));
        };
        self.require(ctx, req.parent_id, Role::Edit).await?;
        let dest = self.live(ctx.tenant, req.parent_id)?;
        if dest.node.kind != NodeKind::Folder {
            return Err(rules::invalid("the new parent is not a folder"));
        }
        if req.parent_id == id
            || self
                .st()
                .ancestors(ctx.tenant, req.parent_id)?
                .contains(&id)
        {
            return Err(rules::invalid(
                "a folder cannot move into itself or its own subtree",
            ));
        }
        let target = rules::member_target(&row.node)
            .filter(|t| old_parent != req.parent_id && self.live(ctx.tenant, *t).is_ok());
        if let Some(t) = target {
            self.require(ctx, t, Permission::Share).await?;
        }
        let _w = self.writes.lock().await;
        let mut updates = Vec::new();
        let done = {
            let mut st = self.st();
            let row = st.live(ctx.tenant, id)?.clone();
            st.live(ctx.tenant, req.parent_id)?;
            rules::check_revision(&row.node, req.if_revision)?;
            let wanted = wanted.unwrap_or_else(|| row.node.name.clone());
            if old_parent == req.parent_id && row.node.name == wanted {
                Err(row.node)
            } else {
                if old_parent != req.parent_id {
                    let mut edges = vec![(id, req.parent_id)];
                    edges.extend(rules::member_target(&row.node).map(|t| (t, req.parent_id)));
                    let pending = Pending {
                        parents: HashMap::from([(id, req.parent_id)]),
                        ..Pending::default()
                    };
                    if st.closes_view_loop(ctx.tenant, &edges, &pending) {
                        return Err(rules::member_link_loop());
                    }
                }
                let taken = st.taken(ctx.tenant, req.parent_id, Some(id));
                let name = rules::pick_name(&wanted, row.node.kind, req.on_conflict, &taken)?;
                let r = st.row_mut(ctx.tenant, id)?;
                r.node.parent_id = Some(req.parent_id);
                r.node.name = name.clone();
                touch(&mut r.node, ctx);
                let node = r.node.clone();
                if old_parent != req.parent_id {
                    updates.push(RelationshipUpdate::Delete(tuples::node_parent(
                        id, old_parent,
                    )));
                    updates.push(RelationshipUpdate::Touch(tuples::node_parent(
                        id,
                        req.parent_id,
                    )));
                    if let Some(t) = rules::member_target(&node) {
                        if !st.has_member_link(ctx.tenant, old_parent, t) {
                            updates.push(RelationshipUpdate::Delete(tuples::member_link(
                                t, old_parent,
                            )));
                        }
                        updates.push(RelationshipUpdate::Touch(tuples::member_link(
                            t,
                            req.parent_id,
                        )));
                    }
                }
                let change = self.append(
                    ctx,
                    id,
                    Some(node.revision),
                    EventType::NodeMoved,
                    rules::moved_data(old_parent, req.parent_id, &row.node.name, &name),
                )?;
                Ok((node, change))
            }
        };
        let (node, change) = match done {
            Ok(v) => v,
            Err(node) => (node, no_change(ctx)),
        };
        self.relate(updates).await?;
        Ok(Written {
            value: self.out(ctx, node).await?,
            change,
        })
    }

    async fn copy(&self, ctx: &Context, id: NodeId, req: CopyRequest) -> Result<Written<Node>> {
        let wanted = req.name.as_deref().map(validate_name).transpose()?;
        let owner = rules::owner_of(ctx)?;
        self.live(ctx.tenant, id)?;
        self.require(ctx, id, Role::View).await?;
        self.require(ctx, req.parent_id, Role::Edit).await?;
        let dest = self.live(ctx.tenant, req.parent_id)?;
        if dest.node.kind != NodeKind::Folder {
            return Err(rules::invalid("the new parent is not a folder"));
        }
        let sources: Vec<Node> = {
            let st = self.st();
            if req.parent_id == id || st.ancestors(ctx.tenant, req.parent_id)?.contains(&id) {
                return Err(rules::invalid(
                    "a folder cannot be copied into itself or its own subtree",
                ));
            }
            // Broken links (the target was purged) are not copied.
            st.subtree(ctx.tenant, id, true)
                .iter()
                .filter_map(|n| st.rows.get(n).map(|r| r.node.clone()))
                .filter(|n| n.link_target.is_none_or(|t| st.row(ctx.tenant, t).is_ok()))
                .collect()
        };
        if sources.first().is_none_or(|n| n.id != id) {
            return Err(rules::invalid(
                "the link target is gone; a broken link is not copied",
            ));
        }
        if sources.len() > rules::MAX_COPY_NODES {
            return Err(rules::invalid(format!(
                "a copy makes at most {} nodes",
                rules::MAX_COPY_NODES
            )));
        }
        let content = self.content.get().cloned();
        if content.is_none() && sources.iter().any(|n| n.head_version.is_some()) {
            // A store without the versions area (`set_content_copy`; `Services::in_memory` sets
            // it) cannot copy content.
            return Err(Error::not_implemented("copying file content in memory"));
        }
        // Member links stay member links only where the caller may share the target.
        let targets: Vec<NodeId> = sources.iter().filter_map(rules::member_target).collect();
        let views = rules::caller_view(&*self.authz, self, ctx, &targets).await?;
        let shares: HashSet<NodeId> = targets
            .iter()
            .zip(&views)
            .filter(|(_, v)| v.can_share)
            .map(|(t, _)| *t)
            .collect();

        let _w = self.writes.lock().await;
        let now = crate::now();
        let mut map: HashMap<NodeId, NodeId> = HashMap::new();
        let mut made: Vec<(Node, NodeId)> = Vec::new();
        {
            let mut st = self.st();
            st.live(ctx.tenant, req.parent_id)?;
            let taken = st.taken(ctx.tenant, req.parent_id, None);
            let root_name = rules::pick_name(
                &wanted.unwrap_or_else(|| sources[0].name.clone()),
                sources[0].kind,
                req.on_conflict,
                &taken,
            )?;
            // A copied member link makes its target depend on the copy, which is inside the
            // new parent: a loop when the new parent's view already follows the target. Such
            // links are copied as shortcuts (`rules::member_link_loop`).
            let loops: HashSet<NodeId> = targets
                .iter()
                .copied()
                .filter(|t| {
                    st.closes_view_loop(ctx.tenant, &[(*t, req.parent_id)], &Pending::default())
                })
                .collect();
            for (i, src) in sources.iter().enumerate() {
                let new_id = NodeId::new();
                map.insert(src.id, new_id);
                let parent = if i == 0 {
                    req.parent_id
                } else {
                    *src.parent_id
                        .and_then(|p| map.get(&p))
                        .ok_or_else(|| Error::Internal("nodes: copy without the parent".into()))?
                };
                let link_kind = match (src.link_kind, src.link_target) {
                    (Some(LinkKind::Member), Some(t))
                        if !shares.contains(&t) || loops.contains(&t) =>
                    {
                        Some(LinkKind::Shortcut)
                    }
                    (k, _) => k,
                };
                let node = Node {
                    id: new_id,
                    tenant_id: ctx.tenant,
                    parent_id: Some(parent),
                    name: if i == 0 {
                        root_name.clone()
                    } else {
                        src.name.clone()
                    },
                    kind: src.kind,
                    node_type: src.node_type.clone(),
                    link_kind,
                    link_target: src.link_target,
                    head_version: None,
                    owner_id: owner,
                    size_bytes: src.size_bytes,
                    revision: 1,
                    trashed_at: None,
                    created_at: now,
                    updated_at: now,
                    created_by: ctx.actor,
                    updated_by: ctx.actor,
                    my_role: None,
                    my_can_share: None,
                    editors_can_share: None,
                    editors_can_share_effective: None,
                };
                st.rows.insert(
                    new_id,
                    Row {
                        node: node.clone(),
                        trash_root: None,
                        purge_after: None,
                    },
                );
                made.push((node, src.id));
            }
        }
        let mut updates = Vec::new();
        for (node, src) in made.iter().skip(1) {
            self.append(
                ctx,
                node.id,
                Some(node.revision),
                EventType::NodeCreated,
                rules::created_data(node, Some(*src)),
            )?;
        }
        let root = made[0].0.clone();
        let change = self.append(
            ctx,
            root.id,
            Some(root.revision),
            EventType::NodeCopied,
            rules::copied_data(id, req.parent_id, &root.name, made.len()),
        )?;
        for (node, _) in &made {
            updates.extend(rules::created_tuples(node));
        }
        self.relate(updates).await?;
        // The content after the node entries: each copied file gets one version with the head
        // of its source (shared chunks) and its own `version.created` entry.
        if let Some(content) = content {
            for (src, (node, _)) in sources.iter().zip(made.iter()) {
                if src.head_version.is_some() {
                    let head = content.copy_head(ctx, src, node.id).await?;
                    let mut st = self.st();
                    let r = st.row_mut(ctx.tenant, node.id)?;
                    r.node.head_version = head;
                }
            }
        }
        let root = self.row(ctx.tenant, root.id)?.node;
        Ok(Written {
            value: self.out(ctx, root).await?,
            change,
        })
    }

    async fn trash(
        &self,
        ctx: &Context,
        id: NodeId,
        if_revision: Option<i64>,
    ) -> Result<Written<Node>> {
        let row = self.row(ctx.tenant, id)?;
        self.require(ctx, id, Role::Manage).await?;
        if row.node.parent_id.is_none() {
            return Err(Error::Api(
                crate::error::ApiError::new(
                    crate::error::ErrorCode::PermissionDenied,
                    "the root folder cannot be trashed",
                )
                .with_hint("trash the nodes inside it"),
            ));
        }
        if row.node.trashed_at.is_some() {
            return Ok(Written {
                value: self.out(ctx, row.node).await?,
                change: no_change(ctx),
            });
        }
        let windows = self.windows(ctx.tenant).await?;
        let _w = self.writes.lock().await;
        let mut updates = Vec::new();
        let done = {
            let mut st = self.st();
            let row = st.row(ctx.tenant, id)?.clone();
            if row.node.trashed_at.is_some() {
                Err(row.node)
            } else {
                rules::check_revision(&row.node, if_revision)?;
                let now = crate::now();
                let purge_after = windows.purge_after(now);
                let ids = st.subtree(ctx.tenant, id, true);
                let mut links = Vec::new();
                for n in &ids {
                    let r = st.row_mut(ctx.tenant, *n)?;
                    r.node.trashed_at = Some(now);
                    r.trash_root = Some(id);
                    r.purge_after = Some(purge_after);
                    touch(&mut r.node, ctx);
                    if let (Some(t), Some(p)) = (rules::member_target(&r.node), r.node.parent_id) {
                        links.push((t, p));
                    }
                }
                for (t, p) in links {
                    if !st.has_member_link(ctx.tenant, p, t) {
                        updates.push(RelationshipUpdate::Delete(tuples::member_link(t, p)));
                    }
                }
                let node = st.row(ctx.tenant, id)?.node.clone();
                let change = self.append(
                    ctx,
                    id,
                    Some(node.revision),
                    EventType::NodeTrashed,
                    rules::trashed_data(node.parent_id, &node.name, ids.len(), purge_after),
                )?;
                Ok((node, change))
            }
        };
        let (node, change) = match done {
            Ok(v) => v,
            Err(node) => (node, no_change(ctx)),
        };
        self.relate(updates).await?;
        Ok(Written {
            value: self.out(ctx, node).await?,
            change,
        })
    }

    async fn restore(
        &self,
        ctx: &Context,
        id: NodeId,
        req: RestoreRequest,
    ) -> Result<Written<Node>> {
        let row = self.trashed_root(ctx, id).await?;
        let windows = self.windows(ctx.tenant).await?;
        let admin = self.is_admin(ctx).await?;
        let at = row.node.trashed_at.unwrap_or_else(crate::now);
        windows.check_restore(at, admin, crate::now())?;
        rules::check_revision(&row.node, req.if_revision)?;
        let old_parent = row
            .node
            .parent_id
            .ok_or_else(|| Error::Internal("nodes: a trashed root".into()))?;
        let dest = match req.parent_id {
            Some(p) => {
                self.require(ctx, p, Role::Edit).await?;
                let d = self.live(ctx.tenant, p)?;
                if d.node.kind != NodeKind::Folder {
                    return Err(rules::invalid("the new parent is not a folder"));
                }
                p
            }
            None => {
                if self.live(ctx.tenant, old_parent).is_err() {
                    return Err(rules::conflict(
                        "the old parent folder is gone or in the trash",
                        "restore it into another folder: send parent_id",
                    ));
                }
                old_parent
            }
        };
        if dest != old_parent
            && let Some(t) = rules::member_target(&row.node)
            && self.live(ctx.tenant, t).is_ok()
        {
            self.require(ctx, t, Permission::Share).await?;
        }
        let _w = self.writes.lock().await;
        let mut updates = Vec::new();
        let (node, change) = {
            let mut st = self.st();
            let row = st.row(ctx.tenant, id)?.clone();
            if row.trash_root != Some(id) {
                return Err(rules::conflict(
                    "the node is not in the trash",
                    "read it again",
                ));
            }
            st.live(ctx.tenant, dest)?;
            let taken = st.taken(ctx.tenant, dest, Some(id));
            let name = rules::pick_name(&row.node.name, row.node.kind, req.on_conflict, &taken)?;
            let members: Vec<NodeId> = st
                .subtree(ctx.tenant, id, false)
                .into_iter()
                .filter(|n| st.rows.get(n).is_some_and(|r| r.trash_root == Some(id)))
                .collect();
            let pending = Pending {
                parents: HashMap::from([(id, dest)]),
                revived: members.iter().copied().collect(),
            };
            let mut edges = vec![(id, dest)];
            for n in &members {
                if let Some(r) = st.rows.get(n)
                    && let Some(t) = rules::member_target(&r.node)
                {
                    let folder = if *n == id {
                        Some(dest)
                    } else {
                        r.node.parent_id
                    };
                    edges.extend(folder.map(|f| (t, f)));
                }
            }
            if st.closes_view_loop(ctx.tenant, &edges, &pending) {
                return Err(rules::member_link_loop());
            }
            for n in &members {
                let r = st.row_mut(ctx.tenant, *n)?;
                r.node.trashed_at = None;
                r.trash_root = None;
                r.purge_after = None;
                touch(&mut r.node, ctx);
            }
            let r = st.row_mut(ctx.tenant, id)?;
            r.node.parent_id = Some(dest);
            r.node.name = name.clone();
            let node = r.node.clone();
            if dest != old_parent {
                updates.push(RelationshipUpdate::Delete(tuples::node_parent(
                    id, old_parent,
                )));
                updates.push(RelationshipUpdate::Touch(tuples::node_parent(id, dest)));
            }
            for n in &members {
                let Some(r) = st.rows.get(n) else { continue };
                if let (Some(t), Some(p)) = (rules::member_target(&r.node), r.node.parent_id)
                    && st.rows.contains_key(&t)
                {
                    updates.push(RelationshipUpdate::Touch(tuples::member_link(t, p)));
                }
            }
            if dest != old_parent
                && let Some(t) = rules::member_target(&node)
                && !st.has_member_link(ctx.tenant, old_parent, t)
            {
                updates.push(RelationshipUpdate::Delete(tuples::member_link(
                    t, old_parent,
                )));
            }
            let change = self.append(
                ctx,
                id,
                Some(node.revision),
                EventType::NodeRestored,
                rules::restored_data(dest, old_parent, &name),
            )?;
            (node, change)
        };
        self.relate(updates).await?;
        Ok(Written {
            value: self.out(ctx, node).await?,
            change,
        })
    }

    async fn list_trash(&self, ctx: &Context, all: bool, page: PageRequest) -> Result<Page<Node>> {
        let after = page
            .cursor
            .as_ref()
            .map(rules::parse_trash_cursor)
            .transpose()?;
        let admin = self.is_admin(ctx).await?;
        if all && !admin {
            return Err(Error::Api(
                crate::error::ApiError::new(
                    crate::error::ErrorCode::PermissionDenied,
                    "only tenant admins can list the whole trash (all=true)",
                )
                .with_hint("leave out all=true"),
            ));
        }
        let windows = self.windows(ctx.tenant).await?;
        let oldest = windows.oldest(all, crate::now());
        let limit = page.limit() as usize;
        let mut rows: Vec<Node> = self
            .st()
            .rows
            .values()
            .filter(|r| r.node.tenant_id == ctx.tenant && r.trash_root == Some(r.node.id))
            .filter(|r| r.node.trashed_at.is_some_and(|t| t >= oldest))
            .map(|r| r.node.clone())
            .collect();
        let key = |n: &Node| {
            (
                std::cmp::Reverse(rules::micros(n.trashed_at.unwrap_or(n.updated_at))),
                n.id.0,
            )
        };
        rows.sort_by_key(key);
        if let Some((t, i)) = after {
            rows.retain(|n| key(n) > (std::cmp::Reverse(t), i));
        }
        let more = rows.len() > limit;
        rows.truncate(limit);
        let next_cursor = if more {
            rows.last()
                .map(|n| rules::trash_cursor(n.trashed_at.unwrap_or(n.updated_at), n.id))
        } else {
            None
        };
        let items = self.present(ctx, rows, Some(Role::Manage)).await?;
        Ok(Page { items, next_cursor })
    }

    async fn purge(&self, ctx: &Context, id: NodeId) -> Result<Written<()>> {
        self.trashed_root(ctx, id).await?;
        let windows = self.windows(ctx.tenant).await?;
        windows.check_purge(self.is_admin(ctx).await?)?;
        let _w = self.writes.lock().await;
        let change = self.purge_root(ctx, id, false).await?;
        Ok(Written { value: (), change })
    }

    async fn empty_trash(&self, ctx: &Context) -> Result<(u32, Option<ChangeInfo>)> {
        let windows = self.windows(ctx.tenant).await?;
        let admin = self.is_admin(ctx).await?;
        if windows.check_purge(admin).is_err() {
            return Ok((0, None));
        }
        let oldest = windows.oldest(admin, crate::now());
        let roots: Vec<NodeId> = self
            .st()
            .rows
            .values()
            .filter(|r| r.node.tenant_id == ctx.tenant && r.trash_root == Some(r.node.id))
            .filter(|r| r.node.trashed_at.is_some_and(|t| t >= oldest))
            .map(|r| r.node.id)
            .collect();
        let views = rules::caller_view(&*self.authz, self, ctx, &roots).await?;
        let _w = self.writes.lock().await;
        let mut count = 0;
        let mut last = None;
        for (id, v) in roots.iter().zip(views) {
            if v.role != Some(Role::Manage) {
                continue;
            }
            match self.purge_root(ctx, *id, false).await {
                Ok(c) => {
                    count += 1;
                    last = Some(c);
                }
                Err(e) if rules::is_not_found(&e) => {}
                Err(e) => return Err(e),
            }
        }
        Ok((count, last))
    }

    async fn path_of(&self, ctx: &Context, id: NodeId) -> Result<String> {
        let (chain, names): (Vec<NodeId>, HashMap<NodeId, String>) = {
            let st = self.st();
            let mut chain = st.ancestors(ctx.tenant, id)?;
            chain.push(id);
            let names = chain
                .iter()
                .filter_map(|n| st.rows.get(n).map(|r| (*n, r.node.name.clone())))
                .collect();
            (chain, names)
        };
        path_from(&*self.authz, self, ctx, &chain, &names).await
    }

    async fn ancestors(&self, ctx: &Context, id: NodeId) -> Result<Vec<NodeId>> {
        self.st().ancestors(ctx.tenant, id)
    }
}

/// A `ChangeInfo` without a journal entry (a write that changed nothing).
pub(crate) fn no_change(ctx: &Context) -> ChangeInfo {
    ChangeInfo {
        seq: None,
        actor: ctx.actor,
        at: crate::now(),
    }
}

/// Checks the kind and the link of a new node; returns the link of a link node.
pub(crate) fn check_new_kind(req: &NewNode) -> Result<Option<LinkSpec>> {
    match (req.kind, &req.link) {
        (NodeKind::Typed, _) => Err(rules::invalid("typed nodes come in step 2")),
        (NodeKind::Link, Some(l)) => Ok(Some(l.clone())),
        (NodeKind::Link, None) => Err(rules::invalid("a link needs `link` (kind and target)")),
        (_, Some(_)) => Err(rules::invalid("only a link has `link`")),
        (_, None) => Ok(None),
    }
}

/// The display path of the last node of `chain` (root first), from the topmost ancestor that the
/// caller can see: names of ancestors the caller cannot see are left out, so a path never shows
/// them. The root has no name in a path. Needs `view` on the node.
pub(crate) async fn path_from(
    authz: &dyn Authz,
    nodes: &dyn NodeStore,
    ctx: &Context,
    chain: &[NodeId],
    names: &HashMap<NodeId, String>,
) -> Result<String> {
    let views = rules::caller_view(authz, nodes, ctx, chain).await?;
    if views.last().is_none_or(|v| v.role.is_none()) {
        return Err(rules::node_not_found());
    }
    let parts: Vec<&str> = chain
        .iter()
        .zip(&views)
        .skip(1)
        .filter(|(_, v)| v.role.is_some())
        .filter_map(|(n, _)| names.get(n).map(String::as_str))
        .collect();
    Ok(format!("/{}", parts.join("/")))
}
