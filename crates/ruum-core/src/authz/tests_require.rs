//! Tests of [`require`], [`effective_role`] and [`subject_of`] on the fixture tree of the
//! security harness (`crates/ruum-testkit/src/fixtures.rs`), with the in-memory engine.

use super::memory::MemoryAuthz;
use super::*;
use crate::actor::{Actor, PrincipalRef};
use crate::error::ErrorCode;
use crate::ids::PrincipalId;
use uuid::Uuid;

use super::testing::TreeNodes;

struct World {
    authz: MemoryAuthz,
    nodes: TreeNodes,
    tenant: TenantId,
    root: NodeId,
    projects: NodeId,
    plan: NodeId,
    archive: NodeId,
    private: NodeId,
    owner: UserId,
    member: UserId,
    guest: UserId,
    guest_view: UserId,
    guest_edit: UserId,
    guest_manage: UserId,
    link: ShareLinkId,
}

use crate::ids::ShareLinkId;
use crate::sharing::ShareLinkRole;

async fn world(members_can_share: bool) -> World {
    let w = World {
        authz: MemoryAuthz::default(),
        nodes: TreeNodes::default(),
        tenant: TenantId::new(),
        root: NodeId::new(),
        projects: NodeId::new(),
        plan: NodeId::new(),
        archive: NodeId::new(),
        private: NodeId::new(),
        owner: UserId::new(),
        member: UserId::new(),
        guest: UserId::new(),
        guest_view: UserId::new(),
        guest_edit: UserId::new(),
        guest_manage: UserId::new(),
        link: ShareLinkId::new(),
    };
    for (n, p) in [
        (w.projects, w.root),
        (w.plan, w.projects),
        (w.archive, w.projects),
        (w.private, w.root),
    ] {
        w.nodes.add(n, p);
    }
    let mut rels = vec![
        tuples::tenant_role(w.tenant, "owner", w.owner),
        tuples::tenant_role(w.tenant, "member", w.owner),
        tuples::tenant_role(w.tenant, "member", w.member),
        tuples::tenant_role(w.tenant, "guest", w.guest),
        tuples::node_tenant(w.root, w.tenant),
        tuples::node_parent(w.projects, w.root),
        tuples::node_parent(w.plan, w.projects),
        tuples::node_parent(w.archive, w.projects),
        tuples::node_parent(w.private, w.root),
        tuples::grant(w.projects, Role::View, SubjectRef::user(w.guest_view)),
        tuples::grant(w.projects, Role::Edit, SubjectRef::user(w.guest_edit)),
        tuples::grant(w.projects, Role::Manage, SubjectRef::user(w.guest_manage)),
        tuples::share_link(w.projects, w.link, ShareLinkRole::View),
    ];
    rels.extend(tuples::team_root(w.root, w.tenant));
    let mut updates: Vec<RelationshipUpdate> =
        rels.into_iter().map(RelationshipUpdate::Touch).collect();
    updates.extend(tuples::editors_can_share(w.archive, Some(false)));
    updates.push(tuples::members_can_share(w.tenant, members_can_share));
    w.authz.write(updates).await.unwrap();
    w
}

fn user_ctx(w: &World, u: UserId) -> Context {
    Context {
        tenant: w.tenant,
        actor: Actor::user(u),
        scope: None,
        idempotency_key: None,
    }
}

fn agent_ctx(w: &World, u: UserId, subtree: Option<NodeId>, max_role: Role) -> Context {
    Context {
        tenant: w.tenant,
        actor: Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::Agent,
                id: PrincipalId(Uuid::now_v7()),
            },
            on_behalf_of: Some(u),
        },
        scope: Some(Scope {
            subtree,
            max_role,
            expires_at: None,
        }),
        idempotency_key: None,
    }
}

fn link_ctx(w: &World) -> Context {
    Context {
        tenant: w.tenant,
        actor: Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::ShareLink,
                id: PrincipalId(w.link.0),
            },
            on_behalf_of: None,
        },
        scope: Some(Scope {
            subtree: Some(w.projects),
            max_role: Role::View,
            expires_at: None,
        }),
        idempotency_key: None,
    }
}

