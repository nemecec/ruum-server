//! The black-box harness of the app gateway and the runtime API (card T-6, `docs/STEP3.md`;
//! design 18.2: tests before code).
//!
//! A case talks HTTP to the listener of `ruum-gateway` (or `ruum-runtime-api`), never to its
//! internals. A **world** starts the binary with an in-memory core and seeds what a case needs
//! (a tenant, an app, a deployment with static files, a grant, a sign-in). The worlds live with
//! the binaries: card G-1a implements [`GatewayWorld`] in `crates/ruum-gateway/tests/`, cards
//! R-3 and G-3 implement [`RuntimeApiWorld`]. Until then the test binaries of this crate
//! (`tests/gateway.rs`, `tests/runtime_api.rs`) run every case without a world, and every case
//! reports `planned`.
//!
//! # Outcomes
//!
//! - **planned**: no world is given, or `make()` or [`GatewayWorld::start`] answers
//!   [`WorldError::Missing`] (the binary is not there yet). Never a failure, also not with
//!   `RUUM_REQUIRE_IMPLEMENTED=1`. A `Missing` from any later step **fails** the case: the world
//!   is written by the implementer, so it must not be able to switch a case off.
//! - **pending**: the case waits for a named card. Either [`PENDING`] lists it (a rule that a
//!   later card builds), or the world answers [`WorldError::Pending`] from a step that
//!   [`WORLD_PENDING_STEPS`] allows (a partner key before B-3) in a case that
//!   [`WORLD_PENDING`] lists with that card. Any other world `Pending` fails the case. Never a
//!   failure otherwise.
//! - **passed** or **failed** otherwise.
//!
//! A case never passes on silence (decision 0006 G, the S-2 rule): a refusal must be the
//! gateway's own answer (a status, and the page or the error body where the contract says),
//! never a connection error or a timeout, and never an answer of the upstream.
//!
//! # Secrets
//!
//! Tokens, cookies and keys never appear in a failure message: the runner masks every value
//! the case got from the world (sign-ins, agent tokens, partner keys) and everything that looks
//! like a JWT, a bearer value or a Ruum cookie value ([`scrub`]).

pub mod cases_access;
pub mod cases_headers;
pub mod cases_later;
pub mod cases_runtime_api;
pub mod cases_session;
pub mod cases_static;
pub mod cases_tenants;
pub mod cases_tokens;
pub mod echo;
pub mod http;
pub mod token;

use crate::report::{REQUIRE_IMPLEMENTED_ENV, require_implemented};
use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::stream::{self, StreamExt};
use serde_json::Value;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use echo::Echo;
pub use http::{Jar, Req, Resp, SetCookie};

/// The text of the gateway contract.
pub const GATEWAY_CONTRACT: &str = include_str!("../../../../contracts/apps/gateway.openapi.yaml");
/// The text of the runtime API contract.
pub const RUNTIME_CONTRACT: &str = include_str!("../../../../contracts/apps/runtime.openapi.yaml");

/// The cards a pending case may wait for.
pub const CARDS: &[&str] = &[
    "G-1a", "G-1b", "G-2", "G-3", "G-4", "R-3", "B-2", "B-3", "W-5", "X-2",
];

/// The cases that wait for a card other than G-1a, with that card and the reason. They do not
/// run, and they never fail the suite (also not with `RUUM_REQUIRE_IMPLEMENTED=1`). The card
/// named removes the line when its code makes the case pass. Without a world a case reports
/// planned first.
pub const PENDING: &[(&str, &str)] = &[
    // Proxy to a sandbox, WebSockets and cold start: G-1b.
    (
        "proxy_reaches_a_running_sandbox",
        "G-1b: the proxy to a sandbox",
    ),
    ("websockets_pass_after_the_check", "G-1b: WebSockets"),
    ("a_cold_start_is_held_and_answered", "G-1b: cold start"),
    // The browser script and telemetry: G-2.
    ("the_browser_script_is_served", "G-2: the browser script"),
    (
        "telemetry_of_a_platform_app_needs_a_session",
        "G-2: frontend telemetry",
    ),
    (
        "telemetry_ignores_identity_in_the_body",
        "G-2: frontend telemetry",
    ),
    // The report origin: G-4.
    (
        "the_handoff_needs_a_top_level_navigation",
        "G-4: the report origin",
    ),
    (
        "an_app_script_cannot_read_the_report_preview",
        "G-4: the report origin",
    ),
    (
        "report_choices_are_refused_from_the_app_origin",
        "G-4: the report origin",
    ),
    (
        "a_report_names_only_its_own_visit",
        "G-4: the report origin",
    ),
    (
        "a_handoff_code_works_once_within_60_seconds",
        "G-4: the report origin",
    ),
    // Receipts of reports: X-2.
    ("a_receipt_token_is_shown_once", "X-2: receipts"),
    // Partner keys: B-3.
    ("a_partner_key_reaches_only_its_api", "B-3: partner keys"),
    ("a_partner_key_never_reaches_the_app", "B-3: partner keys"),
    // Custom domains: W-5.
    (
        "a_custom_domain_serves_only_when_verified",
        "W-5: custom domains",
    ),
    // The runtime API: R-3, the internal gateway: G-3.
    (
        "get_app_self_needs_a_workload_token",
        "R-3: the runtime API",
    ),
    (
        "list_app_self_users_names_role_holders",
        "R-3: the runtime API",
    ),
    (
        "exchange_refuses_a_preview_token_for_the_live_app",
        "R-3: the runtime API",
    ),
    (
        "exchange_refuses_a_token_of_another_app",
        "R-3: the runtime API",
    ),
    (
        "exchange_refuses_an_actor_without_delegate",
        "R-3: the runtime API",
    ),
    ("exchange_gives_a_delegated_token", "R-3: the runtime API"),
    ("bound_content_only", "R-3: the runtime API"),
    ("bound_children_only", "R-3: the runtime API"),
    ("runtime_calls_never_cross_tenants", "R-3: the runtime API"),
    ("publish_app_event_needs_the_topic", "R-3: the runtime API"),
    (
        "app_tokens_never_work_at_the_rest_api",
        "R-3: the runtime API",
    ),
    (
        "the_internal_gateway_removes_the_workload_token",
        "G-3: the internal gateway",
    ),
    (
        "a_chain_longer_than_three_hops_is_refused",
        "G-3: the internal gateway (B-3 delegate)",
    ),
];

/// The reason of a case in [`PENDING`].
pub fn pending_reason(case: &str) -> Option<&'static str> {
    PENDING.iter().find(|(c, _)| *c == case).map(|(_, r)| *r)
}

/// The cases in which a world may answer [`WorldError::Pending`], each with the one card it may
/// name. A world `Pending` in any other case, or with another card, fails the case. Empty: the
/// three app role cases waited for B-2, which is merged (backlog P-68), so a world must grant
/// and remove app roles now.
pub const WORLD_PENDING: &[(&str, &str)] = &[];

/// The world steps that may answer [`WorldError::Pending`], each with the one card it may name:
/// a partner key (B-3). Every other step that answers `Pending` fails the case (see [`Guard`]);
/// granting or removing an app role no longer may (B-2 is merged).
pub const WORLD_PENDING_STEPS: &[(&str, &str)] = &[("partner_key", "B-3")];

