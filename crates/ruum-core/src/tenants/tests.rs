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

/// The invitation token of an invitation mail's accept link.
fn mailed_token(body: &str) -> String {
    let at = body.find("token=").expect("an accept link") + "token=".len();
    body[at..]
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
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
    a.deliver().await;
    let mails = a.mailer.sent();
    let mail = mails.last().unwrap();
    assert_eq!(mail.to, "new@example.test");
    assert!(mail.body.contains("Welcome"));
    // The mail names the role and the end date (decision 0003-3).
    assert!(mail.body.contains("as a member"), "{}", mail.body);
    assert!(
        mail.body
            .contains(&crate::tenants::mail::date_words(inv.expires_at))
    );
    // The mail's token: the in-memory store mails the returned token; the Postgres worker
    // mints its own at the send, and both work until the invitation is accepted.
    let mailed = mailed_token(&mail.body);
    if a.pool.is_none() {
        assert_eq!(mailed, token);
    } else {
        assert_ne!(mailed, token);
    }
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
    // The accept ended every token of the invitation, also the mailed one.
    assert_eq!(
        code(
            a.tenants
                .accept_invitation(&Actor::user(t.guest.id), &mailed)
                .await
        ),
        ErrorCode::Conflict
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
    // A change of the content role alone moves the date too (decision 0003-1), and only a
    // member with `manage` manages the team tree (`manager_since`).
    let oc = ctx(tid, t.owner.id);
    assert_eq!(
        a.tenants.manager_since(tid, t.member.id).await.unwrap(),
        Some(member)
    );
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    a.tenants
        .update_member(&oc, t.member.id, MemberRole::Member, Some(Role::View))
        .await
        .unwrap();
    let viewer = ms(t.member.id).await.expect("still a member");
    assert!(viewer > member, "{viewer} vs {member}");
    assert_eq!(
        a.tenants.manager_since(tid, t.member.id).await.unwrap(),
        None
    );
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    a.tenants
        .update_member(&oc, t.member.id, MemberRole::Member, Some(Role::Manage))
        .await
        .unwrap();
    let manager = a
        .tenants
        .manager_since(tid, t.member.id)
        .await
        .unwrap()
        .expect("manages again");
    assert!(manager > viewer, "{manager} vs {viewer}");
    assert_eq!(ms(t.member.id).await, Some(manager));
    // Owners and admins manage; guests and strangers do not.
    assert!(
        a.tenants
            .manager_since(tid, t.owner.id)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        a.tenants.manager_since(tid, UserId::new()).await.unwrap(),
        None
    );
}

/// The members of `tenant` with their content roles, as `(user, role, content_role)`.
async fn content_roles(a: &Area, oc: &Context) -> Vec<(UserId, MemberRole, Option<Role>)> {
    a.tenants
        .list_members(oc, PageRequest::default())
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|m| (m.user_id, m.role, m.content_role))
        .collect()
}

/// The `tenant#...@user:<u>` relations of one user, sorted.
async fn user_tuples(a: &Area, tenant: TenantId, u: UserId) -> Vec<String> {
    let suffix = format!("@user:{u}");
    a.tenant_tuples(tenant)
        .await
        .into_iter()
        .filter(|t| t.ends_with(&suffix))
        .map(|t| t.trim_end_matches(&suffix).to_string())
        .collect()
}

