//! Agents (`docs/DESIGN.md`, section 3.4 and 11): agent principals with their own identity, their
//! scope, API keys and the exchange of a key for a short-lived access token, activity and undo.
//!
//! - An agent belongs to one user (decision 13: no team agents yet) and acts for that user.
//! - API keys start with [`API_KEY_PREFIX`], are stored only as a hash and have an expiry.
//! - The agent's right is the user's right intersected with the agent's scope.
//! - If the user leaves the tenant, the agent is disabled.
//! - Only a user can register, change, enable, disable or undo an agent, or make its keys
//!   ([`crate::actor::require_user_actor`]); an agent can never widen its own scope.
//! - MCP clients connect with OAuth ([`oauth`]); scripts and the CLI use API keys.

pub mod memory;
pub mod oauth;
pub mod postgres;
pub mod undo;

#[cfg(test)]
mod tests;

use crate::actor::Context;
use crate::authz::{Role, Scope};
use crate::error::{Error, ErrorCode, Result};
use crate::ids::{AgentId, AgentKeyId, NodeId, TenantId, UserId};
use crate::journal::{ChangeInfo, JournalEntry};
use crate::paging::{Page, PageRequest};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// The visible prefix of agent API keys, so secret scanners find leaked keys.
pub const API_KEY_PREFIX: &str = "ruum_agent_";

/// `AgentStore::authenticate` writes `agents.last_used_at` at most this often (seconds), so a
/// request of an agent is a read, not a row lock.
pub const AGENT_TOUCH_SECONDS: i64 = 300;
/// The lifetime of an access token from a key exchange.
pub const ACCESS_TOKEN_SECONDS: i64 = 15 * 60;
/// The default lifetime of an API key.
pub const KEY_DEFAULT_DAYS: i64 = 90;
/// The longest lifetime of an API key.
pub const KEY_MAX_DAYS: i64 = 366;
/// The `subject_token_type` of an API key in the token exchange.
pub const API_KEY_TOKEN_TYPE: &str = "urn:ruum:params:oauth:token-type:api-key";
/// The `issued_token_type` of an access token.
pub const ACCESS_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";

/// A monthly budget with a hard stop (section 11). Enforcement grows with later steps.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    pub max_storage_bytes: Option<i64>,
    pub max_writes_per_hour: Option<u32>,
}

/// What an agent may touch: one subtree, roles up to `max_role`, until a time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentScope {
    /// The folder the agent may touch. `None` means all nodes the user can reach in the tenant.
    pub subtree: Option<NodeId>,
    /// The highest role the agent may use. Its right is never higher than the user's.
    pub max_role: Role,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub expires_at: Option<OffsetDateTime>,
    #[serde(default)]
    pub budget: Budget,
}

impl AgentScope {
    /// The part of the scope that limits rights, for [`Context::scope`].
    pub fn as_scope(&self) -> Scope {
        Scope {
            subtree: self.subtree,
            max_role: self.max_role,
            expires_at: self.expires_at,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Agent {
    pub id: AgentId,
    pub tenant_id: TenantId,
    /// The user the agent acts for.
    pub owner_id: UserId,
    pub name: String,
    pub description: Option<String>,
    pub scope: AgentScope,
    #[serde(with = "time::serde::rfc3339::option")]
    pub disabled_at: Option<OffsetDateTime>,
    pub disabled_reason: Option<DisabledReason>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_used_at: Option<OffsetDateTime>,
}

/// Why an agent is disabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisabledReason {
    ByOwner,
    ByAdmin,
    /// The user left the tenant. Such an agent cannot be enabled again.
    OwnerLeft,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewAgent {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub scope: AgentScope,
}

/// A change of an agent: fields that are `None` keep their value. `description: Some(None)`
/// clears the description.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AgentUpdate {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub scope: Option<AgentScope>,
}

/// Metadata of an API key. The secret is shown only once, at creation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentKey {
    pub id: AgentKeyId,
    pub agent_id: AgentId,
    pub name: Option<String>,
    /// The first characters of the key, for example `ruum_agent_3fK9`, to recognise it.
    pub prefix: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_used_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
}

/// The result of a key exchange.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccessToken {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: i64,
    /// Only for the token exchange: `urn:ietf:params:oauth:token-type:access_token`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued_token_type: Option<String>,
}

/// What to undo: the agent's entries between `since` and `until`, or only the entries in `seqs`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UndoRequest {
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub since: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub until: Option<OffsetDateTime>,
    pub seqs: Option<Vec<i64>>,
    pub dry_run: bool,
}

