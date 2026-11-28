//! App cases of card W-3b: the approvals of agents' publishes through the API
//! (`listAppApprovals`, `getAppApproval` with `wait`, `decideAppApproval`, and
//! `publishAppDeployment` with `approval_id`). See [`super::apps`].

use super::apps::*;
use super::{Case, Ctx, Fail};
use crate::{case, check};
use ruum_testkit::client::RequestSpec;
use ruum_testkit::fixtures::Who;
use serde_json::{Value, json};

fn list_approvals(cx: &Ctx, app: &str, query: &str) -> RequestSpec {
    RequestSpec::get(app_path(app, &format!("/approvals{query}"))).tenant(cx.a().id)
}

fn get_approval(cx: &Ctx, app: &str, id: &str, query: &str) -> RequestSpec {
    RequestSpec::get(app_path(app, &format!("/approvals/{id}{query}"))).tenant(cx.a().id)
}

fn decide(cx: &Ctx, app: &str, id: &str, body: Value) -> RequestSpec {
    RequestSpec::post(app_path(app, &format!("/approvals/{id}/decision")))
        .tenant(cx.a().id)
        .json(body)
}

/// An app of `member` in `Projects` and a ready preview of it made by `agent` (scope
/// `Projects`, `edit`), then the agent's publish request. Returns the app, the deployment and
/// the request's ID.
async fn asked(cx: &Ctx, name: &str) -> Result<(String, String, String), Fail> {
    super::api::set_scope(cx, Who::Agent, Some(cx.a().projects), "edit").await?;
    let app = new_app(cx, Who::Member, cx.a().projects, name).await?;
    let id = app_id(&app)?;
    let s = site(
        cx,
        Who::Agent,
        cx.a().projects,
        &format!("{name} site"),
        STATIC_MANIFEST,
    )
    .await?;
    let p = preview(cx, Who::Agent, &id, from_folder(&s.folder, None)).await?;
    let dep = str_at(&p, "/id")?;
    let body = cx
        .ok(Who::Agent, publish(cx, &id, &dep, json!({})), &[202])
        .await?;
    let approval = str_at(&body, "/approval/id")?;
    Ok((id, dep, approval))
}

/// `409 conflict` with the hint `approval_stale`.
async fn stale(cx: &Ctx, who: Who, req: RequestSpec) -> Result<(), Fail> {
    let err = cx.refused(who, req, 409, &["conflict"]).await?;
    super::api::hint_has(&err, "approval_stale")
}

