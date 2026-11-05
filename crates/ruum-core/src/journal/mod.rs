//! The change journal (`docs/DESIGN.md`, section 2.5).
//!
//! Every change to a node writes one row to the journal in the same database transaction as the
//! change. `seq` grows per tenant. The journal feeds the changes feed (`GET /v1/changes`), the
//! event bus (through an outbox), agent activity and undo, and the audit log.
//!
//! Event payloads are described in `contracts/events/*.schema.json`.

pub mod memory;

use crate::actor::{Actor, Context};
use crate::error::Result;
use crate::ids::{NodeId, TenantId};
use crate::paging::Page;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// The type of a journal event. The wire names (`node.created`, ...) match the event schemas in
/// `contracts/events/`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventType {
    #[serde(rename = "node.created")]
    NodeCreated,
    #[serde(rename = "node.renamed")]
    NodeRenamed,
    #[serde(rename = "node.moved")]
    NodeMoved,
    #[serde(rename = "node.copied")]
    NodeCopied,
    #[serde(rename = "node.trashed")]
    NodeTrashed,
    #[serde(rename = "node.restored")]
    NodeRestored,
    #[serde(rename = "node.purged")]
    NodePurged,
    #[serde(rename = "version.created")]
    VersionCreated,
    #[serde(rename = "version.restored")]
    VersionRestored,
    #[serde(rename = "version.labeled")]
    VersionLabeled,
    #[serde(rename = "tag.attached")]
    TagAttached,
    #[serde(rename = "tag.detached")]
    TagDetached,
    #[serde(rename = "grant.added")]
    GrantAdded,
    #[serde(rename = "grant.removed")]
    GrantRemoved,
    #[serde(rename = "share_link.created")]
    ShareLinkCreated,
    #[serde(rename = "share_link.revoked")]
    ShareLinkRevoked,
}

/// One row of the journal.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JournalEntry {
    pub tenant_id: TenantId,
    /// Grows by one per change in the tenant. Also the cursor of the changes feed.
    pub seq: i64,
    pub node_id: Option<NodeId>,
    #[serde(rename = "type")]
    pub event: EventType,
    #[serde(flatten)]
    pub actor: Actor,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// Event-specific data, as in the event schema.
    pub data: serde_json::Value,
}

/// What a write response says about the change it made (section 3.4).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChangeInfo {
    /// The journal sequence number of the change, if the write made a journal entry.
    pub seq: Option<i64>,
    #[serde(flatten)]
    pub actor: Actor,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// The result of a write: the new value and the change that made it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Written<T> {
    pub value: T,
    pub change: ChangeInfo,
}

/// A new entry, before it has a sequence number.
#[derive(Clone, Debug)]
pub struct NewEntry {
    pub node_id: Option<NodeId>,
    pub event: EventType,
    pub data: serde_json::Value,
}

/// The journal. The Postgres implementation appends inside the caller's transaction; the
/// in-memory one appends at once.
#[async_trait]
pub trait Journal: Send + Sync {
    /// Appends one entry and returns it with its `seq`.
    async fn append(&self, ctx: &Context, entry: NewEntry) -> Result<JournalEntry>;

    /// Reads the entries after `after` (exclusive), oldest first, at most `limit`.
    /// The cursor of the returned page is the `seq` of the last entry.
    async fn read(
        &self,
        ctx: &Context,
        after: Option<i64>,
        limit: u32,
    ) -> Result<Page<JournalEntry>>;

    /// The newest `seq` of the tenant, or 0 when the journal is empty.
    async fn latest_seq(&self, ctx: &Context) -> Result<i64>;
}
