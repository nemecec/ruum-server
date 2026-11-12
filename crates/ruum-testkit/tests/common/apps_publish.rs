//! App cases of card W-3a: `publishAppDeployment` for people (publish and rollback), the
//! `202` of an agent's publish, and the app branch of `restoreVersion`. See [`super::apps`].

use super::apps::*;
use super::{Case, Ctx, Fail};
use crate::{case, check, fail};
use ruum_testkit::fixtures::Who;
use serde_json::{Value, json};

/// A live app with two releases (v1, then v2) made by `member` in `Projects`. Returns the app,
/// the first live deployment and the second one.
async fn two_releases(cx: &Ctx, name: &str) -> Result<(LiveApp, Value), Fail> {
    let v1 = live_app(cx, cx.a().projects, name, STATIC_MANIFEST).await?;
    let v2 = new_release(
        cx,
        &v1.id,
        cx.a().projects,
        &format!("{name} v2"),
        "runtime: static\nenv:\n  RELEASE: two\n",
    )
    .await?;
    Ok((v1, v2))
}

pub fn cases() -> Vec<Case> {
    vec![
        case!(
            "publish_makes_a_preview_live_and_writes_a_version",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "getApp",
                "listVersions",
                "listAppDeployments"
            ],
            "member publishes a ready preview: 200, the deployment becomes live with a \
             version_id and published_at; listVersions of the app node lists that version; \
             getApp names it as live_deployment and its status reaches ready; \
             listAppDeployments?environment=live lists it",
            |cx| {
                let live = live_app(&cx, cx.a().projects, "published", STATIC_MANIFEST).await?;
                check!(
                    live.live["published_at"].is_string(),
                    "published_at: {}",
                    live.live
                );
                let versions = cx
                    .ok(Who::Member, list_versions(&cx, &live.id), &[200])
                    .await?;
                check!(
                    items(&versions)
                        .iter()
                        .any(|v| v["id"] == live.version().as_str()),
                    "listVersions does not list the version {} of the publish: {versions}",
                    live.version()
                );
                let a = wait_live_is(&cx, &live.id, &live.deployment()).await?;
                let start = std::time::Instant::now();
                let mut a = a;
                while a["status"]["state"] != "ready" {
                    check!(
                        start.elapsed() < DEPLOY_WITHIN,
                        "the app status after a publish: {}",
                        a["status"]
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    a = cx.ok(Who::Member, get_app(&cx, &live.id), &[200]).await?;
                }
                let page = cx
                    .ok(
                        Who::Member,
                        list_deployments(&cx, &live.id, "?environment=live"),
                        &[200],
                    )
                    .await?;
                check!(
                    items(&page)
                        .iter()
                        .any(|d| d["id"] == live.deployment().as_str()),
                    "listAppDeployments does not list the live deployment: {page}"
                );
                Ok(())
            }
        ),
        case!(
            "an_agent_publish_waits_for_a_person",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "updateAgent",
                "getApp",
            ],
            "agent (scope Projects, max_role edit) makes a preview freely, then its \
             publishAppDeployment answers 202 with a pending approval for exactly that \
             deployment and app (AppApprovalRequired); nothing goes live (design 11.1, \
             decision 0006-4)",
            |cx| {
                super::api::set_scope(&cx, Who::Agent, Some(cx.a().projects), "edit").await?;
                let app = new_app(&cx, Who::Member, cx.a().projects, "asked").await?;
                let id = app_id(&app)?;
                let s = site(
                    &cx,
                    Who::Agent,
                    cx.a().projects,
                    "bot site",
                    STATIC_MANIFEST,
                )
                .await?;
                let p = preview(&cx, Who::Agent, &id, from_folder(&s.folder, None)).await?;
                let dep = str_at(&p, "/id")?;
                let body = cx
                    .ok(Who::Agent, publish(&cx, &id, &dep, json!({})), &[202])
                    .await?;
                let a = &body["approval"];
                check!(
                    a["status"] == "pending"
                        && a["deployment_id"] == dep.as_str()
                        && a["app_id"] == id.as_str()
                        && a["id"].is_string(),
                    "the 202 of an agent's publish: {body}"
                );
                let app = cx.ok(Who::Member, get_app(&cx, &id), &[200]).await?;
                check!(
                    app["live_deployment"].is_null(),
                    "the agent's publish went live: {app}"
                );
                let d = cx
                    .ok(Who::Member, get_deployment(&cx, &id, &dep, None), &[200])
                    .await?;
                check!(d["state"] == "ready", "the preview after the 202: {d}");
                Ok(())
            }
        ),
        case!(
            "an_agent_cannot_restore_an_app_version",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "updateAgent",
                "restoreVersion",
                "getApp"
            ],
            "agent (max_role edit) gets 403 permission_denied, hint publishAppDeployment, with \
             the deployment of that version in error.details.deployment_id, for restoreVersion \
             of an app version; the live deployment stays (restoreVersion, step 3)",
            |cx| {
                let (v1, v2) = two_releases(&cx, "kept").await?;
                super::api::set_scope(&cx, Who::Agent, Some(cx.a().projects), "edit").await?;
                let err = cx
                    .refused(
                        Who::Agent,
                        restore_version(&cx, &v1.id, v1.version()),
                        403,
                        &["permission_denied"],
                    )
                    .await?;
                super::api::hint_has(&err, "publishAppDeployment")?;
                check!(
                    err["details"]["deployment_id"] == v1.deployment().as_str(),
                    "error.details.deployment_id is not {}: {err}",
                    v1.deployment()
                );
                let a = cx.ok(Who::Member, get_app(&cx, &v1.id), &[200]).await?;
                check!(
                    a["live_deployment"]["id"] == v2["id"],
                    "the live deployment changed: {}",
                    a["live_deployment"]
                );
                Ok(())
            }
        ),
        case!(
            "a_person_restoring_an_app_version_rolls_back",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "restoreVersion",
                "getApp"
            ],
            "member's restoreVersion of the first release: 200 with a new head version \
             (restored_from = that version); the first release is live again and the second \
             one is superseded (restoreVersion: a rollback with the rules of \
             publishAppDeployment)",
            |cx| {
                let (v1, v2) = two_releases(&cx, "rolled").await?;
                let w = cx
                    .ok(
                        Who::Member,
                        restore_version(&cx, &v1.id, v1.version()),
                        &[200],
                    )
                    .await?;
                check!(
                    w["version"]["restored_from"] == v1.version().as_str(),
                    "restored_from: {w}"
                );
                // The first release is live again (the same deployment, or one with its
                // manifest).
                let start = std::time::Instant::now();
                loop {
                    let a = cx.ok(Who::Member, get_app(&cx, &v1.id), &[200]).await?;
                    let l = &a["live_deployment"];
                    if l["id"] == v1.deployment().as_str()
                        || (l["id"] != v2["id"] && l["manifest"] == v1.live["manifest"])
                    {
                        break;
                    }
                    check!(
                        start.elapsed() < DEPLOY_WITHIN,
                        "the first release is not live after the restore: {l}"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                let second = str_at(&v2, "/id")?;
                wait_state(&cx, Who::Member, &v1.id, &second, &["superseded"]).await?;
                Ok(())
            }
        ),
        case!(
            "publishing_an_older_live_deployment_rolls_back",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "getApp",
                "listVersions"
            ],
            "member publishes the superseded first release again: 200, it is live again and a \
             new version is written (publishAppDeployment: a rollback; no migration runs)",
            |cx| {
                let (v1, _v2) = two_releases(&cx, "back").await?;
                let before = items(
                    &cx.ok(Who::Member, list_versions(&cx, &v1.id), &[200])
                        .await?,
                )
                .len();
                let old = cx
                    .ok(
                        Who::Member,
                        get_deployment(&cx, &v1.id, v1.deployment(), None),
                        &[200],
                    )
                    .await?;
                check!(old["state"] == "superseded", "the first release: {old}");
                let p = cx
                    .ok(
                        Who::Member,
                        publish(&cx, &v1.id, v1.deployment(), json!({})),
                        &[200],
                    )
                    .await?;
                let id = str_at(&p, "/id")?;
                wait_state(&cx, Who::Member, &v1.id, &id, &["live"]).await?;
                wait_live_is(&cx, &v1.id, &id).await?;
                let after = items(
                    &cx.ok(Who::Member, list_versions(&cx, &v1.id), &[200])
                        .await?,
                )
                .len();
                check!(
                    after > before,
                    "a rollback wrote no version ({before} before, {after} after)"
                );
                Ok(())
            }
        ),
        case!(
            "rollback_across_a_migration_needs_force",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "getApp"
            ],
            "after a release whose migration ran, publishing an older deployment without force \
             is refused and with force: true goes live, keeping the database as it is \
             (publishAppDeployment); needs database bindings and migrations at publish",
            |cx| {
                let _ = &cx;
                fail!("written when D-2 lands: the manifest of a release with a migration")
            }
        ),
    ]
}
