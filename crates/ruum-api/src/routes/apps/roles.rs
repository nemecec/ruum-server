//! Card B-2 (area `roles`): `listAppRoles`, `listAppRoleGrants`, `addAppRoleGrant`,
//! `removeAppRoleGrant`.
//!
//! `501` stubs until the card builds them (made by card W-1).

use super::stub;
use crate::state::AppState;
use axum::Router;
use axum::routing::{delete, get};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/apps/{node_id}/roles", get(list_app_roles))
        .route(
            "/v1/apps/{node_id}/role-grants",
            get(list_app_role_grants).post(add_app_role_grant),
        )
        .route(
            "/v1/apps/{node_id}/role-grants/{role_grant_id}",
            delete(remove_app_role_grant),
        )
}

stub!(list_app_roles, "listAppRoles");
stub!(list_app_role_grants, "listAppRoleGrants");
stub!(add_app_role_grant, "addAppRoleGrant");
stub!(remove_app_role_grant, "removeAppRoleGrant");
