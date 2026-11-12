//! Cards W-2 and W-3a: `createAppDeployment` (previews) and `publishAppDeployment`.
//!
//! `501` stubs until the cards build them (made by card W-1).

use super::stub;
use crate::state::AppState;
use axum::Router;
use axum::routing::post;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/apps/{node_id}/deployments",
            post(create_app_deployment),
        )
        .route(
            "/v1/apps/{node_id}/deployments/{deployment_id}/publish",
            post(publish_app_deployment),
        )
}

stub!(create_app_deployment, "createAppDeployment");
stub!(publish_app_deployment, "publishAppDeployment");