/// Why an undo skipped an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UndoSkipReason {
    /// Another actor changed the node later.
    ChangedLater,
    /// The event has no inverse (`node.purged`, `share_link.revoked`).
    Irreversible,
    /// The caller may not do the inverse.
    PermissionDenied,
}

/// The inverse of each event type, as in the `undoAgent` description of `openapi.yaml`.
/// `None` means the event cannot be undone.
pub fn undo_action(event: crate::journal::EventType) -> Option<&'static str> {
    use crate::journal::EventType::*;
    match event {
        NodeCreated | NodeCopied => Some("trash the node"),
        NodeRenamed => Some("rename back"),
        NodeMoved => Some("move back to the old parent and name"),
        NodeTrashed => Some("restore"),
        NodeRestored => Some("trash again"),
        NodePurged => None,
        VersionCreated | VersionRestored => Some("restore the version before it"),
        VersionLabeled => Some("set the old label"),
        TagAttached => Some("detach the tag"),
        TagDetached => Some("attach the tag"),
        GrantAdded => Some("remove the grant"),
        GrantUpdated => Some("set the old role"),
        GrantRemoved => Some("add the grant again"),
        ShareLinkCreated => Some("revoke the link"),
        ShareLinkRevoked => None,
        // Only a person confirms classes (card K-1); an agent never wrote one.
        DatabaseColumnsConfirmed => None,
        NodeSharingUpdated => Some("set the old editors_can_share"),
        // Only people give grants, so an agent never writes `database.grant_created`; its
        // inverse would end the grant. A grant that ended (given back) cannot start again: a
        // new grant needs the agent's user and a new restore point.
        DatabaseGrantCreated => Some("end the grant"),
        DatabaseGrantRevoked => None,
        // Only a person decides an approval (card A-2); an agent never wrote one.
        DatabaseApprovalDecided => None,
        // `app.settings_changed` names the new values only, so there is no old value to set
        // back (card W-1); an agent can only lower the settings anyway.
        AppSettingsChanged => None,
        // An agent's preview expires on its own and a failed deployment changed nothing; only a
        // person publishes or rolls back (an agent's publish waits for a person, card W-2s).
        AppDeployed | AppDeploymentFailed | AppRolledBack => None,
        AppRoleGranted => Some("remove the app role grant"),
        AppRoleRevoked => Some("add the app role grant again"),
    }
}

/// One change that an undo reverts, or could not revert.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UndoItem {
    pub seq: i64,
    pub node_id: Option<NodeId>,
    #[serde(rename = "type")]
    pub event: crate::journal::EventType,
    /// Why the change was skipped; `None` for a reverted change.
    pub reason: Option<UndoSkipReason>,
    /// For `changed_later`: the first later change of the node by another actor, the one that
    /// blocked the undo (C-11, decision 0003-6). `None` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_by: Option<crate::actor::Actor>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UndoReport {
    pub dry_run: bool,
    pub reverted: Vec<UndoItem>,
    pub skipped: Vec<UndoItem>,
    /// The last change of the undo; `None` for a dry run or when nothing was reverted.
    pub change: Option<ChangeInfo>,
}

/// Makes a new API key. Returns the secret (shown once) and its hash (stored).
pub fn generate_key() -> (String, [u8; 32]) {
    let secret = crate::identity::secret::random_secret(API_KEY_PREFIX);
    let hash = hash_key(&secret);
    (secret, hash)
}

/// The stored hash of a key (SHA-256; keys have 256 bits of entropy, so a slow hash is not
/// needed).
pub fn hash_key(secret: &str) -> [u8; 32] {
    crate::identity::secret::sha256(secret)
}

/// Checks the name, description and scope of a new or changed agent. The scope subtree is
/// checked by the store (it must be a node of the tenant).
pub fn check_new_agent(req: &NewAgent) -> Result<NewAgent> {
    let name = crate::identity::check_name("the agent name", &req.name, 100)?;
    let description = check_description(req.description.clone())?;
    check_scope(&req.scope)?;
    Ok(NewAgent {
        name,
        description,
        scope: req.scope.clone(),
    })
}

/// The longest agent name (`agents.name` checks the same).
pub const AGENT_NAME_MAX: usize = 100;

