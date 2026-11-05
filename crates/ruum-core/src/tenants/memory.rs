//! In-memory tenant store, for unit tests. Not implemented yet.

use super::*;

#[derive(Debug, Default)]
pub struct MemoryTenantStore {}

#[async_trait]
impl TenantStore for MemoryTenantStore {
    async fn list_for_user(&self, _user: UserId) -> Result<Vec<(Tenant, MemberRole)>> {
        todo!("tenants: list_for_user")
    }
    async fn create_personal(&self, _user: UserId, _name: String) -> Result<Tenant> {
        todo!("tenants: create_personal")
    }
    async fn create_team(&self, _actor: &Actor, _name: String) -> Result<Tenant> {
        todo!("tenants: create_team")
    }
    async fn get(&self, _ctx: &Context) -> Result<Tenant> {
        todo!("tenants: get")
    }
    async fn rename(&self, _ctx: &Context, _name: String) -> Result<Tenant> {
        todo!("tenants: rename")
    }
    async fn role_of(&self, _tenant: TenantId, _user: UserId) -> Result<Option<MemberRole>> {
        todo!("tenants: role_of")
    }
    async fn list_members(&self, _ctx: &Context, _page: PageRequest) -> Result<Page<Member>> {
        todo!("tenants: list_members")
    }
    async fn set_member_role(
        &self,
        _ctx: &Context,
        _user: UserId,
        _role: MemberRole,
    ) -> Result<Member> {
        todo!("tenants: set_member_role")
    }
    async fn remove_member(&self, _ctx: &Context, _user: UserId) -> Result<()> {
        todo!("tenants: remove_member")
    }
    async fn invite(
        &self,
        _ctx: &Context,
        _email: String,
        _role: MemberRole,
    ) -> Result<(Invitation, String)> {
        todo!("tenants: invite")
    }
    async fn list_invitations(
        &self,
        _ctx: &Context,
        _page: PageRequest,
    ) -> Result<Page<Invitation>> {
        todo!("tenants: list_invitations")
    }
    async fn revoke_invitation(&self, _ctx: &Context, _id: InvitationId) -> Result<()> {
        todo!("tenants: revoke_invitation")
    }
    async fn accept_invitation(&self, _actor: &Actor, _token: &str) -> Result<Member> {
        todo!("tenants: accept_invitation")
    }
    async fn usage(&self, _ctx: &Context) -> Result<Usage> {
        todo!("tenants: usage")
    }
}
