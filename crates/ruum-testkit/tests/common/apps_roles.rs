//! App cases of card B-2: `listAppRoles`, `listAppRoleGrants`, `addAppRoleGrant` and
//! `removeAppRoleGrant`; the active flag follows the live manifest. See [`super::apps`].
//!
//! The app of these cases has [`ROLES_MANIFEST`]: `member` (default), `approver` and `admin`
//! (`grants_roles`). `guest` has no grant on any node, so whatever it may do with app roles
//! comes from its app role alone.

use super::apps::*;
use super::{Case, Ctx, Fail};
use crate::{case, check};
use ruum_testkit::fixtures::Who;
use serde_json::Value;

/// The roles of `listAppRoles`.
async fn roles(cx: &Ctx, app: &str) -> Result<Vec<Value>, Fail> {
    Ok(items(
        &cx.ok(Who::Member, list_roles(cx, app), &[200]).await?,
    ))
}

/// Gives `role` to `grantee` as `by` (`addAppRoleGrant`, `201`) and returns the grant ID.
async fn grant(cx: &Ctx, by: Who, app: &str, role: &str, grantee: Who) -> Result<String, Fail> {
    let g = cx
        .ok(by, add_role_grant(cx, app, role, grantee), &[201])
        .await?;
    check!(
        g["role"] == role && g["grantee"]["id"] == cx.p(grantee).id.to_string().as_str(),
        "the grant of {role} to {}: {g}",
        grantee.name()
    );
    str_at(&g, "/id")
}

/// The grant IDs of `app` (`listAppRoleGrants` as `member`, the first page), sorted.
async fn grant_ids(cx: &Ctx, app: &str) -> Result<Vec<String>, Fail> {
    let page = cx
        .ok(Who::Member, list_role_grants(cx, app, ""), &[200])
        .await?;
    let mut ids: Vec<String> = items(&page)
        .iter()
        .filter_map(|g| g["id"].as_str().map(str::to_string))
        .collect();
    ids.sort();
    Ok(ids)
}

/// Fails when the grants of `app` are not `before`: a refused grant change leaves nothing
/// behind (a handler that writes and then answers `403` fails here).
async fn grants_unchanged(cx: &Ctx, app: &str, before: &[String], what: &str) -> Result<(), Fail> {
    let after = grant_ids(cx, app).await?;
    check!(
        after == before,
        "{what}: the grants changed from {before:?} to {after:?}"
    );
    Ok(())
}

/// `use` of `app` for the user `user` in the engine of the services (the gateway's check).
async fn uses(cx: &Ctx, user: uuid::Uuid, app: &str) -> Result<bool, Fail> {
    use ruum_core::authz::{ObjectRef, SubjectRef};
    let node: uuid::Uuid = app
        .parse()
        .map_err(|e| Fail::Failed(format!("app ID {app}: {e}")))?;
    cx.state
        .services
        .authz
        .check(
            &SubjectRef::user(ruum_core::UserId(user)),
            "use",
            &ObjectRef::node(ruum_core::NodeId(node)),
            None,
        )
        .await
        .map_err(|e| Fail::Failed(format!("use check: {e}")))
}

/// The grantee IDs of the user grants of `app` (`listAppRoleGrants` as `member`).
async fn user_grantees(cx: &Ctx, app: &str) -> Result<Vec<String>, Fail> {
    let page = cx
        .ok(Who::Member, list_role_grants(cx, app, ""), &[200])
        .await?;
    Ok(items(&page)
        .iter()
        .filter(|g| g["grantee"]["type"] == "user")
        .filter_map(|g| g["grantee"]["id"].as_str().map(str::to_string))
        .collect())
}

