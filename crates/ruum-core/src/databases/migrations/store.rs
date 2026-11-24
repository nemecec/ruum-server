//! The `database_migrations` and `database_restore_points` methods of the two database stores
//! (card D-2): the migration lock, the restore point row, the migration row with its journal
//! entry and the take of its approval, the list, and the lens-rebuild marks. The trait methods
//! are declared in [`super::super::DatabaseStore`]; each store delegates here.
//!
//! Postgres: every statement filters by `tenant_id` and runs in a transaction of the tenant
//! (row-level security), except the scan of the lens sweeper, which runs as the system and
//! reads IDs only. The migration lock is a transaction-level advisory lock on a key of the
//! tenant and the database, held on a connection of the store's lock pool (never its main
//! pool) and taken with `pg_try_advisory_xact_lock` in a loop up to a deadline. It does not
//! lock the `databases` row, because card K-1's catalog sync locks that row (`for update`)
//! inside the engine's migration call, which a row lock held around the call would wait for
//! forever.

use super::{
    LENS_PENDING, LockGuard, MigrationLock, MigrationRecord, MigrationRow, MigrationStatus,
    decode_cursor, lock_busy, micros, page_of, without_lens_pending,
};
use crate::actor::{Actor, Context, PrincipalKind, PrincipalRef};
use crate::databases::MemoryDatabaseStore;
use crate::databases::access::approvals::{ApprovalStore, PgApprovalStore};
use crate::databases::access::grants::RestorePointRow;
use crate::error::{Error, ErrorCode, Result};
use crate::ids::{NodeId, TenantId, UserId};
use crate::journal::{JournalEntry, NewEntry};
use crate::paging::{Page, PageRequest};
use async_trait::async_trait;
use ruum_dbengine::RestorePointMark;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use time::OffsetDateTime;
use uuid::Uuid;

/// `409 conflict`: an applied migration of this name exists (the partial unique index
/// `database_migrations_applied_key`).
fn applied_already() -> Error {
    Error::api(
        ErrorCode::Conflict,
        "a migration of this name is applied already",
    )
}

// ---------------------------------------------------------------------------- memory

/// The migration locks of the memory store: one async mutex per database.
#[derive(Default)]
pub(crate) struct MemoryLocks(Mutex<HashMap<(TenantId, NodeId), DatabaseLock>>);

/// The lock of one database in [`MemoryLocks`].
type DatabaseLock = Arc<tokio::sync::Mutex<()>>;

