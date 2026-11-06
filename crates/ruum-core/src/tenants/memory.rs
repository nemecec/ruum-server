//! In-memory tenant store, for unit tests. It shares the state of the area with the identity
//! and agent stores ([`MemoryArea`]).

use super::rules::{
    accepted_role, apply_policy, caller_user, check_member_limit, check_removal, check_role_change,
    require_role,
};
use super::*;
use crate::actor::{PrincipalKind, require_user_actor};
use crate::agents::DisabledReason;
use crate::error::{Error, ErrorCode};
use crate::identity::memory::{InvitationRow, MembershipRow, MemoryArea, page_of};
use crate::identity::{check_email, check_name, secret};
use crate::ids::AgentId;
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct MemoryTenantStore {
    area: Arc<MemoryArea>,
}

impl MemoryTenantStore {
    pub fn new(area: Arc<MemoryArea>) -> Self {
        Self { area }
    }

    fn member(&self, t: TenantId, u: UserId) -> Option<Member> {
        let st = self.area.lock();
        let m = st.memberships.get(&(t, u))?;
        let user = st.users.get(&u)?;
        Some(Member {
            tenant_id: t,
            user_id: u,
            email: user.user.email.clone(),
            display_name: user.user.display_name.clone(),
            role: m.role,
            joined_at: m.joined_at,
        })
    }
}

#[async_trait]
impl TenantStore for MemoryTenantStore {
    async fn list_for_user(&self, user: UserId) -> Result<Vec<(Tenant, MemberRole)>> {
        let st = self.area.lock();
        let mut v: Vec<(u64, Tenant, MemberRole)> = st
            .memberships
            .iter()
            .filter(|((_, u), _)| *u == user)
            .filter_map(|((t, _), m)| st.tenants.get(t).map(|t| (m.order, t.clone(), m.role)))
            .collect();
        v.sort_by_key(|(o, _, _)| *o);
        Ok(v.into_iter().map(|(_, t, r)| (t, r)).collect())
    }

    async fn create_team(&self, actor: &Actor, name: String) -> Result<Tenant> {
        if !actor.is_user() {
            return Err(Error::user_required());
        }
        let owner = UserId(actor.actor.id.0);
        let name = check_name("the tenant name", &name, 100)?;
        let (tenant, tuples) = {
            let mut st = self.area.lock();
            if !st.users.contains_key(&owner) {
                return Err(Error::not_found("user"));
            }
            MemoryArea::insert_tenant(&mut st, TenantKind::Team, Plan::Team, name, owner)
        };
        self.area.relate(tuples).await?;
        if let Some(hook) = self.area.root_hook() {
            hook.root_created(&tenant, owner);
        }
        let ctx = Context {
            tenant: tenant.id,
            actor: *actor,
            scope: None,
            idempotency_key: None,
        };
        self.area
            .audit(
                &ctx,
                "tenant.created",
                "tenant",
                tenant.id,
                serde_json::json!({ "name": tenant.name }),
            )
            .await?;
        Ok(tenant)
    }

    async fn get(&self, ctx: &Context) -> Result<Tenant> {
        let user = caller_user(ctx)?;
        let st = self.area.lock();
        require_role(st.role_of(ctx.tenant, user), MemberRole::Guest)?;
        st.tenants
            .get(&ctx.tenant)
            .cloned()
            .ok_or_else(|| Error::not_found("tenant"))
    }

    async fn find(&self, tenant: TenantId) -> Result<Tenant> {
        self.area
            .lock()
            .tenants
            .get(&tenant)
            .cloned()
            .ok_or_else(|| Error::not_found("tenant"))
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
        let (tenant, tuples) = {
            let mut st = self.area.lock();
            require_role(st.role_of(ctx.tenant, user), MemberRole::Admin)?;
            let t = st
                .tenants
                .get_mut(&ctx.tenant)
                .ok_or_else(|| Error::not_found("tenant"))?;
            let mut tuples = Vec::new();
            if let Some(change) = &policy {
                let new = apply_policy(&t.policy, change)?;
                if t.kind == TenantKind::Team && new.members_can_share != t.policy.members_can_share
                {
                    tuples = relations::members_can_share(t.id, new.members_can_share);
                }
                t.policy = new;
            }
            if let Some(n) = name {
                t.name = n;
            }
            (t.clone(), tuples)
        };
        self.area.relate(tuples).await?;
        self.area
            .audit(
                ctx,
                "tenant.updated",
                "tenant",
                tenant.id,
                serde_json::json!({ "name": tenant.name, "policy": tenant.policy }),
            )
            .await?;
        Ok(tenant)
    }

