//! Routes of the `agents` area: agents, scopes, API keys, token exchange, activity and undo, and
//! OAuth 2.1 for MCP clients. See `docs/STEP1.md` for the owner of this file.

use crate::auth::{
    JsonBody, Paging, Principal, QueryParams, RequestContext, authenticate, require_user,
};
use crate::error::{ApiResult, HttpError};
use crate::state::AppState;
use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, LOCATION, PRAGMA};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use ruum_core::agents::oauth::{
    AuthorizationRequest, ClientRegistration, Consent, OAuthError, REQUEST_SECONDS, TokenRequest,
    TokenResponse, redirect_with, resource_audience,
};
use ruum_core::agents::{API_KEY_TOKEN_TYPE, AgentScope, AgentUpdate, NewAgent, UndoRequest, undo};
use ruum_core::{AgentId, AgentKeyId, ApiError, Error, ErrorCode, UserId};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use time::OffsetDateTime;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/agents", get(list_agents).post(register_agent))
        .route("/v1/agents/{agent_id}", get(get_agent).patch(update_agent))
        .route("/v1/agents/{agent_id}/disable", post(disable_agent))
        .route("/v1/agents/{agent_id}/enable", post(enable_agent))
        .route(
            "/v1/agents/{agent_id}/keys",
            get(list_agent_keys).post(create_agent_key),
        )
        .route(
            "/v1/agents/{agent_id}/keys/{key_id}",
            delete(revoke_agent_key),
        )
        .route("/v1/agents/{agent_id}/activity", get(list_agent_activity))
        .route("/v1/agents/{agent_id}/undo", post(undo_agent))
        .route("/v1/oauth/register", post(register_oauth_client))
        .route("/v1/oauth/clients/{client_id}", get(get_oauth_client))
        .route("/v1/oauth/authorize", get(oauth_authorize))
        .route("/v1/oauth/consent", post(oauth_consent))
        .route("/v1/oauth/token", post(exchange_token))
}

fn agent_id(s: &str) -> Result<AgentId, HttpError> {
    s.parse().map_err(|_| HttpError(Error::not_found("agent")))
}

/// The OAuth error response.
fn oauth_error(e: OAuthError) -> Response {
    let status = StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_REQUEST);
    let mut res = (status, Json(json!(e))).into_response();
    res.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

// ---------------------------------------------------------------------------- OAuth

/// Register an OAuth client (RFC 7591). `POST /v1/oauth/register` (operationId
/// `registerOAuthClient`).
pub async fn register_oauth_client(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> Response {
    let req: ClientRegistration = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return oauth_error(OAuthError::new(
                "invalid_client_metadata",
                format!("the body is not a valid registration: {e}"),
                ErrorCode::ValidationFailed,
            ));
        }
    };
    match state.services.oauth.register_client(req).await {
        Ok(c) => (StatusCode::CREATED, Json(json!(c))).into_response(),
        Err(e) => oauth_error(e),
    }
}

/// Get a registered OAuth client. `GET /v1/oauth/clients/{client_id}` (operationId
/// `getOAuthClient`).
pub async fn get_oauth_client(
    State(state): State<AppState>,
    Principal(who): Principal,
    Path(client_id): Path<String>,
) -> ApiResult<Json<Value>> {
    require_user(&who)?;
    let c = state.services.oauth.get_client(&client_id).await?;
    Ok(Json(json!(c)))
}

#[derive(Deserialize)]
pub struct AuthorizeQuery {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    state: Option<String>,
    #[allow(dead_code)]
    scope: Option<String>,
    resource: Option<String>,
}

fn found(location: &str) -> ApiResult<Response> {
    let mut res = StatusCode::FOUND.into_response();
    res.headers_mut().insert(
        LOCATION,
        HeaderValue::from_str(location)
            .map_err(|_| HttpError(Error::Internal("bad redirect".into())))?,
    );
    Ok(res)
}

