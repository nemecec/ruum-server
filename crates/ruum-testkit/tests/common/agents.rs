//! Agents on databases (design 8.5, decisions 0004-5, 0004-12, 0004-20 and 0004-26): the order
//! of checks, `write_live`, approvals and who decides them, grant requests, migrations of
//! agents, dry runs, the query log and the "Waiting for you" list, column classes and restore
//! points.
//!
//! The agent is `agent` (the agent of `member`); a case widens its scope through `member`
//! (`updateAgent`) when it needs `edit` or `manage`. `member` (content role `manage`) is the
//! agent's user and a manager of the database; `owner` is a manager who is not the agent's user;
//! `member_edit` is an editor who is a stranger to the agent.

use super::api::*;
use super::{Case, Ctx, Fail};
use crate::{case, check, fail};
use ruum_testkit::RequestSpec;
use ruum_testkit::fixtures::Who;
use serde_json::{Value, json};

/// The rows of `table` in the garden, read by the owner.
async fn rows(cx: &Ctx, db: &str, table: &str) -> Result<i64, Fail> {
    let r = cx
        .ok(
            Who::Owner,
            read(cx, db, &format!("select count(*) from {table}")),
            &[200],
        )
        .await?;
    match first_cell(&r) {
        Value::Number(n) => Ok(n.as_i64().unwrap_or(-1)),
        Value::String(s) => Ok(s.parse().unwrap_or(-1)),
        other => fail!("count(*) gave {other}"),
    }
}

/// The approvals on `db` that `who` sees (`listDatabaseApprovals`, all statuses).
async fn approvals(cx: &Ctx, who: Who, db: &str) -> Result<Vec<Value>, Fail> {
    let page = cx
        .ok(who, list(cx, db, "/approvals?limit=200"), &[200])
        .await?;
    Ok(items(&page))
}

/// The pending approvals that `who` sees across the tenant (`listPendingDatabaseApprovals`).
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

/// A statement sent again with its approval.
fn approved(cx: &Ctx, db: &str, sql: &str, approval: &str) -> RequestSpec {
    query_body(
        cx,
        db,
        json!({ "sql": sql, "mode": "write", "approval_id": approval }),
    )
}

/// The actor IDs of the entries of a query log page.
fn log_actors(page: &Value) -> Vec<String> {
    items(page)
        .iter()
        .filter_map(|e| e["by"]["actor"]["id"].as_str().map(str::to_string))
        .collect()
}

/// The garden, the agent widened to `role` on `Projects`.
async fn garden_with_agent(cx: &Ctx, name: &str, role: &str) -> Result<Garden, Fail> {
    let g = garden(cx, name).await?;
    set_scope(cx, Who::Agent, Some(cx.a().projects), role).await?;
    Ok(g)
}

