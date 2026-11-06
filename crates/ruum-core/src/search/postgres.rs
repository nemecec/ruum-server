//! Search by name in Postgres: `lower(name) like '%q%'` on the trigram index
//! `nodes_name_trgm_idx`, best match first, then the permission filter with bulk checks
//! ([`crate::nodes::rules::caller_view`]).

use super::*;
use crate::authz::{Authz, Role};
use crate::journal::postgres::begin_tenant;
use crate::nodes::postgres::{COLUMNS, like_escape, node_of};
use crate::nodes::{NodeStore, rules as node_rules};
use sqlx::PgPool;
use std::sync::Arc;

/// The Postgres search.
pub struct PgSearch {
    pool: PgPool,
    authz: Arc<dyn Authz>,
    nodes: Arc<dyn NodeStore>,
}

impl std::fmt::Debug for PgSearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgSearch").finish_non_exhaustive()
    }
}

impl PgSearch {
    pub fn new(pool: PgPool, authz: Arc<dyn Authz>, nodes: Arc<dyn NodeStore>) -> Self {
        Self { pool, authz, nodes }
    }

    async fn candidates(
        &self,
        ctx: &Context,
        q: &str,
        query: &SearchQuery,
        offset: usize,
        n: usize,
    ) -> Result<Vec<Node>> {
        let sql = format!(
            "with recursive sub(id, depth) as ( \
                 select id, 0 from nodes where tenant_id = $1 and parent_id = $5 \
                     and trashed_at is null \
                 union all \
                 select c.id, sub.depth + 1 from nodes c join sub on c.parent_id = sub.id \
                 where c.tenant_id = $1 and c.trashed_at is null and sub.depth < 10000) \
             select {COLUMNS} from nodes n \
             where n.tenant_id = $1 and n.trashed_at is null and n.parent_id is not null \
             and lower(n.name) like '%' || $2 || '%' escape '\\' \
             and ($3::node_kind is null or n.kind = $3) \
             and ($4::uuid is null or exists (select 1 from node_tags nt \
                  where nt.tenant_id = $1 and nt.node_id = n.id and nt.tag_id = $4)) \
             and ($5::uuid is null or n.id in (select id from sub)) \
             order by (lower(n.name) = $6) desc, (lower(n.name) like $2 || '%' escape '\\') desc, \
                 similarity(lower(n.name), $6) desc, length(n.name), n.id \
             offset $7 limit $8"
        );
        let mut c = begin_tenant(&self.pool, ctx.tenant).await?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(ctx.tenant.0)
            .bind(like_escape(q))
            .bind(query.kind)
            .bind(query.tag.map(|t| t.0))
            .bind(query.within.map(|w| w.0))
            .bind(q)
            .bind(offset as i64)
            .bind(n as i64)
            .fetch_all(&mut *c)
            .await?;
        c.commit().await?;
        rows.iter().map(|r| Ok(node_of(r)?.node)).collect()
    }
}

#[async_trait]
impl Search for PgSearch {
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
            let seen = match self.nodes.get(ctx, w).await {
                Ok(n) => n.trashed_at.is_none() && n.my_role.is_some(),
                Err(e) if matches!(&e, crate::error::Error::Api(_)) => false,
                Err(e) => return Err(e),
            };
            if !seen {
                return Ok(Page::empty());
            }
        }
        let mut pos = offset;
        let mut hits = Vec::new();
        let mut done = false;
        for _ in 0..MAX_BATCHES {
            if hits.len() >= limit {
                break;
            }
            let want = limit - hits.len();
            // One more than needed tells whether candidates are left.
            let mut batch = self.candidates(ctx, &q, &query, pos, want + 1).await?;
            if batch.len() <= want {
                done = true;
            }
            batch.truncate(want);
            pos += batch.len();
            let seen =
                node_rules::visible(&*self.authz, &*self.nodes, ctx, batch, Role::View).await?;
            for node in seen {
                let path = self.nodes.path_of(ctx, node.id).await?;
                hits.push(SearchHit { node, path });
            }
            if done {
                break;
            }
        }
        Ok(Page {
            items: hits,
            next_cursor: (!done).then(|| node_rules::offset_cursor(pos as u64)),
        })
    }
}
