//! The Postgres content store: uploads, downloads and versions on the tables `uploads`,
//! `versions` and `chunks` (migration 0001), with the chunks in a [`BlobStore`].
//!
//! [`PgContentStore`] implements [`Transfers`] and [`VersionStore`]. Every write runs in one
//! transaction with the row-level security setting of the tenant
//! ([`journal::postgres::begin_tenant`]): the version row, the file's `nodes` row (head, size,
//! revision, `updated_*`) and the journal entry ([`journal::postgres::append`]) commit together.
//! A `new_file` upload also inserts the file's `nodes` row and its relationships
//! (`node:<file>#parent@node:<folder>`, through [`authz::outbox`]) in the same transaction.
//!
//! Chunks are stored in the blob store before the transaction; the `chunks` rows say which
//! chunks the tenant has (`missing`) and drive garbage collection ([`super::gc`]).
//!
//! Permission checks use [`authz::require`] before the transaction. Node responses come from
//! the node store ([`NodeStore::get`]) after the commit, so they carry the fields of the nodes
//! area (`my_role`, ...).

use super::gc::GcReport;
use super::logic::{self, check_link_may_write, version_not_found};
use super::*;
use crate::actor::PrincipalRef;
use crate::authz::{self, Authz, RelationshipUpdate, Role};
use crate::ids::PrincipalId;
use crate::journal::{self, ChangeInfo, EventType, NewEntry, Written};
use crate::nodes::{NodeKind, NodeStore};
use crate::paging::{Page, PageRequest};
use crate::tenants::Plan;
use crate::tenants::limits::{Limits, tenant_limits};
use crate::versions::policy::{self, HeadInfo, VersionMeta};
use crate::versions::{VersionFilter, VersionPolicy, VersionStore};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Row};
use std::collections::HashSet;
use std::sync::Arc;
use time::Duration;
use uuid::Uuid;

type ContentWritten = Written<(Node, Version)>;

/// The Postgres content store.
pub struct PgContentStore {
    pool: PgPool,
    authz: Arc<dyn Authz>,
    nodes: Arc<dyn NodeStore>,
    blobs: Arc<dyn BlobStore>,
    signer: ContentSigner,
}

/// The columns of a version row that [`version_of`] reads.
const VERSION_COLUMNS: &str = "id, tenant_id, node_id, manifest, encoding, size_bytes, \
     content_hash, content_type, restored_from, label, actor_kind, actor, on_behalf_of, \
     created_at, updated_at";

/// The columns of an upload row that [`upload_of`] reads.
const UPLOAD_COLUMNS: &str = "id, tenant_id, node_id, base_version, parent_id, name, \
     on_conflict, content_type, size_bytes, content_hash, manifest, chunking, status::text \
     as status, actor_kind, actor, on_behalf_of, expires_at";

/// What the store reads of a `nodes` row.
#[derive(Clone, Debug)]
struct NodeRow {
    id: NodeId,
    name: String,
    kind: NodeKind,
    node_type: Option<String>,
    head_version: Option<VersionId>,
    size_bytes: i64,
    trashed: bool,
    owner_id: Uuid,
}

fn hash_of(bytes: Vec<u8>) -> Result<Hash> {
    let a: [u8; 32] = bytes
        .try_into()
        .map_err(|_| Error::Internal("a stored hash is not 32 bytes".into()))?;
    Ok(Hash(a))
}

fn actor_of(row: &PgRow) -> Result<Actor> {
    Ok(Actor {
        actor: PrincipalRef {
            kind: row.try_get("actor_kind")?,
            id: PrincipalId(row.try_get::<Uuid, _>("actor")?),
        },
        on_behalf_of: row
            .try_get::<Option<Uuid>, _>("on_behalf_of")?
            .map(crate::ids::UserId),
    })
}

/// A version row: the version, its manifest and the time of its last save.
struct VersionRow {
    version: Version,
    manifest: Manifest,
    updated_at: OffsetDateTime,
}

fn version_of(row: &PgRow) -> Result<VersionRow> {
    let encoding: String = row.try_get("encoding")?;
    let format = match encoding.as_str() {
        "chunks_v1" => ManifestFormat::ChunksV1,
        other => {
            return Err(Error::Internal(format!(
                "unknown manifest encoding {other}"
            )));
        }
    };
    let manifest = Manifest::decode(format, &row.try_get::<Vec<u8>, _>("manifest")?)?;
    Ok(VersionRow {
        version: Version {
            id: VersionId(row.try_get("id")?),
            tenant_id: TenantId(row.try_get("tenant_id")?),
            node_id: NodeId(row.try_get("node_id")?),
            size_bytes: row.try_get("size_bytes")?,
            content_hash: hash_of(row.try_get("content_hash")?)?,
            content_type: row.try_get("content_type")?,
            restored_from: row
                .try_get::<Option<Uuid>, _>("restored_from")?
                .map(VersionId),
            label: row.try_get("label")?,
            actor: actor_of(row)?,
            created_at: row.try_get("created_at")?,
        },
        manifest,
        updated_at: row.try_get("updated_at")?,
    })
}

