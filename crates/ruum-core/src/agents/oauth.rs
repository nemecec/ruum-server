//! OAuth 2.1 for agents (decision 3, `docs/DESIGN.md` sections 3.4 and 4.2): the first way an
//! agent connects is a remote MCP client with OAuth. Ruum is the authorization server for agent
//! tokens and signs them itself ([`crate::identity::TokenIssuer`]); Zitadel only signs the user
//! in before the consent screen.
//!
//! The flow (endpoints in `contracts/openapi.yaml`, tag `agents`):
//!
//! 1. The client reads `/.well-known/oauth-protected-resource` and
//!    `/.well-known/oauth-authorization-server`.
//! 2. It registers (`POST /v1/oauth/register`, RFC 7591): a public client with PKCE, no secret
//!    (table `oauth_clients`, the one table without a tenant).
//! 3. It sends the browser to `GET /v1/oauth/authorize`. The server checks the client, the exact
//!    redirect URI and the PKCE challenge, and redirects to the consent screen of the web UI with
//!    a signed `request` value ([`AuthorizationRequest`]). No row is stored for it.
//! 4. The signed-in user approves (`POST /v1/oauth/consent`, users only): a new agent, or an
//!    existing agent of the user, in one tenant, with an `AgentScope`. The binding is a row of
//!    `agent_oauth_grants`; a one-time code goes back to the client.
//! 5. The client gets tokens at `POST /v1/oauth/token` (`authorization_code` with the PKCE
//!    verifier, then `refresh_token`). Access tokens live 15 minutes (`typ` `agent`, `aud` = the
//!    resource). Refresh tokens rotate; reuse of an old one ends the grant.
//!
//! Disabling the agent, or removing its user from the tenant, ends its grants.

use crate::actor::{Actor, Context};
use crate::agents::{AccessToken, NewAgent};
use crate::error::{Error, ErrorCode, Result};
use crate::identity::secret;
use crate::ids::{AgentId, TenantId};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

pub use memory::MemoryOAuthServer;
pub(crate) use memory::{check_consent, open_request as open_consent_request};

#[path = "oauth_memory.rs"]
mod memory;

/// How long a consent request (the signed `request` value) lives.
pub const REQUEST_SECONDS: i64 = 10 * 60;
/// How long an authorization code lives.
pub const CODE_SECONDS: i64 = 60;
/// How long a refresh token lives after its last use.
pub const REFRESH_DAYS: i64 = 30;
/// The visible prefix of refresh tokens.
pub const REFRESH_PREFIX: &str = "ruum_rt_";
/// The visible prefix of client IDs.
pub const CLIENT_PREFIX: &str = "ruum_client_";
/// The purpose of the sealed consent request ([`crate::identity::TokenIssuer::seal`]).
pub const REQUEST_PURPOSE: &str = "oauth_request";

/// A registration request (RFC 7591).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClientRegistration {
    pub client_name: String,
    pub redirect_uris: Vec<String>,
    #[serde(default)]
    pub grant_types: Vec<String>,
    #[serde(default)]
    pub response_types: Vec<String>,
    #[serde(default)]
    pub token_endpoint_auth_method: Option<String>,
    #[serde(default)]
    pub client_uri: Option<String>,
    #[serde(default)]
    pub logo_uri: Option<String>,
    #[serde(default)]
    pub software_id: Option<String>,
    #[serde(default)]
    pub software_version: Option<String>,
}

/// A registered public client.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OAuthClient {
    pub client_id: String,
    /// Seconds since the epoch.
    pub client_id_issued_at: i64,
    pub client_name: String,
    pub redirect_uris: Vec<String>,
    pub grant_types: Vec<String>,
    pub response_types: Vec<String>,
    /// Always `none` (public clients with PKCE).
    pub token_endpoint_auth_method: String,
    pub client_uri: Option<String>,
    pub logo_uri: Option<String>,
}

/// The checked parameters of `GET /v1/oauth/authorize`. The server signs them into the `request`
/// value of the consent screen URL (10 minutes) and keeps no row.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthorizationRequest {
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub state: Option<String>,
    /// RFC 8707; the `aud` of the tokens.
    pub resource: String,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

/// The decision of the consent screen (`OAuthConsentRequest`): `agent_id` binds an existing
/// agent of the user, `new_agent` creates one; not both.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Consent {
    /// The signed `request` value.
    pub request: String,
    pub approve: bool,
    #[serde(default)]
    pub tenant_id: Option<TenantId>,
    #[serde(default)]
    pub agent_id: Option<AgentId>,
    #[serde(default)]
    pub new_agent: Option<NewAgent>,
}