struct MemoryLock(#[allow(dead_code)] tokio::sync::OwnedMutexGuard<()>);

#[async_trait]
impl LockGuard for MemoryLock {
    async fn release(self: Box<Self>) {}
}

impl MemoryLocks {
    pub(crate) async fn lock(
        &self,
        tenant: TenantId,
        node: NodeId,
        wait: Duration,
    ) -> Result<MigrationLock> {
        let m = {
            let mut map = self.0.lock().unwrap_or_else(|e| e.into_inner());
            map.entry((tenant, node)).or_default().clone()
        };
        let guard = tokio::time::timeout(wait, m.lock_owned())
            .await
            .map_err(|_| lock_busy())?;
        Ok(MigrationLock::new(MemoryLock(guard)))
    }
}

/// The restore point rows of the memory store, written by [`mem_insert_restore_point`].
pub(crate) fn mem_insert_restore_point(
    s: &MemoryDatabaseStore,
    ctx: &Context,
    node: NodeId,
    reason: &str,
    mark: &RestorePointMark,
) -> Result<Uuid> {
    let mut inner = s.lock();
    if !inner.rows.contains_key(&(ctx.tenant, node)) {
        return Err(Error::not_found("database"));
    }
    let id = Uuid::now_v7();
    inner.restore_points.push(RestorePointRow {
        id,
        tenant_id: ctx.tenant,
        node_id: node,
        label: None,
        reason: reason.to_string(),
        at: micros(mark.at),
        wal_lsn: mark.wal_lsn.clone(),
        actor: ctx.actor,
    });
    Ok(id)
}

/// The restore point rows of a database in the memory store (tests).
#[cfg(test)]
pub(crate) fn mem_restore_points(
    s: &MemoryDatabaseStore,
    tenant: TenantId,
    node: NodeId,
) -> Vec<RestorePointRow> {
    s.lock()
        .restore_points
        .iter()
        .filter(|p| p.tenant_id == tenant && p.node_id == node)
        .cloned()
        .collect()
}

fn mem_has_applied(s: &MemoryDatabaseStore, row: &MigrationRow) -> bool {
    s.lock().migrations.iter().any(|m| {
        m.tenant_id == row.tenant_id
            && m.node_id == row.node_id
            && m.name == row.name
            && m.status == MigrationStatus::Applied
    })
}

/// [`MigrationRecord`] in the memory store: the take of the approval through the approval
/// store first (its own lock serializes takes), then the row and the journal entry, which
/// cannot fail after it.
pub(crate) async fn mem_record(
    s: &MemoryDatabaseStore,
    ctx: &Context,
    rec: MigrationRecord,
    approvals: &dyn ApprovalStore,
) -> Result<(MigrationRow, Option<JournalEntry>)> {
    let row = rec.row;
    let (tenant, node) = (row.tenant_id, row.node_id);
    if tenant != ctx.tenant {
        return Err(Error::Internal("a migration row of another tenant".into()));
    }
    let revision = s
        .nodes
        .peek(node)
        .filter(|n| n.tenant_id == tenant)
        .map(|n| n.revision)
        .ok_or_else(|| Error::not_found("database"))?;
    if !s.lock().rows.contains_key(&(tenant, node)) {
        return Err(Error::not_found("database"));
    }
    if row.status == MigrationStatus::Applied && mem_has_applied(s, &row) {
        return Err(applied_already());
    }
    if let Some((id, take, at)) = &rec.take {
        approvals.take(tenant, node, *id, take, *at).await?;
    }
    s.lock().migrations.push(row.clone());
    let entry = match rec.event {
        Some((event, data)) => Some(s.journal.append(
            ctx,
            NewEntry {
                node_id: Some(node),
                revision: Some(revision),
                event,
                data,
            },
        )?),
        None => None,
    };
    Ok((row, entry))
}

pub(crate) fn mem_applied(
    s: &MemoryDatabaseStore,
    tenant: TenantId,
    node: NodeId,
    name: &str,
) -> Option<MigrationRow> {
    s.lock()
        .migrations
        .iter()
        .find(|m| {
            m.tenant_id == tenant
                && m.node_id == node
                && m.name == name
                && m.status == MigrationStatus::Applied
        })
        .cloned()
}

pub(crate) fn mem_list(
    s: &MemoryDatabaseStore,
    tenant: TenantId,
    node: NodeId,
    page: &PageRequest,
) -> Result<Page<MigrationRow>> {
    let after = page
        .cursor
        .as_ref()
        .map(|c| decode_cursor(node, c))
        .transpose()?;
    let mut items: Vec<MigrationRow> = s
        .lock()
        .migrations
        .iter()
        .filter(|m| m.tenant_id == tenant && m.node_id == node)
        .filter(|m| after.is_none_or(|(at, id)| (m.at, m.id) < (at, id)))
        .cloned()
        .collect();
    items.sort_by_key(|m| std::cmp::Reverse((m.at, m.id)));
    let limit = page.limit() as usize;
    items.truncate(limit + 1);
    Ok(page_of(node, items, limit))
}

pub(crate) fn mem_lens_pending(s: &MemoryDatabaseStore) -> Vec<(TenantId, NodeId, Uuid)> {
    s.lock()
        .migrations
        .iter()
        .filter(|m| without_lens_pending(m.error.as_deref()).is_some())
        .map(|m| (m.tenant_id, m.node_id, m.id))
        .collect()
}

pub(crate) fn mem_clear_lens_pending(
    s: &MemoryDatabaseStore,
    tenant: TenantId,
    node: NodeId,
    id: Uuid,
) -> bool {
    let mut inner = s.lock();
    let Some(m) = inner
        .migrations
        .iter_mut()
        .find(|m| m.tenant_id == tenant && m.node_id == node && m.id == id)
    else {
        return false;
    };
    match without_lens_pending(m.error.as_deref()) {
        Some(rest) => {
            m.error = rest;
            true
        }
        None => false,
    }
}

/// The copies of the source's applied migration rows for a new branch (card D-3a, decision
/// 0004-17), in the memory store's lock of [`super::super::DatabaseStore::insert_branch`]: a new
/// ID each, no restore point (it stays with the source).
pub(crate) fn mem_copy_applied(
    migrations: &mut Vec<MigrationRow>,
    tenant: TenantId,
    source: NodeId,
    branch: NodeId,
) {
    let copies: Vec<MigrationRow> = migrations
        .iter()
        .filter(|m| {
            m.tenant_id == tenant && m.node_id == source && m.status == MigrationStatus::Applied
        })
        .map(|m| MigrationRow {
            id: Uuid::now_v7(),
            node_id: branch,
            restore_point_id: None,
            ..m.clone()
        })
        .collect();
    migrations.extend(copies);
}

// ---------------------------------------------------------------------------- postgres

struct PgLock(sqlx::Transaction<'static, sqlx::Postgres>);

#[async_trait]
impl LockGuard for PgLock {
    async fn release(self: Box<Self>) {
        if let Err(e) = self.0.rollback().await {
            tracing::warn!(error = %e, "the migration lock was not released at once; the connection ends it");
        }
    }
}

/// The advisory lock key of a database's migration lock.
fn lock_key(tenant: TenantId, node: NodeId) -> String {
    format!("ruum.database_migration:{}:{}", tenant.0, node.0)
}

/// The migration lock on Postgres: a transaction-level advisory lock, held by a transaction of
/// `pool` (the store's lock pool, never its main pool) that stays open until the guard is
/// released or dropped. `try` in a loop, so a waiter holds no connection between its tries;
/// the whole acquisition (the connection and the lock) is inside `wait` (review D-2, 1).
pub(crate) async fn pg_lock(
    pool: &sqlx::PgPool,
    tenant: TenantId,
    node: NodeId,
    wait: Duration,
) -> Result<MigrationLock> {
    let key = lock_key(tenant, node);
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let mut tx = tokio::time::timeout_at(
            deadline,
            crate::journal::postgres::begin_tenant(pool, tenant),
        )
        .await
        .map_err(|_| lock_busy())??;
        let got: bool = tokio::time::timeout_at(
            deadline,
            sqlx::query_scalar("select pg_try_advisory_xact_lock(hashtextextended($1, 0))")
                .bind(&key)
                .fetch_one(&mut *tx),
        )
        .await
        .map_err(|_| lock_busy())??;
        if got {
            return Ok(MigrationLock::new(PgLock(tx)));
        }
        tokio::time::timeout_at(deadline, tx.rollback())
            .await
            .map_err(|_| lock_busy())??;
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(lock_busy());
        }
        tokio::time::sleep_until(deadline.min(now + Duration::from_millis(50))).await;
    }
}

