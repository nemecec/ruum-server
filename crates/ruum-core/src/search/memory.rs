//! In-memory search, for unit tests. Not implemented yet.

use super::*;

#[derive(Debug, Default)]
pub struct MemorySearch {}

#[async_trait]
impl Search for MemorySearch {
    async fn by_name(&self, _ctx: &Context, _query: SearchQuery) -> Result<Page<SearchHit>> {
        todo!("search: by_name")
    }
}
