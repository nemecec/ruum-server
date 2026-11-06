//! Routes of the `tenants` area: personal and team tenants, members, invitations, plan limits.
//! See `docs/STEP1.md` for the owner of this file.

use crate::auth::{
    JsonBody, Paging, Principal, QueryParams, refuse_share_link, require_user, tenant_context,
};
use crate::error::{ApiResult, HttpError};
use crate::state::AppState;
use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, get, patch, post};
use ruum_core::identity::Authenticated;
use ruum_core::tenants::{MemberRole, TenantPolicyChange, tenant_limits};
use ruum_core::{Context, Error, InvitationId, PrincipalKind, TenantId, UserId};
use serde::Deserialize;
use serde_json::{Value, json};

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

/// Parses a UUID path parameter; a value that is not a UUID is `not_found`.
fn parse<T: std::str::FromStr>(s: &str, what: &str) -> Result<T, HttpError> {
    s.parse().map_err(|_| HttpError(Error::not_found(what)))
}

/// The context of the caller in the tenant of the path.
async fn ctx_of(
    state: &AppState,
    who: &Authenticated,
    tenant: &str,
    headers: &HeaderMap,
) -> Result<Context, HttpError> {
    refuse_share_link(who)?;
    let tenant: TenantId = parse(tenant, "tenant")?;
    Ok(tenant_context(state, who, tenant, headers).await?)
}

