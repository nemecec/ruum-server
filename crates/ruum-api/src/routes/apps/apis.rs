//! Card B-3 (area `apis`): `listAppApiKeys`, `createAppApiKey`, `revokeAppApiKey`.
//!
//! `501` stubs until the card builds them (made by card W-1).

use super::stub;
use crate::state::AppState;
use axum::Router;
use axum::routing::{delete, get};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/apps/{node_id}/api-keys",
            get(list_app_api_keys).post(create_app_api_key),
        )
        .route(
            "/v1/apps/{node_id}/api-keys/{api_key_id}",
            delete(revoke_app_api_key),
        )
}

stub!(list_app_api_keys, "listAppApiKeys");
stub!(create_app_api_key, "createAppApiKey");
stub!(revoke_app_api_key, "revokeAppApiKey");
