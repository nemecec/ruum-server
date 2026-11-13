//! The internal router: endpoints for the edge inside the cluster, never for the Internet.
//!
//! `ruum-server` serves it on its own listener, `RUUM_INTERNAL_BIND` (off when not set). No
//! public route of the edge points at that listener, and the network policy of staging lets only
//! the edge reach it (review 1 B-6).
//!
//! - `GET /internal/tls-ask?domain=<host>`: the `ask` endpoint of Caddy's on-demand TLS. It
//!   answers `200` only for `<tenant>.<content host>` where the tenant exists, and `404` for every
//!   other name, so Caddy gets no certificate for a name that nobody uses. Caddy fails closed: an
//!   error or a timeout also means no certificate. New names are rate limited
//!   ([`TlsAskConfig::new_hosts_per_hour`]), so even many real tenants cannot use up the CA's
//!   limits in a burst; a name that was allowed once stays allowed without counting again.
//! - `POST /internal/gateway/v1/...`: the small internal API of the app gateway (card G-1a;
//!   `ruum_core::identity::app_sign_in::wire`). The gateway holds no tenant keys and no general
//!   system access (decision 0006-1), so it asks the core: what a deployment is (`deployment`),
//!   redeem a sign-in code once (`redeem`), whether an app session is live and may use the app
//!   (`session`), end a session (`session/end`), and what a bearer token is (`agent`). The
//!   answers name only what the gateway needs: IDs, flags and the pairwise subjects of design
//!   9.4, never a platform user ID or an email.

use crate::state::AppState;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use ruum_core::blobs::signed::tenant_of_host;
use serde::Deserialize;
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Settings of the `ask` endpoint.
#[derive(Clone, Debug)]
pub struct TlsAskConfig {
    /// The host of the user content domain, for example `content.staging.ruumapps.net`.
    pub content_host: String,
    /// At most this many names that were not allowed before get `200` per hour
    /// (`RUUM_TLS_ASK_NEW_HOSTS_PER_HOUR`, default 20). Over the limit: `429`.
    pub new_hosts_per_hour: usize,
}

impl TlsAskConfig {
    /// The settings for a content base URL such as `https://content.staging.ruumapps.net`.
    pub fn for_content_url(content_url: &str, new_hosts_per_hour: usize) -> Self {
        let host = url::Url::parse(content_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
            .unwrap_or_else(|| content_url.trim_end_matches('/').to_ascii_lowercase());
        Self {
            content_host: host,
            new_hosts_per_hour,
        }
    }
}

#[derive(Default)]
struct Window {
    /// Names that got `200`.
    allowed: HashSet<String>,
    /// When each new name got `200`, in the last hour.
    recent: VecDeque<Instant>,
}

#[derive(Clone)]
struct AskState {
    app: AppState,
    config: TlsAskConfig,
    window: Arc<Mutex<Window>>,
}

#[derive(Deserialize)]
struct AskQuery {
    domain: Option<String>,
}

const HOUR: Duration = Duration::from_secs(3600);

/// The internal router (see the module docs).
pub fn router(app: AppState, config: TlsAskConfig) -> Router {
    Router::new()
        .route("/internal/tls-ask", get(tls_ask))
        .with_state(AskState {
            app: app.clone(),
            config,
            window: Arc::default(),
        })
        .merge(gateway::router(app))
}

/// The internal API of the app gateway (see the module docs).
pub mod gateway {
    use crate::state::AppState;
    use axum::Router;
    use axum::extract::{Json, State};
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use axum::routing::post;
    use ruum_core::apps::settings::Access;
    use ruum_core::apps::store::{DeploymentState, Environment};
    use ruum_core::authz::{ObjectRef, Role, SubjectRef};
    use ruum_core::identity::app_sign_in::wire::{
        AgentAnswer, AgentRequest, DeploymentInfo, DeploymentQuery, EndSessionRequest, PREFIX,
        RedeemRequest, RedeemResponse, SessionInfo, SessionRequest,
    };
    use ruum_core::identity::app_sign_in::{AppBinding, AppEnv, pairwise};
    use ruum_core::{NodeId, TenantId, UserId};
    use uuid::Uuid;

