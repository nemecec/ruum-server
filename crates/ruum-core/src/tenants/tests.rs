//! The conformance suite of `TenantStore` (in memory and Postgres).

use super::*;
use crate::actor::{PrincipalKind, PrincipalRef};
use crate::agents::{AgentScope, Budget, DisabledReason, NewAgent};
use crate::authz::Role;
use crate::error::ErrorCode;
use crate::identity::tests::{Area, code, ctx, system, user};
use crate::ids::PrincipalId;
use uuid::Uuid;

/// A team tenant of a new owner, with a member and a guest who joined through invitations.
struct Team {
    tenant: Tenant,
    owner: crate::identity::User,
    member: crate::identity::User,
    guest: crate::identity::User,
}

async fn team(a: &Area) -> Team {
    let owner = user(a, "owner").await;
    let tenant = a
        .tenants
        .create_team(&Actor::user(owner.id), "Acme".into())
        .await
        .unwrap();
    let member = user(a, "member").await;
    let guest = user(a, "guest").await;
    let oc = ctx(tenant.id, owner.id);
    for (u, role) in [(&member, MemberRole::Member), (&guest, MemberRole::Guest)] {
        let (_inv, token) = a
            .tenants
            .invite(&oc, u.email.clone(), role, None)
            .await
            .unwrap();
        let m = a
            .tenants
            .accept_invitation(&Actor::user(u.id), &token)
            .await
            .unwrap();
        assert_eq!(m.role, role);
    }
    Team {
        tenant,
        owner,
        member,
        guest,
    }
}

fn agent_actor(owner: UserId) -> Actor {
    Actor {
        actor: PrincipalRef {
            kind: PrincipalKind::Agent,
            id: PrincipalId(Uuid::now_v7()),
        },
        on_behalf_of: Some(owner),
    }
}

async fn a_team_tenant_has_its_owner_and_root(a: Area) {
    let t = team(&a).await;
    assert_eq!(t.tenant.kind, TenantKind::Team);
    assert_eq!(t.tenant.plan, Plan::Team);
    assert_eq!(t.tenant.policy, TenantPolicy::default());
    let oc = ctx(t.tenant.id, t.owner.id);
    assert_eq!(a.tenants.get(&oc).await.unwrap().id, t.tenant.id);
    assert_eq!(a.tenants.find(t.tenant.id).await.unwrap().name, "Acme");
    assert_eq!(
        a.tenants.role_of(t.tenant.id, t.owner.id).await.unwrap(),
        Some(MemberRole::Owner)
    );
    // The guest sees the tenant; a stranger does not.
    assert!(a.tenants.get(&ctx(t.tenant.id, t.guest.id)).await.is_ok());
    let stranger = user(&a, "stranger").await;
    assert_eq!(
        code(a.tenants.get(&ctx(t.tenant.id, stranger.id)).await),
        ErrorCode::NotFound
    );
    let list = a.tenants.list_for_user(t.member.id).await.unwrap();
    assert_eq!(list.len(), 2, "the personal tenant and the team");
    // Agents cannot make tenants.
    assert_eq!(
        code(
            a.tenants
                .create_team(&agent_actor(t.owner.id), "x".into())
                .await
        ),
        ErrorCode::UserRequired
    );
    assert_eq!(
        code(
            a.tenants
                .create_team(&Actor::user(t.owner.id), " ".into())
                .await
        ),
        ErrorCode::ValidationFailed
    );
    if let Some(pool) = &a.pool {
        let mut c = system(pool).await;
        let (root_name, counter): (String, i64) = sqlx::query_as(
            "select n.name, j.last_seq from nodes n join journal_counters j \
             on j.tenant_id = n.tenant_id where n.id = $1",
        )
        .bind(t.tenant.root_node_id)
        .fetch_one(&mut *c)
        .await
        .unwrap();
        assert_eq!(root_name, crate::identity::postgres::ROOT_NAME);
        assert_eq!(counter, 0);
        let updates: Vec<serde_json::Value> =
            sqlx::query_scalar("select updates from authz_outbox where tenant_id = $1 order by id")
                .bind(t.tenant.id)
                .fetch_all(&mut *c)
                .await
                .unwrap();
        let all = serde_json::to_string(&updates).unwrap();
        assert!(all.contains("\"team\""), "{all}");
        assert!(all.contains("administer"), "{all}");
        assert!(all.contains("members_can_share"), "{all}");
        assert!(all.contains("\"guest\""), "{all}");
    }
}

