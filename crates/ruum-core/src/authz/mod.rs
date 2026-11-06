//! Authorization (`docs/DESIGN.md`, section 3): roles, permissions and the relationship engine.
//!
//! The engine is SpiceDB (schema in `contracts/authz/schema.zed`). This module holds the trait
//! that the rest of the core uses, an in-memory implementation for unit tests, and later the
//! SpiceDB client. In step 1 grants only add rights (decision 8).
//!
//! The right of an agent is the right of its user intersected with the agent scope
//! ([`effective_role`]). The same rule applies to share-link bearers (the link's node, role and
//! expiry) and later to apps that act on behalf of a user: all of them carry a [`Scope`].
//!
//! Writes to the graph go through the outbox ([`outbox`]): the Postgres change and its
//! relationship updates commit together, then the updates are applied to SpiceDB.

pub mod memory;
pub mod outbox;

use crate::actor::{Context, PrincipalKind};
use crate::error::Result;
use crate::ids::NodeId;
use crate::nodes::NodeStore;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// The roles of section 3.2, from lowest to highest. A higher role includes all lower ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    View,
    Comment,
    Edit,
    Manage,
    Share,
}

impl Role {
    /// The SpiceDB relation on `node` that a grant of this role writes.
    pub fn relation(self) -> &'static str {
        match self {
            Role::View => "viewer",
            Role::Comment => "commenter",
            Role::Edit => "editor",
            Role::Manage => "manager",
            Role::Share => "sharer",
        }
    }

    /// The SpiceDB permission that checks this role.
    pub fn permission(self) -> &'static str {
        match self {
            Role::View => "view",
            Role::Comment => "comment",
            Role::Edit => "edit",
            Role::Manage => "manage",
            Role::Share => "share",
        }
    }
}

/// What a principal with limited credentials may touch: one subtree, roles up to `max_role`,
/// until `expires_at`. Used for agents (from the agent row), share-link bearers (the link) and
/// apps acting for a user (step 3). The principal itself is `Context::actor`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    /// The root of the subtree, or `None` for the whole tenant.
    pub subtree: Option<NodeId>,
    /// The highest role that may be used.
    pub max_role: Role,
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub expires_at: Option<OffsetDateTime>,
}

/// An object in the relationship graph, for example `node:<uuid>`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectRef {
    pub object_type: String,
    pub object_id: String,
}

/// A subject, for example `user:<uuid>` or `group:<uuid>#member`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SubjectRef {
    pub object: ObjectRef,
    pub relation: Option<String>,
}

/// One relationship tuple: `object#relation@subject`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Relationship {
    pub object: ObjectRef,
    pub relation: String,
    pub subject: SubjectRef,
}

/// A change to the relationship graph.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelationshipUpdate {
    /// Create or keep.
    Touch(Relationship),
    Delete(Relationship),
}

/// A consistency token (SpiceDB ZedToken). Store it with the write and pass it to later checks
/// for read-after-write consistency.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsistencyToken(pub String);

/// The relationship engine.
#[async_trait]
pub trait Authz: Send + Sync {
    /// Whether the subject has the permission on the object.
    async fn check(
        &self,
        subject: &SubjectRef,
        permission: &str,
        object: &ObjectRef,
        at_least: Option<&ConsistencyToken>,
    ) -> Result<bool>;

    /// Many checks in one call, for filtering lists and search results.
    async fn check_bulk(
        &self,
        subject: &SubjectRef,
        permission: &str,
        objects: &[ObjectRef],
    ) -> Result<Vec<bool>>;

    async fn write(&self, updates: Vec<RelationshipUpdate>) -> Result<ConsistencyToken>;

    /// The relationships whose object matches. Used to list grants.
    async fn read(&self, object: &ObjectRef) -> Result<Vec<Relationship>>;
}

/// The graph subject whose rights apply to the request: `user:<id>` for a user and for an agent
/// (its user, `on_behalf_of`), `share_link:<id>` for a share-link bearer, `app:<id>` for an app
/// without a user. The scope in `ctx` then narrows that right.
pub fn subject_of(ctx: &Context) -> SubjectRef {
    let (object_type, id) = match (ctx.actor.effective_user(), ctx.actor.actor.kind) {
        (Some(user), _) => ("user", user.0),
        (None, PrincipalKind::ShareLink) => ("share_link", ctx.actor.actor.id.0),
        (None, PrincipalKind::App) => ("app", ctx.actor.actor.id.0),
        (None, PrincipalKind::Group) => ("group", ctx.actor.actor.id.0),
        (None, PrincipalKind::Agent | PrincipalKind::User) => ("user", ctx.actor.actor.id.0),
    };
    SubjectRef {
        object: ObjectRef {
            object_type: object_type.to_string(),
            object_id: id.to_string(),
        },
        relation: None,
    }
}

/// The highest role of the actor on a node, after the scope. `subject_role` is the role of the
/// subject of [`subject_of`] in the graph. `in_subtree` says whether the node is inside the scope
/// subtree (true when the scope has no subtree). An expired scope gives `None`.
pub fn effective_role(
    _subject_role: Option<Role>,
    _scope: Option<&Scope>,
    _in_subtree: bool,
    _now: OffsetDateTime,
) -> Option<Role> {
    todo!("authz: effective_role")
}

/// Checks that the actor in `ctx` has at least `role` on `node`; fails with `not_found` when the
/// actor cannot see the node, `permission_denied` when the subject lacks the role, and
/// `scope_exceeded` when only the scope stops it.
///
/// The scope subtree is checked by tree ancestry with [`NodeStore::ancestors`] for agents and
/// apps: a node is inside when the subtree root is the node or one of its ancestors. Member-link
/// targets outside the subtree are outside an agent's scope. For a share-link bearer the graph
/// already limits the link to its node, its subtree and its member-link targets, so only
/// `max_role` and `expires_at` of the scope are checked.
pub async fn require(
    _authz: &dyn Authz,
    _nodes: &dyn NodeStore,
    _ctx: &Context,
    _node: NodeId,
    _role: Role,
) -> Result<()> {
    todo!("authz: require")
}

#[cfg(test)]
mod tests {
    use super::Role;

    #[test]
    fn roles_are_ordered() {
        assert!(Role::View < Role::Comment);
        assert!(Role::Comment < Role::Edit);
        assert!(Role::Edit < Role::Manage);
        assert!(Role::Manage < Role::Share);
    }
}
