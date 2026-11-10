//! The Postgres approval store (`database_approvals` of migration 0004). Every statement filters
//! by `tenant_id` and runs in a transaction that sets the tenant (row-level security).
//!
//! - Requests of one key take an advisory lock first (`pg_advisory_xact_lock` on a hash of the
//!   key), so two at once make one row (decision 0004-35: until 0004 gets the partial unique
//!   index on pending grant requests).
//! - A decision and a take lock the approval row (`select ... for update`), so of two takes at
//!   once exactly one succeeds, and a decision sees the row as the other left it.
//! - An approved grant request writes the grant in the transaction of the decision: the restore
//!   point row, the grant row, `database.grant_created` (with `approval_id`) and the tuple
//!   through the outbox, under the grant tuple lock of card A-1 (the same key), then the
//!   decision and `database.approval_decided`. A failure anywhere rolls back all of it.

use super::{
    ApprovalFilter, ApprovalReason, ApprovalStatus, ApprovalStore, ApprovalSubject,
    DatabaseApproval, Decision, NewApproval, PENDING_HOURS, PlannedColumn, Take,
    approval_not_found, check_take, decided, decided_data, micros, not_pending, page_of,
    parse_cursor, rfc3339,
};
use crate::actor::{Actor, Context, PrincipalKind, PrincipalRef};
use crate::authz::{Authz, RelationshipUpdate};
use crate::databases::access::grants::{GrantKind, NewGrant, tuple};
use crate::error::{Error, Result};
use crate::ids::{NodeId, TenantId, UserId};
use crate::journal::{ChangeInfo, EventType, NewEntry, Written};
use crate::paging::{Page, PageRequest};
use async_trait::async_trait;
use serde_json::{Value, json};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

/// The Postgres store.
pub struct PgApprovalStore {
    pool: sqlx::PgPool,
}