/// A request to the token endpoint.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "grant_type")]
pub enum TokenRequest {
    #[serde(rename = "authorization_code")]
    AuthorizationCode {
        code: String,
        code_verifier: String,
        client_id: String,
        redirect_uri: String,
        #[serde(default)]
        resource: Option<String>,
    },
    #[serde(rename = "refresh_token")]
    RefreshToken {
        refresh_token: String,
        client_id: String,
        #[serde(default)]
        resource: Option<String>,
    },
    /// An agent API key for an access token (scripts and the CLI); see
    /// [`crate::agents::AgentStore::exchange_key`].
    #[serde(rename = "urn:ietf:params:oauth:grant-type:token-exchange")]
    TokenExchange {
        subject_token: String,
        subject_token_type: String,
    },
}

/// The answer of the token endpoint.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenResponse {
    #[serde(flatten)]
    pub access: AccessToken,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
}

/// The OAuth error body (RFC 6749 section 5.2, RFC 7591 section 3.2.2) of the token and
/// registration endpoints, with the Ruum error code in `ruum_code`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuthError {
    pub error: String,
    pub error_description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ruum_code: Option<ErrorCode>,
    /// The HTTP status (not in the body).
    #[serde(skip)]
    pub status: u16,
}

impl OAuthError {
    pub fn new(error: &str, description: impl Into<String>, ruum_code: ErrorCode) -> Self {
        let status = match error {
            "invalid_client" => 401,
            "server_error" => 500,
            _ => 400,
        };
        Self {
            error: error.to_string(),
            error_description: description.into(),
            ruum_code: Some(ruum_code),
            status,
        }
    }

    pub fn invalid_grant(description: impl Into<String>) -> Self {
        Self::new("invalid_grant", description, ErrorCode::Unauthenticated)
    }

    pub fn invalid_request(description: impl Into<String>) -> Self {
        Self::new("invalid_request", description, ErrorCode::BadRequest)
    }
}

impl From<Error> for OAuthError {
    fn from(e: Error) -> Self {
        let api = e.to_api();
        let error = match api.code {
            ErrorCode::Unauthenticated
            | ErrorCode::TokenExpired
            | ErrorCode::AgentDisabled
            | ErrorCode::NotFound => "invalid_grant",
            ErrorCode::BadRequest | ErrorCode::ValidationFailed => "invalid_request",
            ErrorCode::RateLimited => "slow_down",
            _ => "server_error",
        };
        let mut o = OAuthError::new(error, api.message, api.code);
        if api.code == ErrorCode::RateLimited {
            o.status = 429;
        }
        o
    }
}

/// Whether a redirect URI is allowed: `https`, or `http` on a loopback address for native
/// clients; no fragment.
pub fn redirect_uri_allowed(uri: &str) -> bool {
    let Ok(u) = url::Url::parse(uri) else {
        return false;
    };
    if u.fragment().is_some() || !u.username().is_empty() || u.password().is_some() {
        return false;
    }
    match u.scheme() {
        "https" => u.host_str().is_some(),
        "http" => matches!(u.host_str(), Some("127.0.0.1" | "[::1]" | "localhost")),
        _ => false,
    }
}

