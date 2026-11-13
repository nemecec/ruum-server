//! App cases of card W-2: `createAppDeployment` of a static app from a folder or a file, the
//! direct `422` and `403` before the `202`, draft supersession, hostile archives, and secrets
//! that never appear in an answer. See [`super::apps`].

use super::apps::*;
use super::apps_archive::{self, TarEntry, TarKind, ZipEntry};
use super::{Case, Ctx, Fail};
use crate::{case, check, fail};
use ruum_testkit::client::Response;
use ruum_testkit::fixtures::Who;
use serde_json::{Value, json};

/// The JSON pointer of a direct `422`: `error.details.pointer`, a string that is empty or
/// starts with `/`.
fn pointer(err: &Value) -> Result<String, Fail> {
    match err["details"]["pointer"].as_str() {
        Some(p) if p.is_empty() || p.starts_with('/') => Ok(p.to_string()),
        _ => fail!("a bad manifest has no JSON pointer in error.details.pointer: {err}"),
    }
}

/// An app of `member` in `Projects`, and a site folder of its own with `manifest`.
async fn app_and_site(cx: &Ctx, name: &str, manifest: &str) -> Result<(String, Site), Fail> {
    let app = new_app(cx, Who::Member, cx.a().projects, name).await?;
    let s = site(
        cx,
        Who::Member,
        cx.a().projects,
        &format!("{name} site"),
        manifest,
    )
    .await?;
    Ok((app_id(&app)?, s))
}

/// Why an archive is hostile, which decides the answers that refuse it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hostile {
    /// A path that escapes, a link or a device file: a direct `422 validation_failed` only. A
    /// `413` is a size refusal and must not stand in for these.
    Entry,
    /// Too large or too many files when unpacked: a direct `413 payload_too_large` or `422
    /// validation_failed` (the contract names no code for limits that only unpacking finds).
    Size,
}

/// A hostile archive is refused by a direct answer (see [`Hostile`]): the stage `unpack` runs
/// before the answer (`createAppDeployment`), so a `202` is wrong even when the deployment
/// fails later. The refusal leaves no deployment behind (`listAppDeployments`).
async fn refused_archive(
    cx: &Ctx,
    app: &str,
    file_name: &str,
    bytes: &[u8],
    why: Hostile,
) -> Result<(), Fail> {
    let (file, _) = write_file(cx, Who::Member, cx.a().projects, file_name, bytes).await?;
    let before = deployment_ids(cx, app).await?;
    let res = cx
        .send(Who::Member, create_deployment(cx, app, from_file(&file)))
        .await?;
    let code = res.error_code().unwrap_or_default();
    let got = (res.status.as_u16(), code.as_str());
    let (ok, expected) = match why {
        Hostile::Entry => (
            got == (422, "validation_failed"),
            "a direct 422 validation_failed",
        ),
        Hostile::Size => (
            matches!(got, (413, "payload_too_large") | (422, "validation_failed")),
            "a direct 413 payload_too_large or 422 validation_failed",
        ),
    };
    check!(
        ok,
        "{file_name}: got {} {code:?}, expected {expected} (unpack runs before the answer): {}",
        res.status,
        res.text()
    );
    no_new_deployment(cx, app, &before, file_name).await
}

/// The status and the whole `error` object (code, message, hint, details) of an answer, with
/// `id` (the node or version ID that the request named) written as `<id>`: two refusals that
/// must not be told apart have the same normal form.
fn normal(res: &Response, id: &str) -> (u16, String) {
    let e = res
        .json()
        .map(|v| v["error"].clone())
        .unwrap_or(Value::Null);
    (res.status.as_u16(), e.to_string().replace(id, "<id>"))
}

/// Fails when `res` holds one of `markers` (names or IDs of a node the caller must not see).
fn holds_none(res: &Response, markers: &[&str], what: &str) -> Result<(), Fail> {
    let text = res.text();
    for m in markers {
        check!(
            !text.contains(m),
            "{what}: the answer names the hidden {m:?}: {text}"
        );
    }
    Ok(())
}

