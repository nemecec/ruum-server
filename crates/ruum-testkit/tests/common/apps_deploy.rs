//! App cases of card W-2: `createAppDeployment` of a static app from a folder or a file, the
//! direct `422` and `403` before the `202`, draft supersession, hostile archives, and secrets
//! that never appear in an answer. See [`super::apps`].

use super::apps::*;
use super::apps_archive::{self, TarEntry, TarKind, ZipEntry};
use super::{Case, Ctx, Fail};
use crate::{case, check, fail};
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

/// A hostile archive is refused: a direct `413` or `422` with an error code (the stage
/// `unpack` runs before the answer), or, if the platform answers `202`, the deployment fails at
/// `unpack` with a machine-readable error. It never becomes `ready`.
async fn refused_archive(cx: &Ctx, app: &str, file_name: &str, bytes: &[u8]) -> Result<(), Fail> {
    let (file, _) = write_file(cx, Who::Member, cx.a().projects, file_name, bytes).await?;
    let res = cx
        .send(Who::Member, create_deployment(cx, app, from_file(&file)))
        .await?;
    match res.status.as_u16() {
        413 | 422 => {
            check!(
                res.error_code().is_some(),
                "{file_name}: a {} without an error code: {}",
                res.status,
                res.text()
            );
            Ok(())
        }
        202 => {
            let made = res.json().unwrap_or(Value::Null);
            let dep = str_at(&made, "/id")?;
            let d = wait_state(cx, Who::Member, app, &dep, &["failed", "ready"]).await?;
            check!(d["state"] == "failed", "{file_name} was deployed: {d}");
            let unpack = d["stages"]
                .as_array()
                .and_then(|s| s.iter().find(|x| x["name"] == "unpack"))
                .cloned()
                .unwrap_or(Value::Null);
            check!(
                unpack["state"] == "failed" && unpack["error"]["code"].is_string(),
                "{file_name}: the stage unpack did not fail with a machine-readable error: {}",
                d["stages"]
            );
            Ok(())
        }
        other => fail!(
            "{file_name}: got {other}, expected 413 or 422: {}",
            res.text()
        ),
    }
}

