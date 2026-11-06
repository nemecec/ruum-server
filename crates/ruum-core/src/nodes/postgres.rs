//! The Postgres node store: table `nodes`, relationships through the outbox (`authz_outbox`),
//! and the journal in the same transaction.
//!
//! Every write runs in one transaction that sets `ruum.tenant_id` (row-level security), checks
//! and locks the rows it changes, changes them, enqueues the relationship updates
//! ([`crate::authz::outbox::enqueue`]) and appends the journal entries
//! ([`crate::journal::postgres::append`]). After the commit the outbox rows are applied to the
//! engine ([`crate::authz::outbox::apply_after_commit`]); the worker retries what fails.
//! Permission checks run on the engine before the transaction.
//!
//! Names are unique among the live children of a folder by the index `nodes_live_name_key`
//! (`lower(name)`); a race on a name ends in `name_taken`. The trigger
//! `nodes_link_target_check` refuses a link target of another tenant.

use super::rules::{self, ChildKey, TrashWindows};
use super::*;
use crate::actor::{PrincipalKind, PrincipalRef};
use crate::authz::{self, Authz, ObjectRef, Permission, RelationshipUpdate, outbox, tuples};
use crate::error::{ApiError, Error, ErrorCode};
use crate::ids::PrincipalId;
use crate::journal::postgres::{append, begin_tenant};
use crate::journal::{EventType, NewEntry};
use crate::tenants::{MemberRole, Plan};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Row};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use uuid::Uuid;

/// The columns that [`node_of`] reads (with the table alias `n`).
const COLS: &str = "n.id, n.tenant_id, n.parent_id, n.name, n.kind, n.type, n.link_kind, \
    n.link_target, n.head_version, n.editors_can_share, n.owner_id, n.size_bytes, n.revision, \
    n.trashed_at, n.trash_root, n.created_at, n.created_kind, n.created_by, n.created_obo, \
    n.updated_at, n.updated_kind, n.updated_by, n.updated_obo";

/// The node columns with the table alias `n`, for the queries of other stores (tags, search).
pub(crate) const COLUMNS: &str = COLS;

/// A node row: the node (without the caller fields) and its trash root.
#[derive(Clone, Debug)]
pub(crate) struct Row0 {
    pub node: Node,
    pub trash_root: Option<NodeId>,
}

fn actor_of(row: &PgRow, kind: &str, by: &str, obo: &str) -> Result<Actor> {
    Ok(Actor {
        actor: PrincipalRef {
            kind: row.try_get::<PrincipalKind, _>(kind)?,
            id: PrincipalId(row.try_get::<Uuid, _>(by)?),
        },
        on_behalf_of: row.try_get::<Option<Uuid>, _>(obo)?.map(UserId),
    })
}

pub(crate) fn node_of(row: &PgRow) -> Result<Row0> {
    Ok(Row0 {
        node: Node {
            id: NodeId(row.try_get("id")?),
            tenant_id: TenantId(row.try_get("tenant_id")?),
            parent_id: row.try_get::<Option<Uuid>, _>("parent_id")?.map(NodeId),
            name: row.try_get("name")?,
            kind: row.try_get("kind")?,
            node_type: row.try_get("type")?,
            link_kind: row.try_get("link_kind")?,
            link_target: row.try_get::<Option<Uuid>, _>("link_target")?.map(NodeId),
            head_version: row
                .try_get::<Option<Uuid>, _>("head_version")?
                .map(VersionId),
            owner_id: UserId(row.try_get("owner_id")?),
            size_bytes: row.try_get("size_bytes")?,
            revision: row.try_get("revision")?,
            trashed_at: row.try_get("trashed_at")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            created_by: actor_of(row, "created_kind", "created_by", "created_obo")?,
            updated_by: actor_of(row, "updated_kind", "updated_by", "updated_obo")?,
            my_role: None,
            my_can_share: None,
            editors_can_share: row.try_get("editors_can_share")?,
            editors_can_share_effective: None,
        },
        trash_root: row.try_get::<Option<Uuid>, _>("trash_root")?.map(NodeId),
    })
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.code().as_deref() == Some("23505"))
}

fn is_fk_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.code().as_deref() == Some("23503"))
}

/// Maps a unique violation (a name race) to `name_taken`.
fn name_error(name: &str) -> impl FnOnce(sqlx::Error) -> Error + '_ {
    move |e| {
        if is_unique_violation(&e) {
            rules::name_taken(name)
        } else {
            Error::Database(e)
        }
    }
}

/// Loads a node of the tenant (`for update` with `lock`). `not_found` for another tenant's node.
pub(crate) async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: NodeId,
    lock: bool,
) -> Result<Row0> {
    let sql = format!(
        "select {COLS} from nodes n where n.tenant_id = $1 and n.id = $2{}",
        if lock { " for update" } else { "" }
    );
    let row = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(tenant.0)
        .bind(id.0)
        .fetch_optional(conn)
        .await?
        .ok_or_else(rules::node_not_found)?;
    node_of(&row)
}

/// Loads a live node of the tenant and locks it against changes (`for share`).
async fn load_live(conn: &mut PgConnection, tenant: TenantId, id: NodeId) -> Result<Row0> {
    let sql = format!(
        "select {COLS} from nodes n where n.tenant_id = $1 and n.id = $2 \
         and n.trashed_at is null for share"
    );
    let row = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(tenant.0)
        .bind(id.0)
        .fetch_optional(conn)
        .await?
        .ok_or_else(rules::node_not_found)?;
    node_of(&row)
}

