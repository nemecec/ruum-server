//! Card A-1: the grants of agents on a database (`createDatabaseAccessGrant`,
//! `listDatabaseAccessGrants`, `revokeDatabaseAccessGrant`), over HTTP on the backend of the
//! suites (`RUUM_SUITE_BACKEND=memory|postgres`), with the grant check
//! (`grants::active`) and the sweeper of the core. Each case runs in a world of its own; the
//! fixture database `<prefix>-garden` holds the fixture grant: `read_personal` of `agent`,
//! given by `member` for 8 hours. The rules and the harness are those of the T-1 cases
//! (`tests/common/mod.rs`); these cases are not T-1 cases (gate G-3 of the wave plan).

mod common;

use axum::http::Method;
use common::api::{db_path, items, set_scope};
use common::{Case, Ctx, Fail};
use ruum_core::databases::access::grants::{
    EndCause, GrantKind, SweepRound, access_changed_delivery_id, agent_subject,
};
use ruum_core::{AgentId, TenantId};
use ruum_testkit::client::RequestSpec;
use ruum_testkit::fixtures::Who;
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

#[tokio::test]
async fn databases_grants_api() {
    common::run_group("database grants (card A-1)", cases()).await;
}

fn grant_req(cx: &Ctx, kind: &str, agent: Who, minutes: i64) -> RequestSpec {
    RequestSpec::post(db_path(cx.a().database, "/access-grants"))
        .tenant(cx.a().id)
        .json(json!({
            "kind": kind,
            "agent_id": cx.p(agent).id,
            "duration_minutes": minutes,
            "reason": "card A-1 test",
        }))
}

fn revoke_req(cx: &Ctx, grant: &str) -> RequestSpec {
    RequestSpec::new(
        Method::DELETE,
        db_path(cx.a().database, &format!("/access-grants/{grant}")),
    )
    .tenant(cx.a().id)
}

fn list_req(cx: &Ctx, query: &str) -> RequestSpec {
    RequestSpec::get(db_path(cx.a().database, &format!("/access-grants{query}"))).tenant(cx.a().id)
}

/// `createDatabaseAccessGrant` by `member` for `agent`; returns the grant ID.
async fn give(cx: &Ctx, kind: &str, agent: Who, minutes: i64) -> Result<String, Fail> {
    let w = cx
        .ok(Who::Member, grant_req(cx, kind, agent, minutes), &[201])
        .await?;
    match w["grant"]["id"].as_str() {
        Some(id) => Ok(id.to_string()),
        None => fail!("no grant.id: {w}"),
    }
}

fn agent_id(cx: &Ctx, who: Who) -> AgentId {
    AgentId(cx.p(who).id)
}

/// Runs `f` as a request of tenant A would: at least as fresh as the tenant's newest
/// relationship write (decision 0002-4), so a SpiceDB engine reads what the requests wrote.
async fn fresh<F: std::future::Future>(cx: &Ctx, f: F) -> F::Output {
    let services = cx.state.services.clone();
    let t = cx.a().id;
    ruum_core::authz::with_request_freshness(async move {
        ruum_core::authz::load_request_floor(&*services.freshness, t)
            .await
            .expect("the freshness floor");
        f.await
    })
    .await
}

/// `grants::active` of `agent` and `kind` on the fixture database, `minutes` from now (the
/// time of the check; the stores take any time): the grant ID.
async fn active_in(
    cx: &Ctx,
    agent: Who,
    kind: GrantKind,
    minutes: u64,
) -> Result<Option<Uuid>, Fail> {
    let s = cx.state.services.databases.clone();
    let (t, db, a) = (cx.a().id, cx.a().database, agent_id(cx, agent));
    let at = ruum_core::now() + Duration::from_secs(minutes * 60);
    fresh(
        cx,
        async move { s.active_grant_at(t, db, a, kind, at).await },
    )
    .await
    .map(|g| g.map(|g| g.id))
    .map_err(|e| Fail::Failed(format!("grants::active: {e}")))
}

/// One sweeper round, `minutes` from now.
async fn sweep_in(cx: &Ctx, minutes: u64) -> Result<SweepRound, Fail> {
    let s = cx.state.services.databases.clone();
    let at = ruum_core::now() + Duration::from_secs(minutes * 60);
    fresh(cx, async move { s.sweep_grants_at(at).await })
        .await
        .map_err(|e| Fail::Failed(format!("the sweeper: {e}")))
}

