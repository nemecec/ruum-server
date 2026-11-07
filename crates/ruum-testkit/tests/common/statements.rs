//! Statement rules of decision 0004-35 and the branch rules of `createDatabaseBranch` and
//! `restoreDatabase`: qualified table names, transaction control and `SET LOCAL` in a query and
//! in `up`, `database_not_ready` as `503` with `Retry-After`, a branch of a branch, and a restore
//! of a branch to a time before it was ready.
//!
//! The cases use the engine of the fixture database: `public.t` on Postgres, `main.t` on SQLite.
//! The other engine's name runs where a backend has that engine (the Postgres engine on the
//! Postgres backend with a data cluster; SQLite after card E-3).

use super::api::*;
use super::{Case, Ctx, Fail};
use crate::{case, check};
use ruum_testkit::RequestSpec;
use ruum_testkit::fixtures::Who;
use serde_json::{Value, json};

/// The number of items of a list of the database, as the owner sees it.
async fn count(cx: &Ctx, db: &str, rest: &str) -> Result<usize, Fail> {
    Ok(items(&cx.ok(Who::Owner, list(cx, db, rest), &[200]).await?).len())
}

/// What a refused migration must leave: no migration row, no restore point, no journal entry.
async fn trail(cx: &Ctx, db: &str) -> Result<(usize, usize, Value), Fail> {
    Ok((
        count(cx, db, "/migrations?limit=200").await?,
        count(cx, db, "/restore-points?limit=200").await?,
        latest_cursor(cx).await?,
    ))
}

