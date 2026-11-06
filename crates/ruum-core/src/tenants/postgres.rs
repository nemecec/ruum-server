//! The Postgres tenant store (tables `tenants`, `journal_counters`, `memberships`,
//! `invitations`; `removeMember` and `acceptInvitation` also touch `grants`, `share_links`,
//! `share_index`, `agents`, `agent_oauth_grants` and `app_passwords` in the same transaction).

use super::rules::{
    accepted_role, apply_policy, caller_user, check_member_limit, check_removal, check_role_change,
    require_role,
};
use super::*;
use crate::actor::{PrincipalKind, require_user_actor};
use crate::authz::{ObjectRef, Relationship, RelationshipUpdate, Role, SubjectRef};
use crate::error::{ApiError, Error, ErrorCode};
use crate::identity::postgres::{
    PgArea, TENANT_COLS, audit, enqueue, insert_owner_and_root, insert_tenant, offset, page,
    tenant_from, unique_to_conflict,
};
use crate::identity::{check_email, check_name, secret};
use crate::journal::{EventType, NewEntry};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

#[derive(Clone)]
pub struct PgTenantStore {
    area: PgArea,
}

impl PgTenantStore {
    pub fn new(area: PgArea) -> Self {
        Self { area }
    }
}

fn member_from(r: &PgRow) -> Result<Member> {
    Ok(Member {
        tenant_id: r.try_get("tenant_id")?,
        user_id: r.try_get("user_id")?,
        email: r.try_get("email")?,
        display_name: r.try_get("display_name")?,
        role: r.try_get("role")?,
        joined_at: r.try_get("joined_at")?,
    })
}

const MEMBER_SELECT: &str = "select m.tenant_id, m.user_id, u.email, u.display_name, m.role, \
     m.joined_at from memberships m join users u on u.id = m.user_id";

fn invitation_from(r: &PgRow) -> Result<Invitation> {
    let kind: crate::actor::PrincipalKind = r.try_get("created_kind")?;
    let by: Uuid = r.try_get("created_by")?;
    let obo: Option<UserId> = r.try_get("created_obo")?;
    Ok(Invitation {
        id: r.try_get("id")?,
        tenant_id: r.try_get("tenant_id")?,
        email: r.try_get("email")?,
        role: r.try_get("role")?,
        invited_by: Actor {
            actor: crate::actor::PrincipalRef {
                kind,
                id: crate::ids::PrincipalId(by),
            },
            on_behalf_of: obo,
        },
        created_at: r.try_get("created_at")?,
        expires_at: r.try_get("expires_at")?,
        accepted_at: r.try_get("accepted_at")?,
    })
}

const INVITATION_COLS: &str = "id, tenant_id, email, role, created_kind, created_by, created_obo, \
     created_at, expires_at, accepted_at";

async fn role_in(conn: &mut PgConnection, t: TenantId, u: UserId) -> Result<Option<MemberRole>> {
    Ok(
        sqlx::query_scalar("select role from memberships where tenant_id = $1 and user_id = $2")
            .bind(t)
            .bind(u)
            .fetch_optional(&mut *conn)
            .await?,
    )
}

async fn tenant_row(conn: &mut PgConnection, t: TenantId) -> Result<Tenant> {
    let r = sqlx::query(crate::sql!(
        "select {TENANT_COLS} from tenants where id = $1"
    ))
    .bind(t)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| Error::not_found("tenant"))?;
    tenant_from(&r)
}

async fn member_count(conn: &mut PgConnection, t: TenantId) -> Result<u32> {
    let n: i64 = sqlx::query_scalar(
        "select count(*) from memberships where tenant_id = $1 and role <> 'guest'",
    )
    .bind(t)
    .fetch_one(&mut *conn)
    .await?;
    Ok(n as u32)
}

async fn owner_count(conn: &mut PgConnection, t: TenantId) -> Result<u32> {
    let n: i64 = sqlx::query_scalar(
        "select count(*) from memberships where tenant_id = $1 and role = 'owner'",
    )
    .bind(t)
    .fetch_one(&mut *conn)
    .await?;
    Ok(n as u32)
}

async fn member(conn: &mut PgConnection, t: TenantId, u: UserId) -> Result<Member> {
    let r = sqlx::query(crate::sql!(
        "{MEMBER_SELECT} where m.tenant_id = $1 and m.user_id = $2"
    ))
    .bind(t)
    .bind(u)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| Error::not_found("member"))?;
    member_from(&r)
}