pub fn cases() -> Vec<Case> {
    vec![
        case!(
            "a_removed_member_keeps_no_app_role",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "addAppRoleGrant",
                "listAppRoleGrants",
                "removeMember"
            ],
            "an app in Private (no grants): guest_view and guest hold approver, so both may use \
             it; owner removes guest_view (removeMember, 204): guest_view's grants of this app and \
             of W are gone, guest_view has no use and no role, and its agent \
             (agent_of_guest_view) gets no grant list (401, 403 or 404); guest keeps its grant \
             and use, and tenant B's grant to its guest_view stays (backlog S-43)",
            |cx| {
                let live = live_app(&cx, cx.a().private, "left", ROLES_MANIFEST).await?;
                grant(&cx, Who::Member, &live.id, "approver", Who::GuestView).await?;
                grant(&cx, Who::Member, &live.id, "approver", Who::Guest).await?;
                let gv = cx.p(Who::GuestView).id;
                let guest = cx.p(Who::Guest).id;
                check!(
                    uses(&cx, gv, &live.id).await? && uses(&cx, guest, &live.id).await?,
                    "approver gives use before the removal"
                );
                let w = cx.a().app.to_string();
                check!(
                    user_grantees(&cx, &w).await?.contains(&gv.to_string()),
                    "the seeded approver grant of guest_view on W is missing"
                );
                let removal = ruum_testkit::client::RequestSpec::new(
                    axum::http::Method::DELETE,
                    format!("/v1/tenants/{}/members/{gv}", cx.a().id),
                );
                cx.ok(Who::Owner, removal, &[204]).await?;
                for app in [live.id.as_str(), w.as_str()] {
                    let left = user_grantees(&cx, app).await?;
                    check!(
                        !left.contains(&gv.to_string()),
                        "the removed guest_view still holds a role of {app}: {left:?}"
                    );
                }
                check!(
                    user_grantees(&cx, &live.id).await? == [guest.to_string()],
                    "guest's grant must stay"
                );
                check!(
                    !uses(&cx, gv, &live.id).await?,
                    "the removed guest_view still uses the app"
                );
                check!(uses(&cx, guest, &live.id).await?, "guest lost use");
                let roles = cx
                    .state
                    .services
                    .apps
                    .effective_roles(
                        cx.a().id,
                        ruum_core::NodeId(live.id.parse().map_err(|_| Fail::Failed("id".into()))?),
                        Some(ruum_core::UserId(gv)),
                    )
                    .await
                    .map_err(|e| Fail::Failed(format!("effective_roles: {e}")))?;
                check!(
                    roles.is_empty(),
                    "roles of the removed guest_view: {roles:?}"
                );
                let res = cx
                    .send(Who::AgentOfGuestView, list_role_grants(&cx, &w, ""))
                    .await?;
                check!(
                    matches!(res.status.as_u16(), 401 | 403 | 404),
                    "the agent of the removed guest_view lists grants: {} {}",
                    res.status,
                    res.text()
                );
                let b = &cx.w.b;
                let kept = cx
                    .state
                    .services
                    .apps
                    .roles()
                    .get(b.id, b.app, b.app_role_grant)
                    .await
                    .map_err(|e| Fail::Failed(format!("tenant B's grant: {e}")))?;
                check!(
                    kept.is_some(),
                    "the removal in tenant A deleted tenant B's grant"
                );
                check!(
                    uses(&cx, b.user_id(Who::GuestView).0, &b.app.to_string()).await?,
                    "tenant B's guest_view lost use of B's app"
                );
                Ok(())
            }
        ),
        case!(
            "app_roles_follow_the_live_manifest",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "listAppRoles"
            ],
            "listAppRoles of a live app with three roles: each active, member is the only \
             default, admin the only grants_roles, descriptions from the manifest, no holders",
            |cx| {
                let live = live_app(&cx, cx.a().projects, "roles", ROLES_MANIFEST).await?;
                let list = roles(&cx, &live.id).await?;
                check!(list.len() == 3, "listAppRoles: {list:?}");
                for name in ["member", "approver", "admin"] {
                    let Some(r) = role(&list, name) else {
                        return Err(Fail::Failed(format!("no role {name}: {list:?}")));
                    };
                    check!(
                        r["active"] == true
                            && r["holders"] == 0
                            && r["default"] == (name == "member")
                            && r["grants_roles"] == (name == "admin")
                            && r["description"].as_str().is_some_and(|d| !d.is_empty()),
                        "role {name}: {r}"
                    );
                }
                Ok(())
            }
        ),
        case!(
            "a_share_holder_grants_and_removes_app_roles",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "listAppRoles",
                "listAppRoleGrants",
                "addAppRoleGrant",
                "removeAppRoleGrant"
            ],
            "member (share on the app) gives approver to guest: 201 with role and grantee; \
             listAppRoleGrants?role=approver lists it and listAppRoles counts one holder; \
             removeAppRoleGrant answers 204 and the grant is gone",
            |cx| {
                let live = live_app(&cx, cx.a().projects, "orders", ROLES_MANIFEST).await?;
                let id = grant(&cx, Who::Member, &live.id, "approver", Who::Guest).await?;
                let page = cx
                    .ok(
                        Who::Member,
                        list_role_grants(&cx, &live.id, "?role=approver"),
                        &[200],
                    )
                    .await?;
                let listed = items(&page);
                check!(
                    listed.iter().any(|g| g["id"] == id.as_str())
                        && listed.iter().all(|g| g["role"] == "approver"),
                    "listAppRoleGrants?role=approver: {page}"
                );
                let list = roles(&cx, &live.id).await?;
                check!(
                    role(&list, "approver").is_some_and(|r| r["holders"] == 1),
                    "holders of approver: {list:?}"
                );
                cx.ok(Who::Member, remove_role_grant(&cx, &live.id, &id), &[204])
                    .await?;
                let page = cx
                    .ok(Who::Member, list_role_grants(&cx, &live.id, ""), &[200])
                    .await?;
                check!(
                    !items(&page).iter().any(|g| g["id"] == id.as_str()),
                    "the removed grant is still listed: {page}"
                );
                Ok(())
            }
        ),
        case!(
            "a_grants_roles_holder_grants_and_others_cannot",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "addAppRoleGrant",
                "removeAppRoleGrant",
                "listAppRoleGrants"
            ],
            "guest (no grant on any node) holding admin (grants_roles) gives approver to \
             guest_comment (201) and removes it again (204); guest_comment (comment, holds \
             approver) and member_view (view, no share) get 403 for addAppRoleGrant and for \
             removeAppRoleGrant (app_role#grant), and listAppRoleGrants is unchanged after each \
             refusal; after member removes guest's admin grant, guest can no longer grant (403 \
             or 404) and the grants stay as they were",
            |cx| {
                let live = live_app(&cx, cx.a().projects, "granted", ROLES_MANIFEST).await?;
                let admin = grant(&cx, Who::Member, &live.id, "admin", Who::Guest).await?;
                let id = grant(&cx, Who::Guest, &live.id, "approver", Who::GuestComment).await?;
                let before = grant_ids(&cx, &live.id).await?;
                for who in [Who::GuestComment, Who::MemberView] {
                    cx.refused(
                        who,
                        add_role_grant(&cx, &live.id, "approver", Who::GuestEdit),
                        403,
                        &["permission_denied"],
                    )
                    .await?;
                    grants_unchanged(&cx, &live.id, &before, who.name()).await?;
                }
                for who in [Who::GuestComment, Who::MemberView] {
                    cx.refused(
                        who,
                        remove_role_grant(&cx, &live.id, &id),
                        403,
                        &["permission_denied"],
                    )
                    .await?;
                    grants_unchanged(&cx, &live.id, &before, who.name()).await?;
                }
                cx.ok(Who::Guest, remove_role_grant(&cx, &live.id, &id), &[204])
                    .await?;
                cx.ok(
                    Who::Member,
                    remove_role_grant(&cx, &live.id, &admin),
                    &[204],
                )
                .await?;
                let before = grant_ids(&cx, &live.id).await?;
                let res = cx
                    .send(
                        Who::Guest,
                        add_role_grant(&cx, &live.id, "approver", Who::GuestComment),
                    )
                    .await?;
                check!(
                    matches!(res.status.as_u16(), 403 | 404),
                    "a removed grants_roles grant still grants: {} {}",
                    res.status,
                    res.text()
                );
                grants_unchanged(&cx, &live.id, &before, "a removed grants_roles grant").await
            }
        ),
        case!(
            "an_editor_without_share_cannot_change_app_role_grants",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "addAppRoleGrant",
                "removeAppRoleGrant",
                "listAppRoleGrants"
            ],
            "an app in Archive (editors_can_share false): guest_edit and member_edit (edit on \
             the app, no share, no app role) get 403 permission_denied for addAppRoleGrant and \
             for removeAppRoleGrant of member's grant, and listAppRoleGrants is unchanged after \
             each refusal; on an app in Projects (editors share) guest_edit gives a role (201): \
             app_role#grant is share, not edit (schema.zed)",
            |cx| {
                let archived = live_app(&cx, cx.a().archive, "archived", ROLES_MANIFEST).await?;
                let id = grant(&cx, Who::Member, &archived.id, "approver", Who::Guest).await?;
                let before = grant_ids(&cx, &archived.id).await?;
                for who in [Who::GuestEdit, Who::MemberEdit] {
                    cx.refused(
                        who,
                        add_role_grant(&cx, &archived.id, "approver", Who::GuestComment),
                        403,
                        &["permission_denied"],
                    )
                    .await?;
                    grants_unchanged(&cx, &archived.id, &before, who.name()).await?;
                    cx.refused(
                        who,
                        remove_role_grant(&cx, &archived.id, &id),
                        403,
                        &["permission_denied"],
                    )
                    .await?;
                    grants_unchanged(&cx, &archived.id, &before, who.name()).await?;
                }
                let open = live_app(&cx, cx.a().projects, "shared", ROLES_MANIFEST).await?;
                grant(&cx, Who::GuestEdit, &open.id, "approver", Who::GuestComment).await?;
                Ok(())
            }
        ),
        case!(
            "a_role_outside_the_live_manifest_cannot_be_granted",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "addAppRoleGrant",
                "listAppRoleGrants"
            ],
            "addAppRoleGrant of a role that the live manifest does not declare (auditor) is 422 \
             validation_failed, also for member (share), and adds no grant",
            |cx| {
                let live = live_app(&cx, cx.a().projects, "audited", ROLES_MANIFEST).await?;
                cx.refused(
                    Who::Member,
                    add_role_grant(&cx, &live.id, "auditor", Who::Guest),
                    422,
                    &["validation_failed"],
                )
                .await?;
                grants_unchanged(&cx, &live.id, &[], "a role outside the manifest").await
            }
        ),
        case!(
            "a_removed_role_keeps_its_grants_and_gives_nothing",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "listAppRoles",
                "listAppRoleGrants",
                "addAppRoleGrant",
                "getApp"
            ],
            "a new release without admin: listAppRoles shows admin inactive with its holder, \
             listAppRoleGrants still lists guest's grant, guest can no longer grant (403 or \
             404) and nobody can give admin (422), and neither refusal adds a grant; publishing the first release again makes \
             admin active and guest grants again (design 9.3, card B-2)",
            |cx| {
                let live = live_app(&cx, cx.a().projects, "changing", ROLES_MANIFEST).await?;
                let admin = grant(&cx, Who::Member, &live.id, "admin", Who::Guest).await?;
                new_release(
                    &cx,
                    &live.id,
                    cx.a().projects,
                    "changing v2",
                    ROLES_MANIFEST_NO_ADMIN,
                )
                .await?;
                let list = roles(&cx, &live.id).await?;
                check!(
                    role(&list, "admin").is_some_and(|r| r["active"] == false && r["holders"] == 1),
                    "admin after the release without it: {list:?}"
                );
                check!(
                    role(&list, "member").is_some_and(|r| r["active"] == true),
                    "member after the release: {list:?}"
                );
                let page = cx
                    .ok(
                        Who::Member,
                        list_role_grants(&cx, &live.id, "?role=admin"),
                        &[200],
                    )
                    .await?;
                check!(
                    items(&page).iter().any(|g| g["id"] == admin.as_str()),
                    "the grant of the inactive role is gone: {page}"
                );
                let before = grant_ids(&cx, &live.id).await?;
                let res = cx
                    .send(
                        Who::Guest,
                        add_role_grant(&cx, &live.id, "approver", Who::GuestComment),
                    )
                    .await?;
                check!(
                    matches!(res.status.as_u16(), 403 | 404),
                    "an inactive grants_roles role still grants: {} {}",
                    res.status,
                    res.text()
                );
                grants_unchanged(&cx, &live.id, &before, "an inactive grants_roles role").await?;
                cx.refused(
                    Who::Member,
                    add_role_grant(&cx, &live.id, "admin", Who::GuestEdit),
                    422,
                    &["validation_failed"],
                )
                .await?;
                grants_unchanged(&cx, &live.id, &before, "a grant of an inactive role").await?;
                let back = cx
                    .ok(
                        Who::Member,
                        publish(&cx, &live.id, live.deployment(), serde_json::json!({})),
                        &[200],
                    )
                    .await?;
                wait_live_is(&cx, &live.id, &str_at(&back, "/id")?).await?;
                let list = roles(&cx, &live.id).await?;
                check!(
                    role(&list, "admin").is_some_and(|r| r["active"] == true),
                    "admin after the rollback: {list:?}"
                );
                grant(&cx, Who::Guest, &live.id, "approver", Who::GuestComment).await?;
                Ok(())
            }
        ),
        case!(
            "viewers_without_an_app_role_use_the_app",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "listAppRoles",
                "getApp"
            ],
            "the app has exactly one default role; member_view (view, no app role) may use the \
             app (getApp my_permissions.use) but not share; the default role in the user token \
             is the gateway's (card T-6)",
            |cx| {
                let live = live_app(&cx, cx.a().projects, "everyone", ROLES_MANIFEST).await?;
                let list = roles(&cx, &live.id).await?;
                let defaults: Vec<&Value> = list.iter().filter(|r| r["default"] == true).collect();
                check!(
                    defaults.len() == 1 && defaults[0]["name"] == "member",
                    "default roles: {list:?}"
                );
                let a = cx
                    .ok(Who::MemberView, get_app(&cx, &live.id), &[200])
                    .await?;
                check!(
                    a["my_permissions"]["use"] == true && a["my_permissions"]["share"] == false,
                    "my_permissions of member_view: {}",
                    a["my_permissions"]
                );
                Ok(())
            }
        ),
        case!(
            "an_agent_grants_app_roles_as_add_grant",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "addAppRoleGrant",
                "listAppRoleGrants"
            ],
            "agent (scope Projects, view) gets 403 for addAppRoleGrant although its user may \
             share, and no grant is added; agent_out (scope Private, manage) gives approver on an app in Private (201): \
             the user's right intersected with the scope, as addGrant",
            |cx| {
                let projects = live_app(&cx, cx.a().projects, "bot roles", ROLES_MANIFEST).await?;
                cx.refused(
                    Who::Agent,
                    add_role_grant(&cx, &projects.id, "approver", Who::Guest),
                    403,
                    &["permission_denied"],
                )
                .await?;
                grants_unchanged(&cx, &projects.id, &[], "agent's refused grant").await?;
                let private = live_app(&cx, cx.a().private, "own roles", ROLES_MANIFEST).await?;
                let id = grant(&cx, Who::AgentOut, &private.id, "approver", Who::Guest).await?;
                let page = cx
                    .ok(Who::Member, list_role_grants(&cx, &private.id, ""), &[200])
                    .await?;
                let g = items(&page)
                    .into_iter()
                    .find(|g| g["id"] == id.as_str())
                    .unwrap_or(Value::Null);
                check!(
                    g["granted_by"].is_object(),
                    "the agent's grant has no granted_by: {page}"
                );
                Ok(())
            }
        ),
    ]
}
