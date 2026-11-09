//! Step 1 operations on database nodes (decisions 0004-6, 0004-7 and 0004-18; card P-3 built
//! the typed-node rules of the node stores). The cases with branches are in
//! [`super::PENDING`] until card D-3 makes branches.

use super::api::*;
use super::{Case, Ctx, Fail};
use crate::{case, check, fail};
use axum::http::Method;
use ruum_testkit::RequestSpec;
use ruum_testkit::fixtures::{Principal, Who};
use ruum_testkit::requests::{CHUNKING, EMPTY_HASH};
use serde_json::{Value, json};

/// `putContent` on a node.
fn put_content(cx: &Ctx, node: &str) -> RequestSpec {
    RequestSpec::new(Method::PUT, format!("/v1/nodes/{node}/content"))
        .tenant(cx.a().id)
        .header("content-type", "text/plain")
        .bytes(b"not a manifest".to_vec())
}

/// `createUpload` with a `new_version` target.
fn new_version_upload(cx: &Ctx, node: &str) -> RequestSpec {
    RequestSpec::post("/v1/uploads")
        .tenant(cx.a().id)
        .json(json!({
            "target": "new_version", "node_id": node, "size": 0,
            "content_hash": EMPTY_HASH, "chunking": CHUNKING, "chunks": [],
        }))
}

/// `createUpload` with a `new_file` target.
fn new_file_upload(cx: &Ctx, parent: &str) -> RequestSpec {
    RequestSpec::post("/v1/uploads")
        .tenant(cx.a().id)
        .json(json!({
            "target": "new_file", "parent_id": parent, "name": "inside.txt", "size": 0,
            "content_hash": EMPTY_HASH, "chunking": CHUNKING, "chunks": [],
        }))
}

/// The head version of a node (`getNode`).
async fn head(cx: &Ctx, node: &str) -> Result<Value, Fail> {
    let n = cx.ok(Who::Owner, get_node(cx, node), &[200]).await?;
    Ok(n["head_version"].clone())
}

/// The value of `plants.email` of row 1 in `db`, read by the owner (`manage`: no masking).
async fn stored_email(cx: &Ctx, db: &str) -> Result<String, Fail> {
    let r = cx
        .ok(
            Who::Owner,
            read(cx, db, "select email from plants where id = 1"),
            &[200],
        )
        .await?;
    match first_cell(&r) {
        Value::String(s) => Ok(s),
        other => fail!("plants.email of the copy is {other}"),
    }
}

/// Makes a masked branch of `db` as `member` and waits until it is ready.
async fn masked_branch(cx: &Ctx, db: &str, name: &str) -> Result<String, Fail> {
    let id = written_id(
        &cx.ok(Who::Member, branch(cx, db, name, "masked"), &[202])
            .await?,
    )?;
    wait_ready(cx, &id).await?;
    Ok(id)
}

