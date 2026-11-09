//! The node tree (`docs/DESIGN.md`, section 2): folders, files, links and typed nodes;
//! create, get, by path, list children, rename, move, copy, trash and restore.
//!
//! Rules:
//! - Names are stored in Unicode NFC. A name is unique among the live (not trashed) children of a
//!   folder without regard to case (names keep their case).
//! - Links point to the node ID, not to the path. The target is in the same tenant as the link.
//!   A `member` link needs the `share` permission on the target (section 2.3); after that it acts
//!   like a `view` grant by its creator.
//! - `owner_id` is the creator, for display and accounting; it gives no rights.
//! - Every write appends a journal entry in the same transaction
//!   ([`crate::journal::postgres::append`] in the Postgres implementation).
//! - Writing a `collection` tuple (a member link in a folder) needs `share` on the target: when a
//!   member link is made, or moved into another folder. A copy of a member link by a caller
//!   without `share` on the target becomes a shortcut. Trash deletes the tuple; restore brings
//!   it back; purge of the link or of the target deletes it.
//! - Trash (section 5.2): members with `manage` see and restore a trashed root for the plan's
//!   `trash_days`; tenant admins until `admin_restore_days`. Purge needs `manage`, and on plans
//!   where `admin_restore_days > trash_days` (team plans) also the tenant role `admin`.
//!   Trashing a node that is already in the trash is a no-op (no journal entry).
//! - Node responses fill `my_role`, `my_can_share` and `editors_can_share_effective`
//!   ([`rules::decorate`]).
//! - Typed nodes (section 7.1) come only from the provider operations (`createDatabase`), through
//!   [`NodeStore::create_typed`]: the node, its first manifest version and its `typed_nodes`
//!   row in one transaction. The public `createNode` refuses the kind `typed`.
//! - The rules of typed nodes in the tree (card P-3, decisions 0004-6, 0004-7 and 0004-18;
//!   [`typed`]): a typed node is not a folder, so `createNode`, `moveNode` and `copyNode` into
//!   one are `409 conflict`, and a child of a typed node (a database branch) cannot move away
//!   from it (`409`). A copy does not descend into a typed node (a copy of a database copies
//!   no branch). A copy of a typed node makes a new typed node in `provisioning` (its manifest
//!   copied, its `typed_nodes` row) and a `copied` delivery whose `data` is `full` only for a
//!   person with `manage` on the source, else `masked`. Copy, move, rename, trash, restore and
//!   purge write one delivery per typed node they touch ([`TypedHooks::provider_for`]), in the
//!   transaction of the journal entry. The store learns the providers through
//!   [`NodeStore::set_typed_hooks`]; without hooks it writes no delivery and refuses to copy a
//!   typed node.

pub mod memory;
pub mod postgres;
pub mod rules;
pub mod typed;

#[cfg(test)]
mod conformance;

use crate::actor::{Actor, Context};
use crate::authz::Role;
use crate::error::Result;
use crate::ids::{NodeId, TenantId, UserId, VersionId};
use crate::journal::{ChangeInfo, Written};
use crate::paging::{Page, PageRequest};
use crate::providers::HookKind;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// The longest allowed node name, in characters (Unicode code points) after NFC. The same unit
/// as `NodeName.maxLength` in `openapi.yaml` and `length(name)` in the SQL check.
pub const MAX_NAME_CHARS: usize = 255;

/// The four kinds of node that the core knows (section 2.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "node_kind", rename_all = "snake_case")]
pub enum NodeKind {
    Folder,
    File,
    Link,
    Typed,
}

/// The two kinds of link (section 2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "link_kind", rename_all = "snake_case")]
pub enum LinkKind {
    /// Does not give access to the target.
    Shortcut,
    /// Gives `view` on the target to people who can see the folder that holds the link.
    Member,
}

