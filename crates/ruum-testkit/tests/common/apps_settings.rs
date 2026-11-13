//! App cases of card W-1: `createApp`, `getApp`, `updateAppSettings`, `listAppDeployments` and
//! `getAppDeployment` (`wait`): slugs and retired slugs, `access: public`, the telemetry
//! settings, and who may create an app (viewers, guests, agents). See [`super::apps`].

use super::apps::*;
use super::{Case, Ctx, Fail};
use crate::{case, check, fail};
use axum::http::Method;
use ruum_core::TenantId;
use ruum_testkit::client::RequestSpec;
use ruum_testkit::fixtures::Who;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// A new team tenant of `owner` (`createTenant`): its ID and root node. The platform admin
/// has not approved public apps for it (`Tenant.app_approval` is off at first).
async fn new_tenant(cx: &Ctx, name: &str) -> Result<(TenantId, String, Value), Fail> {
    let t = cx
        .ok(
            Who::Owner,
            RequestSpec::post("/v1/tenants").json(json!({ "name": name })),
            &[201],
        )
        .await?;
    let id: TenantId = match str_at(&t, "/id")?.parse() {
        Ok(id) => id,
        Err(_) => fail!("createTenant gave an ID that is not a UUID: {t}"),
    };
    Ok((id, str_at(&t, "/root_node_id")?, t))
}

/// `createApp` in another tenant of the owner.
fn create_app_at(
    tenant: TenantId,
    parent: &str,
    name: &str,
    settings: Option<Value>,
) -> RequestSpec {
    let mut body = json!({ "parent_id": parent, "name": name });
    if let Some(s) = settings {
        body["settings"] = s;
    }
    RequestSpec::post("/v1/apps").tenant(tenant).json(body)
}

/// The slug of a new app in tenant A follows the rules and is the first label of its address.
fn check_slug(app: &Value, name: &str) -> Result<String, Fail> {
    let slug = str_at(app, "/slug")?;
    if let Some(p) = slug_problem(&slug, 53) {
        fail!("the app named {name:?} got a slug against the rules: {p}");
    }
    let (in_url, tenant_slug) = url_slugs(&str_at(app, "/url")?)?;
    check!(
        in_url == slug,
        "the address of {name:?} starts with {in_url:?}, not its slug {slug:?}"
    );
    if let Some(p) = slug_problem(&tenant_slug, 40) {
        fail!("the tenant slug in the address of {name:?} is against the rules: {p}");
    }
    Ok(slug)
}

/// The personal tenant of `who` (`listTenants`): its ID, name and root node (`getRoot`).
async fn personal_tenant(cx: &Ctx, who: Who) -> Result<(TenantId, String, String), Fail> {
    let list = cx.ok(who, RequestSpec::get("/v1/tenants"), &[200]).await?;
    let Some(m) = items(&list)
        .into_iter()
        .find(|m| m["tenant"]["kind"] == "personal" && m["role"] == "owner")
    else {
        fail!("{} has no personal space: {list}", who.name());
    };
    let id: TenantId = match str_at(&m, "/tenant/id")?.parse() {
        Ok(id) => id,
        Err(_) => fail!("listTenants gave an ID that is not a UUID: {m}"),
    };
    let root = cx
        .ok(who, RequestSpec::get("/v1/nodes/root").tenant(id), &[200])
        .await?;
    Ok((id, str_at(&m, "/tenant/name")?, str_at(&root, "/id")?))
}

/// `ws-` and 6 lower-case base32 characters (decision 0008-1).
fn is_neutral_slug(slug: &str) -> bool {
    slug.len() == 9
        && slug.starts_with("ws-")
        && slug[3..]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
}

/// The first word (3 or more letters or digits, lower case) of `name` that `slug` holds.
fn name_word_in(slug: &str, name: &str) -> Option<String> {
    name.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| w.len() >= 3)
        .find(|w| slug.contains(w))
        .map(str::to_string)
}

/// `updateTenant` with only `app_slug`.
fn choose_slug(tenant: TenantId, body: Value) -> RequestSpec {
    RequestSpec::new(Method::PATCH, format!("/v1/tenants/{tenant}")).json(body)
}

