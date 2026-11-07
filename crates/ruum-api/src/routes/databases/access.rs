//! Access grants and approvals (cards A-1, A-2): `501` stubs until they are built.

use super::stub;
use crate::state::AppState;
use axum::Router;
use axum::routing::{delete, get, post};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/databases/{node_id}/access-grants",
            get(list_database_access_grants).post(create_database_access_grant),
        )
        .route(
            "/v1/databases/{node_id}/access-grants/{grant_id}",
            delete(revoke_database_access_grant),
        )
        .route(
            "/v1/databases/{node_id}/access-grant-requests",
            post(request_database_access_grant),
        )
        .route(
            "/v1/database-approvals",
            get(list_pending_database_approvals),
        )
        .route(
            "/v1/databases/{node_id}/approvals",
            get(list_database_approvals),
        )
        .route(
            "/v1/databases/{node_id}/approvals/{approval_id}",
            get(get_database_approval),
        )
        .route(
            "/v1/databases/{node_id}/approvals/{approval_id}/decision",
            post(decide_database_approval),
        )
}

stub!(list_database_access_grants, "listDatabaseAccessGrants");
stub!(create_database_access_grant, "createDatabaseAccessGrant");
stub!(revoke_database_access_grant, "revokeDatabaseAccessGrant");
stub!(request_database_access_grant, "requestDatabaseAccessGrant");
stub!(
    list_pending_database_approvals,
    "listPendingDatabaseApprovals"
);
stub!(list_database_approvals, "listDatabaseApprovals");
stub!(get_database_approval, "getDatabaseApproval");
stub!(decide_database_approval, "decideDatabaseApproval");
