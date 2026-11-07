//! The Postgres sharing store: tables `grants`, `share_links` and `share_index`, relationships
//! through the outbox (`authz_outbox`), and the journal in the same transaction.
//!
//! Every write runs in one transaction that sets `ruum.tenant_id` (row-level security), changes
//! the rows, enqueues the relationship updates ([`crate::authz::outbox::enqueue`]) and appends
//! the journal entry ([`crate::journal::postgres::append`]). After the commit the outbox rows are
//! applied to the engine ([`crate::authz::outbox::apply_after_commit`]); the worker retries
//! what fails. Permission checks run on the engine before the transaction.
//!
//! Node responses come from the node store ([`NodeStore::get`]), so the nodes area owns the
//! `Node` shape.

use super::memory::{is_inside, share_link_created};
use super::rules;
use super::*;
use crate::actor::{PrincipalKind, PrincipalRef};
use crate::authz::{self, Authz, Permission, RelationshipUpdate, outbox, tuples};
use crate::error::{Error, ErrorCode};
use crate::ids::PrincipalId;
use crate::journal::{ChangeInfo, EventType, NewEntry};
use crate::nodes::NodeStore;
use crate::tenants::{MemberRole, TenantPolicy};
use serde_json::json;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Postgres, Row, Transaction};
use std::sync::Arc;

/// The Postgres store.
pub struct PgSharingStore {
    pool: PgPool,
    authz: Arc<dyn Authz>,
    nodes: Arc<dyn NodeStore>,
}

impl std::fmt::Debug for PgSharingStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgSharingStore").finish_non_exhaustive()
    }
}

/// Starts a transaction in the tenant of `ctx` (and its user, for `share_index`).
pub(crate) async fn begin(pool: &PgPool, ctx: &Context) -> Result<Transaction<'static, Postgres>> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "select set_config('ruum.tenant_id', $1, true), set_config('ruum.user_id', $2, true)",
    )
    .bind(ctx.tenant.0.to_string())
    .bind(
        ctx.actor
            .effective_user()
            .map(|u| u.0.to_string())
            .unwrap_or_default(),
    )
    .execute(&mut *tx)
    .await?;
    Ok(tx)
}

async fn begin_system(pool: &PgPool) -> Result<Transaction<'static, Postgres>> {
    let mut tx = pool.begin().await?;
    sqlx::query("set local ruum.system = 'on'")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

fn role_of_text(s: &str) -> Result<Role> {
    serde_json::from_value(json!(s)).map_err(|e| Error::Internal(format!("bad role {s}: {e}")))
}

fn link_role_of_text(s: &str) -> Result<ShareLinkRole> {
    serde_json::from_value(json!(s)).map_err(|e| Error::Internal(format!("bad link role {s}: {e}")))
}

fn role_text(r: Role) -> &'static str {
    match r {
        Role::View => "view",
        Role::Comment => "comment",
        Role::Edit => "edit",
        Role::Manage => "manage",
    }
}

fn actor_of(kind: PrincipalKind, by: uuid::Uuid, obo: Option<uuid::Uuid>) -> Actor {
    Actor {
        actor: PrincipalRef {
            kind,
            id: PrincipalId(by),
        },
        on_behalf_of: obo.map(UserId),
    }
}

macro_rules! grant_columns {
    () => {
        "id, node_id, grantee_kind::text as grantee_kind, grantee_user_id, grantee_email, \
         role::text as role, created_at, created_kind, created_by, created_obo"
    };
}

fn grant_of(row: &PgRow) -> Result<Grant> {
    let grantee = match row.get::<String, _>("grantee_kind").as_str() {
        "user" => Grantee::User {
            id: UserId(row.get("grantee_user_id")),
        },
        "email" => Grantee::Email {
            email: row.get("grantee_email"),
        },
        _ => Grantee::TenantMembers,
    };
    Ok(Grant {
        id: GrantId(row.get("id")),
        node_id: NodeId(row.get("node_id")),
        grantee,
        role: role_of_text(row.get("role"))?,
        inherited_from: None,
        created_by: actor_of(
            row.get("created_kind"),
            row.get("created_by"),
            row.get("created_obo"),
        ),
        created_at: row.get("created_at"),
    })
}

macro_rules! link_columns {
    () => {
        "id, tenant_id, node_id, role::text as role, password_hash, expires_at, created_at, \
         created_kind, created_by, created_obo, revoked_at"
    };
}

fn link_of(row: &PgRow) -> Result<ShareLink> {
    Ok(ShareLink {
        id: ShareLinkId(row.get("id")),
        node_id: NodeId(row.get("node_id")),
        role: link_role_of_text(row.get("role"))?,
        token: None,
        url: None,
        expires_at: row.get("expires_at"),
        has_password: row.get::<Option<String>, _>("password_hash").is_some(),
        created_by: actor_of(
            row.get("created_kind"),
            row.get("created_by"),
            row.get("created_obo"),
        ),
        created_at: row.get("created_at"),
        revoked_at: row.get("revoked_at"),
    })
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.code().as_deref() == Some("23505"))
}

fn is_fk_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.code().as_deref() == Some("23503"))
}

