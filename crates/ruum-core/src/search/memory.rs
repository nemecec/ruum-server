//! In-memory search, for unit tests and for running without Docker. It scans the live nodes of
//! the tenant and follows the rules of the Postgres search.

use super::*;
use crate::authz::{Authz, Role};
use crate::nodes::memory::MemoryNodeStore;
use crate::nodes::{NodeStore, rules as node_rules};
use crate::tags::memory::MemoryTagStore;
use std::sync::Arc;

/// The in-memory search over the in-memory node and tag stores.
pub struct MemorySearch {
    authz: Arc<dyn Authz>,
    nodes: Arc<MemoryNodeStore>,
    tags: Arc<MemoryTagStore>,
}

impl std::fmt::Debug for MemorySearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemorySearch").finish_non_exhaustive()
    }
}

impl MemorySearch {
    pub fn new(
        authz: Arc<dyn Authz>,
        nodes: Arc<MemoryNodeStore>,
        tags: Arc<MemoryTagStore>,
    ) -> Self {
        Self { authz, nodes, tags }
    }
}

#[async_trait]
impl Search for MemorySearch {
    async fn by_name(&self, ctx: &Context, query: SearchQuery) -> Result<Page<SearchHit>> {
        let q = query_text(&query.q)?;
        let offset = query
            .page
            .cursor
            .as_ref()
            .map(node_rules::parse_offset_cursor)
            .transpose()?
            .unwrap_or(0) as usize;
        let limit = query.page.limit() as usize;
        if let Some(w) = query.within {
            let seen = self.nodes.live_node(ctx.tenant, w).is_some()
                && node_rules::caller_view(&*self.authz, &*self.nodes, ctx, &[w])
                    .await?
                    .first()
                    .is_some_and(|v| v.role.is_some());
            if !seen {
                return Ok(Page::empty());
            }
        }
        let tagged = query.tag.map(|t| self.tags.tagged(ctx.tenant, t));
        let mut candidates: Vec<Node> = Vec::new();
        for n in self.nodes.live_nodes(ctx.tenant) {
            if n.parent_id.is_none()
                || !n.name.to_lowercase().contains(&q)
                || query.kind.is_some_and(|k| k != n.kind)
                || tagged.as_ref().is_some_and(|t| !t.contains(&n.id))
            {
                continue;
            }
            if let Some(w) = query.within
                && !self.nodes.ancestors(ctx, n.id).await?.contains(&w)
            {
                continue;
            }
            candidates.push(n);
        }
        candidates.sort_by_key(|a| (rank(&a.name, &q), a.id));
        let mut pos = offset.min(candidates.len());
        let mut hits = Vec::new();
        for _ in 0..MAX_BATCHES {
            if hits.len() >= limit || pos >= candidates.len() {
                break;
            }
            let want = limit - hits.len();
            let batch: Vec<Node> = candidates[pos..(pos + want).min(candidates.len())].to_vec();
            pos += batch.len();
            for node in self.nodes.present(ctx, batch, Some(Role::View)).await? {
                let path = self.nodes.path_of(ctx, node.id).await?;
                hits.push(SearchHit { node, path });
            }
        }
        Ok(Page {
            items: hits,
            next_cursor: (pos < candidates.len()).then(|| node_rules::offset_cursor(pos as u64)),
        })
    }
}