    /// The routes under [`PREFIX`].
    pub fn router(app: AppState) -> Router {
        Router::new()
            .route(&format!("{PREFIX}/deployment"), post(deployment))
            .route(&format!("{PREFIX}/redeem"), post(redeem))
            .route(&format!("{PREFIX}/session"), post(session))
            .route(&format!("{PREFIX}/session/end"), post(end_session))
            .route(&format!("{PREFIX}/agent"), post(agent))
            .with_state(app)
    }

    /// An internal error: logged without values, answered `500` (the gateway fails closed).
    fn failed(what: &str, e: impl std::fmt::Display) -> Response {
        tracing::error!(target: "ruum::gateway_api", error = %e, "{what} failed");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }

    /// The deployment of a host, as the core sees it. `None` when the app or the deployment is
    /// gone.
    pub async fn deployment_info(
        s: &AppState,
        tenant: TenantId,
        app: NodeId,
        deployment: Uuid,
    ) -> ruum_core::Result<Option<DeploymentInfo>> {
        let store = s.services.apps.store();
        let Some(row) = store.get_app(tenant, app).await? else {
            return Ok(None);
        };
        let Some(dep) = store.get_deployment(tenant, app, deployment).await? else {
            return Ok(None);
        };
        let public_approved = store
            .tenant_settings(tenant)
            .await?
            .is_some_and(|t| t.public_apps_approved);
        let now = ruum_core::now();
        let (environment, serving) = match dep.environment {
            Environment::Live => (
                AppEnv::Live,
                dep.state == DeploymentState::Live && row.live_deployment_id == Some(dep.id),
            ),
            Environment::Preview => (
                AppEnv::Preview,
                dep.state == DeploymentState::Ready && dep.expires_at.is_none_or(|t| t > now),
            ),
        };
        Ok(Some(DeploymentInfo {
            environment,
            access: match row.settings.access {
                Access::Platform => "platform".into(),
                Access::Public => "public".into(),
            },
            public_approved,
            serving,
        }))
    }

    async fn deployment(State(s): State<AppState>, Json(q): Json<DeploymentQuery>) -> Response {
        match deployment_info(&s, q.tenant_id, q.app_id, q.deployment_id).await {
            Ok(Some(info)) => Json(info).into_response(),
            Ok(None) => StatusCode::NOT_FOUND.into_response(),
            Err(e) => failed("deployment", e),
        }
    }

    async fn redeem(State(s): State<AppState>, Json(r): Json<RedeemRequest>) -> Response {
        match s
            .services
            .identity
            .redeem_app_code(&r.binding, &r.code, &r.state)
            .await
        {
            Ok(Some((session, secret))) => Json(RedeemResponse {
                session: secret,
                max_age: (session.expires_at - ruum_core::now())
                    .whole_seconds()
                    .max(0),
            })
            .into_response(),
            Ok(None) => {
                tracing::warn!(
                    target: "ruum::security",
                    event = "app.sign_in_refused",
                    "an app sign-in code was refused"
                );
                StatusCode::BAD_REQUEST.into_response()
            }
            Err(e) => failed("redeem", e),
        }
    }

    /// The pairwise subject of `principal` for `app`, with the tenant's pseudonym key.
    async fn subject(
        s: &AppState,
        tenant: TenantId,
        app: NodeId,
        principal: Uuid,
    ) -> ruum_core::Result<String> {
        let key = s.services.tenant_keys.tenant_key(tenant).await?;
        Ok(pairwise(key.as_bytes(), app, principal))
    }

    /// `use` (live) or `open_preview` (preview) of `user` on the app node, at least as fresh as
    /// the tenant's newest relationship write.
    async fn may_open(s: &AppState, binding: &AppBinding, user: UserId) -> ruum_core::Result<bool> {
        let permission = match binding.environment {
            AppEnv::Live => "use",
            AppEnv::Preview => "open_preview",
        };
        let token = s.services.freshness.tenant_token(binding.tenant_id).await?;
        s.services
            .authz
            .check(
                &SubjectRef::user(user),
                permission,
                &ObjectRef::node(binding.app_id),
                token.as_ref(),
            )
            .await
    }

