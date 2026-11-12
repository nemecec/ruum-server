//! Card W-5: `listAppDomains`, `addAppDomain`, `removeAppDomain`.
//!
//! `501` stubs until the card builds them (made by card W-1).

use super::stub;
use crate::state::AppState;
use axum::Router;
use axum::routing::{delete, get};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/apps/{node_id}/domains",
            get(list_app_domains).post(add_app_domain),
        )
        .route(
            "/v1/apps/{node_id}/domains/{domain_id}",
            delete(remove_app_domain),
        )
}

stub!(list_app_domains, "listAppDomains");
stub!(add_app_domain, "addAppDomain");
stub!(remove_app_domain, "removeAppDomain");
