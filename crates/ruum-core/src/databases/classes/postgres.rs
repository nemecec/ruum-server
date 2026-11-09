//! The Postgres `database_columns` store (migration 0004). Every statement filters by
//! `tenant_id` and runs in a transaction of the tenant (row-level security), except the scan
//! of the scheduled sync, which runs as the system (`ruum.system = on`) and reads only IDs.

use super::{
    ColumnRow, ColumnStore, Confirmation, Confirmed, LiveColumn, Suggestion, SyncOutcome,
    UnconfirmedReason, class_name, confirmed_data, no_such_column, parse_class, plan,
};
use crate::actor::{Actor, Context, PrincipalKind, PrincipalRef};
use crate::error::{Error, Result};
use crate::ids::{NodeId, PrincipalId, TenantId, UserId, VersionId};
use crate::journal::{ChangeInfo, EventType, NewEntry};
use async_trait::async_trait;
use ruum_dbengine::{ColumnIdentity, ColumnRef, DataClass};
use sqlx::{PgConnection, PgPool, Row};
use time::OffsetDateTime;

/// The Postgres store.
#[derive(Clone, Debug)]
pub struct PgColumnStore {
    pool: PgPool,
}

impl PgColumnStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// The columns that [`row_from`] reads.
const COLUMNS: &str = "schema_name, table_name, column_name, data_type, rel_oid::int8 as rel_oid, \
    attnum, type_oid::int8 as type_oid, unconfirmed_reason, suggested_class::text as suggested_class, \
    suggested_reason, suggested_kind, suggested_by, suggested_obo, suggested_at, \
    confirmed_class::text as confirmed_class, confirmed_by, confirmed_at, confirmed_version, \
    dropped_at, first_seen_at";

fn row_from(r: &sqlx::postgres::PgRow) -> Result<ColumnRow> {
    let class = |name: &str| -> Result<Option<DataClass>> {
        match r.try_get::<Option<String>, _>(name)? {
            None => Ok(None),
            Some(s) => parse_class(&s)
                .map(Some)
                .ok_or_else(|| Error::Internal(format!("unknown data class {s}"))),
        }
    };
    let identity = match (
        r.try_get::<Option<i64>, _>("rel_oid")?,
        r.try_get::<Option<i16>, _>("attnum")?,
    ) {
        (Some(rel), Some(attnum)) => Some(ColumnIdentity {
            rel_oid: rel as u32,
            attnum,
            type_oid: r.try_get::<Option<i64>, _>("type_oid")?.unwrap_or(0) as u32,
        }),
        _ => None,
    };
    let suggestion = match (class("suggested_class")?, r.try_get("suggested_at")?) {
        (Some(class), Some(at)) => {
            let kind: Option<PrincipalKind> = r.try_get("suggested_kind")?;
            let by: Option<uuid::Uuid> = r.try_get("suggested_by")?;
            Some(Suggestion {
                class,
                reason: r
                    .try_get::<Option<String>, _>("suggested_reason")?
                    .unwrap_or_default(),
                by: Actor {
                    actor: PrincipalRef {
                        kind: kind.unwrap_or(PrincipalKind::App),
                        id: PrincipalId(by.unwrap_or(super::SYSTEM_APP_ID)),
                    },
                    on_behalf_of: r
                        .try_get::<Option<uuid::Uuid>, _>("suggested_obo")?
                        .map(UserId),
                },
                at,
            })
        }
        _ => None,
    };
    let confirmed = match (
        class("confirmed_class")?,
        r.try_get::<Option<uuid::Uuid>, _>("confirmed_by")?,
        r.try_get::<Option<OffsetDateTime>, _>("confirmed_at")?,
        r.try_get::<Option<VersionId>, _>("confirmed_version")?,
    ) {
        (Some(class), Some(by), Some(at), Some(version)) => Some(Confirmation {
            class,
            by: UserId(by),
            at,
            version,
        }),
        _ => None,
    };
    let reason = match r.try_get::<Option<String>, _>("unconfirmed_reason")? {
        None => None,
        Some(s) => Some(
            UnconfirmedReason::parse(&s)
                .ok_or_else(|| Error::Internal(format!("unknown unconfirmed reason {s}")))?,
        ),
    };
    Ok(ColumnRow {
        column: ColumnRef {
            schema: r.try_get("schema_name")?,
            table: r.try_get("table_name")?,
            column: r.try_get("column_name")?,
        },
        data_type: r.try_get("data_type")?,
        identity,
        unconfirmed_reason: reason,
        suggestion,
        confirmed,
        dropped_at: r.try_get("dropped_at")?,
        first_seen_at: r.try_get("first_seen_at")?,
    })
}

