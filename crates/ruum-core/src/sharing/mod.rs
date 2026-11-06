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
    /// A person by email. The grant stays an email grant and sends an invitation. It binds to a
    /// user only through the invitation token, or a verified email of a trusted provider; an
    /// unverified email never claims it.
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
    pub token: Option<String>,
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
    /// Adds a grant. Needs the `share` permission on the node.
    async fn add_grant(
        &self,
        ctx: &Context,
        node: NodeId,
        grantee: Grantee,
        role: Role,
    ) -> Result<Written<Grant>>;
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
    async fn revoke_share_link(&self, ctx: &Context, link: ShareLinkId) -> Result<Written<()>>;
    /// Resolves a share link token without sign-in. Returns the link, its tenant and node. A link
    /// that the tenant policy no longer allows is `not_found`.
    async fn resolve_share_link(
        &self,
        token: &str,
        password: Option<&str>,
    ) -> Result<(TenantId, Node, ShareLink)>;
    /// The grant roots for the actor's user, across all tenants where the user is a member or
    /// guest. `ctx.tenant` is ignored for this call.
    async fn shared_with_me(&self, ctx: &Context, page: PageRequest) -> Result<Page<SharedItem>>;
}