/// Whether the tuple of `kind` for `agent` holds on the fixture database.
async fn tuple_holds(cx: &Ctx, agent: Who, kind: GrantKind) -> Result<bool, Fail> {
    let authz = cx.state.services.authz.clone();
    let (db, a) = (cx.a().database, agent_id(cx, agent));
    fresh(cx, async move {
        authz
            .check(
                &agent_subject(a),
                kind.permission(),
                &ruum_core::authz::ObjectRef::node(db),
                None,
            )
            .await
    })
    .await
    .map_err(|e| Fail::Failed(format!("authz check: {e}")))
}

fn tenant(cx: &Ctx) -> TenantId {
    cx.a().id
}

fn cases() -> Vec<Case> {
    vec![
        case!(
            "only_the_agents_user_gives_a_grant",
            ["createDatabaseAccessGrant"],
            "createDatabaseAccessGrant: the person must be the user the agent acts for; users \
             only; view on the database",
            |cx| {
                let w = cx
                    .ok(
                        Who::Member,
                        grant_req(&cx, "read_personal", Who::Agent, 60),
                        &[201],
                    )
                    .await?;
                let g = &w["grant"];
                check!(g["agent_id"] == json!(cx.p(Who::Agent).id), "agent_id: {w}");
                check!(
                    g["granted_by"] == json!(cx.p(Who::Member).id),
                    "granted_by: {w}"
                );
                check!(g["kind"] == "read_personal", "kind: {w}");
                check!(g["restore_point_id"].is_null(), "no restore point: {w}");
                check!(
                    g["revoked_at"].is_null() && g["revoked_by"].is_null(),
                    "live: {w}"
                );
                check!(g["agent_display_name"].is_string(), "the agent's name: {w}");
                check!(
                    g["granted_by_display_name"].is_string(),
                    "the giver's name: {w}"
                );
                check!(w["change"]["seq"].is_i64(), "a journal entry: {w}");
                for who in [Who::MemberEdit, Who::Admin, Who::Owner, Who::GuestManage] {
                    cx.refused(
                        who,
                        grant_req(&cx, "read_personal", Who::Agent, 60),
                        403,
                        &[],
                    )
                    .await?;
                }
                for who in [Who::Agent, Who::AgentOfGuestView] {
                    cx.refused(
                        who,
                        grant_req(&cx, "read_personal", Who::Agent, 60),
                        403,
                        &["user_required"],
                    )
                    .await?;
                }
                cx.refused(
                    Who::Guest,
                    grant_req(&cx, "read_personal", Who::Agent, 60),
                    404,
                    &[],
                )
                .await?;
                Ok(())
            }
        ),
        case!(
            "grant_times_over_the_limits_are_422",
            ["createDatabaseAccessGrant"],
            "createDatabaseAccessGrant: at most 24 hours, read_personal at most 8 hours",
            |cx| {
                cx.refused(
                    Who::Member,
                    grant_req(&cx, "read_personal", Who::Agent, 481),
                    422,
                    &["validation_failed"],
                )
                .await?;
                cx.refused(
                    Who::Member,
                    grant_req(&cx, "write_live", Who::Agent, 1441),
                    422,
                    &["validation_failed"],
                )
                .await?;
                give(&cx, "read_personal", Who::Agent, 480).await?;
                Ok(())
            }
        ),
        case!(
            "write_live_records_a_restore_point_first",
            ["updateAgent", "createDatabaseAccessGrant"],
            "createDatabaseAccessGrant: write_live needs the agent's scope edit and records a \
             restore point (reason before_write_grant) before the grant",
            |cx| {
                cx.refused(
                    Who::Member,
                    grant_req(&cx, "write_live", Who::Agent, 60),
                    403,
                    &[],
                )
                .await?;
                set_scope(&cx, Who::Agent, Some(cx.a().projects), "edit").await?;
                let w = cx
                    .ok(
                        Who::Member,
                        grant_req(&cx, "write_live", Who::Agent, 1440),
                        &[201],
                    )
                    .await?;
                let Some(point) = w["grant"]["restore_point_id"]
                    .as_str()
                    .and_then(|s| s.parse::<Uuid>().ok())
                else {
                    fail!("write_live without a restore point: {w}");
                };
                let row = cx
                    .state
                    .services
                    .databases
                    .deps()
                    .grants
                    .restore_point(tenant(&cx), point)
                    .await
                    .map_err(|e| Fail::Failed(e.to_string()))?;
                let Some(row) = row else {
                    fail!("no database_restore_points row {point}");
                };
                check!(row.reason == "before_write_grant", "reason: {row:?}");
                check!(row.node_id == cx.a().database, "node: {row:?}");
                check!(
                    row.actor.actor.id.0 == cx.p(Who::Member).id
                        && row.actor.on_behalf_of.is_none(),
                    "actor: {row:?}"
                );
                check!(
                    tuple_holds(&cx, Who::Agent, GrantKind::WriteLive).await?,
                    "the live_writer tuple"
                );
                Ok(())
            }
        ),
        case!(
            "people_list_all_grants_and_an_agent_its_own",
            [
                "updateAgent",
                "createDatabaseAccessGrant",
                "listDatabaseAccessGrants"
            ],
            "listDatabaseAccessGrants: view; an agent sees only its own grants; the names are \
             filled (decision 0004-35 c)",
            |cx| {
                set_scope(&cx, Who::AgentOut, Some(cx.a().projects), "view").await?;
                let other = give(&cx, "read_personal", Who::AgentOut, 30).await?;
                let fixture = cx.a().database_grant.to_string();
                let ids = |page: &Value| -> Vec<String> {
                    items(page)
                        .iter()
                        .filter_map(|g| g["id"].as_str().map(String::from))
                        .collect()
                };
                for who in [Who::Member, Who::MemberView, Who::Owner, Who::GuestView] {
                    let page = cx.ok(who, list_req(&cx, ""), &[200]).await?;
                    check!(
                        ids(&page) == vec![other.clone(), fixture.clone()],
                        "{} lists both, newest first: {page}",
                        who.name()
                    );
                    for g in items(&page) {
                        check!(
                            g["agent_display_name"].is_string()
                                && g["granted_by_display_name"].is_string(),
                            "names: {g}"
                        );
                    }
                }
                let page = cx.ok(Who::Agent, list_req(&cx, ""), &[200]).await?;
                check!(
                    ids(&page) == vec![fixture.clone()],
                    "agent: own only: {page}"
                );
                let page = cx.ok(Who::AgentOut, list_req(&cx, ""), &[200]).await?;
                check!(
                    ids(&page) == vec![other.clone()],
                    "agent_out: own only: {page}"
                );
                let page = cx
                    .ok(Who::AgentOfGuestView, list_req(&cx, ""), &[200])
                    .await?;
                check!(ids(&page).is_empty(), "another user's agent: none: {page}");
                let page = cx
                    .ok(Who::Member, list_req(&cx, "?active=true&limit=1"), &[200])
                    .await?;
                check!(ids(&page) == vec![other.clone()], "first page: {page}");
                let Some(next) = page["next_cursor"].as_str() else {
                    fail!("a next page: {page}");
                };
                let page = cx
                    .ok(
                        Who::Member,
                        list_req(&cx, &format!("?active=true&limit=1&cursor={next}")),
                        &[200],
                    )
                    .await?;
                check!(
                    ids(&page) == vec![fixture.clone()] && page["next_cursor"].is_null(),
                    "second page: {page}"
                );
                cx.refused(Who::Guest, list_req(&cx, ""), 404, &[]).await?;
                Ok(())
            }
        ),
        case!(
            "the_giver_a_manager_or_the_agent_ends_a_grant",
            ["createDatabaseAccessGrant", "revokeDatabaseAccessGrant"],
            "revokeDatabaseAccessGrant: the user who gave it, a person with manage, or the agent \
             that holds it (it gives the grant back); others 403",
            |cx| {
                let fixture = cx.a().database_grant.to_string();
                for who in [Who::MemberEdit, Who::MemberView, Who::AgentOfGuestView] {
                    cx.refused(who, revoke_req(&cx, &fixture), 403, &[]).await?;
                }
                cx.refused(Who::Guest, revoke_req(&cx, &fixture), 404, &[])
                    .await?;
                // The agent gives its grant back.
                let w = cx.ok(Who::Agent, revoke_req(&cx, &fixture), &[200]).await?;
                check!(
                    w["grant"]["revoked_by"]["kind"] == "agent"
                        && w["grant"]["revoked_by"]["id"] == json!(cx.p(Who::Agent).id),
                    "revoked by the agent: {w}"
                );
                check!(w["grant"]["revoked_at"].is_string(), "revoked_at: {w}");
                check!(w["change"]["seq"].is_i64(), "a journal entry: {w}");
                cx.refused(Who::Member, revoke_req(&cx, &fixture), 404, &["not_found"])
                    .await?;
                // A manager who did not give it.
                let g = give(&cx, "read_personal", Who::Agent, 60).await?;
                let w = cx.ok(Who::Admin, revoke_req(&cx, &g), &[200]).await?;
                check!(
                    w["grant"]["revoked_by"]["id"] == json!(cx.p(Who::Admin).id),
                    "revoked by the admin: {w}"
                );
                // The giver.
                let g = give(&cx, "read_personal", Who::Agent, 60).await?;
                cx.ok(Who::Member, revoke_req(&cx, &g), &[200]).await?;
                check!(
                    active_in(&cx, Who::Agent, GrantKind::ReadPersonal, 0)
                        .await?
                        .is_none(),
                    "no grant in force after the revokes"
                );
                check!(
                    !tuple_holds(&cx, Who::Agent, GrantKind::ReadPersonal).await?,
                    "the tuple is gone"
                );
                cx.refused(
                    Who::Member,
                    revoke_req(&cx, &Uuid::now_v7().to_string()),
                    404,
                    &[],
                )
                .await?;
                Ok(())
            }
        ),
        case!(
            "an_expired_grant_gives_nothing_before_the_sweeper",
            ["listDatabaseAccessGrants"],
            "grants::active needs a live row AND the tuple: after the end time it is empty while \
             the tuple still holds; the sweeper then deletes the tuple once (mutation rule)",
            |cx| {
                let fixture = cx.a().database_grant;
                let page = cx
                    .ok(Who::Member, list_req(&cx, "?active=true"), &[200])
                    .await?;
                check!(
                    items(&page).len() == 1,
                    "the fixture grant is in force: {page}"
                );
                check!(
                    active_in(&cx, Who::Agent, GrantKind::ReadPersonal, 0).await? == Some(fixture),
                    "in force now"
                );
                check!(
                    active_in(&cx, Who::Agent, GrantKind::ReadPersonal, 481)
                        .await?
                        .is_none(),
                    "nothing after the end time"
                );
                check!(
                    tuple_holds(&cx, Who::Agent, GrantKind::ReadPersonal).await?,
                    "the sweeper has not run: the tuple holds"
                );
                let round = sweep_in(&cx, 481).await?;
                // The fixture grants of tenant A and tenant B (the world has both).
                check!(
                    round
                        == SweepRound {
                            ended: 2,
                            tuples_deleted: 2
                        },
                    "the first round: {round:?}"
                );
                check!(
                    !tuple_holds(&cx, Who::Agent, GrantKind::ReadPersonal).await?,
                    "the tuple is gone"
                );
                let round = sweep_in(&cx, 482).await?;
                check!(
                    round == SweepRound::default(),
                    "the second round: {round:?}"
                );
                let d = cx
                    .state
                    .services
                    .typed_nodes
                    .delivery(
                        tenant(&cx),
                        access_changed_delivery_id(tenant(&cx), fixture, EndCause::Expired),
                    )
                    .await
                    .map_err(|e| Fail::Failed(e.to_string()))?;
                check!(
                    d.is_some_and(|d| d.node_id == Some(cx.a().database)),
                    "access_changed for the database"
                );
                Ok(())
            }
        ),
        case!(
            "a_revoked_grant_gives_nothing_while_another_holds_the_tuple",
            ["createDatabaseAccessGrant", "revokeDatabaseAccessGrant"],
            "grants::active: a revoked grant gives nothing, also while the tuple holds for \
             another live grant of the agent (mutation rule)",
            |cx| {
                cx.ok(
                    Who::Member,
                    revoke_req(&cx, &cx.a().database_grant.to_string()),
                    &[200],
                )
                .await?;
                let long = give(&cx, "read_personal", Who::Agent, 120).await?;
                let short = give(&cx, "read_personal", Who::Agent, 1).await?;
                cx.ok(Who::Member, revoke_req(&cx, &long), &[200]).await?;
                check!(
                    tuple_holds(&cx, Who::Agent, GrantKind::ReadPersonal).await?,
                    "the short grant keeps the tuple"
                );
                check!(
                    active_in(&cx, Who::Agent, GrantKind::ReadPersonal, 0)
                        .await?
                        .map(|g| g.to_string())
                        == Some(short),
                    "the short grant is in force"
                );
                check!(
                    active_in(&cx, Who::Agent, GrantKind::ReadPersonal, 30)
                        .await?
                        .is_none(),
                    "later: the long grant is revoked and the short one has ended"
                );
                Ok(())
            }
        ),
    ]
}
