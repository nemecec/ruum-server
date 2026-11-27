//! App cases of cards B-1 and B-1d: bindings (`docs/STEP3.md` rules 5 and 6, decisions 0006-3
//! and 0006-5). A folder binding is granted to the preview principal and,
//! at publish, to the live principal; a dropped binding loses its tuple; a superseded preview
//! and a trashed app lose theirs; an API binding passes its check and waits for card B-3, a
//! preview getting only the called API's `preview_scopes`. A database binding (card B-1d)
//! gives a preview a role on a masked branch of its own and the live app a role on the live
//! database; a SQLite database is refused. The tuples are read from the engine of the services
//! (memory or SpiceDB). See [`super::apps`].

use super::apps::*;
use super::{Case, Ctx, Fail};
use crate::{case, check};
use axum::http::Method;
use ruum_testkit::client::RequestSpec;
use ruum_testkit::fixtures::Who;
use serde_json::Value;
use std::time::{Duration, Instant};

fn uuid_of(s: &str) -> Result<uuid::Uuid, Fail> {
    s.parse()
        .map_err(|e| Fail::Failed(format!("the ID {s:?} is not a UUID: {e}")))
}

/// Whether the app principal `principal` (an app node ID or a preview's deployment ID) can
/// view `node` in the engine of the services.
async fn app_sees(cx: &Ctx, principal: &str, node: &str) -> Result<bool, Fail> {
    use ruum_core::authz::{ObjectRef, SubjectRef};
    cx.state
        .services
        .authz
        .check(
            &SubjectRef::object(ObjectRef::new("app", uuid_of(principal)?)),
            "view",
            &ObjectRef::node(ruum_core::NodeId(uuid_of(node)?)),
            None,
        )
        .await
        .map_err(|e| Fail::Failed(format!("view check: {e}")))
}

