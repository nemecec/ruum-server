//! Sharing (`docs/DESIGN.md`, sections 3.1-3.3): grants of roles on nodes, secret share links
//! with expiry, and the "Shared with me" index.
//!
//! A grant writes a relationship to the authz engine and a row to `share_index` (the roots of
//! grants per principal), so "Shared with me" is a list query, not a graph walk.
//! Guests from other tenants and share links are allowed (decision 9); public files come later.
//! The tenant policy (`crate::tenants::TenantPolicy`) can forbid guests, links or edit links.
//!
//! Relationship writes go through the outbox ([`crate::authz::outbox`]), so Postgres and SpiceDB
//! never diverge.

pub mod memory;
pub mod postgres;
pub mod rules;

#[cfg(test)]
mod conformance;

use crate::actor::{Actor, Context};
use crate::authz::Role;
use crate::error::Result;
use crate::ids::{GrantId, NodeId, ShareLinkId, TenantId, UserId};
use crate::journal::Written;
use crate::nodes::Node;
use crate::paging::{Page, PageRequest};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// The principal of a grant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Grantee {
    /// A user by ID (a member or a guest).
    User { id: UserId },
    /// A person by email (decision 0003-2). When the caller may list the members (a member,
    /// admin or owner, or an agent of one) and the email is the verified email of a person of
    /// the tenant, `addGrant` stores a user grant instead. Otherwise it stays an email grant,
    /// which binds to a user only by a verified email in an interactive user session (sign-in,
    /// or the bind check of `getMe` and `listSharedWithMe` with the session cookie), or through
    /// an accepted admin invitation; an unverified email never claims it.
    Email { email: String },
    /// Everybody in the tenant of the node (`tenant#member`).
    TenantMembers,
}

/// A grant of a role on a node.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Grant {
    pub id: GrantId,
    pub node_id: NodeId,
    pub grantee: Grantee,
    pub role: Role,
    /// Set when the grant is on an ancestor and only shows here by inheritance.
    pub inherited_from: Option<NodeId>,
    pub created_by: Actor,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// The roles a share link can have. Never `manage` (also checked in SQL), and a link never has
/// the `share` permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareLinkRole {
    View,
    Comment,
    /// Only when the tenant policy has `allow_edit_links`.
    Edit,
}

impl ShareLinkRole {
    pub fn as_role(self) -> Role {
        match self {
            ShareLinkRole::View => Role::View,
            ShareLinkRole::Comment => Role::Comment,
            ShareLinkRole::Edit => Role::Edit,
        }
    }

    /// The SpiceDB relation on `node` that a link of this role writes (`node:N#<relation>@share_link:S`).
    /// Links have their own relations, so that they never get the `share` permission.
    pub fn relation(self) -> &'static str {
        match self {
            ShareLinkRole::View => "link_viewer",
            ShareLinkRole::Comment => "link_commenter",
            ShareLinkRole::Edit => "link_editor",
        }
    }
}

/// A secret share link (`anyone_with_link`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShareLink {
    pub id: ShareLinkId,
    pub node_id: NodeId,
    pub role: ShareLinkRole,
    /// The secret token. Only the create response has it; it is stored as a hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// The link to give to people. Only the create response has it (the API fills it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
    pub has_password: bool,
    pub created_by: Actor,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
}

/// A request to make a share link.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewShareLink {
    pub role: ShareLinkRole,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub expires_at: Option<OffsetDateTime>,
    pub password: Option<String>,
}

/// A request of `addGrant`.
#[derive(Clone, Debug)]
pub struct NewGrant {
    pub grantee: Grantee,
    pub role: Role,
    /// Queue a share email to the grantee (decision 0003-3). The default of the API is `true`.
    pub notify: bool,
    /// The personal message of the share email (at most 1000 characters).
    pub message: Option<String>,
}

