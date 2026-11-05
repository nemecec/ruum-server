//! In-memory blob store and transfers, for unit tests. Not implemented yet.

use super::*;
use crate::journal::Written;

#[derive(Debug, Default)]
pub struct MemoryBlobStore {}

#[async_trait]
impl BlobStore for MemoryBlobStore {
    async fn put_chunk(&self, _tenant: TenantId, _hash: Hash, _data: Bytes) -> Result<()> {
        todo!("blobs: put_chunk")
    }
    async fn get_chunk(&self, _tenant: TenantId, _hash: Hash) -> Result<Bytes> {
        todo!("blobs: get_chunk")
    }
    async fn has_chunks(&self, _tenant: TenantId, _hashes: &[Hash]) -> Result<Vec<bool>> {
        todo!("blobs: has_chunks")
    }
    async fn delete_chunk(&self, _tenant: TenantId, _hash: Hash) -> Result<()> {
        todo!("blobs: delete_chunk")
    }
}

#[derive(Debug, Default)]
pub struct MemoryTransfers {}

#[async_trait]
impl Transfers for MemoryTransfers {
    async fn create_upload(&self, _ctx: &Context, _req: NewUpload) -> Result<Upload> {
        todo!("blobs: create_upload")
    }
    async fn get_upload(&self, _ctx: &Context, _id: UploadId) -> Result<Upload> {
        todo!("blobs: get_upload")
    }
    async fn put_chunk(
        &self,
        _ctx: &Context,
        _id: UploadId,
        _hash: Hash,
        _data: Bytes,
    ) -> Result<()> {
        todo!("blobs: put_chunk")
    }
    async fn complete_upload(
        &self,
        _ctx: &Context,
        _id: UploadId,
    ) -> Result<Written<(Node, Version)>> {
        todo!("blobs: complete_upload")
    }
    async fn abort_upload(&self, _ctx: &Context, _id: UploadId) -> Result<()> {
        todo!("blobs: abort_upload")
    }
    async fn open_content(
        &self,
        _ctx: &Context,
        _node: NodeId,
        _version: Option<VersionId>,
        _range: Option<ByteRange>,
    ) -> Result<Content> {
        todo!("blobs: open_content")
    }
}
