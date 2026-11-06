//! In-memory agent store, for unit tests. It shares the state of the area with the identity and
//! tenant stores ([`MemoryArea`]).

use super::*;
use crate::actor::{PrincipalKind, require_user_actor};
use crate::identity::memory::{KeyRow, MemoryArea, State, page_of};
use crate::identity::{AccessClaims, TokenType, unauthenticated};
use crate::tenants::rules::caller_user;
use crate::tenants::{MemberRole, tenant_limits};
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct MemoryAgentStore {
    area: Arc<MemoryArea>,
}

impl MemoryAgentStore {
    pub fn new(area: Arc<MemoryArea>) -> Self {
        Self { area }
    }
}

/// How the caller relates to an agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Relation {
    Owner,
    Admin,
    /// The agent itself.
    Itself,
    None,
}

/// How the caller in `ctx` relates to `agent`.
pub(crate) fn relation(ctx: &Context, agent: &Agent, caller_role: Option<MemberRole>) -> Relation {
    if ctx.actor.actor.kind == PrincipalKind::Agent {
        return if ctx.actor.actor.id.0 == agent.id.0 {
            Relation::Itself
        } else {
            Relation::None
        };
    }
    if ctx.actor.is_user() && UserId(ctx.actor.actor.id.0) == agent.owner_id {
        Relation::Owner
    } else if ctx.actor.is_user() && caller_role.is_some_and(|r| r >= MemberRole::Admin) {
        Relation::Admin
    } else {
        Relation::None
    }
}

/// The access rules of agent records. `admins` says whether tenant admins may do it; `itself`
/// whether the agent may read its own record. Others get `not_found`; admins where only the
/// owner may get `permission_denied`.
pub(crate) fn check_access(rel: Relation, admins: bool) -> Result<()> {
    match rel {
        Relation::Owner => Ok(()),
        Relation::Admin if admins => Ok(()),
        Relation::Admin => Err(Error::Api(
            crate::error::ApiError::new(
                ErrorCode::PermissionDenied,
                "only the owner of the agent can do this",
            )
            .with_hint("ask the owner of the agent"),
        )),
        Relation::Itself => Err(Error::Api(
            crate::error::ApiError::new(
                ErrorCode::PermissionDenied,
                "an agent cannot read or change agent records",
            )
            .with_hint("read GET /v1/me for the agent's own scope"),
        )),
        Relation::None => Err(Error::not_found("agent")),
    }
}

fn agent_of(st: &State, ctx: &Context, id: AgentId) -> Result<Agent> {
    st.agents
        .get(&id)
        .filter(|a| a.tenant_id == ctx.tenant)
        .cloned()
        .ok_or_else(|| Error::not_found("agent"))
}

/// The role of the user behind the caller (for agents: their user).
fn caller_role(st: &State, ctx: &Context) -> Option<MemberRole> {
    ctx.actor
        .effective_user()
        .and_then(|u| st.role_of(ctx.tenant, u))
}

impl MemoryAgentStore {
    /// Registers an agent in the state (also for the OAuth consent). The caller has checked the
    /// request and the subtree.
    pub(crate) async fn insert(
        area: &MemoryArea,
        ctx: &Context,
        owner: UserId,
        req: NewAgent,
    ) -> Result<Agent> {
        let agent = {
            let mut st = area.lock();
            let t = st
                .tenants
                .get(&ctx.tenant)
                .cloned()
                .ok_or_else(|| Error::not_found("tenant"))?;
            let Some(role) = st.role_of(ctx.tenant, owner) else {
                return Err(Error::not_found("tenant"));
            };
            // A guest's agents have their own limit, so guests cannot use up the tenant's
            // (`agent_pool`, security review of 2026-11-06).
            let guest = role == MemberRole::Guest;
            let limit = if guest {
                guest_agent_limit(t.plan)
            } else {
                tenant_limits(t.plan, st.member_count(ctx.tenant)).agents
            };
            let live = st
                .agents
                .values()
                .filter(|a| a.tenant_id == ctx.tenant && a.disabled_at.is_none())
                .filter(|a| {
                    let owner_is_guest =
                        st.role_of(ctx.tenant, a.owner_id) == Some(MemberRole::Guest);
                    if guest {
                        a.owner_id == owner
                    } else {
                        !owner_is_guest
                    }
                })
                .count() as u32;
            if live >= limit {
                return Err(agent_limit_reached(limit));
            }
            let agent = Agent {
                id: AgentId::new(),
                tenant_id: ctx.tenant,
                owner_id: owner,
                name: req.name,
                description: req.description,
                scope: req.scope,
                disabled_at: None,
                disabled_reason: None,
                created_at: crate::now(),
                last_used_at: None,
            };
            st.agents.insert(agent.id, agent.clone());
            agent
        };
        area.relate(crate::tenants::relations::agent_registered(
            agent.id,
            agent.tenant_id,
            owner,
        ))
        .await?;
        area.audit(
            ctx,
            "agent.registered",
            "agent",
            agent.id,
            serde_json::json!({ "name": agent.name, "scope": agent.scope }),
        )
        .await?;
        Ok(agent)
    }