pub fn cases() -> Vec<Case> {
    vec![
        case!(
            "personal_tenant_slugs_are_neutral_and_random",
            [
                "getMe",
                "listTenants",
                "getRoot",
                "getTenant",
                "createTenant",
                "createApp"
            ],
            "the first app in a personal space fixes a neutral random tenant slug (ws- and 6 \
             lower-case base32 characters) that holds no word of the display name or of the \
             space name; Tenant.app_slug shows it; two personal spaces get two slugs; a team \
             space keeps the slug of its name (decision 0008-1, card S-35)",
            |cx| {
                let mut seen = std::collections::BTreeSet::new();
                for who in [Who::MemberView, Who::MemberComment] {
                    let me = cx.ok(who, RequestSpec::get("/v1/me"), &[200]).await?;
                    let display = str_at(&me, "/user/display_name")?;
                    let (tid, name, root) = personal_tenant(&cx, who).await?;
                    let before = cx
                        .ok(who, RequestSpec::get(format!("/v1/tenants/{tid}")), &[200])
                        .await?;
                    let earlier = before["app_slug"].as_str().unwrap_or_default();
                    check!(
                        before["app_slug"].is_null() || is_neutral_slug(earlier),
                        "{} has a personal space slug that is not neutral: {before}",
                        who.name()
                    );
                    let app = cx
                        .ok(who, create_app_at(tid, &root, "plant list", None), &[202])
                        .await?;
                    let (_, slug) = url_slugs(&str_at(&app, "/app/url")?)?;
                    check!(
                        is_neutral_slug(&slug),
                        "the personal space {name:?} of {} got the slug {slug:?}, not ws-<6 \
                         base32>",
                        who.name()
                    );
                    for n in [&display, &name] {
                        if let Some(w) = name_word_in(&slug, n) {
                            fail!("the slug {slug:?} holds {w:?} of the name {n:?}");
                        }
                    }
                    let after = cx
                        .ok(who, RequestSpec::get(format!("/v1/tenants/{tid}")), &[200])
                        .await?;
                    check!(
                        after["app_slug"] == slug.as_str(),
                        "getTenant shows app_slug {} after the first app, not {slug:?}",
                        after["app_slug"]
                    );
                    check!(
                        seen.insert(slug.clone()),
                        "two personal spaces got the slug {slug:?}"
                    );
                }
                let team_name = format!(
                    "Orchard club {}",
                    &uuid::Uuid::new_v4().simple().to_string()[..8]
                );
                let (tid, root, _) = new_tenant(&cx, &team_name).await?;
                let app = cx
                    .ok(
                        Who::Owner,
                        create_app_at(tid, &root, "plant list", None),
                        &[202],
                    )
                    .await?;
                let (_, slug) = url_slugs(&str_at(&app, "/app/url")?)?;
                check!(
                    slug.starts_with("orchard-club-"),
                    "the team space {team_name:?} got the slug {slug:?}, not one from its name"
                );
                Ok(())
            }
        ),
        case!(
            "a_person_chooses_a_permanent_personal_slug",
            [
                "listTenants",
                "getRoot",
                "getTenant",
                "updateTenant",
                "createApp",
                "listAudit"
            ],
            "updateTenant with app_slug chooses the tenant slug of a personal space once: rule \
             breaks and app_slug with name are 422 (hint /app_slug), a team space is 422, an \
             agent is user_required; then the same slug is 200 and another is 409 \
             app_slug_fixed; the choice has one tenant.updated audit entry by the person; the \
             next app uses it; another person's space gets 409 app_slug_taken for it \
             (decision 0008-1, card S-35)",
            |cx| {
                let who = Who::MemberEdit;
                let (tid, _, root) = personal_tenant(&cx, who).await?;
                let mine = format!("s35-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
                for bad in [
                    json!({ "app_slug": "admin" }),
                    json!({ "app_slug": "a--b" }),
                    json!({ "app_slug": "Mine" }),
                    json!({ "app_slug": "xn--mine" }),
                    json!({ "app_slug": mine, "name": "New name" }),
                ] {
                    let err = cx
                        .refused(
                            who,
                            choose_slug(tid, bad.clone()),
                            422,
                            &["validation_failed"],
                        )
                        .await?;
                    super::api::hint_has(&err, "/app_slug")?;
                }
                let err = cx
                    .refused(
                        Who::Owner,
                        choose_slug(cx.a().id, json!({ "app_slug": mine })),
                        422,
                        &["validation_failed"],
                    )
                    .await?;
                super::api::hint_has(&err, "/app_slug")?;
                cx.refused(
                    Who::Agent,
                    choose_slug(cx.a().id, json!({ "app_slug": mine })),
                    403,
                    &["user_required"],
                )
                .await?;
                let t = cx
                    .ok(who, choose_slug(tid, json!({ "app_slug": mine })), &[200])
                    .await?;
                check!(
                    t["app_slug"] == mine.as_str(),
                    "updateTenant answered app_slug {}, not {mine:?}",
                    t["app_slug"]
                );
                cx.ok(who, choose_slug(tid, json!({ "app_slug": mine })), &[200])
                    .await?;
                let err = cx
                    .refused(
                        who,
                        choose_slug(tid, json!({ "app_slug": format!("{mine}-b") })),
                        409,
                        &["conflict"],
                    )
                    .await?;
                super::api::hint_has(&err, "app_slug_fixed")?;
                // One audit entry for the choice, by the person; none for the same slug again.
                let audit = cx
                    .ok(
                        who,
                        RequestSpec::get(format!(
                            "/v1/tenants/{tid}/audit?action=tenant.updated&limit=200"
                        )),
                        &[200],
                    )
                    .await?;
                let me = cx.p(who).id.to_string();
                let chosen: Vec<&Value> = audit["items"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter(|r| r["data"]["app_slug"] == mine.as_str())
                            .collect()
                    })
                    .unwrap_or_default();
                check!(
                    chosen.len() == 1
                        && chosen[0]["action"] == "tenant.updated"
                        && chosen[0]["actor"]["kind"] == "user"
                        && chosen[0]["actor"]["id"] == me.as_str()
                        && chosen[0]["target_type"] == "tenant"
                        && chosen[0]["target_id"] == tid.to_string().as_str(),
                    "the choice of {mine:?} has not one tenant.updated entry by the person: {audit}"
                );
                let app = cx
                    .ok(who, create_app_at(tid, &root, "plant list", None), &[202])
                    .await?;
                let (_, slug) = url_slugs(&str_at(&app, "/app/url")?)?;
                check!(
                    slug == mine,
                    "the app address has the tenant slug {slug:?}, not the chosen {mine:?}"
                );
                let got = cx
                    .ok(who, RequestSpec::get(format!("/v1/tenants/{tid}")), &[200])
                    .await?;
                check!(
                    got["app_slug"] == mine.as_str(),
                    "getTenant shows app_slug {}",
                    got["app_slug"]
                );
                let (other, _, _) = personal_tenant(&cx, Who::GuestComment).await?;
                let err = cx
                    .refused(
                        Who::GuestComment,
                        choose_slug(other, json!({ "app_slug": mine })),
                        409,
                        &["conflict"],
                    )
                    .await?;
                super::api::hint_has(&err, "app_slug_taken")
            }
        ),
        case!(
            "a_new_app_waits_for_its_first_deployment",
            [
                "createApp",
                "getApp",
                "listAppDeployments",
                "getAppDeployment"
            ],
            "createApp answers 202 with the app: status provisioning, no live deployment, no \
             previews, the default settings (access platform, telemetry_mode privacy, \
             raw_copy_days 0), slug and url; listAppDeployments is empty; my_permissions of \
             member (manage) allow everything, of member_view only use; getApp of a node that \
             is not an app and getAppDeployment of an unknown deployment are 404",
            |cx| {
                let app = new_app(&cx, Who::Member, cx.a().projects, "plant list").await?;
                let id = app_id(&app)?;
                check_slug(&app, "plant list")?;
                let a = cx.ok(Who::Member, get_app(&cx, &id), &[200]).await?;
                check!(a["status"]["state"] == "provisioning", "status: {a}");
                check!(a["live_deployment"].is_null(), "live_deployment: {a}");
                check!(
                    a["previews"].as_array().is_some_and(|p| p.is_empty()),
                    "previews: {a}"
                );
                let s = &a["settings"];
                check!(
                    s["access"] == "platform"
                        && s["telemetry_mode"] == "privacy"
                        && s["raw_copy_days"] == 0,
                    "the default settings: {s}"
                );
                let m = &a["my_permissions"];
                for p in [
                    "use",
                    "deploy",
                    "publish",
                    "read_telemetry",
                    "configure",
                    "share",
                ] {
                    check!(m[p] == true, "member (manage) may not {p}: {m}");
                }
                let v = cx.ok(Who::MemberView, get_app(&cx, &id), &[200]).await?;
                let m = &v["my_permissions"];
                check!(
                    m["use"] == true,
                    "member_view (view) may not use the app: {m}"
                );
                for p in ["deploy", "publish", "read_telemetry", "configure", "share"] {
                    check!(m[p] == false, "member_view (view) may {p}: {m}");
                }
                let page = cx
                    .ok(Who::Member, list_deployments(&cx, &id, ""), &[200])
                    .await?;
                check!(items(&page).is_empty(), "a new app has deployments: {page}");
                cx.refused(Who::Member, get_app(&cx, cx.a().plan), 404, &["not_found"])
                    .await?;
                cx.refused(
                    Who::Member,
                    get_deployment(&cx, &id, uuid::Uuid::now_v7(), None),
                    404,
                    &["not_found"],
                )
                .await?;
                Ok(())
            }
        ),
        case!(
            "reserved_names_get_a_free_slug",
            ["createApp"],
            "an app named like a reserved slug (every one of docs/STEP3.md), a name shorter \
             than 3 and one starting with _ still gets 202: the platform picks a free slug from \
             the name that keeps the rules (createApp: a clash with a name rule picks a free \
             slug); the slug is the first label of App.url",
            |cx| {
                let mut names: Vec<String> = RESERVED_SLUGS.iter().map(|s| s.to_string()).collect();
                names.extend(["ab".into(), "x".into(), "_hidden".into(), "_".into()]);
                for name in names {
                    let app = new_app(&cx, Who::Member, cx.a().projects, &name).await?;
                    let slug = check_slug(&app, &name)?;
                    check!(slug != name, "the app named {name:?} got that slug");
                }
                Ok(())
            }
        ),
        case!(
            "odd_names_give_slugs_within_the_rules",
            ["createApp"],
            "names with --, an xn-- prefix, a - at either end, upper case, spaces, letters \
             outside ASCII and 60 or 255 characters give a slug of 3 to 53 characters with no \
             --, no - at an end and no xn-- prefix (App.slug, decision 0006-15); two apps never \
             share a slug",
            |cx| {
                let long60 = "p".repeat(60);
                let long255 = "a-".repeat(127) + "b";
                let names = [
                    "garden--plants",
                    "xn--plants",
                    "-edge-",
                    "Plant List",
                    "Õunaaed ja köök",
                    "a - - b",
                    long60.as_str(),
                    long255.as_str(),
                    "garden-plants",
                ];
                let mut seen = std::collections::BTreeSet::new();
                for name in names {
                    let app = new_app(&cx, Who::Member, cx.a().projects, name).await?;
                    let slug = check_slug(&app, name)?;
                    check!(seen.insert(slug.clone()), "two apps got the slug {slug:?}");
                }
                Ok(())
            }
        ),
        case!(
            "tenant_slugs_keep_the_rules",
            ["createTenant", "createApp"],
            "the tenant slug in App.url keeps the rules (3 to 40 characters, no --, no - at an \
             end, no xn-- prefix, not reserved) for tenants named with 100 characters, like a \
             reserved slug, and with -- (decision 0006-15)",
            |cx| {
                let names = [
                    "t".repeat(100),
                    "admin".to_string(),
                    "xn--a--b-".to_string(),
                ];
                for (i, name) in names.iter().enumerate() {
                    let (tid, root, _) = new_tenant(&cx, name).await?;
                    let app = cx
                        .ok(
                            Who::Owner,
                            create_app_at(tid, &root, &format!("app {i}"), None),
                            &[202],
                        )
                        .await?;
                    let (_, tenant_slug) = url_slugs(&str_at(&app, "/app/url")?)?;
                    if let Some(p) = slug_problem(&tenant_slug, 40) {
                        fail!("the tenant named {name:?} has a slug against the rules: {p}");
                    }
                }
                Ok(())
            }
        ),
        case!(
            "retired_slug_is_not_given_again",
            [
                "createApp",
                "getApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "trashNode",
                "purgeNode",
            ],
            "an app that was deployed and then purged retires its slug: a new app of the same \
             tenant with the same name (so the same slug) gets 409 conflict, hint slug_retired \
             (createApp)",
            |cx| {
                // A name of its own, so worlds that share a database never meet.
                let name = format!(
                    "orchard-{}",
                    &uuid::Uuid::new_v4().simple().to_string()[..8]
                );
                let first = new_folder(&cx, Who::Member, cx.a().projects, "first").await?;
                let old = live_app(&cx, &first, &name, STATIC_MANIFEST).await?;
                check!(
                    slug_problem(&old.slug, 53).is_none(),
                    "the app {name:?} got the slug {:?}",
                    old.slug
                );
                cx.ok(
                    Who::Owner,
                    RequestSpec::post(format!("/v1/nodes/{}/trash", old.id)).tenant(cx.a().id),
                    &[200],
                )
                .await?;
                cx.ok(
                    Who::Owner,
                    RequestSpec::new(Method::DELETE, format!("/v1/nodes/{}", old.id))
                        .tenant(cx.a().id),
                    &[200],
                )
                .await?;
                let second = new_folder(&cx, Who::Member, cx.a().projects, "second").await?;
                let err = cx
                    .refused(
                        Who::Member,
                        create_app(&cx, &second, &name),
                        409,
                        &["conflict"],
                    )
                    .await?;
                super::api::hint_has(&err, "slug_retired")
            }
        ),
        case!(
            "retired_slug_is_not_given_to_another_tenant",
            [
                "createApp",
                "getApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "trashNode",
                "purgeNode",
            ],
            "open question (see the pending entry): after tenant A retires the slug orchard, \
             what does createApp named orchard in tenant B answer?",
            |cx| {
                let _ = &cx;
                fail!("the contract does not say yet what tenant B gets; answer the question first")
            }
        ),
        case!(
            "public_access_needs_the_tenant_approval",
            [
                "getTenant",
                "createTenant",
                "createApp",
                "updateAppSettings"
            ],
            "fixture tenant A has app_approval.public_apps; a new tenant has not, and there \
             createApp with access public and updateAppSettings to public are 403 \
             permission_denied, hint public_apps_not_allowed, also for its owner (decision 22)",
            |cx| {
                let a = cx
                    .ok(
                        Who::Owner,
                        RequestSpec::get(format!("/v1/tenants/{}", cx.a().id)),
                        &[200],
                    )
                    .await?;
                check!(
                    a["app_approval"]["public_apps"] == true,
                    "the fixture tenant has no approval of public apps (W-1 seeds it): {a}"
                );
                let (tid, root, t) = new_tenant(&cx, "no approval").await?;
                check!(
                    t["app_approval"]["public_apps"] == false,
                    "a new tenant starts with public apps approved: {t}"
                );
                let err = cx
                    .refused(
                        Who::Owner,
                        create_app_at(tid, &root, "open", Some(json!({ "access": "public" }))),
                        403,
                        &["permission_denied"],
                    )
                    .await?;
                super::api::hint_has(&err, "public_apps_not_allowed")?;
                let made = cx
                    .ok(
                        Who::Owner,
                        create_app_at(tid, &root, "closed", None),
                        &[202],
                    )
                    .await?;
                let id = str_at(&made, "/app/node/id")?;
                let err = cx
                    .refused(
                        Who::Owner,
                        RequestSpec::new(Method::PATCH, app_path(&id, "/settings"))
                            .tenant(tid)
                            .json(json!({ "access": "public" })),
                        403,
                        &["permission_denied"],
                    )
                    .await?;
                super::api::hint_has(&err, "public_apps_not_allowed")
            }
        ),
        case!(
            "only_owners_and_admins_make_an_app_public",
            ["createApp", "updateAppSettings", "getApp"],
            "in team tenant A (approved): member (manage) gets 403 permission_denied, hint \
             tenant_admin_required, for updateAppSettings access public, and 403 for createApp \
             with access public; admin may change it (200) and owner may create a public app \
             (202) (decision 22, updateAppSettings)",
            |cx| {
                let app = new_app(&cx, Who::Member, cx.a().projects, "shop").await?;
                let id = app_id(&app)?;
                let err = cx
                    .refused(
                        Who::Member,
                        update_settings(&cx, &id, json!({ "access": "public" })),
                        403,
                        &["permission_denied"],
                    )
                    .await?;
                super::api::hint_has(&err, "tenant_admin_required")?;
                cx.refused(
                    Who::Member,
                    create_app_with(
                        &cx,
                        cx.a().projects,
                        "open shop",
                        json!({ "access": "public" }),
                    ),
                    403,
                    &["permission_denied"],
                )
                .await?;
                let a = cx
                    .ok(
                        Who::Admin,
                        update_settings(&cx, &id, json!({ "access": "public" })),
                        &[200],
                    )
                    .await?;
                check!(a["app"]["settings"]["access"] == "public", "access: {a}");
                let made = cx
                    .ok(
                        Who::Owner,
                        create_app_with(
                            &cx,
                            cx.a().projects,
                            "fair",
                            json!({ "access": "public" }),
                        ),
                        &[202],
                    )
                    .await?;
                check!(
                    made["app"]["settings"]["access"] == "public",
                    "access: {made}"
                );
                Ok(())
            }
        ),
        case!(
            "an_agent_never_sets_public_access",
            ["createApp", "updateAppSettings", "getApp"],
            "agent_out (scope Private, manage) gets 403 user_required for createApp with access \
             public and for updateAppSettings access public; the access stays platform \
             (updateAppSettings: agents may not change access)",
            |cx| {
                cx.refused(
                    Who::AgentOut,
                    create_app_with(
                        &cx,
                        cx.a().private,
                        "bot shop",
                        json!({ "access": "public" }),
                    ),
                    403,
                    &["user_required"],
                )
                .await?;
                let app = new_app(&cx, Who::AgentOut, cx.a().private, "bot shop").await?;
                let id = app_id(&app)?;
                cx.refused(
                    Who::AgentOut,
                    update_settings(&cx, &id, json!({ "access": "public" })),
                    403,
                    &["user_required"],
                )
                .await?;
                let a = cx.ok(Who::Member, get_app(&cx, &id), &[200]).await?;
                check!(a["settings"]["access"] == "platform", "access: {a}");
                Ok(())
            }
        ),
        case!(
            "an_agent_lowers_telemetry_but_never_raises_it",
            ["createApp", "updateAppSettings", "getApp"],
            "member (manage) raises telemetry_mode to full and raw_copy_days to 7 (200); \
             agent_out (manage in its scope) may lower both (200) but gets 403 user_required \
             when it raises either (design 9.6, updateAppSettings)",
            |cx| {
                let app = new_app(&cx, Who::Member, cx.a().private, "diary").await?;
                let id = app_id(&app)?;
                let set = |body: Value| update_settings(&cx, &id, body);
                cx.ok(
                    Who::Member,
                    set(json!({ "telemetry_mode": "full", "raw_copy_days": 7 })),
                    &[200],
                )
                .await?;
                for body in [
                    json!({ "raw_copy_days": 14 }),
                    json!({ "raw_copy_days": 30 }),
                ] {
                    cx.refused(Who::AgentOut, set(body), 403, &["user_required"])
                        .await?;
                }
                cx.ok(
                    Who::AgentOut,
                    set(json!({ "telemetry_mode": "privacy" })),
                    &[200],
                )
                .await?;
                cx.refused(
                    Who::AgentOut,
                    set(json!({ "telemetry_mode": "full" })),
                    403,
                    &["user_required"],
                )
                .await?;
                cx.ok(Who::AgentOut, set(json!({ "raw_copy_days": 3 })), &[200])
                    .await?;
                cx.refused(
                    Who::AgentOut,
                    set(json!({ "raw_copy_days": 4 })),
                    403,
                    &["user_required"],
                )
                .await?;
                cx.ok(
                    Who::AgentOut,
                    set(json!({ "report_widget": { "enabled": false } })),
                    &[200],
                )
                .await?;
                let a = cx.ok(Who::Member, get_app(&cx, &id), &[200]).await?;
                let s = &a["settings"];
                check!(
                    s["telemetry_mode"] == "privacy"
                        && s["raw_copy_days"] == 3
                        && s["report_widget"]["enabled"] == false,
                    "the settings after the agent: {s}"
                );
                Ok(())
            }
        ),
        case!(
            "settings_need_manage",
            ["createApp", "updateAppSettings", "getApp"],
            "member_edit and member_view get 403 permission_denied for every settings change, \
             also a lowering one; the settings stay (updateAppSettings needs manage)",
            |cx| {
                let app = new_app(&cx, Who::Member, cx.a().projects, "notes").await?;
                let id = app_id(&app)?;
                for who in [Who::MemberEdit, Who::MemberView] {
                    for body in [
                        json!({ "telemetry_mode": "full" }),
                        json!({ "raw_copy_days": 0 }),
                        json!({ "auto_rollback": { "enabled": false } }),
                    ] {
                        cx.refused(
                            who,
                            update_settings(&cx, &id, body),
                            403,
                            &["permission_denied"],
                        )
                        .await?;
                    }
                }
                let a = cx.ok(Who::Member, get_app(&cx, &id), &[200]).await?;
                check!(
                    a["settings"]["telemetry_mode"] == "privacy"
                        && a["settings"]["auto_rollback"]["enabled"] != false,
                    "the settings changed: {}",
                    a["settings"]
                );
                Ok(())
            }
        ),
        case!(
            "get_app_deployment_waits_for_a_state_change",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "listAppDeployments",
            ],
            "getAppDeployment?wait=30 on a running preview answers when the state changes, \
             long before 30 s, with another state; on a ready preview wait=1 answers within the \
             asked time (plus a margin); wait=31 is over the maximum of the parameter (422); \
             listAppDeployments?environment=preview lists the preview",
            |cx| {
                let app = new_app(&cx, Who::Member, cx.a().projects, "waiting").await?;
                let id = app_id(&app)?;
                let s = site(
                    &cx,
                    Who::Member,
                    cx.a().projects,
                    "waiting site",
                    STATIC_MANIFEST,
                )
                .await?;
                let made = cx
                    .ok(
                        Who::Member,
                        create_deployment(&cx, &id, from_folder(&s.folder, None)),
                        &[202],
                    )
                    .await?;
                let dep = str_at(&made, "/id")?;
                let first = made["state"].clone();
                let start = Instant::now();
                let d = cx
                    .ok(
                        Who::Member,
                        get_deployment(&cx, &id, &dep, Some(30)),
                        &[200],
                    )
                    .await?;
                check!(
                    start.elapsed() < Duration::from_secs(25),
                    "wait=30 held {:?} although the preview kept working",
                    start.elapsed()
                );
                check!(
                    d["state"] != first || matches!(d["state"].as_str(), Some("ready" | "failed")),
                    "wait=30 answered with the state of the 202 ({first}) and no change: {d}"
                );
                wait_state(&cx, Who::Member, &id, &dep, &["ready"]).await?;
                let start = Instant::now();
                let d = cx
                    .ok(Who::Member, get_deployment(&cx, &id, &dep, Some(1)), &[200])
                    .await?;
                check!(
                    start.elapsed() < Duration::from_secs(6),
                    "wait=1 on a ready preview held {:?}",
                    start.elapsed()
                );
                check!(d["state"] == "ready", "the ready preview changed: {d}");
                cx.refused(
                    Who::Member,
                    get_deployment(&cx, &id, &dep, Some(31)),
                    422,
                    &["validation_failed"],
                )
                .await?;
                let page = cx
                    .ok(
                        Who::Member,
                        list_deployments(&cx, &id, "?environment=preview"),
                        &[200],
                    )
                    .await?;
                check!(
                    items(&page).iter().any(|x| x["id"] == dep.as_str()),
                    "the preview is not listed: {page}"
                );
                let page = cx
                    .ok(
                        Who::Member,
                        list_deployments(&cx, &id, "?environment=live"),
                        &[200],
                    )
                    .await?;
                check!(
                    items(&page).is_empty(),
                    "a preview is listed as live: {page}"
                );
                Ok(())
            }
        ),
        case!(
            "wait_on_an_unknown_deployment_is_404_at_once",
            ["createApp", "getAppDeployment"],
            "getAppDeployment?wait=30 of a deployment that does not exist answers 404 at once, \
             not after 30 s",
            |cx| {
                let app = new_app(&cx, Who::Member, cx.a().projects, "nothing yet").await?;
                let id = app_id(&app)?;
                let start = Instant::now();
                cx.refused(
                    Who::Member,
                    get_deployment(&cx, &id, uuid::Uuid::now_v7(), Some(30)),
                    404,
                    &["not_found"],
                )
                .await?;
                check!(
                    start.elapsed() < Duration::from_secs(5),
                    "the 404 took {:?}",
                    start.elapsed()
                );
                Ok(())
            }
        ),
        case!(
            "guests_create_apps_only_with_the_policy",
            ["createApp", "updateTenant"],
            "guest_edit (edit on Projects) gets 403 permission_denied, hint \
             guests_create_resources, for createApp while the tenant policy is false and 202 \
             when it is true; agent_of_guest_view (its user has view) stays refused (this \
             checks the view cap only: an agent of an edit guest, which would test the policy \
             for agents, is not in the fixture yet, backlog TK-5)",
            |cx| {
                let err = cx
                    .refused(
                        Who::GuestEdit,
                        create_app(&cx, cx.a().projects, "guest app"),
                        403,
                        &["permission_denied"],
                    )
                    .await?;
                super::api::hint_has(&err, "guests_create_resources")?;
                super::api::guests_create_resources(&cx, true).await?;
                new_app(&cx, Who::GuestEdit, cx.a().projects, "guest app").await?;
                cx.refused(
                    Who::AgentOfGuestView,
                    create_app(&cx, cx.a().projects, "guest bot app"),
                    403,
                    &["permission_denied"],
                )
                .await?;
                Ok(())
            }
        ),
        case!(
            "viewers_neither_create_nor_deploy",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment"
            ],
            "member_view and member_comment get 403 permission_denied for createApp in \
             Projects and for createAppDeployment of member's app (both need edit)",
            |cx| {
                let app = new_app(&cx, Who::Member, cx.a().projects, "viewed").await?;
                let id = app_id(&app)?;
                let s = site(
                    &cx,
                    Who::Member,
                    cx.a().projects,
                    "viewed site",
                    STATIC_MANIFEST,
                )
                .await?;
                for who in [Who::MemberView, Who::MemberComment] {
                    cx.refused(
                        who,
                        create_app(&cx, cx.a().projects, "mine"),
                        403,
                        &["permission_denied"],
                    )
                    .await?;
                    cx.refused(
                        who,
                        create_deployment(&cx, &id, from_folder(&s.folder, None)),
                        403,
                        &["permission_denied"],
                    )
                    .await?;
                }
                Ok(())
            }
        ),
        case!(
            "an_agent_creates_apps_with_its_user_right_and_scope",
            ["createApp", "updateAgent"],
            "agent (scope Projects, max_role view) gets 403 for createApp in Projects although \
             its user has manage; agent_out (scope Private) cannot create in Projects (403 or \
             404) but can in Private (202); with max_role edit, agent creates in Projects (202)",
            |cx| {
                cx.refused(
                    Who::Agent,
                    create_app(&cx, cx.a().projects, "bot app"),
                    403,
                    &["permission_denied"],
                )
                .await?;
                let res = cx
                    .send(Who::AgentOut, create_app(&cx, cx.a().projects, "outside"))
                    .await?;
                check!(
                    matches!(res.status.as_u16(), 403 | 404),
                    "agent_out created an app outside its scope: {} {}",
                    res.status,
                    res.text()
                );
                new_app(&cx, Who::AgentOut, cx.a().private, "inside").await?;
                super::api::set_scope(&cx, Who::Agent, Some(cx.a().projects), "edit").await?;
                new_app(&cx, Who::Agent, cx.a().projects, "bot app").await?;
                Ok(())
            }
        ),
    ]
}