pub fn cases() -> Vec<Case> {
    vec![
        case!(
            "put_content_on_a_database_is_a_conflict",
            [
                "getDatabase",
                "createDatabase",
                "putContent",
                "getNode",
                "updateTenant",
                "createShareLink",
                "resolveShareLink"
            ],
            "decision 0004-6: putContent on a typed node is 409 conflict, for a manager and for \
             an edit share link; the head version does not change",
            |cx| {
                let e = engine(&cx).await?;
                let db = new_database(&cx, Who::Member, cx.a().projects, "no content", e).await?;
                let before = head(&cx, &db).await?;
                cx.refused(Who::Member, put_content(&cx, &db), 409, &["conflict"])
                    .await?;
                let link: Principal = edit_link(&cx).await?;
                cx.refused_as(&link, put_content(&cx, &db), 409, &["conflict"])
                    .await?;
                check!(
                    head(&cx, &db).await? == before,
                    "putContent changed the head of a database"
                );
                Ok(())
            }
        ),
        case!(
            "new_version_upload_on_a_database_is_a_conflict",
            [
                "getDatabase",
                "createDatabase",
                "createUpload",
                "getNode",
                "updateTenant",
                "createShareLink",
                "resolveShareLink"
            ],
            "decision 0004-6: createUpload with target new_version on a typed node is 409 \
             conflict, for a manager and for an edit share link",
            |cx| {
                let e = engine(&cx).await?;
                let db = new_database(&cx, Who::Member, cx.a().projects, "no uploads", e).await?;
                let before = head(&cx, &db).await?;
                cx.refused(
                    Who::Member,
                    new_version_upload(&cx, &db),
                    409,
                    &["conflict"],
                )
                .await?;
                let link = edit_link(&cx).await?;
                cx.refused_as(&link, new_version_upload(&cx, &db), 409, &["conflict"])
                    .await?;
                check!(
                    head(&cx, &db).await? == before,
                    "an upload changed the head of a database"
                );
                Ok(())
            }
        ),
        case!(
            "restore_version_of_a_database",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "updateDatabaseManifest",
                "listVersions",
                "restoreVersion",
                "updateAgent",
            ],
            "decision 0004-6: restoreVersion of a database needs manage (member_edit: 403); an \
             agent with manage that restores a version with other data_classes gets 422 \
             validation_failed; a manager restores it (200, a new version) and the old classes \
             apply again",
            |cx| {
                let g = garden(&cx, "versions").await?;
                let key = g.engine.key("plants", "email");
                // Two manifest versions by a person: email personal, then email normal.
                let current = cx.ok(Who::Owner, get_database(&cx, &g.id), &[200]).await?;
                let mut m = current["manifest"].clone();
                m["data_classes"][&key] = json!("personal");
                let w = cx
                    .ok(Who::Member, update_manifest(&cx, &g.id, m.clone()), &[202])
                    .await?;
                let Some(first) = w["database"]["manifest_version_id"]
                    .as_str()
                    .map(str::to_string)
                else {
                    fail!("no manifest_version_id: {w}")
                };
                wait_ready(&cx, &g.id).await?;
                m["data_classes"][&key] = json!("normal");
                cx.ok(Who::Member, update_manifest(&cx, &g.id, m), &[202])
                    .await?;
                wait_ready(&cx, &g.id).await?;
                let restore_v = || {
                    RequestSpec::post(format!("/v1/nodes/{}/versions/{first}/restore", g.id))
                        .tenant(cx.a().id)
                };
                cx.refused(Who::MemberEdit, restore_v(), 403, &["permission_denied"])
                    .await?;
                set_scope(&cx, Who::Agent, Some(cx.a().projects), "manage").await?;
                cx.refused(Who::Agent, restore_v(), 422, &["validation_failed"])
                    .await?;
                cx.ok(Who::Member, restore_v(), &[200]).await?;
                let d = wait_ready(&cx, &g.id).await?;
                check!(
                    d["manifest"]["data_classes"][&key] == "personal",
                    "the restored manifest does not have the old class: {}",
                    d["manifest"]
                );
                let versions = cx
                    .ok(
                        Who::Owner,
                        RequestSpec::get(format!("/v1/nodes/{}/versions", g.id)).tenant(cx.a().id),
                        &[200],
                    )
                    .await?;
                check!(
                    items(&versions)
                        .iter()
                        .filter(|v| v["id"] != json!(first))
                        .count()
                        >= 2,
                    "restoreVersion made no new version: {versions}"
                );
                Ok(())
            }
        ),
        case!(
            "copy_of_a_database_never_raises_the_copiers_rights",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "addGrant",
                "createNode",
                "updateAgent",
                "copyNode",
            ],
            "decision 0004-7: copyNode of a database holds masked data for a viewer (member_view \
             with edit on its own folder), an editor (member_edit), an agent (edit scope) and \
             full data only for a person with manage (member); the owner reads the copies",
            |cx| {
                let g = garden(&cx, "copy source").await?;
                let own = folder(&cx, Who::Owner, cx.a().root, "viewer's folder").await?;
                add_grant(&cx, &own, Who::MemberView, "edit").await?;
                set_scope(&cx, Who::Agent, Some(cx.a().projects), "edit").await?;
                let projects = cx.a().projects.to_string();
                for (who, parent, full) in [
                    (Who::MemberView, own.as_str(), false),
                    (Who::MemberEdit, projects.as_str(), false),
                    (Who::Agent, projects.as_str(), false),
                    (Who::Member, projects.as_str(), true),
                ] {
                    let made = cx.ok(who, copy_node(&cx, &g.id, parent), &[201]).await?;
                    let Some(copy) = made["node"]["id"].as_str().map(str::to_string) else {
                        fail!("copyNode by {} gave no node.id: {made}", who.name());
                    };
                    wait_ready(&cx, &copy).await?;
                    let email = stored_email(&cx, &copy).await?;
                    if full {
                        check!(email == g.email, "a manager's copy is not full: {email:?}");
                    } else {
                        check!(
                            email != g.email && is_pseudonym(&email),
                            "the copy of {} holds {email:?}, not a pseudonym",
                            who.name()
                        );
                    }
                }
                Ok(())
            }
        ),
        case!(
            "children_of_a_database_are_refused",
            [
                "getDatabase",
                "createDatabase",
                "createNode",
                "createUpload",
                "moveNode",
                "copyNode",
                "listChildren"
            ],
            "decision 0004-18: createNode, createUpload new_file, moveNode and copyNode into a \
             database are 409 conflict; nothing lands inside it",
            |cx| {
                let e = engine(&cx).await?;
                let db = new_database(&cx, Who::Member, cx.a().projects, "no children", e).await?;
                cx.refused(
                    Who::Member,
                    RequestSpec::post("/v1/nodes")
                        .tenant(cx.a().id)
                        .json(json!({ "parent_id": db, "name": "inside", "kind": "folder" })),
                    409,
                    &["conflict"],
                )
                .await?;
                cx.refused(Who::Member, new_file_upload(&cx, &db), 409, &["conflict"])
                    .await?;
                cx.refused(
                    Who::Member,
                    move_node(&cx, cx.a().plan, &db),
                    409,
                    &["conflict"],
                )
                .await?;
                cx.refused(
                    Who::Member,
                    copy_node(&cx, cx.a().plan, &db),
                    409,
                    &["conflict"],
                )
                .await?;
                let kids = cx
                    .ok(
                        Who::Owner,
                        RequestSpec::get(format!("/v1/nodes/{db}/children")).tenant(cx.a().id),
                        &[200, 404, 409],
                    )
                    .await?;
                check!(
                    items(&kids).is_empty(),
                    "a database has children that are not branches: {kids}"
                );
                Ok(())
            }
        ),
        case!(
            "a_branch_cannot_move_away",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "createDatabaseBranch",
                "moveNode",
                "getNode"
            ],
            "decision 0004-18: a branch (a child that child_types lists) cannot move away from \
             its database: 409 conflict",
            |cx| {
                let g = garden(&cx, "branch stays").await?;
                let b = masked_branch(&cx, &g.id, "stays here").await?;
                cx.refused(
                    Who::Member,
                    move_node(&cx, &b, cx.a().projects),
                    409,
                    &["conflict"],
                )
                .await?;
                let n = cx.ok(Who::Owner, get_node(&cx, &b), &[200]).await?;
                check!(n["parent_id"] == json!(g.id), "the branch moved: {n}");
                Ok(())
            }
        ),
        case!(
            "copy_of_a_database_copies_no_branch",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "createDatabaseBranch",
                "copyNode",
                "listDatabaseBranches"
            ],
            "decision 0004-18: a copy is a new live database without branches",
            |cx| {
                let g = garden(&cx, "copy without branches").await?;
                masked_branch(&cx, &g.id, "left behind").await?;
                let made = cx
                    .ok(Who::Member, copy_node(&cx, &g.id, cx.a().projects), &[201])
                    .await?;
                let Some(copy) = made["node"]["id"].as_str().map(str::to_string) else {
                    fail!("no node.id: {made}")
                };
                let d = wait_ready(&cx, &copy).await?;
                check!(d["branch_of"].is_null(), "a copy is a live database: {d}");
                let branches = cx
                    .ok(Who::Owner, list(&cx, &copy, "/branches"), &[200])
                    .await?;
                check!(
                    items(&branches).is_empty(),
                    "the copy has branches: {branches}"
                );
                let source = cx
                    .ok(Who::Owner, list(&cx, &g.id, "/branches"), &[200])
                    .await?;
                check!(
                    items(&source).len() == 1,
                    "the source lost its branch: {source}"
                );
                Ok(())
            }
        ),
        case!(
            "trash_and_purge_take_the_branches_along",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "createDatabaseBranch",
                "trashNode",
                "purgeNode",
                "getNode",
                "listTrash",
            ],
            "decision 0004-18: trashing a database trashes its branches (an editor no longer \
             sees the branch; it is suspended); purging the database purges them (404 for \
             everyone)",
            |cx| {
                let g = garden(&cx, "trash with branches").await?;
                let b = masked_branch(&cx, &g.id, "goes along").await?;
                cx.ok(
                    Who::Member,
                    RequestSpec::post(format!("/v1/nodes/{}/trash", g.id)).tenant(cx.a().id),
                    &[200],
                )
                .await?;
                cx.refused(Who::MemberEdit, get_node(&cx, &b), 404, &["not_found"])
                    .await?;
                let bd = cx.send(Who::Owner, get_database(&cx, &b)).await?;
                if bd.status.is_success() {
                    let v = bd.json().unwrap_or_default();
                    check!(
                        v["status"]["state"] == "suspended",
                        "a trashed branch is suspended: {}",
                        v["status"]
                    );
                }
                cx.ok(
                    Who::Owner,
                    RequestSpec::new(Method::DELETE, format!("/v1/nodes/{}", g.id))
                        .tenant(cx.a().id),
                    &[200, 204],
                )
                .await?;
                for node in [&g.id, &b] {
                    cx.refused(Who::Owner, get_node(&cx, node), 404, &["not_found"])
                        .await?;
                }
                let trash = cx
                    .ok(
                        Who::Owner,
                        RequestSpec::get("/v1/trash?limit=200").tenant(cx.a().id),
                        &[200],
                    )
                    .await?;
                check!(
                    !trash.to_string().contains(&b) && !trash.to_string().contains(&g.id),
                    "the purged database or its branch is still in the trash"
                );
                Ok(())
            }
        ),
    ]
}
