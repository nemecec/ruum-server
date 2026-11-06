//! The conformance suite of [`BlobStore`]: the in-memory store, the `object_store` store over
//! `object_store::memory::InMemory`, and Garage when the S3 settings are set (`scripts/dev.sh
//! env`; the test skips without them).

use super::memory::MemoryBlobStore;
use super::object::{ObjectBlobStore, S3Settings, chunk_path};
use super::{BlobStore, Hash};
use crate::error::{Error, ErrorCode};
use crate::ids::TenantId;
use bytes::Bytes;

async fn suite(store: &dyn BlobStore) {
    let (a, b) = (TenantId::new(), TenantId::new());
    let data = Bytes::from(format!("chunk of {a}"));
    let h = Hash::of(&data);
    assert_eq!(store.has_chunks(a, &[h]).await.unwrap(), vec![false]);
    store.put_chunk(a, h, data.clone()).await.unwrap();
    // Storing again is a no-op.
    store.put_chunk(a, h, data.clone()).await.unwrap();
    assert_eq!(store.get_chunk(a, h).await.unwrap(), data);
    let other = Hash::of(b"other");
    assert_eq!(
        store.has_chunks(a, &[h, other]).await.unwrap(),
        vec![true, false]
    );
    // No dedup across tenants: tenant B does not see A's chunk.
    assert_eq!(store.has_chunks(b, &[h]).await.unwrap(), vec![false]);
    match store.get_chunk(b, h).await {
        Err(Error::Api(e)) => assert_eq!(e.code, ErrorCode::NotFound),
        other => panic!("tenant B read A's chunk: {other:?}"),
    }
    store.delete_chunk(a, h).await.unwrap();
    store.delete_chunk(a, h).await.unwrap();
    assert_eq!(store.has_chunks(a, &[h]).await.unwrap(), vec![false]);
    assert!(store.get_chunk(a, h).await.is_err());
}

#[test]
fn chunks_live_at_tenant_prefix_hash() {
    let t = TenantId::new();
    let h = Hash::of(b"x");
    let hex = h.to_hex();
    assert_eq!(
        chunk_path(t, &h).to_string(),
        format!("{t}/{}/{hex}", &hex[..2])
    );
}

#[tokio::test]
async fn memory_blob_store() {
    suite(&MemoryBlobStore::default()).await;
}

#[tokio::test]
async fn object_blob_store_in_memory() {
    suite(&ObjectBlobStore::in_memory()).await;
}

#[tokio::test]
async fn object_blob_store_on_garage() {
    let Some(settings) = S3Settings::from_env() else {
        eprintln!("skipped: no S3 settings (eval \"$(scripts/dev.sh env)\")");
        return;
    };
    let store = ObjectBlobStore::s3(&settings).unwrap();
    suite(&store).await;
}

// ---------------------------------------------------------------------------- content stores

use super::gc::GcReport;
use super::testing::World;
use super::{ByteRange, Transfers, UploadTarget, chunk, chunking_for};
use crate::actor::{Actor, Context};
use crate::authz::memory::MemoryAuthz;
use crate::authz::{Authz, RelationshipUpdate, Role, tuples};
use crate::blobs::postgres::PgContentStore;
use crate::blobs::{ContentSigner, NewUpload};
use crate::ids::{NodeId, UserId, VersionId};
use crate::journal::EventType;
use crate::nodes::{Node, NodeKind, NodeStore, OnConflict};
use crate::paging::PageRequest;
use crate::versions::{VersionFilter, VersionStore};
use async_trait::async_trait;
use futures::StreamExt;
use sqlx::{PgPool, Row};
use std::sync::Arc;

