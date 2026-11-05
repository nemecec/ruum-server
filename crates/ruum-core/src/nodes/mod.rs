//! The node tree (`docs/DESIGN.md`, section 2): folders, files, links and typed nodes;
//! create, get, by path, list children, rename, move, copy, trash and restore.
//!
//! Rules:
//! - A name is unique among the live (not trashed) children of a folder.
//! - Links point to the node ID, not to the path. A `member` link needs the `share` permission on
//!   the target (section 2.3).
//! - Every write appends a journal entry in the same transaction.

pub mod memory;

use crate::actor::{Actor, Context};
use crate::error::Result;
use crate::ids::{NodeId, TenantId, UserId, VersionId};
use crate::journal::Written;
use crate::paging::{Page, PageRequest};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// The longest allowed node name, in bytes.
pub const MAX_NAME_BYTES: usize = 255;

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
}

/// Checks a node name: not empty, at most [`MAX_NAME_BYTES`], no `/`, no NUL, not `.` or `..`,
/// no leading or trailing spaces.
pub fn validate_name(_name: &str) -> Result<()> {
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
    /// Moves a node (and its subtree) to the trash.
    async fn trash(&self, ctx: &Context, id: NodeId) -> Result<Written<Node>>;
    async fn restore(
        &self,
        ctx: &Context,
        id: NodeId,
        req: RestoreRequest,
    ) -> Result<Written<Node>>;
    /// The trashed roots that the actor can see, newest first.
    async fn list_trash(&self, ctx: &Context, page: PageRequest) -> Result<Page<Node>>;
    /// The display path of a node from the tenant root, for example `/Projects/Garden`.
    async fn path_of(&self, ctx: &Context, id: NodeId) -> Result<String>;
}
