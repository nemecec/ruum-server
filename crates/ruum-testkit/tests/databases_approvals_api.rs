//! Card A-2: grant requests and approvals (`requestDatabaseAccessGrant`,
//! `decideDatabaseApproval`, `listPendingDatabaseApprovals`, `listDatabaseApprovals`,
//! `getDatabaseApproval`), over HTTP on the backend of the suites
//! (`RUUM_SUITE_BACKEND=memory|postgres`). Each case runs in a world of its own; the fixture
//! database `<prefix>-garden` holds the fixture request: a pending grant request of `agent`
//! (`read_personal`, 60 minutes), which only `member` (the agent's user) decides.
//!
//! These are the cases of the card that need no table: the T-1 cases of the same rules
//! (`an_agent_asks_for_a_grant`, `who_decides_an_approval`, `waiting_for_you_across_the_tenant`
//! in `tests/common/agents.rs`) make their tables with `applyDatabaseMigration` (card D-2) or
//! send statements through the statement path (card A-2w), so they stay planned until then.

mod common;

use common::api::{Engine, db_path, engine, items};
use common::{Case, Ctx, Fail};
use ruum_core::actor::{Actor, Context, PrincipalKind, PrincipalRef};
use ruum_testkit::client::RequestSpec;
use ruum_testkit::fixtures::Who;
use serde_json::{Value, json};
use std::collections::HashMap;

#[tokio::test]
async fn databases_approvals_api() {
    common::run_group("database approvals (card A-2)", cases()).await;
}

fn ask(cx: &Ctx, kind: &str, minutes: i64) -> RequestSpec {
    RequestSpec::post(db_path(cx.a().database, "/access-grant-requests"))
        .tenant(cx.a().id)
        .json(json!({ "kind": kind, "duration_minutes": minutes, "reason": "match names" }))
}

fn decide(cx: &Ctx, approval: &str, decision: &str) -> RequestSpec {
    RequestSpec::post(db_path(
        cx.a().database,
        &format!("/approvals/{approval}/decision"),
    ))
    .tenant(cx.a().id)
    .json(json!({ "decision": decision, "comment": "card A-2 test" }))
}

fn get(cx: &Ctx, approval: &str) -> RequestSpec {
    RequestSpec::get(db_path(cx.a().database, &format!("/approvals/{approval}"))).tenant(cx.a().id)
}

fn fixture_request(cx: &Ctx) -> String {
    cx.a().database_approval.to_string()
}

/// The IDs of the pending requests that `who` sees across the tenant.
async fn waiting(cx: &Ctx, who: Who) -> Result<Vec<String>, Fail> {
    let page = cx
        .ok(
            who,
            RequestSpec::get("/v1/database-approvals?limit=200").tenant(cx.a().id),
            &[200],
        )
        .await?;
    Ok(items(&page)
        .iter()
        .filter_map(|a| a["id"].as_str().map(str::to_string))
        .collect())
}

/// The IDs of the approvals on the fixture database that `who` lists.
async fn listed(cx: &Ctx, who: Who) -> Result<Vec<String>, Fail> {
    let page = cx
        .ok(
            who,
            RequestSpec::get(db_path(cx.a().database, "/approvals?limit=200")).tenant(cx.a().id),
            &[200],
        )
        .await?;
    Ok(items(&page)
        .iter()
        .filter_map(|a| a["id"].as_str().map(str::to_string))
        .collect())
}

/// `review_url` holds the tenant, the database and the request IDs only (C-16).
fn check_review_url(cx: &Ctx, a: &Value) -> Result<(), Fail> {
    let url = a["review_url"].as_str().unwrap_or_default();
    let want = format!(
        "/t/{}/databases/{}/access?request={}",
        cx.a().id,
        cx.a().database,
        a["id"].as_str().unwrap_or_default()
    );
    check!(url.ends_with(&want), "review_url {url:?} is not {want:?}");
    check!(
        !url.contains("match") && !url.contains("names"),
        "review_url holds the reason: {url}"
    );
    Ok(())
}

/// The marker texts of [`marker_up`], one per kind of literal and the comment.
const UP_MARKERS: [&str; 5] = [
    "MarkerString",
    "424242",
    "MarkerFunctionBody",
    "MarkerComment",
    "MarkerInsert",
];