/// Escapes `%`, `_` and `\` for `like`.
pub(crate) fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The name for a node in `parent` (see [`rules::pick_name`]), reading only the names that can
/// clash.
async fn pick_name(
    conn: &mut PgConnection,
    tenant: TenantId,
    parent: NodeId,
    except: Option<NodeId>,
    wanted: &str,
    kind: NodeKind,
    on_conflict: OnConflict,
) -> Result<String> {
    let stem = match wanted.rfind('.') {
        Some(i) if i > 0 && kind != NodeKind::Folder => &wanted[..i],
        _ => wanted,
    };
    let taken: Vec<String> = sqlx::query_scalar(
        "select lower(name) from nodes where tenant_id = $1 and parent_id = $2 \
         and trashed_at is null and id <> coalesce($3, '00000000-0000-0000-0000-000000000000') \
         and (lower(name) = lower($4) or lower(name) like lower($5) || ' (%' escape '\\')",
    )
    .bind(tenant.0)
    .bind(parent.0)
    .bind(except.map(|n| n.0))
    .bind(wanted)
    .bind(like_escape(stem))
    .fetch_all(conn)
    .await?;
    let taken: HashSet<String> = taken.into_iter().collect();
    rules::pick_name(wanted, kind, on_conflict, &taken)
}

/// Whether a live member link in `folder` points at `target`.
async fn has_member_link(
    conn: &mut PgConnection,
    tenant: TenantId,
    folder: NodeId,
    target: NodeId,
) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "select exists (select 1 from nodes where tenant_id = $1 and parent_id = $2 \
         and link_target = $3 and link_kind = 'member' and trashed_at is null)",
    )
    .bind(tenant.0)
    .bind(folder.0)
    .bind(target.0)
    .fetch_one(conn)
    .await?)
}

async fn exists(conn: &mut PgConnection, tenant: TenantId, id: NodeId) -> Result<bool> {
    Ok(
        sqlx::query_scalar("select exists (select 1 from nodes where tenant_id = $1 and id = $2)")
            .bind(tenant.0)
            .bind(id.0)
            .fetch_one(conn)
            .await?,
    )
}

/// The ancestors of a node with their names, root first, without the node.
async fn ancestors_with_names(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: NodeId,
) -> Result<Vec<(NodeId, String)>> {
    let rows: Vec<(Uuid, String, i32)> = sqlx::query_as(
        "with recursive up(id, parent_id, name, depth) as ( \
             select id, parent_id, name, 0 from nodes where tenant_id = $1 and id = $2 \
             union all \
             select n.id, n.parent_id, n.name, up.depth + 1 from nodes n \
             join up on n.id = up.parent_id where n.tenant_id = $1 and up.depth < 10000) \
         select id, name, depth from up order by depth desc",
    )
    .bind(tenant.0)
    .bind(id.0)
    .fetch_all(conn)
    .await?;
    if rows.is_empty() {
        return Err(rules::node_not_found());
    }
    Ok(rows
        .into_iter()
        .filter(|(_, _, d)| *d > 0)
        .map(|(i, n, _)| (NodeId(i), n))
        .collect())
}

/// The node and its descendants, parents before children. With `live_only`, trashed
/// descendants (and what is below them) are left out.
async fn subtree(
    conn: &mut PgConnection,
    tenant: TenantId,
    root: NodeId,
    live_only: bool,
) -> Result<Vec<Row0>> {
    let sql = format!(
        "with recursive sub(id, depth) as ( \
             select id, 0 from nodes where tenant_id = $1 and id = $2 \
             union all \
             select c.id, sub.depth + 1 from nodes c join sub on c.parent_id = sub.id \
             where c.tenant_id = $1 {} and sub.depth < 10000) \
         select {COLS} from nodes n join sub on sub.id = n.id where n.tenant_id = $1 \
         order by sub.depth, n.id",
        if live_only {
            "and c.trashed_at is null"
        } else {
            ""
        }
    );
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(tenant.0)
        .bind(root.0)
        .fetch_all(conn)
        .await?
        .iter()
        .map(node_of)
        .collect()
}

/// Copies the head version of `head` to a new version of `target` in the caller's transaction:
/// the same manifest, so the chunks are shared and no bytes are copied. The new version records
/// the caller as actor. Returns the new version.
///
/// TODO(integrator): this writes the `versions` table of the blobs and versions area. When that
/// area has a Postgres store, it may own this function (same signature) and add its version
/// policy; the node store only needs the new version ID in the same transaction.
pub async fn copy_head_version(
    conn: &mut PgConnection,
    ctx: &Context,
    head: VersionId,
    target: NodeId,
) -> Result<VersionId> {
    let id = VersionId::new();
    let n = sqlx::query(
        "insert into versions (id, tenant_id, node_id, manifest, encoding, size_bytes, \
             content_hash, content_type, actor_kind, actor, on_behalf_of) \
         select $3, tenant_id, $4, manifest, encoding, size_bytes, content_hash, content_type, \
             $5, $6, $7 \
         from versions where tenant_id = $1 and id = $2",
    )
    .bind(ctx.tenant.0)
    .bind(head.0)
    .bind(id.0)
    .bind(target.0)
    .bind(ctx.actor.actor.kind)
    .bind(ctx.actor.actor.id.0)
    .bind(ctx.actor.on_behalf_of.map(|u| u.0))
    .execute(conn)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(Error::Internal(format!(
            "nodes: the head version {} is missing",
            head.0
        )));
    }
    Ok(id)
}

fn from_micros(t: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(t) * 1000)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

/// The Postgres store.
pub struct PgNodeStore {
    pool: PgPool,
    authz: Arc<dyn Authz>,
}

impl std::fmt::Debug for PgNodeStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgNodeStore").finish_non_exhaustive()
    }
}

/// Pending changes of one write transaction.
struct Tx {
    tx: sqlx::Transaction<'static, sqlx::Postgres>,
    updates: Vec<RelationshipUpdate>,
}