/// Whether a world may answer pending with `card` in `case`.
pub fn world_pending_allowed(case: &str, card: &str) -> bool {
    WORLD_PENDING.iter().any(|(c, k)| *c == case && *k == card)
}

// ---------------------------------------------------------------------------------------------
// The seeded world
// ---------------------------------------------------------------------------------------------

/// Why a world step did not happen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorldError {
    /// The binary under test is not there yet: the case reports planned.
    Missing(String),
    /// The world cannot do this step until the named card lands (for example a partner key
    /// before B-3): the case reports pending with that card.
    Pending { card: String, why: String },
    /// The step failed: the case fails.
    Failed(String),
}

impl std::fmt::Display for WorldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorldError::Missing(m) => write!(f, "missing: {m}"),
            WorldError::Pending { card, why } => write!(f, "pending {card}: {why}"),
            WorldError::Failed(m) => write!(f, "failed: {m}"),
        }
    }
}

pub type WorldResult<T> = Result<T, WorldError>;

/// The access setting of an app (`updateAppSettings`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Platform,
    Public,
}

/// The environment of a deployment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Env {
    Live,
    Preview,
}

impl Env {
    /// The value of the `env` claim.
    pub fn claim(self) -> &'static str {
        match self {
            Env::Live => "live",
            Env::Preview => "preview",
        }
    }
}

/// A right on an app node that a case grants or removes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Relation {
    /// `view` (gives `use`).
    View,
    /// `edit` (gives `open_preview`).
    Edit,
    /// An app role of the manifest (gives `use` while the role is active; B-2).
    Role(String),
}

/// The audience of an agent token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentAudience {
    /// `ruumapps`: accepted on preview hosts only (decision 0006-10).
    Ruumapps,
    /// The REST API's audience: never accepted by the gateway.
    Api,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tenant {
    pub id: String,
    pub slug: String,
}

/// A person of a tenant (a member with a platform account).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    pub id: String,
    pub name: String,
    pub tenant: Tenant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct App {
    /// The app node ID (a UUID).
    pub id: String,
    pub slug: String,
    pub tenant: Tenant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deployment {
    /// The deployment ID (a UUID).
    pub id: String,
    /// 8 characters of lower-case base32 (the preview host label).
    pub short_id: String,
    pub env: Env,
    pub app_id: String,
}

/// A credential. Its `Debug` never shows the value.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(pub String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// The answer of a sign-in on an app origin: the raw `Set-Cookie` headers that the gateway's
/// `/.ruum/v1/auth/start` and `/.ruum/v1/auth/callback` sent.
#[derive(Clone)]
pub struct SignIn {
    pub set_cookies: Vec<String>,
}

impl std::fmt::Debug for SignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SignIn({} cookies, <redacted>)", self.set_cookies.len())
    }
}

/// What a started gateway gives the cases.
#[derive(Clone, Debug)]
pub struct GatewayHandle {
    /// The gateway's listener. Every host name resolves to it.
    pub addr: SocketAddr,
    /// The apps domain (`ruumapps.localhost` on the laptop).
    pub apps_domain: String,
    /// The `iss` of the user tokens: the instance's origin (`https://ruum.localhost`).
    pub issuer: String,
    /// The public key set of the user tokens (JWKS JSON, EdDSA keys).
    pub user_token_jwks: Value,
    /// The Ruum UI origin (the `frame-ancestors` of previews).
    pub ui_origin: String,
    /// The host of the platform origin (the report origin: paths under `/.ruum-report/v1/`,
    /// which Caddy routes to the gateway).
    pub platform_host: String,
}

/// The seeded world of the gateway cases. Card G-1a implements it in `crates/ruum-gateway/tests/`
/// with the in-memory core. A method is about the seeded world (the core's state), never about
/// the gateway's internals; the cases talk to the gateway only over HTTP. A step the world cannot
/// do yet answers [`WorldError::Pending`] with the card that brings it.
#[async_trait]
pub trait GatewayWorld: Send + Sync {
    /// Starts the gateway (in process or the binary) with an in-memory core.
    /// [`WorldError::Missing`] when the binary is not there.
    async fn start(&self) -> WorldResult<GatewayHandle>;
    async fn seed_tenant(&self, slug: &str) -> WorldResult<Tenant>;
    /// A person of `tenant`, with a platform account (no right on any app yet).
    async fn seed_user(&self, tenant: &Tenant, name: &str) -> WorldResult<User>;
    async fn seed_app(&self, tenant: &Tenant, slug: &str, access: Access) -> WorldResult<App>;
    /// A `ready` deployment with the static files `files` (`(path, bytes)`, paths relative to the
    /// site root, `/` separators). A preview deployment is reachable at once on its preview host;
    /// a live one after [`GatewayWorld::set_live`]. The world may seed paths that the archive
    /// rules would refuse (`.ruum/x`), straight into the site index.
    async fn seed_deployment(
        &self,
        app: &App,
        env: Env,
        files: &[(String, Vec<u8>)],
        spa_fallback: Option<&str>,
    ) -> WorldResult<Deployment>;
    async fn set_live(&self, app: &App, deployment: &Deployment) -> WorldResult<()>;
    async fn grant(&self, user: &User, app: &App, relation: &Relation) -> WorldResult<()>;
    async fn revoke(&self, user: &User, app: &App, relation: &Relation) -> WorldResult<()>;
    /// Makes an existing person a member of another tenant too (one platform account in two
    /// tenants), without any right on its apps. For the cases across tenants.
    async fn seed_member(&self, user: &User, tenant: &Tenant) -> WorldResult<()>;
    /// The platform side of the sign-in code flow, and only that. `login_location` is the
    /// `Location` of the gateway's answer to `/.ruum/v1/auth/start` (the platform's `loginStart`
    /// with the app audience). The world signs `user` in to the platform (a test session may be
    /// used), follows `login_location` as that user and returns the platform's redirect: the
    /// absolute URL `https://<app host>/.ruum/v1/auth/callback?code=...&state=...`. The cases
    /// send the start and the callback to the gateway over HTTP themselves ([`Ctx::sign_in`]).
    /// The platform issues the code also for a user without any right on the app, or who is not
    /// a member of the app's tenant: the gateway refuses such a visitor afterwards.
    async fn platform_authorize(&self, user: &User, login_location: &str) -> WorldResult<String>;
    /// Ends the user's platform session (sign out of Ruum).
    async fn sign_out_platform(&self, user: &User) -> WorldResult<()>;
    /// A token of an agent of `user` (scope: the whole tenant) with `audience`.
    async fn agent_token(&self, user: &User, audience: AgentAudience) -> WorldResult<Secret>;
    /// A partner key of `app` for its external API `api` (B-3).
    async fn partner_key(&self, app: &App, api: &str) -> WorldResult<Secret>;
    /// Sends the requests of `deployment` to `url` (the echo app) instead of its static files:
    /// the fixed test upstream of the cleaning cases, before G-1b exists.
    async fn set_upstream(&self, deployment: &Deployment, url: &str) -> WorldResult<()>;
    /// The tenant's public approval of public apps (backlog S-33).
    async fn set_public_approval(&self, tenant: &Tenant, approved: bool) -> WorldResult<()>;
    /// Removes `removed` and gives its host name to `new` (same tenant), past the retired-slug
    /// rule: a test-only step, so a case can check that a session is bound to the app and not to
    /// the host. Returns `new` with its new slug.
    async fn reuse_host(&self, removed: &App, new: &App) -> WorldResult<App>;
    /// The status of an authenticated read of the REST API (`GET /v1/me`) with
    /// `Authorization: Bearer <bearer>` (decision 0006-10: app tokens never work there).
    async fn rest_api_status(&self, bearer: &str) -> WorldResult<u16>;
}

