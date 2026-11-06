//! Identity (`docs/DESIGN.md`, section 4.2): users, linked identities, sessions and sign-in
//! through OIDC. The identity provider is Zitadel; Ruum does not store passwords.
//!
//! Sign-in for the web UI is the OIDC authorization code flow with PKCE, run by the server
//! (`/v1/auth/login` and `/v1/auth/callback`). The server keeps a session and sets an HttpOnly
//! cookie. Native clients and the CLI send a bearer access token. Agents exchange an API key for a
//! short-lived access token that this module signs ([`TokenIssuer`]).
//!
//! Identities are linked only by an explicit action of the signed-in user, or by a verified email
//! from a trusted provider. Never by email alone.
//!
//! Email rules:
//! - `users.email_verified` is true only when a trusted provider verified the email. Only verified
//!   emails are unique among users, so an unverified sign-in never takes an address from its real
//!   owner, and the code never needs to auto-link by email.
//! - An email grant or invitation binds to a user only through its secret invitation token, or
//!   when the user signs in with that email as a verified email of a trusted provider.

pub mod memory;

use crate::actor::Actor;
use crate::authz::Scope;
use crate::error::Result;
use crate::ids::{AgentId, ShareLinkId, TenantId, UserId};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// The name of the session cookie of the web UI.
pub const SESSION_COOKIE: &str = "ruum_session";

/// A person.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct User {
    pub id: UserId,
    /// The personal tenant of the user, made at first sign-in.
    pub home_tenant_id: TenantId,
    pub email: String,
    /// True when a trusted provider verified the email.
    pub email_verified: bool,
    pub display_name: String,
    pub avatar_url: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// One linked identity of a user at an identity provider.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Identity {
    pub id: Uuid,
    pub user_id: UserId,
    /// The OIDC issuer URL.
    pub issuer: String,
    /// The `sub` claim at the issuer.
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub linked_at: OffsetDateTime,
}

/// A web session.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    pub user_id: UserId,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub last_seen_at: OffsetDateTime,
    pub user_agent: Option<String>,
}

/// The settings of the OIDC client (Zitadel).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OidcSettings {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
}

/// The start of a login: where to send the browser, and the state to keep until the callback.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoginStart {
    pub authorize_url: String,
    pub state: String,
    pub nonce: String,
    pub pkce_verifier: String,
}

/// The verified claims of an ID token after the callback.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerifiedLogin {
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub name: Option<String>,
}

/// The OIDC client.
#[async_trait]
pub trait OidcClient: Send + Sync {
    /// Builds the authorization URL. `idp_hint` selects Google, Microsoft or passkeys.
    async fn start_login(
        &self,
        return_to: Option<String>,
        idp_hint: Option<String>,
    ) -> Result<LoginStart>;
    /// Exchanges the code and verifies the ID token.
    async fn finish_login(&self, code: &str, start: &LoginStart) -> Result<VerifiedLogin>;
    /// The end-session URL at the identity provider.
    async fn logout_url(&self, id_token_hint: Option<String>) -> Result<String>;
}

/// The type of a Ruum access token (claim `typ`). The API accepts only these three; tokens for
/// providers (step 2) and apps (step 3) get their own types and audiences.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenType {
    Agent,
    User,
    ShareLink,
}

/// The claims of a Ruum access token (agents, users of native clients, share-link bearers). The
/// claim set is documented in `bearerAuth` of `contracts/openapi.yaml`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccessClaims {
    pub iss: String,
    /// The audience: the API origin, for example `https://api.ruum.space`. The API refuses a
    /// token whose `aud` is not its own origin.
    pub aud: String,
    /// The token type. The API refuses other types.
    pub typ: TokenType,
    /// The actor: an agent ID, a user ID or a share link ID.
    pub sub: Uuid,
    /// The tenant the token is bound to. Always set for agent and share-link tokens.
    pub tid: Option<TenantId>,
    /// The user for whom the actor acts (agents).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obo: Option<UserId>,
    /// The agent, when `typ` is `agent`. Its scope is read from the agent row on each request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentId>,
    /// The share link, when `typ` is `share_link`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_link: Option<ShareLinkId>,
    /// The scope of a share-link token: the link's node, role and expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
    pub iat: i64,
    pub exp: i64,
    pub jti: Uuid,
}

/// The result of an authenticated request: who it is.
#[derive(Clone, Debug)]
pub struct Authenticated {
    pub actor: Actor,
    /// The tenant the credentials are bound to (agent and share-link tokens). `None` for user
    /// sessions, which choose the tenant per request.
    pub bound_tenant: Option<TenantId>,
    /// The scope of the credentials (agents, share links), as in [`crate::actor::Context`].
    pub scope: Option<Scope>,
}

/// Signs and verifies Ruum access tokens (asymmetric keys, published as JWKS).
pub trait TokenIssuer: Send + Sync {
    fn issue(&self, claims: &AccessClaims) -> Result<String>;
    fn verify(&self, token: &str) -> Result<AccessClaims>;
    /// The public keys as a JWKS document.
    fn jwks(&self) -> serde_json::Value;
}

/// Users, identities and sessions.
#[async_trait]
pub trait IdentityStore: Send + Sync {
    /// Finds the user of a login, or makes a new user with a personal tenant.
    async fn upsert_from_login(&self, login: &VerifiedLogin) -> Result<User>;
    async fn get_user(&self, id: UserId) -> Result<User>;
    async fn identities_of(&self, user: UserId) -> Result<Vec<Identity>>;
    async fn create_session(
        &self,
        user: UserId,
        user_agent: Option<String>,
    ) -> Result<(Session, String)>;
    /// Finds a live session by the secret from the cookie.
    async fn session_by_secret(&self, secret: &str) -> Result<Session>;
    async fn end_session(&self, id: Uuid) -> Result<()>;
    /// Checks a bearer token (Ruum access token or identity provider token).
    async fn authenticate_bearer(&self, token: &str) -> Result<Authenticated>;
}
