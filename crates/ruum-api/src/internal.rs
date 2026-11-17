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
//!   (`session`), end a session (`session/end`), what a bearer token is (`agent`), and where the
//!   instance of a sandbox deployment runs (`upstream`, card G-1b: the core's
//!   `SandboxDirectory` answers from its own deployment record; without a directory the answer
//!   is `not_running`). The answers name only what the gateway needs: IDs, flags, the pairwise
//!   subjects of design 9.4 and an instance's `http://<host>:<port>`, never a platform user ID or
//!   an email.

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
        RedeemRequest, RedeemResponse, SessionInfo, SessionRequest, UpstreamAnswer, UpstreamQuery,
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
            .route(&format!("{PREFIX}/upstream"), post(upstream))
            // Each request gets its own authz freshness floor, as on the public router (decision
            // 0002-4). Without it the floor helpers do nothing here, and a check falls back to
            // this process's own last token: a revoke made on another replica could still pass.
            .layer(axum::middleware::from_fn(request_freshness))
            .with_state(app)
    }

    /// Runs one internal request inside `ruum_core::authz::with_request_freshness`.
    async fn request_freshness(
        req: axum::extract::Request,
        next: axum::middleware::Next,
    ) -> Response {
        ruum_core::authz::with_request_freshness(next.run(req)).await
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
            sandbox: ruum_core::apps::sandboxes::is_sandbox(dep.manifest.as_ref()),
        }))
    }

    /// Where the instance of a sandbox deployment runs (card G-1b). The core's directory reads
    /// the deployment record by the query's tenant, app and deployment and checks its
    /// environment; without a directory (the server until I-4) the answer is `not_running`.
    async fn upstream(State(s): State<AppState>, Json(q): Json<UpstreamQuery>) -> Response {
        let Some(dir) = s.services.sandboxes.clone() else {
            return Json(UpstreamAnswer::NotRunning).into_response();
        };
        match dir.upstream(&q).await {
            Ok(a) => Json(a).into_response(),
            Err(e) => failed("upstream", e),
        }
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
    /// the tenant's newest relationship write. It also raises the request's floor to that token,
    /// so the later reads of the request (the roles claim) are as fresh (Kimi G-1a K-1).
    async fn may_open(s: &AppState, binding: &AppBinding, user: UserId) -> ruum_core::Result<bool> {
        let permission = match binding.environment {
            AppEnv::Live => "use",
            AppEnv::Preview => "open_preview",
        };
        let token = s.services.freshness.tenant_token(binding.tenant_id).await?;
        if let Some(t) = &token {
            ruum_core::authz::raise_request_floor(t);
        }
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
        // The roles claim (plan seam 3, card B-2): the active app roles of the user, or the
        // default role. Empty when the user may not open the app. The role reads pass no token:
        // they read at the request floor that `may_open` raised to the tenant's newest token, so
        // a role revoked on another replica is gone here at once.
        let roles = if allowed {
            s.services
                .apps
                .effective_roles(b.tenant_id, b.app_id, Some(session.user_id))
                .await?
        } else {
            Vec::new()
        };
        Ok(Some(SessionInfo {
            sub: subject(s, b.tenant_id, b.app_id, session.user_id.0).await?,
            allowed,
            roles,
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

    /// The `upstream` route of the gateway (card G-1b).
    mod upstream_route {
        use super::*;
        use ruum_core::actor::{Actor, Context};
        use ruum_core::apps::sandboxes::{Locator, SandboxDirectory, Upstream};
        use ruum_core::apps::store::{
            Archive, DeploymentSource, DeploymentState, Environment, NewDeployment,
        };
        use ruum_core::identity::app_sign_in::AppEnv;
        use ruum_core::identity::app_sign_in::wire::{PREFIX, UpstreamAnswer, UpstreamQuery};
        use ruum_core::nodes::OnConflict;
        use ruum_core::runtime::{
            ImageRef, InstanceStatus, Limits, MemoryDriver, MemoryRegistry, MemoryUsage, OciDigest,
            RuntimeDriver, SecretValue, ServiceSpec, WorkloadIdentity, WorkloadKind, WorkloadSpec,
        };
        use std::sync::atomic::{AtomicBool, Ordering};

        struct Seed {
            query: UpstreamQuery,
            other_tenant: ruum_core::TenantId,
        }

        async fn seed(state: &AppState, runtime: &str) -> Seed {
            let owner = state
                .services
                .identity
                .upsert_from_login(&VerifiedLogin {
                    issuer: "https://issuer.example".into(),
                    subject: format!("g1b-{}", uuid::Uuid::now_v7()),
                    email: Some(format!("g1b-{}@example.test", uuid::Uuid::now_v7())),
                    email_verified: true,
                    name: None,
                    id_token: None,
                })
                .await
                .unwrap();
            let run = uuid::Uuid::now_v7().simple().to_string();
            let slug = format!("g1b{}", &run[run.len() - 10..]);
            let team = state
                .services
                .tenants
                .create_team(&Actor::user(owner.id), slug.clone())
                .await
                .unwrap();
            let store = state.services.apps.store();
            store
                .ensure_tenant_settings(team.id, &[slug])
                .await
                .unwrap();
            let ctx = Context {
                tenant: team.id,
                actor: Actor::user(owner.id),
                scope: None,
                idempotency_key: None,
            };
            let app = state
                .services
                .apps
                .create(
                    &ctx,
                    ruum_core::apps::CreateApp {
                        parent_id: team.root_node_id,
                        name: "garden".into(),
                        settings: None,
                        on_conflict: OnConflict::Fail,
                    },
                )
                .await
                .unwrap()
                .value;
            let id = uuid::Uuid::now_v7();
            store
                .insert_deployment(
                    &ctx,
                    NewDeployment {
                        id,
                        app_node_id: app.node.id,
                        environment: Environment::Preview,
                        state: DeploymentState::Ready,
                        short_id: ruum_core::apps::slugs::short_id(),
                        label: None,
                        draft: None,
                        new_chunks: Vec::new(),
                        source: DeploymentSource {
                            kind: "folder".into(),
                            node_id: None,
                            version_id: None,
                            files: Some(Vec::new()),
                        },
                        archive: Archive {
                            content_hash: [7u8; 32],
                            size: 1,
                            chunks: Vec::new(),
                        },
                        manifest: Some(serde_json::json!({ "runtime": runtime, "start": "x" })),
                        stages: serde_json::json!([]),
                        published_from: None,
                        expires_at: None,
                    },
                )
                .await
                .unwrap();
            Seed {
                query: UpstreamQuery {
                    tenant_id: team.id,
                    app_id: app.node.id,
                    deployment_id: id,
                    environment: AppEnv::Preview,
                },
                // The owner's personal tenant: a real tenant that does not own the app.
                other_tenant: owner.home_tenant_id,
            }
        }

        async fn ask(app: &Router, q: &UpstreamQuery) -> (StatusCode, Option<UpstreamAnswer>) {
            let req = Request::builder()
                .method("POST")
                .uri(format!("{PREFIX}/upstream"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(q).unwrap()))
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            let status = resp.status();
            let body = http_body_util::BodyExt::collect(resp.into_body())
                .await
                .unwrap()
                .to_bytes();
            (status, serde_json::from_slice(&body).ok())
        }

        fn routes(state: AppState) -> Router {
            router(
                state,
                TlsAskConfig::for_content_url("https://content.example.test", 20),
            )
        }

        #[tokio::test]
        async fn without_a_directory_the_answer_is_not_running() {
            let state = AppState::in_memory();
            let s = seed(&state, "node22").await;
            let app = routes(state);
            assert_eq!(
                ask(&app, &s.query).await,
                (StatusCode::OK, Some(UpstreamAnswer::NotRunning))
            );
        }

        #[tokio::test]
        async fn the_answer_comes_from_the_cores_record() {
            let mut state = AppState::in_memory();
            let s = seed(&state, "node22").await;
            let q = s.query.clone();
            let registry = MemoryRegistry::new();
            let driver = Arc::new(MemoryDriver::new(
                registry.clone(),
                Arc::new(MemoryUsage::new()),
            ));
            let identity = WorkloadIdentity {
                tenant: q.tenant_id,
                app: q.app_id,
                deployment: q.deployment_id,
                environment: ruum_core::runtime::Environment::Preview,
            };
            let digest = OciDigest::of(b"g1b route image");
            registry.push(&identity, &digest);
            let st = driver
                .start(WorkloadSpec {
                    identity,
                    kind: WorkloadKind::Service(ServiceSpec {
                        port: 3000,
                        health: None,
                        idle_timeout: None,
                    }),
                    image: ImageRef::Oci(digest),
                    command: Vec::new(),
                    env: Vec::new(),
                    previous_secrets: Vec::new(),
                    workload_token: SecretValue::new("g1b-route-token"),
                    limits: Limits {
                        cpu_millis: 100,
                        memory_bytes: 1 << 26,
                        disk_bytes: 1 << 26,
                    },
                    routes: Vec::new(),
                })
                .await
                .unwrap();
            // The locator notes whether it runs inside the request's freshness scope.
            let in_request = Arc::new(AtomicBool::new(false));
            let seen = in_request.clone();
            let locator: Locator = Arc::new(move |_st: &InstanceStatus| {
                seen.store(ruum_core::authz::in_request(), Ordering::SeqCst);
                Some(Upstream {
                    host: "10.89.0.7".into(),
                    port: 3000,
                })
            });
            let dir = SandboxDirectory::new(state.services.apps.store().clone(), driver, locator);
            dir.register(q.tenant_id, st.id).await.unwrap();
            state.services.sandboxes = Some(Arc::new(dir));
            let app = routes(state);
            assert_eq!(
                ask(&app, &q).await,
                (
                    StatusCode::OK,
                    Some(UpstreamAnswer::Ready {
                        url: "http://10.89.0.7:3000".into()
                    })
                )
            );
            assert!(
                in_request.load(Ordering::SeqCst),
                "the upstream route runs outside with_request_freshness"
            );
            // Another tenant, a forged deployment, the other environment: no upstream.
            for bad in [
                UpstreamQuery {
                    tenant_id: s.other_tenant,
                    ..q.clone()
                },
                UpstreamQuery {
                    deployment_id: uuid::Uuid::now_v7(),
                    ..q.clone()
                },
                UpstreamQuery {
                    environment: AppEnv::Live,
                    ..q.clone()
                },
            ] {
                assert_eq!(
                    ask(&app, &bad).await,
                    (StatusCode::OK, Some(UpstreamAnswer::NotRunning)),
                    "{bad:?}"
                );
            }
        }

        #[tokio::test]
        async fn the_deployment_route_says_whether_it_is_a_sandbox() {
            let state = AppState::in_memory();
            let node = seed(&state, "node22").await.query;
            let stat = seed(&state, "static").await.query;
            for (q, want) in [(node, true), (stat, false)] {
                let info = gateway::deployment_info(&state, q.tenant_id, q.app_id, q.deployment_id)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(info.sandbox, want);
            }
        }
    }

    #[test]
    fn the_content_host_comes_from_the_url() {
        let c = TlsAskConfig::for_content_url("https://Content.Staging.ruumapps.net/", 5);
        assert_eq!(c.content_host, "content.staging.ruumapps.net");
    }
}
