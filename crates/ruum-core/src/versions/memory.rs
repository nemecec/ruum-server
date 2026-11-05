//! In-memory version store and diff service, for unit tests. Not implemented yet.

use super::*;
use crate::versions::diff::DiffService;

#[derive(Debug, Default)]
pub struct MemoryVersionStore {}

#[async_trait]
impl VersionStore for MemoryVersionStore {
    async fn list(
        &self,
        _ctx: &Context,
        _node: NodeId,
        _page: PageRequest,
    ) -> Result<Page<Version>> {
        todo!("versions: list")
    }
    async fn get(&self, _ctx: &Context, _node: NodeId, _version: VersionId) -> Result<Version> {
        todo!("versions: get")
    }
    async fn restore(
        &self,
        _ctx: &Context,
        _node: NodeId,
        _version: VersionId,
    ) -> Result<Written<Version>> {
        todo!("versions: restore")
    }
    async fn set_label(
        &self,
        _ctx: &Context,
        _node: NodeId,
        _version: VersionId,
        _label: Option<String>,
    ) -> Result<Written<Version>> {
        todo!("versions: set_label")
    }
}

#[derive(Debug, Default)]
pub struct MemoryDiffService {}

#[async_trait]
impl DiffService for MemoryDiffService {
    async fn plugins_for(&self, _ctx: &Context, _node: NodeId) -> Result<Vec<String>> {
        todo!("versions: plugins_for")
    }
    async fn diff(
        &self,
        _ctx: &Context,
        _node: NodeId,
        _from: VersionId,
        _to: VersionId,
        _plugin: Option<String>,
    ) -> Result<DiffResult> {
        todo!("versions: diff")
    }
}
