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
///
/// [`EventType::Unknown`] is a type that a newer release writes and this release does not know
/// (backlog P-57, design 12.7): during a rolling deploy old and new pods share one journal. The
/// readers never fail on it ([`EventType::from_wire`]); they pass the entry on in a generic shape
/// ([`JournalEntry::generic_if_unknown`]: the envelope as written, `data` empty). This release
/// never writes it. On the wire it is the type name as written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventType {
    NodeCreated,
    NodeRenamed,
    NodeMoved,
    NodeCopied,
    NodeTrashed,
    NodeRestored,
    NodePurged,
    VersionCreated,
    VersionRestored,
    VersionLabeled,
    TagAttached,
    TagDetached,
    GrantAdded,
    GrantUpdated,
    GrantRemoved,
    ShareLinkCreated,
    ShareLinkRevoked,
    NodeSharingUpdated,
    DatabaseColumnsConfirmed,
    DatabaseGrantCreated,
    DatabaseGrantRevoked,
    DatabaseApprovalDecided,
    AppSettingsChanged,
    AppDeployed,
    AppDeploymentFailed,
    AppRolledBack,
    AppRoleGranted,
    AppRoleRevoked,
    /// A type written by a newer release; the name as written (interned, see
    /// [`EventType::from_wire`]).
    Unknown(&'static str),
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
        EventType::DatabaseGrantCreated,
        EventType::DatabaseGrantRevoked,
        EventType::DatabaseApprovalDecided,
        EventType::AppSettingsChanged,
        EventType::AppDeployed,
        EventType::AppDeploymentFailed,
        EventType::AppRolledBack,
        EventType::AppRoleGranted,
        EventType::AppRoleRevoked,
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
            DatabaseGrantCreated => "database.grant_created",
            DatabaseGrantRevoked => "database.grant_revoked",
            DatabaseApprovalDecided => "database.approval_decided",
            AppSettingsChanged => "app.settings_changed",
            AppDeployed => "app.deployed",
            AppDeploymentFailed => "app.deployment_failed",
            AppRolledBack => "app.rolled_back",
            AppRoleGranted => "app.role_granted",
            AppRoleRevoked => "app.role_revoked",
            Unknown(name) => name,
        }
    }

    /// The event type of a wire name this release knows, or `None`.
    pub fn parse(s: &str) -> Option<EventType> {
        Self::ALL.iter().copied().find(|e| e.as_str() == s)
    }

    /// The event type of a wire name read from the journal: a known type, or
    /// [`EventType::Unknown`] for a type of a newer release. Never fails, so an old pod reads a
    /// journal that a new pod writes to (backlog P-57).
    pub fn from_wire(s: &str) -> EventType {
        Self::parse(s).unwrap_or_else(|| EventType::Unknown(intern_unknown(s)))
    }

    /// Whether this release knows the type (every type but [`EventType::Unknown`]).
    pub fn is_known(self) -> bool {
        !matches!(self, EventType::Unknown(_))
    }
}

/// The name of an unknown type whose wire name is not well formed, or when the table of
/// unknown names is full.
pub const UNKNOWN_EVENT_NAME: &str = "unknown";

/// The most distinct unknown names one process keeps.
const MAX_UNKNOWN_NAMES: usize = 64;

/// The longest wire name kept as written.
const MAX_EVENT_NAME_LEN: usize = 64;

/// A `'static` copy of an unknown wire name, so [`EventType`] stays `Copy`. Only the release's
/// own writers fill the `type` column, so the names are few; the table is capped anyway, and a
/// name that is not of the form `area.event` (lowercase letters, digits, `_`) becomes
/// [`UNKNOWN_EVENT_NAME`].
fn intern_unknown(name: &str) -> &'static str {
    use std::sync::Mutex;
    static NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let well_formed = !name.is_empty()
        && name.len() <= MAX_EVENT_NAME_LEN
        && name.contains('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.');
    if !well_formed {
        return UNKNOWN_EVENT_NAME;
    }
    let mut names = NAMES.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(n) = names.iter().find(|n| **n == name) {
        return n;
    }
    if names.len() >= MAX_UNKNOWN_NAMES {
        return UNKNOWN_EVENT_NAME;
    }
    let kept: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.push(kept);
    kept
}

