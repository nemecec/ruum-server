//! The cases of card D-1: `createDatabase`, `getDatabase` and `updateDatabaseManifest`. They run
//! as soon as D-1 removes the three `planned` marks, and they are D-1's done-when ("T-1 cases
//! for these operations pass on memory and Postgres"). From the contract and decisions 0004-7,
//! 0004-10 and 0004-35.

use super::api::*;
use super::{Case, Fail};
use crate::{case, check, fail};
use ruum_testkit::fixtures::Who;
use serde_json::{Value, json};

pub fn cases() -> Vec<Case> {
    vec![
        case!(
            "create_by_an_editor_provisions_then_ready",
            ["getDatabase", "createDatabase", "listChanges"],
            "a person with edit on the folder (member_edit): 202 with a typed node of type \
             database, state provisioning (or already ready), node.created and version.created \
             in the feed; getDatabase reaches ready within the bound; a non-database node is 404",
            |cx| {
                let e = engine(&cx).await?;
                let made = cx
                    .ok(
                        Who::MemberEdit,
                        create_database(&cx, cx.a().projects, "orchard", e),
                        &[202],
                    )
                    .await?;
                let node = &made["database"]["node"];
                check!(node["kind"] == "typed", "node.kind is not typed: {node}");
                check!(
                    node["type"] == "database",
                    "node.type is not database: {node}"
                );
                check!(node["name"] == "orchard", "node.name: {node}");
                check!(
                    node["parent_id"] == json!(cx.a().projects),
                    "node.parent_id is not Projects: {node}"
                );
                let state = made["database"]["status"]["state"]
                    .as_str()
                    .unwrap_or_default();
                check!(
                    state == "provisioning" || state == "ready",
                    "a new database is provisioning (or ready), got {state:?}"
                );
                check!(
                    made["database"]["manifest"]["engine"] == e.name(),
                    "the manifest is not the one sent: {}",
                    made["database"]["manifest"]
                );
                check!(
                    made["change"].is_object(),
                    "no change in the DatabaseWrite: {made}"
                );
                let id = written_id(&made)?;
                let ready = wait_ready(&cx, &id).await?;
                check!(
                    ready["branch_of"].is_null() && ready["branch_data"].is_null(),
                    "a new database is not a branch: {ready}"
                );
                check!(
                    ready["manifest_version_id"] == node["head_version"]
                        || node["head_version"].is_null(),
                    "manifest_version_id is not the head version: {ready}"
                );
                for field in ["limits", "size_bytes", "unconfirmed_columns"] {
                    check!(
                        !ready[field].is_null(),
                        "getDatabase has no {field}: {ready}"
                    );
                }
                // The journal: node.created (kind typed, type database) and version.created.
                let feed = cx
                    .ok(
                        Who::Owner,
                        ruum_testkit::RequestSpec::get("/v1/changes?limit=500").tenant(cx.a().id),
                        &[200],
                    )
                    .await?;
                let on_node: Vec<Value> = items(&feed)
                    .into_iter()
                    .filter(|c| c.to_string().contains(&id))
                    .collect();
                for ty in ["node.created", "version.created"] {
                    check!(
                        on_node.iter().any(|c| c["type"] == ty),
                        "no {ty} entry for the new database in the feed: {on_node:?}"
                    );
                }
                // getDatabase of a node that is not a database is 404.
                cx.refused(
                    Who::Owner,
                    get_database(&cx, cx.a().plan),
                    404,
                    &["not_found"],
                )
                .await?;
                Ok(())
            }
        ),
        case!(
            "create_needs_edit_on_the_folder",
            ["getDatabase", "createDatabase", "getNodeByPath"],
            "Needs edit on the parent: member_view and guest_view (view on Projects) get 403; a \
             principal that cannot see the folder gets 404; no node is made",
            |cx| {
                let e = engine(&cx).await?;
                for who in [Who::MemberView, Who::GuestView, Who::MemberComment] {
                    cx.refused(
                        who,
                        create_database(&cx, cx.a().projects, "not allowed", e),
                        403,
                        &["permission_denied"],
                    )
                    .await?;
                }
                cx.refused(
                    Who::Guest,
                    create_database(&cx, cx.a().projects, "not allowed", e),
                    404,
                    &["not_found"],
                )
                .await?;
                cx.refused(
                    Who::Owner,
                    ruum_testkit::RequestSpec::get("/v1/nodes/by-path/Projects/not%20allowed")
                        .tenant(cx.a().id),
                    404,
                    &[],
                )
                .await?;
                Ok(())
            }
        ),
        case!(
            "agents_create_with_max_role_edit",
            ["getDatabase", "createDatabase", "updateAgent"],
            "Agents may create databases inside their scope with max_role edit or higher: the \
             narrow agent (view) gets 403 (permission_denied or scope_exceeded); with max_role \
             edit on Projects it gets 202; outside its scope (agent_out, Private) it gets 403 or \
             404",
            |cx| {
                let e = engine(&cx).await?;
                cx.refused(
                    Who::Agent,
                    create_database(&cx, cx.a().projects, "by the agent", e),
                    403,
                    &["permission_denied", "scope_exceeded"],
                )
                .await?;
                let res = cx
                    .send(
                        Who::AgentOut,
                        create_database(&cx, cx.a().projects, "by the agent", e),
                    )
                    .await?;
                check!(
                    matches!(res.status.as_u16(), 403 | 404),
                    "an agent outside its scope: got {}",
                    res.status
                );
                set_scope(&cx, Who::Agent, Some(cx.a().projects), "edit").await?;
                let made = cx
                    .ok(
                        Who::Agent,
                        create_database(&cx, cx.a().projects, "by the agent", e),
                        &[202],
                    )
                    .await?;
                let id = written_id(&made)?;
                wait_ready(&cx, &id).await?;
                Ok(())
            }
        ),
        case!(
            "guests_create_a_database_only_with_the_policy",
            [
                "getDatabase",
                "createDatabase",
                "createNode",
                "addGrant",
                "updateTenant"
            ],
            "decision 0004-10: guest_view gets manage on a new folder in Projects (it already \
             has view on Projects, and a second grant there is a conflict), and its agent \
             agent_of_guest_view (scope everything, manage): 403 permission_denied with hint \
             guests_create_resources while the policy is false (the default); 202 after the \
             owner sets guests_create_resources true",
            |cx| {
                let e = engine(&cx).await?;
                let place = folder(&cx, Who::Owner, cx.a().projects, "guest place").await?;
                add_grant(&cx, &place, Who::GuestView, "manage").await?;
                let place: ruum_core::NodeId = match place.parse() {
                    Ok(id) => id,
                    Err(_) => fail!("createNode gave an ID that is not a UUID: {place}"),
                };
                for (who, name) in [
                    (Who::GuestView, "guest db"),
                    (Who::AgentOfGuestView, "agent db"),
                ] {
                    let err = cx
                        .refused(
                            who,
                            create_database(&cx, place, name, e),
                            403,
                            &["permission_denied"],
                        )
                        .await?;
                    hint_has(&err, "guests_create_resources")?;
                }
                guests_create_resources(&cx, true).await?;
                for (who, name) in [
                    (Who::GuestView, "guest db"),
                    (Who::AgentOfGuestView, "agent db"),
                ] {
                    let made = cx
                        .ok(who, create_database(&cx, place, name, e), &[202])
                        .await?;
                    wait_ready(&cx, &written_id(&made)?).await?;
                }
                Ok(())
            }
        ),
        case!(
            "create_with_a_taken_name_is_a_conflict",
            ["getDatabase", "createDatabase", "listChildren"],
            "the name is taken (by a database and by a file): 409 (name_taken or conflict) with \
             on_conflict fail (the default), and no second node; on_conflict rename makes \
             \"<name> (2)\"",
            |cx| {
                let e = engine(&cx).await?;
                new_database(&cx, Who::Member, cx.a().projects, "twice", e).await?;
                cx.refused(
                    Who::Member,
                    create_database(&cx, cx.a().projects, "twice", e),
                    409,
                    &["name_taken", "conflict"],
                )
                .await?;
                cx.refused(
                    Who::Member,
                    create_database(&cx, cx.a().projects, &cx.a().plan_name.clone(), e),
                    409,
                    &["name_taken", "conflict"],
                )
                .await?;
                let children = cx
                    .ok(
                        Who::Owner,
                        ruum_testkit::RequestSpec::get(format!(
                            "/v1/nodes/{}/children?limit=200",
                            cx.a().projects
                        ))
                        .tenant(cx.a().id),
                        &[200],
                    )
                    .await?;
                let named = items(&children)
                    .iter()
                    .filter(|n| n["name"] == "twice")
                    .count();
                check!(named == 1, "{named} nodes named twice in Projects");
                let mut req = create_database(&cx, cx.a().projects, "twice", e);
                if let ruum_testkit::Body::Json(b) = &mut req.body {
                    b["on_conflict"] = json!("rename");
                }
                let made = cx.ok(Who::Member, req, &[202]).await?;
                check!(
                    made["database"]["node"]["name"] == "twice (2)",
                    "on_conflict rename: {}",
                    made["database"]["node"]
                );
                wait_ready(&cx, &written_id(&made)?).await?;
                Ok(())
            }
        ),
        case!(
            "create_over_the_plan_limit",
            [
                "getDatabase",
                "createDatabase",
                "listTenants",
                "getRoot",
                "listChildren"
            ],
            "plan_limit_reached (403): the member's personal space (plan free: 1 database per \
             engine, design 4.3) takes databases until the limit, then refuses and makes no node",
            |cx| {
                let e = engine(&cx).await?;
                let tenants = cx
                    .ok(
                        Who::Member,
                        ruum_testkit::RequestSpec::get("/v1/tenants"),
                        &[200],
                    )
                    .await?;
                let personal = items(&tenants)
                    .into_iter()
                    .find(|t| t["tenant"]["kind"] == "personal" && t["role"] == "owner");
                let Some(personal) = personal else {
                    fail!("member has no personal space: {tenants}");
                };
                let tid: ruum_core::TenantId =
                    serde_json::from_value(personal["tenant"]["id"].clone())
                        .map_err(|err| Fail::Failed(format!("tenant id: {err}")))?;
                let root = cx
                    .ok(
                        Who::Member,
                        ruum_testkit::RequestSpec::get("/v1/nodes/root").tenant(tid),
                        &[200],
                    )
                    .await?;
                let root_id = root["id"].as_str().unwrap_or_default().to_string();
                let count_children = |cx: super::Ctx, root_id: String| async move {
                    let page = cx
                        .ok(
                            Who::Member,
                            ruum_testkit::RequestSpec::get(format!(
                                "/v1/nodes/{root_id}/children?limit=200"
                            ))
                            .tenant(tid),
                            &[200],
                        )
                        .await?;
                    Ok::<usize, Fail>(items(&page).len())
                };
                let mut made = 0;
                loop {
                    check!(
                        made < 30,
                        "no plan limit after 30 databases in a free space"
                    );
                    let req = ruum_testkit::RequestSpec::post("/v1/databases").tenant(tid).json(json!({
                        "parent_id": root_id, "name": format!("db {made}"), "manifest": e.manifest(),
                    }));
                    let before = count_children(cx.clone(), root_id.clone()).await?;
                    let res = cx.send(Who::Member, req).await?;
                    if res.status.as_u16() == 202 {
                        made += 1;
                        continue;
                    }
                    check!(
                        res.status.as_u16() == 403
                            && res.error_code().as_deref() == Some("plan_limit_reached"),
                        "after {made} databases: got {} {}",
                        res.status,
                        res.text()
                    );
                    let after = count_children(cx.clone(), root_id.clone()).await?;
                    check!(before == after, "the refused create made a node");
                    break;
                }
                check!(
                    made >= 1,
                    "the free plan allows one database per engine (design 4.3)"
                );
                Ok(())
            }
        ),
        case!(
            "create_with_an_invalid_manifest",
            ["getDatabase", "createDatabase"],
            "the manifest must be valid (database.schema.json and the provider rules): \
             extensions on SQLite, an extension outside the allow-list, a wrong engine version, \
             a plan other than shared-small and an unknown property are 422 validation_failed",
            |cx| {
                let e = engine(&cx).await?;
                let mut bad = Vec::new();
                let mut m = e.manifest();
                m["version"] = json!(if e == Engine::Sqlite { "4" } else { "18" });
                bad.push(("a wrong major version", m));
                let mut m = e.manifest();
                m["plan"] = json!("dedicated-large");
                bad.push(("another plan", m));
                let mut m = e.manifest();
                m["colour"] = json!("green");
                bad.push(("an unknown property", m));
                let mut m = e.manifest();
                m["extensions"] = json!(["pg_cron"]);
                bad.push(("an extension outside the allow-list", m));
                let mut m = Engine::Sqlite.manifest();
                m["extensions"] = json!(["vector"]);
                bad.push(("extensions on SQLite", m));
                for (what, manifest) in bad {
                    let req = ruum_testkit::RequestSpec::post("/v1/databases")
                        .tenant(cx.a().id)
                        .json(json!({
                            "parent_id": cx.a().projects, "name": what, "manifest": manifest,
                        }));
                    cx.refused(Who::Member, req, 422, &["validation_failed"])
                        .await
                        .map_err(|f| match f {
                            Fail::Failed(m) => Fail::Failed(format!("{what}: {m}")),
                            p => p,
                        })?;
                }
                Ok(())
            }
        ),
        case!(
            "agents_never_set_data_classes",
            [
                "getDatabase",
                "createDatabase",
                "updateAgent",
                "updateDatabaseManifest"
            ],
            "only a person changes data_classes (updateDatabaseManifest: an agent with manage \
             that sends other data_classes gets 422 validation_failed, hint suggestColumnClasses; \
             the same manifest without a class change is 202; no version is made by the \
             refusal). On createDatabase an agent that sends data_classes is refused (403 or \
             422; wave 3 notes for D-1)",
            |cx| {
                let e = engine(&cx).await?;
                set_scope(&cx, Who::Agent, Some(cx.a().projects), "manage").await?;
                let mut m = e.manifest();
                m["data_classes"] = json!({ e.key("plants", "email"): "normal" });
                let req = ruum_testkit::RequestSpec::post("/v1/databases")
                    .tenant(cx.a().id)
                    .json(json!({
                        "parent_id": cx.a().projects, "name": "classes by an agent", "manifest": m,
                    }));
                let res = cx.send(Who::Agent, req).await?;
                check!(
                    matches!(res.status.as_u16(), 403 | 422),
                    "an agent creates a database with data_classes: got {} {}",
                    res.status,
                    res.text()
                );
                let id = new_database(&cx, Who::Member, cx.a().projects, "classes", e).await?;
                let before = cx.ok(Who::Owner, get_database(&cx, &id), &[200]).await?;
                let err = cx
                    .refused(
                        Who::Agent,
                        update_manifest(&cx, &id, m.clone()),
                        422,
                        &["validation_failed"],
                    )
                    .await?;
                hint_has(&err, "suggestColumnClasses")?;
                let after = cx.ok(Who::Owner, get_database(&cx, &id), &[200]).await?;
                check!(
                    before["manifest_version_id"] == after["manifest_version_id"],
                    "the refused manifest made a version"
                );
                cx.ok(Who::Agent, update_manifest(&cx, &id, e.manifest()), &[202])
                    .await?;
                Ok(())
            }
        ),
        case!(
            "a_person_updates_the_manifest",
            [
                "getDatabase",
                "createDatabase",
                "updateDatabaseManifest",
                "listVersions"
            ],
            "Needs manage: member (manage) gets 202 with a new manifest version and the label; \
             the status shows updating until applied, then ready with observed_version_id = the \
             new version; member_edit gets 403; engine and major version changes are 422 \
             validation_failed and make no version",
            |cx| {
                let e = engine(&cx).await?;
                let id = new_database(&cx, Who::Member, cx.a().projects, "settings", e).await?;
                let before = cx.ok(Who::Owner, get_database(&cx, &id), &[200]).await?;
                cx.refused(
                    Who::MemberEdit,
                    update_manifest(&cx, &id, e.manifest()),
                    403,
                    &["permission_denied"],
                )
                .await?;
                let mut m = e.manifest();
                m["backups"] = json!({ "pitr_days": 7 });
                let req = ruum_testkit::RequestSpec::new(
                    axum::http::Method::PUT,
                    db_path(&id, "/manifest"),
                )
                .tenant(cx.a().id)
                .json(json!({ "manifest": m, "label": "backups stated" }));
                let w = cx.ok(Who::Member, req, &[202]).await?;
                let new_version = w["database"]["manifest_version_id"].clone();
                check!(
                    new_version != before["manifest_version_id"] && !new_version.is_null(),
                    "no new manifest version: {w}"
                );
                let state = w["database"]["status"]["state"]
                    .as_str()
                    .unwrap_or_default();
                check!(
                    state == "updating" || state == "ready",
                    "after a manifest change the state is updating (or ready), got {state:?}"
                );
                let d = wait_ready(&cx, &id).await?;
                check!(
                    d["status"]["observed_version_id"] == new_version,
                    "the engine does not match the new version: {}",
                    d["status"]
                );
                let versions = cx
                    .ok(
                        Who::Owner,
                        ruum_testkit::RequestSpec::get(format!("/v1/nodes/{id}/versions"))
                            .tenant(cx.a().id),
                        &[200],
                    )
                    .await?;
                check!(
                    items(&versions)
                        .iter()
                        .any(|v| v["id"] == new_version && v["label"] == "backups stated"),
                    "listVersions has no labelled new version: {versions}"
                );
                let other = if e == Engine::Sqlite {
                    Engine::Postgres
                } else {
                    Engine::Sqlite
                };
                let mut major = e.manifest();
                major["version"] = json!(if e == Engine::Sqlite { "4" } else { "18" });
                for (what, manifest) in [
                    ("the engine", other.manifest()),
                    ("the major version", major),
                ] {
                    cx.refused(
                        Who::Member,
                        update_manifest(&cx, &id, manifest),
                        422,
                        &["validation_failed"],
                    )
                    .await
                    .map_err(|f| match f {
                        Fail::Failed(m) => Fail::Failed(format!("a change of {what}: {m}")),
                        p => p,
                    })?;
                }
                let after = cx.ok(Who::Owner, get_database(&cx, &id), &[200]).await?;
                check!(
                    after["manifest_version_id"] == new_version,
                    "a refused manifest made a version"
                );
                Ok(())
            }
        ),
        case!(
            "manifest_of_a_trashed_database_is_refused",
            [
                "getDatabase",
                "createDatabase",
                "trashNode",
                "updateDatabaseManifest"
            ],
            "a trashed database is suspended; updateDatabaseManifest on it is refused (D-1 \
             decides 409 conflict, the recommendation, or 404) and makes no version",
            |cx| {
                let e = engine(&cx).await?;
                let id = new_database(&cx, Who::Member, cx.a().projects, "to trash", e).await?;
                let before = cx.ok(Who::Owner, get_database(&cx, &id), &[200]).await?;
                cx.ok(
                    Who::Member,
                    ruum_testkit::RequestSpec::post(format!("/v1/nodes/{id}/trash"))
                        .tenant(cx.a().id),
                    &[200],
                )
                .await?;
                let res = cx
                    .send(Who::Member, update_manifest(&cx, &id, e.manifest()))
                    .await?;
                check!(
                    matches!(res.status.as_u16(), 404 | 409),
                    "a manifest change of a trashed database: got {} {}",
                    res.status,
                    res.text()
                );
                let after = cx.send(Who::Owner, get_database(&cx, &id)).await?;
                if let Some(a) = after.json().filter(|_| after.status.is_success()) {
                    check!(
                        a["manifest_version_id"] == before["manifest_version_id"],
                        "the refused change made a version"
                    );
                    check!(
                        a["status"]["state"] == "suspended",
                        "a trashed database is suspended: {}",
                        a["status"]
                    );
                }
                Ok(())
            }
        ),
        case!(
            "get_database_shows_what_the_caller_may_do",
            ["getDatabase", "createDatabase", "updateAgent"],
            "my_permissions after the agent scope and grants (DatabasePermissions, decision \
             0004-12 c): owner manage (read_secret); member_view view only; the narrow agent \
             query_read only (no read_personal, read_secret or write_live without a grant); \
             an agent with manage still never read_secret; a principal outside the database \
             gets 404; the answer has an ETag",
            |cx| {
                let e = engine(&cx).await?;
                let id = new_database(&cx, Who::Member, cx.a().projects, "perms", e).await?;
                let res = cx.send(Who::Owner, get_database(&cx, &id)).await?;
                check!(res.headers.contains_key("etag"), "getDatabase has no ETag");
                let perms = |v: &Value| v["my_permissions"].clone();
                let expect = |who: &str, v: &Value, want: [bool; 6]| -> Result<(), Fail> {
                    let p = perms(v);
                    let names = [
                        "query_read",
                        "query_write",
                        "change_schema",
                        "write_live",
                        "read_personal",
                        "read_secret",
                    ];
                    for (n, w) in names.iter().zip(want) {
                        check!(
                            p[*n] == json!(w),
                            "{who}: my_permissions.{n} should be {w}: {p}"
                        );
                    }
                    Ok(())
                };
                let owner = res.json().unwrap_or_default();
                expect("owner", &owner, [true, true, true, true, true, true])?;
                let viewer = cx
                    .ok(Who::MemberView, get_database(&cx, &id), &[200])
                    .await?;
                expect(
                    "member_view",
                    &viewer,
                    [true, false, false, false, true, false],
                )?;
                let agent = cx.ok(Who::Agent, get_database(&cx, &id), &[200]).await?;
                expect(
                    "agent (view)",
                    &agent,
                    [true, false, false, false, false, false],
                )?;
                set_scope(&cx, Who::Agent, Some(cx.a().projects), "manage").await?;
                let agent = cx.ok(Who::Agent, get_database(&cx, &id), &[200]).await?;
                check!(
                    perms(&agent)["read_secret"] == false && perms(&agent)["write_live"] == false,
                    "an agent with manage and no grant: {}",
                    perms(&agent)
                );
                cx.refused(Who::Guest, get_database(&cx, &id), 404, &["not_found"])
                    .await?;
                Ok(())
            }
        ),
    ]
}
