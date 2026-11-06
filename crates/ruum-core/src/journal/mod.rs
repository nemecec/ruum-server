//! The change journal (`docs/DESIGN.md`, section 2.5).
//!
//! Every change to a node writes one row to the journal in the same database transaction as the
//! change. `seq` grows per tenant. The journal feeds the changes feed (`GET /v1/changes`), the
//! event bus (through an outbox), agent activity and undo, and the audit log.
//!
//! Event payloads are described in `contracts/events/*.schema.json`.
//!
//! Writing: the Postgres stores call [`postgres::append`] on their own transaction, so the change
//! and its entry commit together. The in-memory stores call
//! [`memory::MemoryJournal::append`]. The [`Journal`] trait is the read side.
//!
//! The changes feed checks visibility against the state before and after each change: an entry
//! is sent when the caller could see its node before or can see it after. When a change alters
//! what the caller sees of a whole subtree (grants, member links, moves across visible folders,
//! restores), the feed adds a [`ResyncMarker`] for the caller.

pub mod memory;
pub mod postgres;

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
    #[serde(rename = "grant.updated")]
    GrantUpdated,
    #[serde(rename = "grant.removed")]
    GrantRemoved,
    #[serde(rename = "share_link.created")]
    ShareLinkCreated,
    #[serde(rename = "share_link.revoked")]
    ShareLinkRevoked,
    #[serde(rename = "node.sharing_updated")]
    NodeSharingUpdated,
}

/// One row of the journal.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JournalEntry {
    pub tenant_id: TenantId,
    /// Grows by one per change in the tenant. Also the cursor of the changes feed.
    pub seq: i64,
    pub node_id: Option<NodeId>,
    /// The revision of the node after the change (`nodes.revision`); `None` when the entry has
    /// no node or the node is purged.
    pub revision: Option<i64>,
    #[serde(rename = "type")]
    pub event: EventType,
    #[serde(flatten)]
    pub actor: Actor,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// Event-specific data, as in the event schema.
    pub data: serde_json::Value,
}

/// Whether the caller gained or lost access to a subtree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Gained,
    Lost,
}

/// A per-caller marker of the changes feed (`ResyncMarker` in `openapi.yaml`): the caller's
/// access to the subtree at `node_id` changed at `seq`, without one entry per node.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResyncMarker {
    pub seq: i64,
    pub node_id: NodeId,
    pub access: Access,
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
    /// The revision of the node after the change.
    pub revision: Option<i64>,
    pub event: EventType,
    pub data: serde_json::Value,
}

/// The read side of the journal. Appending is not in this trait: it must run in the
/// transaction of the change ([`postgres::append`], [`memory::MemoryJournal::append`]).
#[async_trait]
pub trait Journal: Send + Sync {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::UserId;
    use serde_json::Value;
    use std::collections::BTreeSet;
    use uuid::Uuid;

    const ENVELOPE: &str = include_str!("../../../../contracts/events/journal-entry.schema.json");

    const ALL_EVENTS: &[EventType] = &[
        EventType::NodeCreated,
        EventType::NodeRenamed,
        EventType::NodeMoved,
        EventType::NodeCopied,
        EventType::NodeTrashed,
        EventType::NodeRestored,
        EventType::NodePurged,
        EventType::VersionCreated,
        EventType::VersionRestored,
        EventType::VersionLabeled,
        EventType::TagAttached,
        EventType::TagDetached,
        EventType::GrantAdded,
        EventType::GrantUpdated,
        EventType::GrantRemoved,
        EventType::ShareLinkCreated,
        EventType::ShareLinkRevoked,
        EventType::NodeSharingUpdated,
    ];

    fn schema() -> Value {
        serde_json::from_str(ENVELOPE).unwrap()
    }

    #[test]
    fn event_types_match_the_envelope_schema() {
        let schema = schema();
        let in_schema: BTreeSet<String> = schema["properties"]["type"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        let in_rust: BTreeSet<String> = ALL_EVENTS
            .iter()
            .map(|e| {
                serde_json::to_value(e)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(in_rust, in_schema);
        for e in ALL_EVENTS {
            assert!(
                crate::agents::undo_action(*e).is_some()
                    || matches!(e, EventType::NodePurged | EventType::ShareLinkRevoked),
                "{e:?} has no inverse"
            );
        }
    }

    #[test]
    fn a_serialized_entry_has_the_envelope_fields() {
        let schema = schema();
        let entry = JournalEntry {
            tenant_id: TenantId(Uuid::now_v7()),
            seq: 1,
            node_id: Some(NodeId(Uuid::now_v7())),
            revision: Some(2),
            event: EventType::NodeRenamed,
            actor: Actor::user(UserId(Uuid::now_v7())),
            at: OffsetDateTime::UNIX_EPOCH,
            data: serde_json::json!({}),
        };
        let value = serde_json::to_value(&entry).unwrap();
        let keys: BTreeSet<String> = value.as_object().unwrap().keys().cloned().collect();
        let required: BTreeSet<String> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        let allowed: BTreeSet<String> = schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert!(
            required.is_subset(&keys),
            "missing: {:?}",
            required.difference(&keys)
        );
        assert!(
            keys.is_subset(&allowed),
            "extra: {:?}",
            keys.difference(&allowed)
        );
    }
}
