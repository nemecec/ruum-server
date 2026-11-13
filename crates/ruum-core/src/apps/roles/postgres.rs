//! The Postgres [`RoleStore`] (table `app_role_grants` of migration 0011). Every statement
//! filters by `tenant_id` and runs in a transaction of the tenant (forced row-level security).
//! A write stores the row, its journal entry and the outbox row of its tuple in one transaction,
//! then applies the outbox after the commit ([`outbox::apply_after_commit`]); a crash or an engine
//! failure leaves the outbox row to the worker, so the row and the tuple never stay apart.

use super::*;
use crate::authz::outbox;
use crate::journal::postgres::{append, begin_tenant};
use crate::journal::{ChangeInfo, EventType, NewEntry};
use sqlx::Row;
use sqlx::postgres::PgRow;

/// The Postgres store.
#[derive(Clone, Debug)]
pub struct PgRoleStore {
    pool: sqlx::PgPool,
}

impl PgRoleStore {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

const COLUMNS: &str = "tenant_id, id, app_node_id, role, grantee_kind, grantee_id, \
    grantee_email, granted_kind, granted_by, granted_obo, created_at";

fn grant_row(r: &PgRow) -> Result<RoleGrant> {
    let kind: String = r.try_get("grantee_kind")?;
    let grantee = match kind.as_str() {
        "user" => Grantee::User {
            id: UserId(r.try_get("grantee_id")?),
        },
        "email" => Grantee::Email {
            email: r.try_get("grantee_email")?,
        },
        "tenant_members" => Grantee::TenantMembers,
        other => {
            return Err(Error::Internal(format!(
                "app role grant with an unknown grantee kind {other}"
            )));
        }
    };
    Ok(RoleGrant {
        tenant_id: r.try_get("tenant_id")?,
        id: r.try_get("id")?,
        app_id: r.try_get("app_node_id")?,
        role: r.try_get("role")?,
        grantee,
        granted_by: Actor {
            actor: crate::actor::PrincipalRef {
                kind: r.try_get("granted_kind")?,
                id: crate::ids::PrincipalId(r.try_get("granted_by")?),
            },
            on_behalf_of: r.try_get::<Option<Uuid>, _>("granted_obo")?.map(UserId),
        },
        created_at: r.try_get("created_at")?,
    })
}

/// The stored kind, user ID and email of a grantee.
fn grantee_columns(g: &Grantee) -> (&'static str, Option<Uuid>, Option<String>) {
    match g {
        Grantee::User { id } => ("user", Some(id.0), None),
        Grantee::Email { email } => ("email", None, Some(email.clone())),
        Grantee::TenantMembers => ("tenant_members", None, None),
    }
}

fn unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.code().as_deref() == Some("23505"))
}

fn fk_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.code().as_deref() == Some("23503"))
}