/// The name to store for a new or renamed agent (decision 0003-5): `name` itself, or
/// `name (2)`, `name (3)`, ... when another agent of the same owner in the tenant already has
/// that name, ignoring case. Agent names come from the client (OAuth dynamic registration), so
/// two agents of one owner must not look the same in the activity log. The base name is cut so
/// that the result has at most [`AGENT_NAME_MAX`] characters. `taken` holds the names of the
/// owner's other agents in the tenant.
pub fn unique_agent_name<'a>(name: &str, taken: impl IntoIterator<Item = &'a str>) -> String {
    let taken: std::collections::HashSet<String> =
        taken.into_iter().map(|t| t.to_lowercase()).collect();
    if !taken.contains(&name.to_lowercase()) {
        return name.to_string();
    }
    let mut n = 2u32;
    loop {
        let suffix = format!(" ({n})");
        let base: String = name
            .chars()
            .take(AGENT_NAME_MAX - suffix.chars().count())
            .collect();
        let candidate = format!("{}{suffix}", base.trim_end());
        if !taken.contains(&candidate.to_lowercase()) {
            return candidate;
        }
        n += 1;
    }
}

/// Checks a description: at most 1000 characters; empty means none.
pub fn check_description(d: Option<String>) -> Result<Option<String>> {
    match d {
        Some(d) if d.chars().count() > 1000 => Err(crate::identity::invalid(
            "the description must have at most 1000 characters",
        )),
        Some(d) if d.trim().is_empty() => Ok(None),
        other => Ok(other),
    }
}

/// Checks a scope: an expiry in the future, budgets not negative.
pub fn check_scope(scope: &AgentScope) -> Result<()> {
    if scope.expires_at.is_some_and(|e| e <= crate::now()) {
        return Err(crate::identity::invalid(
            "the scope expiry must be in the future",
        ));
    }
    if scope.budget.max_storage_bytes.is_some_and(|b| b < 0) {
        return Err(crate::identity::invalid(
            "max_storage_bytes must not be negative",
        ));
    }
    Ok(())
}

/// The expiry of a new key: the requested time (in the future, at most [`KEY_MAX_DAYS`] ahead),
/// or [`KEY_DEFAULT_DAYS`] from now.
pub fn key_expiry(requested: Option<OffsetDateTime>) -> Result<OffsetDateTime> {
    let now = crate::now();
    match requested {
        None => Ok(now + time::Duration::days(KEY_DEFAULT_DAYS)),
        Some(t) if t <= now => Err(crate::identity::invalid("expires_at must be in the future")),
        Some(t) if t > now + time::Duration::days(KEY_MAX_DAYS) => Err(crate::identity::invalid(
            "an API key expires at most one year ahead",
        )),
        Some(t) => Ok(t),
    }
}

/// The error of a disabled agent.
pub fn agent_disabled() -> Error {
    Error::Api(
        crate::error::ApiError::new(ErrorCode::AgentDisabled, "the agent is disabled")
            .with_hint("the owner of the agent can enable it again in the web UI"),
    )
}

/// The error of an agent whose scope has expired (`scope.expires_at`).
pub fn agent_scope_expired() -> Error {
    Error::Api(
        crate::error::ApiError::new(
            ErrorCode::AgentDisabled,
            "the scope of the agent has expired",
        )
        .with_hint("the owner of the agent can give it a new scope expiry in the web UI"),
    )
}

/// Refuses an agent that is disabled or whose scope has expired: the agent works "until time
/// T" (design 3.3), for every operation, not only for node rights. Every path that
/// authenticates an agent or issues it a token calls this (`AgentStore::authenticate`,
/// `exchange_key`, the OAuth code and refresh grants).
pub fn check_live(agent: &Agent) -> crate::error::Result<()> {
    if let Some(reason) = refusal(agent) {
        return Err(if reason == "agent_disabled" {
            agent_disabled()
        } else {
            agent_scope_expired()
        });
    }
    Ok(())
}

/// Why [`check_live`] refuses the agent (`agent_disabled`, `scope_expired`), as the reason of
/// an `agent.key_refused` audit entry.
pub fn refusal(agent: &Agent) -> Option<&'static str> {
    if agent.disabled_at.is_some() {
        return Some("agent_disabled");
    }
    if agent.scope.expires_at.is_some_and(|t| t <= crate::now()) {
        return Some("scope_expired");
    }
    None
}

/// How many live agents one guest may own in a tenant: the allowance of one member of the plan.
///
/// The tenant's agent limit grows only with its members (not guests, `tenant_limits`), so the
/// agents of guests are counted apart: each guest has this limit of their own, and the tenant's
/// limit counts only the agents of owners, admins and members. So a guest cannot use up the
/// host tenant's agents (security review of 2026-11-06). Guests may own agents in step 1
/// (decision 0002-2), but not while the tenant policy has `allow_guests: false`
/// ([`guest_agents_refused`]).
pub fn guest_agent_limit(plan: crate::tenants::Plan) -> u32 {
    crate::tenants::limits::plan_limits(plan).agents
}