/// Start an OAuth authorization. `GET /v1/oauth/authorize` (operationId `oauthAuthorize`).
pub async fn oauth_authorize(
    State(state): State<AppState>,
    headers: HeaderMap,
    QueryParams(q): QueryParams<AuthorizeQuery>,
) -> ApiResult<Response> {
    let bad = |m: &str| {
        HttpError(Error::Api(
            ApiError::new(ErrorCode::BadRequest, m)
                .with_hint("check client_id and redirect_uri of the OAuth client"),
        ))
    };
    // An unknown client or a wrong redirect URI is a 400 page, never a redirect.
    let client_id = q
        .client_id
        .clone()
        .ok_or_else(|| bad("client_id is missing"))?;
    let redirect_uri = q
        .redirect_uri
        .clone()
        .ok_or_else(|| bad("redirect_uri is missing"))?;
    let client = state
        .services
        .oauth
        .get_client(&client_id)
        .await
        .map_err(|_| bad("unknown client_id"))?;
    if !client.redirect_uris.contains(&redirect_uri) {
        return Err(bad("redirect_uri is not registered for this client"));
    }
    let issuer = state.services.tokens.issuer().to_string();
    let back = |error: &str, description: &str| -> ApiResult<Response> {
        let url = redirect_with(
            &redirect_uri,
            &[("error", error), ("error_description", description)],
            q.state.as_deref(),
            &issuer,
        )?;
        found(&url)
    };
    if q.response_type.as_deref() != Some("code") {
        return back("unsupported_response_type", "response_type must be code");
    }
    if q.code_challenge_method.as_deref() != Some("S256") {
        return back("invalid_request", "code_challenge_method must be S256");
    }
    let Some(challenge) = q
        .code_challenge
        .clone()
        .filter(|c| ruum_core::identity::secret::is_pkce_value(c))
    else {
        return back("invalid_request", "code_challenge is missing or not valid");
    };
    let Ok(resource) = resource_audience(q.resource.as_deref(), state.services.tokens.audience())
    else {
        return back(
            "invalid_target",
            "resource must be the API origin or its /mcp URL",
        );
    };
    let request = state
        .services
        .oauth
        .start_authorization(AuthorizationRequest {
            client_id,
            redirect_uri,
            code_challenge: challenge,
            state: q.state.clone(),
            resource,
            expires_at: ruum_core::now() + time::Duration::seconds(REQUEST_SECONDS),
        })
        .await?;
    let consent_path = format!(
        "/oauth/consent?{}",
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("request", &request)
            .finish()
    );
    let signed_in = matches!(
        authenticate(&state, &axum::http::Method::GET, &headers).await,
        Ok(Some(_))
    );
    if signed_in {
        found(&consent_path)
    } else {
        let login = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("return_to", &consent_path)
            .finish();
        found(&format!("/v1/auth/login?{login}"))
    }
}

/// Approve or deny an OAuth authorization. `POST /v1/oauth/consent` (operationId `oauthConsent`).
pub async fn oauth_consent(
    State(state): State<AppState>,
    Principal(who): Principal,
    JsonBody(body): JsonBody<Consent>,
) -> ApiResult<Json<Value>> {
    require_user(&who)?;
    let redirect_to = state.services.oauth.consent(&who.actor, body).await?;
    Ok(Json(json!({ "redirect_to": redirect_to })))
}

/// Get an access token (authorization code, refresh token or API key exchange).
/// `POST /v1/oauth/token` (operationId `exchangeToken`).
pub async fn exchange_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let is_json = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.starts_with("application/json"));
    let fields: Map<String, Value> = if is_json {
        match serde_json::from_slice(&body) {
            Ok(m) => m,
            Err(_) => return oauth_error(OAuthError::invalid_request("the body is not JSON")),
        }
    } else {
        url::form_urlencoded::parse(&body)
            .map(|(k, v)| (k.into_owned(), Value::String(v.into_owned())))
            .collect()
    };
    let grant = fields
        .get("grant_type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if !matches!(
        grant.as_str(),
        "authorization_code" | "refresh_token" | "urn:ietf:params:oauth:grant-type:token-exchange"
    ) {
        return oauth_error(OAuthError::new(
            if grant.is_empty() {
                "invalid_request"
            } else {
                "unsupported_grant_type"
            },
            "grant_type must be authorization_code, refresh_token or token exchange",
            ErrorCode::BadRequest,
        ));
    }
    let req: TokenRequest = match serde_json::from_value(Value::Object(fields)) {
        Ok(r) => r,
        Err(e) => {
            return oauth_error(OAuthError::invalid_request(format!(
                "the token request is not valid: {e}"
            )));
        }
    };
    let result: Result<TokenResponse, OAuthError> = match req {
        TokenRequest::TokenExchange {
            subject_token,
            subject_token_type,
        } => {
            if subject_token_type != API_KEY_TOKEN_TYPE {
                Err(OAuthError::invalid_request(format!(
                    "subject_token_type must be {API_KEY_TOKEN_TYPE}"
                )))
            } else {
                state
                    .services
                    .agents
                    .exchange_key(&subject_token)
                    .await
                    .map(|access| TokenResponse {
                        access,
                        refresh_token: None,
                    })
                    .map_err(OAuthError::from)
            }
        }
        other => state.services.oauth.token(other).await,
    };
    match result {
        Ok(t) => {
            let mut res = Json(json!(t)).into_response();
            res.headers_mut()
                .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
            res.headers_mut()
                .insert(PRAGMA, HeaderValue::from_static("no-cache"));
            res
        }
        Err(e) => oauth_error(e),
    }
}

// ---------------------------------------------------------------------------- agents

#[derive(Deserialize)]
pub struct ListAgentsQuery {
    owner: Option<UserId>,
    cursor: Option<String>,
    limit: Option<u32>,
}

/// List agents. `GET /v1/agents` (operationId `listAgents`).
pub async fn list_agents(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    QueryParams(q): QueryParams<ListAgentsQuery>,
) -> ApiResult<Json<Value>> {
    let page = state
        .services
        .agents
        .list(
            &ctx,
            q.owner,
            Paging {
                cursor: q.cursor,
                limit: q.limit,
            }
            .page()?,
        )
        .await?;
    Ok(Json(json!(page)))
}

