//! The audit log per tenant (`docs/DESIGN.md`, section 15): auth events, membership changes,
//! agent and key changes, sharing changes and node changes from the journal. Exportable.
//! Every entry records `actor` and `on_behalf_of`.

pub mod memory;

use crate::actor::{Actor, Context};
use crate::error::Result;
use crate::paging::{Page, PageRequest};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// One audit entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: Uuid,
    /// A dotted action name, for example `auth.login`, `agent.key_created`, `grant.added`.
    pub action: String,
    #[serde(flatten)]
    pub actor: Actor,
    /// The type of the target, for example `node`, `agent`, `member`.
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub data: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// A new entry.
#[derive(Clone, Debug)]
pub struct NewAuditEntry {
    pub action: String,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub data: serde_json::Value,
}

/// Filters for the audit log.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AuditQuery {
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub since: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub until: Option<OffsetDateTime>,
    /// Only entries by this actor (user, agent or app ID).
    pub actor: Option<Uuid>,
    /// Only entries whose action starts with this prefix, for example `agent.`.
    pub action: Option<String>,
    #[serde(flatten)]
    pub page: PageRequest,
}

#[async_trait]
pub trait AuditLog: Send + Sync {
    async fn record(&self, ctx: &Context, entry: NewAuditEntry) -> Result<()>;
    /// Newest first. Needs the tenant `admin` role.
    async fn list(&self, ctx: &Context, query: AuditQuery) -> Result<Page<AuditEntry>>;
}
