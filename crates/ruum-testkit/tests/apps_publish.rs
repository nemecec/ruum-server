//! App acceptance cases of milestone M1 (card T-4a), card W-3a: publishAppDeployment for people and the app branch of restoreVersion. See
//! `tests/common/apps.rs` for the rules (planned, pending, passed, failed) and `docs/TESTING.md`.

mod common;

#[tokio::test]
async fn apps_publish() {
    common::apps::run_group("app cases: publish", common::apps_publish::cases()).await;
}