/// A node.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub tenant_id: TenantId,
    /// `None` only for the root folder of a tenant.
    pub parent_id: Option<NodeId>,
    pub name: String,
    pub kind: NodeKind,
    /// MIME type for files; `album`, `project`, `vault`, ... for folders; the provider type for
    /// typed nodes.
    #[serde(rename = "type")]
    pub node_type: Option<String>,
    pub link_kind: Option<LinkKind>,
    pub link_target: Option<NodeId>,
    pub head_version: Option<VersionId>,
    /// The creator (the owner, for a personal tree). Display and accounting only.
    pub owner_id: UserId,
    pub size_bytes: i64,
    /// Grows by one on each change of the node row; used with `If-Match`.
    pub revision: i64,
    #[serde(with = "time::serde::rfc3339::option")]
    pub trashed_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub created_by: Actor,
    pub updated_by: Actor,
    /// The caller's effective role on the node (after the scope), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_role: Option<Role>,
    /// Whether the caller has the `share` permission on the node (after the scope). Set when
    /// `my_role` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_can_share: Option<bool>,
    /// The node setting `editors_can_share` (`nodes.editors_can_share`); `None` inherits.
    #[serde(default)]
    pub editors_can_share: Option<bool>,
    /// `editors_can_share` after inheritance, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editors_can_share_effective: Option<bool>,
}

impl Node {
    /// The `ETag` of the node: `"<revision>"`.
    pub fn etag(&self) -> String {
        format!("\"{}\"", self.revision)
    }

    /// Drops the fields that depend on the caller (`my_role` and the like).
    pub fn without_caller_fields(mut self) -> Self {
        self.my_role = None;
        self.my_can_share = None;
        self.editors_can_share_effective = None;
        self
    }
}

/// What to do when the name is taken.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnConflict {
    /// Fail with `name_taken`.
    #[default]
    Fail,
    /// Add " (2)", " (3)", ... before the extension.
    Rename,
}

/// The target of a new link.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LinkSpec {
    pub kind: LinkKind,
    pub target: NodeId,
}

/// A request to create a folder, an empty file or a link.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewNode {
    pub parent_id: NodeId,
    pub name: String,
    pub kind: NodeKind,
    #[serde(rename = "type")]
    pub node_type: Option<String>,
    pub link: Option<LinkSpec>,
    #[serde(default)]
    pub on_conflict: OnConflict,
}

/// A new typed node ([`NodeStore::create_typed`]). Only the provider operations make typed
/// nodes; the manifest is already valid for its type.
#[derive(Clone, Debug)]
pub struct NewTyped {
    pub parent_id: NodeId,
    pub name: String,
    /// The node type, for example `database`.
    pub node_type: String,
    /// The provider that owns the type, for example `ruum.database`.
    pub provider: String,
    /// The first manifest version.
    pub manifest: serde_json::Value,
    pub on_conflict: OnConflict,
}

/// The format of a manifest version of a typed node (`versions.encoding`, migration 0004).
pub const TYPED_MANIFEST_ENCODING: &str = "typed_manifest_v1";

/// The stored form of a typed node's manifest: its canonical JSON (keys sorted) and the BLAKE3
/// of it (`versions.manifest`, `versions.content_hash`).
pub fn typed_manifest_bytes(manifest: &serde_json::Value) -> (Vec<u8>, [u8; 32]) {
    // `serde_json` keeps object keys sorted (no `preserve_order`), so this is canonical.
    let bytes = serde_json::to_vec(manifest).unwrap_or_default();
    let hash = *blake3::hash(&bytes).as_bytes();
    (bytes, hash)
}

/// The `data` of `version.created` for a manifest version of a typed node.
pub fn typed_version_created_data(
    version: VersionId,
    previous: Option<VersionId>,
    size_bytes: usize,
    hash: &[u8; 32],
) -> serde_json::Value {
    serde_json::json!({
        "version_id": version,
        "previous_version_id": previous,
        "size_bytes": size_bytes,
        "content_hash": hex::encode(hash),
        "content_type": "application/json",
        "coalesced": false,
    })
}

/// The order of a child list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildOrder {
    /// Folders first, then by name.
    #[default]
    Name,
    UpdatedAt,
    Size,
}

/// A request to move a node, and optionally rename it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MoveRequest {
    pub parent_id: NodeId,
    pub name: Option<String>,
    #[serde(default)]
    pub on_conflict: OnConflict,
    pub if_revision: Option<i64>,
}

