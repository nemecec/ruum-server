//! The relationship tuples that tenants, members and agents write (`contracts/authz/schema.zed`,
//! the comment at its end, and decision 0001-4).
//!
//! The Postgres stores put them in the outbox in the transaction of the change; the in-memory
//! stores write them to the in-memory engine.

use super::{MemberRole, TenantKind};
use crate::authz::{ObjectRef, Relationship, RelationshipUpdate, Role, SubjectRef};
use crate::ids::{AgentId, NodeId, TenantId, UserId};

fn obj(object_type: &str, id: impl ToString) -> ObjectRef {
    ObjectRef {
        object_type: object_type.to_string(),
        object_id: id.to_string(),
    }
}

fn subject(object_type: &str, id: impl ToString, relation: Option<&str>) -> SubjectRef {
    SubjectRef {
        object: obj(object_type, id),
        relation: relation.map(str::to_string),
    }
}

fn rel(object: ObjectRef, relation: &str, subject: SubjectRef) -> Relationship {
    Relationship {
        object,
        relation: relation.to_string(),
        subject,
    }
}

fn user(u: UserId) -> SubjectRef {
    subject("user", u, None)
}

fn tenant(t: TenantId) -> ObjectRef {
    obj("tenant", t)
}

/// `tenant:<t>#members_can_share@user:*`.
pub fn members_can_share_tuple(t: TenantId) -> Relationship {
    rel(tenant(t), "members_can_share", subject("user", "*", None))
}

/// A membership as the graph sees it: the tenant role and, for members only, the content role
/// (decision 0003-1).
pub type Access = (MemberRole, Option<Role>);

/// The relation of a member's content role below `manage` (none for `manage`, the default).
fn content_relation(content: Role) -> Option<&'static str> {
    match content {
        Role::View => Some("content_viewer"),
        Role::Comment => Some("content_commenter"),
        Role::Edit => Some("content_editor"),
        Role::Manage => None,
    }
}

/// The relations of a membership (the rebuild mapping in the header of `schema.zed`): owners
/// and admins are also members; guests are only guests; a member with a content role below
/// `manage` also has its `content_*` relation. Only members get a content relation.
fn role_relations((role, content): Access) -> Vec<&'static str> {
    let mut rels = match role {
        MemberRole::Owner => vec!["owner", "member"],
        MemberRole::Admin => vec!["admin", "member"],
        MemberRole::Member => vec!["member"],
        MemberRole::Guest => vec!["guest"],
    };
    if role == MemberRole::Member
        && let Some(r) = content.and_then(content_relation)
    {
        rels.push(r);
    }
    rels
}

/// The tuples of a membership change from `old` to `new` (`None` = not a member): every
/// relation of `old` that `new` does not have is deleted, every relation of `new` is written.
pub fn membership(
    t: TenantId,
    u: UserId,
    old: Option<Access>,
    new: Option<Access>,
) -> Vec<RelationshipUpdate> {
    let old_rels: Vec<&str> = old.map(role_relations).unwrap_or_default();
    let new_rels: Vec<&str> = new.map(role_relations).unwrap_or_default();
    let mut out = Vec::new();
    for r in &old_rels {
        if !new_rels.contains(r) {
            out.push(RelationshipUpdate::Delete(rel(tenant(t), r, user(u))));
        }
    }
    for r in &new_rels {
        out.push(RelationshipUpdate::Touch(rel(tenant(t), r, user(u))));
    }
    out
}

/// The tuples of a new tenant with root folder `root`, made by `owner`.
pub fn tenant_created(
    kind: TenantKind,
    t: TenantId,
    root: NodeId,
    owner: UserId,
    members_can_share: bool,
) -> Vec<RelationshipUpdate> {
    let mut out = membership(t, owner, None, Some((MemberRole::Owner, None)));
    let root_obj = obj("node", root);
    out.push(RelationshipUpdate::Touch(rel(
        root_obj.clone(),
        "tenant",
        subject("tenant", t, None),
    )));
    match kind {
        TenantKind::Personal => {
            out.push(RelationshipUpdate::Touch(rel(
                root_obj,
                "owner",
                user(owner),
            )));
        }
        TenantKind::Team => {
            out.push(RelationshipUpdate::Touch(rel(
                root_obj.clone(),
                "manager",
                subject("tenant", t, Some("administer")),
            )));
            out.push(RelationshipUpdate::Touch(rel(
                root_obj,
                "team",
                subject("tenant", t, None),
            )));
            if members_can_share {
                out.push(RelationshipUpdate::Touch(members_can_share_tuple(t)));
            }
        }
    }
    out
}

/// The tuple change of the policy `members_can_share`.
pub fn members_can_share(t: TenantId, on: bool) -> Vec<RelationshipUpdate> {
    if on {
        vec![RelationshipUpdate::Touch(members_can_share_tuple(t))]
    } else {
        vec![RelationshipUpdate::Delete(members_can_share_tuple(t))]
    }
}