pub(crate) async fn pg_insert_restore_point(
    pool: &sqlx::PgPool,
    ctx: &Context,
    node: NodeId,
    reason: &str,
    mark: &RestorePointMark,
) -> Result<Uuid> {
    let id = Uuid::now_v7();
    let mut tx = crate::sharing::postgres::begin(pool, ctx).await?;
    sqlx::query(
        "insert into database_restore_points (id, tenant_id, node_id, reason, at, wal_lsn, \
         actor_kind, actor, on_behalf_of) \
         values ($1, $2, $3, $4::restore_point_reason, $5, $6::pg_lsn, $7, $8, $9)",
    )
    .bind(id)
    .bind(ctx.tenant.0)
    .bind(node.0)
    .bind(reason)
    .bind(micros(mark.at))
    .bind(mark.wal_lsn.as_deref())
    .bind(ctx.actor.actor.kind)
    .bind(ctx.actor.actor.id.0)
    .bind(ctx.actor.on_behalf_of.map(|u| u.0))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

/// The columns that [`row_of`] reads.
const COLUMNS: &str = "id, tenant_id, node_id, name, checksum, up_sql, data_classes, \
    status::text as status, error, restore_point_id, actor_kind, actor, on_behalf_of, at";

fn row_of(r: &sqlx::postgres::PgRow) -> Result<MigrationRow> {
    use sqlx::Row;
    let checksum: Vec<u8> = r.try_get("checksum")?;
    let checksum: [u8; 32] = checksum
        .try_into()
        .map_err(|_| Error::Internal("a migration checksum is not 32 bytes".into()))?;
    Ok(MigrationRow {
        id: r.try_get("id")?,
        tenant_id: TenantId(r.try_get("tenant_id")?),
        node_id: NodeId(r.try_get("node_id")?),
        name: r.try_get("name")?,
        checksum,
        up_sql: r.try_get("up_sql")?,
        data_classes: r.try_get::<Value, _>("data_classes")?,
        status: MigrationStatus::parse(&r.try_get::<String, _>("status")?)?,
        error: r.try_get("error")?,
        restore_point_id: r.try_get("restore_point_id")?,
        actor: Actor {
            actor: PrincipalRef {
                kind: r.try_get::<PrincipalKind, _>("actor_kind")?,
                id: r.try_get::<Uuid, _>("actor")?.into(),
            },
            on_behalf_of: r.try_get::<Option<Uuid>, _>("on_behalf_of")?.map(UserId),
        },
        at: r.try_get::<OffsetDateTime, _>("at")?,
    })
}

/// [`MigrationRecord`] on Postgres, in ONE transaction of the tenant: the take of the approval
/// ([`PgApprovalStore::take_in`]), the migration row and the journal entry. A failure anywhere
/// writes nothing (the approval stays unused).
pub(crate) async fn pg_record(
    pool: &sqlx::PgPool,
    ctx: &Context,
    rec: MigrationRecord,
) -> Result<(MigrationRow, Option<JournalEntry>)> {
    let row = rec.row;
    if row.tenant_id != ctx.tenant {
        return Err(Error::Internal("a migration row of another tenant".into()));
    }
    let mut tx = crate::sharing::postgres::begin(pool, ctx).await?;
    if let Some((id, take, at)) = &rec.take {
        PgApprovalStore::take_in(&mut tx, row.tenant_id, row.node_id, *id, take, *at).await?;
    }
    let done = sqlx::query(
        "insert into database_migrations (id, tenant_id, node_id, name, checksum, up_sql, \
         data_classes, status, error, restore_point_id, actor_kind, actor, on_behalf_of, at) \
         values ($1, $2, $3, $4, $5, $6, $7, $8::migration_status, $9, $10, $11, $12, $13, $14) \
         on conflict (tenant_id, node_id, name) where status = 'applied' do nothing",
    )
    .bind(row.id)
    .bind(row.tenant_id.0)
    .bind(row.node_id.0)
    .bind(&row.name)
    .bind(row.checksum.to_vec())
    .bind(&row.up_sql)
    .bind(&row.data_classes)
    .bind(row.status.as_str())
    .bind(row.error.as_deref())
    .bind(row.restore_point_id)
    .bind(row.actor.actor.kind)
    .bind(row.actor.actor.id.0)
    .bind(row.actor.on_behalf_of.map(|u| u.0))
    .bind(row.at)
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() == 0 {
        return Err(applied_already());
    }
    let entry = match rec.event {
        Some((event, data)) => {
            let revision: i64 = sqlx::query_scalar(
                "select revision from nodes where tenant_id = $1 and id = $2 for share",
            )
            .bind(row.tenant_id.0)
            .bind(row.node_id.0)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| Error::not_found("database"))?;
            Some(
                crate::journal::postgres::append(
                    &mut tx,
                    ctx,
                    NewEntry {
                        node_id: Some(row.node_id),
                        revision: Some(revision),
                        event,
                        data,
                    },
                )
                .await?,
            )
        }
        None => None,
    };
    tx.commit().await?;
    Ok((row, entry))
}

