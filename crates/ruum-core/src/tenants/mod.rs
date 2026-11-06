//! Tenants (`docs/DESIGN.md`, section 4.1 and 4.3): personal tenants and small teams (decision 7),
//! members, guests, invitations, plans, limits and usage.
//!
//! A private person is a tenant with one member. A user can be a member of many tenants. People
//! from other tenants get access as guests of the owner tenant.

pub mod limits;
pub mod mail;
pub mod memory;
pub mod postgres;
pub mod relations;
pub mod rules;

#[cfg(test)]
mod tests;

use crate::actor::{Actor, Context};
use crate::error::Result;
use crate::ids::{InvitationId, NodeId, TenantId, UserId};
use crate::paging::{Page, PageRequest};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use time::OffsetDateTime;
use uuid::Uuid;

pub use limits::{Limits, PlanTable, configure_plans, plan_limits, tenant_limits};

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

/// What a tenant allows (section 4.1; `TenantPolicy` in `openapi.yaml`). Serialized into
/// `tenants.settings`; missing keys take the defaults below.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TenantPolicy {
    /// Share links may be made. When it turns off, existing links stop working.
    pub allow_share_links: bool,
    /// Share links may have the role `edit`.
    pub allow_edit_links: bool,
    /// When set, every share link needs an expiry at most this many days ahead.
    pub share_link_max_days: Option<u32>,
    /// Grants and invitations to people outside the tenant are allowed.
    pub allow_guests: bool,
    /// Team tenants: members manage the team tree through the team root, and while this is true
    /// they also have the `share` permission there (the tuple
    /// `tenant:<id>#members_can_share@user:*`, written or deleted when this changes).
    pub members_can_share: bool,
}

impl Default for TenantPolicy {
    fn default() -> Self {
        Self {
            allow_share_links: true,
            allow_edit_links: false,
            share_link_max_days: None,
            allow_guests: true,
            members_can_share: true,
        }
    }
}

/// A change of the tenant policy: fields that are `None` keep their value.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TenantPolicyChange {
    pub allow_share_links: Option<bool>,
    pub allow_edit_links: Option<bool>,
    #[serde(default, with = "double_option")]
    pub share_link_max_days: Option<Option<u32>>,
    pub allow_guests: Option<bool>,
    pub members_can_share: Option<bool>,
}

/// `Option<Option<T>>` for JSON: absent is `None`, `null` is `Some(None)`.
mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer, T: Serialize>(
        v: &Option<Option<T>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match v {
            Some(inner) => inner.serialize(s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        d: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(d).map(Some)
    }
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
    pub policy: TenantPolicy,
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

/// The current usage of a tenant.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    pub storage_bytes: i64,
    pub nodes: i64,
    pub agents: u32,
    pub members: u32,
}

/// What answers show of a principal (contract need C-7, decision 0003-5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrincipalName {
    /// The current name of a member, guest or agent, or the last name the tenant knew of a
    /// person who left.
    pub display_name: String,
    /// The email of a current member or guest. `None` for agents and for people who left. The
    /// API shows it only to callers who may list the members.
    pub email: Option<String>,
    /// The user an agent acts for. `None` for users.
    pub owner: Option<UserId>,
}

/// How long an invitation stays open.
pub const INVITATION_DAYS: i64 = 14;
/// The visible prefix of invitation tokens.
pub const INVITATION_PREFIX: &str = "ruum_inv_";

/// What the in-memory tenant store asks of the sharing area when a member leaves: remove the
/// user's direct grants in the tenant (`grant.removed` for each) and revoke the share links that
/// the user or the user's agents made there (`share_link.revoked` for each). The actor of these
/// entries is the caller in `ctx`. The Postgres store does the same in SQL, in its own
/// transaction, so it needs no hook.
///
/// It also binds email grants when an invitation is accepted, or when the user signs in with a
/// verified email: the email grants (`addGrant` with an email) to that email become grants to
/// the user.
#[async_trait]
pub trait MemberCleanup: Send + Sync {
    async fn member_removed(&self, ctx: &Context, user: UserId) -> Result<()>;
    async fn invitation_accepted(&self, ctx: &Context, email: &str, user: UserId) -> Result<()> {
        let _ = (ctx, email, user);
        Ok(())
    }
    /// A user signed in with `email` as a verified email of a trusted provider (a new user, or a
    /// new identity linked by that email): bind the email grants of `email` in every tenant to
    /// the user (as the Postgres store does in `bind_email_grants_on_sign_in`), except in a
    /// tenant where the user is not a member or guest and that does not allow guests. The actor
    /// of the journal entries is the user.
    async fn verified_email_signed_in(&self, email: &str, user: UserId) -> Result<()> {
        let _ = (email, user);
        Ok(())
    }
}

/// Called by the in-memory tenant store when it makes a tenant, so the in-memory node store can
/// hold the root folder (the Postgres store inserts the root row itself, in the same
/// transaction as the tenant).
pub trait RootCreated: Send + Sync {
    fn root_created(&self, tenant: &Tenant, owner: UserId);
}

