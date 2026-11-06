//! Test helpers of the blobs and versions areas: a small node store (the nodes area builds the
//! real one) and a test world with a tenant, its owner and the in-memory content store.

use crate::actor::{Actor, Context, PrincipalKind, PrincipalRef};
use crate::authz::memory::MemoryAuthz;
use crate::authz::{Authz, RelationshipUpdate, Role, Scope, tuples};
use crate::blobs::memory::{ContentDeps, MemoryBlobStore, MemoryContentStore};
use crate::blobs::{BlobStore, ContentSigner};
use crate::error::{Error, ErrorCode, Result};
use crate::ids::{NodeId, PrincipalId, TenantId, UserId, VersionId};
use crate::journal::memory::MemoryJournal;
use crate::journal::{ChangeInfo, EventType, NewEntry, Written};
use crate::nodes::*;
use crate::paging::{Page, PageRequest};
use crate::versions::HeadUpdate;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// The one place that builds a [`Node`] in the tests of this area.
pub fn node(
    tenant: TenantId,
    id: NodeId,
    parent: Option<NodeId>,
    name: &str,
    kind: NodeKind,
    owner: UserId,
) -> Node {
    let now = crate::now();
    Node {
        id,
        tenant_id: tenant,
        parent_id: parent,
        name: name.to_string(),
        kind,
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
        created_by: Actor::user(owner),
        updated_by: Actor::user(owner),
        my_role: None,
        my_can_share: None,
        editors_can_share: None,
        editors_can_share_effective: None,
    }
}

/// A node store with `get`, `create` (files and folders), `ancestors` and [`HeadUpdate`]. It
/// filters by tenant and checks `edit` on the parent for `create`; `get` checks nothing else.
pub struct FakeNodes {
    pub nodes: RwLock<HashMap<NodeId, Node>>,
    pub authz: Arc<dyn Authz>,
    pub journal: Arc<MemoryJournal>,
}

impl FakeNodes {
    pub fn insert(&self, n: Node) {
        self.nodes.write().unwrap().insert(n.id, n);
    }
}

#[async_trait]
impl NodeStore for FakeNodes {
    async fn root(&self, _: &Context) -> Result<Node> {
        unimplemented!()
    }
    async fn create(&self, ctx: &Context, req: NewNode) -> Result<Written<Node>> {
        crate::authz::require(&*self.authz, self, ctx, req.parent_id, Role::Edit).await?;
        let mut name = req.name.clone();
        {
            let nodes = self.nodes.read().unwrap();
            let taken = |n: &str| {
                nodes.values().any(|x| {
                    x.parent_id == Some(req.parent_id)
                        && x.trashed_at.is_none()
                        && x.name.to_lowercase() == n.to_lowercase()
                })
            };
            let mut i = 2;
            while taken(&name) {
                if req.on_conflict == OnConflict::Fail {
                    return Err(Error::api(ErrorCode::NameTaken, "name taken"));
                }
                name = crate::blobs::logic::numbered_name(&req.name, i);
                i += 1;
            }
        }
        let owner = ctx
            .actor
            .effective_user()
            .unwrap_or(UserId(uuid::Uuid::nil()));
        let mut n = node(
            ctx.tenant,
            NodeId::new(),
            Some(req.parent_id),
            &name,
            req.kind,
            owner,
        );
        n.node_type = req.node_type.clone();
        n.created_by = ctx.actor;
        n.updated_by = ctx.actor;
        self.authz
            .write(vec![RelationshipUpdate::Touch(tuples::node_parent(
                n.id,
                req.parent_id,
            ))])
            .await?;
        self.insert(n.clone());
        let e = self.journal.append(
            ctx,
            NewEntry {
                node_id: Some(n.id),
                revision: Some(n.revision),
                event: EventType::NodeCreated,
                data: crate::blobs::logic::node_created_data(
                    req.parent_id,
                    &name,
                    req.node_type.as_deref(),
                ),
            },
        )?;
        Ok(Written {
            value: n,
            change: ChangeInfo {
                seq: Some(e.seq),
                actor: e.actor,
                at: e.at,
            },
        })
    }
    async fn get(&self, ctx: &Context, id: NodeId) -> Result<Node> {
        self.nodes
            .read()
            .unwrap()
            .get(&id)
            .filter(|n| n.tenant_id == ctx.tenant)
            .cloned()
            .ok_or_else(|| Error::not_found("node"))
    }
    async fn get_by_path(&self, _: &Context, _: &str) -> Result<Node> {
        unimplemented!()
    }
    async fn list_children(
        &self,
        _: &Context,
        _: NodeId,
        _: ChildOrder,
        _: PageRequest,
    ) -> Result<Page<Node>> {
        unimplemented!()
    }
    async fn rename(
        &self,
        _: &Context,
        _: NodeId,
        _: String,
        _: OnConflict,
        _: Option<i64>,
    ) -> Result<Written<Node>> {
        unimplemented!()
    }
    async fn move_node(&self, _: &Context, _: NodeId, _: MoveRequest) -> Result<Written<Node>> {
        unimplemented!()
    }
    async fn copy(&self, _: &Context, _: NodeId, _: CopyRequest) -> Result<Written<Node>> {
        unimplemented!()
    }
    async fn trash(&self, _: &Context, _: NodeId, _: Option<i64>) -> Result<Written<Node>> {
        unimplemented!()
    }
    async fn restore(&self, _: &Context, _: NodeId, _: RestoreRequest) -> Result<Written<Node>> {
        unimplemented!()
    }
    async fn list_trash(&self, _: &Context, _: bool, _: PageRequest) -> Result<Page<Node>> {
        unimplemented!()
    }
    async fn purge(&self, _: &Context, _: NodeId) -> Result<Written<()>> {
        unimplemented!()
    }
    async fn empty_trash(&self, _: &Context) -> Result<(u32, Option<ChangeInfo>)> {
        unimplemented!()
    }
    async fn path_of(&self, _: &Context, _: NodeId) -> Result<String> {
        unimplemented!()
    }
    async fn ancestors(&self, _: &Context, id: NodeId) -> Result<Vec<NodeId>> {
        let nodes = self.nodes.read().unwrap();
        let mut out = Vec::new();
        let mut cur = nodes.get(&id).and_then(|n| n.parent_id);
        while let Some(p) = cur {
            out.push(p);
            cur = nodes.get(&p).and_then(|n| n.parent_id);
        }
        out.reverse();
        Ok(out)
    }
}