fn upload_of(row: &PgRow) -> Result<Upload> {
    let manifest = Manifest::decode(
        ManifestFormat::ChunksV1,
        &row.try_get::<Vec<u8>, _>("manifest")?,
    )?;
    let chunking: String = row.try_get("chunking")?;
    let target = match row.try_get::<Option<Uuid>, _>("node_id")? {
        Some(n) => UploadTarget::NewVersion {
            node_id: NodeId(n),
            base_version: row
                .try_get::<Option<Uuid>, _>("base_version")?
                .map(VersionId),
        },
        None => UploadTarget::NewFile {
            parent_id: NodeId(
                row.try_get::<Option<Uuid>, _>("parent_id")?
                    .ok_or_else(|| Error::Internal("upload without target".into()))?,
            ),
            name: row
                .try_get::<Option<String>, _>("name")?
                .unwrap_or_default(),
            on_conflict: match row.try_get::<String, _>("on_conflict")?.as_str() {
                "rename" => crate::nodes::OnConflict::Rename,
                _ => crate::nodes::OnConflict::Fail,
            },
        },
    };
    let status = match row.try_get::<String, _>("status")?.as_str() {
        "open" => UploadStatus::Open,
        "completed" => UploadStatus::Completed,
        "aborted" => UploadStatus::Aborted,
        _ => UploadStatus::Expired,
    };
    Ok(Upload {
        id: UploadId(row.try_get("id")?),
        tenant_id: TenantId(row.try_get("tenant_id")?),
        target,
        content_type: row.try_get("content_type")?,
        size: row.try_get::<i64, _>("size_bytes")? as u64,
        content_hash: hash_of(row.try_get("content_hash")?)?,
        chunks: manifest.chunks,
        missing: Vec::new(),
        status,
        chunking: chunking_by_id(&chunking)
            .ok_or_else(|| Error::Internal(format!("unknown chunking {chunking}")))?,
        expires_at: row.try_get("expires_at")?,
        created_by: actor_of(row)?,
    })
}

async fn node_row(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: NodeId,
    lock: bool,
) -> Result<NodeRow> {
    let sql = format!(
        "select id, name, kind, type, head_version, size_bytes, trashed_at is not null as trashed, \
         owner_id from nodes where tenant_id = $1 and id = $2{}",
        if lock { " for update" } else { "" }
    );
    let row = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(tenant.0)
        .bind(id.0)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| Error::not_found("node"))?;
    Ok(NodeRow {
        id: NodeId(row.try_get("id")?),
        name: row.try_get("name")?,
        kind: row.try_get("kind")?,
        node_type: row.try_get("type")?,
        head_version: row
            .try_get::<Option<Uuid>, _>("head_version")?
            .map(VersionId),
        size_bytes: row.try_get("size_bytes")?,
        trashed: row.try_get("trashed")?,
        owner_id: row.try_get("owner_id")?,
    })
}

/// The live file `id` of the tenant (for writes; reads accept a trashed file).
fn check_file(n: &NodeRow, write: bool) -> Result<()> {
    if n.kind != NodeKind::File {
        return Err(logic::not_a_file());
    }
    if write && n.trashed {
        return Err(Error::not_found("node"));
    }
    Ok(())
}

async fn version_row(
    conn: &mut PgConnection,
    tenant: TenantId,
    node: NodeId,
    id: VersionId,
    lock: bool,
) -> Result<VersionRow> {
    let sql = format!(
        "select {VERSION_COLUMNS} from versions where tenant_id = $1 and id = $2 and node_id = $3{}",
        if lock { " for update" } else { "" }
    );
    let row = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(tenant.0)
        .bind(id.0)
        .bind(node.0)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(version_not_found)?;
    version_of(&row)
}

/// The version `id` of the node, or its head.
async fn version_or_head(
    conn: &mut PgConnection,
    tenant: TenantId,
    n: &NodeRow,
    id: Option<VersionId>,
) -> Result<VersionRow> {
    match id.or(n.head_version) {
        Some(v) => version_row(conn, tenant, n.id, v, false).await,
        None => Err(logic::api(
            ErrorCode::NotFound,
            "the file has no content yet",
            "upload content first (PUT /v1/nodes/{node_id}/content)",
        )),
    }
}

/// The limits of the tenant's plan, and the bytes it uses now (the sum of its file sizes).
async fn limits_and_usage(conn: &mut PgConnection, tenant: TenantId) -> Result<(Limits, i64)> {
    let row = sqlx::query(
        "select t.plan, \
           (select count(*) from memberships m where m.tenant_id = t.id and m.role <> 'guest') \
             as members, \
           (select coalesce(sum(n.size_bytes), 0)::bigint from nodes n where n.tenant_id = t.id) \
             as used \
         from tenants t where t.id = $1",
    )
    .bind(tenant.0)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| Error::not_found("tenant"))?;
    let plan: Plan = row.try_get("plan")?;
    let members: i64 = row.try_get("members")?;
    Ok((
        tenant_limits(plan, members.max(1) as u32),
        row.try_get("used")?,
    ))
}

