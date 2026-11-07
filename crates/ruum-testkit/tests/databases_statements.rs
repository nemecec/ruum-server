//! Database acceptance cases: statement and branch rules (decision 0004-35). See `tests/common/mod.rs` for the
//! rules (planned, pending, passed, failed) and `docs/TESTING.md`.

mod common;

#[tokio::test]
async fn databases_statements() {
    common::run_group("database cases: statements", common::statements::cases()).await;
}
