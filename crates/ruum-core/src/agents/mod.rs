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

use crate::actor::Context;
use crate::authz::{Role, Scope};
use crate::error::Result;
use crate::ids::{AgentId, AgentKeyId, NodeId, TenantId, UserId};
use crate::journal::JournalEntry;
use crate::paging::{Page, PageRequest};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// The visible prefix of agent API keys, so secret scanners find leaked keys.
pub const API_KEY_PREFIX: &str = "ruum_agent_";
/// The lifetime of an access token from a key exchange.
pub const ACCESS_TOKEN_SECONDS: i64 = 15 * 60;

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
    pub description: Option<String>,
    pub scope: AgentScope,
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
    pub issued_token_type: String,
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UndoReport {
    pub dry_run: bool,
    pub reverted: Vec<UndoItem>,
    pub skipped: Vec<UndoItem>,
}

/// Makes a new API key. Returns the secret (shown once) and its hash (stored).
pub fn generate_key() -> (String, [u8; 32]) {
    todo!("agents: generate_key")
}

/// The stored hash of a key (SHA-256; keys have 256 bits of entropy, so a slow hash is not
/// needed).
pub fn hash_key(_secret: &str) -> [u8; 32] {
    todo!("agents: hash_key")
}

#[async_trait]
pub trait AgentStore: Send + Sync {
    // Every write below is for users only: it calls `require_user_actor` first.

    /// Registers an agent for the actor's user. Fails with `plan_limit_reached` at the limit.
    async fn register(&self, ctx: &Context, req: NewAgent) -> Result<Agent>;
    /// The user's agents; tenant admins see all agents of the tenant.
    async fn list(&self, ctx: &Context, page: PageRequest) -> Result<Page<Agent>>;
    async fn get(&self, ctx: &Context, id: AgentId) -> Result<Agent>;
    async fn update(
        &self,
        ctx: &Context,
        id: AgentId,
        name: Option<String>,
        description: Option<String>,
        scope: Option<AgentScope>,
    ) -> Result<Agent>;
    async fn set_disabled(&self, ctx: &Context, id: AgentId, disabled: bool) -> Result<Agent>;
    /// Makes a key. Returns the metadata and the secret.
    async fn create_key(
        &self,
        ctx: &Context,
        id: AgentId,
        name: Option<String>,
        expires_at: Option<OffsetDateTime>,
    ) -> Result<(AgentKey, String)>;
    async fn list_keys(&self, ctx: &Context, id: AgentId) -> Result<Vec<AgentKey>>;
    async fn revoke_key(&self, ctx: &Context, id: AgentId, key: AgentKeyId) -> Result<()>;
    /// Exchanges a key for a short-lived access token. No session is needed.
    async fn exchange_key(&self, secret: &str) -> Result<AccessToken>;
    /// The journal entries where the agent is the actor, newest first.
    async fn activity(
        &self,
        ctx: &Context,
        id: AgentId,
        page: PageRequest,
    ) -> Result<Page<JournalEntry>>;
    /// Reverts the changes of the agent, newest first, with the inverse of each event type
    /// ([`undo_action`]). Changes that others changed later are skipped and reported. Users only.
    async fn undo(&self, ctx: &Context, id: AgentId, req: UndoRequest) -> Result<UndoReport>;
}