/// Makes a fresh world for one case.
pub type MakeGatewayWorld =
    Arc<dyn Fn() -> BoxFuture<'static, WorldResult<Arc<dyn GatewayWorld>>> + Send + Sync>;

/// The world as the cases see it: every step of the implementer's world after `start()`, with
/// the outcome rules applied. A [`WorldError::Missing`] becomes a failure. A
/// [`WorldError::Pending`] passes only from a step of [`WORLD_PENDING_STEPS`] with its card
/// (the runner then also checks [`WORLD_PENDING`]); any other becomes a failure.
pub struct Guard(pub Arc<dyn GatewayWorld>);

fn guarded<T>(step: &str, r: WorldResult<T>) -> WorldResult<T> {
    match r {
        Err(WorldError::Missing(m)) => Err(WorldError::Failed(format!(
            "{step} answered Missing after start ({m}); Missing counts only from make() and \
             start(), never to switch a case off"
        ))),
        Err(WorldError::Pending { card, why })
            if !WORLD_PENDING_STEPS
                .iter()
                .any(|(s, c)| *s == step && *c == card) =>
        {
            Err(WorldError::Failed(format!(
                "{step} answered pending {card} ({why}); a world may answer pending only from \
                 the steps of WORLD_PENDING_STEPS {WORLD_PENDING_STEPS:?}"
            )))
        }
        r => r,
    }
}

fn relation_step(verb: &str, r: &Relation) -> String {
    match r {
        Relation::Role(_) => format!("{verb}(role)"),
        Relation::View => format!("{verb}(view)"),
        Relation::Edit => format!("{verb}(edit)"),
    }
}

#[async_trait]
impl GatewayWorld for Guard {
    async fn start(&self) -> WorldResult<GatewayHandle> {
        guarded("start (again)", self.0.start().await)
    }
    async fn seed_tenant(&self, slug: &str) -> WorldResult<Tenant> {
        guarded("seed_tenant", self.0.seed_tenant(slug).await)
    }
    async fn seed_user(&self, tenant: &Tenant, name: &str) -> WorldResult<User> {
        guarded("seed_user", self.0.seed_user(tenant, name).await)
    }
    async fn seed_member(&self, user: &User, tenant: &Tenant) -> WorldResult<()> {
        guarded("seed_member", self.0.seed_member(user, tenant).await)
    }
    async fn seed_app(&self, tenant: &Tenant, slug: &str, access: Access) -> WorldResult<App> {
        guarded("seed_app", self.0.seed_app(tenant, slug, access).await)
    }
    async fn seed_deployment(
        &self,
        app: &App,
        env: Env,
        files: &[(String, Vec<u8>)],
        spa_fallback: Option<&str>,
    ) -> WorldResult<Deployment> {
        guarded(
            "seed_deployment",
            self.0.seed_deployment(app, env, files, spa_fallback).await,
        )
    }
    async fn set_live(&self, app: &App, deployment: &Deployment) -> WorldResult<()> {
        guarded("set_live", self.0.set_live(app, deployment).await)
    }
    async fn grant(&self, user: &User, app: &App, relation: &Relation) -> WorldResult<()> {
        guarded(
            &relation_step("grant", relation),
            self.0.grant(user, app, relation).await,
        )
    }
    async fn revoke(&self, user: &User, app: &App, relation: &Relation) -> WorldResult<()> {
        guarded(
            &relation_step("revoke", relation),
            self.0.revoke(user, app, relation).await,
        )
    }
    async fn platform_authorize(&self, user: &User, login_location: &str) -> WorldResult<String> {
        guarded(
            "platform_authorize",
            self.0.platform_authorize(user, login_location).await,
        )
    }
    async fn sign_out_platform(&self, user: &User) -> WorldResult<()> {
        guarded("sign_out_platform", self.0.sign_out_platform(user).await)
    }
    async fn agent_token(&self, user: &User, audience: AgentAudience) -> WorldResult<Secret> {
        guarded("agent_token", self.0.agent_token(user, audience).await)
    }
    async fn partner_key(&self, app: &App, api: &str) -> WorldResult<Secret> {
        guarded("partner_key", self.0.partner_key(app, api).await)
    }
    async fn set_upstream(&self, deployment: &Deployment, url: &str) -> WorldResult<()> {
        guarded("set_upstream", self.0.set_upstream(deployment, url).await)
    }
    async fn set_public_approval(&self, tenant: &Tenant, approved: bool) -> WorldResult<()> {
        guarded(
            "set_public_approval",
            self.0.set_public_approval(tenant, approved).await,
        )
    }
    async fn reuse_host(&self, removed: &App, new: &App) -> WorldResult<App> {
        guarded("reuse_host", self.0.reuse_host(removed, new).await)
    }
    async fn rest_api_status(&self, bearer: &str) -> WorldResult<u16> {
        guarded("rest_api_status", self.0.rest_api_status(bearer).await)
    }
}

/// A binding of a running app (runtime API cases).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub name: String,
    pub kind: BindingKind,
    pub on_behalf_of_user: bool,
    pub delegate: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingKind {
    /// A file node (`view` in step 3); the world seeds it with the bytes.
    File(Vec<u8>),
    /// A folder node with files of these names.
    Folder(Vec<String>),
    /// An API of another app (app to app through the internal gateway). `tenant_slug` is `None`
    /// for an app of the same tenant; `Some` names another tenant (a binding the core must
    /// refuse: the world answers `WorldError::Failed` with a message that starts with
    /// `refused`, or seeds it and the call is refused).
    App {
        app_slug: String,
        api: String,
        tenant_slug: Option<String>,
    },
}

/// A running app in the runtime API world.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RtApp {
    pub id: String,
    pub slug: String,
    pub tenant_slug: String,
    /// The preview deployment of the app (each seeded app has one live and one preview).
    pub preview_id: String,
    /// The node IDs of the bindings, by binding name.
    pub bound_nodes: Vec<(String, String)>,
}

/// What a started runtime API gives the cases.
#[derive(Clone, Debug)]
pub struct RuntimeHandle {
    /// The listener of the runtime API and the internal gateway.
    pub addr: SocketAddr,
    /// `ruum.internal`.
    pub internal_host: String,
    pub issuer: String,
    /// The public key set of the delegated tokens.
    pub delegated_jwks: Value,
    /// The public key set of the `Ruum-User` tokens that the internal gateway adds to an
    /// app-to-app call.
    pub user_token_jwks: Value,
}