pub(crate) async fn pg_applied(
    pool: &sqlx::PgPool,
    tenant: TenantId,
    node: NodeId,
    name: &str,
) -> Result<Option<MigrationRow>> {
    let mut tx = crate::journal::postgres::begin_tenant(pool, tenant).await?;
    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select {COLUMNS} from database_migrations \
         where tenant_id = $1 and node_id = $2 and name = $3 and status = 'applied'"
    )))
    .bind(tenant.0)
    .bind(node.0)
    .bind(name)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    r.as_ref().map(row_of).transpose()
}

pub(crate) async fn pg_list(
    pool: &sqlx::PgPool,
    tenant: TenantId,
    node: NodeId,
    page: &PageRequest,
) -> Result<Page<MigrationRow>> {
    let after = page
        .cursor
        .as_ref()
        .map(|c| decode_cursor(node, c))
        .transpose()?;
    let limit = page.limit() as usize;
    let mut tx = crate::journal::postgres::begin_tenant(pool, tenant).await?;
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select {COLUMNS} from database_migrations \
         where tenant_id = $1 and node_id = $2 \
         and ($3::timestamptz is null or (at, id) < ($3, $4)) \
         order by at desc, id desc limit $5"
    )))
    .bind(tenant.0)
    .bind(node.0)
    .bind(after.map(|(at, _)| at))
    .bind(after.map(|(_, id)| id))
    .bind(i64::try_from(limit + 1).unwrap_or(i64::MAX))
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let items = rows.iter().map(row_of).collect::<Result<Vec<_>>>()?;
    Ok(page_of(node, items, limit))
}