/// What the content suite needs from a store under test.
#[async_trait]
trait Harness: Send + Sync {
    fn transfers(&self) -> Arc<dyn Transfers>;
    fn versions(&self) -> Arc<dyn VersionStore>;
    fn owner(&self) -> Context;
    fn docs(&self) -> NodeId;
    /// A new user of the tenant with a grant of `role` on `node`.
    async fn user_with(&self, role: Role, node: NodeId) -> Context;
    /// The journal event types of the tenant, oldest first.
    async fn events(&self) -> Vec<EventType>;
    async fn head_of(&self, node: NodeId) -> (Option<VersionId>, i64);
    async fn gc(&self, grace: time::Duration) -> GcReport;
    /// Makes a new empty file in `Docs` and runs `copy_head` from `from` to it (what `copyNode`
    /// of the nodes area does). Returns the new file and the version it got.
    async fn copy_head(&self, from: NodeId) -> (NodeId, Option<crate::versions::Version>);
}

fn code<T: std::fmt::Debug>(r: crate::Result<T>) -> ErrorCode {
    match r {
        Err(Error::Api(e)) => e.code,
        other => panic!("expected an API error, got {other:?}"),
    }
}

async fn read_all(
    t: &dyn Transfers,
    ctx: &Context,
    node: NodeId,
    range: Option<ByteRange>,
) -> Vec<u8> {
    let c = t.open_content(ctx, node, None, range).await.unwrap();
    let mut out = Vec::new();
    let mut body = c.body;
    while let Some(p) = body.next().await {
        out.extend_from_slice(&p.unwrap());
    }
    out
}

fn request(target: UploadTarget, data: &[u8]) -> NewUpload {
    let params = chunking_for(data.len() as u64, Some("text/plain"));
    NewUpload {
        target,
        content_type: Some("text/plain".into()),
        chunking: params.id.to_string(),
        size: data.len() as u64,
        content_hash: Hash::of(data),
        chunks: chunk(data, params),
    }
}

async fn upload(
    t: &dyn Transfers,
    ctx: &Context,
    target: UploadTarget,
    data: &[u8],
) -> crate::Result<(Node, crate::versions::Version)> {
    let req = request(target, data);
    let up = t.create_upload(ctx, req.clone()).await?;
    let mut off = 0usize;
    for c in &req.chunks {
        let part = Bytes::copy_from_slice(&data[off..off + c.size as usize]);
        off += c.size as usize;
        if up.missing.contains(&c.hash) {
            t.put_chunk(ctx, up.id, c.hash, part).await?;
        }
    }
    Ok(t.complete_upload(ctx, up.id).await?.value)
}

fn new_file(parent: NodeId, name: &str, on_conflict: OnConflict) -> UploadTarget {
    UploadTarget::NewFile {
        parent_id: parent,
        name: name.into(),
        on_conflict,
    }
}