async fn read_rows(
    conn: &mut PgConnection,
    tenant: TenantId,
    node: NodeId,
) -> Result<Vec<ColumnRow>> {
    // Byte order (`collate "C"`), as the memory store sorts: the database's collation puts
    // `ä` before `e`, Rust after it (review K-1, C-2 conformance case).
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select {COLUMNS} from database_columns where tenant_id = $1 and node_id = $2 \
         order by schema_name collate \"C\", table_name collate \"C\", column_name collate \"C\""
    )))
    .bind(tenant)
    .bind(node)
    .fetch_all(&mut *conn)
    .await?;
    rows.iter().map(row_from).collect()
}

async fn insert_row(
    conn: &mut PgConnection,
    tenant: TenantId,
    node: NodeId,
    r: &ColumnRow,
) -> Result<()> {
    let s = r.suggestion.as_ref();
    let c = r.confirmed.as_ref();
    sqlx::query(
        "insert into database_columns (tenant_id, node_id, schema_name, table_name, column_name, \
         data_type, rel_oid, attnum, type_oid, unconfirmed_reason, suggested_class, \
         suggested_reason, suggested_kind, suggested_by, suggested_obo, suggested_at, \
         confirmed_class, confirmed_by, confirmed_at, confirmed_version, dropped_at, \
         first_seen_at) values ($1, $2, $3, $4, $5, $6, $7::int8::oid, $8, $9::int8::oid, $10, \
         $11::data_class, $12, $13, $14, $15, $16, $17::data_class, $18, $19, $20, $21, $22)",
    )
    .bind(tenant)
    .bind(node)
    .bind(&r.column.schema)
    .bind(&r.column.table)
    .bind(&r.column.column)
    .bind(&r.data_type)
    .bind(r.identity.map(|i| i64::from(i.rel_oid)))
    .bind(r.identity.map(|i| i.attnum))
    .bind(r.identity.map(|i| i64::from(i.type_oid)))
    .bind(r.unconfirmed_reason.map(|x| x.as_str()))
    .bind(s.map(|s| class_name(s.class)))
    .bind(s.map(|s| s.reason.as_str()))
    .bind(s.map(|s| s.by.actor.kind))
    .bind(s.map(|s| s.by.actor.id.0))
    .bind(s.and_then(|s| s.by.on_behalf_of.map(|u| u.0)))
    .bind(s.map(|s| s.at))
    .bind(c.map(|c| class_name(c.class)))
    .bind(c.map(|c| c.by.0))
    .bind(c.map(|c| c.at))
    .bind(c.map(|c| c.version))
    .bind(r.dropped_at)
    .bind(r.first_seen_at)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Locks the `databases` row of the node (`404` without one), so the writes of one database's
/// columns never interleave.
async fn lock_database(conn: &mut PgConnection, tenant: TenantId, node: NodeId) -> Result<()> {
    sqlx::query("select 1 from databases where tenant_id = $1 and node_id = $2 for update")
        .bind(tenant)
        .bind(node)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| Error::not_found("database"))?;
    Ok(())
}

#[async_trait]
impl ColumnStore for PgColumnStore {
    async fn sync(
        &self,
        tenant: TenantId,
        node: NodeId,
        live: &[LiveColumn],
        now: OffsetDateTime,
    ) -> Result<(Vec<ColumnRow>, SyncOutcome)> {
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        lock_database(&mut tx, tenant, node).await?;
        let existing = read_rows(&mut tx, tenant, node).await?;
        let p = plan(&existing, live, now);
        if p.outcome.rows_changed {
            // The whole set of the database is written again: a renamed column, a column of a
            // name that comes back and a dropped row all change their primary key or take one
            // over, so deleting first never collides (decision 0004-35).
            sqlx::query("delete from database_columns where tenant_id = $1 and node_id = $2")
                .bind(tenant)
                .bind(node)
                .execute(&mut *tx)
                .await?;
            for r in &p.rows {
                insert_row(&mut tx, tenant, node, r).await?;
            }
        }
        tx.commit().await?;
        Ok((p.rows, p.outcome))
    }

    async fn list(&self, tenant: TenantId, node: NodeId) -> Result<Vec<ColumnRow>> {
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        let rows = read_rows(&mut tx, tenant, node).await?;
        tx.commit().await?;
        Ok(rows)
    }

