//! The Postgres agent store (tables `agents`, `agent_keys`) and OAuth server (`oauth_clients`,
//! `agent_oauth_grants`, `oauth_codes`).

use super::memory::{Relation, check_access, relation};
use super::oauth::*;
use super::*;
use crate::actor::{Actor, PrincipalKind, require_user_actor};
use crate::identity::postgres::{PgArea, actor_cols, audit, db_code, enqueue, offset, page};
use crate::identity::{AccessClaims, TokenType, secret, unauthenticated};
use crate::tenants::rules::caller_user;
use crate::tenants::{MemberRole, Plan, tenant_limits};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

#[derive(Clone)]
pub struct PgAgentStore {
    area: PgArea,
}

impl PgAgentStore {
    pub fn new(area: PgArea) -> Self {
        Self { area }
    }
}

/// The condition (on `agents a`) of an agent counted against the tenant's agent limit: its
/// owner is not a guest of the tenant (`guest_agent_limit`).
pub(crate) const MEMBER_AGENT: &str = "not exists (select 1 from memberships m \
     where m.tenant_id = a.tenant_id and m.user_id = a.owner_id and m.role = 'guest')";

const AGENT_COLS: &str = "id, tenant_id, owner_id, name, description, scope_subtree, \
     scope_max_role::text as scope_max_role, scope_expires_at, budget, disabled_at, \
     disabled_reason::text as disabled_reason, created_at, last_used_at";

fn role_text(r: Role) -> &'static str {
    r.permission()
}

fn agent_from(r: &PgRow) -> Result<Agent> {
    let max_role: String = r.try_get("scope_max_role")?;
    let reason: Option<String> = r.try_get("disabled_reason")?;
    let budget: serde_json::Value = r.try_get("budget")?;
    Ok(Agent {
        id: r.try_get("id")?,
        tenant_id: r.try_get("tenant_id")?,
        owner_id: r.try_get("owner_id")?,
        name: r.try_get("name")?,
        description: r.try_get("description")?,
        scope: AgentScope {
            subtree: r.try_get("scope_subtree")?,
            max_role: serde_json::from_value(serde_json::Value::String(max_role))
                .map_err(|e| Error::Internal(e.to_string()))?,
            expires_at: r.try_get("scope_expires_at")?,
            budget: serde_json::from_value(budget).unwrap_or_default(),
        },
        disabled_at: r.try_get("disabled_at")?,
        disabled_reason: reason
            .map(|s| serde_json::from_value(serde_json::Value::String(s)))
            .transpose()
            .map_err(|e| Error::Internal(e.to_string()))?,
        created_at: r.try_get("created_at")?,
        last_used_at: r.try_get("last_used_at")?,
    })
}

const KEY_COLS: &str =
    "id, agent_id, name, prefix, created_at, expires_at, last_used_at, revoked_at";

fn key_from(r: &PgRow) -> Result<AgentKey> {
    Ok(AgentKey {
        id: r.try_get("id")?,
        agent_id: r.try_get("agent_id")?,
        name: r.try_get("name")?,
        prefix: r.try_get("prefix")?,
        created_at: r.try_get("created_at")?,
        expires_at: r.try_get("expires_at")?,
        last_used_at: r.try_get("last_used_at")?,
        revoked_at: r.try_get("revoked_at")?,
    })
}

async fn agent_in(conn: &mut PgConnection, t: TenantId, id: AgentId) -> Result<Agent> {
    let r = sqlx::query(crate::sql!(
        "select {AGENT_COLS} from agents where tenant_id = $1 and id = $2"
    ))
    .bind(t)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| Error::not_found("agent"))?;
    agent_from(&r)
}

async fn caller_role(conn: &mut PgConnection, ctx: &Context) -> Result<Option<MemberRole>> {
    let Some(u) = ctx.actor.effective_user() else {
        return Ok(None);
    };
    Ok(
        sqlx::query_scalar("select role from memberships where tenant_id = $1 and user_id = $2")
            .bind(ctx.tenant)
            .bind(u)
            .fetch_optional(&mut *conn)
            .await?,
    )
}

/// Maps the foreign key violation of a scope subtree outside the tenant to `not_found`.
fn subtree_error(e: sqlx::Error) -> Error {
    let e: Error = e.into();
    if db_code(&e).as_deref() == Some("23503") {
        Error::not_found("scope subtree")
    } else {
        e
    }
}