impl PgNodeStore {
    pub fn new(pool: PgPool, authz: Arc<dyn Authz>) -> Self {
        Self { pool, authz }
    }

    async fn begin(&self, ctx: &Context) -> Result<Tx> {
        Ok(Tx {
            tx: begin_tenant(&self.pool, ctx.tenant).await?,
            updates: Vec::new(),
        })
    }

    /// Enqueues the relationship updates, commits, and applies the outbox after the commit.
    async fn commit(&self, ctx: &Context, mut t: Tx) -> Result<()> {
        let ob = if t.updates.is_empty() {
            None
        } else {
            Some(outbox::enqueue(&mut t.tx, ctx.tenant, &t.updates).await?)
        };
        t.tx.commit().await?;
        if let Some(id) = ob {
            outbox::apply_after_commit(&self.pool, &*self.authz, ctx.tenant, &[id]).await;
        }
        Ok(())
    }

    /// A read transaction with the row-level security setting of the tenant.
    async fn conn(&self, ctx: &Context) -> Result<sqlx::Transaction<'static, sqlx::Postgres>> {
        begin_tenant(&self.pool, ctx.tenant).await
    }

    async fn row(&self, ctx: &Context, id: NodeId) -> Result<Row0> {
        let mut c = self.conn(ctx).await?;
        let r = load(&mut c, ctx.tenant, id, false).await?;
        c.commit().await?;
        Ok(r)
    }

    async fn live(&self, ctx: &Context, id: NodeId) -> Result<Row0> {
        let r = self.row(ctx, id).await?;
        if r.node.trashed_at.is_some() {
            return Err(rules::node_not_found());
        }
        Ok(r)
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
        rules::decorate_one(&*self.authz, self, ctx, node).await
    }

    async fn windows(&self, ctx: &Context) -> Result<TrashWindows> {
        let mut c = self.conn(ctx).await?;
        let plan: Plan = sqlx::query_scalar::<_, Plan>("select plan from tenants where id = $1")
            .bind(ctx.tenant.0)
            .fetch_optional(&mut *c)
            .await?
            .ok_or_else(|| Error::not_found("tenant"))?;
        c.commit().await?;
        Ok(TrashWindows::of_plan(plan))
    }

    async fn is_admin(&self, ctx: &Context) -> Result<bool> {
        let Some(user) = ctx.actor.effective_user() else {
            return Ok(false);
        };
        let mut c = self.conn(ctx).await?;
        let role: Option<MemberRole> = sqlx::query_scalar::<_, MemberRole>(
            "select role from memberships where tenant_id = $1 and user_id = $2",
        )
        .bind(ctx.tenant.0)
        .bind(user.0)
        .fetch_optional(&mut *c)
        .await?;
        c.commit().await?;
        Ok(rules::is_admin(role))
    }

    async fn journal(
        t: &mut Tx,
        ctx: &Context,
        node: NodeId,
        revision: Option<i64>,
        event: EventType,
        data: serde_json::Value,
    ) -> Result<ChangeInfo> {
        let e = append(
            &mut t.tx,
            ctx,
            NewEntry {
                node_id: Some(node),
                revision,
                event,
                data,
            },
        )
        .await?;
        Ok(ChangeInfo {
            seq: Some(e.seq),
            actor: ctx.actor,
            at: e.at,
        })
    }

    async fn insert(t: &mut Tx, n: &Node) -> Result<()> {
        sqlx::query(
            "insert into nodes (id, tenant_id, parent_id, name, kind, type, link_kind, \
                 link_target, head_version, owner_id, size_bytes, revision, created_at, \
                 created_kind, created_by, created_obo, updated_at, updated_kind, updated_by, \
                 updated_obo) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, \
                 $13, $14, $15, $16)",
        )
        .bind(n.id.0)
        .bind(n.tenant_id.0)
        .bind(n.parent_id.map(|p| p.0))
        .bind(&n.name)
        .bind(n.kind)
        .bind(&n.node_type)
        .bind(n.link_kind)
        .bind(n.link_target.map(|t| t.0))
        .bind(n.head_version.map(|v| v.0))
        .bind(n.owner_id.0)
        .bind(n.size_bytes)
        .bind(n.revision)
        .bind(n.created_at)
        .bind(n.created_by.actor.kind)
        .bind(n.created_by.actor.id.0)
        .bind(n.created_by.on_behalf_of.map(|u| u.0))
        .execute(&mut *t.tx)
        .await
        .map_err(|e| {
            if is_fk_violation(&e) {
                rules::node_not_found()
            } else {
                name_error(&n.name)(e)
            }
        })?;
        Ok(())
    }

    /// The trashed root, with its permission checked (`manage`), for restore and purge.
    async fn trashed_root(&self, ctx: &Context, id: NodeId) -> Result<Row0> {
        let row = self.row(ctx, id).await?;
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

    /// Purges a trashed root and everything below it, in one transaction.
    async fn purge_root(&self, ctx: &Context, id: NodeId, expired: bool) -> Result<ChangeInfo> {
        let mut t = self.begin(ctx).await?;
        let root = load(&mut t.tx, ctx.tenant, id, true).await?;
        if root.trash_root != Some(id) {
            return Err(rules::conflict(
                "the node is not a trashed root",
                "read the trash again",
            ));
        }
        let rows = subtree(&mut t.tx, ctx.tenant, id, false).await?;
        let ids: Vec<Uuid> = rows.iter().map(|r| r.node.id.0).collect();
        let scoped: bool = sqlx::query_scalar(
            "select exists (select 1 from agents where tenant_id = $1 and scope_subtree = any($2))",
        )
        .bind(ctx.tenant.0)
        .bind(&ids)
        .fetch_one(&mut *t.tx)
        .await?;
        if scoped {
            return Err(rules::conflict(
                "an agent's scope is inside this node",
                "change the scope of the agent, or remove the agent, then purge",
            ));
        }
        for r in &rows {
            for rel in self.authz.read(&ObjectRef::node(r.node.id)).await? {
                t.updates.push(RelationshipUpdate::Delete(rel));
            }
        }
        for stmt in [
            "delete from node_tags where tenant_id = $1 and node_id = any($2)",
            "delete from share_index where tenant_id = $1 and node_id = any($2)",
            "delete from grants where tenant_id = $1 and node_id = any($2)",
            "delete from share_links where tenant_id = $1 and node_id = any($2)",
            "delete from uploads where tenant_id = $1 and (node_id = any($2) or parent_id = any($2))",
            "update nodes set head_version = null where tenant_id = $1 and id = any($2)",
            "delete from versions where tenant_id = $1 and node_id = any($2)",
            "delete from nodes where tenant_id = $1 and id = any($2)",
        ] {
            sqlx::query(stmt)
                .bind(ctx.tenant.0)
                .bind(&ids)
                .execute(&mut *t.tx)
                .await?;
        }
        for r in &rows {
            if let (Some(target), Some(folder)) = (rules::member_target(&r.node), r.node.parent_id)
                && !has_member_link(&mut t.tx, ctx.tenant, folder, target).await?
            {
                t.updates
                    .push(RelationshipUpdate::Delete(tuples::member_link(
                        target, folder,
                    )));
            }
        }
        let change = Self::journal(
            &mut t,
            ctx,
            id,
            None,
            EventType::NodePurged,
            rules::purged_data(&root.node.name, rows.len(), expired),
        )
        .await?;
        self.commit(ctx, t).await?;
        Ok(change)
    }
}

