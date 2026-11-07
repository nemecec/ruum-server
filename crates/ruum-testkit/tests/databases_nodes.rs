//! Database acceptance cases: step 1 operations on database nodes (decisions 0004-6, 0004-7, 0004-18). See `tests/common/mod.rs` for the
//! rules (planned, pending, passed, failed) and `docs/TESTING.md`.

mod common;

#[tokio::test]
async fn databases_nodes() {
    common::run_group("database cases: nodes", common::nodes::cases()).await;
}
