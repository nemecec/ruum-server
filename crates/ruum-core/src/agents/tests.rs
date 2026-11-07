//! The conformance suites of `AgentStore` and `OAuthServer` (in memory and Postgres).

use super::oauth::*;
use super::*;
use crate::actor::{Actor, PrincipalKind, PrincipalRef};
use crate::error::ErrorCode;
use crate::identity::secret::pkce_challenge;
use crate::identity::tests::{Area, code, ctx, user};
use crate::identity::{TokenType, User};
use crate::tenants::{MemberRole, Tenant};

const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const REDIRECT: &str = "http://127.0.0.1:33418/callback";

struct Team {
    tenant: Tenant,
    owner: User,
    member: User,
}

async fn team(a: &Area) -> Team {
    let owner = user(a, "owner").await;
    let tenant = a
        .tenants
        .create_team(&Actor::user(owner.id), "Agents Inc".into())
        .await
        .unwrap();
    let member = user(a, "member").await;
    let (_i, token) = a
        .tenants
        .invite(
            &ctx(tenant.id, owner.id),
            member.email.clone(),
            MemberRole::Member,
            None,
        )
        .await
        .unwrap();
    a.tenants
        .accept_invitation(&Actor::user(member.id), &token)
        .await
        .unwrap();
    Team {
        tenant,
        owner,
        member,
    }
}

fn scope(subtree: Option<NodeId>, max_role: Role) -> AgentScope {
    AgentScope {
        subtree,
        max_role,
        expires_at: None,
        budget: Budget::default(),
    }
}

fn new_agent(name: &str, subtree: Option<NodeId>) -> NewAgent {
    NewAgent {
        name: name.into(),
        description: Some("does things".into()),
        scope: scope(subtree, Role::Edit),
    }
}

/// The context of an agent acting for `owner`.
fn as_agent(tenant: TenantId, agent: &Agent) -> Context {
    Context {
        tenant,
        actor: Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::Agent,
                id: agent.id.into(),
            },
            on_behalf_of: Some(agent.owner_id),
        },
        scope: Some(agent.scope.as_scope()),
        idempotency_key: None,
    }
}

