//! The Postgres audit log (table `audit`, merged with `journal` on read).

use super::*;
use crate::actor::{PrincipalKind, PrincipalRef};
use crate::ids::PrincipalId;
use crate::journal::postgres::begin_tenant;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Row};

/// Stores one entry in the caller's transaction, for writes that must commit together with
/// their audit entry (for example a member removal). [`PgAuditLog::record`] uses its own.
pub async fn record_in(conn: &mut PgConnection, ctx: &Context, entry: NewAuditEntry) -> Result<()> {
    let entry = entry.normalized()?;
    sqlx::query(
        "insert into audit (id, tenant_id, action, actor_kind, actor, on_behalf_of, \
             target_type, target_id, ip, user_agent, data, at) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9::inet, $10, $11, $12)",
    )
    .bind(Uuid::now_v7())
    .bind(ctx.tenant.0)
    .bind(&entry.action)
    .bind(ctx.actor.actor.kind)
    .bind(ctx.actor.actor.id.0)
    .bind(ctx.actor.on_behalf_of.map(|u| u.0))
    .bind(&entry.target_type)
    .bind(&entry.target_id)
    .bind(&entry.ip)
    .bind(&entry.user_agent)
    .bind(&entry.data)
    .bind(crate::now())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

#[derive(Clone, Debug)]
pub struct PgAuditLog {
    pool: PgPool,
}

impl PgAuditLog {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn entry_from_row(row: &PgRow) -> Result<AuditEntry> {
    let kind: PrincipalKind = row.try_get("actor_kind")?;
    Ok(AuditEntry {
        id: row.try_get("id")?,
        action: row.try_get("action")?,
        actor: Actor {
            actor: PrincipalRef {
                kind,
                id: PrincipalId(row.try_get::<Uuid, _>("actor")?),
            },
            on_behalf_of: row.try_get::<Option<Uuid>, _>("on_behalf_of")?.map(UserId),
        },
        target_type: row.try_get("target_type")?,
        target_id: row.try_get("target_id")?,
        ip: row.try_get("ip")?,
        user_agent: row.try_get("user_agent")?,
        data: row.try_get("data")?,
        at: row.try_get("at")?,
    })
}

#[async_trait]
impl AuditLog for PgAuditLog {
    async fn record(&self, ctx: &Context, entry: NewAuditEntry) -> Result<()> {
        let mut tx = begin_tenant(&self.pool, ctx.tenant).await?;
        record_in(&mut tx, ctx, entry).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn record_first_use(
        &self,
        ctx: &Context,
        session: Uuid,
        entry: NewAuditEntry,
    ) -> Result<bool> {
        let mut tx = begin_tenant(&self.pool, ctx.tenant).await?;
        let new = sqlx::query(
            "insert into session_tenants (tenant_id, session_id) values ($1, $2) \
             on conflict do nothing",
        )
        .bind(ctx.tenant.0)
        .bind(session)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if new {
            record_in(&mut tx, ctx, entry).await?;
        }
        tx.commit().await?;
        Ok(new)
    }

    /// Keyset per source: at most `limit + 1` rows from `audit` (index `audit_tenant_at_idx`)
    /// and from `journal`, each in the order (`at` desc, `id` desc), merged here. The id of a
    /// journal row grows with its `seq` ([`journal_audit_id`]), so `seq` desc is its `id` order.
    async fn list(&self, ctx: &Context, query: AuditQuery) -> Result<Page<AuditEntry>> {
        let after = query.page.cursor.as_ref().map(decode_cursor).transpose()?;
        let limit = query.page.limit();
        let take = i64::from(limit) + 1;
        let mut tx = begin_tenant(&self.pool, ctx.tenant).await?;
        let audit_rows = sqlx::query(
            "select id, action, actor_kind, actor, on_behalf_of, target_type, target_id, \
                    host(ip) as ip, user_agent, data, at \
             from audit \
             where tenant_id = $1 \
               and ($2::timestamptz is null or at >= $2) \
               and ($3::timestamptz is null or at <= $3) \
               and ($4::uuid is null or actor = $4) \
               and ($5::text is null or starts_with(action, $5)) \
               and ($6::timestamptz is null or (at, id) < ($6, $7::uuid)) \
             order by at desc, id desc \
             limit $8",
        )
        .bind(ctx.tenant.0)
        .bind(query.since)
        .bind(query.until)
        .bind(query.actor)
        .bind(query.action.as_deref())
        .bind(after.map(|(at, _)| at))
        .bind(after.map(|(_, id)| id))
        .bind(take)
        .fetch_all(&mut *tx)
        .await?;
        let journal_rows = sqlx::query(
            "select id, action, actor_kind, actor, on_behalf_of, target_type, target_id, ip, \
                    user_agent, data, at \
             from ( \
                 select ('00000000-0000-8000-8000-' || lpad(to_hex(seq), 12, '0'))::uuid as id, \
                        seq, type as action, actor_kind, actor, on_behalf_of, \
                        case when node_id is null then null else 'node' end as target_type, \
                        node_id::text as target_id, null::text as ip, null::text as user_agent, \
                        data || jsonb_build_object('seq', seq) \
                             || case when revision is null then '{}'::jsonb \
                                     else jsonb_build_object('revision', revision) end as data, \
                        at \
                 from journal \
                 where tenant_id = $1 \
                   and ($2::timestamptz is null or at >= $2) \
                   and ($3::timestamptz is null or at <= $3) \
                   and ($4::uuid is null or actor = $4) \
                   and ($5::text is null or starts_with(type, $5)) \
                   and ($6::timestamptz is null or at <= $6) \
             ) e \
             where ($6::timestamptz is null or (at, id) < ($6, $7::uuid)) \
             order by at desc, seq desc \
             limit $8",
        )
        .bind(ctx.tenant.0)
        .bind(query.since)
        .bind(query.until)
        .bind(query.actor)
        .bind(query.action.as_deref())
        .bind(after.map(|(at, _)| at))
        .bind(after.map(|(_, id)| id))
        .bind(take)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let mut items = audit_rows
            .iter()
            .chain(journal_rows.iter())
            .map(entry_from_row)
            .collect::<Result<Vec<_>>>()?;
        items.sort_by_key(|e| std::cmp::Reverse((e.at, e.id)));
        let more = items.len() > limit as usize;
        items.truncate(limit as usize);
        let next_cursor = if more {
            items.last().map(|e| encode_cursor(e.at, e.id))
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }
}
