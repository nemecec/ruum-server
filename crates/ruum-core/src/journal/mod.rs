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

pub mod feed;
pub mod memory;
pub mod postgres;

#[cfg(test)]
mod conformance;

use crate::actor::{Actor, Context};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{NodeId, TenantId};
use crate::paging::{Cursor, Page};
use async_trait::async_trait;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::time::Duration;
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
    #[serde(rename = "database.columns_confirmed")]
    DatabaseColumnsConfirmed,
}

impl EventType {
    /// Every event type, in the order of the envelope schema.
    pub const ALL: &'static [EventType] = &[
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
        EventType::DatabaseColumnsConfirmed,
    ];

    /// The wire name, for example `node.created` (column `journal.type`).
    pub fn as_str(self) -> &'static str {
        use EventType::*;
        match self {
            NodeCreated => "node.created",
            NodeRenamed => "node.renamed",
            NodeMoved => "node.moved",
            NodeCopied => "node.copied",
            NodeTrashed => "node.trashed",
            NodeRestored => "node.restored",
            NodePurged => "node.purged",
            VersionCreated => "version.created",
            VersionRestored => "version.restored",
            VersionLabeled => "version.labeled",
            TagAttached => "tag.attached",
            TagDetached => "tag.detached",
            GrantAdded => "grant.added",
            GrantUpdated => "grant.updated",
            GrantRemoved => "grant.removed",
            ShareLinkCreated => "share_link.created",
            ShareLinkRevoked => "share_link.revoked",
            NodeSharingUpdated => "node.sharing_updated",
            DatabaseColumnsConfirmed => "database.columns_confirmed",
        }
    }

    /// The event type of a wire name.
    pub fn parse(s: &str) -> Option<EventType> {
        Self::ALL.iter().copied().find(|e| e.as_str() == s)
    }
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
    /// The cursor of the returned page ([`encode_cursor`]) is always set: the `seq` of the last
    /// entry, or the position that was read after when the page is empty, so a caller can
    /// follow from there without missing an entry.
    ///
    /// `None` starts at the oldest kept entry. A position older than the oldest kept entry, or
    /// newer than the newest `seq`, fails with `cursor_expired` (410).
    async fn read(
        &self,
        ctx: &Context,
        after: Option<i64>,
        limit: u32,
    ) -> Result<Page<JournalEntry>>;

    /// The newest `seq` of the tenant, or 0 when the journal is empty.
    async fn latest_seq(&self, ctx: &Context) -> Result<i64>;

    /// The entries on `node` with a `seq` below `before` whose type is one of `events`, newest
    /// first, at most `limit`. The changes feed uses it for nodes that are gone or trashed: the
    /// last known parent, the grants on the node, the link fields of its creation.
    async fn node_entries(
        &self,
        ctx: &Context,
        node: NodeId,
        before: i64,
        events: &[EventType],
        limit: u32,
    ) -> Result<Vec<JournalEntry>>;

    /// The parent of `node` after the newest entry on it before `before` that names a parent
    /// (see [`PARENT_EVENTS`] and [`parent_after`]). The changes feed uses it for purged nodes,
    /// which are no longer in the tree.
    async fn last_parent(
        &self,
        ctx: &Context,
        node: NodeId,
        before: i64,
    ) -> Result<Option<NodeId>> {
        Ok(self
            .node_entries(ctx, node, before, PARENT_EVENTS, 1)
            .await?
            .first()
            .and_then(parent_after))
    }

    /// Waits until the tenant has an entry newer than `after`, or until `timeout` passes.
    /// Returns true when there is a newer entry. The default asks [`Journal::latest_seq`] every
    /// 250 ms; an implementation with notifications can do better.
    async fn wait_for(&self, ctx: &Context, after: i64, timeout: Duration) -> Result<bool> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.latest_seq(ctx).await? > after {
                return Ok(true);
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            tokio::time::sleep((deadline - now).min(Duration::from_millis(250))).await;
        }
    }
}

const CURSOR_PREFIX: &str = "j1.";

/// The opaque cursor of the changes feed for a position (`seq`).
pub fn encode_cursor(seq: i64) -> Cursor {
    Cursor(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("{CURSOR_PREFIX}{seq}")))
}

/// The position (`seq`) of a cursor of [`encode_cursor`]. Fails with `bad_request` when the
/// cursor was not made by this server.
pub fn decode_cursor(cursor: &str) -> Result<i64> {
    let bad = || {
        Error::Api(
            ApiError::new(ErrorCode::BadRequest, "the cursor is not valid")
                .with_hint("pass the next_cursor of the last call, or no cursor to start over"),
        )
    };
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor.as_bytes())
        .map_err(|_| bad())?;
    let text = String::from_utf8(bytes).map_err(|_| bad())?;
    let seq: i64 = text
        .strip_prefix(CURSOR_PREFIX)
        .ok_or_else(bad)?
        .parse()
        .map_err(|_| bad())?;
    if seq < 0 {
        return Err(bad());
    }
    Ok(seq)
}

/// The `cursor_expired` (410) error.
pub fn cursor_expired() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::CursorExpired,
            "the cursor is older than the journal retention",
        )
        .with_hint("do a full listing of the tree, then follow from GET /v1/changes/latest"),
    )
}