/// A migration `up` with a string, a number, a comment and (Postgres) a dollar-quoted function
/// body, each with a marker text.
fn marker_up(e: Engine) -> &'static str {
    match e {
        Engine::Postgres => {
            "create table approval_plants (id int primary key, \
             name text default 'MarkerString', qty int default 424242);\n\
             -- MarkerComment\n\
             create function approval_note() returns text as $$ select 'MarkerFunctionBody' $$ \
             language sql;\n\
             insert into approval_plants (id, name) values (7, 'MarkerInsert');"
        }
        Engine::Sqlite => {
            "create table approval_plants (id integer primary key, \
             name text default 'MarkerString', qty integer default 424242);\n\
             -- MarkerComment\n\
             insert into approval_plants (id, name) values (7, 'MarkerInsert');"
        }
    }
}

/// A migration approval of `agent` on the fixture database, made through the service
/// (`applyDatabaseMigration` is planned until card D-2). Returns its ID.
async fn agent_migration(cx: &Ctx, up: &str) -> Result<String, Fail> {
    let s = &cx.state.services;
    let (tenant, agent) = (cx.a().id, cx.a().agent_id(Who::Agent));
    let err = |e: ruum_core::Error| Fail::Failed(format!("the agent's migration request: {e}"));
    let scope = s
        .agents
        .authenticate(tenant, agent)
        .await
        .map_err(err)?
        .scope;
    let ac = Context {
        tenant,
        actor: Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::Agent,
                id: agent.0.into(),
            },
            on_behalf_of: Some(cx.a().user_id(Who::Member)),
        },
        scope: Some(scope.as_scope()),
        idempotency_key: None,
    };
    let Some(row) = s
        .databases
        .deps()
        .store
        .get(tenant, cx.a().database)
        .await
        .map_err(err)?
    else {
        fail!("the fixture database has no databases row");
    };
    let (a, _) = s
        .databases
        .request_migration(&ac, &row, "0002_approval_plants", up, &HashMap::new())
        .await
        .map_err(err)?;
    Ok(a.id.to_string())
}

/// The approval `id` in `page`, or a failure.
fn find(page: &Value, id: &str, label: &str) -> Result<Value, Fail> {
    match items(page).into_iter().find(|a| a["id"] == json!(id)) {
        Some(a) => Ok(a),
        None => fail!("{label}: the migration approval is not listed: {page}"),
    }
}

/// `a` shows the `up` normalized: no marker text anywhere, and `$L1` in `statement`.
fn check_normalized(a: &Value, label: &str) -> Result<(), Fail> {
    let text = a.to_string();
    for m in UP_MARKERS {
        check!(!text.contains(m), "{label}: {m} is shown: {a}");
    }
    check!(
        a["statement"].as_str().is_some_and(|s| s.contains("$L1")),
        "{label}: no $L1 in the statement: {a}"
    );
    Ok(())
}