async fn journal(
    conn: &mut sqlx::PgConnection,
    ctx: &Context,
    app: NodeId,
    event: EventType,
    data: Value,
) -> Result<ChangeInfo> {
    let e = append(
        conn,
        ctx,
        NewEntry {
            node_id: Some(app),
            revision: None,
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

impl PgRoleStore {
    /// Stores the updates in the outbox of `tx` (nothing for none).
    async fn enqueue(
        conn: &mut sqlx::PgConnection,
        tenant: TenantId,
        updates: &[RelationshipUpdate],
    ) -> Result<Option<i64>> {
        if updates.is_empty() {
            return Ok(None);
        }
        Ok(Some(outbox::enqueue(conn, tenant, updates).await?))
    }

    async fn after_commit(&self, authz: &dyn Authz, tenant: TenantId, id: Option<i64>) {
        if let Some(id) = id {
            outbox::apply_after_commit(&self.pool, authz, tenant, &[id]).await;
        }
    }
}

#[async_trait]
impl RoleStore for PgRoleStore {
    async fn add(
        &self,
        ctx: &Context,
        authz: &dyn Authz,
        grant: NewRoleGrant,
    ) -> Result<Written<RoleGrant>> {
        let mut tx = begin_tenant(&self.pool, ctx.tenant).await?;
        let (kind, user, email) = grantee_columns(&grant.stored_grantee());
        let inserted = sqlx::query(sqlx::AssertSqlSafe(format!(
            "insert into app_role_grants (tenant_id, id, app_node_id, role, grantee_kind, \
             grantee_id, grantee_email, granted_kind, granted_by, granted_obo) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) returning {COLUMNS}"
        )))
        .bind(ctx.tenant)
        .bind(grant.id)
        .bind(grant.app_id)
        .bind(&grant.role)
        .bind(kind)
        .bind(user)
        .bind(email)
        .bind(ctx.actor.actor.kind)
        .bind(ctx.actor.actor.id.0)
        .bind(ctx.actor.on_behalf_of.map(|u| u.0))
        .fetch_one(&mut *tx)
        .await;
        let row = match inserted {
            Ok(r) => grant_row(&r)?,
            Err(e) if unique_violation(&e) => return Err(duplicate()),
            Err(e) if fk_violation(&e) => return Err(Error::not_found("app")),
            Err(e) => return Err(e.into()),
        };
        let change = journal(
            &mut tx,
            ctx,
            row.app_id,
            EventType::AppRoleGranted,
            event_data(&row),
        )
        .await?;
        let updates: Vec<RelationshipUpdate> = row
            .tuple()
            .into_iter()
            .map(RelationshipUpdate::Touch)
            .collect();
        let ob = Self::enqueue(&mut tx, ctx.tenant, &updates).await?;
        tx.commit().await?;
        self.after_commit(authz, ctx.tenant, ob).await;
        Ok(Written { value: row, change })
    }

    async fn get(&self, tenant: TenantId, app: NodeId, id: Uuid) -> Result<Option<RoleGrant>> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {COLUMNS} from app_role_grants \
             where tenant_id = $1 and app_node_id = $2 and id = $3"
        )))
        .bind(tenant)
        .bind(app)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.as_ref().map(grant_row).transpose()
    }

    async fn remove(
        &self,
        ctx: &Context,
        authz: &dyn Authz,
        app: NodeId,
        id: Uuid,
    ) -> Result<Option<Written<RoleGrant>>> {
        let mut tx = begin_tenant(&self.pool, ctx.tenant).await?;
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "delete from app_role_grants where tenant_id = $1 and app_node_id = $2 and id = $3 \
             returning {COLUMNS}"
        )))
        .bind(ctx.tenant)
        .bind(app)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row.as_ref().map(grant_row).transpose()? else {
            tx.rollback().await?;
            return Ok(None);
        };
        let change = journal(
            &mut tx,
            ctx,
            app,
            EventType::AppRoleRevoked,
            event_data(&row),
        )
        .await?;
        let updates: Vec<RelationshipUpdate> = row
            .tuple()
            .into_iter()
            .map(RelationshipUpdate::Delete)
            .collect();
        let ob = Self::enqueue(&mut tx, ctx.tenant, &updates).await?;
        tx.commit().await?;
        self.after_commit(authz, ctx.tenant, ob).await;
        Ok(Some(Written { value: row, change }))
    }

    async fn list(
        &self,
        tenant: TenantId,
        app: NodeId,
        role: Option<&str>,
        page: PageRequest,
    ) -> Result<Page<RoleGrant>> {
        let after = page.cursor.as_ref().map(parse_grant_cursor).transpose()?;
        let limit = page.limit() as usize;
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {COLUMNS} from app_role_grants \
             where tenant_id = $1 and app_node_id = $2 \
             and ($3::text is null or role = $3) and ($4::uuid is null or id > $4) \
             order by id limit $5"
        )))
        .bind(tenant)
        .bind(app)
        .bind(role)
        .bind(after)
        .bind(limit as i64 + 1)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let mut items = rows.iter().map(grant_row).collect::<Result<Vec<_>>>()?;
        let next_cursor = if items.len() > limit {
            items.truncate(limit);
            items.last().map(|g| grant_cursor(g.id))
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }

    async fn counts(&self, tenant: TenantId, app: NodeId) -> Result<BTreeMap<String, u32>> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "select role, count(*) from app_role_grants \
             where tenant_id = $1 and app_node_id = $2 group by role",
        )
        .bind(tenant)
        .bind(app)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows
            .into_iter()
            .map(|(r, n)| (r, n.clamp(0, i64::from(u32::MAX)) as u32))
            .collect())
    }

    async fn bind_email(
        &self,
        ctx: &Context,
        authz: &dyn Authz,
        email: &str,
        user: UserId,
    ) -> Result<usize> {
        let mut tx = begin_tenant(&self.pool, ctx.tenant).await?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {COLUMNS} from app_role_grants where tenant_id = $1 \
             and grantee_kind = 'email' and lower(grantee_email) = lower($2) order by id for update"
        )))
        .bind(ctx.tenant)
        .bind(email)
        .fetch_all(&mut *tx)
        .await?;
        let grants = rows.iter().map(grant_row).collect::<Result<Vec<_>>>()?;
        let mut updates = Vec::new();
        for g in &grants {
            journal(
                &mut tx,
                ctx,
                g.app_id,
                EventType::AppRoleRevoked,
                event_data(g),
            )
            .await?;
            let held: bool = sqlx::query_scalar(
                "select exists (select 1 from app_role_grants where tenant_id = $1 \
                 and app_node_id = $2 and role = $3 and grantee_kind = 'user' and grantee_id = $4)",
            )
            .bind(ctx.tenant)
            .bind(g.app_id)
            .bind(&g.role)
            .bind(user.0)
            .fetch_one(&mut *tx)
            .await?;
            if held {
                sqlx::query("delete from app_role_grants where tenant_id = $1 and id = $2")
                    .bind(ctx.tenant)
                    .bind(g.id)
                    .execute(&mut *tx)
                    .await?;
                continue;
            }
            sqlx::query(
                "update app_role_grants set grantee_kind = 'user', grantee_id = $3, \
                 grantee_email = null where tenant_id = $1 and id = $2",
            )
            .bind(ctx.tenant)
            .bind(g.id)
            .bind(user.0)
            .execute(&mut *tx)
            .await?;
            let mut b = g.clone();
            b.grantee = Grantee::User { id: user };
            journal(
                &mut tx,
                ctx,
                g.app_id,
                EventType::AppRoleGranted,
                event_data(&b),
            )
            .await?;
            updates.extend(b.tuple().map(RelationshipUpdate::Touch));
        }
        let ob = Self::enqueue(&mut tx, ctx.tenant, &updates).await?;
        tx.commit().await?;
        self.after_commit(authz, ctx.tenant, ob).await;
        Ok(grants.len())
    }
}