impl PgApprovalStore {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

macro_rules! approval_columns {
    () => {
        "id, tenant_id, node_id, status::text as status, subject, migration_name, statement, \
         planned_columns, grant_kind::text as grant_kind, grant_minutes, grant_reason, grant_id, \
         statement_hash, params, reasons, estimated_rows, requested_kind, requested_by, \
         requested_obo, decided_by, decided_at, comment, expires_at, used_at, created_at"
    };
}

fn grant_kind(s: &str) -> Result<GrantKind> {
    match s {
        "write_live" => Ok(GrantKind::WriteLive),
        "read_personal" => Ok(GrantKind::ReadPersonal),
        other => Err(Error::Internal(format!("bad grant kind {other}"))),
    }
}

fn approval_of(r: &sqlx::postgres::PgRow) -> Result<DatabaseApproval> {
    let status: String = r.try_get("status")?;
    let subject: String = r.try_get("subject")?;
    let kind: Option<String> = r.try_get("grant_kind")?;
    let hash: Vec<u8> = r.try_get("statement_hash")?;
    let planned: Value = r.try_get("planned_columns")?;
    let params: Value = r.try_get("params")?;
    let reasons: Vec<String> = r.try_get("reasons")?;
    let requested_kind: PrincipalKind = r.try_get("requested_kind")?;
    let requested_by: Uuid = r.try_get("requested_by")?;
    let requested_obo: Option<Uuid> = r.try_get("requested_obo")?;
    Ok(DatabaseApproval {
        id: r.try_get("id")?,
        tenant_id: TenantId(r.try_get("tenant_id")?),
        node_id: NodeId(r.try_get("node_id")?),
        subject: ApprovalSubject::parse(&subject)?,
        status: ApprovalStatus::parse(&status)?,
        migration_name: r.try_get("migration_name")?,
        statement: r.try_get("statement")?,
        planned_columns: serde_json::from_value::<Vec<PlannedColumn>>(planned)
            .map_err(|e| Error::Internal(format!("planned_columns: {e}")))?,
        grant_kind: kind.as_deref().map(grant_kind).transpose()?,
        grant_minutes: r.try_get("grant_minutes")?,
        grant_reason: r.try_get("grant_reason")?,
        grant_id: r.try_get("grant_id")?,
        statement_hash: hash
            .try_into()
            .map_err(|_| Error::Internal("statement_hash is not 32 bytes".into()))?,
        params: serde_json::from_value(params)
            .map_err(|e| Error::Internal(format!("params: {e}")))?,
        reasons: reasons
            .iter()
            .map(|s| ApprovalReason::parse(s))
            .collect::<Result<_>>()?,
        estimated_rows: r.try_get("estimated_rows")?,
        requested_by: Actor {
            actor: PrincipalRef {
                kind: requested_kind,
                id: requested_by.into(),
            },
            on_behalf_of: requested_obo.map(UserId),
        },
        decided_by: r.try_get::<Option<Uuid>, _>("decided_by")?.map(UserId),
        decided_by_display_name: None,
        decided_at: r.try_get("decided_at")?,
        comment: r.try_get("comment")?,
        expires_at: r.try_get("expires_at")?,
        used_at: r.try_get("used_at")?,
        created_at: r.try_get("created_at")?,
    })
}

fn to_json<T: serde::Serialize>(v: &T) -> Result<Value> {
    serde_json::to_value(v).map_err(|e| Error::Internal(e.to_string()))
}

async fn node_revision(
    conn: &mut sqlx::PgConnection,
    tenant: TenantId,
    node: NodeId,
) -> Result<i64> {
    let r: Option<i64> =
        sqlx::query_scalar("select revision from nodes where tenant_id = $1 and id = $2 for share")
            .bind(tenant.0)
            .bind(node.0)
            .fetch_optional(conn)
            .await?;
    r.ok_or_else(|| Error::not_found("database"))
}

/// Serializes the writes of one key until the end of the transaction. A hash collision of two
/// keys only serializes more.
async fn advisory_lock(conn: &mut sqlx::PgConnection, key: String) -> Result<()> {
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(key)
        .execute(conn)
        .await?;
    Ok(())
}

/// The approval row of a database of the tenant, locked until the end of the transaction.
async fn locked(
    conn: &mut sqlx::PgConnection,
    tenant: TenantId,
    node: NodeId,
    id: Uuid,
) -> Result<DatabaseApproval> {
    let row = sqlx::query(concat!(
        "select ",
        approval_columns!(),
        " from database_approvals where tenant_id = $1 and node_id = $2 and id = $3 for update"
    ))
    .bind(tenant.0)
    .bind(node.0)
    .bind(id)
    .fetch_optional(conn)
    .await?
    .ok_or_else(approval_not_found)?;
    approval_of(&row)
}

/// Writes the grant of an approved grant request in the caller's transaction, as card A-1's
/// store does (the same tuple lock, rows, journal entry and outbox row), with `approval_id` in
/// `database.grant_created`. Returns the grant ID and the outbox row.
async fn write_grant(
    conn: &mut sqlx::PgConnection,
    ctx: &Context,
    granted_by: UserId,
    revision: i64,
    g: &NewGrant,
    approval: Uuid,
) -> Result<(Uuid, i64)> {
    // The key of card A-1's `lock_tuple`, so this write is serialized with its revokes.
    advisory_lock(
        conn,
        format!(
            "database_access_grant:{}:{}:{}:{}",
            ctx.tenant.0,
            g.node.0,
            g.agent.0,
            g.kind.as_str()
        ),
    )
    .await?;
    let point = match &g.restore_point {
        Some(m) => {
            let id = Uuid::now_v7();
            sqlx::query(
                "insert into database_restore_points (id, tenant_id, node_id, reason, at, \
                 wal_lsn, actor_kind, actor, on_behalf_of) \
                 values ($1, $2, $3, 'before_write_grant', $4, $5::pg_lsn, $6, $7, $8)",
            )
            .bind(id)
            .bind(ctx.tenant.0)
            .bind(g.node.0)
            .bind(micros(m.at))
            .bind(m.wal_lsn.as_deref())
            .bind(ctx.actor.actor.kind)
            .bind(ctx.actor.actor.id.0)
            .bind(ctx.actor.on_behalf_of.map(|u| u.0))
            .execute(&mut *conn)
            .await?;
            Some(id)
        }
        None => None,
    };
    let id = Uuid::now_v7();
    let expires_at = micros(g.expires_at);
    sqlx::query(
        "insert into database_access_grants (id, tenant_id, node_id, kind, agent_id, \
         granted_by, reason, starts_at, expires_at, restore_point_id, created_at) \
         values ($1, $2, $3, $4::database_grant_kind, $5, $6, $7, $8, $9, $10, $8)",
    )
    .bind(id)
    .bind(ctx.tenant.0)
    .bind(g.node.0)
    .bind(g.kind.as_str())
    .bind(g.agent.0)
    .bind(granted_by.0)
    .bind(g.reason.as_deref())
    .bind(micros(g.starts_at))
    .bind(expires_at)
    .bind(point)
    .execute(&mut *conn)
    .await?;
    crate::journal::postgres::append(
        conn,
        ctx,
        NewEntry {
            node_id: Some(g.node),
            revision: Some(revision),
            event: EventType::DatabaseGrantCreated,
            data: json!({
                "grant_id": id,
                "kind": g.kind.as_str(),
                "agent_id": g.agent,
                "expires_at": rfc3339(expires_at),
                "restore_point_id": point,
                "approval_id": approval,
            }),
        },
    )
    .await?;
    let ob = crate::authz::outbox::enqueue(
        conn,
        ctx.tenant,
        &[RelationshipUpdate::Touch(tuple(g.node, g.kind, g.agent))],
    )
    .await?;
    Ok((id, ob))
}

impl PgApprovalStore {
    /// [`ApprovalStore::take`] in the caller's transaction (`conn` must have the tenant set):
    /// locks the row, checks it ([`check_take`]) and marks it `used`. A caller that runs its own
    /// write in that transaction gets "used" and its write together, or neither.
    pub async fn take_in(
        conn: &mut sqlx::PgConnection,
        tenant: TenantId,
        node: NodeId,
        id: Uuid,
        take: &Take,
        at: OffsetDateTime,
    ) -> Result<DatabaseApproval> {
        let a = locked(conn, tenant, node, id).await?;
        check_take(&a, take, at)?;
        let row = sqlx::query(concat!(
            "update database_approvals set status = 'used', used_at = $3 \
             where tenant_id = $1 and id = $2 returning ",
            approval_columns!()
        ))
        .bind(tenant.0)
        .bind(id)
        .bind(micros(at))
        .fetch_one(conn)
        .await?;
        approval_of(&row)
    }
}

#[async_trait]
impl ApprovalStore for PgApprovalStore {
    async fn request(
        &self,
        ctx: &Context,
        new: NewApproval,
        at: OffsetDateTime,
    ) -> Result<(DatabaseApproval, bool)> {
        let at = micros(at);
        let mut tx = crate::sharing::postgres::begin(&self.pool, ctx).await?;
        let key = match new.subject {
            ApprovalSubject::Grant => format!(
                "database_approval:{}:{}:{}:grant:{}",
                ctx.tenant.0,
                new.node.0,
                ctx.actor.actor.id.0,
                new.grant_kind.map(GrantKind::as_str).unwrap_or_default()
            ),
            s => format!(
                "database_approval:{}:{}:{}:{}:{}",
                ctx.tenant.0,
                new.node.0,
                ctx.actor.actor.id.0,
                s.as_str(),
                hex(&new.statement_hash)
            ),
        };
        advisory_lock(&mut tx, key).await?;
        let rows = sqlx::query(concat!(
            "select ",
            approval_columns!(),
            " from database_approvals where tenant_id = $1 and node_id = $2 \
             and requested_kind = $3 and requested_by = $4 and subject = $5 \
             and status = 'pending' \
             and (case when $5 = 'grant' then grant_kind::text = $6 \
                       else statement_hash = $7 end) \
             order by id for update"
        ))
        .bind(ctx.tenant.0)
        .bind(new.node.0)
        .bind(ctx.actor.actor.kind)
        .bind(ctx.actor.actor.id.0)
        .bind(new.subject.as_str())
        .bind(new.grant_kind.map(GrantKind::as_str))
        .bind(new.statement_hash.to_vec())
        .fetch_all(&mut *tx)
        .await?;
        for r in &rows {
            let a = approval_of(r)?;
            if a.status_at(at) == ApprovalStatus::Pending {
                tx.commit().await?;
                return Ok((a, false));
            }
            // A pending row past its time: mark it, so one pending row of a key exists.
            sqlx::query(
                "update database_approvals set status = 'expired' \
                 where tenant_id = $1 and id = $2",
            )
            .bind(ctx.tenant.0)
            .bind(a.id)
            .execute(&mut *tx)
            .await?;
        }
        let reasons: Vec<String> = new.reasons.iter().map(|r| r.as_str().to_string()).collect();
        let row = sqlx::query(concat!(
            "insert into database_approvals (id, tenant_id, node_id, status, subject, \
             migration_name, statement, planned_columns, grant_kind, grant_minutes, \
             grant_reason, statement_hash, params, reasons, estimated_rows, requested_kind, \
             requested_by, requested_obo, expires_at, created_at) \
             values ($1, $2, $3, 'pending', $4, $5, $6, $7, $8::database_grant_kind, $9, $10, \
             $11, $12, $13, $14, $15, $16, $17, $18, $19) returning ",
            approval_columns!()
        ))
        .bind(Uuid::now_v7())
        .bind(ctx.tenant.0)
        .bind(new.node.0)
        .bind(new.subject.as_str())
        .bind(new.migration_name.as_deref())
        .bind(new.statement.as_deref())
        .bind(to_json(&new.planned_columns)?)
        .bind(new.grant_kind.map(GrantKind::as_str))
        .bind(new.grant_minutes)
        .bind(new.grant_reason.as_deref())
        .bind(new.statement_hash.to_vec())
        .bind(to_json(&new.params)?)
        .bind(&reasons)
        .bind(new.estimated_rows)
        .bind(ctx.actor.actor.kind)
        .bind(ctx.actor.actor.id.0)
        .bind(ctx.actor.on_behalf_of.map(|u| u.0))
        .bind(at + time::Duration::hours(PENDING_HOURS))
        .bind(at)
        .fetch_one(&mut *tx)
        .await?;
        let a = approval_of(&row)?;
        tx.commit().await?;
        Ok((a, true))
    }

