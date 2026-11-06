//! In-memory sharing store, for unit tests. Not implemented yet.

use super::*;

#[derive(Debug, Default)]
pub struct MemorySharingStore {}

#[async_trait]
impl SharingStore for MemorySharingStore {
    async fn list_grants(
        &self,
        _ctx: &Context,
        _node: NodeId,
        _page: PageRequest,
    ) -> Result<Page<Grant>> {
        todo!("sharing: list_grants")
    }
    async fn add_grant(
        &self,
        _ctx: &Context,
        _node: NodeId,
        _grantee: Grantee,
        _role: Role,
    ) -> Result<Written<Grant>> {
        todo!("sharing: add_grant")
    }
    async fn update_grant(
        &self,
        _ctx: &Context,
        _node: NodeId,
        _grant: GrantId,
        _role: Role,
    ) -> Result<Written<Grant>> {
        todo!("sharing: update_grant")
    }
    async fn remove_grant(
        &self,
        _ctx: &Context,
        _node: NodeId,
        _grant: GrantId,
    ) -> Result<Written<()>> {
        todo!("sharing: remove_grant")
    }
    async fn list_share_links(
        &self,
        _ctx: &Context,
        _node: NodeId,
        _page: PageRequest,
    ) -> Result<Page<ShareLink>> {
        todo!("sharing: list_share_links")
    }
    async fn create_share_link(
        &self,
        _ctx: &Context,
        _node: NodeId,
        _req: NewShareLink,
    ) -> Result<Written<ShareLink>> {
        todo!("sharing: create_share_link")
    }
    async fn revoke_share_link(&self, _ctx: &Context, _link: ShareLinkId) -> Result<Written<()>> {
        todo!("sharing: revoke_share_link")
    }
    async fn resolve_share_link(
        &self,
        _token: &str,
        _password: Option<&str>,
    ) -> Result<(TenantId, Node, ShareLink)> {
        todo!("sharing: resolve_share_link")
    }
    async fn shared_with_me(&self, _ctx: &Context, _page: PageRequest) -> Result<Page<SharedItem>> {
        todo!("sharing: shared_with_me")
    }
}
