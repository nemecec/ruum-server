//! The Postgres journal.
//!
//! [`append`] is the only way to write the journal. Every Postgres store calls it with the
//! connection of its own transaction, after the change and before the commit, so a change and its
//! entry are one unit (section 2.5). It takes `ruum_next_seq(tenant)`, which locks the tenant's
//! counter row until the commit, so `seq` has no gaps and no duplicates.
//!
//! [`PgJournal`] is the read side ([`Journal`]).
//!
//! Row-level security: the journal tables are FORCEd. The caller's transaction must have set
//! `ruum.tenant_id` to `ctx.tenant` (or `ruum.system`), as for every other table. The read side
//! sets it itself on its own transaction.

use super::{EventType, Journal, JournalEntry, NewEntry, check_cursor, encode_cursor};
use crate::actor::{Actor, Context, PrincipalKind, PrincipalRef};
use crate::error::{Error, Result};
use crate::ids::{NodeId, PrincipalId, TenantId, UserId};
use crate::paging::Page;
use async_trait::async_trait;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

/// Appends one entry in the caller's transaction and returns it with its `seq`.
///
/// The tenant and the actor come from `ctx`. When the tenant has no counter row yet (the tenant
/// store normally makes it with the tenant), this makes it.
pub async fn append(
    conn: &mut PgConnection,
    ctx: &Context,
    entry: NewEntry,
) -> Result<JournalEntry> {
    let seq = match next_seq(conn, ctx.tenant).await? {
        Some(seq) => seq,
        None => {
            sqlx::query(
                "insert into journal_counters (tenant_id) values ($1) on conflict do nothing",
            )
            .bind(ctx.tenant.0)
            .execute(&mut *conn)
            .await?;
            next_seq(conn, ctx.tenant)
                .await?
                .ok_or_else(|| Error::Internal("journal counter row is missing".into()))?
        }
    };
    let at = crate::now();
    sqlx::query(
        "insert into journal \
             (tenant_id, seq, node_id, revision, type, actor_kind, actor, on_behalf_of, at, data) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(ctx.tenant.0)
    .bind(seq)
    .bind(entry.node_id.map(|n| n.0))
    .bind(entry.revision)
    .bind(entry.event.as_str())
    .bind(ctx.actor.actor.kind)
    .bind(ctx.actor.actor.id.0)
    .bind(ctx.actor.on_behalf_of.map(|u| u.0))
    .bind(at)
    .bind(&entry.data)
    .execute(&mut *conn)
    .await?;
    Ok(JournalEntry {
        tenant_id: ctx.tenant,
        seq,
        node_id: entry.node_id,
        revision: entry.revision,
        event: entry.event,
        actor: ctx.actor,
        at,
        data: entry.data,
    })
}

async fn next_seq(conn: &mut PgConnection, tenant: TenantId) -> Result<Option<i64>> {
    Ok(
        sqlx::query_scalar::<_, Option<i64>>("select ruum_next_seq($1)")
            .bind(tenant.0)
            .fetch_one(&mut *conn)
            .await?,
    )
}

/// Opens a transaction with the row-level security setting of the tenant
/// (`set_config('ruum.tenant_id', <tenant>, true)`). Any store may use it.
pub async fn begin_tenant(
    pool: &PgPool,
    tenant: TenantId,
) -> Result<Transaction<'static, Postgres>> {
    let mut tx = pool.begin().await?;
    sqlx::query("select set_config('ruum.tenant_id', $1, true)")
        .bind(tenant.0.to_string())
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

/// The columns that [`entry_from_row`] reads.
macro_rules! columns {
    () => {
        "tenant_id, seq, node_id, revision, type, actor_kind, actor, on_behalf_of, at, data"
    };
}

fn entry_from_row(row: &PgRow) -> Result<JournalEntry> {
    let event: String = row.try_get("type")?;
    let event = super::EventType::parse(&event)
        .ok_or_else(|| Error::Internal(format!("unknown journal event type {event}")))?;
    let kind: PrincipalKind = row.try_get("actor_kind")?;
    Ok(JournalEntry {
        tenant_id: TenantId(row.try_get::<Uuid, _>("tenant_id")?),
        seq: row.try_get("seq")?,
        node_id: row.try_get::<Option<Uuid>, _>("node_id")?.map(NodeId),
        revision: row.try_get("revision")?,
        event,
        actor: Actor {
            actor: PrincipalRef {
                kind,
                id: PrincipalId(row.try_get::<Uuid, _>("actor")?),
            },
            on_behalf_of: row.try_get::<Option<Uuid>, _>("on_behalf_of")?.map(UserId),
        },
        at: row.try_get::<OffsetDateTime, _>("at")?,
        data: row.try_get("data")?,
    })
}

/// The read side of the journal on Postgres.
#[derive(Clone, Debug)]
pub struct PgJournal {
    pool: PgPool,
}

impl PgJournal {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl Journal for PgJournal {
    async fn read(
        &self,
        ctx: &Context,
        after: Option<i64>,
        limit: u32,
    ) -> Result<Page<JournalEntry>> {
        let mut tx = begin_tenant(&self.pool, ctx.tenant).await?;
        let (oldest, latest): (Option<i64>, Option<i64>) = sqlx::query_as(
            "select (select min(seq) from journal where tenant_id = $1), \
                    (select last_seq from journal_counters where tenant_id = $1)",
        )
        .bind(ctx.tenant.0)
        .fetch_one(&mut *tx)
        .await?;
        let latest = latest.unwrap_or(0);
        // With no rows kept, the next entry gets `latest + 1`.
        let oldest = oldest.unwrap_or(latest + 1);
        let after = check_cursor(after, oldest, latest)?;
        let rows = sqlx::query(concat!(
            "select ",
            columns!(),
            " from journal where tenant_id = $1 and seq > $2 order by seq limit $3"
        ))
        .bind(ctx.tenant.0)
        .bind(after)
        .bind(i64::from(limit.max(1)))
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let items = rows
            .iter()
            .map(entry_from_row)
            .collect::<Result<Vec<_>>>()?;
        let next_cursor = Some(encode_cursor(items.last().map_or(after, |e| e.seq)));
        Ok(Page { items, next_cursor })
    }

    async fn latest_seq(&self, ctx: &Context) -> Result<i64> {
        let mut tx = begin_tenant(&self.pool, ctx.tenant).await?;
        let latest: Option<i64> =
            sqlx::query_scalar("select last_seq from journal_counters where tenant_id = $1")
                .bind(ctx.tenant.0)
                .fetch_optional(&mut *tx)
                .await?;
        tx.commit().await?;
        Ok(latest.unwrap_or(0))
    }

    async fn node_entries(
        &self,
        ctx: &Context,
        node: NodeId,
        before: i64,
        events: &[EventType],
        limit: u32,
    ) -> Result<Vec<JournalEntry>> {
        let types: Vec<&str> = events.iter().map(|e| e.as_str()).collect();
        let mut tx = begin_tenant(&self.pool, ctx.tenant).await?;
        let rows = sqlx::query(concat!(
            "select ",
            columns!(),
            " from journal where tenant_id = $1 and node_id = $2 and seq < $3 \
              and type = any($4) order by seq desc limit $5"
        ))
        .bind(ctx.tenant.0)
        .bind(node.0)
        .bind(before)
        .bind(&types)
        .bind(i64::from(limit))
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter().map(entry_from_row).collect()
    }
}