async fn members_and_roles_follow_the_rules(a: Area) {
    let t = team(&a).await;
    let (tid, oc) = (t.tenant.id, ctx(t.tenant.id, t.owner.id));
    let mc = ctx(tid, t.member.id);
    let gc = ctx(tid, t.guest.id);
    let page = a
        .tenants
        .list_members(&oc, PageRequest::default())
        .await
        .unwrap();
    assert_eq!(page.items.len(), 3);
    assert_eq!(page.items[0].user_id, t.owner.id);
    let first = a
        .tenants
        .list_members(
            &oc,
            PageRequest {
                cursor: None,
                limit: Some(2),
            },
        )
        .await
        .unwrap();
    assert_eq!(first.items.len(), 2);
    let rest = a
        .tenants
        .list_members(
            &oc,
            PageRequest {
                cursor: first.next_cursor,
                limit: Some(2),
            },
        )
        .await
        .unwrap();
    assert_eq!(rest.items.len(), 1);
    assert!(rest.next_cursor.is_none());
    // Guests do not list members.
    assert_eq!(
        code(a.tenants.list_members(&gc, PageRequest::default()).await),
        ErrorCode::PermissionDenied
    );
    // Members do not change roles.
    assert_eq!(
        code(
            a.tenants
                .set_member_role(&mc, t.guest.id, MemberRole::Member)
                .await
        ),
        ErrorCode::PermissionDenied
    );
    // The owner makes the member an admin; an admin cannot make owners.
    let m = a
        .tenants
        .set_member_role(&oc, t.member.id, MemberRole::Admin)
        .await
        .unwrap();
    assert_eq!(m.role, MemberRole::Admin);
    assert_eq!(
        code(
            a.tenants
                .set_member_role(&mc, t.guest.id, MemberRole::Owner)
                .await
        ),
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        a.tenants
            .set_member_role(&mc, t.guest.id, MemberRole::Member)
            .await
            .unwrap()
            .role,
        MemberRole::Member
    );
    // The last owner stays.
    assert_eq!(
        code(
            a.tenants
                .set_member_role(&oc, t.owner.id, MemberRole::Admin)
                .await
        ),
        ErrorCode::Conflict
    );
    assert_eq!(
        code(a.tenants.remove_member(&oc, t.owner.id).await),
        ErrorCode::Conflict
    );
    assert_eq!(
        code(
            a.tenants
                .set_member_role(&oc, UserId::new(), MemberRole::Admin)
                .await
        ),
        ErrorCode::NotFound
    );
    // Agents cannot administer.
    let mut agent = oc.clone();
    agent.actor = agent_actor(t.owner.id);
    assert_eq!(
        code(
            a.tenants
                .set_member_role(&agent, t.guest.id, MemberRole::Guest)
                .await
        ),
        ErrorCode::UserRequired
    );
}

async fn removing_a_member_cleans_up(a: Area) {
    let t = team(&a).await;
    let (tid, oc) = (t.tenant.id, ctx(t.tenant.id, t.owner.id));
    let mc = ctx(tid, t.member.id);
    // The member has an agent with a key and an app password.
    let agent = a
        .agents
        .register(
            &mc,
            NewAgent {
                name: "helper".into(),
                description: None,
                scope: AgentScope {
                    subtree: None,
                    max_role: Role::Edit,
                    expires_at: None,
                    budget: Budget::default(),
                },
            },
        )
        .await
        .unwrap();
    let (_key, secret) = a
        .agents
        .create_key(&mc, agent.id, None, None)
        .await
        .unwrap();
    let (_pw, pw_secret) = a
        .identity
        .create_app_password(&mc, "dav".into(), None)
        .await
        .unwrap();
    // A guest may leave; a member may not remove others.
    assert_eq!(
        code(a.tenants.remove_member(&mc, t.guest.id).await),
        ErrorCode::PermissionDenied
    );
    a.tenants
        .remove_member(&ctx(tid, t.guest.id), t.guest.id)
        .await
        .unwrap();
    assert_eq!(a.tenants.role_of(tid, t.guest.id).await.unwrap(), None);
    // The owner removes the member.
    a.tenants.remove_member(&oc, t.member.id).await.unwrap();
    assert_eq!(a.tenants.role_of(tid, t.member.id).await.unwrap(), None);
    let a2 = a.agents.authenticate(tid, agent.id).await;
    assert_eq!(code(a2), ErrorCode::AgentDisabled);
    let row = a.agents.get(&oc, agent.id).await.unwrap();
    assert_eq!(row.disabled_reason, Some(DisabledReason::OwnerLeft));
    assert_eq!(
        code(a.agents.exchange_key(&secret).await),
        ErrorCode::AgentDisabled
    );
    assert_eq!(
        code(
            a.identity
                .authenticate_app_password(&t.member.email, &pw_secret)
                .await
        ),
        ErrorCode::Unauthenticated
    );
    assert_eq!(
        code(a.tenants.remove_member(&oc, t.member.id).await),
        ErrorCode::NotFound
    );
}

