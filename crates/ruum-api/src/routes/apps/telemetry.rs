//! The area `telemetry` (the O cards of `docs/STEP3.md`): `searchAppLogs`, `getAppMetrics`,
//! `getAppTrace`, `compareAppDeployment`, `listAppTelemetryFields`,
//! `confirmAppTelemetryFields`, `computeAppPseudonym`.
//!
//! `501` stubs until the cards build them (made by card W-1).

use super::stub;
use crate::state::AppState;
use axum::Router;
use axum::routing::{get, post};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/apps/{node_id}/logs", get(search_app_logs))
        .route("/v1/apps/{node_id}/metrics", get(get_app_metrics))
        .route("/v1/apps/{node_id}/traces/{trace_id}", get(get_app_trace))
        .route(
            "/v1/apps/{node_id}/deployments/{deployment_id}/comparison",
            get(compare_app_deployment),
        )
        .route(
            "/v1/apps/{node_id}/telemetry-fields",
            get(list_app_telemetry_fields),
        )
        .route(
            "/v1/apps/{node_id}/telemetry-fields/confirm",
            post(confirm_app_telemetry_fields),
        )
        .route("/v1/apps/{node_id}/pseudonyms", post(compute_app_pseudonym))
}

stub!(search_app_logs, "searchAppLogs");
stub!(get_app_metrics, "getAppMetrics");
stub!(get_app_trace, "getAppTrace");
stub!(compare_app_deployment, "compareAppDeployment");
stub!(list_app_telemetry_fields, "listAppTelemetryFields");
stub!(confirm_app_telemetry_fields, "confirmAppTelemetryFields");
stub!(compute_app_pseudonym, "computeAppPseudonym");
