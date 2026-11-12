//! App acceptance cases of milestone M1 (card T-4a), card W-1: createApp, getApp, updateAppSettings, listAppDeployments, getAppDeployment. See
//! `tests/common/apps.rs` for the rules (planned, pending, passed, failed) and `docs/TESTING.md`.

mod common;

#[tokio::test]
async fn apps_settings() {
    common::apps::run_group("app cases: settings", common::apps_settings::cases()).await;
}