pub fn cases() -> Vec<Case> {
    vec![
        case!(
            "bad_manifest_is_a_direct_422_with_a_pointer",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment"
            ],
            "createAppDeployment answers 422 validation_failed before any 202 for an unknown \
             runtime (pointer /runtime), an env name with the RUUM_ prefix (pointer under /env), \
             a static app with start (a schema rule), two default roles (a provider rule, \
             pointer under /roles) and a folder without app.yaml; each with \
             error.details.pointer where a key is wrong",
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
                Ok(())
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
            ],
            "guest_view (edit on a folder of its own, view on the fixture database) deploys an \
             app with the binding db (edit on that database): 403 permission_denied, hint \
             binding_not_allowed, error.details.binding = db, before any 202 (design 9.4)",
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
                Ok(())
            }
        ),
        case!(
            "binding_to_another_tenants_node_fails_like_a_missing_node",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment"
            ],
            "a binding to node:<id> of tenant B's plan file and one to node:<random id> get the \
             same status, error code and hint, and the answer holds no marker of tenant B \
             (createAppDeployment: like a missing node)",
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
                    let e = res.json().unwrap_or(Value::Null)["error"].clone();
                    answers.push((res.status.as_u16(), e["code"].clone(), e["hint"].clone()));
                }
                check!(
                    answers[0] == answers[1],
                    "tenant B's node and a missing node differ: {answers:?}"
                );
                Ok(())
            }
        ),
        case!(
            "file_binding_with_edit_is_refused",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment"
            ],
            "a binding with role edit on a folder (member has manage on it) is refused before \
             any 202 (403 or 422): in step 3 a file or folder binding is view",
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
                Ok(())
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
                "getAppDeployment"
            ],
            "a .zip with app.yaml and an entry ../../outside.html, and one with an absolute \
             path /etc/ruum.html, is refused at unpack",
            |cx| {
                let app = app_id(&new_app(&cx, Who::Member, cx.a().projects, "slip").await?)?;
                for (file, evil) in [
                    ("slip.zip", "../../outside.html"),
                    ("abs.zip", "/etc/ruum.html"),
                ] {
                    let bytes = apps_archive::zip(&[
                        ZipEntry::file("app.yaml", STATIC_MANIFEST.as_bytes()),
                        ZipEntry::file(evil, INDEX_HTML),
                    ]);
                    refused_archive(&cx, &app, file, &bytes).await?;
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
                "getAppDeployment"
            ],
            "a .tar.gz with app.yaml and an entry ../outside.html, and one with \
             dist/../../outside.html, is refused at unpack",
            |cx| {
                let app = app_id(&new_app(&cx, Who::Member, cx.a().projects, "tar slip").await?)?;
                for (file, evil) in [
                    ("slip.tar.gz", "../outside.html"),
                    ("deep.tar.gz", "dist/../../outside.html"),
                ] {
                    let bytes = apps_archive::tar_gz(&[
                        ("app.yaml", STATIC_MANIFEST.as_bytes()),
                        (evil, INDEX_HTML),
                    ]);
                    refused_archive(&cx, &app, file, &bytes).await?;
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
                "getAppDeployment"
            ],
            "a .tar.gz with a symbolic link (index.html -> /etc/passwd), one with a hard link \
             (to ../../secret), and a .zip with a symbolic link entry are refused at unpack",
            |cx| {
                let app = app_id(&new_app(&cx, Who::Member, cx.a().projects, "links").await?)?;
                let manifest = TarEntry::file("app.yaml", STATIC_MANIFEST.as_bytes());
                let sym = apps_archive::gzip(&apps_archive::tar(&[
                    TarEntry::file("app.yaml", STATIC_MANIFEST.as_bytes()),
                    TarEntry::special("index.html", TarKind::SymLink, "/etc/passwd"),
                ]));
                refused_archive(&cx, &app, "symlink.tar.gz", &sym).await?;
                let hard = apps_archive::gzip(&apps_archive::tar(&[
                    manifest,
                    TarEntry::special("index.html", TarKind::HardLink, "../../secret"),
                ]));
                refused_archive(&cx, &app, "hardlink.tar.gz", &hard).await?;
                let zip = apps_archive::zip(&[
                    ZipEntry::file("app.yaml", STATIC_MANIFEST.as_bytes()),
                    ZipEntry {
                        name: "index.html",
                        data: b"/etc/passwd",
                        unix_mode: Some(0o120_777),
                    },
                ]);
                refused_archive(&cx, &app, "symlink.zip", &zip).await
            }
        ),
        case!(
            "device_files_in_an_archive_are_refused",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "getAppDeployment"
            ],
            "a .tar.gz with a character device, one with a block device and one with a FIFO \
             are refused at unpack",
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
                    refused_archive(&cx, &app, file, &bytes).await?;
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
                "getAppDeployment"
            ],
            "a valid .tar.gz of about 4 MiB that expands to a 4 GiB file is refused at unpack \
             (size limits), not unpacked",
            |cx| {
                let app = app_id(&new_app(&cx, Who::Member, cx.a().projects, "bomb").await?)?;
                refused_archive(&cx, &app, "bomb.tar.gz", &apps_archive::bomb_tar_gz()).await
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
             a secret of the fixture: agent keys, app passwords, link, invitation and receipt \
             tokens, bearer tokens and session values (design 9.9); the values are never printed",
            |cx| {
                let secrets = fixture_secrets(&cx);
                let live = live_app(&cx, cx.a().projects, "quiet", ROLES_MANIFEST).await?;
                let dep = live.deployment();
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
             never appears in getApp, listAppDeployments or getAppDeployment, before and after \
             a publish (design 9.9, secrets are write-only); the value is never printed",
            |cx| {
                let value = format!("sk_test_{}", uuid::Uuid::new_v4().simple());
                let secrets = vec![value.clone()];
                let manifest = "runtime: static\nsecrets: [STRIPE_KEY]\n";
                let (app, s) = app_and_site(&cx, "paid", manifest).await?;
                cx.ok(
                    Who::Member,
                    ruum_testkit::RequestSpec::new(
                        axum::http::Method::PUT,
                        app_path(&app, "/secrets/STRIPE_KEY"),
                    )
                    .tenant(cx.a().id)
                    .json(json!({ "value": value })),
                    &[204],
                )
                .await?;
                let p = preview(&cx, Who::Member, &app, from_folder(&s.folder, None)).await?;
                let live = go_live(&cx, Who::Member, &app, &str_at(&p, "/id")?).await?;
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
