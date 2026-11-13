//! The acceptance cases of the 12 operations of milestone M1 (card T-4a, `docs/STEP3.md`):
//! `createApp`, `getApp`, `updateAppSettings`, `listAppDeployments`, `getAppDeployment` (W-1),
//! `createAppDeployment` (W-2, static), `publishAppDeployment` for people and the app branch of
//! `restoreVersion` (W-3a), and `listAppRoles`, `listAppRoleGrants`, `addAppRoleGrant`,
//! `removeAppRoleGrant` (B-2). Written from the contract (`contracts/openapi.yaml`, tag `apps`;
//! `contracts/manifests/app.schema.json`) and decision 0006 before the code.
//!
//! The rules of the framework are those of `tests/common/mod.rs` (planned, pending, passed,
//! failed; a case names every operation it calls). The app cases keep their own pending list
//! ([`PENDING`] here, not the database list of `mod.rs`): the card named in an entry removes it
//! when it unblocks the case. `tests/apps_meta.rs` checks the cases and this list.
//!
//! This module holds the requests and set-up steps; the cases are in `apps_settings`,
//! `apps_deploy`, `apps_publish` and `apps_roles`.

use super::{Case, Ctx, Fail, Outcome, gate, operation_of, run_case};
use crate::{check, fail};
use axum::http::Method;
use futures::stream::{self, StreamExt};
use ruum_testkit::client::{self, RequestSpec, Response};
use ruum_testkit::contract;
use ruum_testkit::fixtures::{Backend, Credentials, Principal, TenantFixture, Who};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// The app cases that wait for a card other than the one that removes the `planned` marks of
/// their operations, with that card (or the open contract question and the card that answers
/// it). They do not run and never fail a group, also not with `RUUM_REQUIRE_IMPLEMENTED=1`.
/// A planned operation wins: such a case reports planned first.
pub const PENDING: &[(&str, &str)] = &[
    (
        "refused_binding_is_a_direct_403_naming_the_binding",
        "needs the deployer check of bindings: B-1 (and database bindings of previews: D-3a)",
    ),
    (
        "agent_binding_outside_its_scope_is_refused",
        "needs the deployer check of bindings (an agent: its user's right intersected with its \
         scope): B-1",
    ),
    (
        "binding_to_another_tenants_node_fails_like_a_missing_node",
        "needs binding resolution: B-1",
    ),
    (
        "file_binding_with_edit_is_refused",
        "needs binding rules: B-1; open question: 403 or 422 (the contract names no code)",
    ),
    (
        "rollback_across_a_migration_needs_force",
        "needs migrations at publish: D-2 (and database bindings: B-1, D-3a)",
    ),
    (
        "retired_slug_is_not_given_to_another_tenant",
        "open question for W-1: the contract refuses a slug that this tenant used before \
         (slug_retired), STEP3.md says no other tenant gets it either; a host has the tenant \
         slug, so what does tenant B see?",
    ),
    (
        "preview_expiry_is_visible",
        "open question for W-2: AppDeployment has no expiry field; where does a client read when \
         a preview expires?",
    ),
];

/// The reason of a case in [`PENDING`].
pub fn pending_reason(case: &str) -> Option<&'static str> {
    PENDING.iter().find(|(c, _)| *c == case).map(|(_, r)| *r)
}

/// The decision before an app case runs: planned first, then [`PENDING`], else `None`.
pub fn app_gate(case: &Case, is_planned: impl Fn(&str) -> bool) -> Option<Outcome> {
    if let Some(o) = gate(case.name, case.ops, &is_planned) {
        return Some(o);
    }
    pending_reason(case.name).map(Outcome::Blocked)
}

/// Every app case.
pub fn all_cases() -> Vec<Case> {
    let mut v = super::apps_settings::cases();
    v.extend(super::apps_deploy::cases());
    v.extend(super::apps_publish::cases());
    v.extend(super::apps_roles::cases());
    v
}