    async fn session_info(
        s: &AppState,
        r: &SessionRequest,
    ) -> ruum_core::Result<Option<SessionInfo>> {
        let b = &r.binding;
        let Some(session) = s
            .services
            .identity
            .app_session(b.tenant_id, &r.session)
            .await?
        else {
            return Ok(None);
        };
        if !session.serves(b) {
            return Ok(None);
        }
        // The app must still exist (an app removed and its host given to another app).
        if s.services
            .apps
            .store()
            .get_app(b.tenant_id, b.app_id)
            .await?
            .is_none()
        {
            return Ok(None);
        }
        let allowed = may_open(s, b, session.user_id).await?;
        Ok(Some(SessionInfo {
            sub: subject(s, b.tenant_id, b.app_id, session.user_id.0).await?,
            allowed,
            // Card B-2 provides `apps::roles::effective_roles` (plan seam 3); until it is merged
            // the claim is empty.
            roles: Vec::new(),
        }))
    }

    async fn session(State(s): State<AppState>, Json(r): Json<SessionRequest>) -> Response {
        match session_info(&s, &r).await {
            Ok(Some(info)) => Json(info).into_response(),
            Ok(None) => StatusCode::NOT_FOUND.into_response(),
            Err(e) => failed("session", e),
        }
    }

    async fn end_session(State(s): State<AppState>, Json(r): Json<EndSessionRequest>) -> Response {
        match s
            .services
            .identity
            .end_app_session(r.tenant_id, &r.session)
            .await
        {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(e) => failed("session end", e),
        }
    }

    async fn agent_answer(s: &AppState, r: &AgentRequest) -> ruum_core::Result<AgentAnswer> {
        let who =
            match ruum_core::identity::auth::authenticate_app_agent(&s.services, &r.token).await {
                Ok(who) => who,
                Err(ruum_core::Error::Api(_)) => {
                    return Ok(if s.services.tokens.signed_by_platform(&r.token) {
                        AgentAnswer::Platform
                    } else {
                        AgentAnswer::Foreign
                    });
                }
                Err(e) => return Err(e),
            };
        let (Some(user), Some(bound)) = (who.user(), who.bound_tenant) else {
            return Ok(AgentAnswer::Platform);
        };
        let b = &r.binding;
        let sub = subject(s, b.tenant_id, b.app_id, who.actor.actor.id.0).await?;
        let on_behalf_of = subject(s, b.tenant_id, b.app_id, user.0).await?;
        // Only previews, only in the agent's own tenant, only with open_preview of its user and
        // a scope that allows edit on the app node (decision 0006-10).
        let mut allowed = b.environment == AppEnv::Preview && bound == b.tenant_id;
        if allowed {
            let ctx = ruum_core::identity::auth::context_for(&s.services, &who, Some(bound), None)
                .await?;
            allowed = may_open(s, b, user).await?
                && ruum_core::authz::require(
                    s.services.authz.as_ref(),
                    s.services.nodes.as_ref(),
                    &ctx,
                    b.app_id,
                    Role::Edit,
                )
                .await
                .is_ok();
        }
        Ok(AgentAnswer::Agent {
            allowed,
            sub,
            on_behalf_of,
        })
    }