/// Inserts an agent of `owner` in `ctx.tenant`, with the limit check, the tuples and the audit
/// entry, in the caller's transaction. Returns the agent and the outbox row.
pub(crate) async fn insert_agent(
    conn: &mut PgConnection,
    ctx: &Context,
    owner: UserId,
    req: NewAgent,
) -> Result<(Agent, Option<(TenantId, i64)>)> {
    let t = ctx.tenant;
    let plan: Plan = sqlx::query_scalar("select plan from tenants where id = $1")
        .bind(t)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| Error::not_found("tenant"))?;
    let members: i64 = sqlx::query_scalar(
        "select count(*) from memberships where tenant_id = $1 and role <> 'guest'",
    )
    .bind(t)
    .fetch_one(&mut *conn)
    .await?;
    let guest: bool = sqlx::query_scalar(
        "select exists (select 1 from memberships where tenant_id = $1 and user_id = $2 \
         and role = 'guest')",
    )
    .bind(t)
    .bind(owner)
    .fetch_one(&mut *conn)
    .await?;
    // A guest's agents have their own limit, so guests cannot use up the tenant's
    // (`guest_agent_limit`).
    let limit = if guest {
        guest_agent_limit(plan)
    } else {
        tenant_limits(plan, members as u32).agents
    };
    // Lock the tenant's agents while counting, so two requests do not both pass the limit.
    sqlx::query("select 1 from tenants where id = $1 for update")
        .bind(t)
        .execute(&mut *conn)
        .await?;
    let live: i64 = if guest {
        sqlx::query_scalar(
            "select count(*) from agents where tenant_id = $1 and owner_id = $2 \
             and disabled_at is null",
        )
        .bind(t)
        .bind(owner)
        .fetch_one(&mut *conn)
        .await?
    } else {
        sqlx::query_scalar(crate::sql!(
            "select count(*) from agents a where a.tenant_id = $1 and a.disabled_at is null \
             and {MEMBER_AGENT}"
        ))
        .bind(t)
        .fetch_one(&mut *conn)
        .await?
    };
    if live as u32 >= limit {
        return Err(agent_limit_reached(limit));
    }
    let (kind, by, obo) = actor_cols(&ctx.actor);
    let r = sqlx::query(crate::sql!(
        "insert into agents (id, tenant_id, owner_id, name, description, scope_subtree, \
         scope_max_role, scope_expires_at, budget, created_kind, created_by, created_obo) \
         values ($1, $2, $3, $4, $5, $6, $7::grant_role, $8, $9, $10, $11, $12) \
         returning {AGENT_COLS}"
    ))
    .bind(AgentId::new())
    .bind(t)
    .bind(owner)
    .bind(&req.name)
    .bind(&req.description)
    .bind(req.scope.subtree)
    .bind(role_text(req.scope.max_role))
    .bind(req.scope.expires_at)
    .bind(serde_json::to_value(&req.scope.budget).map_err(|e| Error::Internal(e.to_string()))?)
    .bind(kind)
    .bind(by)
    .bind(obo)
    .fetch_one(&mut *conn)
    .await
    .map_err(subtree_error)?;
    let agent = agent_from(&r)?;
    let outbox = enqueue(
        conn,
        t,
        &crate::tenants::relations::agent_registered(agent.id, t, owner),
    )
    .await?;
    audit(
        conn,
        t,
        &ctx.actor,
        "agent.registered",
        "agent",
        agent.id,
        serde_json::json!({ "name": agent.name, "scope": agent.scope }),
    )
    .await?;
    Ok((agent, outbox))
}

/// Issues an access token for an agent.
pub(crate) fn token_for(
    tokens: &dyn crate::identity::TokenIssuer,
    agent: &Agent,
    audience: Option<&str>,
) -> Result<AccessToken> {
    let mut c = AccessClaims::new(tokens, TokenType::Agent, agent.id.0, ACCESS_TOKEN_SECONDS);
    if let Some(a) = audience {
        c.aud = a.to_string();
    }
    c.tid = Some(agent.tenant_id);
    c.obo = Some(agent.owner_id);
    c.agent = Some(agent.id);
    Ok(AccessToken {
        access_token: tokens.issue(&c)?,
        token_type: "Bearer".into(),
        expires_in: ACCESS_TOKEN_SECONDS,
        issued_token_type: None,
    })
}

#[async_trait]
impl AgentStore for PgAgentStore {
    async fn register(&self, ctx: &Context, req: NewAgent) -> Result<Agent> {
        let owner = require_user_actor(ctx)?;
        let req = check_new_agent(&req)?;
        let mut tx = self.area.ctx(ctx).await?;
        let (agent, outbox) = insert_agent(&mut tx, ctx, owner, req).await?;
        tx.commit().await?;
        self.area.apply(outbox).await?;
        Ok(agent)
    }

    async fn list(
        &self,
        ctx: &Context,
        owner: Option<UserId>,
        page_req: PageRequest,
    ) -> Result<Page<Agent>> {
        let user = caller_user(ctx)?;
        let off = offset(&page_req)?;
        let limit = page_req.limit();
        let mut tx = self.area.ctx(ctx).await?;
        let role = caller_role(&mut tx, ctx)
            .await?
            .ok_or_else(|| Error::not_found("tenant"))?;
        let me_agent =
            (ctx.actor.actor.kind == PrincipalKind::Agent).then_some(ctx.actor.actor.id.0);
        let admin = ctx.actor.is_user() && role >= MemberRole::Admin;
        let (filter_id, filter_owner): (Option<Uuid>, Option<UserId>) = match me_agent {
            Some(me) => (Some(me), None),
            None if admin => (None, owner),
            None => (None, Some(user)),
        };
        let rows = sqlx::query(crate::sql!(
            "select {AGENT_COLS} from agents where tenant_id = $1 \
             and ($2::uuid is null or id = $2) and ($3::uuid is null or owner_id = $3) \
             order by created_at, id limit $4 offset $5"
        ))
        .bind(ctx.tenant)
        .bind(filter_id)
        .bind(filter_owner)
        .bind(i64::from(limit) + 1)
        .bind(off)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let items = rows.iter().map(agent_from).collect::<Result<Vec<_>>>()?;
        Ok(page(items, off, limit))
    }