/// The live node row of the tenant: `(revision, editors_can_share)`, locked for the
/// transaction. `not_found` for a missing, trashed or other tenant's node.
async fn lock_node(
    conn: &mut PgConnection,
    tenant: TenantId,
    node: NodeId,
) -> Result<(i64, Option<bool>)> {
    sqlx::query_as(
        "select revision, editors_can_share from nodes \
         where tenant_id = $1 AND id = $2 and trashed_at is null for share",
    )
    .bind(tenant.0)
    .bind(node.0)
    .fetch_optional(conn)
    .await?
    .ok_or_else(|| Error::not_found("node"))
}

/// The tenant policy from `tenants.settings` (missing keys take the defaults).
pub(crate) async fn policy_of(conn: &mut PgConnection, tenant: TenantId) -> Result<TenantPolicy> {
    let v: Option<serde_json::Value> =
        sqlx::query_scalar("select settings from tenants where id = $1")
            .bind(tenant.0)
            .fetch_optional(conn)
            .await?;
    let v = v.ok_or_else(|| Error::not_found("tenant"))?;
    Ok(serde_json::from_value(v).unwrap_or_default())
}

async fn member_role(
    conn: &mut PgConnection,
    tenant: TenantId,
    user: UserId,
) -> Result<Option<MemberRole>> {
    Ok(
        sqlx::query_scalar("select role from memberships where tenant_id = $1 AND user_id = $2")
            .bind(tenant.0)
            .bind(user.0)
            .fetch_optional(conn)
            .await?,
    )
}

