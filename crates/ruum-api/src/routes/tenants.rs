//! Routes of the `tenants` area. See `docs/STEP1.md` for the owner of this file.

use crate::error::{HttpError, not_implemented};
use crate::state::AppState;
use axum::Router;
use axum::routing::{delete, get, patch, post};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/tenants", get(list_tenants).post(create_tenant))
        .route(
            "/v1/tenants/{tenant_id}",
            get(get_tenant).patch(update_tenant),
        )
        .route("/v1/tenants/{tenant_id}/members", get(list_members))
        .route(
            "/v1/tenants/{tenant_id}/members/{user_id}",
            patch(update_member).delete(remove_member),
        )
        .route(
            "/v1/tenants/{tenant_id}/invitations",
            get(list_invitations).post(create_invitation),
        )
        .route(
            "/v1/tenants/{tenant_id}/invitations/{invitation_id}",
            delete(revoke_invitation),
        )
        .route("/v1/invitations/accept", post(accept_invitation))
        .route("/v1/tenants/{tenant_id}/limits", get(get_limits))
}

/// Tenants of the signed-in user. `GET /v1/tenants` (operationId `listTenants`).
pub async fn list_tenants() -> HttpError {
    not_implemented("listTenants")
}

/// Create a team tenant. `POST /v1/tenants` (operationId `createTenant`).
pub async fn create_tenant() -> HttpError {
    not_implemented("createTenant")
}

/// Get a tenant. `GET /v1/tenants/{tenant_id}` (operationId `getTenant`).
pub async fn get_tenant() -> HttpError {
    not_implemented("getTenant")
}

/// Rename a tenant. `PATCH /v1/tenants/{tenant_id}` (operationId `updateTenant`).
pub async fn update_tenant() -> HttpError {
    not_implemented("updateTenant")
}

/// List members and guests. `GET /v1/tenants/{tenant_id}/members` (operationId `listMembers`).
pub async fn list_members() -> HttpError {
    not_implemented("listMembers")
}

/// Change the role of a member. `PATCH /v1/tenants/{tenant_id}/members/{user_id}` (operationId `updateMember`).
pub async fn update_member() -> HttpError {
    not_implemented("updateMember")
}

/// Remove a member. `DELETE /v1/tenants/{tenant_id}/members/{user_id}` (operationId `removeMember`).
pub async fn remove_member() -> HttpError {
    not_implemented("removeMember")
}

/// List open invitations. `GET /v1/tenants/{tenant_id}/invitations` (operationId `listInvitations`).
pub async fn list_invitations() -> HttpError {
    not_implemented("listInvitations")
}

/// Invite a person. `POST /v1/tenants/{tenant_id}/invitations` (operationId `createInvitation`).
pub async fn create_invitation() -> HttpError {
    not_implemented("createInvitation")
}

/// Revoke an invitation. `DELETE /v1/tenants/{tenant_id}/invitations/{invitation_id}` (operationId `revokeInvitation`).
pub async fn revoke_invitation() -> HttpError {
    not_implemented("revokeInvitation")
}

/// Accept an invitation. `POST /v1/invitations/accept` (operationId `acceptInvitation`).
pub async fn accept_invitation() -> HttpError {
    not_implemented("acceptInvitation")
}

/// Plan limits and usage. `GET /v1/tenants/{tenant_id}/limits` (operationId `getLimits`).
pub async fn get_limits() -> HttpError {
    not_implemented("getLimits")
}
