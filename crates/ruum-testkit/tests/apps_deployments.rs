//! App acceptance cases of milestone M1 (card T-4a), card W-2: createAppDeployment of a static app, hostile archives, secrets. See
//! `tests/common/apps.rs` for the rules (planned, pending, passed, failed) and `docs/TESTING.md`.

mod common;

#[tokio::test]
async fn apps_deployments() {
    common::apps::run_group("app cases: deployments", common::apps_deploy::cases()).await;
}