pub fn cases() -> Vec<Case> {
    vec![
        case!(
            "qualified_names_are_refused_in_queries",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase"
            ],
            "decision 0004-35 b: public.t (Postgres) or main.t (SQLite) in queryDatabase is 422 \
             statement_not_allowed with hint unqualified_names, for a person with manage, a \
             viewer and an agent; the unqualified name works for each",
            |cx| {
                let g = garden(&cx, "qualified queries").await?;
                let qualified = format!("select count(*) from {}.plants", g.engine.schema());
                for who in [Who::Member, Who::MemberView, Who::Agent] {
                    let err = cx
                        .refused(
                            who,
                            read(&cx, &g.id, &qualified),
                            422,
                            &["statement_not_allowed"],
                        )
                        .await?;
                    hint_has(&err, "unqualified_names")?;
                    cx.ok(who, read(&cx, &g.id, "select count(*) from plants"), &[200])
                        .await?;
                }
                Ok(())
            }
        ),
        case!(
            "qualified_names_are_refused_in_up",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "updateAgent",
                "createDatabaseBranch",
                "listDatabaseMigrations",
                "listDatabaseRestorePoints",
                "getLatestCursor",
            ],
            "decision 0004-35 b: a qualified name in up is 422 statement_not_allowed with hint \
             unqualified_names, for a person with manage on live and for an agent on its draft \
             branch, and writes nothing; a viewer cannot apply a migration at all (403 before the \
             statement rules)",
            |cx| {
                let g = garden(&cx, "qualified up").await?;
                let up = format!(
                    "create table {}.beds (id integer primary key)",
                    g.engine.schema()
                );
                let before = trail(&cx, &g.id).await?;
                let err = cx
                    .refused(
                        Who::Member,
                        migration(&cx, &g.id, "0002_beds", &up),
                        422,
                        &["statement_not_allowed"],
                    )
                    .await?;
                hint_has(&err, "unqualified_names")?;
                check!(
                    trail(&cx, &g.id).await? == before,
                    "the refused migration wrote a row, a restore point or a journal entry"
                );
                cx.refused(
                    Who::MemberView,
                    migration(&cx, &g.id, "0002_beds", &up),
                    403,
                    &["permission_denied"],
                )
                .await?;
                set_scope(&cx, Who::Agent, Some(cx.a().projects), "edit").await?;
                let draft = written_id(
                    &cx.ok(
                        Who::Agent,
                        branch(&cx, &g.id, "agent draft", "masked"),
                        &[202],
                    )
                    .await?,
                )?;
                wait_ready(&cx, &draft).await?;
                let err = cx
                    .refused(
                        Who::Agent,
                        migration(&cx, &draft, "0002_beds", &up),
                        422,
                        &["statement_not_allowed"],
                    )
                    .await?;
                hint_has(&err, "unqualified_names")?;
                cx.ok(
                    Who::Agent,
                    migration(
                        &cx,
                        &draft,
                        "0002_beds",
                        "create table beds (id integer primary key)",
                    ),
                    &[200],
                )
                .await?;
                Ok(())
            }
        ),
        case!(
            "transaction_control_is_refused",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "listDatabaseMigrations",
                "listDatabaseRestorePoints",
                "getLatestCursor",
            ],
            "decision 0004-35 a: an up with BEGIN or COMMIT is 422 statement_not_allowed with \
             hint remove_transaction_control and the statement number in message, and writes no \
             restore point, migration row or journal entry; BEGIN in a query is refused the \
             same way",
            |cx| {
                let g = garden(&cx, "transaction control").await?;
                let before = trail(&cx, &g.id).await?;
                for (up, number) in [
                    (
                        "begin;\ncreate table beds (id integer primary key);\ncommit;",
                        "1",
                    ),
                    ("create table beds (id integer primary key);\ncommit;", "2"),
                ] {
                    let err = cx
                        .refused(
                            Who::Member,
                            migration(&cx, &g.id, "0002_beds", up),
                            422,
                            &["statement_not_allowed"],
                        )
                        .await?;
                    hint_has(&err, "remove_transaction_control")?;
                    let msg = err["message"].as_str().unwrap_or_default();
                    check!(
                        msg.contains(number),
                        "the message does not name statement {number}: {msg:?}"
                    );
                    check!(
                        trail(&cx, &g.id).await? == before,
                        "the refused migration wrote a row, a restore point or a journal entry"
                    );
                }
                let err = cx
                    .refused(
                        Who::Member,
                        write(&cx, &g.id, "begin"),
                        422,
                        &["statement_not_allowed"],
                    )
                    .await?;
                hint_has(&err, "remove_transaction_control")?;
                Ok(())
            }
        ),
        case!(
            "set_local_is_refused",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "listDatabaseMigrations",
                "listDatabaseRestorePoints",
                "getLatestCursor",
            ],
            "decision 0004-35 a: SET LOCAL statement_timeout in a query (person and agent) and \
             in up is 422 statement_not_allowed; the migration writes nothing",
            |cx| {
                let g = garden(&cx, "set local").await?;
                for who in [Who::Member, Who::Agent] {
                    cx.refused(
                        who,
                        read(&cx, &g.id, "set local statement_timeout = 0"),
                        422,
                        &["statement_not_allowed"],
                    )
                    .await?;
                }
                let before = trail(&cx, &g.id).await?;
                cx.refused(
                    Who::Member,
                    migration(&cx, &g.id, "0002_beds", "set local statement_timeout = 0;\ncreate table beds (id integer primary key);"),
                    422,
                    &["statement_not_allowed"],
                )
                .await?;
                check!(
                    trail(&cx, &g.id).await? == before,
                    "the refused migration wrote a row, a restore point or a journal entry"
                );
                Ok(())
            }
        ),
        case!(
            "database_not_ready_is_a_503",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "createDatabaseRestorePoint",
                "restoreDatabase",
                "getDatabaseSchema",
                "createDatabaseBranch",
            ],
            "decision 0004-35 d: while an in-place restore runs, queryDatabase, getDatabaseSchema, \
             applyDatabaseMigration, createDatabaseBranch, createDatabaseRestorePoint and \
             restoreDatabase answer at once or with 503 database_not_ready and Retry-After, never \
             another status with that code (every answer of every case is checked for this too)",
            |cx| {
                let g = garden(&cx, "not ready").await?;
                let rp = cx
                    .ok(
                        Who::Member,
                        restore_point(&cx, &g.id, "before restores"),
                        &[201],
                    )
                    .await?;
                let rp_id = rp["restore_point"]["id"].clone();
                let in_place = json!({ "restore_point_id": rp_id, "target": "in_place" });
                for round in 0..3 {
                    cx.ok(Who::Member, restore(&cx, &g.id, in_place.clone()), &[202])
                        .await?;
                    let probes: Vec<(Who, RequestSpec, &[u16])> = vec![
                        (
                            Who::MemberView,
                            read(&cx, &g.id, "select count(*) from t1"),
                            &[200, 503],
                        ),
                        (Who::MemberView, list(&cx, &g.id, "/schema"), &[200, 503]),
                        (
                            Who::Member,
                            migration(
                                &cx,
                                &g.id,
                                &format!("000{}_round", round + 2),
                                &format!("create table r{round} (id integer primary key)"),
                            ),
                            &[200, 409, 503],
                        ),
                        (
                            Who::Member,
                            branch(&cx, &g.id, &format!("round {round}"), "masked"),
                            &[202, 409, 503],
                        ),
                        (
                            Who::MemberEdit,
                            restore_point(&cx, &g.id, &format!("round {round}")),
                            &[201, 409, 503],
                        ),
                        (
                            Who::Member,
                            restore(&cx, &g.id, in_place.clone()),
                            &[202, 409, 503],
                        ),
                    ];
                    let answers =
                        futures::future::join_all(probes.into_iter().map(|(who, req, ok)| {
                            let cx = cx.clone();
                            async move { cx.ok(who, req, ok).await }
                        }))
                        .await;
                    for a in answers {
                        a?;
                    }
                    wait_ready(&cx, &g.id).await?;
                }
                Ok(())
            }
        ),
        case!(
            "a_branch_of_a_branch_is_a_conflict",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "createDatabaseBranch",
                "listDatabaseBranches"
            ],
            "createDatabaseBranch: branches are flat children of a live database; a branch of a \
             branch is 409 conflict and makes no node",
            |cx| {
                let g = garden(&cx, "flat branches").await?;
                let b = written_id(
                    &cx.ok(Who::Member, branch(&cx, &g.id, "first", "masked"), &[202])
                        .await?,
                )?;
                let d = wait_ready(&cx, &b).await?;
                check!(
                    d["branch_of"] == json!(g.id) && d["branch_data"] == "masked",
                    "the branch: {d}"
                );
                cx.refused(
                    Who::Member,
                    branch(&cx, &b, "second", "masked"),
                    409,
                    &["conflict"],
                )
                .await?;
                check!(
                    count(&cx, &b, "/branches").await? == 0,
                    "the refused branch made a node"
                );
                check!(
                    count(&cx, &g.id, "/branches").await? == 1,
                    "listDatabaseBranches of the source"
                );
                Ok(())
            }
        ),
        case!(
            "restore_of_a_branch_before_it_was_ready",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "createDatabaseBranch",
                "restoreDatabase"
            ],
            "restoreDatabase: the window of a branch starts when it became ready; a restore of a \
             branch to an earlier time (the source's creation time) is 422 validation_failed",
            |cx| {
                let g = garden(&cx, "branch window").await?;
                let source = cx.ok(Who::Owner, get_database(&cx, &g.id), &[200]).await?;
                let early = source["node"]["created_at"].clone();
                check!(early.is_string(), "the source has no created_at: {source}");
                let b = written_id(
                    &cx.ok(
                        Who::Member,
                        branch(&cx, &g.id, "late branch", "masked"),
                        &[202],
                    )
                    .await?,
                )?;
                wait_ready(&cx, &b).await?;
                cx.refused(
                    Who::Member,
                    restore(
                        &cx,
                        &b,
                        json!({ "at": early, "target": "new_node", "name": "too early" }),
                    ),
                    422,
                    &["validation_failed"],
                )
                .await?;
                Ok(())
            }
        ),
    ]
}
