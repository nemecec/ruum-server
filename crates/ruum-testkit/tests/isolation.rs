//! The tenant isolation suite. See `ruum_testkit::isolation` and `docs/TESTING.md`.

#[tokio::test]
async fn tenant_isolation() {
    ruum_testkit::isolation::run().await.finish();
}