/// Decision 0003-1, the storage invariant: `content_role` is set if and only if the role is
/// `member`, and the tuples follow the rebuild mapping of `schema.zed` after every transition
/// (member(view), admin, member (default), guest, member(edit), removed).
async fn content_roles_keep_the_invariant(a: Area) {
    let t = team(&a).await;
    let tid = t.tenant.id;
    let oc = ctx(tid, t.owner.id);
    let u = t.member.id;
    // The member who joined without a content role has the default.
    let m = content_roles(&a, &oc).await;
    assert!(
        m.contains(&(u, MemberRole::Member, Some(Role::Manage))),
        "{m:?}"
    );
    assert!(m.contains(&(t.owner.id, MemberRole::Owner, None)), "{m:?}");
    assert!(m.contains(&(t.guest.id, MemberRole::Guest, None)), "{m:?}");
    assert_eq!(user_tuples(&a, tid, u).await, vec!["member"]);

    // (new role, requested content role, content role after, the user's tuples after)
    type Step = (
        MemberRole,
        Option<Role>,
        Option<Role>,
        &'static [&'static str],
    );
    let steps: [Step; 5] = [
        (
            MemberRole::Member,
            Some(Role::View),
            Some(Role::View),
            &["content_viewer", "member"],
        ),
        (MemberRole::Admin, None, None, &["admin", "member"]),
        (MemberRole::Member, None, Some(Role::Manage), &["member"]),
        (MemberRole::Guest, None, None, &["guest"]),
        (
            MemberRole::Member,
            Some(Role::Edit),
            Some(Role::Edit),
            &["content_editor", "member"],
        ),
    ];
    for (role, requested, want, tuples) in steps {
        let m = a
            .tenants
            .update_member(&oc, u, role, requested)
            .await
            .unwrap();
        assert_eq!(
            (m.role, m.content_role),
            (role, want),
            "{role:?} {requested:?}"
        );
        assert_eq!(
            user_tuples(&a, tid, u).await,
            tuples.to_vec(),
            "{role:?} {requested:?}"
        );
        assert_eq!(
            a.tenants.access_of(tid, u).await.unwrap(),
            Some((role, want))
        );
    }
    // A member who stays a member keeps the content role when none is given.
    let m = a
        .tenants
        .set_member_role(&oc, u, MemberRole::Member)
        .await
        .unwrap();
    assert_eq!(m.content_role, Some(Role::Edit));
    // A content role for another role is refused, and nothing changes.
    for role in [MemberRole::Admin, MemberRole::Guest] {
        assert_eq!(
            code(
                a.tenants
                    .update_member(&oc, u, role, Some(Role::View))
                    .await
            ),
            ErrorCode::ValidationFailed
        );
    }
    assert_eq!(
        a.tenants.access_of(tid, u).await.unwrap(),
        Some((MemberRole::Member, Some(Role::Edit)))
    );
    // Removal deletes the membership and the content tuple.
    a.tenants.remove_member(&oc, u).await.unwrap();
    assert!(user_tuples(&a, tid, u).await.is_empty());
    assert_eq!(a.tenants.access_of(tid, u).await.unwrap(), None);

    // Invitations carry the content role of members only.
    assert_eq!(
        code(
            a.tenants
                .invite_as(
                    &oc,
                    "admin@example.test".into(),
                    MemberRole::Admin,
                    Some(Role::View),
                    None,
                )
                .await
        ),
        ErrorCode::ValidationFailed
    );
    let (inv, _) = a
        .tenants
        .invite_as(
            &oc,
            "viewer@example.test".into(),
            MemberRole::Member,
            Some(Role::Comment),
            None,
        )
        .await
        .unwrap();
    assert_eq!(inv.content_role, Some(Role::Comment));
    let (inv, _) = a
        .tenants
        .invite(&oc, "plain@example.test".into(), MemberRole::Member, None)
        .await
        .unwrap();
    assert_eq!(inv.content_role, Some(Role::Manage));
    let (inv, _) = a
        .tenants
        .invite(&oc, "guest2@example.test".into(), MemberRole::Guest, None)
        .await
        .unwrap();
    assert_eq!(inv.content_role, None);
    let listed = a
        .tenants
        .list_invitations(&oc, PageRequest::default())
        .await
        .unwrap();
    assert!(
        listed
            .items
            .iter()
            .any(|i| i.email == "viewer@example.test" && i.content_role == Some(Role::Comment))
    );

    // The database checks the invariant itself.
    if let Some(pool) = &a.pool {
        let mut c = system(pool).await;
        let bad = sqlx::query(
            "update memberships set content_role = 'manage' where tenant_id = $1 and user_id = $2",
        )
        .bind(tid)
        .bind(t.guest.id)
        .execute(&mut *c)
        .await;
        assert!(bad.is_err(), "a guest with a content role");
        let bad = sqlx::query(
            "update memberships set content_role = null where tenant_id = $1 and role = 'owner'",
        )
        .bind(tid)
        .execute(&mut *c)
        .await;
        assert!(bad.is_ok(), "owners have none already");
        let stranger = user(&a, "stranger").await;
        let bad = sqlx::query(
            "insert into memberships (tenant_id, user_id, role, created_kind, created_by) \
             values ($1, $2, 'member', 'user', $2)",
        )
        .bind(tid)
        .bind(stranger.id)
        .execute(&mut *c)
        .await;
        assert!(bad.is_err(), "a member without a content role");
        let bad = sqlx::query(
            "update invitations set content_role = 'view' where tenant_id = $1 and role = 'guest'",
        )
        .bind(tid)
        .execute(&mut *c)
        .await;
        assert!(bad.is_err(), "a guest invitation with a content role");
    }
}

