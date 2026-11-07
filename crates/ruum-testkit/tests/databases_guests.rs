//! Database acceptance cases: guests and the host plan (decision 0004-10). See `tests/common/mod.rs` for the
//! rules (planned, pending, passed, failed) and `docs/TESTING.md`.

mod common;

#[tokio::test]
async fn databases_guests() {
    common::run_group("database cases: guests", common::guests::cases()).await;
}