/// The SQL that bumps the revision and the `updated_*` columns (`$a`, `$b`, `$c` are the
/// actor kind, ID and on-behalf-of user).
fn touch_sql(a: usize, b: usize, c: usize) -> String {
    format!(
        "revision = revision + 1, updated_at = now(), updated_kind = ${a}, updated_by = ${b}, \
         updated_obo = ${c}"
    )
}

#[async_trait]
impl NodeStore for PgNodeStore {
    async fn root(&self, ctx: &Context) -> Result<Node> {
        let mut c = self.conn(ctx).await?;
        let sql =
            format!("select {COLS} from nodes n where n.tenant_id = $1 and n.parent_id is null");
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(ctx.tenant.0)
            .fetch_optional(&mut *c)
            .await?
            .ok_or_else(rules::node_not_found)?;
        let root = node_of(&row)?;
        c.commit().await?;
        match self.require(ctx, root.node.id, Role::View).await {
            Err(e) if rules::is_not_found(&e) => return Err(Error::permission_denied()),
            other => other?,
        }
        self.out(ctx, root.node).await
    }

    async fn create(&self, ctx: &Context, req: NewNode) -> Result<Written<Node>> {
        let name = validate_name(&req.name)?;
        let link = super::memory::check_new_kind(&req)?;
        let owner = rules::owner_of(ctx)?;
        self.require(ctx, req.parent_id, Role::Edit).await?;
        let parent = self.live(ctx, req.parent_id).await?;
        if parent.node.kind != NodeKind::Folder {
            return Err(rules::invalid("the parent is not a folder"));
        }
        if let Some(l) = &link {
            self.live(ctx, l.target).await?;
            let p = match l.kind {
                LinkKind::Shortcut => Permission::View,
                LinkKind::Member => Permission::Share,
            };
            self.require(ctx, l.target, p).await?;
        }
        let mut t = self.begin(ctx).await?;
        load_live(&mut t.tx, ctx.tenant, req.parent_id).await?;
        if let Some(l) = &link {
            load_live(&mut t.tx, ctx.tenant, l.target).await?;
        }
        let name = pick_name(
            &mut t.tx,
            ctx.tenant,
            req.parent_id,
            None,
            &name,
            req.kind,
            req.on_conflict,
        )
        .await?;
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
        Self::insert(&mut t, &node).await?;
        let change = Self::journal(
            &mut t,
            ctx,
            node.id,
            Some(node.revision),
            EventType::NodeCreated,
            rules::created_data(&node, None),
        )
        .await?;
        t.updates.extend(rules::created_tuples(&node));
        self.commit(ctx, t).await?;
        let node = self.row(ctx, node.id).await?.node;
        Ok(Written {
            value: self.out(ctx, node).await?,
            change,
        })
    }