    async fn role_of(&self, tenant: TenantId, user: UserId) -> Result<Option<MemberRole>> {
        Ok(self.area.lock().role_of(tenant, user))
    }

    async fn member_since(&self, tenant: TenantId, user: UserId) -> Result<Option<OffsetDateTime>> {
        let st = self.area.lock();
        Ok(st
            .memberships
            .get(&(tenant, user))
            .filter(|m| m.role != MemberRole::Guest)
            .map(|m| m.role_since))
    }

    async fn is_verified_member_email(&self, tenant: TenantId, email: &str) -> Result<bool> {
        let st = self.area.lock();
        Ok(st.memberships.iter().any(|((t, u), m)| {
            *t == tenant
                && m.role != MemberRole::Guest
                && st.users.get(u).is_some_and(|r| {
                    r.user.email_verified && r.user.email.eq_ignore_ascii_case(email)
                })
        }))
    }

    async fn list_members(&self, ctx: &Context, page: PageRequest) -> Result<Page<Member>> {
        let user = caller_user(ctx)?;
        let items = {
            let st = self.area.lock();
            require_role(st.role_of(ctx.tenant, user), MemberRole::Member)?;
            let mut rows: Vec<(u64, Member)> = st
                .memberships
                .iter()
                .filter(|((t, _), _)| *t == ctx.tenant)
                .filter_map(|((t, u), m)| {
                    st.users.get(u).map(|usr| {
                        (
                            m.order,
                            Member {
                                tenant_id: *t,
                                user_id: *u,
                                email: usr.user.email.clone(),
                                display_name: usr.user.display_name.clone(),
                                role: m.role,
                                joined_at: m.joined_at,
                            },
                        )
                    })
                })
                .collect();
            rows.sort_by_key(|(o, _)| *o);
            rows.into_iter().map(|(_, m)| m).collect()
        };
        page_of(items, &page)
    }

    async fn set_member_role(
        &self,
        ctx: &Context,
        user: UserId,
        role: MemberRole,
    ) -> Result<Member> {
        let caller = require_user_actor(ctx)?;
        let (old, tuples) = {
            let mut st = self.area.lock();
            let caller_role = require_role(st.role_of(ctx.tenant, caller), MemberRole::Admin)?;
            let old = st
                .role_of(ctx.tenant, user)
                .ok_or_else(|| Error::not_found("member"))?;
            check_role_change(caller_role, old, role, st.owner_count(ctx.tenant))?;
            if old == MemberRole::Guest && role != MemberRole::Guest {
                let t = st
                    .tenants
                    .get(&ctx.tenant)
                    .ok_or_else(|| Error::not_found("tenant"))?;
                let members = st.member_count(ctx.tenant);
                check_member_limit(tenant_limits(t.plan, members).members, members)?;
            }
            if let Some(m) = st.memberships.get_mut(&(ctx.tenant, user)) {
                m.role = role;
                m.role_since = crate::now();
            }
            (
                old,
                relations::membership(ctx.tenant, user, Some(old), Some(role)),
            )
        };
        if old != role {
            self.area.relate(tuples).await?;
            self.area
                .audit(
                    ctx,
                    "member.role_changed",
                    "member",
                    user,
                    serde_json::json!({ "old_role": old, "new_role": role }),
                )
                .await?;
        }
        self.member(ctx.tenant, user)
            .ok_or_else(|| Error::not_found("member"))
    }