async fn append(
    conn: &mut PgConnection,
    ctx: &Context,
    node: NodeId,
    revision: i64,
    event: EventType,
    data: serde_json::Value,
) -> Result<ChangeInfo> {
    let e = crate::journal::postgres::append(
        conn,
        ctx,
        NewEntry {
            node_id: Some(node),
            revision: Some(revision),
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

fn grant_subject(tenant: TenantId, grantee: &Grantee) -> Option<authz::SubjectRef> {
    match grantee {
        Grantee::User { id } => Some(tuples::grant_subject_user(*id)),
        Grantee::TenantMembers => Some(tuples::grant_subject_members(tenant)),
        Grantee::Email { .. } => None,
    }
}

/// Binds the email grants of `email` in the tenant to `user`: the invitation token was accepted,
/// or the user signed in with that email as a verified email of a trusted provider. Runs in the
/// caller's transaction (which has set `ruum.tenant_id`); returns the outbox row to apply after
/// the commit, if any. A grant whose node already has a grant to the user stays an email grant.
pub async fn bind_email_grants(
    conn: &mut PgConnection,
    tenant: TenantId,
    email: &str,
    user: UserId,
) -> Result<Option<i64>> {
    let rows = sqlx::query(concat!("update grants g set grantee_kind = 'user', grantee_user_id = $3, grantee_email = null \
         where g.tenant_id = $1 AND g.grantee_kind = 'email' and lower(g.grantee_email) = lower($2) \
         and not exists (select 1 from grants o where o.tenant_id = g.tenant_id \
             and o.node_id = g.node_id and o.grantee_kind = 'user' and o.grantee_user_id = $3) \
         returning ", grant_columns!(), ""))
    .bind(tenant.0)
    .bind(email)
    .bind(user.0)
    .fetch_all(&mut *conn)
    .await?;
    let mut updates = Vec::new();
    for row in &rows {
        let g = grant_of(row)?;
        sqlx::query(
            "insert into share_index (tenant_id, principal_kind, principal_id, node_id, grant_id, \
             role, shared_at, shared_kind, shared_by, shared_obo) \
             values ($1, 'user', $2, $3, $4, $5::grant_role, $6, $7, $8, $9) \
             on conflict (tenant_id, principal_id, node_id) do nothing",
        )
        .bind(tenant.0)
        .bind(user.0)
        .bind(g.node_id.0)
        .bind(g.id.0)
        .bind(role_text(g.role))
        .bind(g.created_at)
        .bind(g.created_by.actor.kind)
        .bind(g.created_by.actor.id.0)
        .bind(g.created_by.on_behalf_of.map(|u| u.0))
        .execute(&mut *conn)
        .await?;
        updates.push(RelationshipUpdate::Touch(tuples::grant(
            g.node_id,
            g.role,
            tuples::grant_subject_user(user),
        )));
    }
    if updates.is_empty() {
        return Ok(None);
    }
    Ok(Some(outbox::enqueue(conn, tenant, &updates).await?))
}

/// The sharing part of `removeMember` (`TenantStore::remove_member`), in the caller's
/// transaction (which has set `ruum.tenant_id`): removes the user's direct grants in the tenant
/// (`grant.removed` each) and revokes the share links that the user or the user's agents made
/// there (`share_link.revoked` each). Grants and member links the user made for others stay.
/// Returns the outbox row to apply after the commit, if any. `ctx` is the admin's context.
pub async fn remove_member_sharing(
    conn: &mut PgConnection,
    ctx: &Context,
    user: UserId,
) -> Result<Option<i64>> {
    let mut updates = Vec::new();
    let grants = sqlx::query(concat!(
        "delete from grants where tenant_id = $1 AND grantee_kind = 'user' \
         and grantee_user_id = $2 returning ",
        grant_columns!()
    ))
    .bind(ctx.tenant.0)
    .bind(user.0)
    .fetch_all(&mut *conn)
    .await?;
    for row in &grants {
        let g = grant_of(row)?;
        let revision = node_revision(conn, ctx.tenant, g.node_id).await?;
        append(
            conn,
            ctx,
            g.node_id,
            revision,
            EventType::GrantRemoved,
            json!({ "grant_id": g.id, "grantee": g.grantee, "role": g.role }),
        )
        .await?;
        updates.push(RelationshipUpdate::Delete(tuples::grant(
            g.node_id,
            g.role,
            tuples::grant_subject_user(user),
        )));
    }
    let links = sqlx::query(concat!(
        "update share_links set revoked_at = now(), revoked_kind = $3, revoked_by = $4, \
         revoked_obo = $5 where tenant_id = $1 AND coalesce(created_obo, created_by) = $2 \
         and revoked_at is null returning ",
        link_columns!()
    ))
    .bind(ctx.tenant.0)
    .bind(user.0)
    .bind(ctx.actor.actor.kind)
    .bind(ctx.actor.actor.id.0)
    .bind(ctx.actor.on_behalf_of.map(|u| u.0))
    .fetch_all(&mut *conn)
    .await?;
    for row in &links {
        let l = link_of(row)?;
        let revision = node_revision(conn, ctx.tenant, l.node_id).await?;
        append(
            conn,
            ctx,
            l.node_id,
            revision,
            EventType::ShareLinkRevoked,
            json!({ "share_link_id": l.id }),
        )
        .await?;
        updates.push(RelationshipUpdate::Delete(tuples::share_link(
            l.node_id, l.id, l.role,
        )));
    }
    if updates.is_empty() {
        return Ok(None);
    }
    Ok(Some(outbox::enqueue(conn, ctx.tenant, &updates).await?))
}

async fn node_revision(conn: &mut PgConnection, tenant: TenantId, node: NodeId) -> Result<i64> {
    Ok(
        sqlx::query_scalar("select revision from nodes where tenant_id = $1 AND id = $2")
            .bind(tenant.0)
            .bind(node.0)
            .fetch_optional(conn)
            .await?
            .unwrap_or(0),
    )
}

/// Queues the share email of a new grant in the grant's transaction (decision 0003-3).
/// Returns whether a row was queued: not for `tenant_members`, not for a user without a
/// verified email, not to the caller themselves, not when the recipient got a mail for the
/// node in the last 10 minutes, and not over the mail limits (50 per hour per sharing user
/// and tenant, 500 per tenant). The grant is made in every case.
async fn queue_share_mail(
    conn: &mut PgConnection,
    ctx: &Context,
    node: NodeId,
    grantee: &Grantee,
    role: Role,
    message: Option<&str>,
) -> Result<bool> {
    use crate::mailing::{self as m, ShareFields, outbox, rate};
    let Some(sender) = ctx.actor.effective_user() else {
        return Ok(false);
    };
    let (to, email_grant) = match grantee {
        Grantee::Email { email } => (email.clone(), true),
        Grantee::User { id } => {
            let to: Option<String> = sqlx::query_scalar(
                "select email from users where id = $1 and email_verified and deleted_at is null",
            )
            .bind(id.0)
            .fetch_optional(&mut *conn)
            .await?;
            match to {
                Some(to) if !to.is_empty() => (to, false),
                _ => return Ok(false),
            }
        }
        Grantee::TenantMembers => return Ok(false),
    };
    let me: Option<(String, String, bool)> =
        sqlx::query_as("select display_name, email, email_verified from users where id = $1")
            .bind(sender.0)
            .fetch_optional(&mut *conn)
            .await?;
    let (sharer, sender_email, verified) = me.unwrap_or_default();
    if to.eq_ignore_ascii_case(&sender_email) {
        return Ok(false);
    }
    if outbox::recent_share(conn, ctx.tenant, &to, node.0, m::SHARE_MAIL_REPEAT_SECONDS).await? {
        return Ok(false);
    }
    let user_key = m::share_mail_user_key(sender.0);
    let w = m::SHARE_MAIL_WINDOW_SECONDS;
    if rate::peek(conn, ctx.tenant, &user_key, w).await? >= m::SHARE_MAIL_PER_USER
        || rate::peek(conn, ctx.tenant, m::SHARE_MAIL_TENANT_KEY, w).await?
            >= m::SHARE_MAIL_PER_TENANT
    {
        return Ok(false);
    }
    rate::hit(conn, ctx.tenant, &user_key, w).await?;
    rate::hit(conn, ctx.tenant, m::SHARE_MAIL_TENANT_KEY, w).await?;
    let (item, kind): (String, String) =
        sqlx::query_as("select name, kind::text from nodes where tenant_id = $1 AND id = $2")
            .bind(ctx.tenant.0)
            .bind(node.0)
            .fetch_one(&mut *conn)
            .await?;
    let fields = ShareFields {
        sharer,
        item,
        folder: kind == "folder",
        role,
        message: message.map(str::to_string),
        email_grant,
        reply_to: verified.then_some(sender_email),
    };
    outbox::queue(
        conn,
        ctx.tenant,
        &to,
        Some(sender.0),
        &outbox::Queued::Share {
            node_id: node.0,
            fields,
        },
    )
    .await?;
    Ok(true)
}

impl PgSharingStore {
    pub fn new(pool: PgPool, authz: Arc<dyn Authz>, nodes: Arc<dyn NodeStore>) -> Self {
        Self { pool, authz, nodes }
    }

    async fn after_commit(&self, tenant: TenantId, ids: &[i64]) {
        outbox::apply_after_commit(&self.pool, &*self.authz, tenant, ids).await;
    }

    /// Counts one `addGrant` call against the limits, in its own committed transaction, and
    /// refuses it over the limit (decision 0003-2).
    async fn count_add_grant(&self, ctx: &Context) -> Result<()> {
        use crate::mailing::{self as m, rate};
        let user = ctx
            .actor
            .effective_user()
            .map(|u| u.0)
            .unwrap_or(ctx.actor.actor.id.0);
        let mut tx = begin(&self.pool, ctx).await?;
        let mine = rate::hit(
            &mut tx,
            ctx.tenant,
            &m::add_grant_user_key(user),
            m::ADD_GRANT_WINDOW_SECONDS,
        )
        .await?;
        let all = rate::hit(
            &mut tx,
            ctx.tenant,
            m::ADD_GRANT_TENANT_KEY,
            m::ADD_GRANT_WINDOW_SECONDS,
        )
        .await?;
        tx.commit().await?;
        if mine > m::ADD_GRANT_PER_USER || all > m::ADD_GRANT_PER_TENANT {
            return Err(m::add_grant_rate_limited());
        }
        Ok(())
    }

    /// Loads a grant and locks its row until the commit (`for update`), so two concurrent
    /// changes of one grant run one after the other and the second sees the role the first
    /// wrote. The role checks ([`rules::check_change`]) then run on the locked role.
    async fn load_grant(
        &self,
        conn: &mut PgConnection,
        tenant: TenantId,
        node: NodeId,
        grant: GrantId,
    ) -> Result<Grant> {
        let row = sqlx::query(concat!(
            "select ",
            grant_columns!(),
            " from grants where tenant_id = $1 AND node_id = $2 and id = $3 for update"
        ))
        .bind(tenant.0)
        .bind(node.0)
        .bind(grant.0)
        .fetch_optional(conn)
        .await?
        .ok_or_else(|| Error::not_found("grant"))?;
        grant_of(&row)
    }
}

#[async_trait]
impl SharingStore for PgSharingStore {
    async fn list_grants(
        &self,
        ctx: &Context,
        node: NodeId,
        page: PageRequest,
    ) -> Result<Page<Grant>> {
        rules::refuse_share_links(ctx)?;
        authz::require(&*self.authz, &*self.nodes, ctx, node, Permission::View).await?;
        let mut on: Vec<uuid::Uuid> = self
            .nodes
            .ancestors(ctx, node)
            .await?
            .into_iter()
            .map(|n| n.0)
            .collect();
        on.push(node.0);
        let after = page
            .cursor
            .as_ref()
            .map(rules::parse_id_cursor)
            .transpose()?;
        let mut tx = begin(&self.pool, ctx).await?;
        lock_node(&mut tx, ctx.tenant, node).await?;
        let rows = sqlx::query(concat!(
            "select ",
            grant_columns!(),
            " from grants where tenant_id = $1 AND node_id = any($2) \
             and ($3::uuid is null or id > $3) order by id limit $4"
        ))
        .bind(ctx.tenant.0)
        .bind(&on)
        .bind(after)
        .bind(i64::from(page.limit()) + 1)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let items = rows
            .iter()
            .map(|r| {
                let mut g = grant_of(r)?;
                g.inherited_from = (g.node_id != node).then_some(g.node_id);
                Ok(g)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(super::memory::page_of(items, page.limit(), |g| g.id.0))
    }

    async fn share(&self, ctx: &Context, node: NodeId, req: NewGrant) -> Result<GrantAdded> {
        let NewGrant {
            grantee,
            role,
            notify,
            message,
        } = req;
        // The limits count every call, also refused ones, in their own committed transaction
        // (decision 0003-2), so probing with refused calls is limited too.
        self.count_add_grant(ctx).await?;
        rules::check_grant(&*self.authz, &*self.nodes, ctx, node, role).await?;
        let email = rules::check_grantee(&grantee)?;
        let grantee = match email {
            Some(email) => Grantee::Email { email },
            None => grantee,
        };
        let mut tx = begin(&self.pool, ctx).await?;
        let (revision, _) = lock_node(&mut tx, ctx.tenant, node).await?;
        let policy = policy_of(&mut tx, ctx.tenant).await?;
        let caller_role = match ctx.actor.effective_user() {
            Some(u) => member_role(&mut tx, ctx.tenant, u).await?,
            None => None,
        };
        let grantee = match grantee {
            Grantee::Email { email } if is_inside(caller_role) => {
                // (a) A caller who may list the members gets a user grant for the verified
                // email of a person of the tenant; the answer tells them nothing new.
                let person: Option<uuid::Uuid> = sqlx::query_scalar(
                    "select m.user_id from memberships m join users u on u.id = m.user_id \
                     where m.tenant_id = $1 AND u.email_verified and u.deleted_at is null \
                     and lower(u.email) = lower($2) limit 1",
                )
                .bind(ctx.tenant.0)
                .bind(&email)
                .fetch_optional(&mut *tx)
                .await?;
                match person {
                    Some(u) => Grantee::User { id: UserId(u) },
                    None => Grantee::Email { email },
                }
            }
            Grantee::Email { email } => {
                // (a') A guest caller (or an agent of a guest) always gets an email grant, and
                // while guests are off the same refusal for every email, so a guest cannot
                // tell the email of a member from any other.
                if !policy.allow_guests {
                    return Err(rules::guests_refused());
                }
                Grantee::Email { email }
            }
            other => other,
        };
        let mut updates = Vec::new();
        match &grantee {
            Grantee::User { id } => {
                // A grant by user ID only reaches people already in the tenant (members and
                // guests). It never makes a guest: that would show a stranger's email to the
                // tenant (listMembers) and put the tenant in their list unasked. People from
                // outside come in through an email grant or an invitation, which bind by token
                // or verified email. An unknown ID and a user of another tenant get the same
                // answer, so the call tells nothing about other tenants' users.
                let role_in_tenant = member_role(&mut tx, ctx.tenant, *id).await?;
                if role_in_tenant.is_none() {
                    return Err(rules::no_such_user());
                }
                if !is_inside(role_in_tenant) && !policy.allow_guests {
                    return Err(rules::guests_refused());
                }
            }
            Grantee::Email { email } => {
                let inside: bool = sqlx::query_scalar(
                    "select exists (select 1 from memberships m join users u on u.id = m.user_id \
                     where m.tenant_id = $1 AND m.role <> 'guest' and u.email_verified \
                     and lower(u.email) = lower($2))",
                )
                .bind(ctx.tenant.0)
                .bind(email)
                .fetch_one(&mut *tx)
                .await?;
                if !inside && !policy.allow_guests {
                    return Err(rules::guests_refused());
                }
            }
            Grantee::TenantMembers => {}
        }
        let grant = Grant {
            id: GrantId::new(),
            node_id: node,
            grantee: grantee.clone(),
            role,
            inherited_from: None,
            created_by: ctx.actor,
            created_at: crate::now(),
        };
        let (kind, user, email) = match &grantee {
            Grantee::User { id } => ("user", Some(id.0), None),
            Grantee::Email { email } => ("email", None, Some(email.clone())),
            Grantee::TenantMembers => ("tenant_members", None, None),
        };
        let r = sqlx::query(
            "insert into grants (id, tenant_id, node_id, grantee_kind, grantee_user_id, \
             grantee_email, role, created_at, created_kind, created_by, created_obo) \
             values ($1, $2, $3, $4::grantee_kind, $5, $6, $7::grant_role, $8, $9, $10, $11)",
        )
        .bind(grant.id.0)
        .bind(ctx.tenant.0)
        .bind(node.0)
        .bind(kind)
        .bind(user)
        .bind(email)
        .bind(role_text(role))
        .bind(grant.created_at)
        .bind(ctx.actor.actor.kind)
        .bind(ctx.actor.actor.id.0)
        .bind(ctx.actor.on_behalf_of.map(|u| u.0))
        .execute(&mut *tx)
        .await;
        match r {
            Ok(_) => {}
            Err(e) if is_unique_violation(&e) => {
                return Err(Error::api(
                    ErrorCode::Conflict,
                    "this grantee already has a grant on the node",
                ));
            }
            Err(e) if is_fk_violation(&e) => return Err(rules::no_such_user()),
            Err(e) => return Err(e.into()),
        }
        if let Some(u) = user {
            sqlx::query(
                "insert into share_index (tenant_id, principal_kind, principal_id, node_id, \
                 grant_id, role, shared_at, shared_kind, shared_by, shared_obo) \
                 values ($1, 'user', $2, $3, $4, $5::grant_role, $6, $7, $8, $9)",
            )
            .bind(ctx.tenant.0)
            .bind(u)
            .bind(node.0)
            .bind(grant.id.0)
            .bind(role_text(role))
            .bind(grant.created_at)
            .bind(ctx.actor.actor.kind)
            .bind(ctx.actor.actor.id.0)
            .bind(ctx.actor.on_behalf_of.map(|u| u.0))
            .execute(&mut *tx)
            .await?;
        }
        if let Some(subject) = grant_subject(ctx.tenant, &grantee) {
            updates.push(RelationshipUpdate::Touch(tuples::grant(
                node, role, subject,
            )));
        }
        let change = append(
            &mut tx,
            ctx,
            node,
            revision,
            EventType::GrantAdded,
            rules::grant_added_data(grant.id, &grantee, role),
        )
        .await?;
        let notified = if notify {
            queue_share_mail(&mut tx, ctx, node, &grantee, role, message.as_deref()).await?
        } else {
            false
        };
        let ob = if updates.is_empty() {
            None
        } else {
            Some(outbox::enqueue(&mut tx, ctx.tenant, &updates).await?)
        };
        tx.commit().await?;
        if notified {
            crate::mailing::outbox::wake();
        }
        if let Some(id) = ob {
            self.after_commit(ctx.tenant, &[id]).await;
        }
        Ok(GrantAdded {
            written: Written {
                value: grant,
                change,
            },
            notified,
        })
    }

    async fn update_grant(
        &self,
        ctx: &Context,
        node: NodeId,
        grant: GrantId,
        role: Role,
    ) -> Result<Written<Grant>> {
        rules::refuse_share_links(ctx)?;
        authz::require(&*self.authz, &*self.nodes, ctx, node, Permission::View).await?;
        let mut tx = begin(&self.pool, ctx).await?;
        let (revision, _) = lock_node(&mut tx, ctx.tenant, node).await?;
        let old = self.load_grant(&mut tx, ctx.tenant, node, grant).await?;
        rules::check_change(&*self.authz, &*self.nodes, ctx, node, old.role, Some(role)).await?;
        sqlx::query(
            "update grants set role = $3::grant_role, updated_at = now(), updated_kind = $4, \
             updated_by = $5, updated_obo = $6 where tenant_id = $1 AND id = $2",
        )
        .bind(ctx.tenant.0)
        .bind(grant.0)
        .bind(role_text(role))
        .bind(ctx.actor.actor.kind)
        .bind(ctx.actor.actor.id.0)
        .bind(ctx.actor.on_behalf_of.map(|u| u.0))
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "update share_index set role = $3::grant_role where tenant_id = $1 AND grant_id = $2",
        )
        .bind(ctx.tenant.0)
        .bind(grant.0)
        .bind(role_text(role))
        .execute(&mut *tx)
        .await?;
        let change = append(
            &mut tx,
            ctx,
            node,
            revision,
            EventType::GrantUpdated,
            json!({
                "grant_id": grant, "grantee": old.grantee,
                "old_role": old.role, "new_role": role,
            }),
        )
        .await?;
        let ob = match grant_subject(ctx.tenant, &old.grantee) {
            Some(s) => Some(
                outbox::enqueue(
                    &mut tx,
                    ctx.tenant,
                    &tuples::change_grant(node, old.role, role, s),
                )
                .await?,
            ),
            None => None,
        };
        tx.commit().await?;
        if let Some(id) = ob {
            self.after_commit(ctx.tenant, &[id]).await;
        }
        let mut value = old;
        value.role = role;
        Ok(Written { value, change })
    }

    async fn remove_grant(
        &self,
        ctx: &Context,
        node: NodeId,
        grant: GrantId,
    ) -> Result<Written<()>> {
        rules::refuse_share_links(ctx)?;
        authz::require(&*self.authz, &*self.nodes, ctx, node, Permission::View).await?;
        let mut tx = begin(&self.pool, ctx).await?;
        let (revision, _) = lock_node(&mut tx, ctx.tenant, node).await?;
        let old = self.load_grant(&mut tx, ctx.tenant, node, grant).await?;
        rules::check_change(&*self.authz, &*self.nodes, ctx, node, old.role, None).await?;
        sqlx::query("delete from grants where tenant_id = $1 AND id = $2")
            .bind(ctx.tenant.0)
            .bind(grant.0)
            .execute(&mut *tx)
            .await?;
        let change = append(
            &mut tx,
            ctx,
            node,
            revision,
            EventType::GrantRemoved,
            json!({ "grant_id": grant, "grantee": old.grantee, "role": old.role }),
        )
        .await?;
        let ob = match grant_subject(ctx.tenant, &old.grantee) {
            Some(s) => Some(
                outbox::enqueue(
                    &mut tx,
                    ctx.tenant,
                    &[RelationshipUpdate::Delete(tuples::grant(node, old.role, s))],
                )
                .await?,
            ),
            None => None,
        };
        tx.commit().await?;
        if let Some(id) = ob {
            self.after_commit(ctx.tenant, &[id]).await;
        }
        Ok(Written { value: (), change })
    }

    async fn list_share_links(
        &self,
        ctx: &Context,
        node: NodeId,
        page: PageRequest,
    ) -> Result<Page<ShareLink>> {
        rules::refuse_share_links(ctx)?;
        authz::require(&*self.authz, &*self.nodes, ctx, node, Permission::Share).await?;
        let after = page
            .cursor
            .as_ref()
            .map(rules::parse_id_cursor)
            .transpose()?;
        let mut tx = begin(&self.pool, ctx).await?;
        lock_node(&mut tx, ctx.tenant, node).await?;
        let rows = sqlx::query(concat!(
            "select ",
            link_columns!(),
            " from share_links where tenant_id = $1 AND node_id = $2 \
             and ($3::uuid is null or id > $3) order by id limit $4"
        ))
        .bind(ctx.tenant.0)
        .bind(node.0)
        .bind(after)
        .bind(i64::from(page.limit()) + 1)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let items = rows.iter().map(link_of).collect::<Result<Vec<_>>>()?;
        Ok(super::memory::page_of(items, page.limit(), |l| l.id.0))
    }

    async fn create_share_link(
        &self,
        ctx: &Context,
        node: NodeId,
        req: NewShareLink,
    ) -> Result<Written<ShareLink>> {
        rules::check_grant(&*self.authz, &*self.nodes, ctx, node, req.role.as_role()).await?;
        let mut tx = begin(&self.pool, ctx).await?;
        let (revision, _) = lock_node(&mut tx, ctx.tenant, node).await?;
        let policy = policy_of(&mut tx, ctx.tenant).await?;
        rules::check_new_link(&policy, &req, crate::now())?;
        let token = rules::new_link_token();
        let password_hash = req
            .password
            .as_deref()
            .map(rules::hash_password)
            .transpose()?;
        let link = ShareLink {
            id: ShareLinkId::new(),
            node_id: node,
            role: req.role,
            token: None,
            url: None,
            expires_at: req.expires_at,
            has_password: password_hash.is_some(),
            created_by: ctx.actor,
            created_at: crate::now(),
            revoked_at: None,
        };
        sqlx::query(
            "insert into share_links (id, tenant_id, node_id, role, token_hash, password_hash, \
             expires_at, created_at, created_kind, created_by, created_obo) \
             values ($1, $2, $3, $4::grant_role, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(link.id.0)
        .bind(ctx.tenant.0)
        .bind(node.0)
        .bind(role_text(link.role.as_role()))
        .bind(rules::hash_token(&token))
        .bind(&password_hash)
        .bind(link.expires_at)
        .bind(link.created_at)
        .bind(ctx.actor.actor.kind)
        .bind(ctx.actor.actor.id.0)
        .bind(ctx.actor.on_behalf_of.map(|u| u.0))
        .execute(&mut *tx)
        .await?;
        let change = append(
            &mut tx,
            ctx,
            node,
            revision,
            EventType::ShareLinkCreated,
            share_link_created(&link),
        )
        .await?;
        let ob = outbox::enqueue(
            &mut tx,
            ctx.tenant,
            &[RelationshipUpdate::Touch(tuples::share_link(
                node, link.id, link.role,
            ))],
        )
        .await?;
        tx.commit().await?;
        self.after_commit(ctx.tenant, &[ob]).await;
        let mut value = link;
        value.token = Some(token);
        Ok(Written { value, change })
    }

    async fn revoke_share_link(&self, ctx: &Context, link: ShareLinkId) -> Result<Written<()>> {
        rules::refuse_share_links(ctx)?;
        let mut tx = begin(&self.pool, ctx).await?;
        let row = sqlx::query(concat!(
            "select ",
            link_columns!(),
            " from share_links where tenant_id = $1 AND id = $2 \
             and revoked_at is null for update"
        ))
        .bind(ctx.tenant.0)
        .bind(link.0)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::not_found("share link"))?;
        let l = link_of(&row)?;
        authz::require(
            &*self.authz,
            &*self.nodes,
            ctx,
            l.node_id,
            Permission::Share,
        )
        .await?;
        let (revision, _) = lock_node(&mut tx, ctx.tenant, l.node_id).await?;
        sqlx::query(
            "update share_links set revoked_at = now(), revoked_kind = $3, revoked_by = $4, \
             revoked_obo = $5 where tenant_id = $1 AND id = $2",
        )
        .bind(ctx.tenant.0)
        .bind(link.0)
        .bind(ctx.actor.actor.kind)
        .bind(ctx.actor.actor.id.0)
        .bind(ctx.actor.on_behalf_of.map(|u| u.0))
        .execute(&mut *tx)
        .await?;
        let change = append(
            &mut tx,
            ctx,
            l.node_id,
            revision,
            EventType::ShareLinkRevoked,
            json!({ "share_link_id": link }),
        )
        .await?;
        let ob = outbox::enqueue(
            &mut tx,
            ctx.tenant,
            &[RelationshipUpdate::Delete(tuples::share_link(
                l.node_id, link, l.role,
            ))],
        )
        .await?;
        tx.commit().await?;
        self.after_commit(ctx.tenant, &[ob]).await;
        Ok(Written { value: (), change })
    }

    async fn resolve_share_link(
        &self,
        token: &str,
        password: Option<&str>,
    ) -> Result<(TenantId, Node, ShareLink)> {
        // A token lookup is one of the few cross-tenant lookups (system mode).
        let token_hash = rules::hash_token(token);
        let mut tx = begin_system(&self.pool).await?;
        let row = sqlx::query(concat!(
            "select ",
            link_columns!(),
            " from share_links where token_hash = $1"
        ))
        .bind(&token_hash)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::not_found("share link"))?;
        let tenant = TenantId(row.get("tenant_id"));
        let phc: Option<String> = row.get("password_hash");
        let link = link_of(&row)?;
        let policy = policy_of(&mut tx, tenant).await?;
        tx.commit().await?;
        if !rules::link_is_open(&policy, &link, crate::now()) {
            return Err(Error::not_found("share link"));
        }
        if let Some(phc) = &phc {
            rules::check_link_password(&token_hash, phc, password)?;
        }
        let ctx = rules::link_context(tenant, &link);
        let node = self.nodes.get(&ctx, link.node_id).await?;
        Ok((tenant, node, link))
    }

    async fn shared_with_me(&self, ctx: &Context, page: PageRequest) -> Result<Page<SharedItem>> {
        rules::refuse_share_links(ctx)?;
        let Some(user) = ctx.actor.effective_user() else {
            return Ok(Page::empty());
        };
        let after = page
            .cursor
            .as_ref()
            .map(rules::parse_shared_cursor)
            .transpose()?;
        // Agents are bound to one tenant and see only its items.
        let only_tenant = ctx.scope.as_ref().map(|_| ctx.tenant.0);
        let mut tx = self.pool.begin().await?;
        sqlx::query("select set_config('ruum.user_id', $1, true)")
            .bind(user.0.to_string())
            .execute(&mut *tx)
            .await?;
        let rows: Vec<SharedRow> = sqlx::query_as(
            "select tenant_id, node_id, role::text, shared_at, shared_kind, shared_by, shared_obo \
             from share_index where principal_id = $1 \
             and ($2::uuid is null or tenant_id = $2) \
             and ($3::timestamptz is null or (shared_at, node_id) < ($3, $4)) \
             order by shared_at desc, node_id desc limit $5",
        )
        .bind(user.0)
        .bind(only_tenant)
        .bind(after.map(|a| a.0))
        .bind(after.map(|a| a.1.0))
        .bind(i64::from(page.limit()) + 1)
        .fetch_all(&mut *tx)
        .await?;
        // Grants to `tenant#member` are not in `share_index`: they are expanded here from the
        // user's memberships (member, admin or owner; guests are not `tenant#member`). The
        // grants of other tenants are only visible in system mode; the query is bound to the
        // user's own memberships.
        sqlx::query("set local ruum.system = 'on'")
            .execute(&mut *tx)
            .await?;
        let members: Vec<SharedRow> = sqlx::query_as(
            "select g.tenant_id, g.node_id, g.role::text, g.created_at, g.created_kind,              g.created_by, g.created_obo              from grants g join memberships m                on m.tenant_id = g.tenant_id and m.user_id = $1                and m.role in ('member', 'admin', 'owner')              where g.grantee_kind = 'tenant_members'              and ($2::uuid is null or g.tenant_id = $2)              and ($3::timestamptz is null or (g.created_at, g.node_id) < ($3, $4))              order by g.created_at desc, g.node_id desc limit $5",
        )
        .bind(user.0)
        .bind(only_tenant)
        .bind(after.map(|a| a.0))
        .bind(after.map(|a| a.1.0))
        .bind(i64::from(page.limit()) + 1)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let mut rows = rows;
        rows.extend(members);
        let mut rows = rules::one_per_node(
            rows,
            |r| (TenantId(r.0), NodeId(r.1)),
            |r| role_of_text(&r.2).unwrap_or(Role::View),
        );
        rows.sort_by_key(|r| std::cmp::Reverse((r.3, r.1)));

        let limit = page.limit() as usize;
        let more = rows.len() > limit;
        let mut items = Vec::new();
        let mut last = None;
        for r in rows.into_iter().take(limit) {
            let (tenant, node, role, at, kind, by, obo) = r;
            last = Some((at, NodeId(node)));
            let tctx = Context {
                tenant: TenantId(tenant),
                ..ctx.clone()
            };
            if !authz::in_scope_subtree(&*self.nodes, &tctx, NodeId(node)).await? {
                continue;
            }
            match self.nodes.get(&tctx, NodeId(node)).await {
                Ok(n) if n.trashed_at.is_none() => items.push(SharedItem {
                    node: n,
                    role: role_of_text(&role)?,
                    shared_by: actor_of(kind, by, obo),
                    shared_at: at,
                }),
                Ok(_) => {}
                Err(Error::Api(e)) if e.code == ErrorCode::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(Page {
            items,
            next_cursor: if more {
                last.map(|(at, n)| rules::shared_cursor(at, n))
            } else {
                None
            },
        })
    }

    async fn update_node_sharing(
        &self,
        ctx: &Context,
        node: NodeId,
        editors_can_share: Option<bool>,
        if_revision: Option<i64>,
    ) -> Result<Written<Node>> {
        rules::refuse_share_links(ctx)?;
        authz::require(&*self.authz, &*self.nodes, ctx, node, Permission::Manage).await?;
        let mut tx = begin(&self.pool, ctx).await?;
        let (revision, old): (i64, Option<bool>) = sqlx::query_as(
            "select revision, editors_can_share from nodes \
             where tenant_id = $1 AND id = $2 and trashed_at is null for update",
        )
        .bind(ctx.tenant.0)
        .bind(node.0)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::not_found("node"))?;
        if if_revision.is_some_and(|r| r != revision) {
            return Err(Error::api(
                ErrorCode::PreconditionFailed,
                "the node changed since you read it",
            ));
        }
        let revision: i64 = sqlx::query_scalar(
            "update nodes set editors_can_share = $3, revision = revision + 1, updated_at = now(), \
             updated_kind = $4, updated_by = $5, updated_obo = $6 \
             where tenant_id = $1 AND id = $2 returning revision",
        )
        .bind(ctx.tenant.0)
        .bind(node.0)
        .bind(editors_can_share)
        .bind(ctx.actor.actor.kind)
        .bind(ctx.actor.actor.id.0)
        .bind(ctx.actor.on_behalf_of.map(|u| u.0))
        .fetch_one(&mut *tx)
        .await?;
        let change = append(
            &mut tx,
            ctx,
            node,
            revision,
            EventType::NodeSharingUpdated,
            json!({ "old_editors_can_share": old, "new_editors_can_share": editors_can_share }),
        )
        .await?;
        let ob = outbox::enqueue(
            &mut tx,
            ctx.tenant,
            &tuples::editors_can_share(node, editors_can_share),
        )
        .await?;
        tx.commit().await?;
        self.after_commit(ctx.tenant, &[ob]).await;
        let value = self.nodes.get(ctx, node).await?;
        Ok(Written { value, change })
    }
}

type SharedRow = (
    uuid::Uuid,
    uuid::Uuid,
    String,
    OffsetDateTime,
    PrincipalKind,
    uuid::Uuid,
    Option<uuid::Uuid>,
);
