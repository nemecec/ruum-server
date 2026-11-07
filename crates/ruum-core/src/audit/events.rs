//! Audit events of sign-in and credential use, for the identity and agents area.
//!
//! The auth code calls these helpers where the event happens. Each helper builds the
//! [`Context`] of the event (the tenant and the principal) and calls [`AuditLog::record`].
//! A helper returns the error of the store; the caller decides whether a failed audit write
//! fails the request (for sign-in and key exchange it should: section 15).
//!
//! Action names are stable: the audit filter `action` matches by prefix (`auth.`, `agent.`).

use super::{AuditLog, NewAuditEntry};
use crate::actor::{Actor, Context, PrincipalKind, PrincipalRef};
use crate::error::Result;
use crate::ids::{AgentId, AgentKeyId, AppPasswordId, TenantId, UserId};
use serde_json::json;
use uuid::Uuid;

/// The action names of the audit log. Other areas may add their own (`member.removed`,
/// `tenant.updated`, ...) with the same dotted form.
pub mod actions {
    /// A user signed in (web session). Recorded in the user's home tenant at sign-in
    /// ([`super::sign_in`]), and once per session in each other tenant where the user is a
    /// member, admin or owner, at the first use of the session there
    /// ([`super::session_first_use`], `data.via = session_first_use`). Never in a tenant where
    /// the user is a guest (decision 0003-4).
    pub const AUTH_LOGIN: &str = "auth.login";
    /// A sign-in of a known user failed after the identity provider answered (no secrets in
    /// `data`). Only in the user's home tenant; failures without a known user go to no tenant.
    pub const AUTH_LOGIN_FAILED: &str = "auth.login_failed";
    /// A user signed out, or ended one of their sessions.
    pub const AUTH_LOGOUT: &str = "auth.logout";
    /// A WebDAV app password signed in.
    pub const AUTH_APP_PASSWORD_USED: &str = "auth.app_password_used";
    /// An agent API key was exchanged for an access token.
    pub const AGENT_KEY_USED: &str = "agent.key_used";
    /// An agent API key was refused (expired, revoked, agent disabled).
    pub const AGENT_KEY_REFUSED: &str = "agent.key_refused";
    /// An OAuth access or refresh token was issued to an agent (MCP clients).
    pub const AGENT_TOKEN_ISSUED: &str = "agent.token_issued";
    pub const AGENT_REGISTERED: &str = "agent.registered";
    pub const AGENT_UPDATED: &str = "agent.updated";
    pub const AGENT_DISABLED: &str = "agent.disabled";
    pub const AGENT_ENABLED: &str = "agent.enabled";
    pub const AGENT_KEY_CREATED: &str = "agent.key_created";
    pub const AGENT_KEY_REVOKED: &str = "agent.key_revoked";
    pub const AGENT_UNDO: &str = "agent.undo";
    pub const MEMBER_ADDED: &str = "member.added";
    pub const MEMBER_UPDATED: &str = "member.updated";
    pub const MEMBER_REMOVED: &str = "member.removed";
    pub const INVITATION_CREATED: &str = "invitation.created";
    pub const INVITATION_REVOKED: &str = "invitation.revoked";
    pub const INVITATION_ACCEPTED: &str = "invitation.accepted";
    pub const TENANT_CREATED: &str = "tenant.created";
    pub const TENANT_UPDATED: &str = "tenant.updated";
    pub const APP_PASSWORD_CREATED: &str = "app_password.created";
    pub const APP_PASSWORD_REVOKED: &str = "app_password.revoked";
}

/// The client of the request: IP address and user agent. Fill it from the connection (or the
/// trusted proxy header) and the `User-Agent` header.
#[derive(Clone, Debug, Default)]
pub struct ClientInfo {
    pub ip: Option<String>,
    pub user_agent: Option<String>,
}

/// The context of an audit event of `actor` in `tenant`.
pub fn context(tenant: TenantId, actor: Actor) -> Context {
    Context {
        tenant,
        actor,
        scope: None,
        idempotency_key: None,
    }
}

/// The actor of an agent that acts for `owner`.
pub fn agent_actor(agent: AgentId, owner: UserId) -> Actor {
    Actor {
        actor: PrincipalRef {
            kind: PrincipalKind::Agent,
            id: agent.into(),
        },
        on_behalf_of: Some(owner),
    }
}

/// A user signed in. `method` names how (`passkey`, `google`, `microsoft`, `oidc`). `session`
/// is the `sessions.id` row ID of the new web session, if any (never derived from the cookie
/// secret). The sign-in writes this in the user's home tenant only.
pub async fn sign_in(
    log: &dyn AuditLog,
    tenant: TenantId,
    user: UserId,
    method: &str,
    session: Option<Uuid>,
    client: &ClientInfo,
) -> Result<()> {
    let mut data = json!({ "method": method });
    if let Some(s) = session {
        data["session_id"] = json!(s);
    }
    log.record(
        &context(tenant, Actor::user(user)),
        NewAuditEntry::new(actions::AUTH_LOGIN)
            .target("user", user)
            .client(client.ip.clone(), client.user_agent.clone())
            .data(data),
    )
    .await
}