fn cases() -> Vec<Case> {
    vec![
        case!(
            "an_agent_asks_for_a_grant_and_its_user_approves",
            [
                "requestDatabaseAccessGrant",
                "decideDatabaseApproval",
                "getDatabaseApproval",
                "listDatabaseAccessGrants",
            ],
            "decision 0004-20 C-17: the agent asks (201, a pending approval with subject grant \
             and a review_url of IDs); again (200, the same request); a person asking gets 403 \
             permission_denied; a manager who is not the agent's user cannot decide it (403); \
             the agent's user approves (200) and the grant exists (grant_request.grant_id, \
             listDatabaseAccessGrants); a second decision is 409",
            |cx| {
                // The fixture request is the pending read_personal request of `agent`: its user
                // denies it first, so the agent can ask anew.
                let fx = fixture_request(&cx);
                let denied = cx.ok(Who::Member, decide(&cx, &fx, "deny"), &[200]).await?;
                check!(denied["status"] == "denied", "the denial: {denied}");
                let first = cx
                    .ok(Who::Agent, ask(&cx, "read_personal", 60), &[201])
                    .await?;
                check!(
                    first["subject"] == "grant"
                        && first["status"] == "pending"
                        && first["statement"].is_null()
                        && first["params"] == json!([])
                        && first["reasons"] == json!(["grant"])
                        && first["grant_request"]["kind"] == "read_personal"
                        && first["grant_request"]["duration_minutes"] == 60
                        && first["grant_request"]["grant_id"].is_null()
                        && first["requested_by"]["actor"]["id"] == json!(cx.p(Who::Agent).id)
                        && first["requested_by"]["on_behalf_of"] == json!(cx.p(Who::Member).id),
                    "the grant request: {first}"
                );
                check_review_url(&cx, &first)?;
                let again = cx
                    .ok(Who::Agent, ask(&cx, "read_personal", 30), &[200])
                    .await?;
                check!(
                    again["id"] == first["id"],
                    "a second request is not the first one: {again}"
                );
                for who in [Who::Member, Who::Owner] {
                    cx.refused(
                        who,
                        ask(&cx, "read_personal", 60),
                        403,
                        &["permission_denied"],
                    )
                    .await?;
                }
                let id = first["id"].as_str().unwrap_or_default().to_string();
                cx.refused(
                    Who::Owner,
                    decide(&cx, &id, "approve"),
                    403,
                    &["permission_denied"],
                )
                .await?;
                cx.refused(
                    Who::Agent,
                    decide(&cx, &id, "approve"),
                    403,
                    &["user_required"],
                )
                .await?;
                let still = cx.ok(Who::Member, get(&cx, &id), &[200]).await?;
                check!(
                    still["status"] == "pending",
                    "a refused decision changed the request: {still}"
                );
                let decided = cx
                    .ok(Who::Member, decide(&cx, &id, "approve"), &[200])
                    .await?;
                let grant_id = decided["grant_request"]["grant_id"].clone();
                check!(
                    decided["status"] == "approved"
                        && grant_id.is_string()
                        && decided["decided_by"] == json!(cx.p(Who::Member).id)
                        && decided["comment"] == "card A-2 test",
                    "the approval: {decided}"
                );
                let grants = cx
                    .ok(
                        Who::Member,
                        RequestSpec::get(db_path(cx.a().database, "/access-grants?active=true"))
                            .tenant(cx.a().id),
                        &[200],
                    )
                    .await?;
                check!(
                    items(&grants).iter().any(|gr| gr["id"] == grant_id
                        && gr["kind"] == "read_personal"
                        && gr["agent_id"] == json!(cx.p(Who::Agent).id)
                        && gr["granted_by"] == json!(cx.p(Who::Member).id)),
                    "listDatabaseAccessGrants has no such grant: {grants}"
                );
                cx.refused(Who::Member, decide(&cx, &id, "approve"), 409, &["conflict"])
                    .await?;
                Ok(())
            }
        ),
        case!(
            "who_decides_a_grant_request",
            ["decideDatabaseApproval", "getDatabaseApproval"],
            "decision 0004-12 b and C-17: a grant request is decided only by the agent's user \
             (member); managers who are not its user (owner, admin, guest_manage), an editor and \
             a viewer get 403 permission_denied; an agent and a share link 403 user_required; a \
             guest without access 404; the request stays pending until member decides",
            |cx| {
                let fx = fixture_request(&cx);
                for who in [
                    Who::Owner,
                    Who::Admin,
                    Who::GuestManage,
                    Who::MemberEdit,
                    Who::MemberView,
                ] {
                    cx.refused(
                        who,
                        decide(&cx, &fx, "approve"),
                        403,
                        &["permission_denied"],
                    )
                    .await?;
                }
                for who in [Who::Agent, Who::AgentOfGuestView, Who::ShareLink] {
                    cx.refused(who, decide(&cx, &fx, "approve"), 403, &["user_required"])
                        .await?;
                }
                cx.refused(Who::Guest, decide(&cx, &fx, "approve"), 404, &["not_found"])
                    .await?;
                let still = cx.ok(Who::Owner, get(&cx, &fx), &[200]).await?;
                check!(
                    still["status"] == "pending" && still["decided_by"].is_null(),
                    "a refused decision changed the request: {still}"
                );
                let ok = cx
                    .ok(Who::Member, decide(&cx, &fx, "approve"), &[200])
                    .await?;
                check!(
                    ok["status"] == "approved" && ok["grant_request"]["grant_id"].is_string(),
                    "member's approval: {ok}"
                );
                Ok(())
            }
        ),
        case!(
            "waiting_for_you_lists_what_the_caller_may_decide",
            [
                "listPendingDatabaseApprovals",
                "requestDatabaseAccessGrant",
                "decideDatabaseApproval",
            ],
            "decision 0004-20 C-18: a grant request waits for the agent's user only (member sees \
             agent's, guest_view sees agent_of_guest_view's); an agent sees its own; managers \
             who are not the user, other people and other agents do not see it; a decided \
             request leaves the pending list and shows with status=approved",
            |cx| {
                let fx = fixture_request(&cx);
                let theirs = cx
                    .ok(Who::AgentOfGuestView, ask(&cx, "read_personal", 30), &[201])
                    .await?;
                let theirs = theirs["id"].as_str().unwrap_or_default().to_string();
                for (who, sees, hidden) in [
                    (Who::Member, &fx, &theirs),
                    (Who::Agent, &fx, &theirs),
                    (Who::GuestView, &theirs, &fx),
                    (Who::AgentOfGuestView, &theirs, &fx),
                ] {
                    let w = waiting(&cx, who).await?;
                    check!(w.contains(sees), "{} does not see its request", who.name());
                    check!(
                        !w.contains(hidden),
                        "{} sees a request it may not decide",
                        who.name()
                    );
                }
                for who in [
                    Who::Owner,
                    Who::Admin,
                    Who::GuestManage,
                    Who::MemberEdit,
                    Who::MemberView,
                    Who::Guest,
                    Who::AgentOut,
                ] {
                    let w = waiting(&cx, who).await?;
                    check!(
                        !w.contains(&fx) && !w.contains(&theirs),
                        "{} sees a grant request of another user's agent",
                        who.name()
                    );
                }
                cx.refused(
                    Who::ShareLink,
                    RequestSpec::get("/v1/database-approvals").tenant(cx.a().id),
                    403,
                    &["user_required"],
                )
                .await?;
                cx.ok(Who::Member, decide(&cx, &fx, "approve"), &[200])
                    .await?;
                check!(
                    !waiting(&cx, Who::Member).await?.contains(&fx),
                    "a decided request is still pending"
                );
                let approved = cx
                    .ok(
                        Who::Member,
                        RequestSpec::get("/v1/database-approvals?status=approved")
                            .tenant(cx.a().id),
                        &[200],
                    )
                    .await?;
                check!(
                    items(&approved).iter().any(|a| a["id"] == json!(fx)),
                    "status=approved does not list the decided request: {approved}"
                );
                Ok(())
            }
        ),
        case!(
            "approvals_are_read_with_view_and_agents_read_their_own",
            ["listDatabaseApprovals", "getDatabaseApproval"],
            "listDatabaseApprovals and getDatabaseApproval need view; an agent reads only its \
             own requests (another agent's is 404 and not listed); a guest without access gets \
             404; parameters are an empty list for a grant request",
            |cx| {
                let fx = fixture_request(&cx);
                for who in [Who::Member, Who::MemberView, Who::Owner, Who::Agent] {
                    check!(
                        listed(&cx, who).await?.contains(&fx),
                        "{} does not list the request",
                        who.name()
                    );
                    let a = cx.ok(who, get(&cx, &fx), &[200]).await?;
                    check!(
                        a["params"] == json!([]) && a["subject"] == "grant",
                        "{} reads {a}",
                        who.name()
                    );
                    check_review_url(&cx, &a)?;
                }
                check!(
                    !listed(&cx, Who::AgentOfGuestView).await?.contains(&fx),
                    "another agent lists the request"
                );
                cx.refused(Who::AgentOfGuestView, get(&cx, &fx), 404, &["not_found"])
                    .await?;
                cx.refused(Who::Guest, get(&cx, &fx), 404, &["not_found"])
                    .await?;
                cx.refused(
                    Who::Guest,
                    RequestSpec::get(db_path(cx.a().database, "/approvals")).tenant(cx.a().id),
                    404,
                    &["not_found"],
                )
                .await?;
                Ok(())
            }
        ),
        case!(
            "a_grant_request_holds_the_limits_of_a_grant",
            ["requestDatabaseAccessGrant"],
            "the limits of createDatabaseAccessGrant: write_live needs an agent scope of edit \
             (agent has view: 403); read_personal at most 480 minutes and at least 1 (422); a \
             reason of at most 500 characters (422); a guest's agent without access: 404",
            |cx| {
                cx.refused(Who::Agent, ask(&cx, "write_live", 60), 403, &[])
                    .await?;
                for minutes in [0, 481] {
                    cx.refused(
                        Who::Agent,
                        ask(&cx, "read_personal", minutes),
                        422,
                        &["validation_failed"],
                    )
                    .await?;
                }
                let long = RequestSpec::post(db_path(cx.a().database, "/access-grant-requests"))
                    .tenant(cx.a().id)
                    .json(json!({
                        "kind": "read_personal", "duration_minutes": 60, "reason": "x".repeat(501)
                    }));
                cx.refused(Who::Agent, long, 422, &["validation_failed"])
                    .await?;
                let out = cx
                    .send(Who::AgentOut, ask(&cx, "read_personal", 60))
                    .await?;
                check!(
                    [403, 404].contains(&out.status.as_u16()),
                    "an agent outside its scope asked: {} {}",
                    out.status,
                    out.text()
                );
                Ok(())
            }
        ),
        case!(
            "a_migration_up_is_shown_in_full_only_to_managers",
            [
                "getDatabase",
                "getDatabaseApproval",
                "listDatabaseApprovals",
                "listPendingDatabaseApprovals",
                "decideDatabaseApproval",
            ],
            "decision 0004-40 (backlog S-29): the up of an agent's migration approval, with a \
             string, a number, a comment and on Postgres a dollar-quoted function body, is \
             normalized for the agent that asked and for a person with view only (get and \
             list; the agent's waiting list): no marker text, $L1 present; a person with manage \
             (owner) reads it byte-equal in get, list and the waiting list, and in the response \
             of the decision",
            |cx| {
                let e = engine(&cx).await?;
                let up = marker_up(e);
                let id = agent_migration(&cx, up).await?;
                for who in [Who::Agent, Who::MemberView] {
                    let label = format!("{} ({e:?})", who.name());
                    let a = cx.ok(who, get(&cx, &id), &[200]).await?;
                    check_normalized(&a, &format!("get as {label}"))?;
                    let page = cx
                        .ok(
                            who,
                            RequestSpec::get(db_path(cx.a().database, "/approvals?limit=200"))
                                .tenant(cx.a().id),
                            &[200],
                        )
                        .await?;
                    check_normalized(&find(&page, &id, &label)?, &format!("list as {label}"))?;
                }
                let waiting_page = |who: Who| {
                    cx.ok(
                        who,
                        RequestSpec::get("/v1/database-approvals?limit=200").tenant(cx.a().id),
                        &[200],
                    )
                };
                let page = waiting_page(Who::Agent).await?;
                check_normalized(&find(&page, &id, "agent")?, "waiting list as agent")?;
                let a = cx.ok(Who::Owner, get(&cx, &id), &[200]).await?;
                check!(a["statement"] == json!(up), "get as owner: {a}");
                let page = cx
                    .ok(
                        Who::Owner,
                        RequestSpec::get(db_path(cx.a().database, "/approvals?limit=200"))
                            .tenant(cx.a().id),
                        &[200],
                    )
                    .await?;
                let a = find(&page, &id, "owner")?;
                check!(a["statement"] == json!(up), "list as owner: {a}");
                let page = waiting_page(Who::Owner).await?;
                let a = find(&page, &id, "owner")?;
                check!(a["statement"] == json!(up), "waiting list as owner: {a}");
                let decided = cx
                    .ok(Who::Owner, decide(&cx, &id, "approve"), &[200])
                    .await?;
                check!(
                    decided["status"] == "approved" && decided["statement"] == json!(up),
                    "the decision's response: {decided}"
                );
                let a = cx.ok(Who::Agent, get(&cx, &id), &[200]).await?;
                check_normalized(&a, "get as agent after the decision")?;
                Ok(())
            }
        ),
        case!(
            "a_decision_with_an_empty_classes_list_is_refused",
            ["decideDatabaseApproval", "getDatabaseApproval"],
            "decision 0004-40 i (review-A-2 F-6): classes has minItems 1, so classes: [] gets \
             422 validation_failed with the pointer /classes, and the request stays pending",
            |cx| {
                let fx = fixture_request(&cx);
                let req = RequestSpec::post(db_path(
                    cx.a().database,
                    &format!("/approvals/{fx}/decision"),
                ))
                .tenant(cx.a().id)
                .json(json!({ "decision": "approve", "classes": [] }));
                let err = cx
                    .refused(Who::Member, req, 422, &["validation_failed"])
                    .await?;
                check!(err["hint"] == "/classes", "the pointer: {err}");
                let still = cx.ok(Who::Member, get(&cx, &fx), &[200]).await?;
                check!(still["status"] == "pending", "the request changed: {still}");
                Ok(())
            }
        ),
    ]
}