/// The conformance suite of the content stores (`Transfers` and `VersionStore`).
async fn content_suite(h: &dyn Harness) {
    let t = h.transfers();
    let vs = h.versions();
    let owner = h.owner();
    let data: Vec<u8> = (0..150_000u32).map(|i| (i * 7 % 251) as u8).collect();

    // A new file: node, version, head, entries.
    let (node, v1) = upload(
        &*t,
        &owner,
        new_file(h.docs(), "a.bin", OnConflict::Fail),
        &data,
    )
    .await
    .unwrap();
    assert_eq!(node.kind, NodeKind::File);
    assert_eq!(h.head_of(node.id).await, (Some(v1.id), data.len() as i64));
    assert_eq!(
        h.events().await,
        vec![EventType::NodeCreated, EventType::VersionCreated]
    );
    assert_eq!(read_all(&*t, &owner, node.id, None).await, data);
    assert_eq!(
        read_all(
            &*t,
            &owner,
            node.id,
            Some(ByteRange {
                start: 100,
                end: 199
            })
        )
        .await,
        data[100..200]
    );

    // The same content again: nothing is missing. The same name: name_taken, or renamed.
    let up = t
        .create_upload(
            &owner,
            request(new_file(h.docs(), "b.bin", OnConflict::Fail), &data),
        )
        .await
        .unwrap();
    assert!(up.missing.is_empty());
    assert_eq!(
        code(
            upload(
                &*t,
                &owner,
                new_file(h.docs(), "A.BIN", OnConflict::Fail),
                b"x"
            )
            .await
        ),
        ErrorCode::NameTaken
    );
    let (renamed, _) = upload(
        &*t,
        &owner,
        new_file(h.docs(), "a.bin", OnConflict::Rename),
        b"x",
    )
    .await
    .unwrap();
    assert_eq!(renamed.name, "a (2).bin");

    // Uploads belong to their actor.
    let editor = h.user_with(Role::Edit, h.docs()).await;
    assert_eq!(
        code(t.get_upload(&editor, up.id).await),
        ErrorCode::NotFound
    );
    t.abort_upload(&owner, up.id).await.unwrap();
    assert_eq!(
        code(t.complete_upload(&owner, up.id).await),
        ErrorCode::Conflict
    );

    // putContent: the same actor coalesces; If-Match guards; another actor makes a version.
    let w = t
        .put_content(
            &owner,
            node.id,
            Bytes::from_static(b"two"),
            None,
            Some(v1.id),
        )
        .await
        .unwrap();
    assert_eq!(w.value.1.id, v1.id);
    assert_eq!(read_all(&*t, &owner, node.id, None).await, b"two");
    assert_eq!(
        code(
            t.put_content(
                &editor,
                node.id,
                Bytes::from_static(b"x"),
                None,
                Some(VersionId::new())
            )
            .await
        ),
        ErrorCode::PreconditionFailed
    );
    let v2 = t
        .put_content(&editor, node.id, Bytes::from_static(b"three"), None, None)
        .await
        .unwrap()
        .value
        .1;
    assert_ne!(v2.id, v1.id);
    assert_eq!(h.head_of(node.id).await, (Some(v2.id), 5));

    // Restore and label; list with paging and filters.
    let v3 = vs.restore(&owner, node.id, v1.id).await.unwrap().value;
    assert_eq!(v3.restored_from, Some(v1.id));
    assert_eq!(read_all(&*t, &owner, node.id, None).await, b"two");
    vs.set_label(&owner, node.id, v1.id, Some("First".into()))
        .await
        .unwrap();
    let p1 = vs
        .list(
            &owner,
            node.id,
            PageRequest {
                cursor: None,
                limit: Some(2),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        p1.items.iter().map(|v| v.id).collect::<Vec<_>>(),
        vec![v3.id, v2.id]
    );
    let p2 = vs
        .list(
            &owner,
            node.id,
            PageRequest {
                cursor: p1.next_cursor,
                limit: Some(2),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        p2.items.iter().map(|v| v.id).collect::<Vec<_>>(),
        vec![v1.id]
    );
    assert!(p2.next_cursor.is_none());
    let named = vs
        .list_filtered(
            &owner,
            node.id,
            VersionFilter {
                labeled: Some(true),
                ..Default::default()
            },
            PageRequest::default(),
        )
        .await
        .unwrap();
    assert_eq!(named.items.len(), 1);
    assert_eq!(
        vs.get(&owner, node.id, v1.id)
            .await
            .unwrap()
            .label
            .as_deref(),
        Some("First")
    );
    let ev = h.events().await;
    assert_eq!(ev.last(), Some(&EventType::VersionLabeled));
    assert!(ev.contains(&EventType::VersionRestored));

    // Permissions and tenants.
    let viewer = h.user_with(Role::View, h.docs()).await;
    assert_eq!(read_all(&*t, &viewer, node.id, None).await, b"two");
    assert_eq!(
        code(
            t.put_content(&viewer, node.id, Bytes::from_static(b"x"), None, None)
                .await
        ),
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        code(vs.set_label(&viewer, node.id, v1.id, None).await),
        ErrorCode::PermissionDenied
    );
    let stranger = Context {
        actor: Actor::user(UserId::new()),
        ..owner.clone()
    };
    assert_eq!(
        code(t.open_content(&stranger, node.id, None, None).await),
        ErrorCode::NotFound
    );
    let elsewhere = Context {
        tenant: TenantId::new(),
        ..owner.clone()
    };
    assert_eq!(
        code(vs.get(&elsewhere, node.id, v1.id).await),
        ErrorCode::NotFound
    );
    assert_eq!(
        code(t.open_content(&elsewhere, node.id, None, None).await),
        ErrorCode::NotFound
    );

    // Signed grants.
    let (token, claims) = t
        .content_grant(
            &viewer,
            node.id,
            Some(v2.id),
            super::Disposition::Attachment,
        )
        .await
        .unwrap();
    assert_eq!(claims.version, v2.id);
    let (c, _) = t.open_granted(&token, None).await.unwrap();
    assert_eq!(c.total_size, 5);

    // copy_head: the copy gets one version with the content of the head (shared chunks), and
    // `version.created`; a node without content gives no version.
    let (copy, cv) = h.copy_head(node.id).await;
    let cv = cv.expect("the copy has a version");
    assert_eq!(h.head_of(copy).await, (Some(cv.id), 3));
    assert_eq!(read_all(&*t, &owner, copy, None).await, b"two");
    assert_eq!(h.events().await.last(), Some(&EventType::VersionCreated));
    let (empty, none) = h.copy_head(h.docs()).await;
    assert!(none.is_none());
    assert_eq!(h.head_of(empty).await, (None, 0));

    // Garbage collection: chunks that no version and no open upload uses go (the aborted
    // upload's chunk, and the first content of a.bin, which the coalesced save replaced);
    // nothing in the grace period goes, and the live files stay readable.
    let req = request(
        new_file(h.docs(), "drop.txt", OnConflict::Fail),
        b"only in this upload",
    );
    let up = t.create_upload(&owner, req.clone()).await.unwrap();
    t.put_chunk(
        &owner,
        up.id,
        req.chunks[0].hash,
        Bytes::from_static(b"only in this upload"),
    )
    .await
    .unwrap();
    t.abort_upload(&owner, up.id).await.unwrap();
    assert_eq!(h.gc(time::Duration::hours(1)).await.deleted, 0);
    let r = h.gc(time::Duration::seconds(-1)).await;
    assert!(r.deleted >= 2, "{r:?}");
    assert_eq!(h.gc(time::Duration::seconds(-1)).await.deleted, 0);
    assert_eq!(read_all(&*t, &owner, node.id, None).await, b"two");
    assert_eq!(read_all(&*t, &owner, renamed.id, None).await, b"x");
    assert_eq!(read_all(&*t, &owner, copy, None).await, b"two");
}

struct MemoryHarness(World);

#[async_trait]
impl Harness for MemoryHarness {
    fn transfers(&self) -> Arc<dyn Transfers> {
        self.0.store.clone()
    }
    fn versions(&self) -> Arc<dyn VersionStore> {
        self.0.store.clone()
    }
    fn owner(&self) -> Context {
        self.0.owner_ctx()
    }
    fn docs(&self) -> NodeId {
        self.0.docs
    }
    async fn user_with(&self, role: Role, node: NodeId) -> Context {
        self.0.user_with(role, node).await
    }
    async fn events(&self) -> Vec<EventType> {
        self.0
            .journal
            .entries(self.0.tenant)
            .iter()
            .map(|e| e.event)
            .collect()
    }
    async fn head_of(&self, node: NodeId) -> (Option<VersionId>, i64) {
        let n = self.0.nodes.get(&self.0.owner_ctx(), node).await.unwrap();
        (n.head_version, n.size_bytes)
    }
    async fn gc(&self, grace: time::Duration) -> GcReport {
        self.0
            .store
            .collect_garbage(self.0.tenant, grace)
            .await
            .unwrap()
    }
    async fn copy_head(&self, from: NodeId) -> (NodeId, Option<crate::versions::Version>) {
        let w = &self.0;
        let to = super::testing::node(
            w.tenant,
            NodeId::new(),
            Some(w.docs),
            &format!("copy {}.bin", uuid::Uuid::now_v7()),
            NodeKind::File,
            w.owner,
        );
        w.authz
            .write(vec![RelationshipUpdate::Touch(tuples::node_parent(
                to.id, w.docs,
            ))])
            .await
            .unwrap();
        w.nodes.insert(to.clone());
        let id = to.id;
        (
            id,
            w.store.copy_head(&w.owner_ctx(), from, to).await.unwrap(),
        )
    }
}

#[tokio::test]
async fn memory_content_store_conforms() {
    content_suite(&MemoryHarness(World::new().await)).await;
}

/// A node store over the `nodes` table, for the Postgres suite (the nodes area builds the real
/// one). `get` and `ancestors` only.
struct PgNodes {
    pool: PgPool,
    inner: Arc<super::testing::FakeNodes>,
}

#[async_trait]
impl NodeStore for PgNodes {
    async fn root(&self, c: &Context) -> crate::Result<Node> {
        self.inner.root(c).await
    }
    async fn create(
        &self,
        c: &Context,
        r: crate::nodes::NewNode,
    ) -> crate::Result<crate::journal::Written<Node>> {
        self.inner.create(c, r).await
    }
    async fn get(&self, ctx: &Context, id: NodeId) -> crate::Result<Node> {
        let r = sqlx::query(
            "select id, parent_id, name, kind, type, head_version, size_bytes, revision, owner_id \
             from nodes where tenant_id = $1 and id = $2",
        )
        .bind(ctx.tenant.0)
        .bind(id.0)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| Error::not_found("node"))?;
        let mut n = super::testing::node(
            ctx.tenant,
            id,
            r.get::<Option<uuid::Uuid>, _>("parent_id").map(NodeId),
            &r.get::<String, _>("name"),
            r.get("kind"),
            UserId(r.get("owner_id")),
        );
        n.node_type = r.get("type");
        n.head_version = r
            .get::<Option<uuid::Uuid>, _>("head_version")
            .map(VersionId);
        n.size_bytes = r.get("size_bytes");
        n.revision = r.get("revision");
        Ok(n)
    }
    async fn get_by_path(&self, c: &Context, p: &str) -> crate::Result<Node> {
        self.inner.get_by_path(c, p).await
    }
    async fn list_children(
        &self,
        c: &Context,
        p: NodeId,
        o: crate::nodes::ChildOrder,
        pg: PageRequest,
    ) -> crate::Result<crate::paging::Page<Node>> {
        self.inner.list_children(c, p, o, pg).await
    }
    async fn rename(
        &self,
        c: &Context,
        i: NodeId,
        n: String,
        o: OnConflict,
        r: Option<i64>,
    ) -> crate::Result<crate::journal::Written<Node>> {
        self.inner.rename(c, i, n, o, r).await
    }
    async fn move_node(
        &self,
        c: &Context,
        i: NodeId,
        r: crate::nodes::MoveRequest,
    ) -> crate::Result<crate::journal::Written<Node>> {
        self.inner.move_node(c, i, r).await
    }
    async fn copy(
        &self,
        c: &Context,
        i: NodeId,
        r: crate::nodes::CopyRequest,
    ) -> crate::Result<crate::journal::Written<Node>> {
        self.inner.copy(c, i, r).await
    }
    async fn trash(
        &self,
        c: &Context,
        i: NodeId,
        r: Option<i64>,
    ) -> crate::Result<crate::journal::Written<Node>> {
        self.inner.trash(c, i, r).await
    }
    async fn restore(
        &self,
        c: &Context,
        i: NodeId,
        r: crate::nodes::RestoreRequest,
    ) -> crate::Result<crate::journal::Written<Node>> {
        self.inner.restore(c, i, r).await
    }
    async fn list_trash(
        &self,
        c: &Context,
        a: bool,
        p: PageRequest,
    ) -> crate::Result<crate::paging::Page<Node>> {
        self.inner.list_trash(c, a, p).await
    }
    async fn purge(&self, c: &Context, i: NodeId) -> crate::Result<crate::journal::Written<()>> {
        self.inner.purge(c, i).await
    }
    async fn empty_trash(
        &self,
        c: &Context,
    ) -> crate::Result<(u32, Option<crate::journal::ChangeInfo>)> {
        self.inner.empty_trash(c).await
    }
    async fn path_of(&self, c: &Context, i: NodeId) -> crate::Result<String> {
        self.inner.path_of(c, i).await
    }
    async fn ancestors(&self, ctx: &Context, id: NodeId) -> crate::Result<Vec<NodeId>> {
        let ids: Vec<uuid::Uuid> = sqlx::query_scalar(
            "with recursive up(id, parent_id, depth) as ( \
               select id, parent_id, 0 from nodes where tenant_id = $1 and id = $2 \
               union all select n.id, n.parent_id, up.depth + 1 from nodes n \
               join up on n.id = up.parent_id where n.tenant_id = $1) \
             select id from up where depth > 0 order by depth desc",
        )
        .bind(ctx.tenant.0)
        .bind(id.0)
        .fetch_all(&self.pool)
        .await?;
        Ok(ids.into_iter().map(NodeId).collect())
    }
}

struct PgHarness {
    pool: PgPool,
    store_pool: PgPool,
    tenant: TenantId,
    owner: UserId,
    docs: NodeId,
    authz: Arc<MemoryAuthz>,
    store: Arc<PgContentStore>,
}

/// A second pool on the test database whose connections set `ruum.system = on`, so the harness
/// can seed and check rows across row-level security. The store under test uses the plain pool.
async fn system_pool(pool: &PgPool) -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("select set_config('ruum.system', 'on', false)")
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap()
}