/// Waits (at most [`DEPLOY_WITHIN`]) until `principal` sees `node` as `want`.
async fn wait_sees(cx: &Ctx, principal: &str, node: &str, want: bool) -> Result<(), Fail> {
    let start = Instant::now();
    loop {
        if app_sees(cx, principal, node).await? == want {
            return Ok(());
        }
        check!(
            start.elapsed() < DEPLOY_WITHIN,
            "app:{principal} view on {node} is not {want} after {DEPLOY_WITHIN:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The `app.binding_revoked` entries on the app node (the journal of the services).
async fn revoked(cx: &Ctx, app: &str) -> Result<Vec<ruum_core::journal::JournalEntry>, Fail> {
    let ctx = ruum_core::actor::Context {
        tenant: cx.a().id,
        actor: ruum_core::actor::Actor::user(cx.a().user_id(Who::Member)),
        scope: None,
        idempotency_key: None,
    };
    cx.state
        .services
        .journal
        .node_entries(
            &ctx,
            ruum_core::NodeId(uuid_of(app)?),
            i64::MAX,
            &[ruum_core::journal::EventType::AppBindingRevoked],
            50,
        )
        .await
        .map_err(|e| Fail::Failed(format!("journal read: {e}")))
}

/// The binding rows (active or not) on `target` in tenant A.
async fn rows_on(cx: &Ctx, target: &str) -> Result<Vec<ruum_core::apps::store::BindingRow>, Fail> {
    cx.state
        .services
        .apps
        .store()
        .list_bindings(
            cx.a().id,
            &ruum_core::apps::store::BindingQuery {
                target: Some(ruum_core::NodeId(uuid_of(target)?)),
                limit: 100,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| Fail::Failed(format!("binding rows: {e}")))
}

/// A manifest with the node binding `docs` (`view`) to `node`.
fn bound(node: &str) -> String {
    format!("runtime: static\nbindings:\n  docs:\n    node: \"{node}\"\n    role: view\n")
}

/// A manifest with the database binding `shop` (`role`) to `node`.
fn db_bound(node: &str, role: &str) -> String {
    format!("runtime: static\nbindings:\n  shop:\n    node: \"node:{node}\"\n    role: {role}\n")
}

/// A manifest with the API binding `shop` to the fixture app's API `orders`.
fn api_bound(cx: &Ctx, scopes: &str) -> String {
    format!(
        "runtime: static\nbindings:\n  shop:\n    node: node:{}\n    api: orders\n    scopes: {scopes}\n",
        cx.a().app
    )
}

/// The state of stage `name` of a deployment.
fn stage_state(d: &Value, name: &str) -> String {
    d["stages"]
        .as_array()
        .and_then(|a| a.iter().find(|s| s["name"] == name))
        .and_then(|s| s["state"].as_str())
        .unwrap_or_default()
        .to_string()
}

pub fn cases() -> Vec<Case> {
    vec![
        case!(
            "folder_binding_is_granted_to_the_preview_then_to_the_live_app",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment"
            ],
            "member deploys an app in Projects with the binding docs (view, the relative path \
             ../<folder>): the preview is ready with the stage bindings done, app:<preview> can \
             view the folder and app:<app> cannot; after the publish app:<app> can view it and \
             the preview keeps its own grant (design 9.4, decision 0006-5)",
            |cx| {
                let app = app_id(&new_app(&cx, Who::Member, cx.a().projects, "bound app").await?)?;
                let docs = new_folder(&cx, Who::Member, cx.a().projects, "bound docs").await?;
                let s = site(
                    &cx,
                    Who::Member,
                    cx.a().projects,
                    "bound site",
                    &bound("../bound docs"),
                )
                .await?;
                let p = preview(&cx, Who::Member, &app, from_folder(&s.folder, None)).await?;
                let pid = str_at(&p, "/id")?;
                check!(
                    stage_state(&p, "bindings") == "done",
                    "stage bindings: {}",
                    p["stages"]
                );
                check!(
                    app_sees(&cx, &pid, &docs).await?,
                    "the preview principal cannot view its bound folder"
                );
                check!(
                    !app_sees(&cx, &app, &docs).await?,
                    "the live principal got the preview's binding"
                );
                go_live(&cx, Who::Member, &app, &pid).await?;
                check!(
                    app_sees(&cx, &app, &docs).await?,
                    "the live principal cannot view the bound folder after the publish"
                );
                check!(
                    app_sees(&cx, &pid, &docs).await?,
                    "the preview lost its own grant at the publish"
                );
                Ok(())
            }
        ),
        case!(
            "publish_that_drops_a_binding_removes_its_tuple",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment"
            ],
            "a live app with the binding docs (view on a folder) publishes a version without \
             it: app:<app> can no longer view the folder (an engine check, not only the row), \
             and the journal has app.binding_revoked, reason dropped, with the publisher as \
             actor (design 9.4)",
            |cx| {
                let docs = new_folder(&cx, Who::Member, cx.a().projects, "dropped docs").await?;
                let live = live_app(
                    &cx,
                    cx.a().projects,
                    "dropper",
                    &bound(&format!("node:{docs}")),
                )
                .await?;
                check!(
                    app_sees(&cx, &live.id, &docs).await?,
                    "the live principal cannot view the bound folder"
                );
                new_release(
                    &cx,
                    &live.id,
                    cx.a().projects,
                    "dropper v2",
                    STATIC_MANIFEST,
                )
                .await?;
                check!(
                    !app_sees(&cx, &live.id, &docs).await?,
                    "the dropped binding still gives view"
                );
                let entries = revoked(&cx, &live.id).await?;
                check!(
                    entries.iter().any(|e| e.data["reason"] == "dropped"
                        && e.data["principal_id"] == live.id.as_str()
                        && e.actor.actor.id.0 == cx.a().user_id(Who::Member).0),
                    "no app.binding_revoked (dropped, by member) for the live principal: {:?}",
                    entries.iter().map(|e| &e.data).collect::<Vec<_>>()
                );
                Ok(())
            }
        ),
        case!(
            "superseded_preview_loses_its_binding",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment"
            ],
            "two previews with the draft label main and the same folder binding: once the second \
             is ready the first is superseded and app:<first> can no longer view the folder; \
             app:<second> can, and app:<app> never could (decision 0006-5)",
            |cx| {
                let app = app_id(&new_app(&cx, Who::Member, cx.a().projects, "drafty").await?)?;
                let docs = new_folder(&cx, Who::Member, cx.a().projects, "draft docs").await?;
                let s = site(
                    &cx,
                    Who::Member,
                    cx.a().projects,
                    "drafty site",
                    &bound(&format!("node:{docs}")),
                )
                .await?;
                let first = str_at(
                    &preview(&cx, Who::Member, &app, from_folder(&s.folder, Some("main"))).await?,
                    "/id",
                )?;
                check!(
                    app_sees(&cx, &first, &docs).await?,
                    "the first preview cannot view its folder"
                );
                let second = str_at(
                    &preview(&cx, Who::Member, &app, from_folder(&s.folder, Some("main"))).await?,
                    "/id",
                )?;
                wait_sees(&cx, &first, &docs, false).await?;
                check!(
                    app_sees(&cx, &second, &docs).await?,
                    "the second preview cannot view its folder"
                );
                check!(
                    !app_sees(&cx, &app, &docs).await?,
                    "the live principal got a preview's binding"
                );
                Ok(())
            }
        ),
        case!(
            "app_trash_revokes_its_bindings",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "trashNode"
            ],
            "a live app with a folder binding goes to the trash: once the provider's trash hook \
             ran, app:<app> can no longer view the folder and the journal has \
             app.binding_revoked, reason app_trashed, with the app as actor (review R14)",
            |cx| {
                let docs = new_folder(&cx, Who::Member, cx.a().projects, "trash docs").await?;
                let live = live_app(
                    &cx,
                    cx.a().projects,
                    "trashed app",
                    &bound(&format!("node:{docs}")),
                )
                .await?;
                check!(
                    app_sees(&cx, &live.id, &docs).await?,
                    "the live principal cannot view the bound folder"
                );
                cx.ok(
                    Who::Member,
                    RequestSpec::post(format!("/v1/nodes/{}/trash", live.id)).tenant(cx.a().id),
                    &[200],
                )
                .await?;
                // The server runs the hook deliveries as a task; the suite runs a round.
                let worker = ruum_core::providers::deliveries::DeliveryWorker::new(
                    cx.state.services.typed_nodes.clone(),
                    cx.state.services.providers.clone(),
                    ruum_core::providers::deliveries::Backoff::default(),
                );
                worker
                    .run_once(ruum_core::providers::deliveries::DeliveryWorker::ROUND_LIMIT)
                    .await
                    .map_err(|e| Fail::Failed(format!("the hook deliveries failed: {e}")))?;
                wait_sees(&cx, &live.id, &docs, false).await?;
                let entries = revoked(&cx, &live.id).await?;
                check!(
                    entries.iter().any(|e| e.data["reason"] == "app_trashed"
                        && e.actor.actor.id.0.to_string() == live.id),
                    "no app.binding_revoked (app_trashed, by the app): {:?}",
                    entries.iter().map(|e| &e.data).collect::<Vec<_>>()
                );
                Ok(())
            }
        ),
        case!(
            "purge_of_a_bound_node_succeeds_and_the_repair_ends_its_rows",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "trashNode",
                "purgeNode",
                "emptyTrash",
                "listTrash"
            ],
            "a folder with an active and a revoked binding row is trashed and purged \
             (purgeNode 200), and a folder above a file bound by a preview is trashed and \
             purged by emptyTrash (200, every root): the purge does not stop at the rows; one \
             round of the repair pass ends the active rows with target_gone and the app \
             principals have no permission (review-b1 1)",
            |cx| {
                // A folder with a revoked row (the live app drops the binding) and an active
                // row (a preview of another app).
                let docs = new_folder(&cx, Who::Member, cx.a().projects, "purged docs").await?;
                let live = live_app(
                    &cx,
                    cx.a().projects,
                    "purge dropper",
                    &bound(&format!("node:{docs}")),
                )
                .await?;
                new_release(
                    &cx,
                    &live.id,
                    cx.a().projects,
                    "purge dropper v2",
                    STATIC_MANIFEST,
                )
                .await?;
                let other =
                    app_id(&new_app(&cx, Who::Member, cx.a().projects, "purge viewer").await?)?;
                let s = site(
                    &cx,
                    Who::Member,
                    cx.a().projects,
                    "purge viewer site",
                    &bound(&format!("node:{docs}")),
                )
                .await?;
                let pid = str_at(
                    &preview(&cx, Who::Member, &other, from_folder(&s.folder, None)).await?,
                    "/id",
                )?;
                check!(
                    app_sees(&cx, &pid, &docs).await?,
                    "the preview cannot view the folder"
                );
                let rows = rows_on(&cx, &docs).await?;
                check!(
                    rows.iter().any(|b| b.is_active()) && rows.iter().any(|b| !b.is_active()),
                    "the folder needs an active and a revoked row: {rows:?}"
                );
                // A folder above a file that a preview binds.
                let above = new_folder(&cx, Who::Member, cx.a().projects, "purged above").await?;
                let inner = site(&cx, Who::Member, &above, "inner", STATIC_MANIFEST).await?;
                let file = inner.files[1].1.clone();
                let s2 = site(
                    &cx,
                    Who::Member,
                    cx.a().projects,
                    "purge file site",
                    &bound(&format!("node:{file}")),
                )
                .await?;
                let pid2 = str_at(
                    &preview(&cx, Who::Member, &other, from_folder(&s2.folder, None)).await?,
                    "/id",
                )?;
                check!(
                    app_sees(&cx, &pid2, &file).await?,
                    "the preview cannot view the file"
                );
                for node in [&docs, &above] {
                    cx.ok(
                        Who::Owner,
                        RequestSpec::post(format!("/v1/nodes/{node}/trash")).tenant(cx.a().id),
                        &[200],
                    )
                    .await?;
                }
                cx.ok(
                    Who::Owner,
                    RequestSpec::new(Method::DELETE, format!("/v1/nodes/{docs}")).tenant(cx.a().id),
                    &[200],
                )
                .await?;
                let emptied = cx
                    .ok(
                        Who::Owner,
                        RequestSpec::post("/v1/trash/empty").tenant(cx.a().id),
                        &[200],
                    )
                    .await?;
                check!(
                    emptied["purged"].as_u64().unwrap_or(0) >= 1,
                    "emptyTrash purged nothing: {emptied}"
                );
                let trash = cx
                    .ok(
                        Who::Owner,
                        RequestSpec::get("/v1/trash").tenant(cx.a().id),
                        &[200],
                    )
                    .await?;
                check!(
                    !trash.to_string().contains(&above),
                    "the folder above the bound file is still in the trash: {trash}"
                );
                cx.state
                    .services
                    .apps
                    .repair_bindings(
                        cx.a().id,
                        None,
                        ruum_core::apps::bindings::REPAIR_TARGETS_PER_TENANT,
                    )
                    .await
                    .map_err(|e| Fail::Failed(format!("the repair pass failed: {e}")))?;
                for (target, principal) in [(&docs, &pid), (&file, &pid2)] {
                    let rows = rows_on(&cx, target).await?;
                    check!(
                        !rows.is_empty() && rows.iter().all(|b| !b.is_active()),
                        "rows on the purged {target} stay active: {rows:?}"
                    );
                    check!(
                        rows.iter().any(|b| b.revoke_reason
                            == Some(ruum_core::apps::store::RevokeReason::TargetGone)),
                        "no row on {target} ended with target_gone: {rows:?}"
                    );
                    check!(
                        !app_sees(&cx, principal, target).await?,
                        "app:{principal} still views the purged {target}"
                    );
                }
                Ok(())
            }
        ),
        case!(
            "database_binding_previews_on_a_masked_branch_then_binds_the_live_database",
            [
                "createDatabase",
                "getDatabase",
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment"
            ],
            "member binds a postgres database (edit) as shop: the preview is ready with the \
             stage bindings done; its row is database_branch on a branch of the database \
             (getDatabase: branch_of the database, branch_data masked) that app:<preview> can \
             edit, and app:<preview> has no right on the live database; after the publish \
             app:<app> can edit the live database; a release without the binding takes that \
             away (card B-1d, decision 0006-5)",
            |cx| {
                let db = super::api::new_database(
                    &cx,
                    Who::Member,
                    cx.a().projects,
                    "bound shop db",
                    super::api::Engine::Postgres,
                )
                .await?;
                let app = app_id(&new_app(&cx, Who::Member, cx.a().projects, "db app").await?)?;
                let s = site(
                    &cx,
                    Who::Member,
                    cx.a().projects,
                    "db site",
                    &db_bound(&db, "edit"),
                )
                .await?;
                let p = preview(&cx, Who::Member, &app, from_folder(&s.folder, None)).await?;
                let pid = str_at(&p, "/id")?;
                check!(
                    stage_state(&p, "bindings") == "done",
                    "stage bindings: {}",
                    p["stages"]
                );
                let rows: Vec<_> = cx
                    .state
                    .services
                    .apps
                    .store()
                    .list_bindings(
                        cx.a().id,
                        &ruum_core::apps::store::BindingQuery {
                            principal: Some(uuid_of(&pid)?),
                            active_only: true,
                            limit: 10,
                            ..Default::default()
                        },
                    )
                    .await
                    .map_err(|e| Fail::Failed(format!("binding rows: {e}")))?;
                check!(rows.len() == 1, "the preview's rows: {rows:?}");
                check!(
                    rows[0].kind == ruum_core::apps::store::BindingKind::DatabaseBranch,
                    "not a database_branch row: {rows:?}"
                );
                let branch = rows[0].target_node_id.to_string();
                check!(branch != db, "the preview is bound to the live database");
                let b = cx
                    .ok(Who::Member, super::api::get_database(&cx, &branch), &[200])
                    .await?;
                check!(
                    b["branch_of"] == db.as_str() && b["branch_data"] == "masked",
                    "the preview's branch: {b}"
                );
                check!(
                    app_sees(&cx, &pid, &branch).await?,
                    "app:<preview> cannot reach its branch"
                );
                check!(
                    !app_sees(&cx, &pid, &db).await?,
                    "app:<preview> reaches the live database"
                );
                go_live(&cx, Who::Member, &app, &pid).await?;
                check!(
                    app_sees(&cx, &app, &db).await?,
                    "app:<app> cannot reach the live database after the publish"
                );
                let none = site(
                    &cx,
                    Who::Member,
                    cx.a().projects,
                    "db site without",
                    "runtime: static\n",
                )
                .await?;
                let p2 = preview(&cx, Who::Member, &app, from_folder(&none.folder, None)).await?;
                go_live(&cx, Who::Member, &app, &str_at(&p2, "/id")?).await?;
                check!(
                    !app_sees(&cx, &app, &db).await?,
                    "app:<app> keeps the live database after a release without the binding"
                );
                Ok(())
            }
        ),
        case!(
            "database_binding_of_a_sqlite_database_is_refused",
            [
                "createDatabase",
                "getDatabase",
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "listAppDeployments"
            ],
            "member binds a SQLite database: 422 validation_failed at /bindings/shop/node, with \
             no deployment left (an app cannot bind a SQLite database yet; backlog D5-2)",
            |cx| {
                let db = super::api::new_database(
                    &cx,
                    Who::Member,
                    cx.a().projects,
                    "bound lite db",
                    super::api::Engine::Sqlite,
                )
                .await?;
                let app = app_id(&new_app(&cx, Who::Member, cx.a().projects, "lite app").await?)?;
                let s = site(
                    &cx,
                    Who::Member,
                    cx.a().projects,
                    "lite site",
                    &db_bound(&db, "view"),
                )
                .await?;
                let err = cx
                    .refused(
                        Who::Member,
                        create_deployment(&cx, &app, from_folder(&s.folder, None)),
                        422,
                        &["validation_failed"],
                    )
                    .await?;
                check!(
                    err["details"]["pointer"] == "/bindings/shop/node",
                    "error.details: {err}"
                );
                no_new_deployment(&cx, &app, &[], "a SQLite binding").await
            }
        ),
        case!(
            "api_binding_passes_its_check_then_waits_for_b3",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "listAppDeployments"
            ],
            "member (manage on the fixture app W) deploys an app with the binding shop to W's \
             API orders, scope orders.read: the grant check passes, and the answer is 409 \
             conflict, hint bindings_not_ready, error.details.binding = shop, with no deployment \
             left (API bindings come with card B-3)",
            |cx| {
                let app = app_id(&new_app(&cx, Who::Member, cx.a().projects, "caller").await?)?;
                let s = site(
                    &cx,
                    Who::Member,
                    cx.a().projects,
                    "caller site",
                    &api_bound(&cx, "[orders.read]"),
                )
                .await?;
                let err = cx
                    .refused(
                        Who::Member,
                        create_deployment(&cx, &app, from_folder(&s.folder, None)),
                        409,
                        &["conflict"],
                    )
                    .await?;
                super::api::hint_has(&err, "bindings_not_ready")?;
                check!(
                    err["details"]["binding"] == "shop"
                        && err["details"]["scopes"] == serde_json::json!(["orders.read"]),
                    "error.details: {err}"
                );
                no_new_deployment(&cx, &app, &[], "an API binding").await
            }
        ),
        case!(
            "preview_api_binding_gets_only_preview_scopes",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "listAppDeployments"
            ],
            "a preview's API binding to W's orders with the scopes orders.read and orders.write \
             gets only orders.read, the API's preview_scopes (capped, not refused; decision \
             0006-5): the 409 bindings_not_ready names the granted scopes in \
             error.details.scopes",
            |cx| {
                let app = app_id(&new_app(&cx, Who::Member, cx.a().projects, "wide").await?)?;
                let s = site(
                    &cx,
                    Who::Member,
                    cx.a().projects,
                    "wide site",
                    &api_bound(&cx, "[orders.read, orders.write]"),
                )
                .await?;
                let err = cx
                    .refused(
                        Who::Member,
                        create_deployment(&cx, &app, from_folder(&s.folder, None)),
                        409,
                        &["conflict"],
                    )
                    .await?;
                super::api::hint_has(&err, "bindings_not_ready")?;
                check!(
                    err["details"]["scopes"] == serde_json::json!(["orders.read"]),
                    "a preview got more than the preview_scopes: {err}"
                );
                no_new_deployment(&cx, &app, &[], "a wide API binding").await
            }
        ),
    ]
}