/// A request to copy a node (a folder is copied with all live children).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CopyRequest {
    pub parent_id: NodeId,
    pub name: Option<String>,
    #[serde(default)]
    pub on_conflict: OnConflict,
}

/// A request to restore a node from the trash.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RestoreRequest {
    /// Where to restore when the old parent is gone or trashed. Default: the old parent.
    pub parent_id: Option<NodeId>,
    #[serde(default)]
    pub on_conflict: OnConflict,
    /// The expected revision of the trashed node (`If-Match`).
    pub if_revision: Option<i64>,
}

/// The name of the root folder of every tenant.
pub const ROOT_NAME: &str = "root";

fn invalid_name(message: impl Into<String>) -> crate::error::Error {
    crate::error::Error::Api(
        crate::error::ApiError::new(crate::error::ErrorCode::ValidationFailed, message).with_hint(
            "a name is 1 to 255 characters, without `/` or NUL, not `.` or `..`, and \
                 without leading or trailing spaces",
        ),
    )
}

/// Normalizes a node name to NFC and checks it: not empty, at most [`MAX_NAME_CHARS`]
/// characters after NFC, no `/`, no NUL, not `.` or `..`, no leading or trailing spaces.
/// Returns the normalized name, which is the one to store and to compare.
pub fn validate_name(name: &str) -> Result<String> {
    use unicode_normalization::UnicodeNormalization;
    let nfc: String = name.nfc().collect();
    if nfc.is_empty() {
        return Err(invalid_name("the name is empty"));
    }
    if nfc.chars().count() > MAX_NAME_CHARS {
        return Err(invalid_name(format!(
            "the name is longer than {MAX_NAME_CHARS} characters"
        )));
    }
    if nfc.contains('/') || nfc.contains('\0') {
        return Err(invalid_name("the name contains `/` or NUL"));
    }
    if nfc == "." || nfc == ".." {
        return Err(invalid_name("the name cannot be `.` or `..`"));
    }
    if nfc.starts_with(char::is_whitespace) || nfc.ends_with(char::is_whitespace) {
        return Err(invalid_name("the name starts or ends with a space"));
    }
    if nfc.chars().any(char::is_control) {
        return Err(invalid_name("the name contains a control character"));
    }
    Ok(nfc)
}

/// The key that makes names unique in a folder: the lower case of the NFC name. The same as
/// `lower(name)` of the unique index `nodes_live_name_key`.
pub fn name_key(name: &str) -> String {
    name.to_lowercase()
}

/// Splits a display path (`Projects/Garden`) into names. Empty segments are ignored.
pub fn split_path(path: &str) -> Result<Vec<String>> {
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(validate_name)
        .collect()
}

/// What the nodes area asks of the versions area when it copies a file (`copyNode`): one new
/// version of `target` with the head content of `source`. The chunks are shared; no bytes are
/// copied. Returns the new version, or `None` when `source` has no head version.
///
/// The in-memory node store calls it through [`memory::MemoryNodeStore::set_content_copy`]; the
/// Postgres node store calls [`crate::blobs::postgres::copy_head`] in its own transaction.
#[async_trait]
pub trait ContentCopy: Send + Sync {
    async fn copy_head(
        &self,
        ctx: &Context,
        source: &Node,
        target: NodeId,
    ) -> Result<Option<VersionId>>;

    /// The nodes are purged: forget their versions. For the in-memory stores; the Postgres
    /// node store deletes the `versions` rows in the purge transaction.
    fn purged(&self, _nodes: &[NodeId]) {}
}

/// What the nodes area asks of the providers of typed nodes (card P-3, `nodes::TypedHooks` of
/// `docs/STEP2.md`): which provider gets a lifecycle hook, and the platform rules for a new
/// typed node that a copy makes. [`crate::providers::hooks::RegistryHooks`] is the real one.
#[async_trait]
pub trait TypedHooks: Send + Sync {
    /// The provider ID of `type_name` when that provider takes `hook`; `None` when the type has
    /// no provider or the provider does not take the hook (then no delivery is written).
    fn provider_for(&self, type_name: &str, hook: HookKind) -> Option<String>;