    async fn get(&self, ctx: &Context, id: NodeId) -> Result<Node> {
        let row = self.row(ctx, id).await?;
        if row.node.trashed_at.is_some() {
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
        let mut c = self.conn(ctx).await?;
        let sql =
            format!("select {COLS} from nodes n where n.tenant_id = $1 and n.parent_id is null");
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(ctx.tenant.0)
            .fetch_optional(&mut *c)
            .await?
            .ok_or_else(rules::node_not_found)?;
        let mut node = node_of(&row)?;
        let sql = format!(
            "select {COLS} from nodes n where n.tenant_id = $1 and n.parent_id = $2 \
             and n.trashed_at is null and lower(n.name) = lower($3)"
        );
        for name in &names {
            let row = sqlx::query(sqlx::AssertSqlSafe(sql.clone()))
                .bind(ctx.tenant.0)
                .bind(node.node.id.0)
                .bind(name)
                .fetch_optional(&mut *c)
                .await?
                .ok_or_else(rules::node_not_found)?;
            node = node_of(&row)?;
        }
        c.commit().await?;
        self.require(ctx, node.node.id, Role::View).await?;
        self.out(ctx, node.node).await
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
        let p = self.live(ctx, parent).await?;
        if p.node.kind != NodeKind::Folder {
            return Err(rules::invalid("the node is not a folder"));
        }
        let limit = page.limit() as usize;
        let (cond, ord) = match order {
            ChildOrder::Name => (
                "((n.kind <> 'folder')::int, lower(n.name) collate \"C\", n.id) \
                 > ($3::int, $4::text collate \"C\", $5::uuid)",
                "(n.kind <> 'folder'), lower(n.name) collate \"C\", n.id",
            ),
            ChildOrder::UpdatedAt => (
                "(n.updated_at < $6 or (n.updated_at = $6 and n.id > $5))",
                "n.updated_at desc, n.id",
            ),
            ChildOrder::Size => (
                "(n.size_bytes < $7 or (n.size_bytes = $7 and n.id > $5))",
                "n.size_bytes desc, n.id",
            ),
        };
        let sql = format!(
            "select {COLS}, lower(n.name) as name_key from nodes n \
             where n.tenant_id = $1 and n.parent_id = $2 and n.trashed_at is null \
             and ($5::uuid is null or {cond}) \
             and ($3::int is null or $4::text is null or $6::timestamptz is null \
                  or $7::bigint is null or true) \
             order by {ord} limit $8"
        );
        let a = after.as_ref();
        let mut c = self.conn(ctx).await?;
        let raw = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(ctx.tenant.0)
            .bind(parent.0)
            .bind(a.map(|k| k.k))
            .bind(a.map(|k| k.n.clone()))
            .bind(a.map(|k| k.i))
            .bind(a.map(|k| from_micros(k.t)))
            .bind(a.map(|k| k.s))
            .bind((limit + 1) as i64)
            .fetch_all(&mut *c)
            .await?;
        c.commit().await?;
        let rows = raw
            .iter()
            .map(|r| {
                let n = node_of(r)?.node;
                let mut k = ChildKey::of(&n);
                k.n = r.try_get("name_key")?;
                Ok((k, n))
            })
            .collect::<Result<Vec<(ChildKey, Node)>>>()?;
        let more = rows.len() > limit;
        let rows: Vec<(ChildKey, Node)> = rows.into_iter().take(limit).collect();
        let next_cursor = if more {
            rows.last().map(|(k, _)| k.cursor())
        } else {
            None
        };
        let items = rules::visible(
            &*self.authz,
            self,
            ctx,
            rows.into_iter().map(|(_, n)| n).collect(),
            Role::View,
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
        self.live(ctx, id).await?;
        self.require(ctx, id, Role::Edit).await?;
        let mut t = self.begin(ctx).await?;
        let row = load(&mut t.tx, ctx.tenant, id, true).await?;
        if row.node.trashed_at.is_some() {
            return Err(rules::node_not_found());
        }
        let Some(parent) = row.node.parent_id else {
            return Err(rules::invalid("the root folder cannot be renamed"));
        };
        rules::check_revision(&row.node, if_revision)?;
        if row.node.name == name {
            drop(t);
            return Ok(Written {
                value: self.out(ctx, row.node).await?,
                change: super::memory::no_change(ctx),
            });
        }
        let name = pick_name(
            &mut t.tx,
            ctx.tenant,
            parent,
            Some(id),
            &name,
            row.node.kind,
            on_conflict,
        )
        .await?;
        let sql = format!(
            "update nodes set name = $3, {} where tenant_id = $1 and id = $2 returning revision",
            touch_sql(4, 5, 6)
        );
        let revision: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .bind(ctx.tenant.0)
            .bind(id.0)
            .bind(&name)
            .bind(ctx.actor.actor.kind)
            .bind(ctx.actor.actor.id.0)
            .bind(ctx.actor.on_behalf_of.map(|u| u.0))
            .fetch_one(&mut *t.tx)
            .await
            .map_err(name_error(&name))?;
        let change = Self::journal(
            &mut t,
            ctx,
            id,
            Some(revision),
            EventType::NodeRenamed,
            rules::renamed_data(Some(parent), &row.node.name, &name),
        )
        .await?;
        self.commit(ctx, t).await?;
        let node = self.row(ctx, id).await?.node;
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
        let row = self.live(ctx, id).await?;
        self.require(ctx, id, Role::Manage).await?;
        let Some(old_parent) = row.node.parent_id else {
            return Err(rules::invalid("the root folder cannot be moved"));
        };
        self.require(ctx, req.parent_id, Role::Edit).await?;
        let dest = self.live(ctx, req.parent_id).await?;
        if dest.node.kind != NodeKind::Folder {
            return Err(rules::invalid("the new parent is not a folder"));
        }
        if req.parent_id == id || self.ancestors(ctx, req.parent_id).await?.contains(&id) {
            return Err(rules::invalid(
                "a folder cannot move into itself or its own subtree",
            ));
        }
        let mut target = None;
        if let Some(t) = rules::member_target(&row.node)
            && old_parent != req.parent_id
            && self.live(ctx, t).await.is_ok()
        {
            self.require(ctx, t, Permission::Share).await?;
            target = Some(t);
        }
        let mut t = self.begin(ctx).await?;
        let row = load(&mut t.tx, ctx.tenant, id, true).await?;
        if row.node.trashed_at.is_some() {
            return Err(rules::node_not_found());
        }
        load_live(&mut t.tx, ctx.tenant, req.parent_id).await?;
        rules::check_revision(&row.node, req.if_revision)?;
        let wanted = wanted.unwrap_or_else(|| row.node.name.clone());
        if old_parent == req.parent_id && row.node.name == wanted {
            drop(t);
            return Ok(Written {
                value: self.out(ctx, row.node).await?,
                change: super::memory::no_change(ctx),
            });
        }
        let name = pick_name(
            &mut t.tx,
            ctx.tenant,
            req.parent_id,
            Some(id),
            &wanted,
            row.node.kind,
            req.on_conflict,
        )
        .await?;
        let sql = format!(
            "update nodes set parent_id = $3, name = $4, {} where tenant_id = $1 and id = $2 \
             returning revision",
            touch_sql(5, 6, 7)
        );
        let revision: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .bind(ctx.tenant.0)
            .bind(id.0)
            .bind(req.parent_id.0)
            .bind(&name)
            .bind(ctx.actor.actor.kind)
            .bind(ctx.actor.actor.id.0)
            .bind(ctx.actor.on_behalf_of.map(|u| u.0))
            .fetch_one(&mut *t.tx)
            .await
            .map_err(name_error(&name))?;
        if old_parent != req.parent_id {
            t.updates
                .push(RelationshipUpdate::Delete(tuples::node_parent(
                    id, old_parent,
                )));
            t.updates
                .push(RelationshipUpdate::Touch(tuples::node_parent(
                    id,
                    req.parent_id,
                )));
            if let Some(tg) = rules::member_target(&row.node) {
                if !has_member_link(&mut t.tx, ctx.tenant, old_parent, tg).await? {
                    t.updates
                        .push(RelationshipUpdate::Delete(tuples::member_link(
                            tg, old_parent,
                        )));
                }
                if target.is_some() {
                    t.updates
                        .push(RelationshipUpdate::Touch(tuples::member_link(
                            tg,
                            req.parent_id,
                        )));
                }
            }
        }
        let change = Self::journal(
            &mut t,
            ctx,
            id,
            Some(revision),
            EventType::NodeMoved,
            rules::moved_data(old_parent, req.parent_id, &row.node.name, &name),
        )
        .await?;
        self.commit(ctx, t).await?;
        let node = self.row(ctx, id).await?.node;
        Ok(Written {
            value: self.out(ctx, node).await?,
            change,
        })
    }

    async fn copy(&self, ctx: &Context, id: NodeId, req: CopyRequest) -> Result<Written<Node>> {
        let wanted = req.name.as_deref().map(validate_name).transpose()?;
        let owner = rules::owner_of(ctx)?;
        self.live(ctx, id).await?;
        self.require(ctx, id, Role::View).await?;
        self.require(ctx, req.parent_id, Role::Edit).await?;
        let dest = self.live(ctx, req.parent_id).await?;
        if dest.node.kind != NodeKind::Folder {
            return Err(rules::invalid("the new parent is not a folder"));
        }
        if req.parent_id == id || self.ancestors(ctx, req.parent_id).await?.contains(&id) {
            return Err(rules::invalid(
                "a folder cannot be copied into itself or its own subtree",
            ));
        }
        let mut sources = Vec::new();
        {
            let mut tx = self.conn(ctx).await?;
            let all = subtree(&mut tx, ctx.tenant, id, true).await?;
            for r in all {
                // Broken links (the target was purged) are not copied.
                if let Some(t) = r.node.link_target
                    && !exists(&mut tx, ctx.tenant, t).await?
                {
                    continue;
                }
                sources.push(r.node);
            }
            tx.commit().await?;
        }
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
        let targets: Vec<NodeId> = sources.iter().filter_map(rules::member_target).collect();
        let views = rules::caller_view(&*self.authz, self, ctx, &targets).await?;
        let shares: HashSet<NodeId> = targets
            .iter()
            .zip(&views)
            .filter(|(_, v)| v.can_share)
            .map(|(t, _)| *t)
            .collect();

        let mut t = self.begin(ctx).await?;
        load_live(&mut t.tx, ctx.tenant, req.parent_id).await?;
        let root_name = pick_name(
            &mut t.tx,
            ctx.tenant,
            req.parent_id,
            None,
            &wanted.unwrap_or_else(|| sources[0].name.clone()),
            sources[0].kind,
            req.on_conflict,
        )
        .await?;
        let now = crate::now();
        let mut map: HashMap<NodeId, NodeId> = HashMap::new();
        let mut made: Vec<(Node, NodeId)> = Vec::with_capacity(sources.len());
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
                (Some(LinkKind::Member), Some(tg)) if !shares.contains(&tg) => {
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
                head_version: src.head_version.map(|_| VersionId::new()),
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
            // The node first (its head version is checked at the commit), then the version.
            let mut ins = node.clone();
            ins.head_version = None;
            Self::insert(&mut t, &ins).await?;
            if let (Some(head), Some(_)) = (src.head_version, node.head_version) {
                let v = copy_head_version(&mut t.tx, ctx, head, new_id).await?;
                sqlx::query("update nodes set head_version = $3 where tenant_id = $1 and id = $2")
                    .bind(ctx.tenant.0)
                    .bind(new_id.0)
                    .bind(v.0)
                    .execute(&mut *t.tx)
                    .await?;
            }
            made.push((node, src.id));
        }
        for (node, src) in made.iter().skip(1) {
            Self::journal(
                &mut t,
                ctx,
                node.id,
                Some(node.revision),
                EventType::NodeCreated,
                rules::created_data(node, Some(*src)),
            )
            .await?;
        }
        let root_id = made[0].0.id;
        let change = Self::journal(
            &mut t,
            ctx,
            root_id,
            Some(1),
            EventType::NodeCopied,
            rules::copied_data(id, req.parent_id, &root_name, made.len()),
        )
        .await?;
        for (node, _) in &made {
            t.updates.extend(rules::created_tuples(node));
        }
        self.commit(ctx, t).await?;
        let node = self.row(ctx, root_id).await?.node;
        Ok(Written {
            value: self.out(ctx, node).await?,
            change,
        })
    }

    async fn trash(
        &self,
        ctx: &Context,
        id: NodeId,
        if_revision: Option<i64>,
    ) -> Result<Written<Node>> {
        let row = self.row(ctx, id).await?;
        self.require(ctx, id, Role::Manage).await?;
        if row.node.parent_id.is_none() {
            return Err(Error::Api(
                ApiError::new(
                    ErrorCode::PermissionDenied,
                    "the root folder cannot be trashed",
                )
                .with_hint("trash the nodes inside it"),
            ));
        }
        if row.node.trashed_at.is_some() {
            return Ok(Written {
                value: self.out(ctx, row.node).await?,
                change: super::memory::no_change(ctx),
            });
        }
        let windows = self.windows(ctx).await?;
        let mut t = self.begin(ctx).await?;
        let row = load(&mut t.tx, ctx.tenant, id, true).await?;
        if row.node.trashed_at.is_some() {
            drop(t);
            return Ok(Written {
                value: self.out(ctx, row.node).await?,
                change: super::memory::no_change(ctx),
            });
        }
        rules::check_revision(&row.node, if_revision)?;
        let now = crate::now();
        let purge_after = windows.purge_after(now);
        let rows: Vec<(Uuid, Option<Uuid>, NodeKind, Option<LinkKind>, Option<Uuid>)> =
            sqlx::query_as(
                "with recursive sub(id, depth) as ( \
                     select id, 0 from nodes where tenant_id = $1 and id = $2 \
                     union all \
                     select c.id, sub.depth + 1 from nodes c join sub on c.parent_id = sub.id \
                     where c.tenant_id = $1 and c.trashed_at is null and sub.depth < 10000) \
                 update nodes set trashed_at = $3, trash_root = $2, purge_after = $4, \
                     trashed_kind = $5, trashed_by = $6, trashed_obo = $7, \
                     revision = revision + 1, updated_at = $3, updated_kind = $5, \
                     updated_by = $6, updated_obo = $7 \
                 where tenant_id = $1 and id in (select id from sub) and trashed_at is null \
                 returning id, parent_id, kind, link_kind, link_target",
            )
            .bind(ctx.tenant.0)
            .bind(id.0)
            .bind(now)
            .bind(purge_after)
            .bind(ctx.actor.actor.kind)
            .bind(ctx.actor.actor.id.0)
            .bind(ctx.actor.on_behalf_of.map(|u| u.0))
            .fetch_all(&mut *t.tx)
            .await?;
        for (_, parent, kind, lk, target) in &rows {
            if let (NodeKind::Link, Some(LinkKind::Member), Some(tg), Some(p)) =
                (kind, lk, target, parent)
                && !has_member_link(&mut t.tx, ctx.tenant, NodeId(*p), NodeId(*tg)).await?
            {
                t.updates
                    .push(RelationshipUpdate::Delete(tuples::member_link(
                        NodeId(*tg),
                        NodeId(*p),
                    )));
            }
        }
        let node = load(&mut t.tx, ctx.tenant, id, false).await?.node;
        let change = Self::journal(
            &mut t,
            ctx,
            id,
            Some(node.revision),
            EventType::NodeTrashed,
            rules::trashed_data(node.parent_id, &node.name, rows.len(), purge_after),
        )
        .await?;
        self.commit(ctx, t).await?;
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
        let windows = self.windows(ctx).await?;
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
                let d = self.live(ctx, p).await?;
                if d.node.kind != NodeKind::Folder {
                    return Err(rules::invalid("the new parent is not a folder"));
                }
                p
            }
            None => {
                if self.live(ctx, old_parent).await.is_err() {
                    return Err(rules::conflict(
                        "the old parent folder is gone or in the trash",
                        "restore it into another folder: send parent_id",
                    ));
                }
                old_parent
            }
        };
        if dest != old_parent
            && let Some(tg) = rules::member_target(&row.node)
            && self.live(ctx, tg).await.is_ok()
        {
            self.require(ctx, tg, Permission::Share).await?;
        }
        let mut t = self.begin(ctx).await?;
        let row = load(&mut t.tx, ctx.tenant, id, true).await?;
        if row.trash_root != Some(id) {
            return Err(rules::conflict(
                "the node is not in the trash",
                "read it again",
            ));
        }
        rules::check_revision(&row.node, req.if_revision)?;
        load_live(&mut t.tx, ctx.tenant, dest).await?;
        let name = pick_name(
            &mut t.tx,
            ctx.tenant,
            dest,
            Some(id),
            &row.node.name,
            row.node.kind,
            req.on_conflict,
        )
        .await?;
        let clear = "trashed_at = null, trash_root = null, purge_after = null, \
                     trashed_kind = null, trashed_by = null, trashed_obo = null";
        let sql = format!(
            "update nodes set parent_id = $3, name = $4, {clear}, {} \
             where tenant_id = $1 and id = $2",
            touch_sql(5, 6, 7)
        );
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(ctx.tenant.0)
            .bind(id.0)
            .bind(dest.0)
            .bind(&name)
            .bind(ctx.actor.actor.kind)
            .bind(ctx.actor.actor.id.0)
            .bind(ctx.actor.on_behalf_of.map(|u| u.0))
            .execute(&mut *t.tx)
            .await
            .map_err(name_error(&name))?;
        let sql = format!(
            "update nodes set {clear}, {} where tenant_id = $1 and trash_root = $2",
            touch_sql(3, 4, 5)
        );
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(ctx.tenant.0)
            .bind(id.0)
            .bind(ctx.actor.actor.kind)
            .bind(ctx.actor.actor.id.0)
            .bind(ctx.actor.on_behalf_of.map(|u| u.0))
            .execute(&mut *t.tx)
            .await?;
        if dest != old_parent {
            t.updates
                .push(RelationshipUpdate::Delete(tuples::node_parent(
                    id, old_parent,
                )));
            t.updates
                .push(RelationshipUpdate::Touch(tuples::node_parent(id, dest)));
            if let Some(tg) = rules::member_target(&row.node)
                && !has_member_link(&mut t.tx, ctx.tenant, old_parent, tg).await?
            {
                t.updates
                    .push(RelationshipUpdate::Delete(tuples::member_link(
                        tg, old_parent,
                    )));
            }
        }
        // The member links that come back give access again (their targets still exist).
        let links: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "with recursive sub(id, depth) as ( \
                 select id, 0 from nodes where tenant_id = $1 and id = $2 \
                 union all \
                 select c.id, sub.depth + 1 from nodes c join sub on c.parent_id = sub.id \
                 where c.tenant_id = $1 and c.trashed_at is null and sub.depth < 10000) \
             select n.link_target, n.parent_id from nodes n join sub on sub.id = n.id \
             where n.tenant_id = $1 and n.link_kind = 'member' \
             and exists (select 1 from nodes t where t.tenant_id = $1 and t.id = n.link_target)",
        )
        .bind(ctx.tenant.0)
        .bind(id.0)
        .fetch_all(&mut *t.tx)
        .await?;
        for (tg, p) in links {
            t.updates
                .push(RelationshipUpdate::Touch(tuples::member_link(
                    NodeId(tg),
                    NodeId(p),
                )));
        }
        let node = load(&mut t.tx, ctx.tenant, id, false).await?.node;
        let change = Self::journal(
            &mut t,
            ctx,
            id,
            Some(node.revision),
            EventType::NodeRestored,
            rules::restored_data(dest, old_parent, &name),
        )
        .await?;
        self.commit(ctx, t).await?;
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
                ApiError::new(
                    ErrorCode::PermissionDenied,
                    "only tenant admins can list the whole trash (all=true)",
                )
                .with_hint("leave out all=true"),
            ));
        }
        let windows = self.windows(ctx).await?;
        let oldest = windows.oldest(all, crate::now());
        let limit = page.limit() as usize;
        let sql = format!(
            "select {COLS} from nodes n where n.tenant_id = $1 \
             and n.trash_root = n.id and n.trashed_at >= $2 \
             and ($3::timestamptz is null or n.trashed_at < $3 \
                  or (n.trashed_at = $3 and n.id > $4)) \
             order by n.trashed_at desc, n.id limit $5"
        );
        let mut c = self.conn(ctx).await?;
        let raw = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(ctx.tenant.0)
            .bind(oldest)
            .bind(after.map(|(t, _)| from_micros(t)))
            .bind(after.map(|(_, i)| i))
            .bind((limit + 1) as i64)
            .fetch_all(&mut *c)
            .await?;
        c.commit().await?;
        let rows = raw
            .iter()
            .map(|r| Ok(node_of(r)?.node))
            .collect::<Result<Vec<Node>>>()?;
        let more = rows.len() > limit;
        let rows: Vec<Node> = rows.into_iter().take(limit).collect();
        let next_cursor = if more {
            rows.last()
                .map(|n| rules::trash_cursor(n.trashed_at.unwrap_or(n.updated_at), n.id))
        } else {
            None
        };
        let items = rules::visible(&*self.authz, self, ctx, rows, Role::Manage).await?;
        Ok(Page { items, next_cursor })
    }

    async fn purge(&self, ctx: &Context, id: NodeId) -> Result<Written<()>> {
        self.trashed_root(ctx, id).await?;
        let windows = self.windows(ctx).await?;
        windows.check_purge(self.is_admin(ctx).await?)?;
        let change = self.purge_root(ctx, id, false).await?;
        Ok(Written { value: (), change })
    }

    async fn empty_trash(&self, ctx: &Context) -> Result<(u32, Option<ChangeInfo>)> {
        let windows = self.windows(ctx).await?;
        let admin = self.is_admin(ctx).await?;
        if windows.check_purge(admin).is_err() {
            return Ok((0, None));
        }
        let oldest = windows.oldest(admin, crate::now());
        let mut c = self.conn(ctx).await?;
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "select id from nodes where tenant_id = $1 and trash_root = id \
             and trashed_at >= $2 order by trashed_at, id",
        )
        .bind(ctx.tenant.0)
        .bind(oldest)
        .fetch_all(&mut *c)
        .await?;
        c.commit().await?;
        let roots: Vec<NodeId> = ids.into_iter().map(NodeId).collect();
        let views = rules::caller_view(&*self.authz, self, ctx, &roots).await?;
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
                // Gone meanwhile, or held by an agent's scope: it stays.
                Err(Error::Api(e))
                    if matches!(e.code, ErrorCode::NotFound | ErrorCode::Conflict) => {}
                Err(e) => return Err(e),
            }
        }
        Ok((count, last))
    }

    async fn path_of(&self, ctx: &Context, id: NodeId) -> Result<String> {
        let mut c = self.conn(ctx).await?;
        let mut up = ancestors_with_names(&mut c, ctx.tenant, id).await?;
        let me = load(&mut c, ctx.tenant, id, false).await?;
        c.commit().await?;
        up.push((id, me.node.name));
        let chain: Vec<NodeId> = up.iter().map(|(n, _)| *n).collect();
        let names: HashMap<NodeId, String> = up.into_iter().collect();
        super::memory::path_from(&*self.authz, self, ctx, &chain, &names).await
    }

    async fn ancestors(&self, ctx: &Context, id: NodeId) -> Result<Vec<NodeId>> {
        let mut c = self.conn(ctx).await?;
        let up = ancestors_with_names(&mut c, ctx.tenant, id).await?;
        c.commit().await?;
        Ok(up.into_iter().map(|(n, _)| n).collect())
    }
}
