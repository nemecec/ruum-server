//! Search by name (`docs/DESIGN.md`, section 12.3). Step 1 searches node names in Postgres
//! (trigram index); results are filtered by permission with bulk checks. Full-text search of file
//! content comes in a later phase.

pub mod memory;

use crate::actor::Context;
use crate::error::Result;
use crate::ids::{NodeId, TagId};
use crate::nodes::{Node, NodeKind};
use crate::paging::{Page, PageRequest};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A search by name.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SearchQuery {
    /// Matches names that contain the text, case-insensitive.
    pub q: String,
    pub kind: Option<NodeKind>,
    /// Only nodes with this tag.
    pub tag: Option<TagId>,
    /// Only nodes inside this folder (at any depth).
    pub within: Option<NodeId>,
    #[serde(flatten)]
    pub page: PageRequest,
}

/// One search result.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchHit {
    pub node: Node,
    /// The display path, for example `/Projects/Garden/plan.txt`.
    pub path: String,
}

#[async_trait]
pub trait Search: Send + Sync {
    async fn by_name(&self, ctx: &Context, query: SearchQuery) -> Result<Page<SearchHit>>;
}