/// Runs a group of app cases on the backend of `RUUM_SUITE_BACKEND`, prints the report and
/// panics when the group fails (as [`super::run_group`], with [`app_gate`]).
pub async fn run_group(title: &str, cases: Vec<Case>) {
    let backend = Backend::start().await;
    let rows: Vec<(&'static str, Outcome)> = stream::iter(cases.iter())
        .map(|c| {
            let backend = &backend;
            async move {
                let o = match app_gate(c, contract::is_planned) {
                    Some(o) => o,
                    None => run_case(backend, c).await,
                };
                (c.name, o)
            }
        })
        .buffered(backend.jobs())
        .collect()
        .await;
    let name = backend.name();
    backend.finish().await;
    let (text, ok) = super::render(&format!("{title} [{name}]"), &rows);
    println!("{text}");
    assert!(ok, "{title} failed:{text}");
}

// ------------------------------------------------------------------ rules of the contract

/// How long a case waits for a deployment to reach a state.
pub const DEPLOY_WITHIN: Duration = Duration::from_secs(60);

/// The reserved slugs of `docs/STEP3.md` ("Host names and slugs") and decision 0006-15.
pub const RESERVED_SLUGS: &[&str] = &[
    "content",
    "www",
    "api",
    "admin",
    "static",
    "internal",
    "ruum",
    "status",
    "mail",
    "report",
    "preview",
    "app",
    "apps",
    "auth",
    "login",
    "id",
    "cdn",
    "assets",
    "docs",
    "help",
    "support",
    "blog",
    "dev",
    "staging",
    "test",
    "mta-sts",
    "autoconfig",
];

/// The slug rules (`App.slug`, decision 0006-15): one DNS label of 3 (shorter ones are
/// reserved) to `max` characters, lower-case letters, digits and single `-`, no `-` at either
/// end, no `xn--` prefix, not reserved, not starting with `_`.
pub fn slug_problem(slug: &str, max: usize) -> Option<String> {
    let ok_chars = slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !ok_chars {
        return Some(format!("{slug:?} has a character outside a-z, 0-9 and -"));
    }
    if slug.len() < 3 || slug.len() > max {
        return Some(format!("{slug:?} is not 3 to {max} characters long"));
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return Some(format!("{slug:?} has a - at an end"));
    }
    if slug.contains("--") {
        return Some(format!("{slug:?} has --"));
    }
    if slug.starts_with("xn-") {
        return Some(format!("{slug:?} has an xn-- prefix"));
    }
    if RESERVED_SLUGS.contains(&slug) {
        return Some(format!("{slug:?} is reserved"));
    }
    None
}

/// The host of a URL (`https://host/...`).
pub fn host_of(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    Some(rest.split(['/', ':', '?']).next().unwrap_or_default())
}

/// The app slug and the tenant slug of a live address `https://<app>.<tenant>.<apps domain>`.
pub fn url_slugs(url: &str) -> Result<(String, String), Fail> {
    let Some(host) = host_of(url) else {
        fail!("{url:?} is not an https URL");
    };
    let labels: Vec<&str> = host.split('.').collect();
    check!(
        labels.len() >= 4,
        "{url:?} is not https://<app-slug>.<tenant-slug>.<apps domain>"
    );
    Ok((labels[0].to_string(), labels[1].to_string()))
}

/// The preview host rule: `<short_id>--<app-slug>.<tenant-slug>.<apps domain>`, `short_id`
/// 8 characters of lower-case base32.
pub fn preview_url_problem(url: &str, slug: &str) -> Option<String> {
    let host = host_of(url)?;
    let first = host.split('.').next().unwrap_or_default();
    let Some((short, rest)) = first.split_once("--") else {
        return Some(format!("the preview host {host:?} has no <short_id>--"));
    };
    let base32 = short.len() == 8
        && short
            .chars()
            .all(|c| c.is_ascii_lowercase() || ('2'..='7').contains(&c));
    if !base32 {
        return Some(format!(
            "{short:?} is not 8 characters of lower-case base32"
        ));
    }
    if rest != slug {
        return Some(format!(
            "the preview host {host:?} is not for the slug {slug:?}"
        ));
    }
    None
}

// ------------------------------------------------------------------ requests

pub fn app_path(app: impl std::fmt::Display, rest: &str) -> String {
    format!("/v1/apps/{app}{rest}")
}

/// `createApp` in tenant A.
pub fn create_app(cx: &Ctx, parent: impl std::fmt::Display, name: &str) -> RequestSpec {
    create_app_in(cx.a(), parent, name, None)
}

/// `createApp` with settings, in tenant A.
pub fn create_app_with(
    cx: &Ctx,
    parent: impl std::fmt::Display,
    name: &str,
    settings: Value,
) -> RequestSpec {
    create_app_in(cx.a(), parent, name, Some(settings))
}

/// `createApp` in any tenant.
pub fn create_app_in(
    t: &TenantFixture,
    parent: impl std::fmt::Display,
    name: &str,
    settings: Option<Value>,
) -> RequestSpec {
    let mut body = json!({ "parent_id": parent.to_string(), "name": name });
    if let Some(s) = settings {
        body["settings"] = s;
    }
    RequestSpec::post("/v1/apps").tenant(t.id).json(body)
}

/// `getApp`.
pub fn get_app(cx: &Ctx, app: impl std::fmt::Display) -> RequestSpec {
    RequestSpec::get(app_path(app, "")).tenant(cx.a().id)
}

/// `updateAppSettings`.
pub fn update_settings(cx: &Ctx, app: impl std::fmt::Display, body: Value) -> RequestSpec {
    RequestSpec::new(Method::PATCH, app_path(app, "/settings"))
        .tenant(cx.a().id)
        .json(body)
}

/// `listAppDeployments` (`query` is `""` or `?environment=...`).
pub fn list_deployments(cx: &Ctx, app: impl std::fmt::Display, query: &str) -> RequestSpec {
    RequestSpec::get(app_path(app, &format!("/deployments{query}"))).tenant(cx.a().id)
}

/// `createAppDeployment`.
pub fn create_deployment(cx: &Ctx, app: impl std::fmt::Display, body: Value) -> RequestSpec {
    RequestSpec::post(app_path(app, "/deployments"))
        .tenant(cx.a().id)
        .json(body)
}

/// The body of `createAppDeployment` from a folder.
pub fn from_folder(folder: &str, draft: Option<&str>) -> Value {
    let mut b = json!({ "source": { "kind": "folder", "node_id": folder } });
    if let Some(d) = draft {
        b["draft"] = json!(d);
    }
    b
}

/// The body of `createAppDeployment` from a file (its head version).
pub fn from_file(file: &str) -> Value {
    json!({ "source": { "kind": "file", "node_id": file } })
}

/// The body of `createAppDeployment` from a given version of a file.
pub fn from_file_version(file: &str, version: &str) -> Value {
    json!({ "source": { "kind": "file", "node_id": file, "version_id": version } })
}

/// `getAppDeployment`, with `wait` seconds when given.
pub fn get_deployment(
    cx: &Ctx,
    app: impl std::fmt::Display,
    dep: impl std::fmt::Display,
    wait: Option<u32>,
) -> RequestSpec {
    let q = wait.map(|w| format!("?wait={w}")).unwrap_or_default();
    RequestSpec::get(app_path(app, &format!("/deployments/{dep}{q}"))).tenant(cx.a().id)
}

/// `publishAppDeployment`.
pub fn publish(
    cx: &Ctx,
    app: impl std::fmt::Display,
    dep: impl std::fmt::Display,
    body: Value,
) -> RequestSpec {
    RequestSpec::post(app_path(app, &format!("/deployments/{dep}/publish")))
        .tenant(cx.a().id)
        .json(body)
}

/// `listVersions` of a node.
pub fn list_versions(cx: &Ctx, node: impl std::fmt::Display) -> RequestSpec {
    RequestSpec::get(format!("/v1/nodes/{node}/versions")).tenant(cx.a().id)
}

/// `restoreVersion`.
pub fn restore_version(
    cx: &Ctx,
    node: impl std::fmt::Display,
    version: impl std::fmt::Display,
) -> RequestSpec {
    RequestSpec::post(format!("/v1/nodes/{node}/versions/{version}/restore")).tenant(cx.a().id)
}

/// `listAppRoles`.
pub fn list_roles(cx: &Ctx, app: impl std::fmt::Display) -> RequestSpec {
    RequestSpec::get(app_path(app, "/roles")).tenant(cx.a().id)
}

/// `listAppRoleGrants` (`query` is `""` or `?role=...`).
pub fn list_role_grants(cx: &Ctx, app: impl std::fmt::Display, query: &str) -> RequestSpec {
    RequestSpec::get(app_path(app, &format!("/role-grants{query}"))).tenant(cx.a().id)
}

/// `addAppRoleGrant` of `role` to the user `grantee` of tenant A.
pub fn add_role_grant(
    cx: &Ctx,
    app: impl std::fmt::Display,
    role: &str,
    grantee: Who,
) -> RequestSpec {
    RequestSpec::post(app_path(app, "/role-grants"))
        .tenant(cx.a().id)
        .json(json!({
            "role": role,
            "grantee": { "type": "user", "id": cx.p(grantee).id },
            "notify": false,
        }))
}

/// `removeAppRoleGrant`.
pub fn remove_role_grant(
    cx: &Ctx,
    app: impl std::fmt::Display,
    grant: impl std::fmt::Display,
) -> RequestSpec {
    RequestSpec::new(
        Method::DELETE,
        app_path(app, &format!("/role-grants/{grant}")),
    )
    .tenant(cx.a().id)
}

// ------------------------------------------------------------------ set-up steps

/// A string field of a JSON value, or a failed case.
pub fn str_at(v: &Value, pointer: &str) -> Result<String, Fail> {
    match v.pointer(pointer).and_then(Value::as_str) {
        Some(s) => Ok(s.to_string()),
        None => fail!("the answer has no string at {pointer}: {v}"),
    }
}

/// Creates an app as `who` (`createApp`, `202`) and returns its `App`.
pub async fn new_app(
    cx: &Ctx,
    who: Who,
    parent: impl std::fmt::Display,
    name: &str,
) -> Result<Value, Fail> {
    new_app_scanned(cx, who, parent, name, &[]).await
}

/// [`new_app`], with the answer scanned for `secrets` ([`ok_scanned`]).
pub async fn new_app_scanned(
    cx: &Ctx,
    who: Who,
    parent: impl std::fmt::Display,
    name: &str,
    secrets: &[String],
) -> Result<Value, Fail> {
    let made = ok_scanned(cx, who, create_app(cx, parent, name), &[202], secrets).await?;
    check!(
        made["app"]["node"]["id"].is_string(),
        "createApp gave no app.node.id: {made}"
    );
    Ok(made["app"].clone())
}

/// The node ID of an `App`.
pub fn app_id(app: &Value) -> Result<String, Fail> {
    str_at(app, "/node/id")
}

/// Makes a file with `content` as `who` (`createNode`, then `putContent`). Returns the node ID
/// and the version ID.
pub async fn write_file(
    cx: &Ctx,
    who: Who,
    parent: impl std::fmt::Display,
    name: &str,
    content: &[u8],
) -> Result<(String, String), Fail> {
    let made = cx
        .ok(
            who,
            RequestSpec::post("/v1/nodes")
                .tenant(cx.a().id)
                .json(json!({
                    "parent_id": parent.to_string(), "name": name, "kind": "file",
                })),
            &[201],
        )
        .await?;
    let node = str_at(&made, "/node/id")?;
    let version = put_file(cx, who, &node, content).await?;
    Ok((node, version))
}

/// Replaces the content of a file as `who` (`putContent`, a new version). Returns the version.
pub async fn put_file(cx: &Ctx, who: Who, node: &str, content: &[u8]) -> Result<String, Fail> {
    let put = cx
        .ok(
            who,
            RequestSpec::new(
                Method::PUT,
                format!("/v1/nodes/{node}/content?coalesce=false"),
            )
            .tenant(cx.a().id)
            .header("content-type", "application/octet-stream")
            .bytes(content.to_vec()),
            &[200],
        )
        .await?;
    str_at(&put, "/version/id")
}

/// The page of a static app.
pub const INDEX_HTML: &[u8] = b"<!doctype html><title>Plants</title><h1>Plants</h1>\n";

/// The smallest valid manifest.
pub const STATIC_MANIFEST: &str = "runtime: static\n";

/// A folder with the files of a static site, made by `who`.
#[derive(Clone, Debug)]
pub struct Site {
    pub folder: String,
    /// `(path, node_id, version_id)` of each file.
    pub files: Vec<(String, String, String)>,
}

/// Makes a site folder `name` in `parent` as `who` with `app.yaml` (`manifest`) and
/// `index.html`. Calls `createNode` and `putContent`.
pub async fn site(
    cx: &Ctx,
    who: Who,
    parent: impl std::fmt::Display,
    name: &str,
    manifest: &str,
) -> Result<Site, Fail> {
    let folder = new_folder(cx, who, parent, name).await?;
    let mut files = Vec::new();
    for (path, content) in [
        ("app.yaml", manifest.as_bytes()),
        ("index.html", INDEX_HTML),
    ] {
        let (node, version) = write_file(cx, who, &folder, path, content).await?;
        files.push((path.to_string(), node, version));
    }
    Ok(Site { folder, files })
}

/// Makes a folder as `who` (`createNode`) and returns its ID.
pub async fn new_folder(
    cx: &Ctx,
    who: Who,
    parent: impl std::fmt::Display,
    name: &str,
) -> Result<String, Fail> {
    let made = cx
        .ok(
            who,
            RequestSpec::post("/v1/nodes")
                .tenant(cx.a().id)
                .json(json!({
                    "parent_id": parent.to_string(), "name": name, "kind": "folder",
                })),
            &[201],
        )
        .await?;
    str_at(&made, "/node/id")
}

/// Waits (as `who`, with `getAppDeployment?wait=10`) until the deployment is in one of
/// `states`. A `failed` deployment that `states` does not name fails the case, with its stages.
pub async fn wait_state(
    cx: &Ctx,
    who: Who,
    app: &str,
    dep: &str,
    states: &[&str],
) -> Result<Value, Fail> {
    wait_state_scanned(cx, who, app, dep, states, &[]).await
}

/// [`wait_state`], with every answer scanned for `secrets` ([`ok_scanned`]).
pub async fn wait_state_scanned(
    cx: &Ctx,
    who: Who,
    app: &str,
    dep: &str,
    states: &[&str],
    secrets: &[String],
) -> Result<Value, Fail> {
    let start = Instant::now();
    loop {
        let asked = Instant::now();
        let d = ok_scanned(
            cx,
            who,
            get_deployment(cx, app, dep, Some(10)),
            &[200],
            secrets,
        )
        .await?;
        let state = d["state"].as_str().unwrap_or_default().to_string();
        if states.contains(&state.as_str()) {
            return Ok(d);
        }
        if state == "failed" {
            fail!("deployment {dep} failed: {}", d["stages"]);
        }
        check!(
            start.elapsed() < DEPLOY_WITHIN,
            "deployment {dep} is {state:?}, not {states:?}, after {DEPLOY_WITHIN:?}"
        );
        // An implementation that ignores `wait` must not be polled without a pause.
        if asked.elapsed() < Duration::from_secs(1) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

/// Starts a preview of `body` as `who` (`createAppDeployment`, `202`) and waits until it is
/// `ready`. Returns the deployment.
pub async fn preview(cx: &Ctx, who: Who, app: &str, body: Value) -> Result<Value, Fail> {
    preview_scanned(cx, who, app, body, &[]).await
}

/// [`preview`], with every answer scanned for `secrets` ([`ok_scanned`]).
pub async fn preview_scanned(
    cx: &Ctx,
    who: Who,
    app: &str,
    body: Value,
    secrets: &[String],
) -> Result<Value, Fail> {
    let made = ok_scanned(cx, who, create_deployment(cx, app, body), &[202], secrets).await?;
    check!(
        made["environment"] == "preview",
        "createAppDeployment did not start a preview: {made}"
    );
    let id = str_at(&made, "/id")?;
    wait_state_scanned(cx, who, app, &id, &["ready"], secrets).await
}

/// Publishes a deployment as a person (`publishAppDeployment`, `200`) and waits until the
/// deployment that the answer names is `live`. Returns it.
pub async fn go_live(cx: &Ctx, who: Who, app: &str, dep: &str) -> Result<Value, Fail> {
    go_live_scanned(cx, who, app, dep, &[]).await
}

/// [`go_live`], with every answer scanned for `secrets` ([`ok_scanned`]).
pub async fn go_live_scanned(
    cx: &Ctx,
    who: Who,
    app: &str,
    dep: &str,
    secrets: &[String],
) -> Result<Value, Fail> {
    let p = ok_scanned(cx, who, publish(cx, app, dep, json!({})), &[200], secrets).await?;
    let id = str_at(&p, "/id")?;
    let live = wait_state_scanned(cx, who, app, &id, &["live"], secrets).await?;
    check!(
        live["environment"] == "live" && live["version_id"].is_string(),
        "a live deployment has environment live and a version_id: {live}"
    );
    Ok(live)
}

/// A live app made by `member`: the app, a site folder with `manifest`, a ready preview and its
/// publish. Calls `createApp`, `createNode`, `putContent`, `createAppDeployment`,
/// `getAppDeployment` and `publishAppDeployment`.
#[derive(Clone, Debug)]
pub struct LiveApp {
    pub id: String,
    pub slug: String,
    /// The live deployment.
    pub live: Value,
}

impl LiveApp {
    pub fn deployment(&self) -> String {
        self.live["id"].as_str().unwrap_or_default().to_string()
    }

    pub fn version(&self) -> String {
        self.live["version_id"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }
}

pub async fn live_app(
    cx: &Ctx,
    parent: impl std::fmt::Display,
    name: &str,
    manifest: &str,
) -> Result<LiveApp, Fail> {
    live_app_scanned(cx, parent, name, manifest, &[]).await
}

/// [`live_app`], with every answer of `createApp`, `createAppDeployment`, `getAppDeployment`
/// and `publishAppDeployment` scanned for `secrets` ([`ok_scanned`]).
pub async fn live_app_scanned(
    cx: &Ctx,
    parent: impl std::fmt::Display,
    name: &str,
    manifest: &str,
    secrets: &[String],
) -> Result<LiveApp, Fail> {
    let parent = parent.to_string();
    let app = new_app_scanned(cx, Who::Member, &parent, name, secrets).await?;
    let id = app_id(&app)?;
    let slug = str_at(&app, "/slug")?;
    let s = site(cx, Who::Member, &parent, &format!("{name} site"), manifest).await?;
    let p = preview_scanned(cx, Who::Member, &id, from_folder(&s.folder, None), secrets).await?;
    let live = go_live_scanned(cx, Who::Member, &id, &str_at(&p, "/id")?, secrets).await?;
    Ok(LiveApp { id, slug, live })
}

/// Publishes a new manifest on a live app as `member`: a new site folder, a preview and its
/// publish. Returns the new live deployment.
pub async fn new_release(
    cx: &Ctx,
    app: &str,
    parent: impl std::fmt::Display,
    folder_name: &str,
    manifest: &str,
) -> Result<Value, Fail> {
    let s = site(cx, Who::Member, parent, folder_name, manifest).await?;
    let p = preview(cx, Who::Member, app, from_folder(&s.folder, None)).await?;
    go_live(cx, Who::Member, app, &str_at(&p, "/id")?).await
}

/// The IDs of the deployments of `app` (`listAppDeployments` as `member`, the first page).
pub async fn deployment_ids(cx: &Ctx, app: &str) -> Result<Vec<String>, Fail> {
    let page = cx
        .ok(Who::Member, list_deployments(cx, app, ""), &[200])
        .await?;
    Ok(items(&page)
        .iter()
        .filter_map(|d| d["id"].as_str().map(str::to_string))
        .collect())
}

/// Fails when `app` has a deployment that `before` does not list: a direct refusal leaves no
/// `pending` or `failed` deployment that a later stage could pick up (`listAppDeployments`).
pub async fn no_new_deployment(
    cx: &Ctx,
    app: &str,
    before: &[String],
    what: &str,
) -> Result<(), Fail> {
    let after = deployment_ids(cx, app).await?;
    let new: Vec<&String> = after.iter().filter(|d| !before.contains(d)).collect();
    check!(
        new.is_empty(),
        "{what}: the refusal left the deployments {new:?} behind"
    );
    Ok(())
}

/// Waits until `getApp` (as `member`) names `dep` as the live deployment.
pub async fn wait_live_is(cx: &Ctx, app: &str, dep: &str) -> Result<Value, Fail> {
    let start = Instant::now();
    loop {
        let a = cx.ok(Who::Member, get_app(cx, app), &[200]).await?;
        if a["live_deployment"]["id"] == dep {
            return Ok(a);
        }
        check!(
            start.elapsed() < DEPLOY_WITHIN,
            "the live deployment of {app} is {}, not {dep}, after {DEPLOY_WITHIN:?}",
            a["live_deployment"]["id"]
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The `items` of a page or list.
pub fn items(v: &Value) -> Vec<Value> {
    v["items"].as_array().cloned().unwrap_or_default()
}

/// The role `name` of an `AppRoleList`.
pub fn role<'a>(list: &'a [Value], name: &str) -> Option<&'a Value> {
    list.iter().find(|r| r["name"] == name)
}

/// The manifest with three app roles: `member` (default), `approver`, and `admin`
/// (`grants_roles`).
pub const ROLES_MANIFEST: &str = "runtime: static\n\
roles:\n  member:\n    description: Uses the plant list\n    default: true\n\
  approver:\n    description: Approves orders\n\
  admin:\n    description: Gives app roles\n    grants_roles: true\n";

/// [`ROLES_MANIFEST`] without `admin`.
pub const ROLES_MANIFEST_NO_ADMIN: &str = "runtime: static\n\
roles:\n  member:\n    description: Uses the plant list\n    default: true\n\
  approver:\n    description: Approves orders\n";

/// The secrets of the fixture: credentials and bearer tokens of both tenants. No answer may
/// hold one (design 9.9, card T-4a case 9). The values are never printed.
pub fn fixture_secrets(cx: &Ctx) -> Vec<String> {
    let mut v = Vec::new();
    for t in [&cx.w.a, &cx.w.b] {
        v.push(t.agent_key_secret.clone());
        v.push(t.app_password_secret.clone());
        v.push(t.share_link_token.clone());
        v.push(t.invitation_token.clone());
        v.push(t.issue_receipt_token.clone());
        for p in t.principals.values() {
            match &p.credentials {
                Credentials::Bearer(s) => v.push(s.clone()),
                Credentials::Session { session, csrf } => {
                    v.push(session.clone());
                    v.push(csrf.clone());
                }
                Credentials::None => {}
            }
        }
    }
    // The entered secret value of the standard requests of the isolation suite.
    v.push("sk_test_ruum_fixture".to_string());
    // A short value could match by chance; every real secret is longer.
    v.retain(|s| s.len() >= 12);
    v
}

/// Base64 of `data` without padding, standard (`+/`) or URL-safe (`-_`) alphabet.
fn base64(data: &[u8], url_safe: bool) -> String {
    let abc: &[u8; 64] = if url_safe {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
    } else {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
    };
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..=c.len() {
            out.push(abc[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// The forms of a secret that no answer may hold: the value itself, its hex (lower and upper
/// case), its percent-encoding (when it differs), and its base64 (standard and URL-safe) at
/// each of the three byte positions it can have in a longer base64 text. For base64 the
/// characters that also depend on the bytes before or after the secret are left out; a secret
/// has at least 12 bytes, so each base64 form keeps at least 8 characters.
pub fn secret_forms(secret: &str) -> Vec<String> {
    let b = secret.as_bytes();
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let mut v = vec![secret.to_string(), hex.to_uppercase(), hex];
    for upper in [true, false] {
        let pct: String = b
            .iter()
            .map(
                |&x| match (x.is_ascii_alphanumeric() || b"-._~".contains(&x), upper) {
                    (true, _) => (x as char).to_string(),
                    (false, true) => format!("%{x:02X}"),
                    (false, false) => format!("%{x:02x}"),
                },
            )
            .collect();
        if pct != secret {
            v.push(pct);
        }
    }
    for url_safe in [false, true] {
        for k in 0..3 {
            let mut data = vec![0u8; k];
            data.extend_from_slice(b);
            let enc = base64(&data, url_safe);
            let from = if k == 0 { 0 } else { 4 };
            let to = enc.len().saturating_sub(4);
            if to > from + 7 {
                v.push(enc[from..to].to_string());
            }
        }
    }
    v.sort();
    v.dedup();
    v
}

/// The number of the first of `secrets` that `text` holds in any of its [`secret_forms`].
pub fn leaked(text: &str, secrets: &[String]) -> Option<usize> {
    secrets
        .iter()
        .position(|s| secret_forms(s).iter().any(|f| text.contains(f.as_str())))
}

/// Sends `req` as `who` and scans the whole answer (body and headers) for `secrets` and their
/// encoded forms ([`secret_forms`]) **before** any check can print it; then the checks of
/// [`Ctx::call`]. A value the caller sent itself is not exempt: an answer that echoes an
/// entered secret leaks it too. Values are never printed.
pub async fn call_scanned(
    cx: &Ctx,
    who: &Principal,
    req: RequestSpec,
    secrets: &[String],
) -> Result<Response, Fail> {
    let label = format!("{} as {}", req.label(), who.who.name());
    if let Some(op) = operation_of(req.method.as_str(), &req.uri) {
        cx.called.lock().expect("lock").insert(op);
    }
    let res = client::send(&cx.app, &req, who).await;
    if let Some(i) = leaked(&res.text(), secrets) {
        fail!(
            "{label}: the answer ({}) holds secret number {i} of the list or an encoding of it \
             (value not printed)",
            res.status
        );
    }
    cx.vet(&label, res)
}

/// [`Ctx::ok`] through [`call_scanned`]: the answer is scanned for `secrets` before its status
/// is checked (a wrong status prints the answer). With no secrets it is [`Ctx::ok`].
pub async fn ok_scanned(
    cx: &Ctx,
    who: Who,
    req: RequestSpec,
    statuses: &[u16],
    secrets: &[String],
) -> Result<Value, Fail> {
    let p = cx.p(who).clone();
    let label = format!("{} as {}", req.label(), p.who.name());
    let res = call_scanned(cx, &p, req, secrets).await?;
    check!(
        statuses.contains(&res.status.as_u16()),
        "{label}: got {}, expected {statuses:?}: {}",
        res.status,
        res.text()
    );
    Ok(res.json().unwrap_or(Value::Null))
}

/// Sends `req` as `who` and fails, without printing any value, when the answer (of any status)
/// holds one of `secrets` ([`call_scanned`]). Returns the answer's status and JSON body.
pub async fn no_secret_in(
    cx: &Ctx,
    who: &Principal,
    req: RequestSpec,
    secrets: &[String],
) -> Result<(u16, Value), Fail> {
    let res = call_scanned(cx, who, req, secrets).await?;
    Ok((res.status.as_u16(), res.json().unwrap_or(Value::Null)))
}
