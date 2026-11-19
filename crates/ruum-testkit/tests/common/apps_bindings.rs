//! App cases of card B-1: bindings without database bindings (`docs/STEP3.md` rules 5 and 6,
//! decisions 0006-3 and 0006-5). A folder binding is granted to the preview principal and,
//! at publish, to the live principal; a dropped binding loses its tuple; a superseded preview
//! and a trashed app lose theirs; an API binding passes its check and waits for card B-3, a
//! preview getting only the called API's `preview_scopes`. The tuples are read from the
//! engine of the services (memory or SpiceDB). See [`super::apps`].

use super::apps::*;
use super::{Case, Ctx, Fail};
use crate::{case, check};
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

/// A manifest with the node binding `docs` (`view`) to `node`.
fn bound(node: &str) -> String {
    format!("runtime: static\nbindings:\n  docs:\n    node: \"{node}\"\n    role: view\n")
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