    async fn get(
        &self,
        tenant: TenantId,
        node: NodeId,
        id: Uuid,
    ) -> Result<Option<DatabaseApproval>> {
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        let row = sqlx::query(concat!(
            "select ",
            approval_columns!(),
            " from database_approvals where tenant_id = $1 and node_id = $2 and id = $3"
        ))
        .bind(tenant.0)
        .bind(node.0)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.as_ref().map(approval_of).transpose()
    }

    async fn list(
        &self,
        tenant: TenantId,
        filter: ApprovalFilter,
        page: PageRequest,
        at: OffsetDateTime,
    ) -> Result<Page<DatabaseApproval>> {
        let after = parse_cursor(&page)?;
        let limit = page.limit() as usize;
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        // The status at `at`, as `DatabaseApproval::status_at` reads it.
        let rows = sqlx::query(concat!(
            "select ",
            approval_columns!(),
            " from database_approvals where tenant_id = $1 \
             and ($2::uuid is null or node_id = $2) \
             and ($3::uuid is null or (requested_kind = 'agent' and requested_by = $3)) \
             and ($4::uuid is null or id < $4) \
             and ($5::text is null \
                  or ($5 = 'pending' and status = 'pending' and expires_at > $6) \
                  or ($5 = 'approved' and status = 'approved' \
                      and (subject = 'grant' or expires_at > $6)) \
                  or ($5 = 'expired' and (status = 'expired' \
                      or (status = 'pending' and expires_at <= $6) \
                      or (status = 'approved' and subject <> 'grant' and expires_at <= $6))) \
                  or ($5 in ('denied', 'used') and status::text = $5)) \
             order by id desc limit $7"
        ))
        .bind(tenant.0)
        .bind(filter.node.map(|n| n.0))
        .bind(filter.requested_by.map(|a| a.0))
        .bind(after)
        .bind(filter.status.map(ApprovalStatus::as_str))
        .bind(at)
        .bind((limit + 1) as i64)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let items = rows.iter().map(approval_of).collect::<Result<Vec<_>>>()?;
        Ok(page_of(items, limit))
    }

