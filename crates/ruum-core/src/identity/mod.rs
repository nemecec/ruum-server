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
//!
//! Authentication of a request (the session cookie, a bearer token) is in [`auth`]; it is used by
//! the `RequestContext` extractor of `ruum-api`, and by the MCP and WebDAV faces.

pub mod auth;
pub mod idempotency;
pub mod memory;
pub mod oidc;
pub mod postgres;
pub mod secret;
pub mod setup;
pub mod sweep;
pub mod tokens;

#[cfg(test)]
pub(crate) mod tests;

use crate::actor::{Actor, Context};
use crate::authz::Scope;
use crate::error::{Error, ErrorCode, Result};
use crate::ids::{AgentId, AppPasswordId, ShareLinkId, TenantId, UserId};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

pub use tokens::{JwtTokenIssuer, issue_user_token};

/// The name of the session cookie of the web UI.
pub const SESSION_COOKIE: &str = "ruum_session";
/// The name of the CSRF cookie of the web UI (double submit with the `Ruum-Csrf` header). Its
/// value is [`csrf_token`] of the session, so a cookie set by another host (cookie tossing)
/// does not pass the check (decision 0002-3).
pub const CSRF_COOKIE: &str = "ruum_csrf";
/// The name of the CSRF header.
pub const CSRF_HEADER: &str = "ruum-csrf";
/// The CSRF value of a web session: base64url of SHA-256 over a fixed label and the session
/// secret. Only the server and the browser that holds the session cookie know it, so the check
/// compares the `Ruum-Csrf` header with this value, not only with the cookie (decision 0002-3).
/// The label keeps it apart from the stored session hash (`secret::sha256`).
pub fn csrf_token(session_secret: &str) -> String {
    use base64::Engine as _;
    let digest = secret::sha256(&format!("ruum-csrf\0{session_secret}"));
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// The name of the short-lived cookie that holds the state of a sign-in.
pub const LOGIN_COOKIE: &str = "ruum_login";
/// How long a web session lives without use.
pub const SESSION_DAYS: i64 = 30;
/// How often a session row records its last use, at most.
pub const SESSION_TOUCH_SECONDS: i64 = 300;
/// The visible prefix of WebDAV app passwords (decision 0001-7).
pub const APP_PASSWORD_PREFIX: &str = "ruum_app_";

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

/// A WebDAV app password of a user, for one tenant (decision 0001-7). The secret is shown once.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppPassword {
    pub id: AppPasswordId,
    pub tenant_id: TenantId,
    pub name: String,
    /// The first characters of the password, for example `ruum_app_7hQ2`.
    pub prefix: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_used_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
}

/// The settings of the OIDC client (Zitadel).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OidcSettings {
    pub issuer: String,
    pub client_id: String,
    /// `None` for a public client (PKCE only), as the `ruum-web` app of the laptop stack.
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
    /// The relative path of the web UI to go to after sign-in.
    pub return_to: String,
}

/// The verified claims of an ID token after the callback.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerifiedLogin {
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub name: Option<String>,
    /// The ID token, kept for the end-session request (`id_token_hint`).
    #[serde(default)]
    pub id_token: Option<String>,
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
    /// Exchanges the code and verifies the ID token (signature, issuer, audience, nonce).
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

impl AccessClaims {
    /// New claims of `typ` for `sub`, valid for `seconds` from now, with the issuer and audience
    /// of `issuer`.
    pub fn new(issuer: &dyn TokenIssuer, typ: TokenType, sub: Uuid, seconds: i64) -> Self {
        let now = crate::now().unix_timestamp();
        Self {
            iss: issuer.issuer().to_string(),
            aud: issuer.audience().to_string(),
            typ,
            sub,
            tid: None,
            obo: None,
            agent: None,
            share_link: None,
            scope: None,
            iat: now,
            exp: now + seconds,
            jti: Uuid::now_v7(),
        }
    }
}

/// The result of an authenticated request: who it is.
#[derive(Clone, Debug)]
pub struct Authenticated {
    pub actor: Actor,
    /// The tenant the credentials are bound to (agent and share-link tokens, app passwords).
    /// `None` for user sessions and user tokens, which choose the tenant per request.
    pub bound_tenant: Option<TenantId>,
    /// The scope of the credentials (agents, share links), as in [`crate::actor::Context`].
    pub scope: Option<Scope>,
    /// The web session of the request, when it came with the session cookie.
    pub session_id: Option<Uuid>,
}

impl Authenticated {
    /// The signed-in user (the actor, or the user an agent acts for). `None` for a share link.
    pub fn user(&self) -> Option<UserId> {
        self.actor.effective_user()
    }
}

/// Signs and verifies Ruum access tokens (asymmetric keys, published as JWKS). The same keys
/// sign short-lived opaque values of the server (the sign-in state, the OAuth consent request),
/// with an audience of their own, so they never pass as access tokens.
pub trait TokenIssuer: Send + Sync {
    /// The `iss` of the tokens.
    fn issuer(&self) -> &str;
    /// The `aud` of access tokens for the API: the API origin.
    fn audience(&self) -> &str;
    fn issue(&self, claims: &AccessClaims) -> Result<String>;
    /// Checks the signature, the issuer and the expiry. The audience must be the API origin or
    /// its `/mcp` URL; the caller compares it with what it needs.
    fn verify(&self, token: &str) -> Result<AccessClaims>;
    /// The public keys as a JWKS document.
    fn jwks(&self) -> serde_json::Value;
    /// Signs a JSON value for `purpose` (for example `oauth_request`), valid for `seconds`.
    fn seal(&self, purpose: &str, value: &serde_json::Value, seconds: i64) -> Result<String>;
    /// Checks a value of [`TokenIssuer::seal`] for `purpose` and returns it.
    fn open(&self, purpose: &str, sealed: &str) -> Result<serde_json::Value>;
}