#[async_trait]
pub trait TenantStore: Send + Sync {
    /// The tenants where the user is a member or guest, with the user's role.
    async fn list_for_user(&self, user: UserId) -> Result<Vec<(Tenant, MemberRole)>>;
    /// Makes a team tenant on the `team` plan; the creator becomes the owner. Users only.
    async fn create_team(&self, actor: &Actor, name: String) -> Result<Tenant>;
    /// The tenant of `ctx`. Members and guests (and agents of members and guests).
    async fn get(&self, ctx: &Context) -> Result<Tenant>;
    /// A system lookup of a tenant without an access check, for authentication and for the
    /// policy checks of other areas (for example the sharing area reads `policy`).
    async fn find(&self, tenant: TenantId) -> Result<Tenant>;
    /// Renames the tenant and changes its policy. Users only; needs `admin`.
    async fn update(
        &self,
        ctx: &Context,
        name: Option<String>,
        policy: Option<TenantPolicyChange>,
    ) -> Result<Tenant>;
    /// The role of a user in the tenant, or `None`. A system lookup.
    async fn role_of(&self, tenant: TenantId, user: UserId) -> Result<Option<MemberRole>>;
    /// Since when the user is a member, admin or owner of the tenant (not a guest): the join or
    /// the last change of the role, whichever is newer (`memberships.updated_at`). `None` for a
    /// guest or a stranger. A system lookup for the changes feed (decision 0002-6): access through
    /// the membership counts only from this time. A change between member, admin and owner also
    /// moves the time, so the answer is never too early. The default answers `None`.
    async fn member_since(&self, tenant: TenantId, user: UserId) -> Result<Option<OffsetDateTime>> {
        let _ = (tenant, user);
        Ok(None)
    }
    /// Whether `email` is the verified email of a member, admin or owner (not a guest) of the
    /// tenant, case-insensitive. A system lookup for the `allow_guests` policy of email grants
    /// (decision 0002-1). The default answers `false`, which refuses the grant.
    async fn is_verified_member_email(&self, tenant: TenantId, email: &str) -> Result<bool> {
        let _ = (tenant, email);
        Ok(false)
    }
    /// The names of `ids` in the tenant, for the names in answers (C-7, decision 0003-5). A
    /// lookup in this tenant only, with no access check: the API calls it only for principals
    /// that an answer it may send already names. In this order: current members and guests
    /// (their current name, from `users`), agents of the tenant (their name), then the
    /// tenant's snapshot of people who left (`principal_names`). Principals with none of these
    /// (unknown IDs, share-link bearers, other tenants' principals) are not in the map. The
    /// default knows no names.
    async fn principal_names(
        &self,
        tenant: TenantId,
        ids: &[Uuid],
    ) -> Result<HashMap<Uuid, PrincipalName>> {
        let _ = (tenant, ids);
        Ok(HashMap::new())
    }
    /// Members and guests, in the order they joined. Needs the tenant role `member`.
    async fn list_members(&self, ctx: &Context, page: PageRequest) -> Result<Page<Member>>;
    /// Changes the role of a member. Users only; needs `admin`; only an owner makes or removes
    /// owners; a tenant keeps at least one owner (`conflict`).
    async fn set_member_role(
        &self,
        ctx: &Context,
        user: UserId,
        role: MemberRole,
    ) -> Result<Member>;
    /// Removes a member. Users only; needs `admin`, or the member removes themselves; the last
    /// owner cannot leave (`conflict`). In one transaction it also removes the user's direct
    /// grants in this tenant, revokes the share links that the user or the user's agents made
    /// here, disables the user's agents (`owner_left`), ends their OAuth grants and revokes their
    /// app passwords for the tenant (section 3.4; `removeMember` in `openapi.yaml`). Grants and
    /// member links the user made for others stay.
    async fn remove_member(&self, ctx: &Context, user: UserId) -> Result<()>;
    /// Invites a person by email and sends the email. Users only; needs `admin`. Returns the
    /// invitation and its secret token (in the email only).
    async fn invite(
        &self,
        ctx: &Context,
        email: String,
        role: MemberRole,
        message: Option<String>,
    ) -> Result<(Invitation, String)>;
    /// The open invitations, newest first. Users only; needs `admin`.
    async fn list_invitations(&self, ctx: &Context, page: PageRequest) -> Result<Page<Invitation>>;
    async fn revoke_invitation(&self, ctx: &Context, id: InvitationId) -> Result<()>;
    /// Accepts an invitation by its secret token, for the signed-in user. A user who is already a
    /// member keeps the higher of the two roles.
    async fn accept_invitation(&self, actor: &Actor, token: &str) -> Result<Member>;
    /// The current usage. Needs the tenant role `member`.
    async fn usage(&self, ctx: &Context) -> Result<Usage>;
}
