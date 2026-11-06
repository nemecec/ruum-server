//! Routes of the `sharing` area (`docs/STEP1.md`, area 3). The logic is in
//! `ruum_core::sharing` (the `SharingStore` of the services); these handlers parse the request
//! and render the JSON of `contracts/openapi.yaml`.

use crate::auth::RequestContext;
use crate::error::ApiResult;
use crate::state::AppState;
use axum::Router;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{Json, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, get, patch, post};
use ruum_core::authz::Role;
use ruum_core::identity::{AccessClaims, TokenType};
use ruum_core::paging::{Cursor, PageRequest};
use ruum_core::sharing::rules::{LINK_ACCESS_TOKEN_SECONDS, link_scope};
use ruum_core::sharing::{Grantee, NewShareLink, ShareLinkRole};
use ruum_core::{ApiError, Error, ErrorCode, GrantId, NodeId, ShareLinkId};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/nodes/{node_id}/grants",
            get(list_grants).post(add_grant),
        )
        .route(
            "/v1/nodes/{node_id}/grants/{grant_id}",
            patch(update_grant).delete(remove_grant),
        )
        .route(
            "/v1/nodes/{node_id}/share-links",
            get(list_share_links).post(create_share_link),
        )
        .route("/v1/nodes/{node_id}/sharing", patch(update_node_sharing))
        .route("/v1/share-links/{link_id}", delete(revoke_share_link))
        .route("/v1/share-links/resolve", post(resolve_share_link))
        .route("/v1/shared-with-me", get(list_shared_with_me))
}

/// The paging query of list operations.
#[derive(Debug, Default, Deserialize)]
pub struct PageParams {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

fn page(q: Result<Query<PageParams>, QueryRejection>) -> Result<PageRequest, Error> {
    let Query(p) = q.map_err(|e| {
        Error::Api(
            ApiError::new(ErrorCode::BadRequest, format!("bad query parameters: {e}"))
                .with_hint("see the parameters of this operation in contracts/openapi.yaml"),
        )
    })?;
    Ok(PageRequest {
        cursor: p.cursor.map(Cursor),
        limit: p.limit,
    })
}

fn path<T>(p: Result<Path<T>, PathRejection>) -> Result<T, Error> {
    p.map(|Path(v)| v).map_err(|e| {
        Error::Api(
            ApiError::new(ErrorCode::BadRequest, format!("bad path: {e}"))
                .with_hint("IDs are UUIDs"),
        )
    })
}

fn body<T>(b: Result<Json<T>, JsonRejection>) -> Result<T, Error> {
    b.map(|Json(v)| v).map_err(|e| {
        Error::Api(
            ApiError::new(
                ErrorCode::ValidationFailed,
                format!("bad request body: {e}"),
            )
            .with_hint("see the request schema of this operation in contracts/openapi.yaml"),
        )
    })
}

fn to_json<T: serde::Serialize>(v: &T) -> Result<Value, Error> {
    serde_json::to_value(v).map_err(|e| Error::Internal(e.to_string()))
}

/// `If-Match`: the expected node revision (`"<revision>"` or `<revision>`).
fn if_match(headers: &HeaderMap) -> Result<Option<i64>, Error> {
    let Some(v) = headers.get(axum::http::header::IF_MATCH) else {
        return Ok(None);
    };
    let s = v.to_str().unwrap_or_default().trim();
    let s = s.strip_prefix("W/").unwrap_or(s).trim_matches('"');
    s.parse().map(Some).map_err(|_| {
        Error::Api(
            ApiError::new(ErrorCode::BadRequest, "If-Match must be a node revision")
                .with_hint("send the `revision` of the node, for example If-Match: \"3\""),
        )
    })
}

/// List grants. `GET /v1/nodes/{node_id}/grants` (operationId `listGrants`).
pub async fn list_grants(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    q: Result<Query<PageParams>, QueryRejection>,
) -> ApiResult<Json<Value>> {
    let node = NodeId(path(node)?);
    let p = state
        .services
        .sharing
        .list_grants(&ctx, node, page(q)?)
        .await?;
    Ok(Json(to_json(&p)?))
}

/// The body of `addGrant`.
#[derive(Debug, Deserialize)]
pub struct AddGrantBody {
    pub grantee: Grantee,
    pub role: Role,
    /// Sending an email to the grantee comes with the invitations of the identity area.
    #[serde(default)]
    pub notify: Option<bool>,
    #[serde(default)]
    pub message: Option<String>,
}

/// Add a grant. `POST /v1/nodes/{node_id}/grants` (operationId `addGrant`).
pub async fn add_grant(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    b: Result<Json<AddGrantBody>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let node = NodeId(path(node)?);
    let b = body(b)?;
    if b.message.as_ref().is_some_and(|m| m.chars().count() > 1000) {
        return Err(Error::api(
            ErrorCode::ValidationFailed,
            "message has more than 1000 characters",
        )
        .into());
    }
    let w = state
        .services
        .sharing
        .add_grant(&ctx, node, b.grantee, b.role)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "grant": to_json(&w.value)?, "change": to_json(&w.change)? })),
    ))
}