    /// Issues an access token for an agent (key exchange and OAuth).
    pub(crate) fn token_for(
        area: &MemoryArea,
        agent: &Agent,
        audience: Option<&str>,
    ) -> Result<AccessToken> {
        let tokens = &area.deps.tokens;
        let mut c = AccessClaims::new(
            tokens.as_ref(),
            TokenType::Agent,
            agent.id.0,
            ACCESS_TOKEN_SECONDS,
        );
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

    /// Disables an agent and ends its OAuth grants, in the state.
    pub(crate) fn disable_in(st: &mut State, id: AgentId, reason: DisabledReason) {
        let now = crate::now();
        if let Some(a) = st.agents.get_mut(&id)
            && a.disabled_at.is_none()
        {
            a.disabled_at = Some(now);
            a.disabled_reason = Some(reason);
        }
        for g in st.oauth_grants.values_mut() {
            if g.agent_id == id && g.ended_at.is_none() {
                g.ended_at = Some(now);
            }
        }
    }
}

#[async_trait]
impl AgentStore for MemoryAgentStore {
    async fn register(&self, ctx: &Context, req: NewAgent) -> Result<Agent> {
        let owner = require_user_actor(ctx)?;
        let req = check_new_agent(&req)?;
        if let Some(s) = req.scope.subtree {
            self.area.check_node(ctx, s).await?;
        }
        Self::insert(&self.area, ctx, owner, req).await
    }

    async fn list(
        &self,
        ctx: &Context,
        owner: Option<UserId>,
        page: PageRequest,
    ) -> Result<Page<Agent>> {
        let user = caller_user(ctx)?;
        let items = {
            let st = self.area.lock();
            let role = caller_role(&st, ctx);
            if role.is_none() {
                return Err(Error::not_found("tenant"));
            }
            let me_agent = (ctx.actor.actor.kind == PrincipalKind::Agent)
                .then_some(AgentId(ctx.actor.actor.id.0));
            let admin = ctx.actor.is_user() && role.is_some_and(|r| r >= MemberRole::Admin);
            let mut v: Vec<Agent> = st
                .agents
                .values()
                .filter(|a| a.tenant_id == ctx.tenant)
                .filter(|a| match me_agent {
                    Some(me) => a.id == me,
                    None if admin => owner.is_none_or(|o| a.owner_id == o),
                    None => a.owner_id == user,
                })
                .cloned()
                .collect();
            v.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
            v
        };
        page_of(items, &page)
    }

    async fn get(&self, ctx: &Context, id: AgentId) -> Result<Agent> {
        caller_user(ctx)?;
        let st = self.area.lock();
        let agent = agent_of(&st, ctx, id)?;
        check_access(relation(ctx, &agent, caller_role(&st, ctx)), true)?;
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
        {
            let st = self.area.lock();
            let agent = agent_of(&st, ctx, id)?;
            check_access(relation(ctx, &agent, caller_role(&st, ctx)), false)?;
        }
        if let Some(sub) = change.scope.as_ref().and_then(|s| s.subtree) {
            self.area.check_node(ctx, sub).await?;
        }
        let agent = {
            let mut st = self.area.lock();
            let a = st
                .agents
                .get_mut(&id)
                .filter(|a| a.tenant_id == ctx.tenant)
                .ok_or_else(|| Error::not_found("agent"))?;
            if let Some(n) = name {
                a.name = n;
            }
            if let Some(d) = description {
                a.description = d;
            }
            if let Some(s) = change.scope {
                a.scope = s;
            }
            a.clone()
        };
        self.area
            .audit(
                ctx,
                "agent.updated",
                "agent",
                id,
                serde_json::json!({ "name": agent.name, "scope": agent.scope }),
            )
            .await?;
        Ok(agent)
    }

    async fn set_disabled(&self, ctx: &Context, id: AgentId, disabled: bool) -> Result<Agent> {
        require_user_actor(ctx)?;
        let (agent, changed) = {
            let mut st = self.area.lock();
            let agent = agent_of(&st, ctx, id)?;
            let rel = relation(ctx, &agent, caller_role(&st, ctx));
            check_access(rel, disabled)?;
            if disabled {
                if agent.disabled_at.is_some() {
                    (agent, false)
                } else {
                    let reason = if rel == Relation::Owner {
                        DisabledReason::ByOwner
                    } else {
                        DisabledReason::ByAdmin
                    };
                    MemoryAgentStore::disable_in(&mut st, id, reason);
                    (agent_of(&st, ctx, id)?, true)
                }
            } else {
                match agent.disabled_reason {
                    None => (agent, false),
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
                        if let Some(a) = st.agents.get_mut(&id) {
                            a.disabled_at = None;
                            a.disabled_reason = None;
                        }
                        (agent_of(&st, ctx, id)?, true)
                    }
                }
            }
        };
        if changed {
            let action = if disabled {
                "agent.disabled"
            } else {
                "agent.enabled"
            };
            self.area
                .audit(
                    ctx,
                    action,
                    "agent",
                    id,
                    serde_json::json!({ "reason": agent.disabled_reason }),
                )
                .await?;
        }
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
        let (key, secret) = {
            let mut st = self.area.lock();
            let agent = agent_of(&st, ctx, id)?;
            check_access(relation(ctx, &agent, caller_role(&st, ctx)), false)?;
            let (secret, hash) = generate_key();
            let key = AgentKey {
                id: AgentKeyId::new(),
                agent_id: id,
                name,
                prefix: crate::identity::secret::display_prefix(&secret, API_KEY_PREFIX),
                created_at: crate::now(),
                expires_at,
                last_used_at: None,
                revoked_at: None,
            };
            st.keys.insert(
                key.id,
                KeyRow {
                    key: key.clone(),
                    tenant_id: ctx.tenant,
                    hash,
                },
            );
            (key, secret)
        };
        self.area
            .audit(
                ctx,
                "agent.key_created",
                "agent_key",
                key.id,
                serde_json::json!({ "agent_id": id, "prefix": key.prefix }),
            )
            .await?;
        Ok((key, secret))
    }

