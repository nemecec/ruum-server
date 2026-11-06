//! The relationships that the core writes, in one place (the list at the end of
//! `contracts/authz/schema.zed`). Stores build their outbox updates with these functions, so the
//! tuple shapes never drift from the schema.

use super::{ObjectRef, Relationship, RelationshipUpdate, Role, SubjectRef};
use crate::ids::{NodeId, ShareLinkId, TenantId, UserId};
use crate::sharing::ShareLinkRole;

fn rel(object: ObjectRef, relation: &str, subject: SubjectRef) -> Relationship {
    Relationship {
        object,
        relation: relation.to_string(),
        subject,
    }
}

/// `node:N#parent@node:P`.
pub fn node_parent(node: NodeId, parent: NodeId) -> Relationship {
    rel(
        ObjectRef::node(node),
        "parent",
        SubjectRef::object(ObjectRef::node(parent)),
    )
}

/// `node:N#tenant@tenant:T`.
pub fn node_tenant(node: NodeId, tenant: TenantId) -> Relationship {
    rel(
        ObjectRef::node(node),
        "tenant",
        SubjectRef::object(ObjectRef::tenant(tenant)),
    )
}

/// `node:X#collection@node:F`: a member link in folder F to target X.
pub fn member_link(target: NodeId, folder: NodeId) -> Relationship {
    rel(
        ObjectRef::node(target),
        "collection",
        SubjectRef::object(ObjectRef::node(folder)),
    )
}

/// `node:R#owner@user:U`: only on the root of a personal tenant.
pub fn personal_root_owner(root: NodeId, user: UserId) -> Relationship {
    rel(ObjectRef::node(root), "owner", SubjectRef::user(user))
}

/// The tuples of a team root: `node:R#manager@tenant:T#administer` and `node:R#team@tenant:T`.
pub fn team_root(root: NodeId, tenant: TenantId) -> Vec<Relationship> {
    vec![
        rel(
            ObjectRef::node(root),
            "manager",
            SubjectRef::set(ObjectRef::tenant(tenant), "administer"),
        ),
        rel(
            ObjectRef::node(root),
            "team",
            SubjectRef::object(ObjectRef::tenant(tenant)),
        ),
    ]
}

/// `tenant:T#<owner|admin|member|guest>@user:U`.
pub fn tenant_role(tenant: TenantId, relation: &str, user: UserId) -> Relationship {
    rel(ObjectRef::tenant(tenant), relation, SubjectRef::user(user))
}

/// `tenant:T#members_can_share@user:*`: touched while the policy is on, deleted when off.
pub fn members_can_share(tenant: TenantId, on: bool) -> RelationshipUpdate {
    let r = rel(
        ObjectRef::tenant(tenant),
        "members_can_share",
        SubjectRef::any_user(),
    );
    if on {
        RelationshipUpdate::Touch(r)
    } else {
        RelationshipUpdate::Delete(r)
    }
}

/// The subject of a grant to a user (`user:U`) or to every member (`tenant:T#member`).
pub fn grant_subject_user(user: UserId) -> SubjectRef {
    SubjectRef::user(user)
}

/// `tenant:T#member`, the subject of a grant to every member of the tenant.
pub fn grant_subject_members(tenant: TenantId) -> SubjectRef {
    SubjectRef::set(ObjectRef::tenant(tenant), "member")
}

/// `node:N#<viewer|commenter|editor|manager>@<subject>`: a grant of `role`.
pub fn grant(node: NodeId, role: Role, subject: SubjectRef) -> Relationship {
    rel(ObjectRef::node(node), role.relation(), subject)
}

/// `node:N#link_<viewer|commenter|editor>@share_link:S`.
pub fn share_link(node: NodeId, link: ShareLinkId, role: ShareLinkRole) -> Relationship {
    rel(
        ObjectRef::node(node),
        role.relation(),
        SubjectRef::object(ObjectRef::new("share_link", link)),
    )
}

/// The updates for the node setting `editors_can_share`: `false` touches
/// `editors_share_off@user:*`, `true` touches `editors_share_on@user:*`, `None` (inherit)
/// touches neither; the other tuples are deleted.
pub fn editors_can_share(node: NodeId, value: Option<bool>) -> Vec<RelationshipUpdate> {
    let off = rel(
        ObjectRef::node(node),
        "editors_share_off",
        SubjectRef::any_user(),
    );
    let on = rel(
        ObjectRef::node(node),
        "editors_share_on",
        SubjectRef::any_user(),
    );
    match value {
        Some(false) => vec![
            RelationshipUpdate::Touch(off),
            RelationshipUpdate::Delete(on),
        ],
        Some(true) => vec![
            RelationshipUpdate::Delete(off),
            RelationshipUpdate::Touch(on),
        ],
        None => vec![
            RelationshipUpdate::Delete(off),
            RelationshipUpdate::Delete(on),
        ],
    }
}

/// The updates that change the role of a grant: delete the old relation, touch the new one.
pub fn change_grant(
    node: NodeId,
    old: Role,
    new: Role,
    subject: SubjectRef,
) -> Vec<RelationshipUpdate> {
    if old == new {
        return vec![RelationshipUpdate::Touch(grant(node, new, subject))];
    }
    vec![
        RelationshipUpdate::Delete(grant(node, old, subject.clone())),
        RelationshipUpdate::Touch(grant(node, new, subject)),
    ]
}