/// The seeded world of the runtime API cases (cards R-3 and G-3).
#[async_trait]
pub trait RuntimeApiWorld: Send + Sync {
    async fn start(&self) -> WorldResult<RuntimeHandle>;
    /// An app of `tenant_slug` with a live and a preview deployment and these bindings.
    async fn seed_app(
        &self,
        tenant_slug: &str,
        app_slug: &str,
        bindings: &[Binding],
    ) -> WorldResult<RtApp>;
    /// The workload token that the runtime mounts for the app's live or preview deployment.
    async fn workload_token(&self, app: &RtApp, env: Env) -> WorldResult<Secret>;
    /// A user token as the gateway adds it for a signed-in person with `view` (and `edit`) on
    /// the app, for its live or preview deployment, with `actor`.
    async fn user_token(&self, app: &RtApp, env: Env, actor: &str) -> WorldResult<Secret>;
    /// Sends the app's requests (app to app) to `url` (the echo app).
    async fn set_upstream(&self, app: &RtApp, url: &str) -> WorldResult<()>;
    /// A node of the tenant that no binding of any app names.
    async fn seed_unbound_node(&self, tenant_slug: &str) -> WorldResult<String>;
    /// The status of `GET /v1/me` at the REST API with this bearer.
    async fn rest_api_status(&self, bearer: &str) -> WorldResult<u16>;
}

pub type MakeRuntimeWorld =
    Arc<dyn Fn() -> BoxFuture<'static, WorldResult<Arc<dyn RuntimeApiWorld>>> + Send + Sync>;

// ---------------------------------------------------------------------------------------------
// Cases and outcomes
// ---------------------------------------------------------------------------------------------

/// Why a case stopped.
#[derive(Debug)]
pub enum Fail {
    Failed(String),
    /// A world step answered [`WorldError::Missing`]. Inside a case this fails it (the runner
    /// turns it into a failure): planned comes only from `make()` and `start()`.
    Planned(String),
    /// A world step answered [`WorldError::Pending`]. The runner accepts it only when
    /// [`WORLD_PENDING`] lists the case with this card; otherwise the case fails.
    Pending {
        card: String,
        why: String,
    },
}

impl From<WorldError> for Fail {
    fn from(e: WorldError) -> Self {
        match e {
            WorldError::Missing(m) => Fail::Planned(m),
            WorldError::Pending { card, why } => Fail::Pending { card, why },
            WorldError::Failed(m) => Fail::Failed(format!("world: {m}")),
        }
    }
}

impl From<String> for Fail {
    fn from(s: String) -> Self {
        Fail::Failed(s)
    }
}

pub type CaseResult = Result<(), Fail>;

/// Fails the case with a formatted message.
#[macro_export]
macro_rules! gw_fail {
    ($($t:tt)*) => {
        return Err($crate::gateway::Fail::Failed(format!($($t)*)))
    };
}

/// Fails the case when the condition is false.
#[macro_export]
macro_rules! gw_check {
    ($cond:expr, $($t:tt)*) => {
        if !$cond {
            return Err($crate::gateway::Fail::Failed(format!($($t)*)));
        }
    };
}

/// One case. `C` is the context: [`Ctx`] for the gateway, [`RtCtx`] for the runtime API.
pub struct Case<C> {
    /// A unique snake_case name.
    pub name: &'static str,
    /// The rule the case checks, in one sentence.
    pub rule: &'static str,
    /// Where the rule comes from (the contract file, the decision, the STEP3 section, the
    /// backlog item).
    pub source: &'static str,
    /// The operations of the contract (`gateway.openapi.yaml` or `runtime.openapi.yaml`) that
    /// the case calls; empty for a case of app traffic only (static files, the proxy).
    pub ops: &'static [&'static str],
    pub run: fn(C) -> BoxFuture<'static, CaseResult>,
}

pub type GatewayCase = Case<Ctx>;
pub type RuntimeCase = Case<RtCtx>;

/// What a case reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    Planned(String),
    Pending(String),
    Failed(String),
}

impl Outcome {
    pub fn is_failed(&self) -> bool {
        matches!(self, Outcome::Failed(_))
    }
}

/// Every gateway case.
pub fn gateway_cases() -> Vec<GatewayCase> {
    let mut v = cases_headers::cases();
    v.extend(cases_access::cases());
    v.extend(cases_tokens::cases());
    v.extend(cases_session::cases());
    v.extend(cases_static::cases());
    v.extend(cases_tenants::cases());
    v.extend(cases_later::cases());
    v
}

/// Every runtime API case.
pub fn runtime_cases() -> Vec<RuntimeCase> {
    cases_runtime_api::cases()
}

/// The context of a gateway case: the world, the started gateway, and the secrets the case got.
#[derive(Clone)]
pub struct Ctx {
    pub world: Arc<dyn GatewayWorld>,
    pub gw: GatewayHandle,
    secrets: Arc<Mutex<Vec<String>>>,
}

/// How long one request may take before the case fails (never a pass).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

impl Ctx {
    /// Remembers a value that must never appear in a failure message.
    pub fn secret(&self, value: &str) {
        if !value.is_empty() {
            self.secrets.lock().expect("lock").push(value.to_string());
        }
    }

    /// The live host of `app`.
    pub fn live_host(&self, app: &App) -> String {
        format!("{}.{}.{}", app.slug, app.tenant.slug, self.gw.apps_domain)
    }

    /// The preview host of `deployment` of `app`.
    pub fn preview_host(&self, app: &App, deployment: &Deployment) -> String {
        format!(
            "{}--{}.{}.{}",
            deployment.short_id, app.slug, app.tenant.slug, self.gw.apps_domain
        )
    }

    /// The host of a deployment: live or preview.
    pub fn host_of(&self, app: &App, deployment: &Deployment) -> String {
        match deployment.env {
            Env::Live => self.live_host(app),
            Env::Preview => self.preview_host(app, deployment),
        }
    }

    /// Sends a request to the gateway. A connection error or a timeout fails the case: a
    /// refusal is never silence. The tokens in the answer's `Set-Cookie` are remembered as
    /// secrets.
    pub async fn send(&self, req: Req) -> Result<Resp, Fail> {
        let label = req.label();
        match http::send(self.gw.addr, &req, REQUEST_TIMEOUT).await {
            Ok(resp) => {
                for c in resp.set_cookies() {
                    if let Some(sc) = SetCookie::parse(c) {
                        self.secret(&sc.value);
                    }
                }
                Ok(resp)
            }
            Err(e) => Err(Fail::Failed(format!(
                "{label}: no answer from the gateway ({e}); a refusal must be the gateway's own \
                 answer, never silence"
            ))),
        }
    }

    /// Sends with the cookies of `jar` for the request's host, and stores the answer's cookies.
    pub async fn send_jar(&self, jar: &mut Jar, req: Req) -> Result<Resp, Fail> {
        let host = req.host.clone();
        let path = req.path().to_string();
        let req = match jar.header_for(&host, &path) {
            Some(c) => req.header("Cookie", &c),
            None => req,
        };
        let resp = self.send(req).await?;
        for c in resp.set_cookies() {
            jar.store(&host, c);
        }
        Ok(resp)
    }

    /// Remembers the values of the query parameters of `url` that carry a credential of the
    /// code flow (a name with `code`, `state`, `token` or `nonce`) as secrets. Hosts and IDs stay
    /// readable in the messages.
    fn secret_query(&self, url: &str) {
        if let Some((_, q)) = url.split_once('?') {
            for pair in q.split('&') {
                if let Some((k, v)) = pair.split_once('=') {
                    let k = k.to_ascii_lowercase();
                    if ["code", "state", "token", "nonce"]
                        .iter()
                        .any(|n| k.contains(n))
                    {
                        self.secret(v);
                        self.secret(&percent_decode(v));
                    }
                }
            }
        }
    }