    async fn get(&self, ctx: &Context, id: AgentId) -> Result<Agent> {
        caller_user(ctx)?;
        let mut tx = self.area.ctx(ctx).await?;
        let agent = agent_in(&mut tx, ctx.tenant, id).await?;
        let role = caller_role(&mut tx, ctx).await?;
        tx.commit().await?;
        check_access(relation(ctx, &agent, role), true)?;
        Ok(agent)
    }

    async fn update(&self, ctx: &Context, id: AgentId, change: AgentUpdate) -> Result<Agent> {
        require_user_actor(ctx)?;
        let name = change
            .name
            .map(|n| crate::identity::check_name("the agent name", &n, 100))
            .transpose()?;
        let description = change.description.map(check_description).transpose()?;
        if let Some(s) = &change.scope {
            check_scope(s)?;
        }
        let mut tx = self.area.ctx(ctx).await?;
        let mut agent = agent_in(&mut tx, ctx.tenant, id).await?;
        let role = caller_role(&mut tx, ctx).await?;
        check_access(relation(ctx, &agent, role), false)?;
        if let Some(n) = name {
            agent.name = n;
        }
        if let Some(d) = description {
            agent.description = d;
        }
        if let Some(s) = change.scope {
            agent.scope = s;
        }
        let r = sqlx::query(crate::sql!(
            "update agents set name = $3, description = $4, scope_subtree = $5, \
             scope_max_role = $6::grant_role, scope_expires_at = $7, budget = $8, \
             updated_at = now() where tenant_id = $1 and id = $2 returning {AGENT_COLS}"
        ))
        .bind(ctx.tenant)
        .bind(id)
        .bind(&agent.name)
        .bind(&agent.description)
        .bind(agent.scope.subtree)
        .bind(role_text(agent.scope.max_role))
        .bind(agent.scope.expires_at)
        .bind(
            serde_json::to_value(&agent.scope.budget)
                .map_err(|e| Error::Internal(e.to_string()))?,
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(subtree_error)?;
        let agent = agent_from(&r)?;
        audit(
            &mut tx,
            ctx.tenant,
            &ctx.actor,
            "agent.updated",
            "agent",
            id,
            serde_json::json!({ "name": agent.name, "scope": agent.scope }),
        )
        .await?;
        tx.commit().await?;
        Ok(agent)
    }

    async fn set_disabled(&self, ctx: &Context, id: AgentId, disabled: bool) -> Result<Agent> {
        require_user_actor(ctx)?;
        let mut tx = self.area.ctx(ctx).await?;
        let agent = agent_in(&mut tx, ctx.tenant, id).await?;
        let role = caller_role(&mut tx, ctx).await?;
        let rel = relation(ctx, &agent, role);
        check_access(rel, disabled)?;
        let (kind, by, _) = actor_cols(&ctx.actor);
        let agent = if disabled {
            if agent.disabled_at.is_some() {
                agent
            } else {
                let reason = if rel == Relation::Owner {
                    "by_owner"
                } else {
                    "by_admin"
                };
                let r = sqlx::query(crate::sql!(
                    "update agents set disabled_at = now(), \
                     disabled_reason = $3::agent_disabled_reason, updated_at = now() \
                     where tenant_id = $1 and id = $2 returning {AGENT_COLS}"
                ))
                .bind(ctx.tenant)
                .bind(id)
                .bind(reason)
                .fetch_one(&mut *tx)
                .await?;
                sqlx::query(
                    "update agent_oauth_grants set ended_at = now(), ended_kind = $3, \
                     ended_by = $4 where tenant_id = $1 and agent_id = $2 and ended_at is null",
                )
                .bind(ctx.tenant)
                .bind(id)
                .bind(kind)
                .bind(by)
                .execute(&mut *tx)
                .await?;
                audit(
                    &mut tx,
                    ctx.tenant,
                    &ctx.actor,
                    "agent.disabled",
                    "agent",
                    id,
                    serde_json::json!({ "reason": reason }),
                )
                .await?;
                agent_from(&r)?
            }
        } else {
            match agent.disabled_reason {
                None => agent,
                Some(DisabledReason::OwnerLeft) => {
                    return Err(Error::Api(
                        crate::error::ApiError::new(
                            ErrorCode::PermissionDenied,
                            "the owner left the tenant, so the agent cannot be enabled",
                        )
                        .with_hint("register a new agent"),
                    ));
                }
                Some(_) => {
                    let r = sqlx::query(crate::sql!(
                        "update agents set disabled_at = null, disabled_reason = null, \
                         updated_at = now() where tenant_id = $1 and id = $2 \
                         returning {AGENT_COLS}"
                    ))
                    .bind(ctx.tenant)
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await?;
                    audit(
                        &mut tx,
                        ctx.tenant,
                        &ctx.actor,
                        "agent.enabled",
                        "agent",
                        id,
                        serde_json::json!({}),
                    )
                    .await?;
                    agent_from(&r)?
                }
            }
        };
        tx.commit().await?;
        Ok(agent)
    }

    async fn create_key(
        &self,
        ctx: &Context,
        id: AgentId,
        name: Option<String>,
        expires_at: Option<OffsetDateTime>,
    ) -> Result<(AgentKey, String)> {
        require_user_actor(ctx)?;
        let name = match name {
            Some(n) if n.chars().count() > 100 => {
                return Err(crate::identity::invalid(
                    "the key name must have at most 100 characters",
                ));
            }
            Some(n) if n.trim().is_empty() => None,
            other => other,
        };
        let expires_at = key_expiry(expires_at)?;
        let mut tx = self.area.ctx(ctx).await?;
        let agent = agent_in(&mut tx, ctx.tenant, id).await?;
        let role = caller_role(&mut tx, ctx).await?;
        check_access(relation(ctx, &agent, role), false)?;
        let (secret_value, hash) = generate_key();
        let (kind, by, obo) = actor_cols(&ctx.actor);
        let r = sqlx::query(crate::sql!(
            "insert into agent_keys (id, tenant_id, agent_id, name, prefix, key_hash, \
             created_kind, created_by, created_obo, expires_at) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) returning {KEY_COLS}"
        ))
        .bind(AgentKeyId::new())
        .bind(ctx.tenant)
        .bind(id)
        .bind(&name)
        .bind(secret::display_prefix(&secret_value, API_KEY_PREFIX))
        .bind(&hash[..])
        .bind(kind)
        .bind(by)
        .bind(obo)
        .bind(expires_at)
        .fetch_one(&mut *tx)
        .await?;
        let key = key_from(&r)?;
        audit(
            &mut tx,
            ctx.tenant,
            &ctx.actor,
            "agent.key_created",
            "agent_key",
            key.id,
            serde_json::json!({ "agent_id": id, "prefix": key.prefix }),
        )
        .await?;
        tx.commit().await?;
        Ok((key, secret_value))
    }

    async fn list_keys(&self, ctx: &Context, id: AgentId) -> Result<Vec<AgentKey>> {
        require_user_actor(ctx)?;
        let mut tx = self.area.ctx(ctx).await?;
        let agent = agent_in(&mut tx, ctx.tenant, id).await?;
        let role = caller_role(&mut tx, ctx).await?;
        check_access(relation(ctx, &agent, role), true)?;
        let rows = sqlx::query(crate::sql!(
            "select {KEY_COLS} from agent_keys where tenant_id = $1 and agent_id = $2 \
             order by created_at, id"
        ))
        .bind(ctx.tenant)
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter().map(key_from).collect()
    }

    async fn revoke_key(&self, ctx: &Context, id: AgentId, key: AgentKeyId) -> Result<()> {
        require_user_actor(ctx)?;
        let mut tx = self.area.ctx(ctx).await?;
        let agent = agent_in(&mut tx, ctx.tenant, id).await?;
        let role = caller_role(&mut tx, ctx).await?;
        check_access(relation(ctx, &agent, role), true)?;
        let found: Option<Option<OffsetDateTime>> = sqlx::query_scalar(
            "select revoked_at from agent_keys where tenant_id = $1 and agent_id = $2 and id = $3",
        )
        .bind(ctx.tenant)
        .bind(id)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;
        match found {
            None => return Err(Error::not_found("key")),
            Some(Some(_)) => {}
            Some(None) => {
                let (kind, by, _) = actor_cols(&ctx.actor);
                sqlx::query(
                    "update agent_keys set revoked_at = now(), revoked_kind = $4, revoked_by = $5 \
                     where tenant_id = $1 and agent_id = $2 and id = $3",
                )
                .bind(ctx.tenant)
                .bind(id)
                .bind(key)
                .bind(kind)
                .bind(by)
                .execute(&mut *tx)
                .await?;
                audit(
                    &mut tx,
                    ctx.tenant,
                    &ctx.actor,
                    "agent.key_revoked",
                    "agent_key",
                    key,
                    serde_json::json!({ "agent_id": id }),
                )
                .await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    async fn exchange_key(&self, secret_value: &str) -> Result<AccessToken> {
        let refuse = || {
            unauthenticated(
                "the API key is not valid",
                "make a new key for the agent in the web UI",
            )
        };
        if !secret_value.starts_with(API_KEY_PREFIX) {
            return Err(refuse());
        }
        let mut tx = self.area.system().await?;
        let r = sqlx::query(
            "select id, tenant_id, agent_id from agent_keys where key_hash = $1 \
             and revoked_at is null and expires_at > now()",
        )
        .bind(&hash_key(secret_value)[..])
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(refuse)?;
        let key: AgentKeyId = r.try_get("id")?;
        let tenant: TenantId = r.try_get("tenant_id")?;
        let agent_id: AgentId = r.try_get("agent_id")?;
        let agent = agent_in(&mut tx, tenant, agent_id).await?;
        check_live(&agent)?;
        sqlx::query("update agent_keys set last_used_at = now() where id = $1")
            .bind(key)
            .execute(&mut *tx)
            .await?;
        sqlx::query("update agents set last_used_at = now() where tenant_id = $1 and id = $2")
            .bind(tenant)
            .bind(agent_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        let mut t = token_for(self.area.tokens.as_ref(), &agent, None)?;
        t.issued_token_type = Some(ACCESS_TOKEN_TYPE.into());
        Ok(t)
    }

    async fn authenticate(&self, tenant: TenantId, id: AgentId) -> Result<Agent> {
        // A read on every request; `last_used_at` is written at most every
        // AGENT_TOUCH_SECONDS, so parallel requests of one agent do not queue on its row.
        let mut tx = self.area.system().await?;
        let r = sqlx::query(crate::sql!(
            "select {AGENT_COLS} from agents where tenant_id = $1 and id = $2"
        ))
        .bind(tenant)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let agent = r.map(|r| agent_from(&r)).transpose()?.ok_or_else(|| {
            unauthenticated(
                "the agent of the token does not exist",
                "get a new access token",
            )
        })?;
        check_live(&agent)?;
        let now = crate::now();
        if agent
            .last_used_at
            .is_none_or(|t| now - t >= time::Duration::seconds(AGENT_TOUCH_SECONDS))
        {
            sqlx::query(
                "update agents set last_used_at = now() where tenant_id = $1 and id = $2 \
                 and (last_used_at is null \
                 or last_used_at < now() - make_interval(secs => $3))",
            )
            .bind(tenant)
            .bind(id)
            .bind(AGENT_TOUCH_SECONDS as f64)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(agent)
    }

    async fn activity(
        &self,
        ctx: &Context,
        id: AgentId,
        since: Option<OffsetDateTime>,
        page_req: PageRequest,
    ) -> Result<Page<JournalEntry>> {
        caller_user(ctx)?;
        let off = offset(&page_req)?;
        let limit = page_req.limit();
        let mut tx = self.area.ctx(ctx).await?;
        let agent = agent_in(&mut tx, ctx.tenant, id).await?;
        let role = caller_role(&mut tx, ctx).await?;
        check_access(relation(ctx, &agent, role), true)?;
        let rows = sqlx::query(
            "select tenant_id, seq, node_id, revision, type, actor_kind, actor, on_behalf_of, at, \
             data from journal where tenant_id = $1 and actor = $2 \
             and ($3::timestamptz is null or at >= $3) order by seq desc limit $4 offset $5",
        )
        .bind(ctx.tenant)
        .bind(id)
        .bind(since)
        .bind(i64::from(limit) + 1)
        .bind(off)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let items = rows
            .iter()
            .map(|r| {
                let event: String = r.try_get("type")?;
                let kind: PrincipalKind = r.try_get("actor_kind")?;
                let actor: Uuid = r.try_get("actor")?;
                Ok(JournalEntry {
                    tenant_id: r.try_get("tenant_id")?,
                    seq: r.try_get("seq")?,
                    node_id: r.try_get("node_id")?,
                    revision: r.try_get("revision")?,
                    event: serde_json::from_value(serde_json::Value::String(event))
                        .map_err(|e| Error::Internal(e.to_string()))?,
                    actor: Actor {
                        actor: crate::actor::PrincipalRef {
                            kind,
                            id: crate::ids::PrincipalId(actor),
                        },
                        on_behalf_of: r.try_get("on_behalf_of")?,
                    },
                    at: r.try_get("at")?,
                    data: r.try_get("data")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(page(items, off, limit))
    }
}

/// The Postgres OAuth server.
#[derive(Clone)]
pub struct PgOAuthServer {
    area: PgArea,
}

impl PgOAuthServer {
    pub fn new(area: PgArea) -> Self {
        Self { area }
    }
}

fn client_from(r: &PgRow) -> Result<OAuthClient> {
    let created: OffsetDateTime = r.try_get("created_at")?;
    Ok(OAuthClient {
        client_id: r.try_get("client_id")?,
        client_id_issued_at: created.unix_timestamp(),
        client_name: r.try_get("client_name")?,
        redirect_uris: r.try_get("redirect_uris")?,
        grant_types: r.try_get("grant_types")?,
        response_types: vec!["code".into()],
        token_endpoint_auth_method: "none".into(),
        client_uri: r.try_get("client_uri")?,
        logo_uri: r.try_get("logo_uri")?,
    })
}

const CLIENT_COLS: &str =
    "client_id, client_name, redirect_uris, grant_types, client_uri, logo_uri, created_at";

impl PgOAuthServer {
    /// Issues tokens for a live grant and rotates its refresh token, in the caller's
    /// transaction. Checks the agent.
    async fn issue(
        &self,
        conn: &mut PgConnection,
        tenant: TenantId,
        grant: Uuid,
        agent_id: AgentId,
        audience: &str,
    ) -> std::result::Result<TokenResponse, OAuthError> {
        let agent = agent_in(conn, tenant, agent_id)
            .await
            .map_err(|_| OAuthError::invalid_grant("the agent does not exist"))?;
        check_live(&agent)?;
        let refresh = secret::random_secret(REFRESH_PREFIX);
        sqlx::query(
            "update agent_oauth_grants set previous_refresh_hash = refresh_token_hash, \
             refresh_token_hash = $3, refresh_expires_at = now() + make_interval(days => $4), \
             last_used_at = now() where tenant_id = $1 and id = $2",
        )
        .bind(tenant)
        .bind(grant)
        .bind(&secret::sha256(&refresh)[..])
        .bind(REFRESH_DAYS as i32)
        .execute(&mut *conn)
        .await
        .map_err(Error::from)?;
        sqlx::query("update agents set last_used_at = now() where tenant_id = $1 and id = $2")
            .bind(tenant)
            .bind(agent_id)
            .execute(&mut *conn)
            .await
            .map_err(Error::from)?;
        let access = token_for(self.area.tokens.as_ref(), &agent, Some(audience))?;
        Ok(TokenResponse {
            access,
            refresh_token: Some(refresh),
        })
    }
}

#[async_trait]
impl OAuthServer for PgOAuthServer {
    async fn register_client(
        &self,
        req: ClientRegistration,
    ) -> std::result::Result<OAuthClient, OAuthError> {
        let req = check_registration(req)?;
        let mut tx = self.area.system().await?;
        let r = sqlx::query(crate::sql!(
            "insert into oauth_clients (client_id, client_name, redirect_uris, grant_types, \
             client_uri, logo_uri, software_id, software_version) \
             values ($1, $2, $3, $4, $5, $6, $7, $8) returning {CLIENT_COLS}"
        ))
        .bind(new_client_id())
        .bind(&req.client_name)
        .bind(&req.redirect_uris)
        .bind(&req.grant_types)
        .bind(&req.client_uri)
        .bind(&req.logo_uri)
        .bind(&req.software_id)
        .bind(&req.software_version)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::from)?;
        tx.commit().await.map_err(Error::from)?;
        Ok(client_from(&r)?)
    }

    async fn get_client(&self, client_id: &str) -> Result<OAuthClient> {
        let mut tx = self.area.system().await?;
        let r = sqlx::query(crate::sql!(
            "select {CLIENT_COLS} from oauth_clients where client_id = $1"
        ))
        .bind(client_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::not_found("OAuth client"))?;
        tx.commit().await?;
        client_from(&r)
    }

    async fn start_authorization(&self, req: AuthorizationRequest) -> Result<String> {
        let client = self.get_client(&req.client_id).await?;
        if !client.redirect_uris.contains(&req.redirect_uri) {
            return Err(crate::identity::invalid(
                "the redirect URI is not registered for this client",
            ));
        }
        if !secret::is_pkce_value(&req.code_challenge) {
            return Err(crate::identity::invalid("the code challenge is not valid"));
        }
        let value = serde_json::to_value(&req).map_err(|e| Error::Internal(e.to_string()))?;
        self.area
            .tokens
            .seal(REQUEST_PURPOSE, &value, REQUEST_SECONDS)
    }

    fn open_request(&self, request: &str) -> Result<AuthorizationRequest> {
        open_consent_request(self.area.tokens.as_ref(), request)
    }

    async fn consent(&self, who: &Actor, consent: Consent) -> Result<String> {
        let tokens = self.area.tokens.clone();
        let (user, req) = check_consent(tokens.as_ref(), who, &consent)?;
        let issuer = tokens.issuer().to_string();
        if !consent.approve {
            if let Some(t) = consent.tenant_id {
                let mut tx = self.area.tenant(t, Some(user)).await?;
                let member: bool = sqlx::query_scalar(
                    "select exists (select 1 from memberships where tenant_id = $1 and user_id = $2)",
                )
                .bind(t)
                .bind(user)
                .fetch_one(&mut *tx)
                .await?;
                if member {
                    audit(
                        &mut tx,
                        t,
                        who,
                        "agent.oauth_denied",
                        "oauth_client",
                        &req.client_id,
                        serde_json::json!({}),
                    )
                    .await?;
                }
                tx.commit().await?;
            }
            return redirect_with(
                &req.redirect_uri,
                &[("error", "access_denied")],
                req.state.as_deref(),
                &issuer,
            );
        }
        let tenant = consent.tenant_id.expect("checked");
        // The client must still exist.
        self.get_client(&req.client_id).await?;
        let ctx = consent_context(who, tenant);
        let mut tx = self.area.tenant(tenant, Some(user)).await?;
        let member: bool = sqlx::query_scalar(
            "select exists (select 1 from memberships where tenant_id = $1 and user_id = $2)",
        )
        .bind(tenant)
        .bind(user)
        .fetch_one(&mut *tx)
        .await?;
        if !member {
            return Err(Error::not_found("tenant"));
        }
        let (agent, mut outbox) = match (consent.agent_id, consent.new_agent) {
            (Some(id), _) => {
                let a = agent_in(&mut tx, tenant, id).await?;
                if a.owner_id != user {
                    return Err(Error::not_found("agent"));
                }
                if a.disabled_at.is_some() {
                    return Err(agent_disabled());
                }
                (a, None)
            }
            (None, Some(new)) => {
                let new = check_new_agent(&new)?;
                insert_agent(&mut tx, &ctx, user, new).await?
            }
            (None, None) => unreachable!("checked"),
        };
        let (kind, by, obo) = actor_cols(who);
        sqlx::query(
            "update agent_oauth_grants set ended_at = now(), ended_kind = $4, ended_by = $5 \
             where tenant_id = $1 and agent_id = $2 and client_id = $3 and ended_at is null",
        )
        .bind(tenant)
        .bind(agent.id)
        .bind(&req.client_id)
        .bind(kind)
        .bind(by)
        .execute(&mut *tx)
        .await?;
        let grant = Uuid::now_v7();
        sqlx::query(
            "insert into agent_oauth_grants (id, tenant_id, agent_id, client_id, audience, \
             created_kind, created_by, created_obo) values ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(grant)
        .bind(tenant)
        .bind(agent.id)
        .bind(&req.client_id)
        .bind(&req.resource)
        .bind(kind)
        .bind(by)
        .bind(obo)
        .execute(&mut *tx)
        .await?;
        let code = secret::random_secret("");
        sqlx::query(
            "insert into oauth_codes (code_hash, tenant_id, grant_id, code_challenge, \
             redirect_uri, expires_at) values ($1, $2, $3, $4, $5, now() + make_interval(secs => $6))",
        )
        .bind(&secret::sha256(&code)[..])
        .bind(tenant)
        .bind(grant)
        .bind(&req.code_challenge)
        .bind(&req.redirect_uri)
        .bind(CODE_SECONDS as f64)
        .execute(&mut *tx)
        .await?;
        sqlx::query("update oauth_clients set last_used_at = now() where client_id = $1")
            .bind(&req.client_id)
            .execute(&mut *tx)
            .await?;
        audit(
            &mut tx,
            tenant,
            who,
            "agent.oauth_approved",
            "agent",
            agent.id,
            serde_json::json!({ "client_id": req.client_id }),
        )
        .await?;
        tx.commit().await?;
        self.area.apply(outbox.take()).await?;
        redirect_with(
            &req.redirect_uri,
            &[("code", &code)],
            req.state.as_deref(),
            &issuer,
        )
    }

    async fn token(&self, req: TokenRequest) -> std::result::Result<TokenResponse, OAuthError> {
        match req {
            TokenRequest::AuthorizationCode {
                code,
                code_verifier,
                client_id,
                redirect_uri,
                resource,
            } => {
                if !secret::is_pkce_value(&code_verifier) {
                    return Err(OAuthError::invalid_request("code_verifier is not valid"));
                }
                let mut tx = self.area.system().await?;
                let r = sqlx::query(
                    "select c.tenant_id, c.grant_id, c.code_challenge, c.redirect_uri, \
                     c.used_at, c.expires_at > now() as live, g.client_id, g.agent_id, \
                     g.audience, g.ended_at from oauth_codes c join agent_oauth_grants g \
                     on g.tenant_id = c.tenant_id and g.id = c.grant_id \
                     where c.code_hash = $1 for update of c",
                )
                .bind(&secret::sha256(&code)[..])
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::from)?
                .ok_or_else(|| OAuthError::invalid_grant("the code is not valid"))?;
                let tenant: TenantId = r.try_get("tenant_id").map_err(Error::from)?;
                let grant: Uuid = r.try_get("grant_id").map_err(Error::from)?;
                let used: Option<OffsetDateTime> = r.try_get("used_at").map_err(Error::from)?;
                if used.is_some() {
                    // A replayed code ends the grant (RFC 6749, section 4.1.2).
                    sqlx::query(
                        "update agent_oauth_grants set ended_at = coalesce(ended_at, now()) \
                         where tenant_id = $1 and id = $2",
                    )
                    .bind(tenant)
                    .bind(grant)
                    .execute(&mut *tx)
                    .await
                    .map_err(Error::from)?;
                    tx.commit().await.map_err(Error::from)?;
                    return Err(OAuthError::invalid_grant("the code was already used"));
                }
                sqlx::query("update oauth_codes set used_at = now() where code_hash = $1")
                    .bind(&secret::sha256(&code)[..])
                    .execute(&mut *tx)
                    .await
                    .map_err(Error::from)?;
                let live: bool = r.try_get("live").map_err(Error::from)?;
                let challenge: String = r.try_get("code_challenge").map_err(Error::from)?;
                let code_redirect: String = r.try_get("redirect_uri").map_err(Error::from)?;
                let grant_client: String = r.try_get("client_id").map_err(Error::from)?;
                let agent_id: AgentId = r.try_get("agent_id").map_err(Error::from)?;
                let audience: String = r.try_get("audience").map_err(Error::from)?;
                let ended: Option<OffsetDateTime> = r.try_get("ended_at").map_err(Error::from)?;
                let check = (|| {
                    if !live {
                        return Err(OAuthError::invalid_grant("the code has expired"));
                    }
                    if code_redirect != redirect_uri {
                        return Err(OAuthError::invalid_grant("redirect_uri does not match"));
                    }
                    if secret::pkce_challenge(&code_verifier) != challenge {
                        return Err(OAuthError::invalid_grant(
                            "the PKCE verifier does not match",
                        ));
                    }
                    if ended.is_some() {
                        return Err(OAuthError::invalid_grant("the grant has ended"));
                    }
                    if grant_client != client_id {
                        return Err(OAuthError::invalid_grant("the code is for another client"));
                    }
                    if resource
                        .as_deref()
                        .is_some_and(|r| r.trim_end_matches('/') != audience)
                    {
                        return Err(OAuthError::new(
                            "invalid_target",
                            "resource does not match the authorization",
                            ErrorCode::ValidationFailed,
                        ));
                    }
                    Ok(())
                })();
                if let Err(e) = check {
                    // The code is spent even when the request is wrong.
                    tx.commit().await.map_err(Error::from)?;
                    return Err(e);
                }
                sqlx::query("select set_config('ruum.tenant_id', $1, true)")
                    .bind(tenant.to_string())
                    .execute(&mut *tx)
                    .await
                    .map_err(Error::from)?;
                let out = self
                    .issue(&mut tx, tenant, grant, agent_id, &audience)
                    .await;
                tx.commit().await.map_err(Error::from)?;
                out
            }
            TokenRequest::RefreshToken {
                refresh_token,
                client_id,
                resource,
            } => {
                let hash = secret::sha256(&refresh_token);
                let mut tx = self.area.system().await?;
                let r = sqlx::query(
                    "select tenant_id, id, client_id, agent_id, audience, \
                     refresh_expires_at > now() as live from agent_oauth_grants \
                     where refresh_token_hash = $1 and ended_at is null for update",
                )
                .bind(&hash[..])
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::from)?;
                let Some(r) = r else {
                    // Reuse of an old refresh token ends the grant.
                    sqlx::query(
                        "update agent_oauth_grants set ended_at = coalesce(ended_at, now()) \
                         where previous_refresh_hash = $1",
                    )
                    .bind(&hash[..])
                    .execute(&mut *tx)
                    .await
                    .map_err(Error::from)?;
                    tx.commit().await.map_err(Error::from)?;
                    return Err(OAuthError::invalid_grant("the refresh token is not valid"));
                };
                let tenant: TenantId = r.try_get("tenant_id").map_err(Error::from)?;
                let grant: Uuid = r.try_get("id").map_err(Error::from)?;
                let grant_client: String = r.try_get("client_id").map_err(Error::from)?;
                let agent_id: AgentId = r.try_get("agent_id").map_err(Error::from)?;
                let audience: String = r.try_get("audience").map_err(Error::from)?;
                let live: Option<bool> = r.try_get("live").map_err(Error::from)?;
                if grant_client != client_id {
                    return Err(OAuthError::invalid_grant(
                        "the refresh token is for another client",
                    ));
                }
                if live != Some(true) {
                    return Err(OAuthError::invalid_grant("the refresh token has expired"));
                }
                if resource
                    .as_deref()
                    .is_some_and(|r| r.trim_end_matches('/') != audience)
                {
                    return Err(OAuthError::new(
                        "invalid_target",
                        "resource does not match the authorization",
                        ErrorCode::ValidationFailed,
                    ));
                }
                let out = self
                    .issue(&mut tx, tenant, grant, agent_id, &audience)
                    .await;
                tx.commit().await.map_err(Error::from)?;
                out
            }
            TokenRequest::TokenExchange { .. } => Err(OAuthError::new(
                "unsupported_grant_type",
                "the token exchange goes to the agent store",
                ErrorCode::BadRequest,
            )),
        }
    }

    async fn end_grants(&self, tenant: TenantId, agent: AgentId) -> Result<u32> {
        let mut tx = self.area.tenant(tenant, None).await?;
        let n = sqlx::query(
            "update agent_oauth_grants set ended_at = now() where tenant_id = $1 \
             and agent_id = $2 and ended_at is null",
        )
        .bind(tenant)
        .bind(agent)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        Ok(n as u32)
    }
}