    async fn remove_member(&self, ctx: &Context, user: UserId) -> Result<()> {
        let caller = require_user_actor(ctx)?;
        let old = {
            let st = self.area.lock();
            let caller_role = st
                .role_of(ctx.tenant, caller)
                .ok_or_else(|| Error::not_found("tenant"))?;
            if caller != user {
                require_role(Some(caller_role), MemberRole::Admin)?;
            }
            let target_role = st
                .role_of(ctx.tenant, user)
                .ok_or_else(|| Error::not_found("member"))?;
            check_removal(
                caller,
                caller_role,
                user,
                target_role,
                st.owner_count(ctx.tenant),
            )?;
            target_role
        };
        // The sharing side first: direct grants and the user's share links.
        if let Some(c) = self.area.cleanup_hook() {
            c.member_removed(ctx, user).await?;
        }
        let ended_agents: Vec<AgentId> = {
            let mut st = self.area.lock();
            let now = crate::now();
            st.memberships.remove(&(ctx.tenant, user));
            let mut ended = Vec::new();
            for a in st.agents.values_mut() {
                if a.tenant_id == ctx.tenant && a.owner_id == user {
                    a.disabled_at = Some(now);
                    a.disabled_reason = Some(DisabledReason::OwnerLeft);
                    ended.push(a.id);
                }
            }
            for g in st.oauth_grants.values_mut() {
                if g.tenant_id == ctx.tenant && ended.contains(&g.agent_id) && g.ended_at.is_none()
                {
                    g.ended_at = Some(now);
                }
            }
            for p in st.app_passwords.values_mut() {
                if p.user_id == user && p.pw.tenant_id == ctx.tenant && p.pw.revoked_at.is_none() {
                    p.pw.revoked_at = Some(now);
                }
            }
            ended
        };
        self.area
            .relate(relations::membership(ctx.tenant, user, Some(old), None))
            .await?;
        self.area
            .audit(
                ctx,
                "member.removed",
                "member",
                user,
                serde_json::json!({ "role": old, "agents_disabled": ended_agents }),
            )
            .await
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
        let now = crate::now();
        let (inv, token, tenant_name, inviter) = {
            let mut st = self.area.lock();
            require_role(st.role_of(ctx.tenant, caller), MemberRole::Admin)?;
            let t = st
                .tenants
                .get(&ctx.tenant)
                .cloned()
                .ok_or_else(|| Error::not_found("tenant"))?;
            if role == MemberRole::Guest && !t.policy.allow_guests {
                return Err(Error::Api(
                    crate::error::ApiError::new(
                        ErrorCode::PermissionDenied,
                        "the tenant policy does not allow guests",
                    )
                    .with_hint("an admin can turn on allow_guests in the tenant policy"),
                ));
            }
            if role != MemberRole::Guest {
                let members = st.member_count(ctx.tenant);
                check_member_limit(tenant_limits(t.plan, members).members, members)?;
            }
            let open = st.invitations.values().any(|r| {
                r.inv.tenant_id == ctx.tenant
                    && r.inv.accepted_at.is_none()
                    && r.revoked_at.is_none()
                    && r.inv.email.eq_ignore_ascii_case(&email)
            });
            if open {
                return Err(Error::Api(
                    crate::error::ApiError::new(
                        ErrorCode::Conflict,
                        "an open invitation for this email exists",
                    )
                    .with_hint("revoke it first to send a new one"),
                ));
            }
            let token = secret::random_secret(INVITATION_PREFIX);
            let inv = Invitation {
                id: InvitationId::new(),
                tenant_id: ctx.tenant,
                email: email.clone(),
                role,
                invited_by: ctx.actor,
                created_at: now,
                expires_at: now + time::Duration::days(INVITATION_DAYS),
                accepted_at: None,
            };
            st.invitations.insert(
                inv.id,
                InvitationRow {
                    inv: inv.clone(),
                    token_hash: secret::sha256(&token),
                    revoked_at: None,
                    accepted_by: None,
                },
            );
            let inviter = st
                .users
                .get(&caller)
                .map(|u| u.user.display_name.clone())
                .unwrap_or_default();
            (inv, token, t.name, inviter)
        };
        self.area
            .audit(
                ctx,
                "invitation.created",
                "invitation",
                inv.id,
                serde_json::json!({ "email": inv.email, "role": inv.role }),
            )
            .await?;
        let url = accept_url(&self.area.deps.public_url, &token);
        let mail =
            mail::invitation_mail(&inv.email, &tenant_name, &inviter, &url, message.as_deref());
        if let Err(e) = self.area.deps.mailer.send(mail).await {
            tracing::warn!(error = %e, "the invitation email was not sent");
        }
        Ok((inv, token))
    }

    async fn list_invitations(&self, ctx: &Context, page: PageRequest) -> Result<Page<Invitation>> {
        let caller = require_user_actor(ctx)?;
        let now = crate::now();
        let items = {
            let st = self.area.lock();
            require_role(st.role_of(ctx.tenant, caller), MemberRole::Admin)?;
            let mut v: Vec<Invitation> = st
                .invitations
                .values()
                .filter(|r| {
                    r.inv.tenant_id == ctx.tenant
                        && r.inv.accepted_at.is_none()
                        && r.revoked_at.is_none()
                        && r.inv.expires_at > now
                })
                .map(|r| r.inv.clone())
                .collect();
            v.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
            v
        };
        page_of(items, &page)
    }

    async fn revoke_invitation(&self, ctx: &Context, id: InvitationId) -> Result<()> {
        let caller = require_user_actor(ctx)?;
        {
            let mut st = self.area.lock();
            require_role(st.role_of(ctx.tenant, caller), MemberRole::Admin)?;
            match st.invitations.get_mut(&id) {
                Some(r)
                    if r.inv.tenant_id == ctx.tenant
                        && r.inv.accepted_at.is_none()
                        && r.revoked_at.is_none() =>
                {
                    r.revoked_at = Some(crate::now());
                }
                _ => return Err(Error::not_found("invitation")),
            }
        }
        self.area
            .audit(
                ctx,
                "invitation.revoked",
                "invitation",
                id,
                serde_json::json!({}),
            )
            .await
    }