/// The body of `updateGrant`.
#[derive(Debug, Deserialize)]
pub struct RoleBody {
    pub role: Role,
}

/// Change the role of a grant. `PATCH /v1/nodes/{node_id}/grants/{grant_id}` (operationId
/// `updateGrant`).
pub async fn update_grant(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    ids: Result<Path<(Uuid, Uuid)>, PathRejection>,
    b: Result<Json<RoleBody>, JsonRejection>,
) -> ApiResult<Json<Value>> {
    let (node, grant) = path(ids)?;
    let b = body(b)?;
    let w = state
        .services
        .sharing
        .update_grant(&ctx, NodeId(node), GrantId(grant), b.role)
        .await?;
    Ok(Json(
        json!({ "grant": to_json(&w.value)?, "change": to_json(&w.change)? }),
    ))
}

/// Remove a grant. `DELETE /v1/nodes/{node_id}/grants/{grant_id}` (operationId `removeGrant`).
pub async fn remove_grant(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    ids: Result<Path<(Uuid, Uuid)>, PathRejection>,
) -> ApiResult<Json<Value>> {
    let (node, grant) = path(ids)?;
    let w = state
        .services
        .sharing
        .remove_grant(&ctx, NodeId(node), GrantId(grant))
        .await?;
    Ok(Json(json!({ "change": to_json(&w.change)? })))
}

/// The body of `updateNodeSharing`. `editors_can_share` is required; `null` inherits.
#[derive(Debug, Deserialize)]
pub struct NodeSharingBody {
    #[serde(default, deserialize_with = "present")]
    pub editors_can_share: Option<Option<bool>>,
}

/// `Some(None)` for `null`, `None` when the key is missing.
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<bool>>, D::Error> {
    Option::<bool>::deserialize(d).map(Some)
}

/// Change the sharing settings of a node (`editors_can_share`). `PATCH /v1/nodes/{node_id}/sharing`
/// (operationId `updateNodeSharing`).
pub async fn update_node_sharing(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    headers: HeaderMap,
    b: Result<Json<NodeSharingBody>, JsonRejection>,
) -> ApiResult<Json<Value>> {
    let node = NodeId(path(node)?);
    let b = body(b)?;
    let Some(value) = b.editors_can_share else {
        return Err(Error::api(
            ErrorCode::ValidationFailed,
            "editors_can_share is required (true, false or null)",
        )
        .into());
    };
    let w = state
        .services
        .sharing
        .update_node_sharing(&ctx, node, value, if_match(&headers)?)
        .await?;
    Ok(Json(
        json!({ "node": to_json(&w.value)?, "change": to_json(&w.change)? }),
    ))
}

/// List share links. `GET /v1/nodes/{node_id}/share-links` (operationId `listShareLinks`).
pub async fn list_share_links(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    q: Result<Query<PageParams>, QueryRejection>,
) -> ApiResult<Json<Value>> {
    let node = NodeId(path(node)?);
    let p = state
        .services
        .sharing
        .list_share_links(&ctx, node, page(q)?)
        .await?;
    Ok(Json(to_json(&p)?))
}

/// The page that opens a share link. The token is in the URL fragment, so it never reaches a
/// server log; the web UI reads it and calls `resolveShareLink`.
pub fn share_link_url(public_url: &str, token: &str) -> String {
    format!("{}/s#{token}", public_url.trim_end_matches('/'))
}