    /// The rules for a new typed node made by a copy, checked before anything is written:
    /// a guest, or an agent of a guest, only while the tenant policy
    /// `guests_create_resources` is true (`403 permission_denied`, hint
    /// `guests_create_resources`; `contracts/providers/README.md`, "Guests and the host plan").
    async fn check_new_typed(&self, ctx: &Context, type_name: &str) -> Result<()>;
}

/// The node tree. Every method filters by `ctx.tenant` and checks permissions through
/// [`crate::authz::Authz`].
#[async_trait]
pub trait NodeStore: Send + Sync {
    /// Sets the hooks of typed nodes (card P-3). Only the first call counts. A store without
    /// typed nodes ignores it.
    fn set_typed_hooks(&self, hooks: std::sync::Arc<dyn TypedHooks>) {
        let _ = hooks;
    }

    /// The root folder of the tenant.
    async fn root(&self, ctx: &Context) -> Result<Node>;
    async fn create(&self, ctx: &Context, req: NewNode) -> Result<Written<Node>>;
    /// Creates a typed node (`kind: typed`) with its first manifest version (encoding
    /// [`TYPED_MANIFEST_ENCODING`]) as its head, and its `typed_nodes` row (state
    /// `provisioning`), in one transaction; writes `node.created` and `version.created` and
    /// returns the change of the last. Needs `edit` on the parent folder. For the provider
    /// operations only (`createDatabase`); the public `createNode` refuses typed nodes. A store
    /// without typed nodes fails.
    async fn create_typed(&self, ctx: &Context, req: NewTyped) -> Result<Written<Node>> {
        let _ = (ctx, req);
        Err(crate::error::Error::Internal(
            "this node store makes no typed nodes".into(),
        ))
    }
    async fn get(&self, ctx: &Context, id: NodeId) -> Result<Node>;
    /// Resolves a path from the tenant root. Trashed nodes are not found.
    async fn get_by_path(&self, ctx: &Context, path: &str) -> Result<Node>;
    async fn list_children(
        &self,
        ctx: &Context,
        parent: NodeId,
        order: ChildOrder,
        page: PageRequest,
    ) -> Result<Page<Node>>;
    async fn rename(
        &self,
        ctx: &Context,
        id: NodeId,
        name: String,
        on_conflict: OnConflict,
        if_revision: Option<i64>,
    ) -> Result<Written<Node>>;
    async fn move_node(&self, ctx: &Context, id: NodeId, req: MoveRequest)
    -> Result<Written<Node>>;
    async fn copy(&self, ctx: &Context, id: NodeId, req: CopyRequest) -> Result<Written<Node>>;
    /// Moves a node (and its subtree) to the trash. `if_revision` is the `If-Match` revision.
    async fn trash(
        &self,
        ctx: &Context,
        id: NodeId,
        if_revision: Option<i64>,
    ) -> Result<Written<Node>>;
    async fn restore(
        &self,
        ctx: &Context,
        id: NodeId,
        req: RestoreRequest,
    ) -> Result<Written<Node>>;
    /// The trashed roots that the actor can manage, newest first, within the trash period. With
    /// `all`, a tenant admin also gets the roots inside `admin_restore_days`.
    async fn list_trash(&self, ctx: &Context, all: bool, page: PageRequest) -> Result<Page<Node>>;
    /// Deletes a trashed root and its subtree for good (`purgeNode`). Needs `manage`; on plans
    /// with an admin restore window, also the tenant role `admin`.
    async fn purge(&self, ctx: &Context, id: NodeId) -> Result<Written<()>>;
    /// Purges every trashed root that the actor may purge (`emptyTrash`). Returns the count and
    /// the last change.
    async fn empty_trash(&self, ctx: &Context) -> Result<(u32, Option<ChangeInfo>)>;
    /// The display path of a node from the tenant root, for example `/Projects/Garden`.
    async fn path_of(&self, ctx: &Context, id: NodeId) -> Result<String>;
    /// The ancestors of a node, root first, without the node. No permission check: for
    /// [`crate::authz::require`] (scope subtrees) and the changes feed.
    async fn ancestors(&self, ctx: &Context, id: NodeId) -> Result<Vec<NodeId>>;
}
