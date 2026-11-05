//! Authorization (`docs/DESIGN.md`, section 3): roles, permissions and the relationship engine.
//!
//! The engine is SpiceDB (schema in `contracts/authz/schema.zed`). This module holds the trait
//! that the rest of the core uses, an in-memory implementation for unit tests, and later the
//! SpiceDB client. In step 1 grants only add rights (decision 8).
//!
//! The right of an agent is the right of its user intersected with the agent scope
//! ([`effective_role`]).

pub mod memory;

use crate::actor::Context;
use crate::agents::AgentScope;
use crate::error::Result;
use crate::ids::NodeId;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

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

/// The highest role of the actor on a node, after the agent scope. `user_role` is the role of the
/// user for whom the actor acts. `in_subtree` says whether the node is inside the scope subtree.
pub fn effective_role(
    _user_role: Option<Role>,
    _scope: Option<&AgentScope>,
    _in_subtree: bool,
) -> Option<Role> {
    todo!("authz: effective_role")
}

/// Checks that the actor in `ctx` has at least `role` on `node`; fails with
/// `permission_denied` or `scope_exceeded`.
pub async fn require(_authz: &dyn Authz, _ctx: &Context, _node: NodeId, _role: Role) -> Result<()> {
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
