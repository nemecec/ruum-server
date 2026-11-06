//! In-memory node tree, for unit tests. Not implemented yet.

use super::*;

#[derive(Debug, Default)]
pub struct MemoryNodeStore {}

#[async_trait]
impl NodeStore for MemoryNodeStore {
    async fn root(&self, _ctx: &Context) -> Result<Node> {
        todo!("nodes: root")
    }
    async fn create(&self, _ctx: &Context, _req: NewNode) -> Result<Written<Node>> {
        todo!("nodes: create")
    }
    async fn get(&self, _ctx: &Context, _id: NodeId) -> Result<Node> {
        todo!("nodes: get")
    }
    async fn get_by_path(&self, _ctx: &Context, _path: &str) -> Result<Node> {
        todo!("nodes: get_by_path")
    }
    async fn list_children(
        &self,
        _ctx: &Context,
        _parent: NodeId,
        _order: ChildOrder,
        _page: PageRequest,
    ) -> Result<Page<Node>> {
        todo!("nodes: list_children")
    }
    async fn rename(
        &self,
        _ctx: &Context,
        _id: NodeId,
        _name: String,
        _on_conflict: OnConflict,
        _if_revision: Option<i64>,
    ) -> Result<Written<Node>> {
        todo!("nodes: rename")
    }
    async fn move_node(
        &self,
        _ctx: &Context,
        _id: NodeId,
        _req: MoveRequest,
    ) -> Result<Written<Node>> {
        todo!("nodes: move")
    }
    async fn copy(&self, _ctx: &Context, _id: NodeId, _req: CopyRequest) -> Result<Written<Node>> {
        todo!("nodes: copy")
    }
    async fn trash(
        &self,
        _ctx: &Context,
        _id: NodeId,
        _if_revision: Option<i64>,
    ) -> Result<Written<Node>> {
        todo!("nodes: trash")
    }
    async fn restore(
        &self,
        _ctx: &Context,
        _id: NodeId,
        _req: RestoreRequest,
    ) -> Result<Written<Node>> {
        todo!("nodes: restore")
    }
    async fn list_trash(
        &self,
        _ctx: &Context,
        _all: bool,
        _page: PageRequest,
    ) -> Result<Page<Node>> {
        todo!("nodes: list_trash")
    }
    async fn purge(&self, _ctx: &Context, _id: NodeId) -> Result<Written<()>> {
        todo!("nodes: purge")
    }
    async fn empty_trash(&self, _ctx: &Context) -> Result<(u32, Option<ChangeInfo>)> {
        todo!("nodes: empty_trash")
    }
    async fn path_of(&self, _ctx: &Context, _id: NodeId) -> Result<String> {
        todo!("nodes: path_of")
    }
    async fn ancestors(&self, _ctx: &Context, _id: NodeId) -> Result<Vec<NodeId>> {
        todo!("nodes: ancestors")
    }
}