pub fn cases() -> Vec<Case> {
    vec![
        case!(
            "order_of_checks_for_a_destructive_statement",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "updateAgent",
                "createDatabaseAccessGrant",
                "listDatabaseApprovals",
            ],
            "decision 0004-12 a: the user's right and the scope first (agent with view: 403 \
             permission_denied or scope_exceeded), then write_live (403 write_grant_required, \
             never 202, no approval made), then the statement rules (an UPDATE without WHERE that \
             writes a personal column: 403 class_read_required, never 202), and only then the approval \
             (202 with a pending request, nothing ran)",
            |cx| {
                let g = garden(&cx, "order of checks").await?;
                let del = "delete from t1";
                cx.refused(
                    Who::Agent,
                    write(&cx, &g.id, del),
                    403,
                    &["permission_denied", "scope_exceeded"],
                )
                .await?;
                set_scope(&cx, Who::Agent, Some(cx.a().projects), "edit").await?;
                let err = cx
                    .refused(
                        Who::Agent,
                        write(&cx, &g.id, del),
                        403,
                        &["write_grant_required"],
                    )
                    .await?;
                check!(
                    err["hint"].is_string(),
                    "write_grant_required has no hint (the grant URL)"
                );
                check!(
                    approvals(&cx, Who::Member, &g.id).await?.is_empty(),
                    "write_grant_required made an approval"
                );
                give_grant(&cx, &g.id, "write_live", Who::Agent).await?;
                cx.refused(
                    Who::Agent,
                    write(&cx, &g.id, "update plants set email = 'x'"),
                    403,
                    &["class_read_required"],
                )
                .await?;
                let body = cx.ok(Who::Agent, write(&cx, &g.id, del), &[202]).await?;
                approval_of(&body)?;
                let a = &body["approval"];
                check!(a["subject"] == "statement", "subject: {a}");
                check!(
                    a["reasons"]
                        .as_array()
                        .is_some_and(|r| r.contains(&json!("delete_without_where"))),
                    "reasons: {a}"
                );
                check!(a["review_url"].is_string(), "no review_url: {a}");
                check!(
                    rows(&cx, &g.id, "t1").await? == 2,
                    "the destructive statement ran before approval"
                );
                Ok(())
            }
        ),
        case!(
            "an_approval_never_replaces_write_live",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "updateAgent",
                "createDatabaseAccessGrant",
                "decideDatabaseApproval",
                "revokeDatabaseAccessGrant",
            ],
            "an approved statement sent again after the write_live grant ended is 403 \
             write_grant_required and does not run (every check runs again)",
            |cx| {
                let g = garden_with_agent(&cx, "approval without grant", "edit").await?;
                let grant_id = give_grant(&cx, &g.id, "write_live", Who::Agent).await?;
                let del = "delete from t1";
                let id = approval_of(&cx.ok(Who::Agent, write(&cx, &g.id, del), &[202]).await?)?;
                let d = cx
                    .ok(
                        Who::Member,
                        decide(&cx, &g.id, &id, "approve", None),
                        &[200],
                    )
                    .await?;
                check!(d["status"] == "approved", "decided approval: {d}");
                cx.ok(
                    Who::Member,
                    RequestSpec::new(
                        axum::http::Method::DELETE,
                        db_path(&g.id, &format!("/access-grants/{grant_id}")),
                    )
                    .tenant(cx.a().id),
                    &[200],
                )
                .await?;
                cx.refused(
                    Who::Agent,
                    approved(&cx, &g.id, del, &id),
                    403,
                    &["write_grant_required"],
                )
                .await?;
                check!(
                    rows(&cx, &g.id, "t1").await? == 2,
                    "the statement ran without write_live"
                );
                Ok(())
            }
        ),
        case!(
            "who_decides_an_approval",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "updateAgent",
                "createDatabaseAccessGrant",
                "decideDatabaseApproval",
                "getDatabaseApproval",
            ],
            "decision 0004-12 b: the agent's user (member) and a person with manage (owner) \
             decide (200); another editor (member_edit) and a viewer get 403 permission_denied; \
             an agent gets 403 user_required; the approved statement then runs once (200)",
            |cx| {
                let g = garden_with_agent(&cx, "deciders", "edit").await?;
                give_grant(&cx, &g.id, "write_live", Who::Agent).await?;
                let mut ids = Vec::new();
                for t in ["t1", "t2", "t3"] {
                    let body = cx
                        .ok(
                            Who::Agent,
                            write(&cx, &g.id, &format!("delete from {t}")),
                            &[202],
                        )
                        .await?;
                    ids.push(approval_of(&body)?);
                }
                for who in [Who::MemberEdit, Who::MemberView] {
                    cx.refused(
                        who,
                        decide(&cx, &g.id, &ids[0], "approve", None),
                        403,
                        &["permission_denied"],
                    )
                    .await?;
                }
                cx.refused(
                    Who::Agent,
                    decide(&cx, &g.id, &ids[0], "approve", None),
                    403,
                    &["user_required"],
                )
                .await?;
                let still = cx
                    .ok(
                        Who::Member,
                        list(&cx, &g.id, &format!("/approvals/{}", ids[0])),
                        &[200],
                    )
                    .await?;
                check!(
                    still["status"] == "pending",
                    "a refused decision changed the request: {still}"
                );
                let by_user = cx
                    .ok(
                        Who::Member,
                        decide(&cx, &g.id, &ids[0], "approve", None),
                        &[200],
                    )
                    .await?;
                check!(
                    by_user["status"] == "approved"
                        && by_user["decided_by"] == json!(cx.p(Who::Member).id),
                    "decided by the agent's user: {by_user}"
                );
                let by_manager = cx
                    .ok(
                        Who::Owner,
                        decide(&cx, &g.id, &ids[1], "approve", None),
                        &[200],
                    )
                    .await?;
                check!(
                    by_manager["status"] == "approved",
                    "decided by a manager: {by_manager}"
                );
                let ran = cx
                    .ok(
                        Who::Agent,
                        approved(&cx, &g.id, "delete from t1", &ids[0]),
                        &[200],
                    )
                    .await?;
                check!(ran["rows_affected"] == 2, "the approved DELETE: {ran}");
                check!(
                    rows(&cx, &g.id, "t1").await? == 0,
                    "the approved DELETE did not run"
                );
                check!(rows(&cx, &g.id, "t3").await? == 2, "a pending request ran");
                Ok(())
            }
        ),
        case!(
            "agent_migration_on_live_waits_for_a_person",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "updateAgent",
                "createDatabaseAccessGrant",
                "decideDatabaseApproval",
                "listDatabaseMigrations",
            ],
            "an agent with manage on live: without write_live 403 write_grant_required; with \
             write_live 202 (subject migration, nothing ran), then after the approval the same \
             name and up with approval_id: 200, applied",
            |cx| {
                let g = garden_with_agent(&cx, "agent migrations", "manage").await?;
                let (name, up) = (
                    "0002_add_beds",
                    "create table beds (id integer primary key)",
                );
                cx.refused(
                    Who::Agent,
                    migration(&cx, &g.id, name, up),
                    403,
                    &["write_grant_required"],
                )
                .await?;
                give_grant(&cx, &g.id, "write_live", Who::Agent).await?;
                let body = cx
                    .ok(Who::Agent, migration(&cx, &g.id, name, up), &[202])
                    .await?;
                let id = approval_of(&body)?;
                check!(
                    body["approval"]["subject"] == "migration"
                        && body["approval"]["migration_name"] == name,
                    "the approval of a migration: {body}"
                );
                let listed = cx
                    .ok(Who::Owner, list(&cx, &g.id, "/migrations"), &[200])
                    .await?;
                check!(
                    !items(&listed).iter().any(|m| m["name"] == name),
                    "the migration ran before the approval"
                );
                cx.ok(
                    Who::Member,
                    decide(&cx, &g.id, &id, "approve", None),
                    &[200],
                )
                .await?;
                let ran = cx
                    .ok(
                        Who::Agent,
                        migration_body(
                            &cx,
                            &g.id,
                            json!({ "name": name, "up": up, "approval_id": id }),
                        ),
                        &[200],
                    )
                    .await?;
                check!(
                    ran["migration"]["status"] == "applied",
                    "the approved migration: {ran}"
                );
                let listed = cx
                    .ok(Who::Owner, list(&cx, &g.id, "/migrations"), &[200])
                    .await?;
                check!(
                    items(&listed)
                        .iter()
                        .any(|m| m["name"] == name && m["status"] == "applied"),
                    "listDatabaseMigrations has no applied {name}"
                );
                Ok(())
            }
        ),
        case!(
            "a_full_branch_is_no_draft_branch",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "updateAgent",
                "createDatabaseBranch",
            ],
            "a branch with full data has no draft_branch mark: an agent write there is 403 \
             write_grant_required; on a masked branch the same write runs without a grant; an \
             agent cannot make a full branch (403 user_required)",
            |cx| {
                let g = garden_with_agent(&cx, "full branch", "manage").await?;
                cx.refused(
                    Who::Agent,
                    branch(&cx, &g.id, "agent full", "full"),
                    403,
                    &["user_required"],
                )
                .await?;
                let full = written_id(
                    &cx.ok(Who::Member, branch(&cx, &g.id, "full copy", "full"), &[202])
                        .await?,
                )?;
                wait_ready(&cx, &full).await?;
                let masked = written_id(
                    &cx.ok(
                        Who::Member,
                        branch(&cx, &g.id, "masked copy", "masked"),
                        &[202],
                    )
                    .await?,
                )?;
                wait_ready(&cx, &masked).await?;
                let ins = "insert into t1 (id) values (100)";
                cx.refused(
                    Who::Agent,
                    write(&cx, &full, ins),
                    403,
                    &["write_grant_required"],
                )
                .await?;
                cx.ok(Who::Agent, write(&cx, &masked, ins), &[200]).await?;
                Ok(())
            }
        ),
        case!(
            "agents_dry_run_only_on_draft_branches",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "updateAgent",
                "createDatabaseAccessGrant",
                "createDatabaseBranch",
                "listDatabaseMigrations",
                "listDatabaseApprovals",
            ],
            "decision 0004-26: an agent's dry_run on live and on a full branch is 403 \
             user_required (hint dry_run_on_draft_branch), also with write_live; nothing ran, \
             no approval is made, no lock is held (a person's query answers at once); on a \
             masked branch the dry run is 200",
            |cx| {
                let g = garden_with_agent(&cx, "dry runs", "manage").await?;
                let full = written_id(
                    &cx.ok(
                        Who::Member,
                        branch(&cx, &g.id, "full for dry run", "full"),
                        &[202],
                    )
                    .await?,
                )?;
                wait_ready(&cx, &full).await?;
                let masked = written_id(
                    &cx.ok(
                        Who::Member,
                        branch(&cx, &g.id, "masked for dry run", "masked"),
                        &[202],
                    )
                    .await?,
                )?;
                wait_ready(&cx, &masked).await?;
                let dry = |db: &str| {
                    migration_body(
                        &cx,
                        db,
                        json!({ "name": "0002_try_beds", "up": "create table beds (id integer primary key)", "dry_run": true }),
                    )
                };
                for with_grant in [false, true] {
                    if with_grant {
                        give_grant(&cx, &g.id, "write_live", Who::Agent).await?;
                        give_grant(&cx, &full, "write_live", Who::Agent).await?;
                    }
                    for db in [&g.id, &full] {
                        let before = cx
                            .ok(Who::Owner, list(&cx, db, "/migrations"), &[200])
                            .await?;
                        // With write_live the answer is user_required; without it, the
                        // write_live check may come first (decision 0004-12 a).
                        let codes: &[&str] = if with_grant {
                            &["user_required"]
                        } else {
                            &["user_required", "write_grant_required"]
                        };
                        let err = cx.refused(Who::Agent, dry(db), 403, codes).await?;
                        if err["code"] == "user_required" {
                            hint_has(&err, "dry_run_on_draft_branch")?;
                        }
                        let after = cx
                            .ok(Who::Owner, list(&cx, db, "/migrations"), &[200])
                            .await?;
                        check!(
                            items(&before).len() == items(&after).len(),
                            "the refused dry run wrote a migration row"
                        );
                        check!(
                            approvals(&cx, Who::Member, db).await?.is_empty(),
                            "the refused dry run made an approval"
                        );
                        cx.ok(
                            Who::MemberView,
                            read(&cx, db, "select count(*) from t1"),
                            &[200],
                        )
                        .await?;
                    }
                }
                let ok = cx.ok(Who::Agent, dry(&masked), &[200]).await?;
                check!(
                    ok["migration"]["status"] == "dry_run",
                    "the dry run on a masked branch: {ok}"
                );
                Ok(())
            }
        ),
        case!(
            "an_agent_asks_for_a_grant",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "requestDatabaseAccessGrant",
                "decideDatabaseApproval",
                "listDatabaseAccessGrants",
            ],
            "decision 0004-20 C-17: the agent asks (201, a pending approval with subject grant); \
             again (200, the same request); a person asking gets 403 permission_denied; a \
             manager who is not the agent's user cannot decide it (403); the agent's user \
             approves (200) and the grant exists (grant_request.grant_id, listDatabaseAccessGrants)",
            |cx| {
                let g = garden(&cx, "grant requests").await?;
                let ask = || {
                    RequestSpec::post(db_path(&g.id, "/access-grant-requests"))
                        .tenant(cx.a().id)
                        .json(json!({ "kind": "read_personal", "duration_minutes": 60, "reason": "match names" }))
                };
                let first = cx.ok(Who::Agent, ask(), &[201]).await?;
                check!(
                    first["subject"] == "grant"
                        && first["status"] == "pending"
                        && first["review_url"].is_string(),
                    "the grant request: {first}"
                );
                let again = cx.ok(Who::Agent, ask(), &[200]).await?;
                check!(
                    again["id"] == first["id"],
                    "a second request is not the first one: {again}"
                );
                cx.refused(Who::Member, ask(), 403, &["permission_denied"])
                    .await?;
                let id = first["id"].as_str().unwrap_or_default().to_string();
                cx.refused(
                    Who::Owner,
                    decide(&cx, &g.id, &id, "approve", None),
                    403,
                    &["permission_denied"],
                )
                .await?;
                let decided = cx
                    .ok(
                        Who::Member,
                        decide(&cx, &g.id, &id, "approve", None),
                        &[200],
                    )
                    .await?;
                let grant_id = decided["grant_request"]["grant_id"].clone();
                check!(
                    grant_id.is_string(),
                    "the approval made no grant: {decided}"
                );
                let grants = cx
                    .ok(Who::Member, list(&cx, &g.id, "/access-grants"), &[200])
                    .await?;
                check!(
                    items(&grants).iter().any(|gr| gr["id"] == grant_id
                        && gr["kind"] == "read_personal"
                        && gr["agent_id"] == json!(cx.p(Who::Agent).id)
                        && gr["granted_by"] == json!(cx.p(Who::Member).id)),
                    "listDatabaseAccessGrants has no such grant: {grants}"
                );
                Ok(())
            }
        ),
        case!(
            "migration_approval_confirms_classes_after_the_run",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "updateAgent",
                "createDatabaseAccessGrant",
                "decideDatabaseApproval",
                "listDatabaseColumns",
            ],
            "decision 0004-20 C-15: the approval lists planned_columns; an agent cannot send \
             classes (403 user_required); the agent's user approves with classes; the classes \
             are not confirmed before the migration runs, and are confirmed after it ran",
            |cx| {
                let g = garden_with_agent(&cx, "classes in approvals", "manage").await?;
                give_grant(&cx, &g.id, "write_live", Who::Agent).await?;
                let (name, up) = ("0002_add_notes", "alter table plants add column notes text");
                let body = cx
                    .ok(
                        Who::Agent,
                        migration_body(&cx, &g.id, json!({ "name": name, "up": up, "data_classes": { g.engine.key("plants", "notes"): "personal" } })),
                        &[202],
                    )
                    .await?;
                let id = approval_of(&body)?;
                let planned = body["approval"]["planned_columns"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                check!(
                    planned.iter().any(|c| c["table"] == "plants"
                        && c["column"] == "notes"
                        && c["change"] == "added"),
                    "planned_columns has no added plants.notes: {body}"
                );
                let classes = json!([{ "schema": g.engine.schema(), "table": "plants", "column": "notes", "class": "normal" }]);
                cx.refused(
                    Who::Agent,
                    decide(&cx, &g.id, &id, "approve", Some(classes.clone())),
                    403,
                    &["user_required"],
                )
                .await?;
                cx.ok(
                    Who::Member,
                    decide(&cx, &g.id, &id, "approve", Some(classes)),
                    &[200],
                )
                .await?;
                let notes = |page: &Value| {
                    items(page)
                        .into_iter()
                        .find(|c| c["table"] == "plants" && c["column"] == "notes")
                };
                let before = cx
                    .ok(Who::Owner, list(&cx, &g.id, "/columns?limit=200"), &[200])
                    .await?;
                if let Some(c) = notes(&before) {
                    check!(
                        c["confirmed_class"].is_null(),
                        "the class was confirmed before the migration ran: {c}"
                    );
                }
                cx.ok(
                    Who::Agent,
                    migration_body(&cx, &g.id, json!({ "name": name, "up": up, "approval_id": id, "data_classes": { g.engine.key("plants", "notes"): "personal" } })),
                    &[200],
                )
                .await?;
                let after = cx
                    .ok(Who::Owner, list(&cx, &g.id, "/columns?limit=200"), &[200])
                    .await?;
                let Some(c) = notes(&after) else {
                    fail!("listDatabaseColumns has no plants.notes after the migration: {after}");
                };
                check!(
                    c["confirmed_class"] == "normal" && c["data_class"] == "normal",
                    "the classes of the decision are not confirmed after the run: {c}"
                );
                Ok(())
            }
        ),
        case!(
            "who_reads_the_query_log",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "listDatabaseQueries"
            ],
            "decision 0004-20 C-19: a person with view (guest_view) sees their own entries and \
             those of their own agent (agent_of_guest_view), not the others; an agent sees only \
             its own; a person with manage sees all",
            |cx| {
                let g = garden(&cx, "query log").await?;
                let who = [
                    Who::GuestView,
                    Who::AgentOfGuestView,
                    Who::Agent,
                    Who::Owner,
                ];
                for w in who {
                    cx.ok(w, read(&cx, &g.id, "select count(*) from t1"), &[200])
                        .await?;
                }
                let id = |w: Who| cx.p(w).id.to_string();
                let log = |w: Who| {
                    let cx = cx.clone();
                    let db = g.id.clone();
                    async move {
                        let page = cx
                            .ok(w, list(&cx, &db, "/queries?limit=200"), &[200])
                            .await?;
                        Ok::<Vec<String>, Fail>(log_actors(&page))
                    }
                };
                let viewer = log(Who::GuestView).await?;
                check!(
                    viewer
                        .iter()
                        .all(|a| *a == id(Who::GuestView) || *a == id(Who::AgentOfGuestView)),
                    "a viewer sees other people's entries: {viewer:?}"
                );
                check!(
                    viewer.contains(&id(Who::GuestView))
                        && viewer.contains(&id(Who::AgentOfGuestView)),
                    "a viewer misses their own or their agent's entries: {viewer:?}"
                );
                for agent in [Who::AgentOfGuestView, Who::Agent] {
                    let own = log(agent).await?;
                    check!(
                        !own.is_empty() && own.iter().all(|a| *a == id(agent)),
                        "{} sees entries that are not its own: {own:?}",
                        agent.name()
                    );
                }
                let all = log(Who::Member).await?;
                for w in who {
                    check!(
                        all.contains(&id(w)),
                        "a manager misses the entry of {}",
                        w.name()
                    );
                }
                Ok(())
            }
        ),
        case!(
            "waiting_for_you_across_the_tenant",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "updateAgent",
                "createDatabaseAccessGrant",
                "listPendingDatabaseApprovals",
            ],
            "decision 0004-20 C-18: a person sees the requests they may decide (the agent's user, \
             managers of the database), an agent its own; an editor who is a stranger to the \
             agent, a viewer and another agent do not see it",
            |cx| {
                let g = garden_with_agent(&cx, "waiting", "edit").await?;
                give_grant(&cx, &g.id, "write_live", Who::Agent).await?;
                let id = approval_of(
                    &cx.ok(Who::Agent, write(&cx, &g.id, "delete from t1"), &[202])
                        .await?,
                )?;
                for who in [Who::Member, Who::Owner, Who::GuestManage, Who::Agent] {
                    check!(
                        waiting(&cx, who).await?.contains(&id),
                        "{} does not see the request",
                        who.name()
                    );
                }
                for who in [
                    Who::MemberEdit,
                    Who::MemberView,
                    Who::AgentOfGuestView,
                    Who::GuestEdit,
                ] {
                    check!(
                        !waiting(&cx, who).await?.contains(&id),
                        "{} sees a request it may not decide",
                        who.name()
                    );
                }
                Ok(())
            }
        ),
        case!(
            "column_classes_are_confirmed_by_a_person",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "updateAgent",
                "listDatabaseColumns",
                "suggestColumnClasses",
                "confirmColumnClasses",
                "getDatabaseSchema",
            ],
            "a new column without a class is unconfirmed and counts as personal; an agent's \
             suggestion changes nothing but the suggestion; an agent cannot confirm (403 \
             user_required), an editor cannot (403); a manager confirms (200), then the class is \
             effective; a suggestion for a column that does not exist is 422",
            |cx| {
                let g = garden_with_agent(&cx, "classes", "edit").await?;
                cx.ok(
                    Who::Member,
                    migration(
                        &cx,
                        &g.id,
                        "0002_add_nick",
                        "alter table plants add column nick text",
                    ),
                    &[200],
                )
                .await?;
                let nick = |page: &Value| {
                    items(page)
                        .into_iter()
                        .find(|c| c["table"] == "plants" && c["column"] == "nick")
                };
                let unconfirmed = cx
                    .ok(
                        Who::Owner,
                        list(&cx, &g.id, "/columns?unconfirmed=true&limit=200"),
                        &[200],
                    )
                    .await?;
                let Some(c) = nick(&unconfirmed) else {
                    fail!("nick is not unconfirmed: {unconfirmed}")
                };
                check!(
                    c["data_class"] == "personal" && c["confirmed_class"].is_null(),
                    "an unconfirmed column: {c}"
                );
                let col = |class: &str| json!({ "schema": g.engine.schema(), "table": "plants", "column": "nick", "class": class });
                let mut s = col("normal");
                s["reason"] = json!("a nickname of a plant");
                cx.ok(
                    Who::Agent,
                    RequestSpec::post(db_path(&g.id, "/columns/suggestions"))
                        .tenant(cx.a().id)
                        .json(json!({ "suggestions": [s] })),
                    &[200],
                )
                .await?;
                let mut missing = col("normal");
                missing["column"] = json!("no_such_column");
                missing["reason"] = json!("not there");
                cx.refused(
                    Who::Agent,
                    RequestSpec::post(db_path(&g.id, "/columns/suggestions"))
                        .tenant(cx.a().id)
                        .json(json!({ "suggestions": [missing] })),
                    422,
                    &["validation_failed"],
                )
                .await?;
                let after = cx
                    .ok(Who::Owner, list(&cx, &g.id, "/columns?limit=200"), &[200])
                    .await?;
                let Some(c) = nick(&after) else {
                    fail!("no nick column: {after}")
                };
                check!(
                    c["data_class"] == "personal"
                        && c["confirmed_class"].is_null()
                        && c["suggestion"]["class"] == "normal",
                    "a suggestion changed the effective class: {c}"
                );
                let confirm = || {
                    RequestSpec::post(db_path(&g.id, "/columns/confirm"))
                        .tenant(cx.a().id)
                        .json(json!({ "columns": [col("normal")] }))
                };
                cx.refused(Who::Agent, confirm(), 403, &["user_required"])
                    .await?;
                cx.refused(Who::MemberEdit, confirm(), 403, &["permission_denied"])
                    .await?;
                cx.ok(Who::Member, confirm(), &[200]).await?;
                let done = cx
                    .ok(Who::Owner, list(&cx, &g.id, "/columns?limit=200"), &[200])
                    .await?;
                let Some(c) = nick(&done) else {
                    fail!("no nick column: {done}")
                };
                check!(
                    c["data_class"] == "normal" && c["confirmed_class"] == "normal",
                    "the confirmed class: {c}"
                );
                let schema = cx
                    .ok(Who::MemberView, list(&cx, &g.id, "/schema"), &[200])
                    .await?;
                let plants = schema["tables"]
                    .as_array()
                    .and_then(|t| t.iter().find(|t| t["name"] == "plants").cloned());
                let Some(plants) = plants else {
                    fail!("getDatabaseSchema has no plants: {schema}")
                };
                let nick_col = plants["columns"]
                    .as_array()
                    .and_then(|c| c.iter().find(|c| c["name"] == "nick").cloned());
                check!(
                    nick_col.is_some_and(|c| c["data_class"] == "normal" && c["confirmed"] == true),
                    "getDatabaseSchema does not show the confirmed class: {plants}"
                );
                Ok(())
            }
        ),
        case!(
            "restore_points_need_edit",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "createDatabaseRestorePoint",
                "listDatabaseRestorePoints"
            ],
            "createDatabaseRestorePoint needs edit: member_edit 201 (reason manual), member_view \
             and the narrow agent 403; the point is listed for a viewer, with the window",
            |cx| {
                let g = garden(&cx, "restore points").await?;
                let made = cx
                    .ok(
                        Who::MemberEdit,
                        restore_point(&cx, &g.id, "before import"),
                        &[201],
                    )
                    .await?;
                check!(
                    made["restore_point"]["reason"] == "manual",
                    "a named restore point: {made}"
                );
                for who in [Who::MemberView, Who::Agent] {
                    cx.refused(
                        who,
                        restore_point(&cx, &g.id, "not allowed"),
                        403,
                        &["permission_denied", "scope_exceeded"],
                    )
                    .await?;
                }
                let page = cx
                    .ok(Who::MemberView, list(&cx, &g.id, "/restore-points"), &[200])
                    .await?;
                check!(
                    items(&page).iter().any(|p| p["label"] == "before import"),
                    "the restore point is not listed: {page}"
                );
                check!(
                    page.get("window").is_some(),
                    "the page has no window: {page}"
                );
                Ok(())
            }
        ),
        case!(
            "who_ends_a_grant",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "updateAgent",
                "createDatabaseAccessGrant",
                "revokeDatabaseAccessGrant",
                "listDatabaseAccessGrants",
            ],
            "the agent that holds a grant gives it back (200); a viewer cannot end one (403); a \
             manager can (200); an agent lists only its own grants; a grant to another user's \
             agent by member is 403",
            |cx| {
                let g = garden_with_agent(&cx, "ending grants", "edit").await?;
                let revoke = |id: &str| {
                    RequestSpec::new(
                        axum::http::Method::DELETE,
                        db_path(&g.id, &format!("/access-grants/{id}")),
                    )
                    .tenant(cx.a().id)
                };
                let first = give_grant(&cx, &g.id, "write_live", Who::Agent).await?;
                let ended = cx.ok(Who::Agent, revoke(&first), &[200]).await?;
                check!(
                    ended["grant"]["revoked_at"].is_string(),
                    "the grant did not end: {ended}"
                );
                let second = give_grant(&cx, &g.id, "read_personal", Who::Agent).await?;
                cx.refused(
                    Who::MemberView,
                    revoke(&second),
                    403,
                    &["permission_denied"],
                )
                .await?;
                cx.ok(Who::Owner, revoke(&second), &[200]).await?;
                cx.refused(
                    Who::Member,
                    grant(&cx, &g.id, "read_personal", Who::AgentOfGuestView),
                    403,
                    &["permission_denied"],
                )
                .await?;
                let own = cx
                    .ok(
                        Who::AgentOfGuestView,
                        list(&cx, &g.id, "/access-grants"),
                        &[200],
                    )
                    .await?;
                check!(
                    items(&own).is_empty(),
                    "an agent sees grants of another agent: {own}"
                );
                Ok(())
            }
        ),
    ]
}