/// The answer of `addGrant`.
#[derive(Clone, Debug)]
pub struct GrantAdded {
    pub written: Written<Grant>,
    /// A share email was queued in the same transaction as the grant (it says nothing about
    /// delivery). False without `notify`, for a grant to `tenant_members`, over the mail
    /// limits, or when the same recipient got a mail for the node in the last 10 minutes.
    pub notified: bool,
}

/// One row of "Shared with me".
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SharedItem {
    pub node: Node,
    pub role: Role,
    pub shared_by: Actor,
    #[serde(with = "time::serde::rfc3339")]
    pub shared_at: OffsetDateTime,
}

#[async_trait]
pub trait SharingStore: Send + Sync {
    /// The direct and inherited grants on a node. Needs `view` on the node.
    async fn list_grants(
        &self,
        ctx: &Context,
        node: NodeId,
        page: PageRequest,
    ) -> Result<Page<Grant>>;
    /// Adds a grant (`addGrant`). Needs the `share` permission on the node. Counts against the
    /// `addGrant` limits first, also when the call is refused (decision 0003-2): 60 calls per
    /// 10 minutes per caller user (agents count against their user) and tenant, 600 per
    /// tenant; over the limit it fails with `rate_limited`. With `notify` it queues the share
    /// email in the grant's transaction (decision 0003-3).
    async fn share(&self, ctx: &Context, node: NodeId, req: NewGrant) -> Result<GrantAdded>;
    /// [`SharingStore::share`] without a share email.
    async fn add_grant(
        &self,
        ctx: &Context,
        node: NodeId,
        grantee: Grantee,
        role: Role,
    ) -> Result<Written<Grant>> {
        let req = NewGrant {
            grantee,
            role,
            notify: false,
            message: None,
        };
        Ok(self.share(ctx, node, req).await?.written)
    }
    /// Changes the role of a grant in one step (`grant.updated`). Needs `share` on the node.
    async fn update_grant(
        &self,
        ctx: &Context,
        node: NodeId,
        grant: GrantId,
        role: Role,
    ) -> Result<Written<Grant>>;
    async fn remove_grant(
        &self,
        ctx: &Context,
        node: NodeId,
        grant: GrantId,
    ) -> Result<Written<()>>;
    async fn list_share_links(
        &self,
        ctx: &Context,
        node: NodeId,
        page: PageRequest,
    ) -> Result<Page<ShareLink>>;
    async fn create_share_link(
        &self,
        ctx: &Context,
        node: NodeId,
        req: NewShareLink,
    ) -> Result<Written<ShareLink>>;
    /// Revokes a link. Needs `share` on the link's node. A link of another tenant is `not_found`.
    async fn revoke_share_link(&self, ctx: &Context, link: ShareLinkId) -> Result<Written<()>>;
    /// Resolves a share link token without sign-in. Returns the link, its tenant and node. A link
    /// that the tenant policy no longer allows is `not_found`.
    async fn resolve_share_link(
        &self,
        token: &str,
        password: Option<&str>,
    ) -> Result<(TenantId, Node, ShareLink)>;
    /// The grant roots for the actor's user, across all tenants where the user is a member or
    /// guest. `ctx.tenant` is ignored for this call, except for agents: an agent sees only the
    /// items of its own tenant that are inside its scope.
    async fn shared_with_me(&self, ctx: &Context, page: PageRequest) -> Result<Page<SharedItem>>;
    /// Sets the node setting `editors_can_share` (`true`, `false`, or `None` to inherit) and
    /// writes the tuples `editors_share_off` / `editors_share_on` (`updateNodeSharing`). Needs
    /// `manage`. `if_revision` is the `If-Match` revision of the node. Writes
    /// `node.sharing_updated`. Returns the node after the change.
    async fn update_node_sharing(
        &self,
        ctx: &Context,
        node: NodeId,
        editors_can_share: Option<bool>,
        if_revision: Option<i64>,
    ) -> Result<Written<Node>>;
}