    async fn accept_invitation(&self, actor: &Actor, token: &str) -> Result<Member> {
        if !actor.is_user() || actor.actor.kind != PrincipalKind::User {
            return Err(Error::user_required());
        }
        let user = UserId(actor.actor.id.0);
        let hash = secret::sha256(token.trim());
        let now = crate::now();
        let (tenant, old, new, email, already) = {
            let mut st = self.area.lock();
            let (id, inv, accepted_by) = st
                .invitations
                .values()
                .find(|r| secret::constant_time_eq(&r.token_hash, &hash))
                .filter(|r| r.revoked_at.is_none() && r.inv.expires_at > now)
                .map(|r| (r.inv.id, r.inv.clone(), r.accepted_by))
                .ok_or_else(|| Error::not_found("invitation"))?;
            if inv.accepted_at.is_some() {
                if accepted_by == Some(user) {
                    let role = st
                        .role_of(inv.tenant_id, user)
                        .ok_or_else(|| Error::not_found("invitation"))?;
                    (inv.tenant_id, Some(role), role, inv.email, true)
                } else {
                    return Err(Error::Api(
                        crate::error::ApiError::new(
                            ErrorCode::Conflict,
                            "the invitation was already accepted",
                        )
                        .with_hint("ask the tenant admin for a new invitation"),
                    ));
                }
            } else {
                if !st.users.contains_key(&user) {
                    return Err(Error::not_found("user"));
                }
                let old = st.role_of(inv.tenant_id, user);
                let new = accepted_role(old, inv.role);
                let t = st
                    .tenants
                    .get(&inv.tenant_id)
                    .ok_or_else(|| Error::not_found("invitation"))?;
                // The policy may have changed since the invitation was sent.
                if new == MemberRole::Guest && !t.policy.allow_guests {
                    return Err(Error::Api(
                        crate::error::ApiError::new(
                            ErrorCode::PermissionDenied,
                            "the tenant policy does not allow guests",
                        )
                        .with_hint(
                            "ask the tenant admin to turn on allow_guests, or to invite you as a member",
                        ),
                    ));
                }
                if new != MemberRole::Guest && old.is_none_or(|o| o == MemberRole::Guest) {
                    let members = st.member_count(inv.tenant_id);
                    check_member_limit(tenant_limits(t.plan, members).members, members)?;
                }
                let order = st.next_order();
                st.memberships
                    .entry((inv.tenant_id, user))
                    .and_modify(|m| {
                        m.role = new;
                        m.role_since = now;
                    })
                    .or_insert(MembershipRow {
                        role: new,
                        joined_at: now,
                        role_since: now,
                        order,
                    });
                if let Some(r) = st.invitations.get_mut(&id) {
                    r.inv.accepted_at = Some(now);
                    r.accepted_by = Some(user);
                }
                (inv.tenant_id, old, new, inv.email, false)
            }
        };
        if !already {
            self.area
                .relate(relations::membership(tenant, user, old, Some(new)))
                .await?;
            let ctx = Context {
                tenant,
                actor: *actor,
                scope: None,
                idempotency_key: None,
            };
            if let Some(c) = self.area.cleanup_hook() {
                c.invitation_accepted(&ctx, &email, user).await?;
            }
            self.area
                .audit(
                    &ctx,
                    "invitation.accepted",
                    "member",
                    user,
                    serde_json::json!({ "role": new }),
                )
                .await?;
        }
        self.member(tenant, user)
            .ok_or_else(|| Error::not_found("member"))
    }

    async fn usage(&self, ctx: &Context) -> Result<Usage> {
        let user = caller_user(ctx)?;
        let st = self.area.lock();
        require_role(st.role_of(ctx.tenant, user), MemberRole::Member)?;
        Ok(Usage {
            storage_bytes: 0,
            nodes: 0,
            // The agents counted against the tenant's limit (not those of guests,
            // `agents::guest_agent_limit`).
            agents: st
                .agents
                .values()
                .filter(|a| a.tenant_id == ctx.tenant && a.disabled_at.is_none())
                .filter(|a| st.role_of(ctx.tenant, a.owner_id) != Some(MemberRole::Guest))
                .count() as u32,
            members: st.member_count(ctx.tenant),
        })
    }
}

/// The link of the invitation email: the accept page of the web UI.
pub fn accept_url(public_url: &str, token: &str) -> String {
    format!(
        "{}/invitations/accept?token={}",
        public_url.trim_end_matches('/'),
        token
    )
}