/// Decision 0003-1, review 1 #2: accepting an invitation writes the content role and its tuple.
/// A new person, or a guest who becomes a member, gets the invitation's content role; a member
/// keeps the higher of the two; an admin stays at none.
async fn accepting_an_invitation_sets_the_content_role(a: Area) {
    let t = team(&a).await;
    let tid = t.tenant.id;
    let oc = ctx(tid, t.owner.id);
    let accept = |email: String, role: MemberRole, content: Option<Role>, who: UserId| {
        let (a, oc) = (&a, oc.clone());
        async move {
            let (_inv, token) = a
                .tenants
                .invite_as(&oc, email, role, content, None)
                .await
                .unwrap();
            a.tenants
                .accept_invitation(&Actor::user(who), &token)
                .await
                .unwrap()
        }
    };
    // A new person gets the invitation's content role and its tuple.
    let newcomer = user(&a, "newcomer").await;
    let m = accept(
        newcomer.email.clone(),
        MemberRole::Member,
        Some(Role::View),
        newcomer.id,
    )
    .await;
    assert_eq!(
        (m.role, m.content_role),
        (MemberRole::Member, Some(Role::View))
    );
    assert_eq!(
        user_tuples(&a, tid, newcomer.id).await,
        vec!["content_viewer", "member"]
    );
    // A guest who becomes a member gets it too.
    let m = accept(
        t.guest.email.clone(),
        MemberRole::Member,
        Some(Role::Edit),
        t.guest.id,
    )
    .await;
    assert_eq!(
        (m.role, m.content_role),
        (MemberRole::Member, Some(Role::Edit))
    );
    assert_eq!(
        user_tuples(&a, tid, t.guest.id).await,
        vec!["content_editor", "member"]
    );
    // A member keeps the higher of the two: never lowered, raised by a higher invitation.
    let m = accept(
        t.member.email.clone(),
        MemberRole::Member,
        Some(Role::View),
        t.member.id,
    )
    .await;
    assert_eq!(
        (m.role, m.content_role),
        (MemberRole::Member, Some(Role::Manage))
    );
    assert_eq!(user_tuples(&a, tid, t.member.id).await, vec!["member"]);
    let m = accept(
        newcomer.email.clone(),
        MemberRole::Member,
        Some(Role::Comment),
        newcomer.id,
    )
    .await;
    assert_eq!(m.content_role, Some(Role::Comment));
    assert_eq!(
        user_tuples(&a, tid, newcomer.id).await,
        vec!["content_commenter", "member"]
    );
    // A member who accepts a guest invitation keeps the content role.
    let m = accept(newcomer.email.clone(), MemberRole::Guest, None, newcomer.id).await;
    assert_eq!(
        (m.role, m.content_role),
        (MemberRole::Member, Some(Role::Comment))
    );
    // An admin stays an admin without a content role.
    a.tenants
        .update_member(&oc, t.member.id, MemberRole::Admin, None)
        .await
        .unwrap();
    let m = accept(
        t.member.email.clone(),
        MemberRole::Member,
        Some(Role::View),
        t.member.id,
    )
    .await;
    assert_eq!((m.role, m.content_role), (MemberRole::Admin, None));
    assert_eq!(
        user_tuples(&a, tid, t.member.id).await,
        vec!["admin", "member"]
    );
}