impl Serialize for EventType {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EventType {
    /// Lenient, like the journal readers: an unknown name gives [`EventType::Unknown`].
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let name = std::borrow::Cow::<'de, str>::deserialize(d)?;
        Ok(EventType::from_wire(&name))
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

impl JournalEntry {
    /// The generic shape of an entry whose type this release does not know (backlog P-57): the
    /// envelope as written and `data` empty (`{}`). This release cannot tell what the data of
    /// such a type holds, or who may read it, so it passes on only that the node changed; the
    /// client reads the node again. Entries of known types are returned as they are.
    pub fn generic_if_unknown(mut self) -> Self {
        if !self.event.is_known() {
            self.data = serde_json::Value::Object(serde_json::Map::new());
        }
        self
    }
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

/// A release writes only the types it knows; [`EventType::Unknown`] exists for reading.
pub(crate) fn refuse_unknown(entry: &NewEntry) -> Result<()> {
    if entry.event.is_known() {
        Ok(())
    } else {
        Err(Error::Internal(format!(
            "a journal entry of unknown type {} cannot be written",
            entry.event.as_str()
        )))
    }
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
                    || matches!(
                        e,
                        EventType::NodePurged
                            | EventType::ShareLinkRevoked
                            | EventType::DatabaseGrantRevoked
                    )
                    // Only a person decides an approval (`decideDatabaseApproval` is users
                    // only), so `undoAgent` never meets this entry among an agent's changes.
                    || *e == EventType::DatabaseApprovalDecided
                    // Only a person confirms classes (`confirmColumnClasses` is users only), so
                    // `undoAgent` never meets this entry among an agent's changes.
                    || *e == EventType::DatabaseColumnsConfirmed
                    // The entry names the new settings only (its schema), so there is nothing
                    // to set back; an agent can only lower settings anyway.
                    || *e == EventType::AppSettingsChanged
                    // A deployment is not undone: a preview expires on its own, a failure
                    // changed nothing, and only a person publishes or rolls back (card W-2s).
                    || matches!(
                        e,
                        EventType::AppDeployed
                            | EventType::AppDeploymentFailed
                            | EventType::AppRolledBack
                    ),
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

    /// Backlog P-57: a type that a newer release writes reads as [`EventType::Unknown`], keeps
    /// its name on the wire and never fails.
    #[test]
    fn a_type_of_a_newer_release_reads_without_failing() {
        let e = EventType::from_wire("app.from_a_newer_release");
        assert_eq!(e, EventType::Unknown("app.from_a_newer_release"));
        assert!(!e.is_known());
        assert_eq!(e.as_str(), "app.from_a_newer_release");
        assert_eq!(EventType::parse("app.from_a_newer_release"), None);
        // One copy per name.
        let again = EventType::from_wire("app.from_a_newer_release");
        assert!(std::ptr::eq(e.as_str(), again.as_str()));
        // Known names stay known, in both directions of serde.
        for k in EventType::ALL {
            assert!(k.is_known());
            assert_eq!(EventType::from_wire(k.as_str()), *k);
            let back: EventType = serde_json::from_value(serde_json::to_value(k).unwrap()).unwrap();
            assert_eq!(back, *k);
        }
        // Serde is lenient too, and writes the name as it was read.
        let read: EventType = serde_json::from_str("\"issue.reveal_requested_v2\"").unwrap();
        assert_eq!(read.as_str(), "issue.reveal_requested_v2");
        assert_eq!(
            serde_json::to_value(read).unwrap(),
            serde_json::json!("issue.reveal_requested_v2")
        );
        // A name that is not of the form `area.event` is never kept as written.
        let long = format!("a.{}", "b".repeat(MAX_EVENT_NAME_LEN));
        for odd in [
            "",
            "nodot",
            "Node.Created",
            "node.created;drop",
            "x.\u{e9}",
            long.as_str(),
        ] {
            assert_eq!(
                EventType::from_wire(odd),
                EventType::Unknown(UNKNOWN_EVENT_NAME),
                "{odd:?}"
            );
        }
    }

    #[test]
    fn an_unknown_entry_has_the_generic_shape() {
        let schema = schema();
        let entry = |event| JournalEntry {
            tenant_id: TenantId(Uuid::now_v7()),
            seq: 7,
            node_id: Some(NodeId(Uuid::now_v7())),
            revision: Some(3),
            event,
            actor: Actor::user(UserId(Uuid::now_v7())),
            at: OffsetDateTime::UNIX_EPOCH,
            data: serde_json::json!({ "secret_name": "DB_PASSWORD" }),
        };
        let known = entry(EventType::NodeRenamed).generic_if_unknown();
        assert_eq!(known.data["secret_name"], "DB_PASSWORD");
        let generic = entry(EventType::from_wire("app.secret_rotated")).generic_if_unknown();
        let value = serde_json::to_value(&generic).unwrap();
        assert_eq!(value["type"], "app.secret_rotated");
        assert_eq!(value["data"], serde_json::json!({}));
        assert_eq!(value["seq"], 7);
        assert_eq!(value["revision"], 3);
        // The envelope fields are all there, as for a known type.
        let keys: BTreeSet<String> = value.as_object().unwrap().keys().cloned().collect();
        for r in schema["required"].as_array().unwrap() {
            assert!(keys.contains(r.as_str().unwrap()), "{r}");
        }
        // A client that reads it back gets the same entry.
        let back: JournalEntry = serde_json::from_value(value).unwrap();
        assert_eq!(back.event.as_str(), "app.secret_rotated");
    }

    /// A release writes only the types it knows.
    #[test]
    fn an_unknown_type_is_never_written() {
        let j = memory::MemoryJournal::default();
        let ctx = Context {
            tenant: TenantId(Uuid::now_v7()),
            actor: Actor::user(UserId(Uuid::now_v7())),
            scope: None,
            idempotency_key: None,
        };
        let err = j
            .append(
                &ctx,
                NewEntry {
                    node_id: None,
                    revision: None,
                    event: EventType::from_wire("node.exploded"),
                    data: serde_json::json!({}),
                },
            )
            .unwrap_err();
        assert!(matches!(err, Error::Internal(_)), "{err:?}");
        assert!(j.entries(ctx.tenant).is_empty());
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