async fn record_chunks(
    conn: &mut PgConnection,
    tenant: TenantId,
    chunks: &[ChunkRef],
) -> Result<()> {
    for c in chunks {
        sqlx::query(
            "insert into chunks (tenant_id, hash, size_bytes, last_marked_at) values ($1, $2, $3, now()) \
             on conflict (tenant_id, hash) do update set last_marked_at = now()",
        )
        .bind(tenant.0)
        .bind(c.hash.0.to_vec())
        .bind(c.size as i32)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

fn hashes_param(hashes: &[Hash]) -> Vec<Vec<u8>> {
    hashes.iter().map(|h| h.0.to_vec()).collect()
}

/// The hashes of `hashes` that the tenant has no `chunks` row for.
async fn missing_rows(
    conn: &mut PgConnection,
    tenant: TenantId,
    hashes: &[Hash],
) -> Result<Vec<Hash>> {
    let have: HashSet<Vec<u8>> = sqlx::query_scalar::<_, Vec<u8>>(
        "select hash from chunks where tenant_id = $1 and hash = any($2)",
    )
    .bind(tenant.0)
    .bind(hashes_param(hashes))
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();
    Ok(hashes
        .iter()
        .filter(|h| !have.contains(h.0.as_slice()))
        .copied()
        .collect())
}

fn change_of(e: &crate::journal::JournalEntry) -> ChangeInfo {
    ChangeInfo {
        seq: Some(e.seq),
        actor: e.actor,
        at: e.at,
    }
}

impl PgContentStore {
    pub fn new(
        pool: PgPool,
        authz: Arc<dyn Authz>,
        nodes: Arc<dyn NodeStore>,
        blobs: Arc<dyn BlobStore>,
        signer: ContentSigner,
    ) -> Self {
        Self {
            pool,
            authz,
            nodes,
            blobs,
            signer,
        }
    }

    async fn require(&self, ctx: &Context, node: NodeId, role: Role) -> Result<()> {
        authz::require(&*self.authz, &*self.nodes, ctx, node, role).await
    }

    /// Makes the new head version (or merges the save into the head), sets the file's head,
    /// writes the entry, and applies thinning and retention to the file. In `conn`'s
    /// transaction. Returns the version and the change.
    async fn commit_version(
        conn: &mut PgConnection,
        ctx: &Context,
        n: &NodeRow,
        manifest: &Manifest,
        content_type: Option<String>,
        restored_from: Option<VersionId>,
        policy: &VersionPolicy,
    ) -> Result<(Version, ChangeInfo)> {
        let now = crate::now();
        let head = match n.head_version {
            Some(h) => Some(version_row(conn, ctx.tenant, n.id, h, true).await?),
            None => None,
        };
        let merge = restored_from.is_none()
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
                    policy,
                )
            });
        let (version, previous) = match &head {
            Some(h) if merge => {
                let row = sqlx::query(sqlx::AssertSqlSafe(format!(
                    "update versions set manifest = $3, size_bytes = $4, content_hash = $5, \
                     content_type = $6, updated_at = $7 where tenant_id = $1 and id = $2 \
                     returning {VERSION_COLUMNS}"
                )))
                .bind(ctx.tenant.0)
                .bind(h.version.id.0)
                .bind(manifest.encode())
                .bind(manifest.size as i64)
                .bind(manifest.content_hash.0.to_vec())
                .bind(&content_type)
                .bind(now)
                .fetch_one(&mut *conn)
                .await?;
                let previous: Option<Uuid> = sqlx::query_scalar(
                    "select id from versions where tenant_id = $1 and node_id = $2 and id <> $3 \
                     order by created_at desc, id desc limit 1",
                )
                .bind(ctx.tenant.0)
                .bind(n.id.0)
                .bind(h.version.id.0)
                .fetch_optional(&mut *conn)
                .await?;
                (version_of(&row)?.version, previous.map(VersionId))
            }
            _ => {
                let id = VersionId::new();
                let row = sqlx::query(sqlx::AssertSqlSafe(format!(
                    "insert into versions (id, tenant_id, node_id, manifest, encoding, size_bytes, \
                     content_hash, content_type, restored_from, actor_kind, actor, on_behalf_of, \
                     created_at, updated_at) \
                     values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $13) \
                     returning {VERSION_COLUMNS}"
                )))
                .bind(id.0)
                .bind(ctx.tenant.0)
                .bind(n.id.0)
                .bind(manifest.encode())
                .bind(manifest.format.as_str())
                .bind(manifest.size as i64)
                .bind(manifest.content_hash.0.to_vec())
                .bind(&content_type)
                .bind(restored_from.map(|r| r.0))
                .bind(ctx.actor.actor.kind)
                .bind(ctx.actor.actor.id.0)
                .bind(ctx.actor.on_behalf_of.map(|u| u.0))
                .bind(now)
                .fetch_one(&mut *conn)
                .await?;
                (
                    version_of(&row)?.version,
                    head.as_ref().map(|h| h.version.id),
                )
            }
        };
        let revision: i64 = sqlx::query_scalar(
            "update nodes set head_version = $3, size_bytes = $4, revision = revision + 1, \
             updated_at = $5, updated_kind = $6, updated_by = $7, updated_obo = $8 \
             where tenant_id = $1 and id = $2 returning revision",
        )
        .bind(ctx.tenant.0)
        .bind(n.id.0)
        .bind(version.id.0)
        .bind(version.size_bytes)
        .bind(now)
        .bind(ctx.actor.actor.kind)
        .bind(ctx.actor.actor.id.0)
        .bind(ctx.actor.on_behalf_of.map(|u| u.0))
        .fetch_one(&mut *conn)
        .await?;
        let (event, data) = match restored_from {
            Some(_) => (
                EventType::VersionRestored,
                logic::version_restored_data(&version, previous),
            ),
            None => (
                EventType::VersionCreated,
                logic::version_created_data(&version, previous, merge),
            ),
        };
        let e = journal::postgres::append(
            conn,
            ctx,
            NewEntry {
                node_id: Some(n.id),
                revision: Some(revision),
                event,
                data,
            },
        )
        .await?;
        Self::prune(conn, ctx.tenant, n.id, Some(version.id), policy, now).await?;
        Ok((version, change_of(&e)))
    }

    /// Thinning and retention of one file. Returns the removed versions.
    async fn prune(
        conn: &mut PgConnection,
        tenant: TenantId,
        node: NodeId,
        head: Option<VersionId>,
        policy: &VersionPolicy,
        now: OffsetDateTime,
    ) -> Result<Vec<VersionId>> {
        let head = match head {
            Some(h) => Some(h),
            None => sqlx::query_scalar::<_, Option<Uuid>>(
                "select head_version from nodes where tenant_id = $1 and id = $2",
            )
            .bind(tenant.0)
            .bind(node.0)
            .fetch_optional(&mut *conn)
            .await?
            .flatten()
            .map(VersionId),
        };
        let rows = sqlx::query(
            "select id, created_at, label is not null as labeled from versions \
             where tenant_id = $1 and node_id = $2",
        )
        .bind(tenant.0)
        .bind(node.0)
        .fetch_all(&mut *conn)
        .await?;
        let mut metas = Vec::with_capacity(rows.len());
        for r in &rows {
            let id = VersionId(r.try_get("id")?);
            metas.push(VersionMeta {
                id,
                created_at: r.try_get("created_at")?,
                labeled: r.try_get("labeled")?,
                is_head: Some(id) == head,
            });
        }
        let gone = policy::prunable(&metas, policy, now);
        if gone.is_empty() {
            return Ok(gone);
        }
        let ids: Vec<Uuid> = gone.iter().map(|v| v.0).collect();
        sqlx::query(
            "update versions set restored_from = null where tenant_id = $1 and restored_from = any($2)",
        )
        .bind(tenant.0)
        .bind(&ids)
        .execute(&mut *conn)
        .await?;
        sqlx::query("delete from versions where tenant_id = $1 and id = any($2)")
            .bind(tenant.0)
            .bind(&ids)
            .execute(&mut *conn)
            .await?;
        Ok(gone)
    }

    /// The policy of the tenant's plan.
    async fn policy(
        conn: &mut PgConnection,
        tenant: TenantId,
    ) -> Result<(VersionPolicy, Limits, i64)> {
        let (limits, used) = limits_and_usage(conn, tenant).await?;
        Ok((VersionPolicy::from_limits(&limits), limits, used))
    }

    /// Inserts the `nodes` row of a new file (with ` (n)` for `rename`), its relationships
    /// through the outbox and its `node.created` entry. Returns the row and the outbox row.
    async fn insert_file(
        conn: &mut PgConnection,
        ctx: &Context,
        parent: &NodeRow,
        name: &str,
        on_conflict: crate::nodes::OnConflict,
        content_type: Option<&str>,
    ) -> Result<(NodeRow, i64)> {
        let owner = ctx
            .actor
            .effective_user()
            .map(|u| u.0)
            .unwrap_or(parent.owner_id);
        let id = NodeId::new();
        let mut candidate = name.to_string();
        let mut n = 2;
        loop {
            sqlx::query("savepoint ruum_insert_file")
                .execute(&mut *conn)
                .await?;
            let r = sqlx::query(
                "insert into nodes (id, tenant_id, parent_id, name, kind, type, owner_id, \
                 created_kind, created_by, created_obo, updated_kind, updated_by, updated_obo) \
                 values ($1, $2, $3, $4, 'file', $5, $6, $7, $8, $9, $7, $8, $9)",
            )
            .bind(id.0)
            .bind(ctx.tenant.0)
            .bind(parent.id.0)
            .bind(&candidate)
            .bind(content_type)
            .bind(owner)
            .bind(ctx.actor.actor.kind)
            .bind(ctx.actor.actor.id.0)
            .bind(ctx.actor.on_behalf_of.map(|u| u.0))
            .execute(&mut *conn)
            .await;
            match r {
                Ok(_) => {
                    sqlx::query("release savepoint ruum_insert_file")
                        .execute(&mut *conn)
                        .await?;
                    break;
                }
                Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                    sqlx::query("rollback to savepoint ruum_insert_file")
                        .execute(&mut *conn)
                        .await?;
                    if on_conflict == crate::nodes::OnConflict::Fail || n > 1000 {
                        return Err(logic::api(
                            ErrorCode::NameTaken,
                            format!("the name `{name}` is taken in this folder"),
                            "pick another name, or set on_conflict to rename",
                        ));
                    }
                    candidate = logic::numbered_name(name, n);
                    n += 1;
                }
                Err(sqlx::Error::Database(e)) if e.is_check_violation() => {
                    sqlx::query("rollback to savepoint ruum_insert_file")
                        .execute(&mut *conn)
                        .await?;
                    return Err(logic::validation(
                        "the name is not valid (it must be in Unicode NFC)",
                        "normalise the name to NFC",
                    ));
                }
                Err(e) => return Err(e.into()),
            }
        }
        let outbox = authz::outbox::enqueue(
            conn,
            ctx.tenant,
            &[RelationshipUpdate::Touch(authz::tuples::node_parent(
                id, parent.id,
            ))],
        )
        .await?;
        journal::postgres::append(
            conn,
            ctx,
            NewEntry {
                node_id: Some(id),
                revision: Some(1),
                event: EventType::NodeCreated,
                data: logic::node_created_data(parent.id, &candidate, content_type),
            },
        )
        .await?;
        Ok((
            NodeRow {
                id,
                name: candidate,
                kind: NodeKind::File,
                node_type: content_type.map(str::to_string),
                head_version: None,
                size_bytes: 0,
                trashed: false,
                owner_id: owner,
            },
            outbox,
        ))
    }

    /// The upload `id` of the caller. An open upload past its expiry becomes `expired`.
    async fn own_upload(
        &self,
        conn: &mut PgConnection,
        ctx: &Context,
        id: UploadId,
    ) -> Result<Upload> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {UPLOAD_COLUMNS} from uploads where tenant_id = $1 and id = $2 \
             and actor_kind = $3 and actor = $4 and on_behalf_of is not distinct from $5"
        )))
        .bind(ctx.tenant.0)
        .bind(id.0)
        .bind(ctx.actor.actor.kind)
        .bind(ctx.actor.actor.id.0)
        .bind(ctx.actor.on_behalf_of.map(|u| u.0))
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(logic::upload_not_found)?;
        let mut u = upload_of(&row)?;
        if u.status == UploadStatus::Open && u.expires_at <= crate::now() {
            sqlx::query("update uploads set status = 'expired' where tenant_id = $1 and id = $2")
                .bind(ctx.tenant.0)
                .bind(id.0)
                .execute(&mut *conn)
                .await?;
            u.status = UploadStatus::Expired;
        }
        Ok(u)
    }

    async fn node_response(&self, ctx: &Context, id: NodeId) -> Result<Node> {
        self.nodes.get(ctx, id).await
    }

    /// Expires old uploads, applies retention to every file of the tenant, and collects
    /// garbage. For a maintenance job (`run_maintenance`).
    pub async fn maintain(&self, tenant: TenantId, grace: Duration) -> Result<GcReport> {
        let mut tx = journal::postgres::begin_tenant(&self.pool, tenant).await?;
        sqlx::query(
            "update uploads set status = 'expired' where tenant_id = $1 and status = 'open' \
             and expires_at <= now()",
        )
        .bind(tenant.0)
        .execute(&mut *tx)
        .await?;
        let (policy, _, _) = Self::policy(&mut tx, tenant).await?;
        let files: Vec<Uuid> =
            sqlx::query_scalar("select distinct node_id from versions where tenant_id = $1")
                .bind(tenant.0)
                .fetch_all(&mut *tx)
                .await?;
        let now = crate::now();
        for f in files {
            Self::prune(&mut tx, tenant, NodeId(f), None, &policy, now).await?;
        }
        tx.commit().await?;
        self.collect_garbage(tenant, grace).await
    }

    /// Garbage collection of the tenant's chunks (mark and sweep with a grace period; the rule
    /// is in [`super::gc`]).
    pub async fn collect_garbage(&self, tenant: TenantId, grace: Duration) -> Result<GcReport> {
        let cutoff = crate::now() - grace;
        let mut tx = journal::postgres::begin_tenant(&self.pool, tenant).await?;
        let manifests: Vec<Vec<u8>> = sqlx::query_scalar(
            "select manifest from versions where tenant_id = $1 \
             union all select manifest from uploads where tenant_id = $1 and status = 'open' \
             and expires_at > now()",
        )
        .bind(tenant.0)
        .fetch_all(&mut *tx)
        .await?;
        let mut live: HashSet<Hash> = HashSet::new();
        for m in manifests {
            let m = Manifest::decode(ManifestFormat::ChunksV1, &m)?;
            live.extend(m.chunks.iter().map(|c| c.hash));
        }
        let candidates: Vec<Vec<u8>> = sqlx::query_scalar(
            "select hash from chunks where tenant_id = $1 and created_at < $2 \
             and coalesce(last_marked_at, created_at) < $2",
        )
        .bind(tenant.0)
        .bind(cutoff)
        .fetch_all(&mut *tx)
        .await?;
        let n_live: i64 = sqlx::query_scalar(
            "select count(*) from chunks where tenant_id = $1 and hash = any($2)",
        )
        .bind(tenant.0)
        .bind(hashes_param(&live.iter().copied().collect::<Vec<_>>()))
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        let mut deleted = 0;
        for h in candidates {
            let h = hash_of(h)?;
            if live.contains(&h) {
                continue;
            }
            self.blobs.delete_chunk(tenant, h).await?;
            let mut tx = journal::postgres::begin_tenant(&self.pool, tenant).await?;
            sqlx::query(
                "delete from chunks where tenant_id = $1 and hash = $2 \
                 and coalesce(last_marked_at, created_at) < $3",
            )
            .bind(tenant.0)
            .bind(h.0.to_vec())
            .bind(cutoff)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            deleted += 1;
        }
        Ok(GcReport {
            live: n_live as usize,
            deleted,
        })
    }

    /// Runs [`PgContentStore::maintain`] for every tenant, then again every `every`.
    pub async fn run_maintenance(self: Arc<Self>, every: std::time::Duration, grace: Duration) {
        loop {
            match self.tenants().await {
                Ok(tenants) => {
                    for t in tenants {
                        if let Err(e) = self.maintain(t, grace).await {
                            tracing::warn!(tenant = %t, error = %e, "content maintenance failed");
                        }
                    }
                }
                Err(e) => tracing::warn!(error = %e, "content maintenance: no tenant list"),
            }
            tokio::time::sleep(every).await;
        }
    }

    async fn tenants(&self) -> Result<Vec<TenantId>> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("select set_config('ruum.system', 'on', true)")
            .execute(&mut *tx)
            .await?;
        let ids: Vec<Uuid> = sqlx::query_scalar("select id from tenants")
            .fetch_all(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(ids.into_iter().map(TenantId).collect())
    }

    fn content(
        &self,
        tenant: TenantId,
        name: String,
        v: VersionRow,
        range: Option<ByteRange>,
    ) -> Content {
        Content {
            node_id: v.version.node_id,
            name,
            version_id: v.version.id,
            total_size: v.manifest.size,
            range,
            content_type: v
                .version
                .content_type
                .clone()
                .unwrap_or_else(|| "application/octet-stream".into()),
            content_hash: v.manifest.content_hash,
            body: logic::stream_chunks(self.blobs.clone(), tenant, v.manifest.chunks, range),
        }
    }
}