/// Names for answers (C-7, decision 0003-5): members, guests and agents of the tenant by their
/// current name; a person who left by the name the tenant kept; nobody of another tenant.
async fn principal_names_follow_the_tenant(a: Area) {
    let t = team(&a).await;
    let tid = t.tenant.id;
    let mc = ctx(tid, t.member.id);
    let agent = a
        .agents
        .register(
            &mc,
            NewAgent {
                name: "Helper".into(),
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
    // Carol has her own tenant with an agent; neither is named in Acme.
    let carol = user(&a, "carol").await;
    let carol_tenant = a.tenants.list_for_user(carol.id).await.unwrap()[0].0.id;
    let carol_agent = a
        .agents
        .register(
            &ctx(carol_tenant, carol.id),
            NewAgent {
                name: "Carol bot".into(),
                description: None,
                scope: AgentScope {
                    subtree: None,
                    max_role: Role::View,
                    expires_at: None,
                    budget: Budget::default(),
                },
            },
        )
        .await
        .unwrap();
    let unknown = Uuid::now_v7();
    let ids = [
        t.owner.id.0,
        t.member.id.0,
        t.guest.id.0,
        agent.id.0,
        carol.id.0,
        carol_agent.id.0,
        unknown,
    ];
    let names = a.tenants.principal_names(tid, &ids).await.unwrap();
    assert_eq!(names.len(), 4, "{names:?}");
    let m = &names[&t.member.id.0];
    assert_eq!(m.display_name, t.member.display_name);
    assert_eq!(m.email.as_deref(), Some(t.member.email.as_str()));
    assert_eq!(names[&t.guest.id.0].display_name, t.guest.display_name);
    let ag = &names[&agent.id.0];
    assert_eq!(ag.display_name, "Helper");
    assert_eq!(ag.owner, Some(t.member.id));
    assert_eq!(ag.email, None);
    assert!(!names.contains_key(&carol.id.0));
    assert!(!names.contains_key(&carol_agent.id.0));
    // The guest leaves: the name stays for Acme, without the email.
    a.tenants
        .remove_member(&ctx(tid, t.guest.id), t.guest.id)
        .await
        .unwrap();
    let names = a
        .tenants
        .principal_names(tid, &[t.guest.id.0])
        .await
        .unwrap();
    let g = &names[&t.guest.id.0];
    assert_eq!(g.display_name, t.guest.display_name);
    assert_eq!(g.email, None);
    // The member is removed; the agent keeps its name (agents stay in the tenant).
    a.tenants
        .remove_member(&ctx(tid, t.owner.id), t.member.id)
        .await
        .unwrap();
    let names = a
        .tenants
        .principal_names(tid, &[t.member.id.0, agent.id.0])
        .await
        .unwrap();
    assert_eq!(names[&t.member.id.0].display_name, t.member.display_name);
    assert_eq!(names[&agent.id.0].display_name, "Helper");
    // The snapshot is per tenant: Carol's tenant never learns the names of Acme.
    let names = a
        .tenants
        .principal_names(carol_tenant, &[t.member.id.0, t.guest.id.0, agent.id.0])
        .await
        .unwrap();
    assert!(names.is_empty(), "{names:?}");
}

crate::conformance!(
    principal_names_follow_the_tenant,
    a_team_tenant_has_its_owner_and_root,
    members_and_roles_follow_the_rules,
    removing_a_member_cleans_up,
    invitations_bind_by_token,
    guests_follow_the_policy,
    usage_and_limits,
    member_since_dates_the_membership,
    content_roles_keep_the_invariant,
    accepting_an_invitation_sets_the_content_role,
);

/// Email grants that bind (decision 0003-2) and invitation mails through the outbox (decision
/// 0003-3), on the Postgres stores. The in-memory flow runs through the API tests
/// (`ruum-api`, `routes::auth::tests`).
mod email_grants {
    use super::*;
    use crate::identity::tests::{login, postgres};

    async fn system(pool: &sqlx::PgPool) -> sqlx::Transaction<'static, sqlx::Postgres> {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("set local ruum.system = 'on'")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx
    }

    /// An email grant on the team root, as `addGrant` stores it for an outsider.
    async fn email_grant(pool: &sqlx::PgPool, t: &Team, email: &str, role: &str) -> Uuid {
        let id = Uuid::now_v7();
        let mut tx = system(pool).await;
        sqlx::query(
            "insert into grants (id, tenant_id, node_id, grantee_kind, grantee_email, role, \
             created_kind, created_by) values ($1, $2, $3, 'email', $4, $5::grant_role, 'user', $6)",
        )
        .bind(id)
        .bind(t.tenant.id)
        .bind(t.tenant.root_node_id)
        .bind(email)
        .bind(role)
        .bind(t.owner.id)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        id
    }

    async fn grantee_kind(pool: &sqlx::PgPool, grant: Uuid) -> Option<String> {
        let mut tx = system(pool).await;
        sqlx::query_scalar("select grantee_kind::text from grants where id = $1")
            .bind(grant)
            .fetch_optional(&mut *tx)
            .await
            .unwrap()
    }

    /// A web session binds the email grants of the user's verified email and makes the user a
    /// guest (`member.added` with `via: email_grant`); the check runs at most once a minute
    /// unless forced; another user's session binds nothing.
    #[sqlx::test(migrations = "../../migrations")]
    async fn a_web_session_binds_email_grants_and_makes_a_guest(pool: sqlx::PgPool) {
        let a = postgres(pool.clone());
        let t = team(&a).await;
        let stranger = user(&a, "stranger").await;
        let grant = email_grant(&pool, &t, &stranger.email.to_uppercase(), "view").await;
        let (session, _) = a.identity.create_session(stranger.id, None).await.unwrap();
        // Another user's session, or no session of this user, binds nothing.
        let (other, _) = a.identity.create_session(t.member.id, None).await.unwrap();
        assert_eq!(
            a.identity
                .bind_email_grants(stranger.id, other.id, true)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            a.identity
                .bind_email_grants(stranger.id, session.id, false)
                .await
                .unwrap(),
            1
        );
        assert_eq!(grantee_kind(&pool, grant).await.as_deref(), Some("user"));
        assert_eq!(
            a.tenants.role_of(t.tenant.id, stranger.id).await.unwrap(),
            Some(MemberRole::Guest)
        );
        let mut tx = system(&pool).await;
        let data: serde_json::Value = sqlx::query_scalar(
            "select data from audit where tenant_id = $1 and action = 'member.added' \
             and target_id = $2",
        )
        .bind(t.tenant.id)
        .bind(stranger.id.to_string())
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        drop(tx);
        assert_eq!(data["via"], "email_grant");
        assert_eq!(data["role"], "guest");
        assert_eq!(data["grant_ids"][0], grant.to_string());
        assert_eq!(data["granted_by"][0]["id"], t.owner.id.to_string());

        // Within the minute a new grant waits, unless the check is forced.
        let second = email_grant(&pool, &t, &stranger.email, "edit").await;
        assert_eq!(
            a.identity
                .bind_email_grants(stranger.id, session.id, false)
                .await
                .unwrap(),
            0
        );
        assert_eq!(grantee_kind(&pool, second).await.as_deref(), Some("email"));
        assert_eq!(
            a.identity
                .bind_email_grants(stranger.id, session.id, true)
                .await
                .unwrap(),
            1
        );
        // The user already had a grant on the node: it is raised to edit, the email grant goes.
        assert_eq!(grantee_kind(&pool, second).await, None);
        // An ended session binds nothing.
        a.identity
            .end_session(stranger.id, session.id)
            .await
            .unwrap();
        email_grant(&pool, &t, &stranger.email, "manage").await;
        assert_eq!(
            a.identity
                .bind_email_grants(stranger.id, session.id, true)
                .await
                .unwrap(),
            0
        );
    }

    /// An unverified email never binds; a tenant that does not allow guests keeps its email
    /// grants and makes no guest (rule (c)).
    #[sqlx::test(migrations = "../../migrations")]
    async fn unverified_emails_and_closed_tenants_bind_nothing(pool: sqlx::PgPool) {
        let a = postgres(pool.clone());
        let t = team(&a).await;
        let mut l = login(&format!(
            "unverified-{}@example.test",
            Uuid::now_v7().simple()
        ));
        l.email_verified = false;
        let unverified = a.identity.upsert_from_login(&l).await.unwrap();
        assert!(!unverified.email_verified);
        let grant = email_grant(&pool, &t, &unverified.email, "view").await;
        let (session, _) = a
            .identity
            .create_session(unverified.id, None)
            .await
            .unwrap();
        assert_eq!(
            a.identity
                .bind_email_grants(unverified.id, session.id, true)
                .await
                .unwrap(),
            0
        );
        assert_eq!(grantee_kind(&pool, grant).await.as_deref(), Some("email"));

        let stranger = user(&a, "closed").await;
        let grant = email_grant(&pool, &t, &stranger.email, "view").await;
        a.tenants
            .update(
                &ctx(t.tenant.id, t.owner.id),
                None,
                Some(TenantPolicyChange {
                    allow_guests: Some(false),
                    ..TenantPolicyChange::default()
                }),
            )
            .await
            .unwrap();
        let (session, _) = a.identity.create_session(stranger.id, None).await.unwrap();
        assert_eq!(
            a.identity
                .bind_email_grants(stranger.id, session.id, true)
                .await
                .unwrap(),
            0
        );
        assert_eq!(grantee_kind(&pool, grant).await.as_deref(), Some("email"));
        assert_eq!(
            a.tenants.role_of(t.tenant.id, stranger.id).await.unwrap(),
            None
        );
    }

    /// The outbox row of an invitation holds no token; a revoke before the send means no mail
    /// and no new token (reviewer 1 #3 (b)).
    #[sqlx::test(migrations = "../../migrations")]
    async fn a_revoked_invitation_sends_no_mail_and_mints_no_token(pool: sqlx::PgPool) {
        let a = postgres(pool.clone());
        let t = team(&a).await;
        let oc = ctx(t.tenant.id, t.owner.id);
        a.deliver().await;
        let before = a.mailer.sent().len();
        let (inv, token) = a
            .tenants
            .invite(&oc, "late@example.test".into(), MemberRole::Member, None)
            .await
            .unwrap();
        let mut tx = system(&pool).await;
        let payload: serde_json::Value =
            sqlx::query_scalar("select payload from mail_outbox where invitation_id = $1")
                .bind(inv.id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        drop(tx);
        assert!(!payload.to_string().contains(INVITATION_PREFIX));
        assert!(!payload.to_string().contains(&token));
        a.tenants.revoke_invitation(&oc, inv.id).await.unwrap();
        let round = a.deliver().await;
        assert_eq!(round.dropped, 1);
        assert_eq!(round.sent, 0);
        assert_eq!(a.mailer.sent().len(), before);
        let mut tx = system(&pool).await;
        let tokens: i64 =
            sqlx::query_scalar("select count(*) from invitation_tokens where invitation_id = $1")
                .bind(inv.id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(tokens, 1, "only the token of the invitation itself");
    }

    /// The tokens of invitations that ended more than 7 days ago are deleted; the invitation
    /// row keeps its state, and the tokens of a pending invitation stay.
    #[sqlx::test(migrations = "../../migrations")]
    async fn tokens_of_ended_invitations_are_cleaned(pool: sqlx::PgPool) {
        let a = postgres(pool.clone());
        let t = team(&a).await;
        let oc = ctx(t.tenant.id, t.owner.id);
        let (pending, _) = a
            .tenants
            .invite(&oc, "pending@example.test".into(), MemberRole::Member, None)
            .await
            .unwrap();
        let mut tx = system(&pool).await;
        sqlx::query(
            "update invitations set accepted_at = accepted_at - interval '8 days' \
             where tenant_id = $1 and accepted_at is not null",
        )
        .bind(t.tenant.id)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let cleaned = crate::mailing::outbox::clean(&pool).await.unwrap();
        assert_eq!(cleaned.invitation_tokens, 2, "the member's and the guest's");
        let mut tx = system(&pool).await;
        let left: Vec<Uuid> = sqlx::query_scalar("select invitation_id from invitation_tokens")
            .fetch_all(&mut *tx)
            .await
            .unwrap();
        assert_eq!(left, vec![pending.id.0]);
        let accepted: i64 = sqlx::query_scalar(
            "select count(*) from invitations where tenant_id = $1 and accepted_at is not null",
        )
        .bind(t.tenant.id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(accepted, 2);
    }
}