    async fn decide(
        &self,
        ctx: &Context,
        authz: &dyn Authz,
        node: NodeId,
        id: Uuid,
        decision: Decision,
        at: OffsetDateTime,
    ) -> Result<Written<DatabaseApproval>> {
        let user = ctx
            .actor
            .effective_user()
            .ok_or_else(Error::user_required)?;
        let mut tx = crate::sharing::postgres::begin(&self.pool, ctx).await?;
        let a = locked(&mut tx, ctx.tenant, node, id).await?;
        if a.status_at(at) != ApprovalStatus::Pending {
            return Err(not_pending());
        }
        let revision = node_revision(&mut tx, ctx.tenant, node).await?;
        let mut outbox = Vec::new();
        let grant_id = match (&decision.grant, decision.approve, a.subject) {
            (Some(g), true, ApprovalSubject::Grant) => {
                let (gid, ob) = write_grant(&mut tx, ctx, user, revision, g, a.id).await?;
                outbox.push(ob);
                Some(gid)
            }
            (None, true, ApprovalSubject::Grant) => {
                return Err(Error::Internal(
                    "an approved grant request needs its grant".into(),
                ));
            }
            _ => None,
        };
        let (a, classes) = decided(a, user, &decision, grant_id, at);
        let row = sqlx::query(concat!(
            "update database_approvals set status = $3::approval_status, decided_by = $4, \
             decided_at = $5, comment = $6, grant_id = $7, expires_at = $8, \
             planned_columns = $9 where tenant_id = $1 and id = $2 returning ",
            approval_columns!()
        ))
        .bind(ctx.tenant.0)
        .bind(a.id)
        .bind(a.status.as_str())
        .bind(user.0)
        .bind(a.decided_at)
        .bind(a.comment.as_deref())
        .bind(a.grant_id)
        .bind(a.expires_at)
        .bind(to_json(&a.planned_columns)?)
        .fetch_one(&mut *tx)
        .await?;
        let a = approval_of(&row)?;
        let e = crate::journal::postgres::append(
            &mut tx,
            ctx,
            NewEntry {
                node_id: Some(node),
                revision: Some(revision),
                event: EventType::DatabaseApprovalDecided,
                data: decided_data(&a, decision.approve, classes),
            },
        )
        .await?;
        tx.commit().await?;
        crate::authz::outbox::apply_after_commit(&self.pool, authz, ctx.tenant, &outbox).await;
        Ok(Written {
            value: a,
            change: ChangeInfo {
                seq: Some(e.seq),
                actor: e.actor,
                at: e.at,
            },
        })
    }

    async fn take(
        &self,
        tenant: TenantId,
        node: NodeId,
        id: Uuid,
        take: &Take,
        at: OffsetDateTime,
    ) -> Result<DatabaseApproval> {
        let mut tx = crate::journal::postgres::begin_tenant(&self.pool, tenant).await?;
        let a = Self::take_in(&mut tx, tenant, node, id, take, at).await?;
        tx.commit().await?;
        Ok(a)
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
