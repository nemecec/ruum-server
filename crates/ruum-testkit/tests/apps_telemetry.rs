//! Telemetry acceptance cases of card O-2 (the telemetry part of T-4): listAppTelemetryFields,
//! confirmAppTelemetryFields, computeAppPseudonym, searchAppLogs, getAppMetrics, getAppTrace and
//! compareAppDeployment. See `tests/common/apps.rs` for the rules (planned, pending, passed,
//! failed) and `docs/TESTING.md`.

mod common;

#[tokio::test]
async fn apps_telemetry() {
    common::apps::run_group("app cases: telemetry", common::apps_telemetry::cases()).await;
}