/// Create a share link. `POST /v1/nodes/{node_id}/share-links` (operationId `createShareLink`).
pub async fn create_share_link(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    node: Result<Path<Uuid>, PathRejection>,
    b: Result<Json<NewShareLink>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let node = NodeId(path(node)?);
    let req = body(b)?;
    let w = state
        .services
        .sharing
        .create_share_link(&ctx, node, req)
        .await?;
    let mut link = w.value;
    link.url = link
        .token
        .as_deref()
        .map(|t| share_link_url(&state.config.public_url, t));
    Ok((
        StatusCode::CREATED,
        Json(json!({ "share_link": to_json(&link)?, "change": to_json(&w.change)? })),
    ))
}

/// Revoke a share link. `DELETE /v1/share-links/{link_id}` (operationId `revokeShareLink`).
pub async fn revoke_share_link(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    link: Result<Path<Uuid>, PathRejection>,
) -> ApiResult<Json<Value>> {
    let link = ShareLinkId(path(link)?);
    let w = state.services.sharing.revoke_share_link(&ctx, link).await?;
    Ok(Json(json!({ "change": to_json(&w.change)? })))
}

/// The body of `resolveShareLink`.
#[derive(Debug, Deserialize)]
pub struct ResolveBody {
    pub token: String,
    #[serde(default)]
    pub password: Option<String>,
}

/// Open a share link. `POST /v1/share-links/resolve` (operationId `resolveShareLink`). No
/// sign-in. Returns the node and a short-lived access token (`typ` `share_link`) whose scope is
/// the link's node, role and expiry.
pub async fn resolve_share_link(
    State(state): State<AppState>,
    b: Result<Json<ResolveBody>, JsonRejection>,
) -> ApiResult<Json<Value>> {
    let b = body(b)?;
    let (tenant, node, link) = state
        .services
        .sharing
        .resolve_share_link(&b.token, b.password.as_deref())
        .await?;
    let now = ruum_core::now();
    let mut exp = now.unix_timestamp() + LINK_ACCESS_TOKEN_SECONDS;
    if let Some(t) = link.expires_at {
        exp = exp.min(t.unix_timestamp());
    }
    let expires_in = (exp - now.unix_timestamp()).max(1);
    let claims = AccessClaims {
        iss: state.config.public_url.clone(),
        aud: state.config.public_url.clone(),
        typ: TokenType::ShareLink,
        sub: link.id.0,
        tid: Some(tenant),
        obo: None,
        agent: None,
        share_link: Some(link.id),
        scope: Some(link_scope(&link)),
        iat: now.unix_timestamp(),
        exp,
        jti: Uuid::now_v7(),
    };
    let access_token = state.services.tokens.issue(&claims)?;
    let role: ShareLinkRole = link.role;
    Ok(Json(json!({
        "tenant_id": tenant,
        "node": to_json(&node)?,
        "role": role,
        "access_token": access_token,
        "expires_in": expires_in,
    })))
}

/// Shared with me. `GET /v1/shared-with-me` (operationId `listSharedWithMe`).
pub async fn list_shared_with_me(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    q: Result<Query<PageParams>, QueryRejection>,
) -> ApiResult<Json<Value>> {
    let p = state
        .services
        .sharing
        .shared_with_me(&ctx, page(q)?)
        .await?;
    Ok(Json(to_json(&p)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn if_match_reads_revisions() {
        let mut h = HeaderMap::new();
        assert_eq!(if_match(&h).unwrap(), None);
        h.insert(axum::http::header::IF_MATCH, "\"7\"".parse().unwrap());
        assert_eq!(if_match(&h).unwrap(), Some(7));
        h.insert(axum::http::header::IF_MATCH, "W/\"8\"".parse().unwrap());
        assert_eq!(if_match(&h).unwrap(), Some(8));
        h.insert(axum::http::header::IF_MATCH, "x".parse().unwrap());
        assert!(if_match(&h).is_err());
    }

    #[test]
    fn link_urls_keep_the_token_out_of_the_path() {
        assert_eq!(
            share_link_url("https://ruum.localhost/", "ruum_link_abc"),
            "https://ruum.localhost/s#ruum_link_abc"
        );
    }

    #[test]
    fn sharing_body_tells_null_from_missing() {
        let b: NodeSharingBody = serde_json::from_str(r#"{"editors_can_share":null}"#).unwrap();
        assert_eq!(b.editors_can_share, Some(None));
        let b: NodeSharingBody = serde_json::from_str("{}").unwrap();
        assert_eq!(b.editors_can_share, None);
        let b: NodeSharingBody = serde_json::from_str(r#"{"editors_can_share":false}"#).unwrap();
        assert_eq!(b.editors_can_share, Some(Some(false)));
    }
}