    async fn agent(State(s): State<AppState>, Json(r): Json<AgentRequest>) -> Response {
        match agent_answer(&s, &r).await {
            Ok(a) => Json(a).into_response(),
            Err(e) => failed("agent", e),
        }
    }
}

async fn tls_ask(State(s): State<AskState>, Query(q): Query<AskQuery>) -> StatusCode {
    let Some(domain) = q
        .domain
        .map(|d| d.trim().trim_end_matches('.').to_ascii_lowercase())
    else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(label) = domain
        .strip_suffix(&s.config.content_host)
        .and_then(|rest| rest.strip_suffix('.'))
    else {
        return StatusCode::NOT_FOUND;
    };
    // Exactly one label in front of the content host, and it is a tenant ID.
    if label.is_empty() || label.contains('.') {
        return StatusCode::NOT_FOUND;
    }
    let Some(tenant) = tenant_of_host(label) else {
        return StatusCode::NOT_FOUND;
    };
    {
        let w = s.window.lock().expect("ask window lock");
        if w.allowed.contains(&domain) {
            return StatusCode::OK;
        }
    }
    if s.app.services.tenants.find(tenant).await.is_err() {
        return StatusCode::NOT_FOUND;
    }
    let mut w = s.window.lock().expect("ask window lock");
    if w.allowed.contains(&domain) {
        return StatusCode::OK;
    }
    let now = Instant::now();
    while w
        .recent
        .front()
        .is_some_and(|t| now.duration_since(*t) >= HOUR)
    {
        w.recent.pop_front();
    }
    if w.recent.len() >= s.config.new_hosts_per_hour {
        tracing::warn!(
            target: "ruum::security",
            event = "tls.ask_rate_limited",
            "too many new content hosts in the last hour; no certificate"
        );
        return StatusCode::TOO_MANY_REQUESTS;
    }
    w.recent.push_back(now);
    w.allowed.insert(domain);
    StatusCode::OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use ruum_core::identity::VerifiedLogin;
    use tower::ServiceExt;

    async fn ask(app: &Router, domain: &str) -> StatusCode {
        let uri = format!("/internal/tls-ask?domain={domain}");
        let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
        app.clone().oneshot(req).await.unwrap().status()
    }

    /// A new user through the fake sign-in; their personal tenant exists.
    async fn tenant(state: &AppState, email: &str) -> ruum_core::TenantId {
        let user = state
            .services
            .identity
            .upsert_from_login(&VerifiedLogin {
                issuer: "https://issuer.example".into(),
                subject: email.into(),
                email: Some(email.into()),
                email_verified: true,
                name: None,
                id_token: None,
            })
            .await
            .unwrap();
        state
            .services
            .tenants
            .find(user.home_tenant_id)
            .await
            .unwrap();
        user.home_tenant_id
    }

    #[tokio::test]
    async fn only_existing_tenant_hosts_get_a_certificate() {
        let (_, _, state) = crate::routes::auth::tests::app();
        let t = tenant(&state, "owner@example.test").await;
        let app = router(
            state,
            TlsAskConfig::for_content_url("https://content.staging.ruumapps.net", 20),
        );
        let host = format!("{t}.content.staging.ruumapps.net");
        assert_eq!(ask(&app, &host).await, StatusCode::OK);
        assert_eq!(ask(&app, &host.to_uppercase()).await, StatusCode::OK);
        assert_eq!(ask(&app, &format!("{host}.")).await, StatusCode::OK);
        let unknown = format!("{}.content.staging.ruumapps.net", uuid::Uuid::now_v7());
        assert_eq!(ask(&app, &unknown).await, StatusCode::NOT_FOUND);
        for bad in [
            "content.staging.ruumapps.net",
            "x.content.staging.ruumapps.net",
            "staging.ruum.space",
            "evil.example",
            &format!("a.{t}.content.staging.ruumapps.net"),
            &format!("{t}.content.staging.ruumapps.net.evil.example"),
            &format!("{t}content.staging.ruumapps.net"),
        ] {
            assert_eq!(ask(&app, bad).await, StatusCode::NOT_FOUND, "{bad}");
        }
        let req = Request::builder()
            .uri("/internal/tls-ask")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn new_hosts_are_rate_limited_and_known_hosts_are_not() {
        let (_, _, state) = crate::routes::auth::tests::app();
        let a = tenant(&state, "a@example.test").await;
        let b = tenant(&state, "b@example.test").await;
        let app = router(
            state,
            TlsAskConfig::for_content_url("https://content.staging.ruumapps.net/", 1),
        );
        let ha = format!("{a}.content.staging.ruumapps.net");
        let hb = format!("{b}.content.staging.ruumapps.net");
        assert_eq!(ask(&app, &ha).await, StatusCode::OK);
        assert_eq!(ask(&app, &hb).await, StatusCode::TOO_MANY_REQUESTS);
        // The host allowed before stays allowed.
        assert_eq!(ask(&app, &ha).await, StatusCode::OK);
    }

    #[test]
    fn the_content_host_comes_from_the_url() {
        let c = TlsAskConfig::for_content_url("https://Content.Staging.ruumapps.net/", 5);
        assert_eq!(c.content_host, "content.staging.ruumapps.net");
    }
}