/// The rows with [`LENS_PENDING`] of every tenant (the lens sweeper; IDs only, as the system).
pub(crate) async fn pg_lens_pending(pool: &sqlx::PgPool) -> Result<Vec<(TenantId, NodeId, Uuid)>> {
    let mut tx = pool.begin().await?;
    sqlx::query("select set_config('ruum.system', 'on', true)")
        .execute(&mut *tx)
        .await?;
    let rows: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "select tenant_id, node_id, id from database_migrations \
         where error = $1 or right(error, length($2)) = $2 \
         order by at limit 500",
    )
    .bind(LENS_PENDING)
    .bind(format!("; {LENS_PENDING}"))
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows
        .into_iter()
        .map(|(t, n, id)| (TenantId(t), NodeId(n), id))
        .collect())
}

pub(crate) async fn pg_clear_lens_pending(
    pool: &sqlx::PgPool,
    tenant: TenantId,
    node: NodeId,
    id: Uuid,
) -> Result<bool> {
    let mut tx = crate::journal::postgres::begin_tenant(pool, tenant).await?;
    // The mark alone becomes `null`; a failed run's error keeps its text before the mark.
    let n = sqlx::query(
        "update database_migrations \
         set error = case when error = $4 then null \
                          else left(error, length(error) - length($5)) end \
         where tenant_id = $1 and node_id = $2 and id = $3 \
           and (error = $4 or right(error, length($5)) = $5)",
    )
    .bind(tenant.0)
    .bind(node.0)
    .bind(id)
    .bind(LENS_PENDING)
    .bind(format!("; {LENS_PENDING}"))
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(n == 1)
}