impl PgHarness {
    async fn new(store_pool: PgPool) -> Self {
        let pool = system_pool(&store_pool).await;
        let tenant = TenantId::new();
        let owner = UserId::new();
        let root = NodeId::new();
        let docs = NodeId::new();
        let mut tx = pool.begin().await.unwrap();
        sqlx::query(
            "insert into tenants (id, name, kind, plan, root_node_id, created_kind, created_by) \
             values ($1, 'mine', 'personal', 'personal', $2, 'user', $3)",
        )
        .bind(tenant.0)
        .bind(root.0)
        .bind(owner.0)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query(
            "insert into users (id, tenant_id, email, email_verified, display_name) \
             values ($1, $2, $3, true, 'Owner')",
        )
        .bind(owner.0)
        .bind(tenant.0)
        .bind(format!("{}@example.test", owner.0))
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query(
            "insert into memberships (tenant_id, user_id, role, created_kind, created_by) \
             values ($1, $2, 'owner', 'user', $2)",
        )
        .bind(tenant.0)
        .bind(owner.0)
        .execute(&mut *tx)
        .await
        .unwrap();
        for (id, parent, name) in [(root, None, "root"), (docs, Some(root), "Docs")] {
            sqlx::query(
                "insert into nodes (id, tenant_id, parent_id, name, kind, owner_id, \
                 created_kind, created_by, updated_kind, updated_by) \
                 values ($1, $2, $3, $4, 'folder', $5, 'user', $5, 'user', $5)",
            )
            .bind(id.0)
            .bind(tenant.0)
            .bind(parent.map(|p: NodeId| p.0))
            .bind(name)
            .bind(owner.0)
            .execute(&mut *tx)
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();
        let authz = Arc::new(MemoryAuthz::default());
        authz
            .write(vec![
                RelationshipUpdate::Touch(tuples::personal_root_owner(root, owner)),
                RelationshipUpdate::Touch(tuples::node_tenant(root, tenant)),
                RelationshipUpdate::Touch(tuples::node_parent(docs, root)),
            ])
            .await
            .unwrap();
        let nodes = Arc::new(PgNodes {
            pool: pool.clone(),
            inner: Arc::new(super::testing::FakeNodes {
                nodes: Default::default(),
                authz: authz.clone(),
                journal: Default::default(),
            }),
        });
        let store = Arc::new(PgContentStore::new(
            store_pool.clone(),
            authz.clone(),
            nodes,
            Arc::new(MemoryBlobStore::default()),
            ContentSigner::random(),
        ));
        Self {
            pool,
            store_pool,
            tenant,
            owner,
            docs,
            authz,
            store,
        }
    }
}

#[async_trait]
impl Harness for PgHarness {
    fn transfers(&self) -> Arc<dyn Transfers> {
        self.store.clone()
    }
    fn versions(&self) -> Arc<dyn VersionStore> {
        self.store.clone()
    }
    fn owner(&self) -> Context {
        Context {
            tenant: self.tenant,
            actor: Actor::user(self.owner),
            scope: None,
            idempotency_key: None,
        }
    }
    fn docs(&self) -> NodeId {
        self.docs
    }
    async fn user_with(&self, role: Role, node: NodeId) -> Context {
        let u = UserId::new();
        sqlx::query(
            "insert into users (id, tenant_id, email, email_verified, display_name) \
             values ($1, $2, $3, true, 'User')",
        )
        .bind(u.0)
        .bind(self.tenant.0)
        .bind(format!("{}@example.test", u.0))
        .execute(&self.pool)
        .await
        .unwrap();
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
        Context {
            actor: Actor::user(u),
            ..self.owner()
        }
    }
    async fn events(&self) -> Vec<EventType> {
        sqlx::query("select type from journal where tenant_id = $1 order by seq")
            .bind(self.tenant.0)
            .fetch_all(&self.pool)
            .await
            .unwrap()
            .iter()
            .map(|r| EventType::parse(&r.get::<String, _>("type")).unwrap())
            .collect()
    }
    async fn head_of(&self, node: NodeId) -> (Option<VersionId>, i64) {
        let r = sqlx::query(
            "select head_version, size_bytes from nodes where tenant_id = $1 and id = $2",
        )
        .bind(self.tenant.0)
        .bind(node.0)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        (
            r.get::<Option<uuid::Uuid>, _>("head_version")
                .map(VersionId),
            r.get("size_bytes"),
        )
    }
    async fn gc(&self, grace: time::Duration) -> GcReport {
        self.store
            .collect_garbage(self.tenant, grace)
            .await
            .unwrap()
    }
    async fn copy_head(&self, from: NodeId) -> (NodeId, Option<crate::versions::Version>) {
        let to = NodeId::new();
        sqlx::query(
            "insert into nodes (id, tenant_id, parent_id, name, kind, owner_id, \
             created_kind, created_by, updated_kind, updated_by) \
             values ($1, $2, $3, $4, 'file', $5, 'user', $5, 'user', $5)",
        )
        .bind(to.0)
        .bind(self.tenant.0)
        .bind(self.docs.0)
        .bind(format!("copy {}.bin", to.0))
        .bind(self.owner.0)
        .execute(&self.pool)
        .await
        .unwrap();
        self.authz
            .write(vec![RelationshipUpdate::Touch(tuples::node_parent(
                to, self.docs,
            ))])
            .await
            .unwrap();
        let ctx = self.owner();
        let mut tx = crate::journal::postgres::begin_tenant(&self.store_pool, ctx.tenant)
            .await
            .unwrap();
        let v = crate::blobs::postgres::copy_head(&mut tx, &ctx, from, to)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        (to, v)
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn postgres_content_store_conforms(pool: PgPool) {
    let h = PgHarness::new(pool.clone()).await;
    content_suite(&h).await;
    // The relationship of the new file reached the engine through the outbox.
    let pending = crate::authz::outbox::pending_count(&pool).await.unwrap();
    assert_eq!(pending, 0);
    // Maintenance (expiry, retention, garbage collection) runs.
    h.store
        .maintain(h.tenant, crate::blobs::gc::DEFAULT_GRACE)
        .await
        .unwrap();
}
