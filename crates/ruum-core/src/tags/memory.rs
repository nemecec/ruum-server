//! In-memory tag store, for unit tests. Not implemented yet.

use super::*;

#[derive(Debug, Default)]
pub struct MemoryTagStore {}

#[async_trait]
impl TagStore for MemoryTagStore {
    async fn list(&self, _ctx: &Context, _page: PageRequest) -> Result<Page<Tag>> {
        todo!("tags: list")
    }
    async fn create(
        &self,
        _ctx: &Context,
        _name: String,
        _color: Option<String>,
    ) -> Result<Written<Tag>> {
        todo!("tags: create")
    }
    async fn get(&self, _ctx: &Context, _id: TagId) -> Result<Tag> {
        todo!("tags: get")
    }
    async fn update(&self, _ctx: &Context, _id: TagId, _input: TagInput) -> Result<Written<Tag>> {
        todo!("tags: update")
    }
    async fn delete(&self, _ctx: &Context, _id: TagId) -> Result<()> {
        todo!("tags: delete")
    }
    async fn attach(&self, _ctx: &Context, _node: NodeId, _tag: TagId) -> Result<Written<()>> {
        todo!("tags: attach")
    }
    async fn detach(&self, _ctx: &Context, _node: NodeId, _tag: TagId) -> Result<Written<()>> {
        todo!("tags: detach")
    }
    async fn tags_of(&self, _ctx: &Context, _node: NodeId) -> Result<Vec<Tag>> {
        todo!("tags: tags_of")
    }
    async fn nodes_with(
        &self,
        _ctx: &Context,
        _tag: TagId,
        _page: PageRequest,
    ) -> Result<Page<Node>> {
        todo!("tags: nodes_with")
    }
}
