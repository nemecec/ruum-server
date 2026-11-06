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

pub mod memory;

use crate::actor::{Actor, Context};
use crate::authz::Role;
use crate::error::Result;
use crate::ids::{NodeId, TenantId, UserId, VersionId};
use crate::journal::{ChangeInfo, Written};
use crate::paging::{Page, PageRequest};
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

/// Normalizes a node name to NFC and checks it: not empty, at most [`MAX_NAME_CHARS`]
/// characters after NFC, no `/`, no NUL, not `.` or `..`, no leading or trailing spaces.
/// Returns the normalized name, which is the one to store and to compare.
pub fn validate_name(_name: &str) -> Result<String> {
    todo!("nodes: validate_name")
}

/// Splits a display path (`Projects/Garden`) into names. Empty segments are ignored.
pub fn split_path(_path: &str) -> Result<Vec<String>> {
    todo!("nodes: split_path")
}

/// The node tree. Every method filters by `ctx.tenant` and checks permissions through
/// [`crate::authz::Authz`].
#[async_trait]
pub trait NodeStore: Send + Sync {
    /// The root folder of the tenant.
    async fn root(&self, ctx: &Context) -> Result<Node>;
    async fn create(&self, ctx: &Context, req: NewNode) -> Result<Written<Node>>;
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
