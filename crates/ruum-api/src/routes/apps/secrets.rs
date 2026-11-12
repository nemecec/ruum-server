//! Card W-4: `listAppSecrets`, `setAppSecret`, `rotateAppSecret`.
//!
//! `501` stubs until the card builds them (made by card W-1).

use super::stub;
use crate::state::AppState;
use axum::Router;
use axum::routing::{get, post, put};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/apps/{node_id}/secrets", get(list_app_secrets))
        .route(
            "/v1/apps/{node_id}/secrets/{secret_name}",
            put(set_app_secret),
        )
        .route(
            "/v1/apps/{node_id}/secrets/{secret_name}/rotate",
            post(rotate_app_secret),
        )
}

stub!(list_app_secrets, "listAppSecrets");
stub!(set_app_secret, "setAppSecret");
stub!(rotate_app_secret, "rotateAppSecret");