/// The tuples of a new agent: `agent:<a>#tenant@tenant:<t>`, `agent:<a>#acts_for@user:<u>`.
pub fn agent_registered(a: AgentId, t: TenantId, owner: UserId) -> Vec<RelationshipUpdate> {
    vec![
        RelationshipUpdate::Touch(rel(obj("agent", a), "tenant", subject("tenant", t, None))),
        RelationshipUpdate::Touch(rel(obj("agent", a), "acts_for", user(owner))),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(u: &[RelationshipUpdate]) -> Vec<String> {
        u.iter()
            .map(|x| {
                let (op, r) = match x {
                    RelationshipUpdate::Touch(r) => ("+", r),
                    RelationshipUpdate::Delete(r) => ("-", r),
                };
                format!(
                    "{op}{}:{}#{}@{}:{}{}",
                    r.object.object_type,
                    r.object.object_id,
                    r.relation,
                    r.subject.object.object_type,
                    r.subject.object.object_id,
                    r.subject
                        .relation
                        .as_ref()
                        .map(|s| format!("#{s}"))
                        .unwrap_or_default()
                )
            })
            .collect()
    }

    #[test]
    fn a_team_root_gets_the_decision_0001_4_tuples() {
        let (t, r, u) = (TenantId::new(), NodeId::new(), UserId::new());
        let n = names(&tenant_created(TenantKind::Team, t, r, u, true));
        assert!(n.contains(&format!("+tenant:{t}#owner@user:{u}")));
        assert!(n.contains(&format!("+tenant:{t}#member@user:{u}")));
        assert!(n.contains(&format!("+node:{r}#tenant@tenant:{t}")));
        assert!(n.contains(&format!("+node:{r}#manager@tenant:{t}#administer")));
        assert!(n.contains(&format!("+node:{r}#team@tenant:{t}")));
        assert!(n.contains(&format!("+tenant:{t}#members_can_share@user:*")));
        assert!(
            !n.iter()
                .any(|s| s.contains("#owner@user") && s.starts_with("+node"))
        );
    }

    #[test]
    fn a_personal_root_has_an_owner() {
        let (t, r, u) = (TenantId::new(), NodeId::new(), UserId::new());
        let n = names(&tenant_created(TenantKind::Personal, t, r, u, true));
        assert!(n.contains(&format!("+node:{r}#owner@user:{u}")));
        assert!(!n.iter().any(|s| s.contains("#team@")));
    }

    #[test]
    fn a_role_change_moves_the_relations() {
        let (t, u) = (TenantId::new(), UserId::new());
        let n = names(&membership(
            t,
            u,
            Some((MemberRole::Admin, None)),
            Some((MemberRole::Guest, None)),
        ));
        assert!(n.contains(&format!("-tenant:{t}#admin@user:{u}")));
        assert!(n.contains(&format!("-tenant:{t}#member@user:{u}")));
        assert!(n.contains(&format!("+tenant:{t}#guest@user:{u}")));
        let n = names(&membership(
            t,
            u,
            Some((MemberRole::Member, Some(Role::Manage))),
            None,
        ));
        assert_eq!(n, vec![format!("-tenant:{t}#member@user:{u}")]);
    }

    /// The rebuild mapping of `schema.zed` (decision 0003-1): a content role below `manage` is
    /// one `content_*` tuple; `manage`, owners, admins and guests have none.
    #[test]
    fn content_roles_map_to_one_tuple() {
        let (t, u) = (TenantId::new(), UserId::new());
        let new = |a: Access| names(&membership(t, u, None, Some(a)));
        assert_eq!(
            new((MemberRole::Member, Some(Role::Manage))),
            vec![format!("+tenant:{t}#member@user:{u}")]
        );
        for (r, rel) in [
            (Role::Edit, "content_editor"),
            (Role::Comment, "content_commenter"),
            (Role::View, "content_viewer"),
        ] {
            assert_eq!(
                new((MemberRole::Member, Some(r))),
                vec![
                    format!("+tenant:{t}#member@user:{u}"),
                    format!("+tenant:{t}#{rel}@user:{u}"),
                ]
            );
        }
        // A content role on another role never makes a tuple.
        assert!(
            !new((MemberRole::Admin, Some(Role::View)))
                .iter()
                .any(|s| s.contains("content_"))
        );
        // A change of the content role moves the tuple; a promotion clears it.
        let n = names(&membership(
            t,
            u,
            Some((MemberRole::Member, Some(Role::View))),
            Some((MemberRole::Member, Some(Role::Edit))),
        ));
        assert!(n.contains(&format!("-tenant:{t}#content_viewer@user:{u}")));
        assert!(n.contains(&format!("+tenant:{t}#content_editor@user:{u}")));
        let n = names(&membership(
            t,
            u,
            Some((MemberRole::Member, Some(Role::Comment))),
            Some((MemberRole::Admin, None)),
        ));
        assert!(n.contains(&format!("-tenant:{t}#content_commenter@user:{u}")));
        assert!(
            !n.iter()
                .any(|s| s.starts_with("+") && s.contains("content_"))
        );
        let n = names(&membership(
            t,
            u,
            Some((MemberRole::Member, Some(Role::View))),
            None,
        ));
        assert!(n.contains(&format!("-tenant:{t}#content_viewer@user:{u}")));
    }
}
