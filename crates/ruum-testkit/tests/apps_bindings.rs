//! App acceptance cases of card B-1: bindings without database bindings (the grants of the
//! preview and live principals, revocation, API bindings that wait for B-3). See
//! `tests/common/apps.rs` for the rules (planned, pending, passed, failed) and
//! `docs/TESTING.md`.

mod common;

#[tokio::test]
async fn apps_bindings() {
    common::apps::run_group("app cases: bindings", common::apps_bindings::cases()).await;
}