pub fn cases() -> Vec<Case> {
    vec![
        case!(
            "bad_manifest_is_a_direct_422_with_a_pointer",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "listAppDeployments"
            ],
            "createAppDeployment answers 422 validation_failed before any 202 for an unknown \
             runtime (pointer /runtime), an env name with the RUUM_ prefix (pointer under /env), \
             a static app with start (a schema rule), two default roles (a provider rule, \
             pointer under /roles) and a folder without app.yaml; each with \
             error.details.pointer where a key is wrong; afterwards listAppDeployments lists no \
             deployment (a direct refusal leaves nothing behind)",
            |cx| {
                let (app, s) = app_and_site(&cx, "bad manifests", "runtime: cobol\n").await?;
                let manifest = &s.files[0].1;
                let deploy = || create_deployment(&cx, &app, from_folder(&s.folder, None));
                let err = cx
                    .refused(Who::Member, deploy(), 422, &["validation_failed"])
                    .await?;
                check!(
                    pointer(&err)? == "/runtime",
                    "the pointer of runtime: {err}"
                );
                for (yaml, under) in [
                    ("runtime: static\nenv:\n  RUUM_TOKEN: x\n", "/env"),
                    (
                        "runtime: static\nroles:\n  a:\n    description: A\n    default: true\n  \
                         b:\n    description: B\n    default: true\n",
                        "/roles",
                    ),
                ] {
                    put_file(&cx, Who::Member, manifest, yaml.as_bytes()).await?;
                    let err = cx
                        .refused(Who::Member, deploy(), 422, &["validation_failed"])
                        .await?;
                    let p = pointer(&err)?;
                    check!(
                        p.starts_with(under),
                        "the pointer {p:?} is not under {under}: {err}"
                    );
                }
                put_file(
                    &cx,
                    Who::Member,
                    manifest,
                    b"runtime: static\nstart: node x.js\n",
                )
                .await?;
                let err = cx
                    .refused(Who::Member, deploy(), 422, &["validation_failed"])
                    .await?;
                pointer(&err)?;
                let empty = new_folder(&cx, Who::Member, cx.a().projects, "no manifest").await?;
                write_file(&cx, Who::Member, &empty, "index.html", INDEX_HTML).await?;
                cx.refused(
                    Who::Member,
                    create_deployment(&cx, &app, from_folder(&empty, None)),
                    422,
                    &["validation_failed"],
                )
                .await?;
                no_new_deployment(&cx, &app, &[], "a bad manifest").await
            }
        ),
        case!(
            "refused_binding_is_a_direct_403_naming_the_binding",
            [
                "createApp",
                "createNode",
                "putContent",
                "addGrant",
                "createAppDeployment",
                "listAppDeployments",
            ],
            "guest_view (edit on a folder of its own, view on the fixture database) deploys an \
             app with the binding db (edit on that database): 403 permission_denied, hint \
             binding_not_allowed, error.details.binding = db, before any 202 and with no \
             deployment left (design 9.4)",
            |cx| {
                let folder = new_folder(&cx, Who::Member, cx.a().projects, "guest work").await?;
                super::api::add_grant(&cx, &folder, Who::GuestView, "edit").await?;
                let app = new_app(&cx, Who::Member, &folder, "guest app").await?;
                let id = app_id(&app)?;
                let yaml = format!(
                    "runtime: static\nbindings:\n  db:\n    node: node:{}\n    role: edit\n",
                    cx.a().database
                );
                let s = site(&cx, Who::GuestView, &folder, "guest site", &yaml).await?;
                let err = cx
                    .refused(
                        Who::GuestView,
                        create_deployment(&cx, &id, from_folder(&s.folder, None)),
                        403,
                        &["permission_denied"],
                    )
                    .await?;
                super::api::hint_has(&err, "binding_not_allowed")?;
                check!(
                    err["details"]["binding"] == "db",
                    "error.details.binding: {err}"
                );
                no_new_deployment(&cx, &id, &[], "a refused binding").await
            }
        ),
        case!(
            "agent_binding_outside_its_scope_is_refused",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "listAppDeployments"
            ],
            "agent_out (scope Private, manage) deploys an app in Private with the binding db \
             (view on the fixture database in Projects, which its user member may view but its \
             scope does not reach): 403 permission_denied, hint binding_not_allowed, \
             error.details.binding = db, before any 202 and with no deployment left (design \
             9.4: an agent's right is its user's right intersected with its scope)",
            |cx| {
                let app = new_app(&cx, Who::Member, cx.a().private, "bot bound").await?;
                let id = app_id(&app)?;
                let yaml = format!(
                    "runtime: static\nbindings:\n  db:\n    node: node:{}\n    role: view\n",
                    cx.a().database
                );
                let s = site(&cx, Who::AgentOut, cx.a().private, "bot bound site", &yaml).await?;
                let err = cx
                    .refused(
                        Who::AgentOut,
                        create_deployment(&cx, &id, from_folder(&s.folder, None)),
                        403,
                        &["permission_denied"],
                    )
                    .await?;
                super::api::hint_has(&err, "binding_not_allowed")?;
                check!(
                    err["details"]["binding"] == "db",
                    "error.details.binding: {err}"
                );
                no_new_deployment(&cx, &id, &[], "a refused agent binding").await
            }
        ),
        case!(
            "deploy_source_needs_view_and_its_own_version",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "listAppDeployments"
            ],
            "createAppDeployment packs only a source that the caller can view, at a version of \
             that node: guest_edit (edit on Projects, so on the app) deploying member's folder \
             and archive in Private gets the same status, code and hint as for a random node \
             ID (status and the whole error: code, message, hint, details, with the ID written \
             as <id>), and no answer names the hidden nodes; a file it can view with the \
             version_id of the hidden file (as guest_edit and as member) gets the same answer as \
             with a random version ID, a client error; a \
             folder source with a version_id, the source kind build and the draft Draft_1 are \
             422 validation_failed; no request leaves a deployment",
            |cx| {
                let app = app_id(&new_app(&cx, Who::Member, cx.a().projects, "sources").await?)?;
                let tgz = apps_archive::tar_gz(&[
                    ("app.yaml", STATIC_MANIFEST.as_bytes()),
                    ("index.html", INDEX_HTML),
                ]);
                let hidden = site(
                    &cx,
                    Who::Member,
                    cx.a().private,
                    "hidden site",
                    STATIC_MANIFEST,
                )
                .await?;
                let (hidden_file, hidden_version) =
                    write_file(&cx, Who::Member, cx.a().private, "hidden.tar.gz", &tgz).await?;
                let (open_file, _) =
                    write_file(&cx, Who::Member, cx.a().projects, "open.tar.gz", &tgz).await?;
                let open = site(
                    &cx,
                    Who::Member,
                    cx.a().projects,
                    "open site",
                    STATIC_MANIFEST,
                )
                .await?;
                let missing = uuid::Uuid::now_v7().to_string();
                let hidden_markers = [
                    "hidden site",
                    "hidden.tar.gz",
                    hidden.files[0].1.as_str(),
                    hidden.files[1].1.as_str(),
                    hidden_version.as_str(),
                ];
                for (kind, node) in [("folder", &hidden.folder), ("file", &hidden_file)] {
                    let body = |id: &str| json!({ "source": { "kind": kind, "node_id": id } });
                    let seen = cx
                        .send(Who::GuestEdit, create_deployment(&cx, &app, body(node)))
                        .await?;
                    let none = cx
                        .send(Who::GuestEdit, create_deployment(&cx, &app, body(&missing)))
                        .await?;
                    check!(
                        seen.status.is_client_error(),
                        "guest_edit deployed a {kind} it cannot view: {} {}",
                        seen.status,
                        seen.text()
                    );
                    check!(
                        normal(&seen, node) == normal(&none, &missing),
                        "a hidden {kind} and a missing one differ: {:?} and {:?}",
                        normal(&seen, node),
                        normal(&none, &missing)
                    );
                    holds_none(&seen, &hidden_markers, kind)?;
                }
                for who in [Who::GuestEdit, Who::Member] {
                    let other = cx
                        .send(
                            who,
                            create_deployment(
                                &cx,
                                &app,
                                from_file_version(&open_file, &hidden_version),
                            ),
                        )
                        .await?;
                    let none = cx
                        .send(
                            who,
                            create_deployment(&cx, &app, from_file_version(&open_file, &missing)),
                        )
                        .await?;
                    check!(
                        other.status.is_client_error(),
                        "{}: a version of another file was accepted: {} {}",
                        who.name(),
                        other.status,
                        other.text()
                    );
                    check!(
                        normal(&other, &hidden_version) == normal(&none, &missing),
                        "{}: a version of another file and a missing version differ: {:?} and \
                         {:?}",
                        who.name(),
                        normal(&other, &hidden_version),
                        normal(&none, &missing)
                    );
                    if who == Who::GuestEdit {
                        holds_none(&other, &["hidden.tar.gz", hidden_file.as_str()], "version")?;
                    }
                }
                let mut folder_version = from_folder(&open.folder, None);
                folder_version["source"]["version_id"] = json!(open.files[0].2);
                let build = json!({ "source": { "kind": "build", "node_id": open.folder } });
                let draft = from_folder(&open.folder, Some("Draft_1"));
                for body in [folder_version, build, draft] {
                    cx.refused(
                        Who::Member,
                        create_deployment(&cx, &app, body),
                        422,
                        &["validation_failed"],
                    )
                    .await?;
                }
                no_new_deployment(&cx, &app, &[], "a refused source").await
            }
        ),
        case!(
            "deploy_source_outside_an_agents_scope_fails_like_a_missing_node",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "listAppDeployments"
            ],
            "agent_out (scope Private, manage) deploys to an app in Private: a folder and an \
             archive file of member in Projects (its user member may view them, its scope does \
             not reach them) get the same status and whole error as a random node ID (with the \
             ID written as <id>), name neither source, and leave no deployment; a folder of \
             member in Private (in its scope) starts a preview (202): an agent's right is its \
             user's right intersected with its scope (design 9.4)",
            |cx| {
                let app = app_id(&new_app(&cx, Who::Member, cx.a().private, "bot sources").await?)?;
                let tgz = apps_archive::tar_gz(&[
                    ("app.yaml", STATIC_MANIFEST.as_bytes()),
                    ("index.html", INDEX_HTML),
                ]);
                let outside = site(
                    &cx,
                    Who::Member,
                    cx.a().projects,
                    "team site",
                    STATIC_MANIFEST,
                )
                .await?;
                let (outside_file, _) =
                    write_file(&cx, Who::Member, cx.a().projects, "team.tar.gz", &tgz).await?;
                let missing = uuid::Uuid::now_v7().to_string();
                for (kind, node) in [("folder", &outside.folder), ("file", &outside_file)] {
                    let body = |id: &str| json!({ "source": { "kind": kind, "node_id": id } });
                    let seen = cx
                        .send(Who::AgentOut, create_deployment(&cx, &app, body(node)))
                        .await?;
                    let none = cx
                        .send(Who::AgentOut, create_deployment(&cx, &app, body(&missing)))
                        .await?;
                    check!(
                        seen.status.is_client_error(),
                        "agent_out deployed a {kind} outside its scope: {} {}",
                        seen.status,
                        seen.text()
                    );
                    check!(
                        normal(&seen, node) == normal(&none, &missing),
                        "a {kind} outside the scope and a missing one differ: {:?} and {:?}",
                        normal(&seen, node),
                        normal(&none, &missing)
                    );
                    holds_none(&seen, &["team site", "team.tar.gz"], kind)?;
                }
                no_new_deployment(&cx, &app, &[], "a source outside the agent's scope").await?;
                let own = site(
                    &cx,
                    Who::Member,
                    cx.a().private,
                    "own site",
                    STATIC_MANIFEST,
                )
                .await?;
                let made = cx
                    .ok(
                        Who::AgentOut,
                        create_deployment(&cx, &app, from_folder(&own.folder, None)),
                        &[202],
                    )
                    .await?;
                check!(
                    made["environment"] == "preview",
                    "agent_out's deployment in its scope is not a preview: {made}"
                );
                Ok(())
            }
        ),
        case!(
            "viewers_see_no_stage_messages_check_or_previews",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "getApp"
            ],
            "a ready preview and a preview whose check route /missing.html does not exist \
             (ready or failed): member (edit) sees the check result or a stage message of the \
             second one; member_view (view) gets both from getAppDeployment with every \
             stages[].message and check null or left out, and getApp with previews empty, \
             while member's getApp lists the ready preview (getAppDeployment: the check result \
             and stage messages need edit; App.previews: callers with edit only)",
            |cx| {
                let (app, s) = app_and_site(&cx, "details", STATIC_MANIFEST).await?;
                let ready = preview(
                    &cx,
                    Who::Member,
                    &app,
                    from_folder(&s.folder, Some("plain")),
                )
                .await?;
                let ready = str_at(&ready, "/id")?;
                let broken = site(
                    &cx,
                    Who::Member,
                    cx.a().projects,
                    "details broken",
                    "runtime: static\ncheck:\n  routes: [/missing.html]\n",
                )
                .await?;
                let made = cx
                    .ok(
                        Who::Member,
                        create_deployment(&cx, &app, from_folder(&broken.folder, Some("broken"))),
                        &[202],
                    )
                    .await?;
                let broken = str_at(&made, "/id")?;
                let d = wait_state(&cx, Who::Member, &app, &broken, &["ready", "failed"]).await?;
                let has_message = |d: &Value| {
                    d["stages"]
                        .as_array()
                        .is_some_and(|v| v.iter().any(|x| x["message"].is_string()))
                };
                check!(
                    d["check"].is_object() || has_message(&d),
                    "member (edit) sees neither a check result nor a stage message of a \
                     deployment whose check route is missing: {d}"
                );
                for dep in [&ready, &broken] {
                    let v = cx
                        .ok(
                            Who::MemberView,
                            get_deployment(&cx, &app, dep, None),
                            &[200],
                        )
                        .await?;
                    check!(
                        v["check"].is_null() && !has_message(&v),
                        "member_view (view) sees the check result or a stage message of \
                         {dep}: check {}, stages {}",
                        v["check"],
                        v["stages"]
                    );
                }
                let a = cx.ok(Who::MemberView, get_app(&cx, &app), &[200]).await?;
                check!(
                    a["previews"].as_array().is_some_and(Vec::is_empty),
                    "member_view (view) sees previews: {}",
                    a["previews"]
                );
                let a = cx.ok(Who::Member, get_app(&cx, &app), &[200]).await?;
                check!(
                    a["previews"]
                        .as_array()
                        .is_some_and(|p| p.iter().any(|x| x["id"] == ready.as_str())),
                    "member (edit) does not see the ready preview: {}",
                    a["previews"]
                );
                Ok(())
            }
        ),
        case!(
            "binding_to_another_tenants_node_fails_like_a_missing_node",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "listAppDeployments"
            ],
            "a binding to node:<id> of tenant B's plan file and one to node:<random id> get the \
             same status and the same whole error (code, message, hint, details, with the ID \
             written as <id>), the answer holds no marker of tenant B, and neither leaves a \
             deployment (createAppDeployment: like a missing node)",
            |cx| {
                let (app, s) = app_and_site(&cx, "bound", STATIC_MANIFEST).await?;
                let manifest = &s.files[0].1;
                let mut answers = Vec::new();
                for target in [cx.w.b.plan.to_string(), uuid::Uuid::now_v7().to_string()] {
                    let yaml = format!(
                        "runtime: static\nbindings:\n  docs:\n    node: node:{target}\n    role: view\n"
                    );
                    put_file(&cx, Who::Member, manifest, yaml.as_bytes()).await?;
                    let res = cx
                        .send(
                            Who::Member,
                            create_deployment(&cx, &app, from_folder(&s.folder, None)),
                        )
                        .await?;
                    check!(
                        res.status.is_client_error(),
                        "a binding to {target} was accepted: {} {}",
                        res.status,
                        res.text()
                    );
                    let text = res.text();
                    for m in cx.w.b.markers() {
                        check!(
                            m.len() < 8 || m == target || !text.contains(&m),
                            "the answer holds a marker of tenant B: {text}"
                        );
                    }
                    answers.push(normal(&res, &target));
                }
                check!(
                    answers[0] == answers[1],
                    "tenant B's node and a missing node differ: {answers:?}"
                );
                no_new_deployment(&cx, &app, &[], "a binding to another tenant").await
            }
        ),
        case!(
            "file_binding_with_edit_is_refused",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "listAppDeployments"
            ],
            "a binding with role edit on a folder (member has manage on it) is refused before \
             any 202 (403 or 422) and leaves no deployment: in step 3 a file or folder binding \
             is view",
            |cx| {
                let (app, s) = app_and_site(&cx, "writer", STATIC_MANIFEST).await?;
                let yaml = format!(
                    "runtime: static\nbindings:\n  docs:\n    node: node:{}\n    role: edit\n",
                    cx.a().projects
                );
                put_file(&cx, Who::Member, &s.files[0].1, yaml.as_bytes()).await?;
                let res = cx
                    .send(
                        Who::Member,
                        create_deployment(&cx, &app, from_folder(&s.folder, None)),
                    )
                    .await?;
                check!(
                    matches!(res.status.as_u16(), 403 | 422),
                    "an edit binding on a folder: {} {}",
                    res.status,
                    res.text()
                );
                no_new_deployment(&cx, &app, &[], "an edit binding on a folder").await
            }
        ),
        case!(
            "folder_source_records_its_file_versions",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "getApp",
            ],
            "a static preview from a folder: 202 with environment preview and source kind \
             folder; it reaches ready with the in-memory driver; source.files names each file \
             version with its path and source.version_id is null; the stages build and test are \
             skipped, unpack and validate done (bindings done or skipped); the address is \
             <8 base32>--<slug>.<tenant>...; getApp lists it in previews",
            |cx| {
                let (app, s) = app_and_site(&cx, "folder app", STATIC_MANIFEST).await?;
                let a = cx.ok(Who::Member, get_app(&cx, &app), &[200]).await?;
                let slug = str_at(&a, "/slug")?;
                let made = cx
                    .ok(
                        Who::Member,
                        create_deployment(&cx, &app, from_folder(&s.folder, None)),
                        &[202],
                    )
                    .await?;
                check!(
                    made["environment"] == "preview"
                        && made["source"]["kind"] == "folder"
                        && made["source"]["node_id"] == s.folder.as_str()
                        && made["version_id"].is_null(),
                    "the 202: {made}"
                );
                let dep = str_at(&made, "/id")?;
                let d = wait_state(&cx, Who::Member, &app, &dep, &["ready"]).await?;
                check!(
                    d["source"]["version_id"].is_null(),
                    "source: {}",
                    d["source"]
                );
                let files = d["source"]["files"].as_array().cloned().unwrap_or_default();
                check!(
                    files.len() == s.files.len(),
                    "source.files: {}",
                    d["source"]
                );
                for (path, node, version) in &s.files {
                    check!(
                        files.iter().any(|f| f["path"] == path.as_str()
                            && f["node_id"] == node.as_str()
                            && f["version_id"] == version.as_str()),
                        "source.files does not name {path} at version {version}: {}",
                        d["source"]
                    );
                }
                let stage = |name: &str| {
                    d["stages"]
                        .as_array()
                        .and_then(|v| v.iter().find(|x| x["name"] == name))
                        .map(|x| x["state"].clone())
                        .unwrap_or(Value::Null)
                };
                for name in ["build", "test"] {
                    check!(stage(name) == "skipped", "stage {name}: {}", d["stages"]);
                }
                for name in ["unpack", "validate"] {
                    check!(stage(name) == "done", "stage {name}: {}", d["stages"]);
                }
                // No binding: the contract does not say whether the stage is done or skipped.
                check!(
                    stage("bindings") == "done" || stage("bindings") == "skipped",
                    "stage bindings: {}",
                    d["stages"]
                );
                let url = str_at(&d, "/url")?;
                if let Some(p) = preview_url_problem(&url, &slug) {
                    fail!("{p}");
                }
                let a = cx.ok(Who::Member, get_app(&cx, &app), &[200]).await?;
                check!(
                    a["previews"]
                        .as_array()
                        .is_some_and(|p| p.iter().any(|x| x["id"] == dep.as_str())),
                    "getApp does not list the ready preview: {a}"
                );
                Ok(())
            }
        ),
        case!(
            "file_source_deploys_a_tar_gz_and_a_zip",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
            ],
            "a .tar.gz and a .zip with app.yaml at the root, each from a file node: the preview \
             reaches ready and source names the file and the version used",
            |cx| {
                let app = new_app(&cx, Who::Member, cx.a().projects, "archived").await?;
                let id = app_id(&app)?;
                let files: [(&str, &[u8]); 2] = [
                    ("app.yaml", STATIC_MANIFEST.as_bytes()),
                    ("index.html", INDEX_HTML),
                ];
                let tgz = apps_archive::tar_gz(&files);
                let zip = apps_archive::zip(&[
                    ZipEntry::file("app.yaml", STATIC_MANIFEST.as_bytes()),
                    ZipEntry::file("index.html", INDEX_HTML),
                ]);
                for (name, bytes) in [("site.tar.gz", tgz), ("site.zip", zip)] {
                    let (file, version) =
                        write_file(&cx, Who::Member, cx.a().projects, name, &bytes).await?;
                    let d = preview(&cx, Who::Member, &id, from_file(&file)).await?;
                    check!(
                        d["source"]["kind"] == "file"
                            && d["source"]["node_id"] == file.as_str()
                            && d["source"]["version_id"] == version.as_str(),
                        "{name}: source {}",
                        d["source"]
                    );
                }
                Ok(())
            }
        ),
        case!(
            "same_draft_supersedes_the_older_preview",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "getApp",
            ],
            "a new preview with the draft label sortable supersedes the older ready one (state \
             superseded); a preview with another draft label stays ready; getApp lists only \
             the ready previews",
            |cx| {
                let (app, s) = app_and_site(&cx, "drafts", STATIC_MANIFEST).await?;
                let first = preview(
                    &cx,
                    Who::Member,
                    &app,
                    from_folder(&s.folder, Some("sortable")),
                )
                .await?;
                let other = preview(
                    &cx,
                    Who::Member,
                    &app,
                    from_folder(&s.folder, Some("colours")),
                )
                .await?;
                let second = preview(
                    &cx,
                    Who::Member,
                    &app,
                    from_folder(&s.folder, Some("sortable")),
                )
                .await?;
                let first_id = str_at(&first, "/id")?;
                let old = wait_state(&cx, Who::Member, &app, &first_id, &["superseded"]).await?;
                check!(old["draft"] == "sortable", "draft: {old}");
                let o = cx
                    .ok(
                        Who::Member,
                        get_deployment(&cx, &app, str_at(&other, "/id")?, None),
                        &[200],
                    )
                    .await?;
                check!(o["state"] == "ready", "the preview of another draft: {o}");
                let a = cx.ok(Who::Member, get_app(&cx, &app), &[200]).await?;
                let ids: Vec<Value> = a["previews"]
                    .as_array()
                    .map(|p| p.iter().map(|x| x["id"].clone()).collect())
                    .unwrap_or_default();
                check!(
                    ids.contains(&second["id"])
                        && ids.contains(&other["id"])
                        && !ids.contains(&first["id"]),
                    "getApp previews: {ids:?}"
                );
                Ok(())
            }
        ),
        case!(
            "preview_expiry_is_visible",
            ["createApp", "createAppDeployment", "getAppDeployment"],
            "open question (see the pending entry): a preview expires after 7 days without a \
             request; which answer shows when?",
            |cx| {
                let _ = &cx;
                fail!("the contract has no expiry field yet; answer the question first")
            }
        ),
        case!(
            "zip_slip_is_refused",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "listAppDeployments"
            ],
            "a .zip with app.yaml and an entry that escapes is refused at unpack with a \
             direct 422 validation_failed and no deployment left, for each of: \
             ../../outside.html, the absolute /etc/ruum.html, the Windows forms \
             ..\\..\\outside.html and dist\\..\\..\\outside.html, the drive paths \
             C:\\ruum\\outside.html and C:/ruum/outside.html, and the UNC paths \
             \\\\server\\share\\outside.html and //server/share/outside.html (decision 0006 H)",
            |cx| {
                let app = app_id(&new_app(&cx, Who::Member, cx.a().projects, "slip").await?)?;
                for (file, evil) in apps_archive::ZIP_ESCAPES {
                    let bytes = apps_archive::zip(&[
                        ZipEntry::file("app.yaml", STATIC_MANIFEST.as_bytes()),
                        ZipEntry::file(evil, INDEX_HTML),
                    ]);
                    refused_archive(&cx, &app, file, &bytes, Hostile::Entry).await?;
                }
                Ok(())
            }
        ),
        case!(
            "tar_slip_is_refused",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "listAppDeployments"
            ],
            "a .tar.gz with app.yaml and an entry that escapes is refused at unpack with a \
             direct 422 validation_failed and no deployment left, for each of: \
             ../outside.html, dist/../../outside.html, the absolute /etc/ruum.html, the \
             Windows forms ..\\outside.html and dist\\..\\..\\outside.html, the drive path \
             C:\\ruum\\outside.html and the UNC path \\\\server\\share\\outside.html \
             (decision 0006 H)",
            |cx| {
                let app = app_id(&new_app(&cx, Who::Member, cx.a().projects, "tar slip").await?)?;
                for (file, evil) in apps_archive::TAR_ESCAPES {
                    let bytes = apps_archive::tar_gz(&[
                        ("app.yaml", STATIC_MANIFEST.as_bytes()),
                        (evil, INDEX_HTML),
                    ]);
                    refused_archive(&cx, &app, file, &bytes, Hostile::Entry).await?;
                }
                Ok(())
            }
        ),
        case!(
            "links_in_an_archive_are_refused",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "listAppDeployments"
            ],
            "a .tar.gz with a symbolic link (index.html -> /etc/passwd), one with a hard link \
             (to ../../secret), and a .zip with a symbolic link entry are refused at unpack \
             (a direct 422 validation_failed, no deployment left)",
            |cx| {
                let app = app_id(&new_app(&cx, Who::Member, cx.a().projects, "links").await?)?;
                let manifest = TarEntry::file("app.yaml", STATIC_MANIFEST.as_bytes());
                let sym = apps_archive::gzip(&apps_archive::tar(&[
                    TarEntry::file("app.yaml", STATIC_MANIFEST.as_bytes()),
                    TarEntry::special("index.html", TarKind::SymLink, "/etc/passwd"),
                ]));
                refused_archive(&cx, &app, "symlink.tar.gz", &sym, Hostile::Entry).await?;
                let hard = apps_archive::gzip(&apps_archive::tar(&[
                    manifest,
                    TarEntry::special("index.html", TarKind::HardLink, "../../secret"),
                ]));
                refused_archive(&cx, &app, "hardlink.tar.gz", &hard, Hostile::Entry).await?;
                let zip = apps_archive::zip(&[
                    ZipEntry::file("app.yaml", STATIC_MANIFEST.as_bytes()),
                    ZipEntry {
                        name: "index.html",
                        data: b"/etc/passwd",
                        unix_mode: Some(0o120_777),
                    },
                ]);
                refused_archive(&cx, &app, "symlink.zip", &zip, Hostile::Entry).await
            }
        ),
        case!(
            "device_files_in_an_archive_are_refused",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "listAppDeployments"
            ],
            "a .tar.gz with a character device, one with a block device and one with a FIFO \
             are refused at unpack \
             (a direct 422 validation_failed, no deployment left)",
            |cx| {
                let app = app_id(&new_app(&cx, Who::Member, cx.a().projects, "devices").await?)?;
                for (file, kind) in [
                    ("char.tar.gz", TarKind::CharDevice),
                    ("block.tar.gz", TarKind::BlockDevice),
                    ("fifo.tar.gz", TarKind::Fifo),
                ] {
                    let bytes = apps_archive::gzip(&apps_archive::tar(&[
                        TarEntry::file("app.yaml", STATIC_MANIFEST.as_bytes()),
                        TarEntry::special("dev", kind, ""),
                    ]));
                    refused_archive(&cx, &app, file, &bytes, Hostile::Entry).await?;
                }
                Ok(())
            }
        ),
        case!(
            "archive_bomb_is_refused",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "listAppDeployments"
            ],
            "a valid .tar.gz of about 4 MiB that expands to a 4 GiB file, and a valid .zip \
             whose deflated entry of about 2 MiB expands to 2 GiB (true sizes and CRC), are \
             refused at unpack (size limits), not unpacked: a direct 413 or 422, no deployment \
             left",
            |cx| {
                let app = app_id(&new_app(&cx, Who::Member, cx.a().projects, "bomb").await?)?;
                refused_archive(
                    &cx,
                    &app,
                    "bomb.tar.gz",
                    &apps_archive::bomb_tar_gz(),
                    Hostile::Size,
                )
                .await?;
                refused_archive(
                    &cx,
                    &app,
                    "bomb.zip",
                    &apps_archive::bomb_zip(),
                    Hostile::Size,
                )
                .await
            }
        ),
        case!(
            "entry_count_bomb_is_refused",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "listAppDeployments"
            ],
            "a .zip (a few MiB) and a stored .tar.gz (about 32 MiB, under every plan's archive \
             limit) with app.yaml and 65,000 empty files are refused at unpack (the file count \
             limit): a direct 413 or 422, no deployment left",
            |cx| {
                let app = app_id(&new_app(&cx, Who::Member, cx.a().projects, "many").await?)?;
                refused_archive(
                    &cx,
                    &app,
                    "many.zip",
                    &apps_archive::count_bomb_zip(),
                    Hostile::Size,
                )
                .await?;
                refused_archive(
                    &cx,
                    &app,
                    "many.tar.gz",
                    &apps_archive::count_bomb_tar_gz(),
                    Hostile::Size,
                )
                .await
            }
        ),
        case!(
            "manifest_paths_that_escape_the_archive_are_refused",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "listAppDeployments"
            ],
            "every manifest field that names a path in the archive (static.root, \
             static.spa_fallback, a migrations folder, an API spec, telemetry.privacy_file and \
             telemetry.source_maps) with a .. segment, a leading / or a \\ is a direct 422 \
             validation_failed whose error.details.pointer is that field, with no deployment \
             left: the archive was unpacked safely, the manifest must not lead out of it \
             (app.schema.json archive_path, decision 0006 H)",
            |cx| {
                let (app, s) = app_and_site(&cx, "escapes", STATIC_MANIFEST).await?;
                let manifest = &s.files[0].1;
                let api = |spec: &str| {
                    format!(
                        "runtime: static\napis:\n  orders:\n    visibility: internal\n    \
                         protocol: http\n    spec: '{spec}'\n    scopes: [orders.read]\n"
                    )
                };
                let mut cases: Vec<(String, &str)> = Vec::new();
                for bad in ["../../outside", "/etc", "dist\\..\\..\\outside"] {
                    cases.push((
                        format!("runtime: static\nstatic:\n  root: '{bad}'\n"),
                        "/static/root",
                    ));
                }
                for bad in ["../index.html", "/index.html", "dist/../../index.html"] {
                    cases.push((
                        format!("runtime: static\nstatic:\n  spa_fallback: '{bad}'\n"),
                        "/static/spa_fallback",
                    ));
                }
                for bad in ["../migrations", "/migrations", "..\\migrations"] {
                    cases.push((
                        format!("runtime: static\nmigrations:\n  db: '{bad}'\n"),
                        "/migrations/db",
                    ));
                }
                cases.push((api("../orders.yaml"), "/apis/orders/spec"));
                cases.push((api("/orders.yaml"), "/apis/orders/spec"));
                for bad in ["../privacy.yaml", "/privacy.yaml"] {
                    cases.push((
                        format!("runtime: static\ntelemetry:\n  privacy_file: '{bad}'\n"),
                        "/telemetry/privacy_file",
                    ));
                }
                for bad in ["../**/*.map", "/maps/*.map"] {
                    cases.push((
                        format!("runtime: static\ntelemetry:\n  source_maps: '{bad}'\n"),
                        "/telemetry/source_maps",
                    ));
                }
                for (yaml, field) in &cases {
                    put_file(&cx, Who::Member, manifest, yaml.as_bytes()).await?;
                    let err = cx
                        .refused(
                            Who::Member,
                            create_deployment(&cx, &app, from_folder(&s.folder, None)),
                            422,
                            &["validation_failed"],
                        )
                        .await?;
                    let p = pointer(&err)?;
                    check!(
                        p == *field,
                        "the pointer {p:?} is not {field} for the manifest {yaml:?}: {err}"
                    );
                }
                no_new_deployment(&cx, &app, &[], "a manifest path that escapes").await
            }
        ),
        case!(
            "no_answer_holds_a_fixture_secret",
            [
                "createApp",
                "getApp",
                "updateAppSettings",
                "listAppDeployments",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "listAppRoles",
                "listAppRoleGrants",
                "addAppRoleGrant",
                "listVersions",
            ],
            "no answer of the M1 operations (to member, member_view, agent and the owner) holds \
             a secret of the fixture, raw or encoded (hex, percent, base64): agent keys, app \
             passwords, link, invitation and receipt tokens, bearer tokens and session values \
             (design 9.9); this covers the answers of createApp, createAppDeployment, \
             getAppDeployment and publishAppDeployment while the app is set up, and refusals \
             (a missing source, a missing deployment); every answer is scanned before a check \
             can print it, and the values are never printed",
            |cx| {
                let secrets = fixture_secrets(&cx);
                let live =
                    live_app_scanned(&cx, cx.a().projects, "quiet", ROLES_MANIFEST, &secrets)
                        .await?;
                let dep = live.deployment();
                let missing = uuid::Uuid::now_v7().to_string();
                let mut reqs = vec![
                    get_app(&cx, &live.id),
                    list_deployments(&cx, &live.id, ""),
                    get_deployment(&cx, &live.id, &dep, None),
                    list_roles(&cx, &live.id),
                    list_role_grants(&cx, &live.id, ""),
                    list_versions(&cx, &live.id),
                ];
                reqs.push(update_settings(
                    &cx,
                    &live.id,
                    json!({ "telemetry_mode": "privacy" }),
                ));
                reqs.push(add_role_grant(&cx, &live.id, "approver", Who::Guest));
                reqs.push(create_deployment(
                    &cx,
                    &live.id,
                    from_folder(&missing, None),
                ));
                reqs.push(publish(&cx, &live.id, &missing, json!({})));
                for who in [Who::Owner, Who::Member, Who::MemberView, Who::Agent] {
                    let p = cx.p(who).clone();
                    for r in &reqs {
                        no_secret_in(&cx, &p, r.clone(), &secrets).await?;
                    }
                }
                Ok(())
            }
        ),
        case!(
            "no_answer_holds_an_app_secret_value",
            [
                "createApp",
                "getApp",
                "listAppDeployments",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment",
                "publishAppDeployment",
                "setAppSecret",
            ],
            "an entered secret value (set with setAppSecret) of an app whose manifest lists it \
             never appears, raw or encoded, in the answer of setAppSecret itself, of \
             createAppDeployment, getAppDeployment and publishAppDeployment while the app goes \
             live, or of getApp, listAppDeployments and getAppDeployment before and after the \
             publish (design 9.9, secrets are write-only); a value the caller sent is not \
             exempt, and the value is never printed",
            |cx| {
                let value = format!("sk_test_{}", uuid::Uuid::new_v4().simple());
                let secrets = vec![value.clone()];
                let manifest = "runtime: static\nsecrets: [STRIPE_KEY]\n";
                let (app, s) = app_and_site(&cx, "paid", manifest).await?;
                ok_scanned(
                    &cx,
                    Who::Member,
                    ruum_testkit::RequestSpec::new(
                        axum::http::Method::PUT,
                        app_path(&app, "/secrets/STRIPE_KEY"),
                    )
                    .tenant(cx.a().id)
                    .json(json!({ "value": value })),
                    &[204],
                    &secrets,
                )
                .await?;
                let p = preview_scanned(
                    &cx,
                    Who::Member,
                    &app,
                    from_folder(&s.folder, None),
                    &secrets,
                )
                .await?;
                let live =
                    go_live_scanned(&cx, Who::Member, &app, &str_at(&p, "/id")?, &secrets).await?;
                let member = cx.p(Who::Member).clone();
                for dep in [str_at(&p, "/id")?, str_at(&live, "/id")?] {
                    for r in [
                        get_app(&cx, &app),
                        list_deployments(&cx, &app, ""),
                        get_deployment(&cx, &app, &dep, None),
                    ] {
                        no_secret_in(&cx, &member, r, &secrets).await?;
                    }
                }
                Ok(())
            }
        ),
    ]
}