/// The error for a guest who registers an agent (`registerAgent`, `oauthConsent` with
/// `new_agent`) while the tenant policy has `allow_guests: false` (decision 0002-2).
pub fn guest_agents_refused() -> Error {
    Error::Api(
        crate::error::ApiError::new(
            ErrorCode::PermissionDenied,
            "the tenant policy does not allow guests, so a guest cannot register an agent",
        )
        .with_hint("ask a tenant admin to allow guests, or to make you a member"),
    )
}

/// The error of the agent limit of the plan.
pub fn agent_limit_reached(limit: u32) -> Error {
    Error::Api(
        crate::error::ApiError::new(
            ErrorCode::PlanLimitReached,
            format!("the tenant has reached its limit of {limit} agents"),
        )
        .with_hint("disable or remove an agent, or move to a larger plan"),
    )
}

#[async_trait]
pub trait AgentStore: Send + Sync {
    // Every write below is for users only: it calls `require_user_actor` first.

    /// Registers an agent for the actor's user. Fails with `plan_limit_reached` at the limit. A
    /// scope subtree outside the tenant is `not_found`.
    async fn register(&self, ctx: &Context, req: NewAgent) -> Result<Agent>;
    /// The caller's agents; tenant admins see all agents of the tenant (`owner` filters, for
    /// admins). An agent caller sees itself.
    async fn list(
        &self,
        ctx: &Context,
        owner: Option<UserId>,
        page: PageRequest,
    ) -> Result<Page<Agent>>;
    /// The owner or a tenant admin; others get `not_found`.
    async fn get(&self, ctx: &Context, id: AgentId) -> Result<Agent>;
    /// Only the owner (admins get `permission_denied`).
    async fn update(&self, ctx: &Context, id: AgentId, change: AgentUpdate) -> Result<Agent>;
    /// Disable: the owner (`by_owner`) or a tenant admin (`by_admin`); it ends the OAuth grants.
    /// Enable: only the owner, and never after `owner_left`.
    async fn set_disabled(&self, ctx: &Context, id: AgentId, disabled: bool) -> Result<Agent>;
    /// Makes a key. Only the owner. Returns the metadata and the secret.
    async fn create_key(
        &self,
        ctx: &Context,
        id: AgentId,
        name: Option<String>,
        expires_at: Option<OffsetDateTime>,
    ) -> Result<(AgentKey, String)>;
    /// The owner or a tenant admin.
    async fn list_keys(&self, ctx: &Context, id: AgentId) -> Result<Vec<AgentKey>>;
    /// The owner or a tenant admin.
    async fn revoke_key(&self, ctx: &Context, id: AgentId, key: AgentKeyId) -> Result<()>;
    /// Exchanges a key for a short-lived access token. No session is needed. A system lookup by
    /// the hash of the key. The same as [`AgentStore::exchange_key_from`] without a client.
    async fn exchange_key(&self, secret: &str) -> Result<AccessToken> {
        self.exchange_key_from(secret, &crate::audit::events::ClientInfo::default())
            .await
    }
    /// [`AgentStore::exchange_key`] for a request from `client`. It writes `agent.key_used` in
    /// the agent's tenant, in the transaction of the exchange (a failed write fails it), and
    /// `agent.key_refused` with the reason (`revoked`, `expired`, `agent_disabled`,
    /// `scope_expired`) when a known key is refused (decision 0003-4). An unknown key writes
    /// nothing: it names no tenant.
    async fn exchange_key_from(
        &self,
        secret: &str,
        client: &crate::audit::events::ClientInfo,
    ) -> Result<AccessToken>;
    /// A system lookup of an agent for authentication: the live row (scope, disabled). Fails
    /// with `agent_disabled` for a disabled agent and `unauthenticated` for an unknown one.
    async fn authenticate(&self, tenant: TenantId, id: AgentId) -> Result<Agent>;
    /// The journal entries where the agent is the actor, newest first. The owner or a tenant
    /// admin.
    async fn activity(
        &self,
        ctx: &Context,
        id: AgentId,
        since: Option<OffsetDateTime>,
        page: PageRequest,
    ) -> Result<Page<JournalEntry>>;
}

// Undo (`undoAgent`) is [`undo::undo`]: it needs the stores of the other areas (nodes, versions,
// tags, sharing), so it takes the whole `Services`.