/// Tenants of the signed-in user. `GET /v1/tenants` (operationId `listTenants`).
pub async fn list_tenants(
    State(state): State<AppState>,
    Principal(who): Principal,
) -> ApiResult<Json<Value>> {
    refuse_share_link(&who)?;
    let user = who.user().ok_or_else(Error::user_required)?;
    let mut list = state.services.tenants.list_for_user(user).await?;
    // An agent sees only the tenant it is bound to.
    if who.actor.actor.kind == PrincipalKind::Agent {
        list.retain(|(t, _)| Some(t.id) == who.bound_tenant);
    }
    let items: Vec<Value> = list
        .into_iter()
        .map(|(tenant, role)| json!({ "tenant": tenant, "role": role }))
        .collect();
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
pub struct CreateTenant {
    name: String,
}

/// Create a team tenant. `POST /v1/tenants` (operationId `createTenant`).
pub async fn create_tenant(
    State(state): State<AppState>,
    Principal(who): Principal,
    JsonBody(body): JsonBody<CreateTenant>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    require_user(&who)?;
    let t = state
        .services
        .tenants
        .create_team(&who.actor, body.name)
        .await?;
    Ok((StatusCode::CREATED, Json(json!(t))))
}

/// Get a tenant. `GET /v1/tenants/{tenant_id}` (operationId `getTenant`).
pub async fn get_tenant(
    State(state): State<AppState>,
    Principal(who): Principal,
    Path(tenant_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    let ctx = ctx_of(&state, &who, &tenant_id, &headers).await?;
    Ok(Json(json!(state.services.tenants.get(&ctx).await?)))
}

#[derive(Deserialize)]
pub struct UpdateTenant {
    name: Option<String>,
    policy: Option<TenantPolicyChange>,
}

/// Rename a tenant or change its policy. `PATCH /v1/tenants/{tenant_id}` (operationId `updateTenant`).
pub async fn update_tenant(
    State(state): State<AppState>,
    Principal(who): Principal,
    Path(tenant_id): Path<String>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<UpdateTenant>,
) -> ApiResult<Json<Value>> {
    let ctx = ctx_of(&state, &who, &tenant_id, &headers).await?;
    ruum_core::actor::require_user_actor(&ctx)?;
    if body.name.is_none() && body.policy.is_none() {
        return Err(HttpError(ruum_core::identity::invalid(
            "give name or policy",
        )));
    }
    let t = state
        .services
        .tenants
        .update(&ctx, body.name, body.policy)
        .await?;
    Ok(Json(json!(t)))
}

/// List members and guests. `GET /v1/tenants/{tenant_id}/members` (operationId `listMembers`).
pub async fn list_members(
    State(state): State<AppState>,
    Principal(who): Principal,
    Path(tenant_id): Path<String>,
    headers: HeaderMap,
    QueryParams(paging): QueryParams<Paging>,
) -> ApiResult<Json<Value>> {
    let ctx = ctx_of(&state, &who, &tenant_id, &headers).await?;
    let page = state
        .services
        .tenants
        .list_members(&ctx, paging.page()?)
        .await?;
    Ok(Json(json!(page)))
}

#[derive(Deserialize)]
pub struct UpdateMember {
    role: MemberRole,
}

/// Change the role of a member. `PATCH /v1/tenants/{tenant_id}/members/{user_id}` (operationId `updateMember`).
pub async fn update_member(
    State(state): State<AppState>,
    Principal(who): Principal,
    Path((tenant_id, user_id)): Path<(String, String)>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<UpdateMember>,
) -> ApiResult<Json<Value>> {
    let ctx = ctx_of(&state, &who, &tenant_id, &headers).await?;
    ruum_core::actor::require_user_actor(&ctx)?;
    let user: UserId = parse(&user_id, "member")?;
    let m = state
        .services
        .tenants
        .set_member_role(&ctx, user, body.role)
        .await?;
    Ok(Json(json!(m)))
}

/// Remove a member. `DELETE /v1/tenants/{tenant_id}/members/{user_id}` (operationId `removeMember`).
pub async fn remove_member(
    State(state): State<AppState>,
    Principal(who): Principal,
    Path((tenant_id, user_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<StatusCode> {
    let ctx = ctx_of(&state, &who, &tenant_id, &headers).await?;
    ruum_core::actor::require_user_actor(&ctx)?;
    let user: UserId = parse(&user_id, "member")?;
    state.services.tenants.remove_member(&ctx, user).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// List open invitations. `GET /v1/tenants/{tenant_id}/invitations` (operationId `listInvitations`).
pub async fn list_invitations(
    State(state): State<AppState>,
    Principal(who): Principal,
    Path(tenant_id): Path<String>,
    headers: HeaderMap,
    QueryParams(paging): QueryParams<Paging>,
) -> ApiResult<Json<Value>> {
    let ctx = ctx_of(&state, &who, &tenant_id, &headers).await?;
    ruum_core::actor::require_user_actor(&ctx)?;
    let page = state
        .services
        .tenants
        .list_invitations(&ctx, paging.page()?)
        .await?;
    Ok(Json(json!(page)))
}

#[derive(Deserialize)]
pub struct CreateInvitation {
    email: String,
    role: MemberRole,
    message: Option<String>,
}

/// Invite a person. `POST /v1/tenants/{tenant_id}/invitations` (operationId `createInvitation`).
pub async fn create_invitation(
    State(state): State<AppState>,
    Principal(who): Principal,
    Path(tenant_id): Path<String>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<CreateInvitation>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let ctx = ctx_of(&state, &who, &tenant_id, &headers).await?;
    ruum_core::actor::require_user_actor(&ctx)?;
    let (inv, _token) = state
        .services
        .tenants
        .invite(&ctx, body.email, body.role, body.message)
        .await?;
    Ok((StatusCode::CREATED, Json(json!(inv))))
}

/// Revoke an invitation. `DELETE /v1/tenants/{tenant_id}/invitations/{invitation_id}` (operationId `revokeInvitation`).
pub async fn revoke_invitation(
    State(state): State<AppState>,
    Principal(who): Principal,
    Path((tenant_id, invitation_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<StatusCode> {
    let ctx = ctx_of(&state, &who, &tenant_id, &headers).await?;
    ruum_core::actor::require_user_actor(&ctx)?;
    let id: InvitationId = parse(&invitation_id, "invitation")?;
    state.services.tenants.revoke_invitation(&ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct AcceptInvitation {
    token: String,
}

/// Accept an invitation. `POST /v1/invitations/accept` (operationId `acceptInvitation`).
pub async fn accept_invitation(
    State(state): State<AppState>,
    Principal(who): Principal,
    JsonBody(body): JsonBody<AcceptInvitation>,
) -> ApiResult<Json<Value>> {
    require_user(&who)?;
    let m = state
        .services
        .tenants
        .accept_invitation(&who.actor, &body.token)
        .await?;
    Ok(Json(json!(m)))
}

/// Plan limits and usage. `GET /v1/tenants/{tenant_id}/limits` (operationId `getLimits`).
pub async fn get_limits(
    State(state): State<AppState>,
    Principal(who): Principal,
    Path(tenant_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    let ctx = ctx_of(&state, &who, &tenant_id, &headers).await?;
    let usage = state.services.tenants.usage(&ctx).await?;
    let tenant = state.services.tenants.get(&ctx).await?;
    let limits = tenant_limits(tenant.plan, usage.members);
    Ok(Json(json!({
        "plan": tenant.plan,
        "limits": limits,
        "usage": usage,
    })))
}