async fn outcome(
    w: &World,
    ctx: &Context,
    node: NodeId,
    p: impl Into<Permission> + Send,
) -> &'static str {
    match require(&w.authz, &w.nodes, ctx, node, p).await {
        Ok(()) => "ok",
        Err(Error::Api(e)) => match e.code {
            ErrorCode::NotFound => "404",
            ErrorCode::PermissionDenied => "403",
            ErrorCode::ScopeExceeded => "scope",
            _ => "other",
        },
        Err(_) => "err",
    }
}

#[tokio::test]
async fn users_follow_the_graph() {
    let w = world(true).await;
    let gv = user_ctx(&w, w.guest_view);
    assert_eq!(outcome(&w, &gv, w.plan, Role::View).await, "ok");
    assert_eq!(outcome(&w, &gv, w.plan, Role::Edit).await, "403");
    assert_eq!(outcome(&w, &gv, w.private, Role::View).await, "404");
    assert_eq!(outcome(&w, &gv, w.root, Role::View).await, "404");
    assert_eq!(outcome(&w, &gv, w.projects, Permission::Share).await, "403");

    let ge = user_ctx(&w, w.guest_edit);
    assert_eq!(outcome(&w, &ge, w.projects, Permission::Share).await, "ok");
    assert_eq!(outcome(&w, &ge, w.archive, Permission::Share).await, "403");
    assert_eq!(outcome(&w, &ge, w.archive, Role::Edit).await, "ok");
    assert_eq!(outcome(&w, &ge, w.archive, Role::Manage).await, "403");

    let gm = user_ctx(&w, w.guest_manage);
    assert_eq!(outcome(&w, &gm, w.archive, Permission::Share).await, "ok");

    let g = user_ctx(&w, w.guest);
    assert_eq!(outcome(&w, &g, w.projects, Role::View).await, "404");

    let m = user_ctx(&w, w.member);
    assert_eq!(outcome(&w, &m, w.private, Role::Manage).await, "ok");
    assert_eq!(outcome(&w, &m, w.private, Permission::Share).await, "ok");
    let o = user_ctx(&w, w.owner);
    assert_eq!(outcome(&w, &o, w.archive, Permission::Share).await, "ok");
}

#[tokio::test]
async fn members_share_only_with_the_policy() {
    let w = world(false).await;
    let m = user_ctx(&w, w.member);
    assert_eq!(outcome(&w, &m, w.private, Role::Manage).await, "ok");
    assert_eq!(outcome(&w, &m, w.private, Permission::Share).await, "403");
    let o = user_ctx(&w, w.owner);
    assert_eq!(outcome(&w, &o, w.private, Permission::Share).await, "ok");
}

#[tokio::test]
async fn agents_get_the_intersection() {
    let w = world(true).await;
    // The narrow agent of member: scope Projects, view.
    let a = agent_ctx(&w, w.member, Some(w.projects), Role::View);
    assert_eq!(outcome(&w, &a, w.plan, Role::View).await, "ok");
    assert_eq!(outcome(&w, &a, w.plan, Role::Edit).await, "scope");
    assert_eq!(
        outcome(&w, &a, w.projects, Permission::Share).await,
        "scope"
    );
    assert_eq!(outcome(&w, &a, w.root, Role::View).await, "404");
    assert_eq!(outcome(&w, &a, w.private, Role::View).await, "404");
    // agent_out: scope Private, manage.
    let a = agent_ctx(&w, w.member, Some(w.private), Role::Manage);
    assert_eq!(outcome(&w, &a, w.private, Permission::Share).await, "ok");
    assert_eq!(outcome(&w, &a, w.plan, Role::View).await, "404");
    // The agent of guest_view with a wide scope never gets more than view.
    let a = agent_ctx(&w, w.guest_view, None, Role::Manage);
    assert_eq!(outcome(&w, &a, w.plan, Role::View).await, "ok");
    assert_eq!(outcome(&w, &a, w.plan, Role::Edit).await, "403");
    assert_eq!(outcome(&w, &a, w.private, Role::View).await, "404");
    // max_role edit: shares only where the user shares and editors_can_share is true.
    let a = agent_ctx(&w, w.member, None, Role::Edit);
    assert_eq!(outcome(&w, &a, w.projects, Permission::Share).await, "ok");
    assert_eq!(outcome(&w, &a, w.archive, Permission::Share).await, "scope");
    let a = agent_ctx(&w, w.guest_edit, None, Role::Edit);
    assert_eq!(outcome(&w, &a, w.archive, Permission::Share).await, "403");
    // An expired scope sees nothing.
    let mut a = agent_ctx(&w, w.member, None, Role::Manage);
    a.scope.as_mut().unwrap().expires_at = Some(crate::now() - time::Duration::minutes(1));
    assert_eq!(outcome(&w, &a, w.plan, Role::View).await, "404");
}