/// The first use of a web session in a tenant that is not the user's home tenant, where the user
/// is a member, admin or owner (the caller checks the role; guests get no row). Writes the
/// `session_tenants` marker and one `auth.login` with `data.via = session_first_use` in one
/// transaction; `Ok(false)` when the session was used there before. `session` is the
/// `sessions.id` row ID.
pub async fn session_first_use(
    log: &dyn AuditLog,
    tenant: TenantId,
    user: UserId,
    session: Uuid,
    client: &ClientInfo,
) -> Result<bool> {
    log.record_first_use(
        &context(tenant, Actor::user(user)),
        session,
        NewAuditEntry::new(actions::AUTH_LOGIN)
            .target("user", user)
            .client(client.ip.clone(), client.user_agent.clone())
            .data(json!({ "via": "session_first_use", "session_id": session })),
    )
    .await
}

/// A sign-in of a known user failed. `reason` is a short code, never a secret.
pub async fn sign_in_failed(
    log: &dyn AuditLog,
    tenant: TenantId,
    user: UserId,
    reason: &str,
    client: &ClientInfo,
) -> Result<()> {
    log.record(
        &context(tenant, Actor::user(user)),
        NewAuditEntry::new(actions::AUTH_LOGIN_FAILED)
            .target("user", user)
            .client(client.ip.clone(), client.user_agent.clone())
            .data(json!({ "reason": reason })),
    )
    .await
}

/// A user signed out or ended a session.
pub async fn sign_out(
    log: &dyn AuditLog,
    tenant: TenantId,
    user: UserId,
    client: &ClientInfo,
) -> Result<()> {
    log.record(
        &context(tenant, Actor::user(user)),
        NewAuditEntry::new(actions::AUTH_LOGOUT)
            .target("user", user)
            .client(client.ip.clone(), client.user_agent.clone()),
    )
    .await
}

/// An agent API key was exchanged for an access token (`exchangeToken`). The actor is the
/// agent, for its owner.
pub async fn agent_key_used(
    log: &dyn AuditLog,
    tenant: TenantId,
    agent: AgentId,
    owner: UserId,
    key: AgentKeyId,
    client: &ClientInfo,
) -> Result<()> {
    log.record(
        &context(tenant, agent_actor(agent, owner)),
        agent_key_used_entry(agent, key, client),
    )
    .await
}

/// The entry of [`agent_key_used`], for stores that write it in their own transaction.
pub fn agent_key_used_entry(agent: AgentId, key: AgentKeyId, client: &ClientInfo) -> NewAuditEntry {
    NewAuditEntry::new(actions::AGENT_KEY_USED)
        .target("agent_key", key)
        .client(client.ip.clone(), client.user_agent.clone())
        .data(json!({ "agent_id": agent }))
}

/// An agent API key was refused. `reason`: `expired`, `revoked`, `agent_disabled`, ...
pub async fn agent_key_refused(
    log: &dyn AuditLog,
    tenant: TenantId,
    agent: AgentId,
    owner: UserId,
    key: AgentKeyId,
    reason: &str,
    client: &ClientInfo,
) -> Result<()> {
    log.record(
        &context(tenant, agent_actor(agent, owner)),
        agent_key_refused_entry(agent, key, reason, client),
    )
    .await
}

/// The entry of [`agent_key_refused`], for stores that write it in their own transaction.
pub fn agent_key_refused_entry(
    agent: AgentId,
    key: AgentKeyId,
    reason: &str,
    client: &ClientInfo,
) -> NewAuditEntry {
    NewAuditEntry::new(actions::AGENT_KEY_REFUSED)
        .target("agent_key", key)
        .client(client.ip.clone(), client.user_agent.clone())
        .data(json!({ "agent_id": agent, "reason": reason }))
}

/// An OAuth token was issued to an agent. `grant` is `authorization_code` or `refresh_token`.
pub async fn agent_token_issued(
    log: &dyn AuditLog,
    tenant: TenantId,
    agent: AgentId,
    owner: UserId,
    grant: &str,
    client: &ClientInfo,
) -> Result<()> {
    log.record(
        &context(tenant, agent_actor(agent, owner)),
        agent_token_issued_entry(agent, grant, client),
    )
    .await
}

/// The entry of [`agent_token_issued`], for stores that write it in their own transaction.
pub fn agent_token_issued_entry(agent: AgentId, grant: &str, client: &ClientInfo) -> NewAuditEntry {
    NewAuditEntry::new(actions::AGENT_TOKEN_ISSUED)
        .target("agent", agent)
        .client(client.ip.clone(), client.user_agent.clone())
        .data(json!({ "grant": grant }))
}

/// A WebDAV app password signed in. The actor is the user (decision 0001-7).
pub async fn app_password_used(
    log: &dyn AuditLog,
    tenant: TenantId,
    user: UserId,
    password: AppPasswordId,
    client: &ClientInfo,
) -> Result<()> {
    log.record(
        &context(tenant, Actor::user(user)),
        app_password_used_entry(password, client),
    )
    .await
}

/// How often `auth.app_password_used` is written for one app password, at most: once per this
/// many seconds (decision 0003-4). The stores check the audit table itself, a shared store.
pub const APP_PASSWORD_AUDIT_SECONDS: i64 = 3600;

/// The entry of [`app_password_used`], for stores that write it in their own transaction.
pub fn app_password_used_entry(password: AppPasswordId, client: &ClientInfo) -> NewAuditEntry {
    NewAuditEntry::new(actions::AUTH_APP_PASSWORD_USED)
        .target("app_password", password)
        .client(client.ip.clone(), client.user_agent.clone())
}
