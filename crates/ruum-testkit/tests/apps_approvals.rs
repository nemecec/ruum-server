//! App acceptance cases of card W-3b: the approvals of agents' publishes. See
//! `tests/common/apps.rs` for the rules (planned, pending, passed, failed) and `docs/TESTING.md`.

mod common;

#[tokio::test]
async fn apps_approvals() {
    common::apps::run_group("app cases: approvals", common::apps_approvals::cases()).await;
}
