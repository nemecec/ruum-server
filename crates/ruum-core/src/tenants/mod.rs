//! Tenants (`docs/DESIGN.md`, section 4.1 and 4.3): personal tenants and small teams (decision 7),
//! members, guests, invitations, plans, limits and usage.
//!
//! A private person is a tenant with one member. A user can be a member of many tenants. People
//! from other tenants get access as guests of the owner tenant.

pub mod memory;

use crate::actor::{Actor, Context};
use crate::error::Result;
use crate::ids::{InvitationId, NodeId, TenantId, UserId};
use crate::paging::{Page, PageRequest};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "tenant_kind", rename_all = "snake_case")]
pub enum TenantKind {
    Personal,
    Team,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "plan", rename_all = "snake_case")]
pub enum Plan {
    Free,
    Personal,
    Team,
}

/// The role of a member in a tenant.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "member_role", rename_all = "snake_case")]
pub enum MemberRole {
    /// A person from another tenant; sees only what is shared with them.
    Guest,
    Member,
    Admin,
    Owner,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tenant {
    pub id: TenantId,
    pub name: String,
    pub kind: TenantKind,
    pub plan: Plan,
    /// The data region, for example `hel1`.
    pub region: String,
    pub root_node_id: NodeId,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Member {
    pub tenant_id: TenantId,
    pub user_id: UserId,
    pub email: String,
    pub display_name: String,
    pub role: MemberRole,
    #[serde(with = "time::serde::rfc3339")]
    pub joined_at: OffsetDateTime,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Invitation {
    pub id: InvitationId,
    pub tenant_id: TenantId,
    pub email: String,
    pub role: MemberRole,
    pub invited_by: Actor,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub accepted_at: Option<OffsetDateTime>,
}

/// The limits of a plan (section 4.3). The numbers are configuration, not code.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Limits {
    pub storage_bytes: i64,
    pub max_file_bytes: i64,
    pub version_days: Option<u32>,
    pub version_count: Option<u32>,
    pub trash_days: u32,
    pub agents: u32,
    pub members: Option<u32>,
}

/// The current usage of a tenant.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    pub storage_bytes: i64,
    pub nodes: i64,
    pub agents: u32,
    pub members: u32,
}

/// The limits of a plan. Reads the plan table from configuration.
pub fn plan_limits(_plan: Plan) -> Limits {
    todo!("tenants: plan_limits")
}

#[async_trait]
pub trait TenantStore: Send + Sync {
    /// The tenants where the user is a member or guest.
    async fn list_for_user(&self, user: UserId) -> Result<Vec<(Tenant, MemberRole)>>;
    /// Makes the personal tenant of a new user, with its root folder.
    async fn create_personal(&self, user: UserId, name: String) -> Result<Tenant>;
    /// Makes a team tenant; the creator becomes the owner.
    async fn create_team(&self, actor: &Actor, name: String) -> Result<Tenant>;
    async fn get(&self, ctx: &Context) -> Result<Tenant>;
    async fn rename(&self, ctx: &Context, name: String) -> Result<Tenant>;
    /// The role of a user in the tenant, or `None`.
    async fn role_of(&self, tenant: TenantId, user: UserId) -> Result<Option<MemberRole>>;
    async fn list_members(&self, ctx: &Context, page: PageRequest) -> Result<Page<Member>>;
    async fn set_member_role(
        &self,
        ctx: &Context,
        user: UserId,
        role: MemberRole,
    ) -> Result<Member>;
    /// Removes a member. Also disables the agents of that user in this tenant (section 3.4).
    async fn remove_member(&self, ctx: &Context, user: UserId) -> Result<()>;
    async fn invite(
        &self,
        ctx: &Context,
        email: String,
        role: MemberRole,
    ) -> Result<(Invitation, String)>;
    async fn list_invitations(&self, ctx: &Context, page: PageRequest) -> Result<Page<Invitation>>;
    async fn revoke_invitation(&self, ctx: &Context, id: InvitationId) -> Result<()>;
    /// Accepts an invitation by its secret token, for the signed-in user.
    async fn accept_invitation(&self, actor: &Actor, token: &str) -> Result<Member>;
    async fn usage(&self, ctx: &Context) -> Result<Usage>;
}