    async fn list_keys(&self, ctx: &Context, id: AgentId) -> Result<Vec<AgentKey>> {
        require_user_actor(ctx)?;
        let st = self.area.lock();
        let agent = agent_of(&st, ctx, id)?;
        check_access(relation(ctx, &agent, caller_role(&st, ctx)), true)?;
        let mut v: Vec<AgentKey> = st
            .keys
            .values()
            .filter(|k| k.tenant_id == ctx.tenant && k.key.agent_id == id)
            .map(|k| k.key.clone())
            .collect();
        v.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        Ok(v)
    }

    async fn revoke_key(&self, ctx: &Context, id: AgentId, key: AgentKeyId) -> Result<()> {
        require_user_actor(ctx)?;
        {
            let mut st = self.area.lock();
            let agent = agent_of(&st, ctx, id)?;
            check_access(relation(ctx, &agent, caller_role(&st, ctx)), true)?;
            match st.keys.get_mut(&key) {
                Some(k) if k.tenant_id == ctx.tenant && k.key.agent_id == id => {
                    if k.key.revoked_at.is_none() {
                        k.key.revoked_at = Some(crate::now());
                    }
                }
                _ => return Err(Error::not_found("key")),
            }
        }
        self.area
            .audit(
                ctx,
                "agent.key_revoked",
                "agent_key",
                key,
                serde_json::json!({ "agent_id": id }),
            )
            .await
    }

    async fn exchange_key(&self, secret: &str) -> Result<AccessToken> {
        let refuse = || {
            unauthenticated(
                "the API key is not valid",
                "make a new key for the agent in the web UI",
            )
        };
        if !secret.starts_with(API_KEY_PREFIX) {
            return Err(refuse());
        }
        let hash = hash_key(secret);
        let now = crate::now();
        let agent = {
            let mut st = self.area.lock();
            let (kid, agent_id) = st
                .keys
                .values()
                .find(|k| crate::identity::secret::constant_time_eq(&k.hash, &hash))
                .filter(|k| k.key.revoked_at.is_none() && k.key.expires_at > now)
                .map(|k| (k.key.id, k.key.agent_id))
                .ok_or_else(refuse)?;
            let agent = st.agents.get(&agent_id).cloned().ok_or_else(refuse)?;
            check_live(&agent)?;
            if let Some(k) = st.keys.get_mut(&kid) {
                k.key.last_used_at = Some(now);
            }
            if let Some(a) = st.agents.get_mut(&agent_id) {
                a.last_used_at = Some(now);
            }
            agent
        };
        let mut t = Self::token_for(&self.area, &agent, None)?;
        t.issued_token_type = Some(ACCESS_TOKEN_TYPE.into());
        Ok(t)
    }

    async fn authenticate(&self, tenant: TenantId, id: AgentId) -> Result<Agent> {
        let mut st = self.area.lock();
        let a = st
            .agents
            .get_mut(&id)
            .filter(|a| a.tenant_id == tenant)
            .ok_or_else(|| {
                unauthenticated(
                    "the agent of the token does not exist",
                    "get a new access token",
                )
            })?;
        check_live(a)?;
        let now = crate::now();
        if a.last_used_at
            .is_none_or(|t| now - t >= time::Duration::seconds(AGENT_TOUCH_SECONDS))
        {
            a.last_used_at = Some(now);
        }
        Ok(a.clone())
    }

    async fn activity(
        &self,
        ctx: &Context,
        id: AgentId,
        since: Option<OffsetDateTime>,
        page: PageRequest,
    ) -> Result<Page<JournalEntry>> {
        caller_user(ctx)?;
        {
            let st = self.area.lock();
            let agent = agent_of(&st, ctx, id)?;
            check_access(relation(ctx, &agent, caller_role(&st, ctx)), true)?;
        }
        let Some(journal) = &self.area.deps.journal else {
            return Ok(Page::empty());
        };
        let entries = undo::entries_of(journal.as_ref(), ctx, id, since, None).await?;
        page_of(entries, &page)
    }
}