    /// `GET /.ruum/v1/auth/start?return_to=<return_to>` on `host` with `jar` (`return_to` is
    /// sent as it is given).
    pub async fn auth_start(
        &self,
        jar: &mut Jar,
        host: &str,
        return_to: &str,
    ) -> Result<Resp, Fail> {
        let req = Req::get(host, &format!("/.ruum/v1/auth/start?return_to={return_to}"));
        let resp = self.send_jar(jar, req).await?;
        if let Some(loc) = resp.header("location") {
            self.secret_query(loc);
        }
        Ok(resp)
    }

    /// The platform side of the code flow: checks that `start` (the answer of
    /// [`Ctx::auth_start`] on `host`) is the gateway's redirect, lets the world follow it as
    /// `user`, checks that the platform sends the browser back to this host's callback, and
    /// returns the callback's target (path and query).
    pub async fn authorize(&self, user: &User, host: &str, start: &Resp) -> Result<String, Fail> {
        let label = format!("GET {host}/.ruum/v1/auth/start");
        gw_check!(
            start.is_redirect() && !is_echo(start),
            "{label}: expected the gateway's redirect to the platform sign-in, got {} {}",
            start.status,
            start.short_body()
        );
        let loc = start.header("location").unwrap_or("").to_string();
        gw_check!(!loc.is_empty(), "{label}: the redirect has no Location");
        let url = self.world.platform_authorize(user, &loc).await?;
        self.secret_query(&url);
        let want = format!("https://{host}/.ruum/v1/auth/callback?");
        match url.strip_prefix(&want) {
            Some(q) => Ok(format!("/.ruum/v1/auth/callback?{q}")),
            None => gw_fail!(
                "the platform sent the browser to {url:?} after the sign-in on {host}, expected                  {want}..."
            ),
        }
    }

    /// Signs `user` in to `host` through the gateway's real `/.ruum/v1/auth/start` and
    /// `/.ruum/v1/auth/callback` (over HTTP, with a browser jar) and the world's platform side.
    /// `Ok(None)` when the callback refused the sign-in (`400` without a session); the case
    /// decides whether that is right.
    pub async fn try_sign_in(
        &self,
        user: &User,
        host: &str,
    ) -> Result<Option<(Jar, SignIn)>, Fail> {
        let mut jar = Jar::default();
        let start = self.auth_start(&mut jar, host, "/").await?;
        let mut raw: Vec<String> = start.set_cookies().iter().map(|c| c.to_string()).collect();
        let target = self.authorize(user, host, &start).await?;
        let req = Req::get(host, &target);
        let label = format!("GET {host}/.ruum/v1/auth/callback");
        let resp = self.send_jar(&mut jar, req).await?;
        if resp.status == 400 && !is_echo(&resp) {
            expect_no_session(&label, &resp)?;
            return Ok(None);
        }
        gw_check!(
            resp.is_redirect() && !is_echo(&resp),
            "{label}: expected the gateway's redirect that sets the app session, got {} {}",
            resp.status,
            resp.short_body()
        );
        raw.extend(resp.set_cookies().iter().map(|c| c.to_string()));
        Ok(Some((jar, SignIn { set_cookies: raw })))
    }

    /// Signs `user` in to `host` ([`Ctx::try_sign_in`]); a refused sign-in fails the case.
    /// Returns a jar with the cookies (all values are secrets).
    pub async fn sign_in(&self, user: &User, host: &str) -> Result<(Jar, SignIn), Fail> {
        match self.try_sign_in(user, host).await? {
            Some((jar, s)) => {
                self.session_of(&jar, host)?;
                Ok((jar, s))
            }
            None => gw_fail!("the sign-in on {host} was refused at the callback (400)"),
        }
    }

    /// An agent token (a secret).
    pub async fn agent_token(&self, user: &User, aud: AgentAudience) -> Result<Secret, Fail> {
        let t = self.world.agent_token(user, aud).await?;
        self.secret(t.expose());
        Ok(t)
    }

    /// The value of the session cookie `__Host-ruum_app` in `jar` for `host`.
    pub fn session_of(&self, jar: &Jar, host: &str) -> Result<String, Fail> {
        jar.value(host, "__Host-ruum_app").ok_or_else(|| {
            Fail::Failed(format!(
                "the sign-in on {host} set no __Host-ruum_app cookie (got {})",
                jar.names(host).join(", ")
            ))
        })
    }
}

/// The context of a runtime API case.
#[derive(Clone)]
pub struct RtCtx {
    pub world: Arc<dyn RuntimeApiWorld>,
    pub rt: RuntimeHandle,
    secrets: Arc<Mutex<Vec<String>>>,
}

impl RtCtx {
    pub fn secret(&self, value: &str) {
        if !value.is_empty() {
            self.secrets.lock().expect("lock").push(value.to_string());
        }
    }

    /// Sends a request to the runtime API listener (a connection error fails the case).
    pub async fn send(&self, req: Req) -> Result<Resp, Fail> {
        let label = req.label();
        http::send(self.rt.addr, &req, REQUEST_TIMEOUT)
            .await
            .map_err(|e| Fail::Failed(format!("{label}: no answer from the runtime API ({e})")))
    }

    pub async fn workload_token(&self, app: &RtApp, env: Env) -> Result<Secret, Fail> {
        let t = self.world.workload_token(app, env).await?;
        self.secret(t.expose());
        Ok(t)
    }

    pub async fn user_token(&self, app: &RtApp, env: Env, actor: &str) -> Result<Secret, Fail> {
        let t = self.world.user_token(app, env, actor).await?;
        self.secret(t.expose());
        Ok(t)
    }
}

// ---------------------------------------------------------------------------------------------
// The runner
// ---------------------------------------------------------------------------------------------

/// The outcome of a case that ran in a world (after `make()` and `start()`).
fn finish(
    case: &str,
    result: Result<CaseResult, tokio::task::JoinError>,
    secrets: &[String],
) -> Outcome {
    match result {
        Err(e) => Outcome::Failed(scrub(&format!("the case panicked: {e}"), secrets)),
        Ok(Ok(())) => Outcome::Passed,
        Ok(Err(Fail::Planned(m))) => Outcome::Failed(scrub(
            &format!(
                "a world step answered Missing after start ({m}); Missing counts only from make() \
                 and start(), never to switch a case off"
            ),
            secrets,
        )),
        Ok(Err(Fail::Pending { card, why })) if world_pending_allowed(case, &card) => {
            Outcome::Pending(scrub(&format!("{card}: {why}"), secrets))
        }
        Ok(Err(Fail::Pending { card, why })) => Outcome::Failed(scrub(
            &format!(
                "the world answered pending {card} ({why}), which this case does not allow \
                 (WORLD_PENDING)"
            ),
            secrets,
        )),
        Ok(Err(Fail::Failed(m))) => Outcome::Failed(scrub(&m, secrets)),
    }
}

