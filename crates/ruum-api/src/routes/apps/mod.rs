//! Routes of the tag `apps` (`docs/STEP3.md`, area 1 and the areas `roles`, `apis` and
//! `telemetry` that live under `/v1/apps/`). The logic is in `ruum_core::apps` (the
//! `AppService` of the services); these handlers parse the request and render the JSON of
//! `contracts/openapi.yaml`.
//!
//! Card W-1 builds `createApp`, `getApp`, `updateAppSettings`, `listAppDeployments` and
//! `getAppDeployment` (in [`apps`]). Every other handler of the tag exists as a `501` stub in
//! the submodule of its card, so later cards change only their own file: `deployments` (W-2,
//! W-3a), `secrets` (W-4), `domains` (W-5), `approvals` (W-3b), `roles` (B-2), `apis` (B-3) and
//! `telemetry` (the O cards). The project publication (V-1) is stubbed here.

pub mod apis;
pub mod approvals;
// The handlers of card W-1 (the file name the card gives).
#[allow(clippy::module_inception)]
pub mod apps;
pub mod deployments;
pub mod domains;
pub mod roles;
pub mod secrets;
pub mod telemetry;

use crate::state::AppState;
use axum::Router;
use axum::routing::get;

pub fn router() -> Router<AppState> {
    Router::new()
        .merge(apps::router())
        .merge(deployments::router())
        .merge(secrets::router())
        .merge(domains::router())
        .merge(approvals::router())
        .merge(roles::router())
        .merge(apis::router())
        .merge(telemetry::router())
        .route(
            "/v1/projects/{node_id}/publication",
            get(get_project_publication).put(update_project_publication),
        )
}

/// A stub of an operation of a later card: `501` for every caller, before any check, as the
/// suites expect of a planned operation (decision 0004-21). It reads nothing, so it shows no
/// data to anyone.
macro_rules! stub {
    ($name:ident, $op:literal) => {
        #[doc = concat!("`", $op, "`: not built yet (`501`).")]
        pub async fn $name() -> crate::error::ApiResult<axum::Json<serde_json::Value>> {
            Err(crate::error::not_implemented($op))
        }
    };
}
pub(crate) use stub;

stub!(get_project_publication, "getProjectPublication");
stub!(update_project_publication, "updateProjectPublication");
