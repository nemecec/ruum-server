//! Guests and resources that count against the host plan (decision 0004-10, closes 0002-2):
//! `createDatabaseBranch`, `restoreDatabase` with `target: new_node` and `copyNode` of a
//! database, by a guest and by an agent of a guest, with `guests_create_resources` false (403,
//! hint `guests_create_resources`) and true (allowed). The `createDatabase` case is in
//! [`super::d1`].
//!
//! The guest is `guest_view` raised to `manage` on `Projects` (a second grant by the owner), so
//! its agent `agent_of_guest_view` (scope everything, `max_role: manage`) has the rights too and
//! only the policy decides.

use super::api::*;
use super::{Case, Ctx, Fail};
use crate::{case, check};
use ruum_testkit::fixtures::Who;
use serde_json::{Value, json};

/// Raises `guest_view` to `manage` on `Projects` and makes the garden.
async fn setup(cx: &Ctx, name: &str) -> Result<Garden, Fail> {
    add_grant(cx, cx.a().projects, Who::GuestView, "manage").await?;
    garden(cx, name).await
}

/// `who` gets 403 permission_denied with the hint guests_create_resources.
async fn refused_by_policy(cx: &Ctx, who: Who, req: ruum_testkit::RequestSpec) -> Result<(), Fail> {
    let err = cx.refused(who, req, 403, &["permission_denied"]).await?;
    hint_has(&err, "guests_create_resources")
}

pub fn cases() -> Vec<Case> {
    vec![
        case!(
            "guests_branch_a_database_only_with_the_policy",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "addGrant",
                "updateTenant",
                "createDatabaseBranch",
            ],
            "createDatabaseBranch (masked) by guest_view and agent_of_guest_view: 403 with hint \
             guests_create_resources while the policy is false; 202 when it is true",
            |cx| {
                let g = setup(&cx, "guest branches").await?;
                for (who, name) in [
                    (Who::GuestView, "guest branch"),
                    (Who::AgentOfGuestView, "agent branch"),
                ] {
                    refused_by_policy(&cx, who, branch(&cx, &g.id, name, "masked")).await?;
                }
                guests_create_resources(&cx, true).await?;
                for (who, name) in [
                    (Who::GuestView, "guest branch"),
                    (Who::AgentOfGuestView, "agent branch"),
                ] {
                    let made = cx
                        .ok(who, branch(&cx, &g.id, name, "masked"), &[202])
                        .await?;
                    wait_ready(&cx, &written_id(&made)?).await?;
                }
                Ok(())
            }
        ),
        case!(
            "guests_restore_into_a_new_node_only_with_the_policy",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "addGrant",
                "updateTenant",
                "createDatabaseRestorePoint",
                "restoreDatabase",
            ],
            "restoreDatabase target new_node by guest_view (manage): 403 with hint \
             guests_create_resources while the policy is false, 202 when it is true; \
             agent_of_guest_view gets 403 user_required both times (restoreDatabase is users \
             only)",
            |cx| {
                let g = setup(&cx, "guest restores").await?;
                let rp = cx
                    .ok(
                        Who::Member,
                        restore_point(&cx, &g.id, "before the guest"),
                        &[201],
                    )
                    .await?;
                let rp_id = rp["restore_point"]["id"].clone();
                check!(rp_id.is_string(), "no restore_point.id: {rp}");
                let body = |name: &str| json!({ "restore_point_id": rp_id, "target": "new_node", "name": name });
                refused_by_policy(
                    &cx,
                    Who::GuestView,
                    restore(&cx, &g.id, body("guest restore")),
                )
                .await?;
                cx.refused(
                    Who::AgentOfGuestView,
                    restore(&cx, &g.id, body("agent restore")),
                    403,
                    &["user_required"],
                )
                .await?;
                guests_create_resources(&cx, true).await?;
                let made = cx
                    .ok(
                        Who::GuestView,
                        restore(&cx, &g.id, body("guest restore")),
                        &[202],
                    )
                    .await?;
                let new_id = written_id(&made)?;
                check!(new_id != g.id, "new_node restored in place");
                wait_ready(&cx, &new_id).await?;
                cx.refused(
                    Who::AgentOfGuestView,
                    restore(&cx, &g.id, body("agent restore")),
                    403,
                    &["user_required"],
                )
                .await?;
                Ok(())
            }
        ),
        case!(
            "guests_copy_a_database_only_with_the_policy",
            [
                "getDatabase",
                "createDatabase",
                "applyDatabaseMigration",
                "queryDatabase",
                "addGrant",
                "updateTenant",
                "copyNode",
            ],
            "copyNode of a database by guest_view and agent_of_guest_view: 403 with hint \
             guests_create_resources while the policy is false; 201 when it is true (a copy of \
             a typed node counts against the host plan)",
            |cx| {
                let g = setup(&cx, "guest copies").await?;
                for who in [Who::GuestView, Who::AgentOfGuestView] {
                    refused_by_policy(&cx, who, copy_node(&cx, &g.id, cx.a().projects)).await?;
                }
                guests_create_resources(&cx, true).await?;
                for who in [Who::GuestView, Who::AgentOfGuestView] {
                    let made: Value = cx
                        .ok(who, copy_node(&cx, &g.id, cx.a().projects), &[201])
                        .await?;
                    let Some(id) = made["node"]["id"].as_str() else {
                        crate::fail!("copyNode gave no node.id: {made}");
                    };
                    wait_ready(&cx, id).await?;
                }
                Ok(())
            }
        ),
    ]
}