/// The outcome of a failed `make()` or `start()`: planned only for [`WorldError::Missing`].
fn not_started(e: WorldError) -> Outcome {
    match e {
        WorldError::Missing(m) => Outcome::Planned(m),
        WorldError::Pending { card, why } => Outcome::Failed(format!(
            "the world answered pending {card} ({why}) before the case ran; only Missing may \
             stop a world from starting"
        )),
        WorldError::Failed(m) => Outcome::Failed(format!("world: {m}")),
    }
}

/// Runs one gateway case: planned without a world, pending when [`PENDING`] lists it, else in a
/// world of its own.
pub async fn run_gateway_case(case: &GatewayCase, make: Option<&MakeGatewayWorld>) -> Outcome {
    let Some(make) = make else {
        return Outcome::Planned("no gateway world yet (G-1a implements GatewayWorld)".into());
    };
    if let Some(r) = pending_reason(case.name) {
        return Outcome::Pending(r.to_string());
    }
    let world = match make().await {
        Ok(w) => w,
        Err(e) => return not_started(e),
    };
    let gw = match world.start().await {
        Ok(h) => h,
        Err(e) => return not_started(e),
    };
    let secrets = Arc::new(Mutex::new(Vec::new()));
    let cx = Ctx {
        world: Arc::new(Guard(world)),
        gw,
        secrets: secrets.clone(),
    };
    // On its own task, so a panic fails this case only.
    let result = tokio::spawn((case.run)(cx)).await;
    let s = secrets.lock().expect("lock").clone();
    finish(case.name, result, &s)
}

/// Runs one runtime API case.
pub async fn run_runtime_case(case: &RuntimeCase, make: Option<&MakeRuntimeWorld>) -> Outcome {
    let Some(make) = make else {
        return Outcome::Planned(
            "no runtime API world yet (R-3 and G-3 implement RuntimeApiWorld)".into(),
        );
    };
    if let Some(r) = pending_reason(case.name) {
        return Outcome::Pending(r.to_string());
    }
    let world = match make().await {
        Ok(w) => w,
        Err(e) => return not_started(e),
    };
    let rt = match world.start().await {
        Ok(h) => h,
        Err(e) => return not_started(e),
    };
    let secrets = Arc::new(Mutex::new(Vec::new()));
    let cx = RtCtx {
        world,
        rt,
        secrets: secrets.clone(),
    };
    let result = tokio::spawn((case.run)(cx)).await;
    let s = secrets.lock().expect("lock").clone();
    finish(case.name, result, &s)
}