/// Register an agent. `POST /v1/agents` (operationId `registerAgent`).
pub async fn register_agent(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    JsonBody(body): JsonBody<NewAgent>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    ruum_core::actor::require_user_actor(&ctx)?;
    let a = state.services.agents.register(&ctx, body).await?;
    Ok((StatusCode::CREATED, Json(json!(a))))
}

/// Get an agent. `GET /v1/agents/{agent_id}` (operationId `getAgent`).
pub async fn get_agent(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let a = state.services.agents.get(&ctx, agent_id(&id)?).await?;
    Ok(Json(json!(a)))
}

/// The body of `updateAgent`: `description: null` clears it.
#[derive(Deserialize)]
pub struct UpdateAgentBody {
    name: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    description: Option<Option<String>>,
    scope: Option<AgentScope>,
}

fn double_option<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// Change an agent's name or scope. `PATCH /v1/agents/{agent_id}` (operationId `updateAgent`).
pub async fn update_agent(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<UpdateAgentBody>,
) -> ApiResult<Json<Value>> {
    ruum_core::actor::require_user_actor(&ctx)?;
    if body.name.is_none() && body.description.is_none() && body.scope.is_none() {
        return Err(HttpError(ruum_core::identity::invalid(
            "give name, description or scope",
        )));
    }
    let a = state
        .services
        .agents
        .update(
            &ctx,
            agent_id(&id)?,
            AgentUpdate {
                name: body.name,
                description: body.description,
                scope: body.scope,
            },
        )
        .await?;
    Ok(Json(json!(a)))
}

/// Disable an agent. `POST /v1/agents/{agent_id}/disable` (operationId `disableAgent`).
pub async fn disable_agent(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    ruum_core::actor::require_user_actor(&ctx)?;
    let a = state
        .services
        .agents
        .set_disabled(&ctx, agent_id(&id)?, true)
        .await?;
    state.services.oauth.end_grants(ctx.tenant, a.id).await?;
    Ok(Json(json!(a)))
}

/// Enable an agent. `POST /v1/agents/{agent_id}/enable` (operationId `enableAgent`).
pub async fn enable_agent(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    ruum_core::actor::require_user_actor(&ctx)?;
    let a = state
        .services
        .agents
        .set_disabled(&ctx, agent_id(&id)?, false)
        .await?;
    Ok(Json(json!(a)))
}

/// List the API keys of an agent. `GET /v1/agents/{agent_id}/keys` (operationId `listAgentKeys`).
pub async fn list_agent_keys(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    ruum_core::actor::require_user_actor(&ctx)?;
    let items = state
        .services
        .agents
        .list_keys(&ctx, agent_id(&id)?)
        .await?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize, Default)]
pub struct CreateKey {
    name: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    expires_at: Option<OffsetDateTime>,
}

/// Create an API key. `POST /v1/agents/{agent_id}/keys` (operationId `createAgentKey`).
pub async fn create_agent_key(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<CreateKey>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    ruum_core::actor::require_user_actor(&ctx)?;
    let (key, secret) = state
        .services
        .agents
        .create_key(&ctx, agent_id(&id)?, body.name, body.expires_at)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "key": key, "secret": secret })),
    ))
}

/// Revoke an API key. `DELETE /v1/agents/{agent_id}/keys/{key_id}` (operationId `revokeAgentKey`).
pub async fn revoke_agent_key(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    Path((id, key_id)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    ruum_core::actor::require_user_actor(&ctx)?;
    let agent = agent_id(&id)?;
    let key: AgentKeyId = key_id
        .parse()
        .map_err(|_| HttpError(Error::not_found("key")))?;
    state.services.agents.revoke_key(&ctx, agent, key).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct ActivityQuery {
    #[serde(default, with = "time::serde::rfc3339::option")]
    since: Option<OffsetDateTime>,
    cursor: Option<String>,
    limit: Option<u32>,
}

/// Changes made by an agent. `GET /v1/agents/{agent_id}/activity` (operationId `listAgentActivity`).
pub async fn list_agent_activity(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    Path(id): Path<String>,
    QueryParams(q): QueryParams<ActivityQuery>,
) -> ApiResult<Json<Value>> {
    let page = state
        .services
        .agents
        .activity(
            &ctx,
            agent_id(&id)?,
            q.since,
            Paging {
                cursor: q.cursor,
                limit: q.limit,
            }
            .page()?,
        )
        .await?;
    Ok(Json(json!(page)))
}

/// Undo what an agent did. `POST /v1/agents/{agent_id}/undo` (operationId `undoAgent`).
pub async fn undo_agent(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<UndoRequest>,
) -> ApiResult<Json<Value>> {
    ruum_core::actor::require_user_actor(&ctx)?;
    let report = undo::undo(&state.services, &ctx, agent_id(&id)?, body).await?;
    Ok(Json(json!(report)))
}