async fn agents_belong_to_their_owner(a: Area) {
    let t = team(&a).await;
    let (tid, oc, mc) = (
        t.tenant.id,
        ctx(t.tenant.id, t.owner.id),
        ctx(t.tenant.id, t.member.id),
    );
    let agent = a
        .agents
        .register(
            &mc,
            new_agent("garden builder", Some(t.tenant.root_node_id)),
        )
        .await
        .unwrap();
    assert_eq!(agent.owner_id, t.member.id);
    assert_eq!(agent.scope.subtree, Some(t.tenant.root_node_id));
    let mine = a
        .agents
        .register(&oc, new_agent("owner's", None))
        .await
        .unwrap();
    // The member sees its agent; the owner (an admin) sees all; the member does not see others.
    assert_eq!(
        a.agents
            .list(&mc, None, PageRequest::default())
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        a.agents
            .list(&oc, None, PageRequest::default())
            .await
            .unwrap()
            .items
            .len(),
        2
    );
    assert_eq!(
        a.agents
            .list(&oc, Some(t.member.id), PageRequest::default())
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(code(a.agents.get(&mc, mine.id).await), ErrorCode::NotFound);
    assert_eq!(a.agents.get(&oc, agent.id).await.unwrap().id, agent.id);
    // An agent sees itself in the list, but not its record.
    let ac = as_agent(tid, &agent);
    let listed = a
        .agents
        .list(&ac, None, PageRequest::default())
        .await
        .unwrap();
    assert_eq!(listed.items.len(), 1);
    assert_eq!(listed.items[0].id, agent.id);
    assert_eq!(
        code(a.agents.get(&ac, agent.id).await),
        ErrorCode::PermissionDenied
    );
    // Only the owner changes it; an agent never.
    let changed = a
        .agents
        .update(
            &mc,
            agent.id,
            AgentUpdate {
                name: Some("renamed".into()),
                description: Some(None),
                scope: Some(scope(None, Role::View)),
            },
        )
        .await
        .unwrap();
    assert_eq!(changed.name, "renamed");
    assert_eq!(changed.description, None);
    assert_eq!(changed.scope.max_role, Role::View);
    assert_eq!(
        code(a.agents.update(&oc, agent.id, AgentUpdate::default()).await),
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        code(
            a.agents
                .update(
                    &ac,
                    agent.id,
                    AgentUpdate {
                        scope: Some(scope(None, Role::Manage)),
                        ..AgentUpdate::default()
                    }
                )
                .await
        ),
        ErrorCode::UserRequired
    );
    assert_eq!(
        code(a.agents.register(&ac, new_agent("child", None)).await),
        ErrorCode::UserRequired
    );
    // Disable (admin) and enable (owner).
    let d = a.agents.set_disabled(&oc, agent.id, true).await.unwrap();
    assert_eq!(d.disabled_reason, Some(DisabledReason::ByAdmin));
    assert_eq!(
        code(a.agents.authenticate(tid, agent.id).await),
        ErrorCode::AgentDisabled
    );
    assert_eq!(
        code(a.agents.set_disabled(&oc, agent.id, false).await),
        ErrorCode::PermissionDenied
    );
    let e = a.agents.set_disabled(&mc, agent.id, false).await.unwrap();
    assert!(e.disabled_at.is_none());
    assert_eq!(
        a.agents.authenticate(tid, agent.id).await.unwrap().id,
        agent.id
    );
    assert!(
        a.agents
            .activity(&mc, agent.id, None, PageRequest::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(
        code(
            a.agents
                .register(
                    &mc,
                    NewAgent {
                        name: "".into(),
                        description: None,
                        scope: scope(None, Role::View)
                    }
                )
                .await
        ),
        ErrorCode::ValidationFailed
    );
}

async fn a_scope_subtree_stays_in_the_tenant(a: Area) {
    let t = team(&a).await;
    let other = team(&a).await;
    let mc = ctx(t.tenant.id, t.member.id);
    assert_eq!(
        code(
            a.agents
                .register(&mc, new_agent("escape", Some(other.tenant.root_node_id)))
                .await
        ),
        ErrorCode::NotFound
    );
    let agent = a.agents.register(&mc, new_agent("ok", None)).await.unwrap();
    assert_eq!(
        code(
            a.agents
                .update(
                    &mc,
                    agent.id,
                    AgentUpdate {
                        scope: Some(scope(Some(other.tenant.root_node_id), Role::View)),
                        ..AgentUpdate::default()
                    }
                )
                .await
        ),
        ErrorCode::NotFound
    );
    // Another tenant's agent is not found.
    assert_eq!(
        code(
            a.agents
                .get(&ctx(other.tenant.id, other.owner.id), agent.id)
                .await
        ),
        ErrorCode::NotFound
    );
}

async fn the_agent_limit_holds(a: Area) {
    let solo = user(&a, "solo").await;
    let c = ctx(solo.home_tenant_id, solo.id);
    // The free plan allows two agents.
    a.agents.register(&c, new_agent("one", None)).await.unwrap();
    let two = a.agents.register(&c, new_agent("two", None)).await.unwrap();
    assert_eq!(
        code(a.agents.register(&c, new_agent("three", None)).await),
        ErrorCode::PlanLimitReached
    );
    // A disabled agent does not count.
    a.agents.set_disabled(&c, two.id, true).await.unwrap();
    a.agents
        .register(&c, new_agent("three", None))
        .await
        .unwrap();
}

/// A guest's agents have their own limit and do not use up the tenant's (security review of
/// 2026-11-06: a guest without grants filled the host's agent limit, and the owner got
/// `plan_limit_reached`).
async fn guests_cannot_use_up_the_tenant_agent_limit(a: Area) {
    let t = team(&a).await;
    let (tid, oc) = (t.tenant.id, ctx(t.tenant.id, t.owner.id));
    let guest = user(&a, "guest").await;
    let (_i, token) = a
        .tenants
        .invite(&oc, guest.email.clone(), MemberRole::Guest, None)
        .await
        .unwrap();
    a.tenants
        .accept_invitation(&Actor::user(guest.id), &token)
        .await
        .unwrap();
    assert_eq!(
        a.tenants.role_of(tid, guest.id).await.unwrap(),
        Some(MemberRole::Guest)
    );
    let gc = ctx(tid, guest.id);
    let own = guest_agent_limit(t.tenant.plan);
    let team_limit = crate::tenants::tenant_limits(t.tenant.plan, 2).agents;
    assert!(own < team_limit);
    let mut first = None;
    for i in 0..own {
        let ag = a
            .agents
            .register(&gc, new_agent(&format!("guest {i}"), None))
            .await
            .unwrap();
        first.get_or_insert(ag.id);
    }
    // The guest reaches their own limit ...
    assert_eq!(
        code(a.agents.register(&gc, new_agent("one more", None)).await),
        ErrorCode::PlanLimitReached
    );
    // ... and the tenant's agents are untouched: the owner and the member register up to the
    // tenant's limit, and the guest's agents are not in the usage.
    let mc = ctx(tid, t.member.id);
    for i in 0..team_limit {
        let c = if i % 2 == 0 { &oc } else { &mc };
        a.agents
            .register(c, new_agent(&format!("team {i}"), None))
            .await
            .unwrap();
    }
    assert_eq!(a.tenants.usage(&oc).await.unwrap().agents, team_limit);
    assert_eq!(
        code(a.agents.register(&oc, new_agent("over", None)).await),
        ErrorCode::PlanLimitReached
    );
    // A disabled agent of the guest frees a place for the guest only.
    a.agents
        .set_disabled(&gc, first.unwrap(), true)
        .await
        .unwrap();
    a.agents
        .register(&gc, new_agent("again", None))
        .await
        .unwrap();
    assert_eq!(
        code(a.agents.register(&oc, new_agent("over", None)).await),
        ErrorCode::PlanLimitReached
    );
}

/// While the tenant policy has `allow_guests: false`, a guest registers no new agent, neither
/// with `registerAgent` nor with the OAuth consent (`new_agent`); members still do (decision
/// 0002-2).
async fn guests_register_no_agents_while_guests_are_off(a: Area) {
    let t = team(&a).await;
    let (tid, oc) = (t.tenant.id, ctx(t.tenant.id, t.owner.id));
    let guest = user(&a, "guest").await;
    let (_i, token) = a
        .tenants
        .invite(&oc, guest.email.clone(), MemberRole::Guest, None)
        .await
        .unwrap();
    a.tenants
        .accept_invitation(&Actor::user(guest.id), &token)
        .await
        .unwrap();
    let gc = ctx(tid, guest.id);
    let kept = a
        .agents
        .register(&gc, new_agent("before", None))
        .await
        .unwrap();
    a.tenants
        .update(
            &oc,
            None,
            Some(crate::tenants::TenantPolicyChange {
                allow_guests: Some(false),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        code(a.agents.register(&gc, new_agent("after", None)).await),
        ErrorCode::PermissionDenied
    );
    let client = register_client(&a).await;
    let sealed = a
        .oauth
        .start_authorization(request(&client, &a))
        .await
        .unwrap();
    let consent = |new_agent: NewAgent| Consent {
        request: sealed.clone(),
        approve: true,
        tenant_id: Some(tid),
        agent_id: None,
        new_agent: Some(new_agent),
    };
    assert_eq!(
        code(
            a.oauth
                .consent(&Actor::user(guest.id), consent(new_agent("oauth", None)))
                .await
        ),
        ErrorCode::PermissionDenied
    );
    // Members are not guests: they still register agents, also through the consent.
    let mc = ctx(tid, t.member.id);
    a.agents
        .register(&mc, new_agent("member", None))
        .await
        .unwrap();
    a.oauth
        .consent(
            &Actor::user(t.member.id),
            consent(new_agent("member oauth", None)),
        )
        .await
        .unwrap();
    // The guest's agent from before stays (the policy refuses only new agents).
    assert!(
        a.agents
            .get(&gc, kept.id)
            .await
            .unwrap()
            .disabled_at
            .is_none()
    );
}

async fn keys_exchange_for_short_tokens(a: Area) {
    let t = team(&a).await;
    let (tid, oc, mc) = (
        t.tenant.id,
        ctx(t.tenant.id, t.owner.id),
        ctx(t.tenant.id, t.member.id),
    );
    let agent = a
        .agents
        .register(&mc, new_agent("script", None))
        .await
        .unwrap();
    let (key, secret) = a
        .agents
        .create_key(&mc, agent.id, Some("laptop".into()), None)
        .await
        .unwrap();
    assert!(secret.starts_with(API_KEY_PREFIX));
    assert_eq!(secret.len(), API_KEY_PREFIX.len() + 43);
    assert!(secret.starts_with(&key.prefix));
    assert!(key.expires_at > crate::now() + time::Duration::days(89));
    // Admins list and revoke keys, but only the owner makes them.
    assert_eq!(
        code(a.agents.create_key(&oc, agent.id, None, None).await),
        ErrorCode::PermissionDenied
    );
    assert_eq!(a.agents.list_keys(&oc, agent.id).await.unwrap().len(), 1);
    assert_eq!(
        code(
            a.agents
                .create_key(
                    &mc,
                    agent.id,
                    None,
                    Some(crate::now() + time::Duration::days(400))
                )
                .await
        ),
        ErrorCode::ValidationFailed
    );
    let token = a.agents.exchange_key(&secret).await.unwrap();
    assert_eq!(token.expires_in, ACCESS_TOKEN_SECONDS);
    assert_eq!(token.issued_token_type.as_deref(), Some(ACCESS_TOKEN_TYPE));
    let claims = a.tokens.verify(&token.access_token).unwrap();
    assert_eq!(claims.typ, TokenType::Agent);
    assert_eq!(claims.tid, Some(tid));
    assert_eq!(claims.obo, Some(t.member.id));
    assert_eq!(claims.agent, Some(agent.id));
    assert!(claims.scope.is_none(), "agent tokens carry no scope");
    assert_eq!(
        code(a.agents.exchange_key("ruum_agent_unknown").await),
        ErrorCode::Unauthenticated
    );
    assert_eq!(
        code(a.agents.exchange_key("garbage").await),
        ErrorCode::Unauthenticated
    );
    // A disabled agent gets no token.
    a.agents.set_disabled(&mc, agent.id, true).await.unwrap();
    assert_eq!(
        code(a.agents.exchange_key(&secret).await),
        ErrorCode::AgentDisabled
    );
    a.agents.set_disabled(&mc, agent.id, false).await.unwrap();
    // A revoked key gets no token.
    a.agents.revoke_key(&oc, agent.id, key.id).await.unwrap();
    assert_eq!(
        code(a.agents.exchange_key(&secret).await),
        ErrorCode::Unauthenticated
    );
    assert!(
        a.agents.list_keys(&mc, agent.id).await.unwrap()[0]
            .revoked_at
            .is_some()
    );
    // Decision 0003-4: each exchange is audited in the agent's tenant, as the agent for its
    // owner; a refused known key with its reason; an unknown key names no tenant.
    let entries = crate::identity::tests::audit_actions(&a, tid).await;
    let used: Vec<_> = entries
        .iter()
        .filter(|e| e.action == "agent.key_used")
        .collect();
    assert_eq!(used.len(), 1);
    assert_eq!(used[0].actor.actor.id.0, agent.id.0);
    assert_eq!(used[0].actor.on_behalf_of, Some(t.member.id));
    assert_eq!(
        used[0].target_id.as_deref(),
        Some(key.id.to_string().as_str())
    );
    let refused: Vec<&str> = entries
        .iter()
        .filter(|e| e.action == "agent.key_refused")
        .map(|e| e.data["reason"].as_str().unwrap())
        .collect();
    assert_eq!(refused, vec!["agent_disabled", "revoked"]);
    assert_eq!(
        code(a.agents.revoke_key(&oc, agent.id, AgentKeyId::new()).await),
        ErrorCode::NotFound
    );
}

async fn register_client(a: &Area) -> OAuthClient {
    a.oauth
        .register_client(ClientRegistration {
            client_name: "Claude".into(),
            redirect_uris: vec![REDIRECT.into()],
            grant_types: vec![],
            response_types: vec![],
            token_endpoint_auth_method: None,
            client_uri: None,
            logo_uri: None,
            software_id: None,
            software_version: None,
        })
        .await
        .unwrap()
}

fn request(client: &OAuthClient, a: &Area) -> AuthorizationRequest {
    AuthorizationRequest {
        client_id: client.client_id.clone(),
        redirect_uri: REDIRECT.into(),
        code_challenge: pkce_challenge(VERIFIER),
        state: Some("xyz".into()),
        resource: a.tokens.audience().to_string(),
        expires_at: crate::now() + time::Duration::minutes(10),
    }
}

fn query_param(url: &str, name: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()?
        .query_pairs()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

async fn oauth_code_flow_with_pkce_and_rotation(a: Area) {
    let t = team(&a).await;
    let member = Actor::user(t.member.id);
    // Registration rules.
    let bad = a
        .oauth
        .register_client(ClientRegistration {
            client_name: "evil".into(),
            redirect_uris: vec!["http://evil.example/cb".into()],
            grant_types: vec![],
            response_types: vec![],
            token_endpoint_auth_method: None,
            client_uri: None,
            logo_uri: None,
            software_id: None,
            software_version: None,
        })
        .await
        .unwrap_err();
    assert_eq!(bad.error, "invalid_redirect_uri");
    let client = register_client(&a).await;
    assert!(client.client_id.starts_with(CLIENT_PREFIX));
    assert_eq!(
        a.oauth
            .get_client(&client.client_id)
            .await
            .unwrap()
            .client_name,
        "Claude"
    );
    let mut wrong = request(&client, &a);
    wrong.redirect_uri = "http://127.0.0.1:1/other".into();
    assert!(a.oauth.start_authorization(wrong).await.is_err());

    // Denial.
    let sealed = a
        .oauth
        .start_authorization(request(&client, &a))
        .await
        .unwrap();
    assert_eq!(
        a.oauth.open_request(&sealed).unwrap().client_id,
        client.client_id
    );
    let denied = a
        .oauth
        .consent(
            &member,
            Consent {
                request: sealed.clone(),
                approve: false,
                tenant_id: None,
                agent_id: None,
                new_agent: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        query_param(&denied, "error").as_deref(),
        Some("access_denied")
    );
    assert_eq!(query_param(&denied, "state").as_deref(), Some("xyz"));

    // Approval with a new agent.
    let approve = |agent_id: Option<AgentId>, new_agent: Option<NewAgent>| Consent {
        request: sealed.clone(),
        approve: true,
        tenant_id: Some(t.tenant.id),
        agent_id,
        new_agent,
    };
    assert_eq!(
        code(a.oauth.consent(&member, approve(None, None)).await),
        ErrorCode::ValidationFailed
    );
    let stranger = user(&a, "stranger").await;
    assert_eq!(
        code(
            a.oauth
                .consent(
                    &Actor::user(stranger.id),
                    approve(None, Some(new_agent("x", None)))
                )
                .await
        ),
        ErrorCode::NotFound
    );
    let redirect = a
        .oauth
        .consent(&member, approve(None, Some(new_agent("mcp", None))))
        .await
        .unwrap();
    assert!(redirect.starts_with(REDIRECT));
    let code_value = query_param(&redirect, "code").unwrap();
    assert_eq!(query_param(&redirect, "state").as_deref(), Some("xyz"));

    let exchange = |code: &str, verifier: &str| TokenRequest::AuthorizationCode {
        code: code.into(),
        code_verifier: verifier.into(),
        client_id: client.client_id.clone(),
        redirect_uri: REDIRECT.into(),
        resource: None,
    };
    // A wrong verifier spends the code.
    let wrong_verifier = "a".repeat(43);
    assert_eq!(
        a.oauth
            .token(exchange(&code_value, &wrong_verifier))
            .await
            .unwrap_err()
            .error,
        "invalid_grant"
    );
    assert_eq!(
        a.oauth
            .token(exchange(&code_value, VERIFIER))
            .await
            .unwrap_err()
            .error,
        "invalid_grant"
    );

    // A second consent binds the same agent again; this time the code works once.
    let agents = a
        .agents
        .list(&ctx(t.tenant.id, t.member.id), None, PageRequest::default())
        .await
        .unwrap();
    let agent = agents
        .items
        .iter()
        .find(|x| x.name == "mcp")
        .unwrap()
        .clone();
    let redirect = a
        .oauth
        .consent(&member, approve(Some(agent.id), None))
        .await
        .unwrap();
    let code_value = query_param(&redirect, "code").unwrap();
    let tokens = a
        .oauth
        .token(exchange(&code_value, VERIFIER))
        .await
        .unwrap();
    let claims = a.tokens.verify(&tokens.access.access_token).unwrap();
    assert_eq!(claims.agent, Some(agent.id));
    assert_eq!(claims.aud, a.tokens.audience());
    let refresh = tokens.refresh_token.unwrap();
    assert!(refresh.starts_with(REFRESH_PREFIX));
    assert!(
        a.oauth
            .token(exchange(&code_value, VERIFIER))
            .await
            .is_err()
    );

    // The replayed code ended the grant: the refresh token is dead.
    let refresh_req = |rt: &str| TokenRequest::RefreshToken {
        refresh_token: rt.into(),
        client_id: client.client_id.clone(),
        resource: None,
    };
    assert_eq!(
        a.oauth
            .token(refresh_req(&refresh))
            .await
            .unwrap_err()
            .error,
        "invalid_grant"
    );

    // A clean flow: refresh rotates, and reuse of an old refresh token ends the grant.
    let redirect = a
        .oauth
        .consent(&member, approve(Some(agent.id), None))
        .await
        .unwrap();
    let code_value = query_param(&redirect, "code").unwrap();
    let first = a
        .oauth
        .token(exchange(&code_value, VERIFIER))
        .await
        .unwrap();
    let r1 = first.refresh_token.unwrap();
    let second = a.oauth.token(refresh_req(&r1)).await.unwrap();
    let r2 = second.refresh_token.unwrap();
    assert_ne!(r1, r2);
    let third = a.oauth.token(refresh_req(&r2)).await.unwrap();
    let r3 = third.refresh_token.unwrap();
    assert_eq!(
        a.oauth.token(refresh_req(&r2)).await.unwrap_err().error,
        "invalid_grant"
    );
    assert_eq!(
        a.oauth.token(refresh_req(&r3)).await.unwrap_err().error,
        "invalid_grant"
    );
    // Decision 0003-4: every issue is audited in the agent's tenant, with its grant type.
    let grants: Vec<String> = crate::identity::tests::audit_actions(&a, t.tenant.id)
        .await
        .into_iter()
        .filter(|e| e.action == "agent.token_issued")
        .map(|e| e.data["grant"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        grants,
        vec![
            "authorization_code",
            "authorization_code",
            "refresh_token",
            "refresh_token"
        ]
    );

    // Disabling the agent ends its grants.
    let redirect = a
        .oauth
        .consent(&member, approve(Some(agent.id), None))
        .await
        .unwrap();
    let code_value = query_param(&redirect, "code").unwrap();
    let r = a
        .oauth
        .token(exchange(&code_value, VERIFIER))
        .await
        .unwrap()
        .refresh_token
        .unwrap();
    a.agents
        .set_disabled(&ctx(t.tenant.id, t.member.id), agent.id, true)
        .await
        .unwrap();
    assert!(a.oauth.token(refresh_req(&r)).await.is_err());
    assert_eq!(a.oauth.end_grants(t.tenant.id, agent.id).await.unwrap(), 0);
    // Agents cannot consent.
    let ac = as_agent(t.tenant.id, &agent);
    assert_eq!(
        code(
            a.oauth
                .consent(&ac.actor, approve(Some(agent.id), None))
                .await
        ),
        ErrorCode::UserRequired
    );
}

/// An agent whose scope has expired (`scope.expires_at`) is refused everywhere: per request
/// (`authenticate`), at the key exchange, and at the OAuth code and refresh grants. Before the
/// expiry all of them work.
async fn an_expired_scope_ends_the_agent(a: Area) {
    let t = team(&a).await;
    let mc = ctx(t.tenant.id, t.member.id);
    let member = Actor::user(t.member.id);
    let mut req = new_agent("short-lived", None);
    req.scope.expires_at = Some(crate::now() + time::Duration::seconds(2));
    let agent = a.agents.register(&mc, req).await.unwrap();
    let (_key, secret) = a
        .agents
        .create_key(&mc, agent.id, None, None)
        .await
        .unwrap();
    assert!(a.agents.exchange_key(&secret).await.is_ok());
    assert!(a.agents.authenticate(t.tenant.id, agent.id).await.is_ok());
    let client = register_client(&a).await;
    let consent = |sealed: String| Consent {
        request: sealed,
        approve: true,
        tenant_id: Some(t.tenant.id),
        agent_id: Some(agent.id),
        new_agent: None,
    };
    let exchange = |code: &str| TokenRequest::AuthorizationCode {
        code: code.into(),
        code_verifier: VERIFIER.into(),
        client_id: client.client_id.clone(),
        redirect_uri: REDIRECT.into(),
        resource: None,
    };
    // A refresh token from before the expiry; it rotates while the scope is live.
    let sealed = a
        .oauth
        .start_authorization(request(&client, &a))
        .await
        .unwrap();
    let redirect = a.oauth.consent(&member, consent(sealed)).await.unwrap();
    let first = a
        .oauth
        .token(exchange(&query_param(&redirect, "code").unwrap()))
        .await
        .unwrap()
        .refresh_token
        .unwrap();
    let refresh_req = |rt: String| TokenRequest::RefreshToken {
        refresh_token: rt,
        client_id: client.client_id.clone(),
        resource: None,
    };
    let refresh = a
        .oauth
        .token(refresh_req(first))
        .await
        .unwrap()
        .refresh_token
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    assert_eq!(
        code(a.agents.authenticate(t.tenant.id, agent.id).await),
        ErrorCode::AgentDisabled
    );
    assert_eq!(
        code(a.agents.exchange_key(&secret).await),
        ErrorCode::AgentDisabled
    );
    assert!(a.oauth.token(refresh_req(refresh)).await.is_err());
    // A new code for the expired agent gives no token (the consent itself may refuse it).
    let sealed = a
        .oauth
        .start_authorization(request(&client, &a))
        .await
        .unwrap();
    if let Ok(redirect) = a.oauth.consent(&member, consent(sealed)).await {
        let code_value = query_param(&redirect, "code").unwrap();
        assert!(a.oauth.token(exchange(&code_value)).await.is_err());
    }
}

/// Agent names are unique per owner and tenant, ignoring case (decision 0003-5): a clash gets
/// " (2)", " (3)", ...; another owner may use the same name; a rename follows the same rule;
/// a long name is cut so that the suffix fits.
async fn agent_names_are_unique_per_owner(a: Area) {
    let t = team(&a).await;
    let (oc, mc) = (ctx(t.tenant.id, t.owner.id), ctx(t.tenant.id, t.member.id));
    let first = a
        .agents
        .register(&oc, new_agent("Kadri Kask", None))
        .await
        .unwrap();
    assert_eq!(first.name, "Kadri Kask");
    let second = a
        .agents
        .register(&oc, new_agent("kadri kask", None))
        .await
        .unwrap();
    assert_eq!(second.name, "kadri kask (2)");
    let third = a
        .agents
        .register(&oc, new_agent("Kadri Kask", None))
        .await
        .unwrap();
    assert_eq!(third.name, "Kadri Kask (3)");
    // Another owner's agent may have the same name; the owner's name tells them apart.
    let other = a
        .agents
        .register(&mc, new_agent("Kadri Kask", None))
        .await
        .unwrap();
    assert_eq!(other.name, "Kadri Kask");
    // A rename to a taken name is numbered too; a rename to its own name keeps it.
    let renamed = a
        .agents
        .update(
            &oc,
            third.id,
            AgentUpdate {
                name: Some("KADRI KASK".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(renamed.name, "KADRI KASK (3)");
    let same = a
        .agents
        .update(
            &oc,
            first.id,
            AgentUpdate {
                name: Some("Kadri Kask".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(same.name, "Kadri Kask");
    // The longest name still fits with its suffix.
    let long = "x".repeat(AGENT_NAME_MAX);
    a.agents
        .register(&oc, new_agent(&long, None))
        .await
        .unwrap();
    let cut = a
        .agents
        .register(&oc, new_agent(&long, None))
        .await
        .unwrap();
    assert_eq!(cut.name.chars().count(), AGENT_NAME_MAX);
    assert!(cut.name.ends_with(" (2)"));
}

crate::conformance!(
    agent_names_are_unique_per_owner,
    agents_belong_to_their_owner,
    a_scope_subtree_stays_in_the_tenant,
    the_agent_limit_holds,
    guests_cannot_use_up_the_tenant_agent_limit,
    guests_register_no_agents_while_guests_are_off,
    keys_exchange_for_short_tokens,
    oauth_code_flow_with_pkce_and_rotation,
    an_expired_scope_ends_the_agent,
);

#[test]
fn every_event_has_an_inverse_or_is_irreversible() {
    use crate::journal::EventType::*;
    for e in [
        NodeCreated,
        NodeRenamed,
        NodeMoved,
        NodeCopied,
        NodeTrashed,
        NodeRestored,
        VersionCreated,
        VersionRestored,
        VersionLabeled,
        TagAttached,
        TagDetached,
        GrantAdded,
        GrantUpdated,
        GrantRemoved,
        ShareLinkCreated,
        NodeSharingUpdated,
    ] {
        assert!(undo_action(e).is_some(), "{e:?}");
    }
    assert!(undo_action(NodePurged).is_none());
    assert!(undo_action(ShareLinkRevoked).is_none());
}

#[test]
fn redirect_uris_follow_the_contract() {
    assert!(redirect_uri_allowed(
        "https://claude.ai/api/mcp/auth_callback"
    ));
    assert!(redirect_uri_allowed("http://127.0.0.1:33418/callback"));
    assert!(redirect_uri_allowed("http://localhost:8976/cb"));
    assert!(!redirect_uri_allowed("http://example.com/cb"));
    assert!(!redirect_uri_allowed("https://example.com/cb#frag"));
    assert!(!redirect_uri_allowed("custom:/cb"));
    assert_eq!(
        resource_audience(Some("https://ruum.localhost/mcp"), "https://ruum.localhost").unwrap(),
        "https://ruum.localhost/mcp"
    );
    assert!(resource_audience(Some("https://evil.example"), "https://ruum.localhost").is_err());
}

#[test]
fn unique_agent_names_skip_taken_suffixes() {
    assert_eq!(unique_agent_name("Bot", []), "Bot");
    assert_eq!(unique_agent_name("Bot", ["bot"]), "Bot (2)");
    assert_eq!(unique_agent_name("Bot", ["BOT", "bot (2)"]), "Bot (3)");
    let long = "y".repeat(AGENT_NAME_MAX);
    let n = unique_agent_name(&long, [long.as_str()]);
    assert_eq!(n.chars().count(), AGENT_NAME_MAX);
    assert!(n.ends_with(" (2)"));
}

/// Migration 0003 renames the duplicate agent names that exist before the unique index: the
/// oldest keeps its name, the others are numbered by age, skipping names that are taken.
#[sqlx::test(migrations = "../../migrations")]
async fn the_migration_numbers_duplicate_agent_names(pool: sqlx::PgPool) {
    let a = crate::identity::tests::postgres(pool.clone());
    let t = team(&a).await;
    let oc = ctx(t.tenant.id, t.owner.id);
    let mut ids = Vec::new();
    for n in ["a", "b", "c", "d"] {
        ids.push(a.agents.register(&oc, new_agent(n, None)).await.unwrap().id);
    }
    let other = a
        .agents
        .register(&ctx(t.tenant.id, t.member.id), new_agent("m", None))
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("select set_config('ruum.system', 'on', true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("drop index agents_owner_name_key")
        .execute(&mut *tx)
        .await
        .unwrap();
    // Three agents named "Bot" (different case), one already named "Bot (2)", and the
    // member's agent also named "Bot".
    for (id, name) in [
        (ids[0], "Bot"),
        (ids[1], "bot"),
        (ids[2], "Bot (2)"),
        (ids[3], "BOT"),
    ] {
        sqlx::query("update agents set name = $2 where id = $1")
            .bind(id)
            .bind(name)
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    sqlx::query("update agents set name = 'Bot' where id = $1")
        .bind(other.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!(
        "../../../../migrations/0003_agent_names_unique.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    let mut names = Vec::new();
    for id in ids.iter().chain([&other.id]) {
        let n: String = sqlx::query_scalar("select name from agents where id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        names.push(n);
    }
    assert_eq!(names, ["Bot", "bot (3)", "Bot (2)", "BOT (4)", "Bot"]);
    tx.commit().await.unwrap();
}
