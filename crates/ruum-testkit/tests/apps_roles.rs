//! App acceptance cases of milestone M1 (card T-4a), card B-2: listAppRoles, listAppRoleGrants, addAppRoleGrant, removeAppRoleGrant. See
//! `tests/common/apps.rs` for the rules (planned, pending, passed, failed) and `docs/TESTING.md`.

mod common;

#[tokio::test]
async fn apps_roles() {
    common::apps::run_group("app cases: roles", common::apps_roles::cases()).await;
}