/// Checks a read position against the kept entries (`oldest` is the oldest kept `seq`, or
/// `latest + 1` when none is kept) and returns the position to read after.
pub(crate) fn check_cursor(after: Option<i64>, oldest: i64, latest: i64) -> Result<i64> {
    let start = (oldest - 1).max(0);
    match after {
        None => Ok(start),
        Some(a) if a < start || a > latest => Err(cursor_expired()),
        Some(a) => Ok(a),
    }
}

fn data_node(entry: &JournalEntry, key: &str) -> Option<NodeId> {
    entry
        .data
        .get(key)
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
}

/// The event types whose data names the parent of the node after the change ([`parent_after`]).
pub const PARENT_EVENTS: &[EventType] = &[
    EventType::NodeCreated,
    EventType::NodeCopied,
    EventType::NodeMoved,
    EventType::NodeRenamed,
    EventType::NodeTrashed,
    EventType::NodeRestored,
];

/// The event types of grant changes.
pub const GRANT_EVENTS: &[EventType] = &[
    EventType::GrantAdded,
    EventType::GrantUpdated,
    EventType::GrantRemoved,
];

/// The parent of the entry's node after the change, when the entry data names it.
pub fn parent_after(entry: &JournalEntry) -> Option<NodeId> {
    match entry.event {
        EventType::NodeMoved => data_node(entry, "new_parent_id"),
        EventType::NodeCreated
        | EventType::NodeCopied
        | EventType::NodeRenamed
        | EventType::NodeTrashed
        | EventType::NodeRestored => data_node(entry, "parent_id"),
        _ => None,
    }
}

/// The parent of the entry's node before the change, when the entry data names it.
pub fn parent_before(entry: &JournalEntry) -> Option<NodeId> {
    match entry.event {
        EventType::NodeMoved | EventType::NodeRestored => data_node(entry, "old_parent_id"),
        EventType::NodeRenamed | EventType::NodeTrashed => data_node(entry, "parent_id"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::UserId;
    use serde_json::Value;
    use std::collections::BTreeSet;
    use uuid::Uuid;

    const ENVELOPE: &str = include_str!("../../../../contracts/events/journal-entry.schema.json");

    const ALL_EVENTS: &[EventType] = EventType::ALL;

    fn schema() -> Value {
        serde_json::from_str(ENVELOPE).unwrap()
    }

    /// The event types of the envelope that are specified but not written yet
    /// (`$defs/planned_types`, decision 0004-21). The task that builds the first writer of a type adds
    /// it to [`EventType`] and removes it from that list.
    fn planned_types(schema: &Value) -> BTreeSet<String> {
        schema["$defs"]["planned_types"]["enum"]
            .as_array()
            .map(|a| a.iter().map(|v| v.as_str().unwrap().to_string()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn event_types_match_the_envelope_schema() {
        let schema = schema();
        let all_in_schema: BTreeSet<String> = schema["properties"]["type"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        let planned = planned_types(&schema);
        assert!(
            planned.is_subset(&all_in_schema),
            "$defs/planned_types names types that are not in the enum: {:?}",
            planned.difference(&all_in_schema)
        );
        // Every type in the enum has a payload schema in the `allOf` list.
        let with_payload: BTreeSet<String> = schema["allOf"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                c["if"]["properties"]["type"]["const"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(with_payload, all_in_schema);
        let in_schema: BTreeSet<String> = all_in_schema.difference(&planned).cloned().collect();
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
                    || matches!(e, EventType::NodePurged | EventType::ShareLinkRevoked)
                    // Only a person confirms classes (`confirmColumnClasses` is users only), so
                    // `undoAgent` never meets this entry among an agent's changes.
                    || *e == EventType::DatabaseColumnsConfirmed,
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

    #[test]
    fn wire_names_round_trip() {
        for e in EventType::ALL {
            let json = serde_json::to_value(e).unwrap();
            assert_eq!(json.as_str(), Some(e.as_str()));
            assert_eq!(EventType::parse(e.as_str()), Some(*e));
        }
        assert_eq!(EventType::parse("node.exploded"), None);
    }

    #[test]
    fn cursors_round_trip_and_reject_garbage() {
        for seq in [0, 1, 18233, i64::MAX] {
            assert_eq!(decode_cursor(&encode_cursor(seq).0).unwrap(), seq);
        }
        for bad in ["", "18233", "!!", "ajEuLTE", "eDEuNQ"] {
            assert!(decode_cursor(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn cursor_positions_are_checked_against_the_kept_entries() {
        fn code(r: Result<i64>) -> Option<ErrorCode> {
            r.err().map(|e| e.to_api().code)
        }
        // Empty journal.
        assert_eq!(check_cursor(None, 1, 0).unwrap(), 0);
        assert_eq!(check_cursor(Some(0), 1, 0).unwrap(), 0);
        assert_eq!(
            code(check_cursor(Some(5), 1, 0)),
            Some(ErrorCode::CursorExpired)
        );
        // Entries 10..=20 are kept.
        assert_eq!(check_cursor(None, 10, 20).unwrap(), 9);
        assert_eq!(check_cursor(Some(9), 10, 20).unwrap(), 9);
        assert_eq!(check_cursor(Some(20), 10, 20).unwrap(), 20);
        assert_eq!(
            code(check_cursor(Some(8), 10, 20)),
            Some(ErrorCode::CursorExpired)
        );
        assert_eq!(
            code(check_cursor(Some(21), 10, 20)),
            Some(ErrorCode::CursorExpired)
        );
    }
}