async fn invitations_bind_by_token(a: Area) {
    let t = team(&a).await;
    let (tid, oc) = (t.tenant.id, ctx(t.tenant.id, t.owner.id));
    let (inv, token) = a
        .tenants
        .invite(
            &oc,
            "new@example.test".into(),
            MemberRole::Member,
            Some("Welcome".into()),
        )
        .await
        .unwrap();
    assert!(token.starts_with(INVITATION_PREFIX));
    assert_eq!(inv.invited_by, Actor::user(t.owner.id));
    let mails = a.mailer.sent();
    let mail = mails.last().unwrap();
    assert_eq!(mail.to, "new@example.test");
    assert!(mail.body.contains(&token));
    assert!(mail.body.contains("Welcome"));
    assert_eq!(
        code(
            a.tenants
                .invite(&oc, "NEW@example.test".into(), MemberRole::Guest, None)
                .await
        ),
        ErrorCode::Conflict
    );
    assert_eq!(
        code(
            a.tenants
                .invite(&oc, "x@example.test".into(), MemberRole::Owner, None)
                .await
        ),
        ErrorCode::ValidationFailed
    );
    assert_eq!(
        code(
            a.tenants
                .invite(&oc, "not-an-email".into(), MemberRole::Member, None)
                .await
        ),
        ErrorCode::ValidationFailed
    );
    assert_eq!(
        code(
            a.tenants
                .invite(
                    &ctx(tid, t.member.id),
                    "y@example.test".into(),
                    MemberRole::Member,
                    None
                )
                .await
        ),
        ErrorCode::PermissionDenied
    );
    let open = a
        .tenants
        .list_invitations(&oc, PageRequest::default())
        .await
        .unwrap();
    assert_eq!(open.items.len(), 1);
    // The token binds the invitation to whoever presents it.
    let newcomer = user(&a, "newcomer").await;
    let m = a
        .tenants
        .accept_invitation(&Actor::user(newcomer.id), &token)
        .await
        .unwrap();
    assert_eq!(m.role, MemberRole::Member);
    // Accepting again is the same membership; another user gets a conflict.
    assert!(
        a.tenants
            .accept_invitation(&Actor::user(newcomer.id), &token)
            .await
            .is_ok()
    );
    assert_eq!(
        code(
            a.tenants
                .accept_invitation(&Actor::user(t.guest.id), &token)
                .await
        ),
        ErrorCode::Conflict
    );
    assert_eq!(
        code(
            a.tenants
                .accept_invitation(&Actor::user(t.guest.id), "nope")
                .await
        ),
        ErrorCode::NotFound
    );
    assert!(
        a.tenants
            .list_invitations(&oc, PageRequest::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    // Revoke.
    let (inv2, token2) = a
        .tenants
        .invite(&oc, "later@example.test".into(), MemberRole::Guest, None)
        .await
        .unwrap();
    a.tenants.revoke_invitation(&oc, inv2.id).await.unwrap();
    assert_eq!(
        code(a.tenants.revoke_invitation(&oc, inv2.id).await),
        ErrorCode::NotFound
    );
    assert_eq!(
        code(
            a.tenants
                .accept_invitation(&Actor::user(newcomer.id), &token2)
                .await
        ),
        ErrorCode::NotFound
    );
    // A member who accepts a guest invitation keeps the higher role.
    let (_i, token3) = a
        .tenants
        .invite(&oc, t.member.email.clone(), MemberRole::Guest, None)
        .await
        .unwrap();
    let m = a
        .tenants
        .accept_invitation(&Actor::user(t.member.id), &token3)
        .await
        .unwrap();
    assert_eq!(m.role, MemberRole::Member);
}

async fn guests_follow_the_policy(a: Area) {
    let t = team(&a).await;
    let oc = ctx(t.tenant.id, t.owner.id);
    // A guest invitation sent while guests were allowed.
    let late = user(&a, "late-guest").await;
    let (_inv, late_token) = a
        .tenants
        .invite(&oc, late.email.clone(), MemberRole::Guest, None)
        .await
        .unwrap();
    let updated = a
        .tenants
        .update(
            &oc,
            Some("Acme Ltd".into()),
            Some(TenantPolicyChange {
                allow_guests: Some(false),
                members_can_share: Some(false),
                share_link_max_days: Some(Some(7)),
                ..TenantPolicyChange::default()
            }),
        )
        .await
        .unwrap();
    assert_eq!(updated.name, "Acme Ltd");
    assert!(!updated.policy.allow_guests);
    assert!(!updated.policy.members_can_share);
    assert_eq!(updated.policy.share_link_max_days, Some(7));
    assert_eq!(
        a.tenants.find(t.tenant.id).await.unwrap().policy,
        updated.policy
    );
    assert_eq!(
        code(
            a.tenants
                .invite(&oc, "g@example.test".into(), MemberRole::Guest, None)
                .await
        ),
        ErrorCode::PermissionDenied
    );
    // ... is refused on accept once the policy says no guests.
    assert_eq!(
        code(
            a.tenants
                .accept_invitation(&Actor::user(late.id), &late_token)
                .await
        ),
        ErrorCode::PermissionDenied
    );
    assert_eq!(a.tenants.role_of(t.tenant.id, late.id).await.unwrap(), None);
    assert_eq!(
        code(
            a.tenants
                .update(&ctx(t.tenant.id, t.member.id), Some("x".into()), None)
                .await
        ),
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        code(
            a.tenants
                .update(
                    &oc,
                    None,
                    Some(TenantPolicyChange {
                        share_link_max_days: Some(Some(0)),
                        ..TenantPolicyChange::default()
                    })
                )
                .await
        ),
        ErrorCode::ValidationFailed
    );
    if let Some(pool) = &a.pool {
        let last: serde_json::Value = sqlx::query_scalar(
            "select updates from authz_outbox where tenant_id = $1 order by id desc limit 1",
        )
        .bind(t.tenant.id)
        .fetch_one(&mut *system(pool).await)
        .await
        .unwrap();
        assert!(last.to_string().contains("Delete"), "{last}");
        assert!(last.to_string().contains("members_can_share"), "{last}");
    }
}

async fn usage_and_limits(a: Area) {
    let t = team(&a).await;
    let u = a
        .tenants
        .usage(&ctx(t.tenant.id, t.member.id))
        .await
        .unwrap();
    assert_eq!(u.members, 2, "owner and member; guests do not count");
    assert_eq!(u.agents, 0);
    assert_eq!(
        code(a.tenants.usage(&ctx(t.tenant.id, t.guest.id)).await),
        ErrorCode::PermissionDenied
    );
    let l = tenant_limits(Plan::Team, u.members);
    assert_eq!(l.agents, 20);
    // A personal tenant holds one member: a second member is over the limit.
    let solo = user(&a, "solo").await;
    let sc = ctx(solo.home_tenant_id, solo.id);
    assert_eq!(
        code(
            a.tenants
                .invite(&sc, "friend@example.test".into(), MemberRole::Member, None)
                .await
        ),
        ErrorCode::PlanLimitReached
    );
    assert!(
        a.tenants
            .invite(&sc, "friend@example.test".into(), MemberRole::Guest, None)
            .await
            .is_ok()
    );
}

/// `member_since` dates the membership for the changes feed (decision 0002-6): from the join
/// or the last role change; `None` for guests and strangers.
async fn member_since_dates_the_membership(a: Area) {
    let t = team(&a).await;
    let tid = t.tenant.id;
    let ms = |u: UserId| {
        let a = &a;
        async move { a.tenants.member_since(tid, u).await.unwrap() }
    };
    let owner = ms(t.owner.id).await.expect("the owner is a member");
    let member = ms(t.member.id).await.expect("a member");
    assert!(owner <= member, "the owner joined first");
    assert_eq!(ms(t.guest.id).await, None);
    assert_eq!(ms(UserId::new()).await, None);
    // A guest who becomes a member is one from then on. Postgres dates with its own clock.
    let before = crate::now() - time::Duration::seconds(5);
    a.tenants
        .set_member_role(&ctx(tid, t.owner.id), t.guest.id, MemberRole::Member)
        .await
        .unwrap();
    let promoted = ms(t.guest.id).await.expect("now a member");
    assert!(
        promoted >= before && promoted > member,
        "{promoted} vs {member}"
    );
}

crate::conformance!(
    a_team_tenant_has_its_owner_and_root,
    members_and_roles_follow_the_rules,
    removing_a_member_cleans_up,
    invitations_bind_by_token,
    guests_follow_the_policy,
    usage_and_limits,
    member_since_dates_the_membership,
);