fn role_from_text(s: &str) -> Result<Role> {
    serde_json::from_value(serde_json::Value::String(s.to_string()))
        .map_err(|_| Error::Internal(format!("unknown role {s}")))
}

fn node_tuple(node: Uuid, relation: &str, subject_type: &str, subject: Uuid) -> Relationship {
    Relationship {
        object: ObjectRef {
            object_type: "node".into(),
            object_id: node.to_string(),
        },
        relation: relation.to_string(),
        subject: SubjectRef {
            object: ObjectRef {
                object_type: subject_type.into(),
                object_id: subject.to_string(),
            },
            relation: None,
        },
    }
}

async fn node_revision(conn: &mut PgConnection, t: TenantId, node: Uuid) -> Result<Option<i64>> {
    Ok(
        sqlx::query_scalar("select revision from nodes where tenant_id = $1 and id = $2")
            .bind(t)
            .bind(node)
            .fetch_optional(&mut *conn)
            .await?,
    )
}

/// Switches a transaction that started as a system lookup to tenant `t` and `user`, so the
/// writes that follow are under the row-level security of that tenant.
pub(crate) async fn leave_system(conn: &mut PgConnection, t: TenantId, user: UserId) -> Result<()> {
    sqlx::query(
        "select set_config('ruum.system', 'off', true), set_config('ruum.tenant_id', $1, true), \
         set_config('ruum.user_id', $2, true)",
    )
    .bind(t.to_string())
    .bind(user.to_string())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Binds the email grants of `email` in `ctx.tenant` to `user` (an accepted invitation, or a
/// sign-in with that email as a verified email of a trusted provider), in the caller's
/// transaction, which must be in `ctx.tenant` (not a system transaction).
///
/// Each email grant gets `grant.removed` (the email grantee); then either it becomes a grant to
/// the user (`grant.added`, same grant ID, a `share_index` row and the tuple), or, when the user
/// already has a direct grant on that node, the email grant is deleted and the user's grant is
/// raised to the higher of the two roles (`grant.updated` when it changes). Returns the tuples
/// to write and the number of email grants bound.
pub(crate) async fn bind_email_grants(
    conn: &mut PgConnection,
    ctx: &Context,
    email: &str,
    user: UserId,
) -> Result<(Vec<RelationshipUpdate>, usize)> {
    let t = ctx.tenant;
    let mut tuples = Vec::new();
    let rows = sqlx::query(
        "select id, node_id, role::text as role from grants where tenant_id = $1 \
         and grantee_kind = 'email' and lower(grantee_email) = lower($2) order by id for update",
    )
    .bind(t)
    .bind(email)
    .fetch_all(&mut *conn)
    .await?;
    for g in &rows {
        let id: Uuid = g.try_get("id")?;
        let node: Uuid = g.try_get("node_id")?;
        let role = role_from_text(&g.try_get::<String, _>("role")?)?;
        let revision = node_revision(conn, t, node).await?;
        let node_id = Some(crate::ids::NodeId(node));
        crate::journal::postgres::append(
            conn,
            ctx,
            NewEntry {
                node_id,
                revision,
                event: EventType::GrantRemoved,
                data: serde_json::json!({
                    "grant_id": id,
                    "grantee": { "type": "email", "email": email.to_lowercase() },
                    "role": role,
                }),
            },
        )
        .await?;
        let existing = sqlx::query(
            "select id, role::text as role from grants where tenant_id = $1 and node_id = $2 \
             and grantee_kind = 'user' and grantee_user_id = $3 for update",
        )
        .bind(t)
        .bind(node)
        .bind(user)
        .fetch_optional(&mut *conn)
        .await?;
        match existing {
            Some(u) => {
                let user_grant: Uuid = u.try_get("id")?;
                let old = role_from_text(&u.try_get::<String, _>("role")?)?;
                sqlx::query("delete from grants where tenant_id = $1 and id = $2")
                    .bind(t)
                    .bind(id)
                    .execute(&mut *conn)
                    .await?;
                if role > old {
                    let text = serde_json::to_value(role)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default();
                    sqlx::query(
                        "update grants set role = $3::grant_role where tenant_id = $1 and id = $2",
                    )
                    .bind(t)
                    .bind(user_grant)
                    .bind(&text)
                    .execute(&mut *conn)
                    .await?;
                    sqlx::query(
                        "update share_index set role = $3::grant_role \
                         where tenant_id = $1 and grant_id = $2",
                    )
                    .bind(t)
                    .bind(user_grant)
                    .bind(&text)
                    .execute(&mut *conn)
                    .await?;
                    tuples.push(RelationshipUpdate::Delete(node_tuple(
                        node,
                        old.relation(),
                        "user",
                        user.0,
                    )));
                    tuples.push(RelationshipUpdate::Touch(node_tuple(
                        node,
                        role.relation(),
                        "user",
                        user.0,
                    )));
                    crate::journal::postgres::append(
                        conn,
                        ctx,
                        NewEntry {
                            node_id,
                            revision,
                            event: EventType::GrantUpdated,
                            data: serde_json::json!({
                                "grant_id": user_grant,
                                "grantee": { "type": "user", "id": user },
                                "old_role": old,
                                "new_role": role,
                            }),
                        },
                    )
                    .await?;
                }
            }
            None => {
                sqlx::query(
                    "update grants set grantee_kind = 'user', grantee_user_id = $3, \
                     grantee_email = null where tenant_id = $1 and id = $2",
                )
                .bind(t)
                .bind(id)
                .bind(user)
                .execute(&mut *conn)
                .await?;
                sqlx::query(
                    "insert into share_index (tenant_id, principal_kind, principal_id, node_id, \
                     grant_id, role, shared_at, shared_kind, shared_by, shared_obo) \
                     select tenant_id, 'user', $3, node_id, id, role, created_at, created_kind, \
                     created_by, created_obo from grants where tenant_id = $1 and id = $2 \
                     on conflict (tenant_id, principal_id, node_id) do nothing",
                )
                .bind(t)
                .bind(id)
                .bind(user)
                .execute(&mut *conn)
                .await?;
                tuples.push(RelationshipUpdate::Touch(node_tuple(
                    node,
                    role.relation(),
                    "user",
                    user.0,
                )));
                crate::journal::postgres::append(
                    conn,
                    ctx,
                    NewEntry {
                        node_id,
                        revision,
                        event: EventType::GrantAdded,
                        data: serde_json::json!({
                            "grant_id": id,
                            "grantee": { "type": "user", "id": user },
                            "role": role,
                        }),
                    },
                )
                .await?;
            }
        }
    }
    Ok((tuples, rows.len()))
}

/// Binds the email grants of `email` to `user` in every tenant, when the user signs in with
/// that email as a verified email of a trusted provider (a new user, or a new identity linked by
/// its verified email). One transaction per tenant, under its row-level security. A tenant where
/// the user is not a member or guest and that does not allow guests keeps its email grants.
pub(crate) async fn bind_email_grants_on_sign_in(
    area: &PgArea,
    email: &str,
    user: UserId,
) -> Result<()> {
    if email.trim().is_empty() {
        return Ok(());
    }
    let mut tx = area.system().await?;
    let tenants: Vec<TenantId> = sqlx::query_scalar(
        "select distinct tenant_id from grants where grantee_kind = 'email' \
         and lower(grantee_email) = lower($1)",
    )
    .bind(email)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let actor = Actor::user(user);
    for t in tenants {
        let mut tx = area.tenant(t, Some(user)).await?;
        let tenant = tenant_row(&mut tx, t).await?;
        if role_in(&mut tx, t, user).await?.is_none() && !tenant.policy.allow_guests {
            continue;
        }
        let ctx = Context {
            tenant: t,
            actor,
            scope: None,
            idempotency_key: None,
        };
        let (tuples, bound) = bind_email_grants(&mut tx, &ctx, email, user).await?;
        let outbox = enqueue(&mut tx, t, &tuples).await?;
        audit(
            &mut tx,
            t,
            &actor,
            "email_grants.bound",
            "user",
            user,
            serde_json::json!({ "email_grants_bound": bound }),
        )
        .await?;
        tx.commit().await?;
        area.apply(outbox).await?;
    }
    Ok(())
}

#[async_trait]
impl TenantStore for PgTenantStore {
    async fn list_for_user(&self, user: UserId) -> Result<Vec<(Tenant, MemberRole)>> {
        let mut tx = self.area.system().await?;
        let rows = sqlx::query(
            "select t.id, t.name, t.kind, t.plan, t.region, t.root_node_id, t.settings, \
             t.created_at, m.role from memberships m join tenants t on t.id = m.tenant_id \
             where m.user_id = $1 order by m.joined_at, t.id",
        )
        .bind(user)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter()
            .map(|r| Ok((tenant_from(r)?, r.try_get("role")?)))
            .collect()
    }

    async fn create_team(&self, actor: &Actor, name: String) -> Result<Tenant> {
        if !actor.is_user() {
            return Err(Error::user_required());
        }
        let owner = UserId(actor.actor.id.0);
        let name = check_name("the tenant name", &name, 100)?;
        let mut tx = self.area.system().await?;
        let exists: bool = sqlx::query_scalar(
            "select exists (select 1 from users where id = $1 and deleted_at is null)",
        )
        .bind(owner)
        .fetch_one(&mut *tx)
        .await?;
        if !exists {
            return Err(Error::not_found("user"));
        }
        let (tenant, tuples) = insert_tenant(
            &mut tx,
            TenantId::new(),
            TenantKind::Team,
            Plan::Team,
            &name,
            owner,
        )
        .await?;
        insert_owner_and_root(&mut tx, &tenant, owner).await?;
        let outbox = enqueue(&mut tx, tenant.id, &tuples).await?;
        audit(
            &mut tx,
            tenant.id,
            actor,
            "tenant.created",
            "tenant",
            tenant.id,
            serde_json::json!({ "name": tenant.name }),
        )
        .await?;
        tx.commit().await?;
        self.area.apply(outbox).await?;
        Ok(tenant)
    }

    async fn get(&self, ctx: &Context) -> Result<Tenant> {
        let user = caller_user(ctx)?;
        let mut tx = self.area.ctx(ctx).await?;
        require_role(role_in(&mut tx, ctx.tenant, user).await?, MemberRole::Guest)?;
        let t = tenant_row(&mut tx, ctx.tenant).await?;
        tx.commit().await?;
        Ok(t)
    }

    async fn find(&self, tenant: TenantId) -> Result<Tenant> {
        let mut tx = self.area.system().await?;
        let t = tenant_row(&mut tx, tenant).await?;
        tx.commit().await?;
        Ok(t)
    }

    async fn update(
        &self,
        ctx: &Context,
        name: Option<String>,
        policy: Option<TenantPolicyChange>,
    ) -> Result<Tenant> {
        let user = require_user_actor(ctx)?;
        let name = name
            .map(|n| check_name("the tenant name", &n, 100))
            .transpose()?;
        let mut tx = self.area.ctx(ctx).await?;
        require_role(role_in(&mut tx, ctx.tenant, user).await?, MemberRole::Admin)?;
        let mut t = tenant_row(&mut tx, ctx.tenant).await?;
        let mut tuples = Vec::new();
        if let Some(change) = &policy {
            let new = apply_policy(&t.policy, change)?;
            if t.kind == TenantKind::Team && new.members_can_share != t.policy.members_can_share {
                tuples = relations::members_can_share(t.id, new.members_can_share);
            }
            t.policy = new;
        }
        if let Some(n) = name {
            t.name = n;
        }
        sqlx::query(
            "update tenants set name = $2, settings = $3, updated_at = now() where id = $1",
        )
        .bind(t.id)
        .bind(&t.name)
        .bind(serde_json::to_value(&t.policy).map_err(|e| Error::Internal(e.to_string()))?)
        .execute(&mut *tx)
        .await?;
        let outbox = enqueue(&mut tx, t.id, &tuples).await?;
        audit(
            &mut tx,
            t.id,
            &ctx.actor,
            "tenant.updated",
            "tenant",
            t.id,
            serde_json::json!({ "name": t.name, "policy": t.policy }),
        )
        .await?;
        tx.commit().await?;
        self.area.apply(outbox).await?;
        Ok(t)
    }

    async fn role_of(&self, tenant: TenantId, user: UserId) -> Result<Option<MemberRole>> {
        let mut tx = self.area.system().await?;
        let r = role_in(&mut tx, tenant, user).await?;
        tx.commit().await?;
        Ok(r)
    }

    async fn list_members(&self, ctx: &Context, page_req: PageRequest) -> Result<Page<Member>> {
        let user = caller_user(ctx)?;
        let off = offset(&page_req)?;
        let limit = page_req.limit();
        let mut tx = self.area.ctx(ctx).await?;
        require_role(
            role_in(&mut tx, ctx.tenant, user).await?,
            MemberRole::Member,
        )?;
        let rows = sqlx::query(crate::sql!(
            "{MEMBER_SELECT} where m.tenant_id = $1 order by m.joined_at, m.user_id \
             limit $2 offset $3"
        ))
        .bind(ctx.tenant)
        .bind(i64::from(limit) + 1)
        .bind(off)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let items = rows.iter().map(member_from).collect::<Result<Vec<_>>>()?;
        Ok(page(items, off, limit))
    }

    async fn set_member_role(
        &self,
        ctx: &Context,
        user: UserId,
        role: MemberRole,
    ) -> Result<Member> {
        let caller = require_user_actor(ctx)?;
        let mut tx = self.area.ctx(ctx).await?;
        let caller_role = require_role(
            role_in(&mut tx, ctx.tenant, caller).await?,
            MemberRole::Admin,
        )?;
        let old = role_in(&mut tx, ctx.tenant, user)
            .await?
            .ok_or_else(|| Error::not_found("member"))?;
        check_role_change(
            caller_role,
            old,
            role,
            owner_count(&mut tx, ctx.tenant).await?,
        )?;
        if old == MemberRole::Guest && role != MemberRole::Guest {
            let t = tenant_row(&mut tx, ctx.tenant).await?;
            let members = member_count(&mut tx, ctx.tenant).await?;
            check_member_limit(tenant_limits(t.plan, members).members, members)?;
        }
        let mut outbox = None;
        if old != role {
            sqlx::query(
                "update memberships set role = $3, updated_at = now() \
                 where tenant_id = $1 and user_id = $2",
            )
            .bind(ctx.tenant)
            .bind(user)
            .bind(role)
            .execute(&mut *tx)
            .await?;
            outbox = enqueue(
                &mut tx,
                ctx.tenant,
                &relations::membership(ctx.tenant, user, Some(old), Some(role)),
            )
            .await?;
            audit(
                &mut tx,
                ctx.tenant,
                &ctx.actor,
                "member.role_changed",
                "member",
                user,
                serde_json::json!({ "old_role": old, "new_role": role }),
            )
            .await?;
        }
        let m = member(&mut tx, ctx.tenant, user).await?;
        tx.commit().await?;
        self.area.apply(outbox).await?;
        Ok(m)
    }

    async fn remove_member(&self, ctx: &Context, user: UserId) -> Result<()> {
        let caller = require_user_actor(ctx)?;
        let t = ctx.tenant;
        let mut tx = self.area.ctx(ctx).await?;
        let caller_role = role_in(&mut tx, t, caller)
            .await?
            .ok_or_else(|| Error::not_found("tenant"))?;
        if caller != user {
            require_role(Some(caller_role), MemberRole::Admin)?;
        }
        let target_role = role_in(&mut tx, t, user)
            .await?
            .ok_or_else(|| Error::not_found("member"))?;
        check_removal(
            caller,
            caller_role,
            user,
            target_role,
            owner_count(&mut tx, t).await?,
        )?;

        let mut tuples = relations::membership(t, user, Some(target_role), None);
        sqlx::query("delete from memberships where tenant_id = $1 and user_id = $2")
            .bind(t)
            .bind(user)
            .execute(&mut *tx)
            .await?;

        // Direct grants to the user (share_index rows go with them).
        let grants = sqlx::query(
            "delete from grants where tenant_id = $1 and grantee_kind = 'user' \
             and grantee_user_id = $2 returning id, node_id, role::text as role",
        )
        .bind(t)
        .bind(user)
        .fetch_all(&mut *tx)
        .await?;
        for g in &grants {
            let id: Uuid = g.try_get("id")?;
            let node: Uuid = g.try_get("node_id")?;
            let role = role_from_text(&g.try_get::<String, _>("role")?)?;
            tuples.push(RelationshipUpdate::Delete(node_tuple(
                node,
                role.relation(),
                "user",
                user.0,
            )));
            let revision = node_revision(&mut tx, t, node).await?;
            crate::journal::postgres::append(
                &mut tx,
                ctx,
                NewEntry {
                    node_id: Some(crate::ids::NodeId(node)),
                    revision,
                    event: EventType::GrantRemoved,
                    data: serde_json::json!({
                        "grant_id": id,
                        "grantee": { "type": "user", "id": user },
                        "role": role,
                    }),
                },
            )
            .await?;
        }

        // Share links the user or the user's agents made.
        let (kind, by, obo) = crate::identity::postgres::actor_cols(&ctx.actor);
        let links = sqlx::query(
            "update share_links set revoked_at = now(), revoked_kind = $3, revoked_by = $4, \
             revoked_obo = $5 where tenant_id = $1 and revoked_at is null \
             and coalesce(created_obo, created_by) = $2 returning id, node_id, role::text as role",
        )
        .bind(t)
        .bind(user)
        .bind(kind)
        .bind(by)
        .bind(obo)
        .fetch_all(&mut *tx)
        .await?;
        for l in &links {
            let id: Uuid = l.try_get("id")?;
            let node: Uuid = l.try_get("node_id")?;
            let role: String = l.try_get("role")?;
            tuples.push(RelationshipUpdate::Delete(node_tuple(
                node,
                &format!(
                    "link_{}",
                    match role.as_str() {
                        "view" => "viewer",
                        "comment" => "commenter",
                        _ => "editor",
                    }
                ),
                "share_link",
                id,
            )));
            let revision = node_revision(&mut tx, t, node).await?;
            crate::journal::postgres::append(
                &mut tx,
                ctx,
                NewEntry {
                    node_id: Some(crate::ids::NodeId(node)),
                    revision,
                    event: EventType::ShareLinkRevoked,
                    data: serde_json::json!({ "share_link_id": id }),
                },
            )
            .await?;
        }

        // The user's agents: disabled for good, OAuth grants ended.
        let agents: Vec<Uuid> = sqlx::query_scalar(
            "update agents set disabled_at = coalesce(disabled_at, now()), \
             disabled_reason = 'owner_left', updated_at = now() \
             where tenant_id = $1 and owner_id = $2 returning id",
        )
        .bind(t)
        .bind(user)
        .fetch_all(&mut *tx)
        .await?;
        sqlx::query(
            "update agent_oauth_grants set ended_at = now(), ended_kind = $3, ended_by = $4 \
             where tenant_id = $1 and agent_id = any($2) and ended_at is null",
        )
        .bind(t)
        .bind(&agents)
        .bind(kind)
        .bind(by)
        .execute(&mut *tx)
        .await?;
        // App passwords for this tenant.
        sqlx::query(
            "update app_passwords set revoked_at = now(), revoked_kind = $3, revoked_by = $4, \
             revoked_obo = $5 where tenant_id = $1 and user_id = $2 and revoked_at is null",
        )
        .bind(t)
        .bind(user)
        .bind(kind)
        .bind(by)
        .bind(obo)
        .execute(&mut *tx)
        .await?;

        let outbox = enqueue(&mut tx, t, &tuples).await?;
        audit(
            &mut tx,
            t,
            &ctx.actor,
            "member.removed",
            "member",
            user,
            serde_json::json!({
                "role": target_role,
                "grants_removed": grants.len(),
                "share_links_revoked": links.len(),
                "agents_disabled": agents,
            }),
        )
        .await?;
        tx.commit().await?;
        self.area.apply(outbox).await
    }

    async fn invite(
        &self,
        ctx: &Context,
        email: String,
        role: MemberRole,
        message: Option<String>,
    ) -> Result<(Invitation, String)> {
        let caller = require_user_actor(ctx)?;
        let email = check_email(&email)?;
        if role == MemberRole::Owner {
            return Err(crate::identity::invalid(
                "an invitation has the role guest, member or admin",
            ));
        }
        if message.as_ref().is_some_and(|m| m.chars().count() > 1000) {
            return Err(crate::identity::invalid(
                "the message must have at most 1000 characters",
            ));
        }
        let mut tx = self.area.ctx(ctx).await?;
        require_role(
            role_in(&mut tx, ctx.tenant, caller).await?,
            MemberRole::Admin,
        )?;
        let t = tenant_row(&mut tx, ctx.tenant).await?;
        if role == MemberRole::Guest && !t.policy.allow_guests {
            return Err(Error::Api(
                ApiError::new(
                    ErrorCode::PermissionDenied,
                    "the tenant policy does not allow guests",
                )
                .with_hint("an admin can turn on allow_guests in the tenant policy"),
            ));
        }
        if role != MemberRole::Guest {
            let members = member_count(&mut tx, ctx.tenant).await?;
            check_member_limit(tenant_limits(t.plan, members).members, members)?;
        }
        // An expired open invitation does not block a new one.
        sqlx::query(
            "update invitations set revoked_at = now() where tenant_id = $1 \
             and lower(email) = lower($2) and accepted_at is null and revoked_at is null \
             and expires_at <= now()",
        )
        .bind(ctx.tenant)
        .bind(&email)
        .execute(&mut *tx)
        .await?;
        let token = secret::random_secret(INVITATION_PREFIX);
        let now = crate::now();
        let (kind, by, obo) = crate::identity::postgres::actor_cols(&ctx.actor);
        let r = sqlx::query(crate::sql!(
            "insert into invitations (id, tenant_id, email, role, token_hash, message, created_at, \
             created_kind, created_by, created_obo, expires_at) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) returning {INVITATION_COLS}"
        ))
        .bind(InvitationId::new())
        .bind(ctx.tenant)
        .bind(&email)
        .bind(role)
        .bind(&secret::sha256(&token)[..])
        .bind(&message)
        .bind(now)
        .bind(kind)
        .bind(by)
        .bind(obo)
        .bind(now + time::Duration::days(INVITATION_DAYS))
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| unique_to_conflict(e.into(), "an open invitation for this email exists"))?;
        let inv = invitation_from(&r)?;
        let inviter: String = sqlx::query_scalar("select display_name from users where id = $1")
            .bind(caller)
            .fetch_optional(&mut *tx)
            .await?
            .unwrap_or_default();
        audit(
            &mut tx,
            ctx.tenant,
            &ctx.actor,
            "invitation.created",
            "invitation",
            inv.id,
            serde_json::json!({ "email": inv.email, "role": inv.role }),
        )
        .await?;
        tx.commit().await?;
        let url = memory::accept_url(&self.area.public_url, &token);
        let mail = mail::invitation_mail(&inv.email, &t.name, &inviter, &url, message.as_deref());
        if let Err(e) = self.area.mailer.send(mail).await {
            tracing::warn!(error = %e, "the invitation email was not sent");
        }
        Ok((inv, token))
    }

    async fn list_invitations(
        &self,
        ctx: &Context,
        page_req: PageRequest,
    ) -> Result<Page<Invitation>> {
        let caller = require_user_actor(ctx)?;
        let off = offset(&page_req)?;
        let limit = page_req.limit();
        let mut tx = self.area.ctx(ctx).await?;
        require_role(
            role_in(&mut tx, ctx.tenant, caller).await?,
            MemberRole::Admin,
        )?;
        let rows = sqlx::query(crate::sql!(
            "select {INVITATION_COLS} from invitations where tenant_id = $1 \
             and accepted_at is null and revoked_at is null and expires_at > now() \
             order by created_at desc, id desc limit $2 offset $3"
        ))
        .bind(ctx.tenant)
        .bind(i64::from(limit) + 1)
        .bind(off)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let items = rows
            .iter()
            .map(invitation_from)
            .collect::<Result<Vec<_>>>()?;
        Ok(page(items, off, limit))
    }

    async fn revoke_invitation(&self, ctx: &Context, id: InvitationId) -> Result<()> {
        let caller = require_user_actor(ctx)?;
        let mut tx = self.area.ctx(ctx).await?;
        require_role(
            role_in(&mut tx, ctx.tenant, caller).await?,
            MemberRole::Admin,
        )?;
        let n = sqlx::query(
            "update invitations set revoked_at = now() where tenant_id = $1 and id = $2 \
             and accepted_at is null and revoked_at is null",
        )
        .bind(ctx.tenant)
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if n == 0 {
            return Err(Error::not_found("invitation"));
        }
        audit(
            &mut tx,
            ctx.tenant,
            &ctx.actor,
            "invitation.revoked",
            "invitation",
            id,
            serde_json::json!({}),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn accept_invitation(&self, actor: &Actor, token: &str) -> Result<Member> {
        if !actor.is_user() || actor.actor.kind != PrincipalKind::User {
            return Err(Error::user_required());
        }
        let user = UserId(actor.actor.id.0);
        // The token lookup is a system lookup (the tenant is not known yet); every write after
        // it runs under the row-level security of the invitation's tenant.
        let mut tx = self.area.system().await?;
        let r = sqlx::query(crate::sql!(
            "select {INVITATION_COLS}, accepted_by from invitations where token_hash = $1 \
             and revoked_at is null and expires_at > now() for update"
        ))
        .bind(&secret::sha256(token.trim())[..])
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::not_found("invitation"))?;
        let inv = invitation_from(&r)?;
        let accepted_by: Option<UserId> = r.try_get("accepted_by")?;
        let t = inv.tenant_id;
        leave_system(&mut tx, t, user).await?;
        if inv.accepted_at.is_some() {
            if accepted_by != Some(user) {
                return Err(Error::Api(
                    ApiError::new(ErrorCode::Conflict, "the invitation was already accepted")
                        .with_hint("ask the tenant admin for a new invitation"),
                ));
            }
            let m = member(&mut tx, t, user)
                .await
                .map_err(|_| Error::not_found("invitation"))?;
            tx.commit().await?;
            return Ok(m);
        }
        let old = role_in(&mut tx, t, user).await?;
        let new = accepted_role(old, inv.role);
        let tenant = tenant_row(&mut tx, t).await?;
        // The policy may have changed since the invitation was sent.
        if new == MemberRole::Guest && !tenant.policy.allow_guests {
            return Err(Error::Api(
                ApiError::new(
                    ErrorCode::PermissionDenied,
                    "the tenant policy does not allow guests",
                )
                .with_hint(
                    "ask the tenant admin to turn on allow_guests, or to invite you as a member",
                ),
            ));
        }
        if new != MemberRole::Guest && old.is_none_or(|o| o == MemberRole::Guest) {
            let members = member_count(&mut tx, t).await?;
            check_member_limit(tenant_limits(tenant.plan, members).members, members)?;
        }
        sqlx::query(
            "insert into memberships (tenant_id, user_id, role, created_kind, created_by) \
             values ($1, $2, $3, 'user', $2) on conflict (tenant_id, user_id) \
             do update set role = excluded.role, updated_at = now()",
        )
        .bind(t)
        .bind(user)
        .bind(new)
        .execute(&mut *tx)
        .await?;
        sqlx::query("update invitations set accepted_at = now(), accepted_by = $2 where id = $1")
            .bind(inv.id)
            .bind(user)
            .execute(&mut *tx)
            .await?;
        let mut tuples = relations::membership(t, user, old, Some(new));
        // Email grants to the invitation's email become grants to the user.
        let ctx = Context {
            tenant: t,
            actor: *actor,
            scope: None,
            idempotency_key: None,
        };
        let (bound_tuples, bound) = bind_email_grants(&mut tx, &ctx, &inv.email, user).await?;
        tuples.extend(bound_tuples);
        let outbox = enqueue(&mut tx, t, &tuples).await?;
        audit(
            &mut tx,
            t,
            actor,
            "invitation.accepted",
            "member",
            user,
            serde_json::json!({ "role": new, "email_grants_bound": bound }),
        )
        .await?;
        let m = member(&mut tx, t, user).await?;
        tx.commit().await?;
        self.area.apply(outbox).await?;
        Ok(m)
    }

    async fn usage(&self, ctx: &Context) -> Result<Usage> {
        let user = caller_user(ctx)?;
        let mut tx = self.area.ctx(ctx).await?;
        require_role(
            role_in(&mut tx, ctx.tenant, user).await?,
            MemberRole::Member,
        )?;
        // `agents`: the agents counted against the tenant's limit (not those of guests,
        // `agents::guest_agent_limit`).
        let member_agent = crate::agents::postgres::MEMBER_AGENT;
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select (select coalesce(sum(size_bytes), 0)::bigint from nodes where tenant_id = $1) \
             as storage, (select count(*) from nodes where tenant_id = $1) as nodes, \
             (select count(*) from agents a where a.tenant_id = $1 and a.disabled_at is null \
             and {member_agent}) as agents"
        )))
        .bind(ctx.tenant)
        .fetch_one(&mut *tx)
        .await?;
        let members = member_count(&mut tx, ctx.tenant).await?;
        tx.commit().await?;
        Ok(Usage {
            storage_bytes: r.try_get("storage")?,
            nodes: r.try_get("nodes")?,
            agents: r.try_get::<i64, _>("agents")? as u32,
            members,
        })
    }
}