    async fn suggest(
        &self,
        tenant: TenantId,
        node: NodeId,
        columns: &[(ColumnRef, DataClass, String)],
        by: Actor,
        at: OffsetDateTime,
    ) -> Result<()> {
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        lock_database(&mut tx, tenant, node).await?;
        for (i, (c, class, reason)) in columns.iter().enumerate() {
            let n = sqlx::query(
                "update database_columns set suggested_class = $6::data_class, \
                 suggested_reason = $7, suggested_kind = $8, suggested_by = $9, \
                 suggested_obo = $10, suggested_at = $11 \
                 where tenant_id = $1 and node_id = $2 and schema_name = $3 and table_name = $4 \
                 and column_name = $5 and dropped_at is null",
            )
            .bind(tenant)
            .bind(node)
            .bind(&c.schema)
            .bind(&c.table)
            .bind(&c.column)
            .bind(class_name(*class))
            .bind(reason)
            .bind(by.actor.kind)
            .bind(by.actor.id.0)
            .bind(by.on_behalf_of.map(|u| u.0))
            .bind(at)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if n != 1 {
                return Err(no_such_column(c, &format!("/suggestions/{i}")));
            }
        }
        tx.commit().await?;
        Ok(())
    }

    async fn confirm(
        &self,
        ctx: &Context,
        node: NodeId,
        columns: &[(ColumnRef, DataClass)],
        version: VersionId,
    ) -> Result<(Vec<Confirmed>, ChangeInfo)> {
        let by = crate::actor::require_user_actor(ctx)?;
        let tenant = ctx.tenant;
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        lock_database(&mut tx, tenant, node).await?;
        let at = crate::now();
        let mut done = Vec::with_capacity(columns.len());
        for (i, (c, class)) in columns.iter().enumerate() {
            let old: Option<Option<String>> = sqlx::query_scalar(
                "select confirmed_class::text from database_columns \
                 where tenant_id = $1 and node_id = $2 and schema_name = $3 and table_name = $4 \
                 and column_name = $5 and dropped_at is null",
            )
            .bind(tenant)
            .bind(node)
            .bind(&c.schema)
            .bind(&c.table)
            .bind(&c.column)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(old) = old else {
                return Err(no_such_column(c, &format!("/columns/{i}")));
            };
            sqlx::query(
                "update database_columns set confirmed_class = $6::data_class, \
                 confirmed_by = $7, confirmed_at = $8, confirmed_version = $9, \
                 unconfirmed_reason = null \
                 where tenant_id = $1 and node_id = $2 and schema_name = $3 and table_name = $4 \
                 and column_name = $5 and dropped_at is null",
            )
            .bind(tenant)
            .bind(node)
            .bind(&c.schema)
            .bind(&c.table)
            .bind(&c.column)
            .bind(class_name(*class))
            .bind(by.0)
            .bind(at)
            .bind(version)
            .execute(&mut *tx)
            .await?;
            done.push(Confirmed {
                column: c.clone(),
                class: *class,
                old_class: old.as_deref().and_then(parse_class),
            });
        }
        let revision: i64 =
            sqlx::query_scalar("select revision from nodes where tenant_id = $1 and id = $2")
                .bind(tenant)
                .bind(node)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| Error::not_found("database"))?;
        let e = crate::journal::postgres::append(
            &mut tx,
            ctx,
            NewEntry {
                node_id: Some(node),
                revision: Some(revision),
                event: EventType::DatabaseColumnsConfirmed,
                data: confirmed_data(version, &done),
            },
        )
        .await?;
        tx.commit().await?;
        Ok((
            done,
            ChangeInfo {
                seq: Some(e.seq),
                actor: ctx.actor,
                at: e.at,
            },
        ))
    }

    async fn unconfirmed_count(&self, tenant: TenantId, node: NodeId) -> Result<i64> {
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        let n: i64 = sqlx::query_scalar(
            "select count(*) from database_columns where tenant_id = $1 and node_id = $2 \
             and confirmed_class is null and dropped_at is null",
        )
        .bind(tenant)
        .bind(node)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(n)
    }

    async fn databases(&self) -> Result<Vec<(TenantId, NodeId)>> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("select set_config('ruum.system', 'on', true)")
            .execute(&mut *tx)
            .await?;
        let rows: Vec<(TenantId, NodeId)> =
            sqlx::query_as("select tenant_id, node_id from databases order by tenant_id, node_id")
                .fetch_all(&mut *tx)
                .await?;
        tx.commit().await?;
        Ok(rows)
    }
}
