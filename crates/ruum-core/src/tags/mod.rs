//! Tags (`docs/DESIGN.md`, section 2.4). Tags belong to the tenant. A user sees the tags of the
//! nodes that the user can see. Attaching and detaching a tag writes a journal entry.

pub mod memory;

use crate::actor::{Actor, Context};
use crate::error::Result;
use crate::ids::{NodeId, TagId, TenantId};
use crate::journal::Written;
use crate::nodes::Node;
use crate::paging::{Page, PageRequest};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// A tenant tag.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tag {
    pub id: TagId,
    pub tenant_id: TenantId,
    /// Unique in the tenant, case-insensitive.
    pub name: String,
    /// A CSS hex color such as `#3b82f6`.
    pub color: Option<String>,
    pub created_by: Actor,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A request to create or change a tag.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TagInput {
    pub name: Option<String>,
    pub color: Option<String>,
}

#[async_trait]
pub trait TagStore: Send + Sync {
    async fn list(&self, ctx: &Context, page: PageRequest) -> Result<Page<Tag>>;
    async fn create(
        &self,
        ctx: &Context,
        name: String,
        color: Option<String>,
    ) -> Result<Written<Tag>>;
    async fn get(&self, ctx: &Context, id: TagId) -> Result<Tag>;
    async fn update(&self, ctx: &Context, id: TagId, input: TagInput) -> Result<Written<Tag>>;
    /// Deletes the tag and detaches it from all nodes.
    async fn delete(&self, ctx: &Context, id: TagId) -> Result<()>;
    /// Attaching a tag twice is a no-op.
    async fn attach(&self, ctx: &Context, node: NodeId, tag: TagId) -> Result<Written<()>>;
    async fn detach(&self, ctx: &Context, node: NodeId, tag: TagId) -> Result<Written<()>>;
    async fn tags_of(&self, ctx: &Context, node: NodeId) -> Result<Vec<Tag>>;
    /// The live nodes with the tag that the actor can see.
    async fn nodes_with(&self, ctx: &Context, tag: TagId, page: PageRequest) -> Result<Page<Node>>;
}