#[async_trait]
impl HeadUpdate for FakeNodes {
    async fn set_head(
        &self,
        ctx: &Context,
        node: NodeId,
        version: VersionId,
        size_bytes: i64,
    ) -> Result<Node> {
        let mut nodes = self.nodes.write().unwrap();
        let n = nodes
            .get_mut(&node)
            .filter(|n| n.tenant_id == ctx.tenant)
            .ok_or_else(|| Error::not_found("node"))?;
        n.head_version = Some(version);
        n.size_bytes = size_bytes;
        n.revision += 1;
        n.updated_at = crate::now();
        n.updated_by = ctx.actor;
        Ok(n.clone())
    }
}

/// A tenant with an owner, a root folder and a `Docs` folder, and the in-memory content store.
pub struct World {
    pub tenant: TenantId,
    pub owner: UserId,
    pub root: NodeId,
    pub docs: NodeId,
    pub authz: Arc<MemoryAuthz>,
    pub journal: Arc<MemoryJournal>,
    pub nodes: Arc<FakeNodes>,
    pub blobs: Arc<MemoryBlobStore>,
    pub store: Arc<MemoryContentStore>,
}

impl World {
    pub async fn new() -> Self {
        let tenant = TenantId::new();
        let owner = UserId::new();
        let root = NodeId::new();
        let docs = NodeId::new();
        let authz = Arc::new(MemoryAuthz::default());
        let journal = Arc::new(MemoryJournal::default());
        let nodes = Arc::new(FakeNodes {
            nodes: RwLock::new(HashMap::new()),
            authz: authz.clone(),
            journal: journal.clone(),
        });
        nodes.insert(node(tenant, root, None, "root", NodeKind::Folder, owner));
        nodes.insert(node(
            tenant,
            docs,
            Some(root),
            "Docs",
            NodeKind::Folder,
            owner,
        ));
        authz
            .write(vec![
                RelationshipUpdate::Touch(tuples::personal_root_owner(root, owner)),
                RelationshipUpdate::Touch(tuples::node_tenant(root, tenant)),
                RelationshipUpdate::Touch(tuples::node_parent(docs, root)),
            ])
            .await
            .unwrap();
        let blobs = Arc::new(MemoryBlobStore::default());
        let store = Arc::new(MemoryContentStore::new(
            ContentDeps {
                nodes: nodes.clone(),
                authz: authz.clone(),
                journal: journal.clone(),
                blobs: blobs.clone() as Arc<dyn BlobStore>,
                tenants: None,
                heads: Some(nodes.clone()),
            },
            ContentSigner::random(),
        ));
        Self {
            tenant,
            owner,
            root,
            docs,
            authz,
            journal,
            nodes,
            blobs,
            store,
        }
    }

    pub fn ctx(&self, actor: Actor) -> Context {
        Context {
            tenant: self.tenant,
            actor,
            scope: None,
            idempotency_key: None,
        }
    }

    pub fn owner_ctx(&self) -> Context {
        self.ctx(Actor::user(self.owner))
    }

    /// A new user with a grant of `role` on `node`.
    pub async fn user_with(&self, role: Role, node: NodeId) -> Context {
        let u = UserId::new();
        self.authz
            .write(vec![RelationshipUpdate::Touch(
                crate::authz::Relationship {
                    object: crate::authz::ObjectRef::node(node),
                    relation: role.relation().to_string(),
                    subject: crate::authz::SubjectRef::user(u),
                },
            )])
            .await
            .unwrap();
        self.ctx(Actor::user(u))
    }

    /// An agent of the owner with this scope.
    pub fn agent_ctx(&self, subtree: Option<NodeId>, max_role: Role) -> Context {
        Context {
            tenant: self.tenant,
            actor: Actor {
                actor: PrincipalRef {
                    kind: PrincipalKind::Agent,
                    id: PrincipalId::new(),
                },
                on_behalf_of: Some(self.owner),
            },
            scope: Some(Scope {
                subtree,
                max_role,
                expires_at: None,
            }),
            idempotency_key: None,
        }
    }
}