/// Gives the file `to`, a new copy of the file `from`, one version with the content of `from`'s
/// head (the chunks are shared, nothing is copied in object storage), and writes
/// `version.created`. Runs in the caller's transaction (the nodes area calls it from
/// `copyNode`, after it inserted the row of `to`). `None` when `from` has no content.
pub async fn copy_head(
    conn: &mut PgConnection,
    ctx: &Context,
    from: NodeId,
    to: NodeId,
) -> Result<Option<Version>> {
    let src = node_row(conn, ctx.tenant, from, false).await?;
    let Some(head) = src.head_version else {
        return Ok(None);
    };
    let v = version_row(conn, ctx.tenant, from, head, false).await?;
    let dst = node_row(conn, ctx.tenant, to, true).await?;
    let policy = VersionPolicy::default();
    let (version, _) = PgContentStore::commit_version(
        conn,
        ctx,
        &dst,
        &v.manifest,
        v.version.content_type,
        None,
        &policy,
    )
    .await?;
    Ok(Some(version))
}

#[async_trait]
impl Transfers for PgContentStore {
    async fn create_upload(&self, ctx: &Context, req: NewUpload) -> Result<Upload> {
        check_link_may_write(ctx)?;
        let params = logic::check_new_upload(&req)?;
        let target_node = match &req.target {
            UploadTarget::NewVersion { node_id, .. } => *node_id,
            UploadTarget::NewFile {
                parent_id, name, ..
            } => {
                logic::check_file_name(name)?;
                *parent_id
            }
        };
        self.require(ctx, target_node, Role::Edit).await?;
        let mut tx = journal::postgres::begin_tenant(&self.pool, ctx.tenant).await?;
        let n = node_row(&mut tx, ctx.tenant, target_node, false).await?;
        let old_size = match &req.target {
            UploadTarget::NewVersion { .. } => {
                check_file(&n, true)?;
                n.size_bytes
            }
            UploadTarget::NewFile { .. } => {
                if n.kind != NodeKind::Folder || n.trashed {
                    return Err(logic::not_a_folder());
                }
                0
            }
        };
        let (limits, used) = limits_and_usage(&mut tx, ctx.tenant).await?;
        logic::check_limits(&limits, used, old_size, req.size)?;
        let hashes = logic::distinct(&req.chunks);
        sqlx::query(
            "update chunks set last_marked_at = now() where tenant_id = $1 and hash = any($2)",
        )
        .bind(ctx.tenant.0)
        .bind(hashes_param(&hashes))
        .execute(&mut *tx)
        .await?;
        let manifest = Manifest {
            format: ManifestFormat::ChunksV1,
            chunking: params.id.to_string(),
            chunks: req.chunks.clone(),
            size: req.size,
            content_hash: req.content_hash,
        };
        let id = UploadId::new();
        let expires_at = crate::now() + Duration::hours(UPLOAD_HOURS);
        let (node_id, base, parent, name, on_conflict) = match &req.target {
            UploadTarget::NewVersion {
                node_id,
                base_version,
            } => (
                Some(node_id.0),
                base_version.map(|v| v.0),
                None,
                None,
                "fail",
            ),
            UploadTarget::NewFile {
                parent_id,
                name,
                on_conflict,
            } => (
                None,
                None,
                Some(parent_id.0),
                Some(name.clone()),
                match on_conflict {
                    crate::nodes::OnConflict::Fail => "fail",
                    crate::nodes::OnConflict::Rename => "rename",
                },
            ),
        };
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "insert into uploads (id, tenant_id, node_id, base_version, parent_id, name, on_conflict, \
             content_type, size_bytes, content_hash, manifest, chunking, actor_kind, actor, \
             on_behalf_of, expires_at) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16) \
             returning {UPLOAD_COLUMNS}"
        )))
        .bind(id.0)
        .bind(ctx.tenant.0)
        .bind(node_id)
        .bind(base)
        .bind(parent)
        .bind(name)
        .bind(on_conflict)
        .bind(&req.content_type)
        .bind(req.size as i64)
        .bind(req.content_hash.0.to_vec())
        .bind(manifest.encode())
        .bind(params.id.as_ref())
        .bind(ctx.actor.actor.kind)
        .bind(ctx.actor.actor.id.0)
        .bind(ctx.actor.on_behalf_of.map(|u| u.0))
        .bind(expires_at)
        .fetch_one(&mut *tx)
        .await?;
        let mut up = upload_of(&row)?;
        up.missing = missing_rows(&mut tx, ctx.tenant, &hashes).await?;
        tx.commit().await?;
        Ok(up)
    }

    async fn get_upload(&self, ctx: &Context, id: UploadId) -> Result<Upload> {
        check_link_may_write(ctx)?;
        let mut tx = journal::postgres::begin_tenant(&self.pool, ctx.tenant).await?;
        let mut up = self.own_upload(&mut tx, ctx, id).await?;
        up.missing = missing_rows(&mut tx, ctx.tenant, &logic::distinct(&up.chunks)).await?;
        tx.commit().await?;
        Ok(up)
    }

    async fn put_chunk(&self, ctx: &Context, id: UploadId, hash: Hash, data: Bytes) -> Result<()> {
        check_link_may_write(ctx)?;
        let mut tx = journal::postgres::begin_tenant(&self.pool, ctx.tenant).await?;
        let up = self.own_upload(&mut tx, ctx, id).await?;
        tx.commit().await?;
        if up.status != UploadStatus::Open {
            return Err(logic::upload_not_open(up.status));
        }
        logic::check_chunk(&up.chunks, hash, &data)?;
        let size = data.len() as u32;
        self.blobs.put_chunk(ctx.tenant, hash, data).await?;
        let mut tx = journal::postgres::begin_tenant(&self.pool, ctx.tenant).await?;
        record_chunks(&mut tx, ctx.tenant, &[ChunkRef { hash, size }]).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn complete_upload(&self, ctx: &Context, id: UploadId) -> Result<ContentWritten> {
        check_link_may_write(ctx)?;
        let mut tx = journal::postgres::begin_tenant(&self.pool, ctx.tenant).await?;
        let up = self.own_upload(&mut tx, ctx, id).await?;
        tx.commit().await?;
        if up.status != UploadStatus::Open {
            return Err(logic::upload_not_open(up.status));
        }
        let manifest = Manifest {
            format: ManifestFormat::ChunksV1,
            chunking: up.chunking.id.to_string(),
            chunks: up.chunks.clone(),
            size: up.size,
            content_hash: up.content_hash,
        };
        if let Err((e, gone)) = logic::verify_content(&*self.blobs, ctx.tenant, &manifest).await {
            if !gone.is_empty() {
                let mut tx = journal::postgres::begin_tenant(&self.pool, ctx.tenant).await?;
                sqlx::query("delete from chunks where tenant_id = $1 and hash = any($2)")
                    .bind(ctx.tenant.0)
                    .bind(hashes_param(&gone))
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
            }
            return Err(e);
        }
        let target_node = match &up.target {
            UploadTarget::NewVersion { node_id, .. } => *node_id,
            UploadTarget::NewFile { parent_id, .. } => *parent_id,
        };
        self.require(ctx, target_node, Role::Edit).await?;
        let mut tx = journal::postgres::begin_tenant(&self.pool, ctx.tenant).await?;
        // Lock the upload row, so two completions of one upload never both make a version.
        let status: String = sqlx::query_scalar(
            "select status::text from uploads where tenant_id = $1 and id = $2 for update",
        )
        .bind(ctx.tenant.0)
        .bind(id.0)
        .fetch_one(&mut *tx)
        .await?;
        if status != "open" {
            return Err(logic::upload_not_open(UploadStatus::Completed));
        }
        let (policy, _, _) = Self::policy(&mut tx, ctx.tenant).await?;
        let mut outbox = None;
        let n = match &up.target {
            UploadTarget::NewVersion {
                node_id,
                base_version,
            } => {
                let n = node_row(&mut tx, ctx.tenant, *node_id, true).await?;
                check_file(&n, true)?;
                if base_version.is_some() && *base_version != n.head_version {
                    return Err(logic::version_mismatch());
                }
                n
            }
            UploadTarget::NewFile {
                parent_id,
                name,
                on_conflict,
            } => {
                let p = node_row(&mut tx, ctx.tenant, *parent_id, false).await?;
                if p.kind != NodeKind::Folder || p.trashed {
                    return Err(logic::not_a_folder());
                }
                let (n, o) = Self::insert_file(
                    &mut tx,
                    ctx,
                    &p,
                    name,
                    *on_conflict,
                    up.content_type.as_deref(),
                )
                .await?;
                outbox = Some(o);
                n
            }
        };
        let (version, change) = Self::commit_version(
            &mut tx,
            ctx,
            &n,
            &manifest,
            up.content_type.clone(),
            None,
            &policy,
        )
        .await?;
        sqlx::query(
            "update uploads set status = 'completed', result_version = $3, completed_at = now() \
             where tenant_id = $1 and id = $2",
        )
        .bind(ctx.tenant.0)
        .bind(id.0)
        .bind(version.id.0)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        if let Some(o) = outbox {
            authz::outbox::apply_after_commit(&self.pool, &*self.authz, ctx.tenant, &[o]).await;
        }
        let node = self.node_response(ctx, n.id).await?;
        Ok(Written {
            value: (node, version),
            change,
        })
    }

    async fn abort_upload(&self, ctx: &Context, id: UploadId) -> Result<()> {
        check_link_may_write(ctx)?;
        let mut tx = journal::postgres::begin_tenant(&self.pool, ctx.tenant).await?;
        let up = self.own_upload(&mut tx, ctx, id).await?;
        if up.status == UploadStatus::Open {
            sqlx::query("update uploads set status = 'aborted' where tenant_id = $1 and id = $2")
                .bind(ctx.tenant.0)
                .bind(id.0)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn open_content(
        &self,
        ctx: &Context,
        node: NodeId,
        version: Option<VersionId>,
        range: Option<ByteRange>,
    ) -> Result<Content> {
        self.require(ctx, node, Role::View).await?;
        let mut tx = journal::postgres::begin_tenant(&self.pool, ctx.tenant).await?;
        let n = node_row(&mut tx, ctx.tenant, node, false).await?;
        check_file(&n, false)?;
        let v = version_or_head(&mut tx, ctx.tenant, &n, version).await?;
        tx.commit().await?;
        let range = logic::resolve_range(range, v.manifest.size)?;
        Ok(self.content(ctx.tenant, n.name, v, range))
    }

    async fn put_content(
        &self,
        ctx: &Context,
        node: NodeId,
        data: Bytes,
        content_type: Option<String>,
        if_version: Option<VersionId>,
    ) -> Result<ContentWritten> {
        check_link_may_write(ctx)?;
        self.require(ctx, node, Role::Edit).await?;
        if data.len() > MAX_PUT_CONTENT_BYTES {
            return Err(logic::payload_too_large(MAX_PUT_CONTENT_BYTES as i64));
        }
        // Check the precondition and the limits before the chunks are stored.
        let mut tx = journal::postgres::begin_tenant(&self.pool, ctx.tenant).await?;
        let n = node_row(&mut tx, ctx.tenant, node, false).await?;
        check_file(&n, true)?;
        if if_version.is_some() && if_version != n.head_version {
            return Err(logic::precondition_failed());
        }
        let (_, limits, used) = Self::policy(&mut tx, ctx.tenant).await?;
        logic::check_limits(&limits, used, n.size_bytes, data.len() as u64)?;
        let old_type = match n.head_version {
            Some(h) => {
                version_row(&mut tx, ctx.tenant, node, h, false)
                    .await?
                    .version
                    .content_type
            }
            None => None,
        };
        tx.commit().await?;
        let content_type = content_type.or(old_type).or(n.node_type.clone());
        let (manifest, stored) =
            logic::store_data(&*self.blobs, ctx.tenant, &data, content_type.as_deref()).await?;
        let mut tx = journal::postgres::begin_tenant(&self.pool, ctx.tenant).await?;
        record_chunks(&mut tx, ctx.tenant, &stored).await?;
        let (policy, _, _) = Self::policy(&mut tx, ctx.tenant).await?;
        let n = node_row(&mut tx, ctx.tenant, node, true).await?;
        check_file(&n, true)?;
        if if_version.is_some() && if_version != n.head_version {
            return Err(logic::precondition_failed());
        }
        let (version, change) =
            Self::commit_version(&mut tx, ctx, &n, &manifest, content_type, None, &policy).await?;
        tx.commit().await?;
        let node = self.node_response(ctx, node).await?;
        Ok(Written {
            value: (node, version),
            change,
        })
    }

    async fn content_grant(
        &self,
        ctx: &Context,
        node: NodeId,
        version: Option<VersionId>,
        disposition: Disposition,
    ) -> Result<(String, ContentClaims)> {
        self.require(ctx, node, Role::View).await?;
        let mut tx = journal::postgres::begin_tenant(&self.pool, ctx.tenant).await?;
        let n = node_row(&mut tx, ctx.tenant, node, false).await?;
        check_file(&n, false)?;
        let v = version_or_head(&mut tx, ctx.tenant, &n, version).await?;
        tx.commit().await?;
        let claims = ContentSigner::claims(
            ctx.tenant,
            node,
            v.version.id,
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
        let mut tx = journal::postgres::begin_tenant(&self.pool, claims.tenant).await?;
        let v = version_row(&mut tx, claims.tenant, claims.node, claims.version, false).await?;
        tx.commit().await?;
        let range = logic::resolve_range(range, v.manifest.size)?;
        let content = self.content(claims.tenant, claims.name.clone(), v, range);
        Ok((content, claims))
    }
}

#[async_trait]
impl VersionStore for PgContentStore {
    async fn list_filtered(
        &self,
        ctx: &Context,
        node: NodeId,
        filter: VersionFilter,
        page: PageRequest,
    ) -> Result<Page<Version>> {
        self.require(ctx, node, Role::View).await?;
        let after = page
            .cursor
            .as_ref()
            .map(logic::decode_version_cursor)
            .transpose()?;
        let limit = page.limit() as i64;
        let mut tx = journal::postgres::begin_tenant(&self.pool, ctx.tenant).await?;
        let n = node_row(&mut tx, ctx.tenant, node, false).await?;
        check_file(&n, false)?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {VERSION_COLUMNS} from versions where tenant_id = $1 and node_id = $2 \
             and ($3::principal_kind is null or actor_kind = $3) \
             and ($4::boolean is null or (label is not null) = $4) \
             and ($5::timestamptz is null or (created_at, id) < ($5, $6)) \
             order by created_at desc, id desc limit $7"
        )))
        .bind(ctx.tenant.0)
        .bind(node.0)
        .bind(filter.actor_kind)
        .bind(filter.labeled)
        .bind(after.map(|(at, _)| at))
        .bind(after.map(|(_, id)| id.0))
        .bind(limit + 1)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let mut items = rows
            .iter()
            .map(|r| version_of(r).map(|v| v.version))
            .collect::<Result<Vec<_>>>()?;
        let next_cursor = if items.len() as i64 > limit {
            items.truncate(limit as usize);
            items.last().map(logic::version_cursor)
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }

    async fn get(&self, ctx: &Context, node: NodeId, version: VersionId) -> Result<Version> {
        self.require(ctx, node, Role::View).await?;
        let mut tx = journal::postgres::begin_tenant(&self.pool, ctx.tenant).await?;
        let n = node_row(&mut tx, ctx.tenant, node, false).await?;
        check_file(&n, false)?;
        let v = version_row(&mut tx, ctx.tenant, node, version, false).await?;
        tx.commit().await?;
        Ok(v.version)
    }

    async fn restore(
        &self,
        ctx: &Context,
        node: NodeId,
        version: VersionId,
    ) -> Result<Written<Version>> {
        check_link_may_write(ctx)?;
        self.require(ctx, node, Role::Edit).await?;
        let mut tx = journal::postgres::begin_tenant(&self.pool, ctx.tenant).await?;
        let (policy, _, _) = Self::policy(&mut tx, ctx.tenant).await?;
        let n = node_row(&mut tx, ctx.tenant, node, true).await?;
        check_file(&n, true)?;
        let src = version_row(&mut tx, ctx.tenant, node, version, false).await?;
        let (v, change) = Self::commit_version(
            &mut tx,
            ctx,
            &n,
            &src.manifest,
            src.version.content_type.clone(),
            Some(version),
            &policy,
        )
        .await?;
        tx.commit().await?;
        Ok(Written { value: v, change })
    }

    async fn set_label(
        &self,
        ctx: &Context,
        node: NodeId,
        version: VersionId,
        label: Option<String>,
    ) -> Result<Written<Version>> {
        check_link_may_write(ctx)?;
        self.require(ctx, node, Role::Edit).await?;
        let label = logic::check_label(label)?;
        let mut tx = journal::postgres::begin_tenant(&self.pool, ctx.tenant).await?;
        let n = node_row(&mut tx, ctx.tenant, node, false).await?;
        check_file(&n, true)?;
        let old = version_row(&mut tx, ctx.tenant, node, version, true).await?;
        if old.version.label == label {
            tx.commit().await?;
            return Ok(Written {
                value: old.version,
                change: ChangeInfo {
                    seq: None,
                    actor: ctx.actor,
                    at: crate::now(),
                },
            });
        }
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "update versions set label = $3 where tenant_id = $1 and id = $2 returning {VERSION_COLUMNS}"
        )))
        .bind(ctx.tenant.0)
        .bind(version.0)
        .bind(&label)
        .fetch_one(&mut *tx)
        .await?;
        let v = version_of(&row)?.version;
        let revision: i64 =
            sqlx::query_scalar("select revision from nodes where tenant_id = $1 and id = $2")
                .bind(ctx.tenant.0)
                .bind(node.0)
                .fetch_one(&mut *tx)
                .await?;
        let e = journal::postgres::append(
            &mut tx,
            ctx,
            NewEntry {
                node_id: Some(node),
                revision: Some(revision),
                event: EventType::VersionLabeled,
                data: logic::version_labeled_data(
                    version,
                    old.version.label.as_deref(),
                    label.as_deref(),
                ),
            },
        )
        .await?;
        tx.commit().await?;
        Ok(Written {
            value: v,
            change: change_of(&e),
        })
    }
}