/// Checks a registration (RFC 7591) and fills the defaults.
pub fn check_registration(
    req: ClientRegistration,
) -> std::result::Result<ClientRegistration, OAuthError> {
    let meta = |d: &str| OAuthError::new("invalid_client_metadata", d, ErrorCode::ValidationFailed);
    let name = req.client_name.trim().to_string();
    if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
        return Err(meta("client_name must have 1 to 100 characters"));
    }
    if req.redirect_uris.is_empty() || req.redirect_uris.len() > 10 {
        return Err(OAuthError::new(
            "invalid_redirect_uri",
            "give 1 to 10 redirect URIs",
            ErrorCode::ValidationFailed,
        ));
    }
    if let Some(bad) = req.redirect_uris.iter().find(|u| !redirect_uri_allowed(u)) {
        return Err(OAuthError::new(
            "invalid_redirect_uri",
            format!("{bad}: use https, or http on 127.0.0.1, [::1] or localhost"),
            ErrorCode::ValidationFailed,
        ));
    }
    let grant_types = if req.grant_types.is_empty() {
        vec![
            "authorization_code".to_string(),
            "refresh_token".to_string(),
        ]
    } else {
        req.grant_types.clone()
    };
    if grant_types
        .iter()
        .any(|g| g != "authorization_code" && g != "refresh_token")
        || !grant_types.iter().any(|g| g == "authorization_code")
    {
        return Err(meta(
            "grant_types: only authorization_code and refresh_token",
        ));
    }
    if req.response_types.iter().any(|r| r != "code") {
        return Err(meta("response_types: only code"));
    }
    if req
        .token_endpoint_auth_method
        .as_deref()
        .is_some_and(|m| m != "none")
    {
        return Err(meta(
            "token_endpoint_auth_method: only none (public clients with PKCE)",
        ));
    }
    for (what, v) in [("client_uri", &req.client_uri), ("logo_uri", &req.logo_uri)] {
        if v.as_deref()
            .is_some_and(|u| !u.starts_with("https://") || url::Url::parse(u).is_err())
        {
            return Err(meta(&format!("{what} must be an https URL")));
        }
    }
    Ok(ClientRegistration {
        client_name: name,
        grant_types,
        response_types: vec!["code".to_string()],
        token_endpoint_auth_method: Some("none".to_string()),
        ..req
    })
}

/// A new client ID.
pub fn new_client_id() -> String {
    secret::random_secret(CLIENT_PREFIX)
}

/// The audience of the tokens for a `resource` (RFC 8707): the API origin or its `/mcp` URL.
/// Without `resource`, the API origin.
pub fn resource_audience(resource: Option<&str>, origin: &str) -> Result<String> {
    let origin = origin.trim_end_matches('/');
    match resource.map(|r| r.trim_end_matches('/')) {
        None | Some("") => Ok(origin.to_string()),
        Some(r) if r == origin || r == format!("{origin}/mcp") => Ok(r.to_string()),
        Some(_) => Err(crate::identity::invalid(
            "resource must be the API origin or its /mcp URL",
        )),
    }
}

/// The URL the browser goes to after the consent: `redirect_uri` with the code (or the error)
/// and the state, and the issuer (RFC 9207).
pub fn redirect_with(
    redirect_uri: &str,
    params: &[(&str, &str)],
    state: Option<&str>,
    issuer: &str,
) -> Result<String> {
    let mut u = url::Url::parse(redirect_uri)
        .map_err(|_| crate::identity::invalid("the redirect URI is not valid"))?;
    {
        let mut q = u.query_pairs_mut();
        for (k, v) in params {
            q.append_pair(k, v);
        }
        if let Some(s) = state {
            q.append_pair("state", s);
        }
        q.append_pair("iss", issuer);
    }
    Ok(u.to_string())
}

/// The OAuth authorization server for agents.
#[async_trait]
pub trait OAuthServer: Send + Sync {
    /// Registers a public client. No sign-in needed.
    async fn register_client(
        &self,
        req: ClientRegistration,
    ) -> std::result::Result<OAuthClient, OAuthError>;
    /// The public metadata of a client (`not_found` when unknown). A system lookup.
    async fn get_client(&self, client_id: &str) -> Result<OAuthClient>;
    /// Checks the client and the exact redirect URI, and returns the signed `request` value of
    /// the consent screen.
    async fn start_authorization(&self, req: AuthorizationRequest) -> Result<String>;
    /// Opens a signed `request` value (`validation_failed` when it is not valid or expired).
    fn open_request(&self, request: &str) -> Result<AuthorizationRequest>;
    /// Applies the decision of the signed-in user. Users only (`user_required` for others).
    /// Returns the URL that the browser goes to: the client's redirect URI with a code, or with
    /// `error=access_denied`.
    async fn consent(&self, who: &Actor, consent: Consent) -> Result<String>;
    /// The token endpoint: the `authorization_code` and `refresh_token` grants. The token
    /// exchange of API keys is [`crate::agents::AgentStore::exchange_key`].
    async fn token(&self, req: TokenRequest) -> std::result::Result<TokenResponse, OAuthError>;
    /// Ends all OAuth grants of an agent (disable, owner left).
    async fn end_grants(&self, tenant: TenantId, agent: AgentId) -> Result<u32>;
}

/// The context of `who` in `tenant`, for writes of the consent (agent registration, audit).
pub(crate) fn consent_context(who: &Actor, tenant: TenantId) -> Context {
    Context {
        tenant,
        actor: *who,
        scope: None,
        idempotency_key: None,
    }
}