/// Users, identities, sessions and app passwords. Users, identities and sessions are global
/// (decision 0001-3); their `tenant_id` is the user's home tenant.
#[async_trait]
pub trait IdentityStore: Send + Sync {
    /// Finds the user of a login, or makes a new user with a personal tenant. A login whose
    /// identity is unknown links to an existing user only when its email is verified and equal
    /// to the verified email of that user; otherwise it makes a new user. An unverified email
    /// that a verified user already has is kept on the new user, but not as verified.
    async fn upsert_from_login(&self, login: &VerifiedLogin) -> Result<User>;
    async fn get_user(&self, id: UserId) -> Result<User>;
    /// The user with this verified email, if any.
    async fn find_verified_email(&self, email: &str) -> Result<Option<User>>;
    async fn identities_of(&self, user: UserId) -> Result<Vec<Identity>>;
    /// Starts a web session. Returns it and the secret for the cookie (stored as SHA-256).
    async fn create_session(
        &self,
        user: UserId,
        user_agent: Option<String>,
    ) -> Result<(Session, String)>;
    /// Finds a live session by the secret from the cookie, and marks it as seen.
    async fn session_by_secret(&self, secret: &str) -> Result<Session>;
    /// The live sessions of a user, newest first.
    async fn sessions_of(&self, user: UserId) -> Result<Vec<Session>>;
    /// Ends a session of `user`. A session of another user is `not_found`.
    async fn end_session(&self, user: UserId, id: Uuid) -> Result<()>;
    /// Binds the email grants of the user's verified email in every tenant (decision 0003-2
    /// (b)), for the live web session `session` of the user: only an interactive user session
    /// binds, never an agent token, app password, share-link token or bearer token, so the
    /// caller passes the session of the request (the cookie) or of a new sign-in. Without
    /// `force` it checks at most once per session per [`BIND_CHECK_SECONDS`] (a timestamp on
    /// the session row, shared by every server process); `force` (a sign-in, or the item page
    /// of a share email) skips that throttle. An unverified email binds nothing. A bind makes
    /// the user a guest of a tenant only while the tenant allows guests. Returns the number of
    /// email grants bound. The default binds nothing.
    async fn bind_email_grants(&self, user: UserId, session: Uuid, force: bool) -> Result<usize> {
        let _ = (user, session, force);
        Ok(0)
    }

    /// The app passwords of the caller in `ctx.tenant`, also revoked and expired ones, newest
    /// first. Users only.
    async fn list_app_passwords(&self, ctx: &Context) -> Result<Vec<AppPassword>>;
    /// Makes an app password for the caller in `ctx.tenant` (a member or guest there). Returns
    /// it and the secret. Users only.
    async fn create_app_password(
        &self,
        ctx: &Context,
        name: String,
        expires_at: Option<OffsetDateTime>,
    ) -> Result<(AppPassword, String)>;
    /// Revokes an app password of the caller. Another user's password is `not_found`.
    async fn revoke_app_password(&self, ctx: &Context, id: AppPasswordId) -> Result<()>;
    /// The WebDAV sign-in (HTTP Basic): the user's email and an app password. A system lookup by
    /// the hash; then it checks the email, the expiry and that the user is still a member or
    /// guest of the tenant. The result is the user (actor = the user) bound to that tenant.
    async fn authenticate_app_password(&self, email: &str, secret: &str) -> Result<Authenticated>;
}

/// A web session checks for email grants to bind at most once in this many seconds
/// ([`IdentityStore::bind_email_grants`] without `force`).
pub const BIND_CHECK_SECONDS: i64 = 60;

/// The `unauthenticated` error with a hint.
pub fn unauthenticated(message: &str, hint: &str) -> Error {
    Error::Api(crate::error::ApiError::new(ErrorCode::Unauthenticated, message).with_hint(hint))
}

/// An audit entry of this area with a target. The one place where this area builds a
/// [`crate::audit::NewAuditEntry`], so a new field of the entry (for example the client IP and
/// user agent of the journal area) is set here only.
pub(crate) fn audit_entry(
    action: &str,
    target_type: &str,
    target_id: impl ToString,
    data: serde_json::Value,
) -> crate::audit::NewAuditEntry {
    crate::audit::NewAuditEntry {
        action: action.to_string(),
        target_type: Some(target_type.to_string()),
        target_id: Some(target_id.to_string()),
        ip: None,
        user_agent: None,
        data,
    }
}

/// The `validation_failed` error.
pub fn invalid(message: impl Into<String>) -> Error {
    Error::api(ErrorCode::ValidationFailed, message)
}

/// Checks a display name, tenant name or agent name: 1 to `max` characters after trimming.
pub fn check_name(what: &str, name: &str, max: usize) -> Result<String> {
    let name = name.trim();
    let n = name.chars().count();
    if n == 0 || n > max {
        return Err(invalid(format!("{what} must have 1 to {max} characters")));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(invalid(format!("{what} must not hold control characters")));
    }
    Ok(name.to_string())
}

/// A light check of an email address: one `@`, a dot in the domain, no spaces. The address is
/// stored as given (trimmed); comparisons ignore case.
pub fn check_email(email: &str) -> Result<String> {
    let e = email.trim();
    let ok = e.len() <= 320
        && !e.chars().any(char::is_whitespace)
        && e.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && domain.contains('.') && !domain.contains('@')
        });
    if ok {
        Ok(e.to_string())
    } else {
        Err(invalid("not a valid email address"))
    }
}