#[tokio::test]
async fn share_links_read_and_never_share() {
    let w = world(true).await;
    let l = link_ctx(&w);
    assert_eq!(subject_of(&l).object.object_type, "share_link");
    assert_eq!(outcome(&w, &l, w.plan, Role::View).await, "ok");
    assert_eq!(outcome(&w, &l, w.plan, Role::Edit).await, "403");
    assert_eq!(outcome(&w, &l, w.private, Role::View).await, "404");
    assert_eq!(outcome(&w, &l, w.projects, Permission::Share).await, "403");
}

#[tokio::test]
async fn roles_and_settings_for_node_responses() {
    let w = world(true).await;
    let a = agent_ctx(&w, w.member, Some(w.projects), Role::View);
    assert_eq!(
        role_on(&w.authz, &w.nodes, &a, w.plan).await.unwrap(),
        Some(Role::View)
    );
    assert_eq!(role_on(&w.authz, &w.nodes, &a, w.root).await.unwrap(), None);
    let ge = user_ctx(&w, w.guest_edit);
    assert_eq!(
        role_on(&w.authz, &w.nodes, &ge, w.archive).await.unwrap(),
        Some(Role::Edit)
    );
    assert!(
        can_share(&w.authz, &w.nodes, &ge, w.projects)
            .await
            .unwrap()
    );
    assert!(!can_share(&w.authz, &w.nodes, &ge, w.archive).await.unwrap());
    assert!(
        editors_can_share_effective(&w.authz, w.projects)
            .await
            .unwrap()
    );
    assert!(
        !editors_can_share_effective(&w.authz, w.archive)
            .await
            .unwrap()
    );
}

#[test]
fn effective_role_is_the_intersection() {
    let now = crate::now();
    let scope = |max_role, expires_at| Scope {
        subtree: None,
        max_role,
        expires_at,
    };
    assert_eq!(
        effective_role(Some(Role::Edit), None, false, now),
        Some(Role::Edit)
    );
    assert_eq!(effective_role(None, None, true, now), None);
    let s = scope(Role::View, None);
    assert_eq!(
        effective_role(Some(Role::Manage), Some(&s), true, now),
        Some(Role::View)
    );
    assert_eq!(
        effective_role(Some(Role::Manage), Some(&s), false, now),
        None
    );
    let s = scope(Role::Manage, None);
    assert_eq!(
        effective_role(Some(Role::Comment), Some(&s), true, now),
        Some(Role::Comment)
    );
    let s = scope(Role::Manage, Some(now - time::Duration::seconds(1)));
    assert_eq!(effective_role(Some(Role::Edit), Some(&s), true, now), None);
    let s = scope(Role::Manage, Some(now + time::Duration::hours(1)));
    assert_eq!(
        effective_role(Some(Role::Edit), Some(&s), true, now),
        Some(Role::Edit)
    );
}

#[test]
fn subject_is_the_user_also_for_an_agent() {
    let user = UserId::new();
    let ctx = Context {
        tenant: TenantId::new(),
        actor: Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::Agent,
                id: PrincipalId(Uuid::now_v7()),
            },
            on_behalf_of: Some(user),
        },
        scope: None,
        idempotency_key: None,
    };
    assert_eq!(subject_of(&ctx), SubjectRef::user(user));
}

#[test]
fn relationships_round_trip_as_text() {
    for s in [
        "node:a#viewer@user:b",
        "node:r#manager@tenant:t#administer",
        "node:s#editors_share_off@user:*",
    ] {
        let r: Relationship = s.parse().unwrap();
        assert_eq!(r.to_string(), s);
    }
    assert!("node:a@user:b".parse::<Relationship>().is_err());
}
