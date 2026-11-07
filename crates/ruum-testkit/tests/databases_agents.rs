//! Database acceptance cases: agents on databases (design 8.5). See `tests/common/mod.rs` for the
//! rules (planned, pending, passed, failed) and `docs/TESTING.md`.

mod common;

#[tokio::test]
async fn databases_agents() {
    common::run_group("database cases: agents", common::agents::cases()).await;
}