pub fn cases() -> Vec<Case> {
    vec![
        case!(
            "an_approved_agent_publish_goes_live_once",
            [
                "updateAgent",
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "decideAppApproval",
                "getAppApproval",
                "listAppApprovals",
                "getApp"
            ],
            "the agent's publish with approval_id is 409 approval_stale while the request is \
             pending; the agent's user (member) approves (200, status approved); the same call \
             with approval_id goes live (200), the approval is used, and a second use is 409 \
             approval_stale (design 11.1, decision 0006-4)",
            |cx| {
                let (app, dep, id) = asked(&cx, "approved").await?;
                let with_id = json!({ "approval_id": id });
                stale(&cx, Who::Agent, publish(&cx, &app, &dep, with_id.clone())).await?;
                let v = cx
                    .ok(
                        Who::Member,
                        decide(&cx, &app, &id, json!({ "decision": "approve" })),
                        &[200],
                    )
                    .await?;
                check!(v["status"] == "approved", "the decision: {v}");
                let seen = cx
                    .ok(Who::Agent, get_approval(&cx, &app, &id, ""), &[200])
                    .await?;
                check!(seen["status"] == "approved", "the agent reads: {seen}");
                let live = cx
                    .ok(
                        Who::Agent,
                        publish(&cx, &app, &dep, with_id.clone()),
                        &[200],
                    )
                    .await?;
                let live_id = str_at(&live, "/id")?;
                wait_live_is(&cx, &app, &live_id).await?;
                let used = cx
                    .ok(
                        Who::Member,
                        list_approvals(&cx, &app, "?status=used"),
                        &[200],
                    )
                    .await?;
                check!(
                    items(&used).iter().any(|a| a["id"] == id.as_str()),
                    "listAppApprovals?status=used does not list the used approval: {used}"
                );
                stale(&cx, Who::Agent, publish(&cx, &app, &dep, with_id)).await?;
                let a = cx.ok(Who::Member, get_app(&cx, &app), &[200]).await?;
                check!(
                    a["live_deployment"]["id"] == live_id.as_str(),
                    "the live deployment after a second use: {}",
                    a["live_deployment"]
                );
                Ok(())
            }
        ),
        case!(
            "an_approval_is_only_for_the_agent_that_asked",
            [
                "updateAgent",
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "decideAppApproval",
                "getAppApproval",
                "listAppApprovals",
                "getApp"
            ],
            "another agent of the same user (agent_out, scope Projects edit) neither reads \
             (404), lists nor uses (409 approval_stale) agent's approved request; agent cannot \
             use it for another deployment either; nothing goes live (decision 0006-4, \
             provisional W3B-D2)",
            |cx| {
                let (app, dep, id) = asked(&cx, "own").await?;
                super::api::set_scope(&cx, Who::AgentOut, Some(cx.a().projects), "edit").await?;
                cx.ok(
                    Who::Member,
                    decide(&cx, &app, &id, json!({ "decision": "approve" })),
                    &[200],
                )
                .await?;
                let with_id = json!({ "approval_id": id });
                stale(
                    &cx,
                    Who::AgentOut,
                    publish(&cx, &app, &dep, with_id.clone()),
                )
                .await?;
                cx.refused(
                    Who::AgentOut,
                    get_approval(&cx, &app, &id, ""),
                    404,
                    &["not_found"],
                )
                .await?;
                let theirs = cx
                    .ok(Who::AgentOut, list_approvals(&cx, &app, ""), &[200])
                    .await?;
                check!(
                    items(&theirs).is_empty(),
                    "agent_out lists agent's request: {theirs}"
                );
                let mine = cx
                    .ok(Who::Agent, list_approvals(&cx, &app, ""), &[200])
                    .await?;
                check!(
                    items(&mine).iter().any(|a| a["id"] == id.as_str()),
                    "agent does not list its request: {mine}"
                );
                let s = site(
                    &cx,
                    Who::Agent,
                    cx.a().projects,
                    "own other",
                    STATIC_MANIFEST,
                )
                .await?;
                let other = preview(&cx, Who::Agent, &app, from_folder(&s.folder, None)).await?;
                let other = str_at(&other, "/id")?;
                stale(&cx, Who::Agent, publish(&cx, &app, &other, with_id)).await?;
                let a = cx.ok(Who::Member, get_app(&cx, &app), &[200]).await?;
                check!(a["live_deployment"].is_null(), "something went live: {a}");
                Ok(())
            }
        ),
        case!(
            "deciding_an_agent_publish_follows_its_rules",
            [
                "updateAgent",
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "decideAppApproval",
                "getAppApproval"
            ],
            "an agent gets 403 user_required; member_edit (edit on the app, not the agent's \
             user, no manage) gets 403; a comment of 501 characters is 422; member denies (200, \
             denied), a second decision is 409, and the agent's publish with the denied \
             request is 409 approval_stale (decideAppApproval)",
            |cx| {
                let (app, dep, id) = asked(&cx, "decided").await?;
                let approve = json!({ "decision": "approve" });
                cx.refused(
                    Who::Agent,
                    decide(&cx, &app, &id, approve.clone()),
                    403,
                    &["user_required"],
                )
                .await?;
                cx.refused(
                    Who::MemberEdit,
                    decide(&cx, &app, &id, approve.clone()),
                    403,
                    &["permission_denied"],
                )
                .await?;
                cx.refused(
                    Who::Member,
                    decide(
                        &cx,
                        &app,
                        &id,
                        json!({ "decision": "approve", "comment": "x".repeat(501) }),
                    ),
                    422,
                    &["validation_failed"],
                )
                .await?;
                let v = cx
                    .ok(
                        Who::Member,
                        decide(
                            &cx,
                            &app,
                            &id,
                            json!({ "decision": "deny", "comment": "no" }),
                        ),
                        &[200],
                    )
                    .await?;
                check!(
                    v["status"] == "denied" && v["comment"] == "no",
                    "the denial: {v}"
                );
                cx.refused(
                    Who::Member,
                    decide(&cx, &app, &id, approve),
                    409,
                    &["conflict"],
                )
                .await?;
                stale(
                    &cx,
                    Who::Agent,
                    publish(&cx, &app, &dep, json!({ "approval_id": id })),
                )
                .await?;
                Ok(())
            }
        ),
        case!(
            "get_app_approval_waits_for_the_decision",
            [
                "updateAgent",
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "decideAppApproval",
                "getAppApproval"
            ],
            "getAppApproval?wait=20 as the agent answers as soon as member approves, well \
             before the wait ends, with the status approved (getAppApproval, Wait)",
            |cx| {
                let (app, _, id) = asked(&cx, "waited").await?;
                let start = std::time::Instant::now();
                let waiting = cx.ok(Who::Agent, get_approval(&cx, &app, &id, "?wait=20"), &[200]);
                let deciding = async {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    cx.ok(
                        Who::Member,
                        decide(&cx, &app, &id, json!({ "decision": "approve" })),
                        &[200],
                    )
                    .await
                };
                let (seen, decided) = tokio::join!(waiting, deciding);
                decided?;
                let seen = seen?;
                check!(
                    seen["status"] == "approved",
                    "the long poll answered: {seen}"
                );
                check!(
                    start.elapsed() < std::time::Duration::from_secs(15),
                    "the long poll waited {:?}",
                    start.elapsed()
                );
                Ok(())
            }
        ),
    ]
}