/// How many cases run at the same time (at least 1).
fn jobs() -> usize {
    std::env::var("RUUM_GATEWAY_JOBS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(4)
        .max(1)
}

/// Runs gateway cases and returns one row per case.
pub async fn run_gateway_cases(
    cases: &[GatewayCase],
    make: Option<MakeGatewayWorld>,
) -> Vec<(&'static str, Outcome)> {
    stream::iter(cases.iter())
        .map(|c| {
            let make = make.clone();
            async move { (c.name, run_gateway_case(c, make.as_ref()).await) }
        })
        .buffered(jobs())
        .collect()
        .await
}

/// Runs runtime API cases and returns one row per case.
pub async fn run_runtime_cases(
    cases: &[RuntimeCase],
    make: Option<MakeRuntimeWorld>,
) -> Vec<(&'static str, Outcome)> {
    stream::iter(cases.iter())
        .map(|c| {
            let make = make.clone();
            async move { (c.name, run_runtime_case(c, make.as_ref()).await) }
        })
        .buffered(jobs())
        .collect()
        .await
}

/// Runs every gateway case against the world of `make` (or none), prints the report and panics
/// when a case fails. The entry point of G-1a's `tests/t6_world.rs`.
pub async fn run_gateway_suite(title: &str, make: Option<MakeGatewayWorld>) {
    let rows = run_gateway_cases(&gateway_cases(), make).await;
    let (text, ok) = render(title, &rows);
    println!("{text}");
    assert!(ok, "{title} failed:{text}");
}

/// Runs every runtime API case against the world of `make` (or none).
pub async fn run_runtime_suite(title: &str, make: Option<MakeRuntimeWorld>) {
    let rows = run_runtime_cases(&runtime_cases(), make).await;
    let (text, ok) = render(title, &rows);
    println!("{text}");
    assert!(ok, "{title} failed:{text}");
}

/// The report of a run, and whether it passes. Planned and pending never fail, also not with
/// `RUUM_REQUIRE_IMPLEMENTED=1` (the step 2 rule: planned is not a failure). The runner makes
/// sure that a world cannot produce them at will (see "Outcomes" above).
pub fn render(title: &str, rows: &[(&'static str, Outcome)]) -> (String, bool) {
    let count = |f: fn(&Outcome) -> bool| rows.iter().filter(|r| f(&r.1)).count();
    let passed = count(|o| *o == Outcome::Passed);
    let planned = count(|o| matches!(o, Outcome::Planned(_)));
    let pending = count(|o| matches!(o, Outcome::Pending(_)));
    let failed = count(|o| matches!(o, Outcome::Failed(_)));
    let mut s = String::new();
    let _ = writeln!(
        s,
        "\n=== {title}: {} cases: {passed} passed, {planned} planned, {pending} pending, {failed} \
         failed; {REQUIRE_IMPLEMENTED_ENV}={} ===",
        rows.len(),
        if require_implemented() { "1" } else { "0" }
    );
    for (name, o) in rows {
        let line = match o {
            Outcome::Passed => format!("  PASS     {name}"),
            Outcome::Planned(why) => format!("  PLANNED  {name} ({why})"),
            Outcome::Pending(why) => format!("  PENDING  {name} ({why})"),
            Outcome::Failed(why) => format!("  FAIL     {name}: {why}"),
        };
        let _ = writeln!(s, "{line}");
    }
    (s, failed == 0)
}

// ---------------------------------------------------------------------------------------------
// Redaction
// ---------------------------------------------------------------------------------------------

/// Masks secrets in a failure message: every value in `known`, everything that looks like a JWT
/// (`eyJ...` with two dots), and the value after `Bearer `, `ruum_app=`, `ruum_visit=`,
/// `ruum_report=` and the prefix `ruum_appkey_`.
pub fn scrub(msg: &str, known: &[String]) -> String {
    let mut out = msg.to_string();
    let mut known: Vec<&String> = known.iter().filter(|k| k.len() >= 4).collect();
    // Longer values first, so a value that holds another is masked whole.
    known.sort_by_key(|k| std::cmp::Reverse(k.len()));
    for k in known {
        out = out.replace(k.as_str(), "<redacted>");
    }
    out = mask_jwts(&out);
    for marker in [
        "Bearer ",
        "bearer ",
        "ruum_app=",
        "ruum_visit=",
        "ruum_report=",
        "ruum_appkey_",
    ] {
        out = mask_after(&out, marker);
    }
    out
}

fn is_b64url(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

fn mask_jwts(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("eyJ") {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail
            .char_indices()
            .find(|(_, c)| !(is_b64url(*c) || *c == '.'))
            .map(|(j, _)| j)
            .unwrap_or(tail.len());
        let cand = &tail[..end];
        if cand.matches('.').count() >= 2 {
            out.push_str("<jwt redacted>");
        } else {
            out.push_str(cand);
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

fn mask_after(s: &str, marker: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(marker) {
        out.push_str(&rest[..i + marker.len()]);
        let tail = &rest[i + marker.len()..];
        let end = tail
            .char_indices()
            .find(|(_, c)| c.is_whitespace() || matches!(c, ';' | ',' | '"' | '\'' | ')' | ']'))
            .map(|(j, _)| j)
            .unwrap_or(tail.len());
        if end > 0 && !tail.starts_with("<redacted>") && !tail.starts_with("<jwt redacted>") {
            out.push_str("<redacted>");
        } else {
            out.push_str(&tail[..end]);
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------------------------
// Shared checks of the cases
// ---------------------------------------------------------------------------------------------

/// The marker of the echo app's answers.
pub const ECHO_MARKER: &str = "ruum-t6-echo";

/// Whether an answer came from the echo app (through the gateway).
pub fn is_echo(resp: &Resp) -> bool {
    resp.json()
        .map(|v| v["echo"] == ECHO_MARKER)
        .unwrap_or(false)
}

/// The request that the echo app saw, as the gateway forwarded it (from the answer's body).
pub fn echoed(label: &str, resp: &Resp) -> Result<Value, Fail> {
    if resp.status != 200 || !is_echo(resp) {
        gw_fail!(
            "{label}: expected 200 from the app (the echo upstream), got {} {}",
            resp.status,
            resp.short_body()
        );
    }
    Ok(resp.json().unwrap_or(Value::Null))
}

/// The forwarded headers in an echo body, as `(lower-case name, value)`.
pub fn echoed_headers(v: &Value) -> Vec<(String, String)> {
    v["headers"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    Some((
                        p.get(0)?.as_str()?.to_ascii_lowercase(),
                        p.get(1)?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The values of header `name` in an echo body.
pub fn echoed_header(v: &Value, name: &str) -> Vec<String> {
    echoed_headers(v)
        .into_iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v)
        .collect()
}

/// A refusal of the gateway itself: `status` in `allowed`, and not an answer of the app.
pub fn expect_refusal(label: &str, resp: &Resp, allowed: &[u16]) -> CaseResult {
    if is_echo(resp) {
        gw_fail!(
            "{label}: the request reached the app (the echo upstream answered {}), expected the \
             gateway's refusal {allowed:?}",
            resp.status
        );
    }
    if !allowed.contains(&resp.status) {
        gw_fail!(
            "{label}: expected the gateway's refusal {allowed:?}, got {} {}",
            resp.status,
            resp.short_body()
        );
    }
    Ok(())
}

/// The statuses of "go and sign in" or "not allowed": a redirect to the sign-in, `401`, `403`.
pub const SIGN_IN_OR_REFUSED: &[u16] = &[302, 303, 307, 401, 403];

/// How much later than its bound a refusal may arrive (the time of one probe's round trip).
pub const POLL_ALLOWANCE: Duration = Duration::from_secs(1);

/// Polls `probe` every `every` until it returns `true` (the refusal arrived). `since` is the
/// moment right after the world step (the right removed, the sign-out, the approval withdrawn).
/// The case fails when the refusal has not **arrived** within `bound` plus [`POLL_ALLOWANCE`]
/// after `since`: each probe gets only the time that is left, so a slow probe that starts inside
/// the bound and answers after it fails. The probe's own failures stop the poll.
pub async fn poll_until<F, Fut>(
    label: &str,
    since: tokio::time::Instant,
    bound: Duration,
    every: Duration,
    mut probe: F,
) -> CaseResult
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<bool, Fail>>,
{
    let deadline = since + bound + POLL_ALLOWANCE;
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            gw_fail!(
                "{label}: still not refused {} ms after the change (bound {} ms)",
                (now - since).as_millis(),
                bound.as_millis()
            );
        }
        match tokio::time::timeout(deadline - now, probe()).await {
            Ok(r) => {
                if r? {
                    return Ok(());
                }
            }
            Err(_) => gw_fail!(
                "{label}: no refusal arrived within {} ms after the change (bound {} ms)",
                (bound + POLL_ALLOWANCE).as_millis(),
                bound.as_millis()
            ),
        }
        tokio::time::sleep(every).await;
    }
}

/// Fails when `resp` sets an app session (`__Host-ruum_app` with a value and without
/// `Max-Age=0`; names and attributes compared without case).
pub fn expect_no_session(label: &str, resp: &Resp) -> CaseResult {
    for c in resp.set_cookies() {
        let Some(sc) = SetCookie::parse(c) else {
            continue;
        };
        let removal = sc
            .attr("max-age")
            .and_then(|v| v.trim().parse::<i64>().ok())
            .map(|v| v <= 0)
            .unwrap_or(false);
        if sc.name.eq_ignore_ascii_case("__Host-ruum_app") && !sc.value.is_empty() && !removal {
            gw_fail!("{label}: the answer set an app session");
        }
    }
    Ok(())
}

/// A jar that holds only `__Host-ruum_app=<sid>` for `host` (a session moved to another host).
pub fn jar_with_session(host: &str, sid: &str) -> Jar {
    let mut j = Jar::default();
    j.store(
        host,
        &format!("__Host-ruum_app={sid}; Path=/; Secure; HttpOnly; SameSite=Lax"),
    );
    j
}

/// Decodes `%XX` escapes (and nothing else; `+` stays).
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The decoded value of the query parameter `name` of a URL or a target.
pub fn query_param(url: &str, name: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    q.split('&').find_map(|p| {
        let (k, v) = p.split_once('=')?;
        (k == name).then(|| percent_decode(&v.replace('+', " ")))
    })
}

/// The path of a redirect `Location` when it stays on `host`: a relative path (`/x`, not
/// `//x` or `/\x`) or `https://<host>/...`. `None` when it leads off the origin.
pub fn same_origin_path(location: &str, host: &str) -> Option<String> {
    let path = match location.strip_prefix(&format!("https://{host}")) {
        Some("") => "/",
        Some(rest) if rest.starts_with('/') => rest,
        Some(_) => return None,
        None => location,
    };
    let ok = path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains('\\')
        && !path.chars().any(|c| c.is_control());
    ok.then(|| path.to_string())
}

/// A small HTML page used as a static file.
pub fn page(title: &str) -> Vec<u8> {
    format!("<!doctype html><title>{title}</title><h1>{title}</h1>").into_bytes()
}

/// `(path, bytes)` pairs from string literals.
pub fn files(list: &[(&str, &[u8])]) -> Vec<(String, Vec<u8>)> {
    list.iter()
        .map(|(p, b)| (p.to_string(), b.to_vec()))
        .collect()
}

/// A platform app of `tenant`/`app` with a live deployment, an echo upstream (when
/// `upstream`), and a member with `view` who is signed in.
pub struct Scene {
    pub tenant: Tenant,
    pub app: App,
    pub live: Deployment,
    pub user: User,
    pub host: String,
    pub jar: Jar,
    pub echo: Option<Echo>,
}

impl Scene {
    pub fn echo(&self) -> &Echo {
        self.echo.as_ref().expect("the scene has an upstream")
    }
}

/// Seeds a [`Scene`]: tenant `acme`, app `garden` (`access: platform`), a live deployment with
/// `index.html`, user `mari` with `view`, signed in on the live host.
pub async fn scene(cx: &Ctx, upstream: bool) -> Result<Scene, Fail> {
    scene_with(cx, "acme", "garden", upstream).await
}

pub async fn scene_with(
    cx: &Ctx,
    tenant_slug: &str,
    app_slug: &str,
    upstream: bool,
) -> Result<Scene, Fail> {
    let w = &cx.world;
    let tenant = w.seed_tenant(tenant_slug).await?;
    let user = w.seed_user(&tenant, "mari").await?;
    let app = w.seed_app(&tenant, app_slug, Access::Platform).await?;
    let live = w
        .seed_deployment(
            &app,
            Env::Live,
            &files(&[("index.html", &page(app_slug))]),
            None,
        )
        .await?;
    w.set_live(&app, &live).await?;
    let echo = if upstream {
        let e = Echo::start().await;
        w.set_upstream(&live, &e.url).await?;
        Some(e)
    } else {
        None
    };
    w.grant(&user, &app, &Relation::View).await?;
    let host = cx.live_host(&app);
    let (jar, _) = cx.sign_in(&user, &host).await?;
    Ok(Scene {
        tenant,
        app,
        live,
        user,
        host,
        jar,
        echo,
    })
}

/// The `max-age` of a `Strict-Transport-Security` value.
pub fn hsts_max_age(value: &str) -> Option<u64> {
    value.split(';').find_map(|d| {
        let (k, v) = d.trim().split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("max-age")
            .then(|| v.trim().trim_matches('"').parse().ok())
            .flatten()
    })
}

/// Checks the response headers that every answer of an app origin gets (`gateway.openapi.yaml`,
/// "The check of every request" 4): HSTS with a positive `max-age`, `nosniff`,
/// `Referrer-Policy: strict-origin-when-cross-origin`, and `frame-ancestors` (`'none'` for live
/// apps without their own value; the UI origin for previews).
pub fn check_security_headers(label: &str, resp: &Resp, env: Env, ui_origin: &str) -> CaseResult {
    let hsts = resp.header("strict-transport-security").unwrap_or("");
    gw_check!(
        hsts_max_age(hsts).map(|a| a > 0).unwrap_or(false),
        "{label}: Strict-Transport-Security missing or without a positive max-age: {hsts:?}"
    );
    gw_check!(
        resp.header("x-content-type-options")
            .map(|v| v.trim().eq_ignore_ascii_case("nosniff"))
            .unwrap_or(false),
        "{label}: X-Content-Type-Options: nosniff missing"
    );
    let rp = resp.headers_all("referrer-policy");
    gw_check!(
        rp.len() == 1
            && rp[0]
                .trim()
                .eq_ignore_ascii_case("strict-origin-when-cross-origin"),
        "{label}: expected exactly Referrer-Policy: strict-origin-when-cross-origin, got {rp:?}"
    );
    let csp = resp.headers_all("content-security-policy").join(", ");
    let fa = frame_ancestors(&csp);
    let only = |want: &str| !fa.is_empty() && fa.iter().all(|d| d == want);
    match env {
        Env::Live => gw_check!(
            only("'none'"),
            "{label}: a live app without its own value needs frame-ancestors 'none' (and no \
             other value), got {csp:?}"
        ),
        Env::Preview => gw_check!(
            only(ui_origin),
            "{label}: a preview allows only the UI origin {ui_origin} as frame-ancestors, got \
             {csp:?}"
        ),
    }
    Ok(())
}

/// The values of every `frame-ancestors` directive in the CSP headers `csp` (joined with `, `:
/// a browser enforces each policy, so every one counts). Directive names are matched without
/// case, as CSP does.
pub fn frame_ancestors(csp: &str) -> Vec<String> {
    csp.split([';', ','])
        .map(str::trim)
        .filter_map(|d| {
            let mut it = d.splitn(2, char::is_whitespace);
            let name = it.next()?;
            name.eq_ignore_ascii_case("frame-ancestors").then(|| {
                it.next()
                    .unwrap_or("")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_masks_tokens_and_cookies() {
        let jwt = "eyJhbGciOiJFZERTQSJ9.eyJzdWIiOiJ4In0.c2lnbmF0dXJl";
        // Built at run time, so the secret scan of the repository does not see a key.
        let key_tail = "ABCDEFGHIJKL".repeat(2);
        let msg = format!(
            "got Ruum-User {jwt}; Cookie: __Host-ruum_app=abc123def; Authorization: Bearer \
             sekrit-value, key ruum_appkey_{key_tail} and known-value-1"
        );
        let out = scrub(&msg, &["known-value-1".into()]);
        for leaked in [
            jwt,
            "abc123def",
            "sekrit-value",
            key_tail.as_str(),
            "known-value-1",
        ] {
            assert!(!out.contains(leaked), "{leaked} leaked: {out}");
        }
        assert!(out.contains("Ruum-User"), "{out}");
    }

    #[test]
    fn frame_ancestors_is_found() {
        assert_eq!(
            frame_ancestors("default-src 'self'; frame-ancestors 'none'"),
            vec!["'none'"]
        );
        assert_eq!(
            frame_ancestors("frame-ancestors https://a.example, frame-ancestors  'none'"),
            vec!["https://a.example", "'none'"]
        );
        assert!(frame_ancestors("default-src 'self'").is_empty());
        assert_eq!(frame_ancestors("Frame-Ancestors 'none'"), vec!["'none'"]);
        assert!(frame_ancestors("frame-ancestorsx 'none'").is_empty());
    }

    #[test]
    fn helpers_of_the_cases() {
        assert_eq!(
            hsts_max_age("max-age=31536000; includeSubDomains"),
            Some(31536000)
        );
        assert_eq!(hsts_max_age("Max-Age=0"), Some(0));
        assert_eq!(hsts_max_age("includeSubDomains"), None);
        assert_eq!(percent_decode("/%2e%2e/a%2Fb%"), "/../a/b%");
        assert_eq!(
            query_param(
                "/.ruum/v1/auth/start?x=1&return_to=%2Fprivate%3Fq%3D1",
                "return_to"
            )
            .as_deref(),
            Some("/private?q=1")
        );
        let h = "garden.acme.ruumapps.localhost";
        assert_eq!(same_origin_path("/a?b", h).as_deref(), Some("/a?b"));
        assert_eq!(
            same_origin_path(&format!("https://{h}/x"), h).as_deref(),
            Some("/x")
        );
        for bad in [
            "//evil.example/",
            "/\\evil.example",
            "https://evil.example/",
            "javascript:alert(1)",
            "https://garden.acme.ruumapps.localhost.evil.example/",
        ] {
            assert_eq!(same_origin_path(bad, h), None, "{bad}");
        }
        let ok = Resp {
            status: 302,
            headers: vec![(
                "set-cookie".into(),
                "__Host-ruum_app=; Path=/; MAX-AGE=0".into(),
            )],
            body: vec![],
        };
        assert!(expect_no_session("x", &ok).is_ok());
        let bad = Resp {
            status: 302,
            headers: vec![("set-cookie".into(), "__host-RUUM_APP=s1; Path=/".into())],
            body: vec![],
        };
        assert!(expect_no_session("x", &bad).is_err());
    }
}
