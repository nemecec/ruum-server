//! Tags (`docs/DESIGN.md`, section 2.4). Tags belong to the tenant. A user sees the tags of the
//! nodes that the user can see. Attaching and detaching a tag writes a journal entry.
//!
//! Rules (`openapi.yaml`, tag `tags`):
//! - Members, admins and owners of the tenant see every tag. Guests, agents and share-link
//!   bearers see only the tags on live nodes that they can see; another tag is `not_found`.
//! - Creating and changing a tag needs the tenant role `member` (not guests); deleting needs
//!   `admin` or the creator of the tag. An agent needs that of its user and a scope with
//!   `max_role` of at least `edit`.
//! - Attaching and detaching need `edit` on the node. Attaching twice and detaching a tag that is
//!   not attached are no-ops without a journal entry. Creating and changing a tag write no
//!   journal entry (the journal is about nodes); deleting writes `tag.detached` for each node.

pub mod memory;
pub mod postgres;
pub mod rules;

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

/// A request to change a tag. Fields that are absent keep their value; `color: null` removes
/// the color (`Some(None)`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TagInput {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, with = "double_option")]
    pub color: Option<Option<String>>,
}

impl TagInput {
    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.color.is_none()
    }
}

/// `Option<Option<T>>` for JSON: absent is `None`, `null` is `Some(None)`.
mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer, T: Serialize>(
        v: &Option<Option<T>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match v {
            Some(inner) => inner.serialize(s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        d: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(d).map(Some)
    }
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
