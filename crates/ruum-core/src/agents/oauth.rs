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

use crate::actor::Context;
use crate::agents::{AccessToken, NewAgent};
use crate::error::Result;
use crate::ids::{AgentId, TenantId};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// A registration request (RFC 7591).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClientRegistration {
    pub client_name: String,
    pub redirect_uris: Vec<String>,
    #[serde(default)]
    pub grant_types: Vec<String>,
    pub client_uri: Option<String>,
    pub logo_uri: Option<String>,
    pub software_id: Option<String>,
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

/// Which agent a consent binds.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentAgent {
    /// An existing agent of the user.
    Existing(AgentId),
    /// A new agent of the user, with its scope.
    New(NewAgent),
}

/// The decision of the consent screen.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Consent {
    /// The signed `request` value.
    pub request: String,
    pub approve: bool,
    pub tenant_id: Option<TenantId>,
    pub agent: Option<ConsentAgent>,
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
        resource: Option<String>,
    },
    #[serde(rename = "refresh_token")]
    RefreshToken {
        refresh_token: String,
        client_id: String,
        resource: Option<String>,
    },
    /// An agent API key for an access token (scripts and the CLI).
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
    pub refresh_token: Option<String>,
}

/// The OAuth authorization server for agents.
#[async_trait]
pub trait OAuthServer: Send + Sync {
    /// Registers a public client. No sign-in needed.
    async fn register_client(&self, req: ClientRegistration) -> Result<OAuthClient>;
    async fn get_client(&self, client_id: &str) -> Result<OAuthClient>;
    /// Checks the authorize parameters and returns the signed `request` value.
    async fn start_authorization(&self, req: AuthorizationRequest) -> Result<String>;
    /// Applies the user's decision. Users only (`require_user_actor`). Returns the URL that the
    /// browser goes to: the client's redirect URI with a code, or with `error=access_denied`.
    async fn consent(&self, ctx: &Context, consent: Consent) -> Result<String>;
    /// The token endpoint: all three grants. API keys go to
    /// [`crate::agents::AgentStore::exchange_key`].
    async fn token(&self, req: TokenRequest) -> Result<TokenResponse>;
    /// Ends all OAuth grants of an agent (disable, owner left).
    async fn end_grants(&self, tenant: TenantId, agent: AgentId) -> Result<u32>;
}

/// In-memory OAuth server, for unit tests. Not implemented yet.
#[derive(Debug, Default)]
pub struct MemoryOAuthServer {}

#[async_trait]
impl OAuthServer for MemoryOAuthServer {
    async fn register_client(&self, _req: ClientRegistration) -> Result<OAuthClient> {
        todo!("oauth: register_client")
    }
    async fn get_client(&self, _client_id: &str) -> Result<OAuthClient> {
        todo!("oauth: get_client")
    }
    async fn start_authorization(&self, _req: AuthorizationRequest) -> Result<String> {
        todo!("oauth: start_authorization")
    }
    async fn consent(&self, _ctx: &Context, _consent: Consent) -> Result<String> {
        todo!("oauth: consent")
    }
    async fn token(&self, _req: TokenRequest) -> Result<TokenResponse> {
        todo!("oauth: token")
    }
    async fn end_grants(&self, _tenant: TenantId, _agent: AgentId) -> Result<u32> {
        todo!("oauth: end_grants")
    }
}
