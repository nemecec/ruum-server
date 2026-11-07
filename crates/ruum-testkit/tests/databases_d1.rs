//! Database acceptance cases: card D-1: createDatabase, getDatabase, updateDatabaseManifest. See `tests/common/mod.rs` for the
//! rules (planned, pending, passed, failed) and `docs/TESTING.md`.

mod common;

#[tokio::test]
async fn databases_d1() {
    common::run_group("database cases: d1", common::d1::cases()).await;
}
