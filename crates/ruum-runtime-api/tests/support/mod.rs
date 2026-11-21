//! The T-6 world of the runtime API (card R-3; `crates/ruum-testkit/src/gateway/`), shared by
//! the tests of this crate.
//!
//! - The core: `ruum-api` with in-memory services. Its public router (the REST API, for
//!   `rest_api_status`) and its internal router (the runtime API's internal routes) listen on
//!   127.0.0.1, so the runtime API and the world talk to them over HTTP, as in production.
//! - The runtime API: [`ruum_runtime_api::serve`] on its own 127.0.0.1 listener, with the
//!   workload key set of the core's issuer ([`WorkloadTokenIssuer`], which the core also holds
//!   in `Services::workload_tokens` to check tokens and sign delegated tokens) and the gateway's
//!   user-token key set (the gateway's own [`UserTokenSigner`] with the world's key).
//! - Seeding goes through the core's real paths: team tenants with exact slugs, apps through
//!   `createApp`, deployments through `createAppDeployment` of a folder with `app.yaml` and
//!   `publishAppDeployment` (so the bindings are B-1's: the deployer check, rows and tuples), a
//!   second ready preview after the publish. Every app declares the roles `member` (default)
//!   and `approver`. A binding the core refuses (another tenant's node, an API binding before
//!   B-3) comes back as `WorldError::Failed("refused: <code> <hint>")` with the core's own
//!   answer.
//! - Workload tokens come from the core's issuer; user tokens from the gateway's signer, for a
//!   guest of the tenant with `edit` on the app node (its pairwise subject made with the
//!   tenant's key, as the core's `session` route makes it).

#![allow(dead_code)]

use async_trait::async_trait;
use bytes::Bytes;
use ruum_api::AppState;
use ruum_core::actor::{Actor, Context};
use ruum_core::apps::deployments::{CreateDeployment, PublishDeployment, Published, SourceRequest};
use ruum_core::apps::store::DeploymentState;
use ruum_core::apps::workload_tokens::WorkloadTokenIssuer;
use ruum_core::authz::{ObjectRef, Relationship, RelationshipUpdate, Role, SubjectRef};
use ruum_core::identity::VerifiedLogin;
use ruum_core::identity::app_sign_in::pairwise;
use ruum_core::nodes::{NewNode, NodeKind, OnConflict};
use ruum_core::runtime::{Environment, WorkloadIdentity};
use ruum_core::{NodeId, TenantId, UserId};
use ruum_gateway::token::{TokenSubject, UserTokenSigner, live_aud, preview_aud};
use ruum_runtime_api::caller::CallerNetwork;
use ruum_runtime_api::core_api::HttpCore;
use ruum_runtime_api::limits::Caps;
use ruum_runtime_api::token::KeySet;
use ruum_runtime_api::{Config, RuntimeApi};
use ruum_testkit::gateway::{
    Binding, BindingKind, Env, MakeRuntimeWorld, Req, Resp, RtApp, RuntimeApiWorld, RuntimeHandle,
    Secret, WorldError, WorldResult,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

pub const ISSUER: &str = "https://ruum.localhost";
pub const INTERNAL_HOST: &str = "ruum.internal";
const INDEX: &[u8] = b"<!doctype html><title>t6</title>";

pub fn failed(what: &str, e: impl std::fmt::Display) -> WorldError {
    WorldError::Failed(format!("{what}: {e}"))
}

/// The core's refusal of a step, passed through (`refused: <code> <hint>`).
fn refused(e: ruum_core::Error) -> WorldError {
    match e {
        ruum_core::Error::Api(a) => WorldError::Failed(format!(
            "refused: {} {}",
            a.code.as_str(),
            a.hint.unwrap_or_default()
        )),
        other => failed("the core", other),
    }
}

/// A seeded app.
#[derive(Clone, Debug)]
pub struct AppSeed {
    pub tenant: TenantId,
    pub tenant_slug: String,
    pub owner: UserId,
    pub app: NodeId,
    pub slug: String,
    pub live: Uuid,
    pub preview: Uuid,
}

#[derive(Default)]
pub struct Seeds {
    /// tenant slug -> (tenant, owner)
    tenants: HashMap<String, (TenantId, UserId)>,
    /// app ID -> the seed
    pub apps: HashMap<String, AppSeed>,
    /// app ID -> the URL of its upstream (G-3)
    upstreams: HashMap<String, String>,
    count: u64,
}

/// Options of a world.
#[derive(Clone, Default)]
pub struct Opts {
    pub caps: Option<Caps>,
    pub network: Option<Arc<dyn CallerNetwork>>,
}

pub struct World {
    pub state: AppState,
    pub issuer: Arc<WorkloadTokenIssuer>,
    pub users: Arc<UserTokenSigner>,
    pub rt: Arc<RuntimeApi>,
    pub runtime: SocketAddr,
    pub platform: SocketAddr,
    pub internal: SocketAddr,
    pub http: reqwest::Client,
    pub seeds: Mutex<Seeds>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl Drop for World {
    fn drop(&mut self) {
        for t in self.tasks.lock().expect("tasks").drain(..) {
            t.abort();
        }
    }
}

async fn listen() -> tokio::net::TcpListener {
    tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind 127.0.0.1")
}

/// Makes a fresh world for each case.
pub fn make() -> MakeRuntimeWorld {
    Arc::new(|| {
        Box::pin(async {
            let w = World::new().await?;
            Ok(Arc::new(w) as Arc<dyn RuntimeApiWorld>)
        })
    })
}

/// The manifest of a seeded app with `bindings` (YAML lines) and `extra` (more top-level YAML).
pub fn manifest(bindings: &[String], extra: &str) -> String {
    let mut m = String::from(
        "runtime: static\nroles:\n  member:\n    description: Member\n    default: true\n  \
         approver:\n    description: Approver\n",
    );
    if !bindings.is_empty() {
        m.push_str("bindings:\n");
        for b in bindings {
            m.push_str(b);
        }
    }
    m.push_str(extra);
    m
}

impl World {
    pub async fn new() -> WorldResult<World> {
        Self::with(Opts::default()).await
    }

    pub async fn with(opts: Opts) -> WorldResult<World> {
        let issuer = Arc::new(WorkloadTokenIssuer::generate(ISSUER));
        let mut state = AppState::in_memory();
        state.services.workload_tokens = Some(issuer.clone());
        let platform_l = listen().await;
        let internal_l = listen().await;
        let runtime_l = listen().await;
        let platform = platform_l.local_addr().expect("addr");
        let internal = internal_l.local_addr().expect("addr");
        let runtime = runtime_l.local_addr().expect("addr");
        let api = ruum_api::router(state.clone());
        let internal_router = ruum_api::internal::router(
            state.clone(),
            ruum_api::internal::TlsAskConfig::for_content_url("https://content.ruumapps.net", 20),
        );
        let users = Arc::new(UserTokenSigner::generate(ISSUER));
        let mut config = Config::new(ISSUER, INTERNAL_HOST);
        if let Some(c) = opts.caps {
            config.caps = c;
        }
        let core = HttpCore::new(&format!("http://{internal}")).map_err(|e| failed("core", e))?;
        let mut rt = RuntimeApi::new(
            config,
            KeySet::from_jwks(&issuer.jwks()).map_err(|e| failed("workload keys", e))?,
            KeySet::from_jwks(&users.jwks()).map_err(|e| failed("user keys", e))?,
            Arc::new(core),
        );
        if let Some(n) = opts.network {
            rt = rt.with_network(n);
        }
        let rt = Arc::new(rt);
        let mut tasks = Vec::new();
        tasks.push(tokio::spawn(async move {
            let _ = axum::serve(
                platform_l,
                api.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await;
        }));
        tasks.push(tokio::spawn(async move {
            let _ = axum::serve(internal_l, internal_router.into_make_service()).await;
        }));
        let r = rt.clone();
        tasks.push(tokio::spawn(async move {
            let _ = ruum_runtime_api::serve(runtime_l, r).await;
        }));
        Ok(World {
            state,
            issuer,
            users,
            rt,
            runtime,
            platform,
            internal,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("client"),
            seeds: Mutex::new(Seeds::default()),
            tasks: Mutex::new(tasks),
        })
    }

    pub fn next(&self) -> u64 {
        let mut s = self.seeds.lock().expect("seeds");
        s.count += 1;
        s.count
    }

    pub fn ctx(tenant: TenantId, user: UserId) -> Context {
        Context {
            tenant,
            actor: Actor::user(user),
            scope: None,
            idempotency_key: None,
        }
    }

    pub async fn person(&self, label: &str) -> WorldResult<UserId> {
        let n = self.next();
        let email = format!("r3-{label}-{n}-{}@example.test", Uuid::now_v7().simple());
        let user = self
            .state
            .services
            .identity
            .upsert_from_login(&VerifiedLogin {
                issuer: "https://issuer.r3.test".into(),
                subject: email.clone(),
                email: Some(email),
                email_verified: true,
                name: Some(label.into()),
                id_token: None,
            })
            .await
            .map_err(|e| failed("a person", e))?;
        Ok(user.id)
    }

    /// The team tenant of `slug` (made once), with its owner.
    pub async fn tenant(&self, slug: &str) -> WorldResult<(TenantId, UserId)> {
        if let Some(t) = self.seeds.lock().expect("seeds").tenants.get(slug) {
            return Ok(*t);
        }
        let owner = self.person("owner").await?;
        let t = self
            .state
            .services
            .tenants
            .create_team(&Actor::user(owner), slug.to_string())
            .await
            .map_err(|e| failed("a team tenant", e))?;
        self.state
            .services
            .apps
            .store()
            .ensure_tenant_settings(t.id, &[slug.to_string()])
            .await
            .map_err(|e| failed("the tenant slug", e))?;
        self.seeds
            .lock()
            .expect("seeds")
            .tenants
            .insert(slug.to_string(), (t.id, owner));
        Ok((t.id, owner))
    }

    pub async fn root(&self, tenant: TenantId) -> WorldResult<NodeId> {
        Ok(self
            .state
            .services
            .tenants
            .find(tenant)
            .await
            .map_err(|e| failed("the tenant", e))?
            .root_node_id)
    }

    pub async fn node(
        &self,
        c: &Context,
        parent: NodeId,
        name: &str,
        kind: NodeKind,
    ) -> WorldResult<NodeId> {
        Ok(self
            .state
            .services
            .nodes
            .create(
                c,
                NewNode {
                    parent_id: parent,
                    name: name.into(),
                    kind,
                    node_type: None,
                    link: None,
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .map_err(|e| failed("a node", e))?
            .value
            .id)
    }

    pub async fn file(
        &self,
        c: &Context,
        parent: NodeId,
        name: &str,
        data: &[u8],
    ) -> WorldResult<NodeId> {
        let id = self.node(c, parent, name, NodeKind::File).await?;
        self.state
            .services
            .transfers
            .put_content(c, id, Bytes::copy_from_slice(data), None, None)
            .await
            .map_err(|e| failed("the content", e))?;
        Ok(id)
    }

    /// A ready deployment of `manifest` for `app` (a preview), with an optional draft label.
    pub async fn deploy(
        &self,
        c: &Context,
        app: NodeId,
        manifest: &str,
        draft: Option<&str>,
    ) -> WorldResult<Uuid> {
        let root = self.root(c.tenant).await?;
        let n = self.next();
        let site = self
            .node(c, root, &format!("site {n}"), NodeKind::Folder)
            .await?;
        self.file(c, site, "app.yaml", manifest.as_bytes()).await?;
        self.file(c, site, "index.html", INDEX).await?;
        self.file(c, site, "events.yaml", b"asyncapi: 3.0.0\n")
            .await?;
        let d = self
            .state
            .services
            .apps
            .create_deployment(
                c,
                app,
                CreateDeployment {
                    source: SourceRequest {
                        kind: "folder".into(),
                        node_id: site,
                        version_id: None,
                    },
                    label: None,
                    draft: draft.map(str::to_string),
                },
            )
            .await
            .map_err(refused)?;
        for _ in 0..200 {
            let got = self
                .state
                .services
                .apps
                .get_deployment(c, app, d.id, Duration::from_secs(5))
                .await
                .map_err(|e| failed("the deployment", e))?;
            if !got.state.is_moving() {
                if got.state != DeploymentState::Ready {
                    return Err(WorldError::Failed(format!(
                        "the deployment ended {}: {:?}",
                        got.state.as_str(),
                        got.stages
                    )));
                }
                return Ok(d.id);
            }
        }
        Err(WorldError::Failed("the deployment keeps moving".into()))
    }

    /// Publishes `dep` as the live deployment of `app`; returns the live deployment.
    pub async fn publish(&self, c: &Context, app: NodeId, dep: Uuid) -> WorldResult<Uuid> {
        match self
            .state
            .services
            .apps
            .publish_deployment(c, app, dep, PublishDeployment::default(), ISSUER)
            .await
            .map_err(refused)?
        {
            Published::Live(d) => Ok(d.id),
            Published::ApprovalRequired(_) => Err(WorldError::Failed("an approval".into())),
        }
    }

    /// The binding lines of `bindings`, making their targets in the tenant root.
    async fn binding_lines(
        &self,
        tenant_slug: &str,
        c: &Context,
        bindings: &[Binding],
    ) -> WorldResult<(Vec<String>, Vec<(String, String)>)> {
        let root = self.root(c.tenant).await?;
        let mut lines = Vec::new();
        let mut bound = Vec::new();
        for b in bindings {
            let n = self.next();
            let obo = if b.on_behalf_of_user {
                "    on_behalf_of_user: true\n"
            } else {
                ""
            };
            match &b.kind {
                BindingKind::File(data) => {
                    let id = self.file(c, root, &format!("{} {n}", b.name), data).await?;
                    lines.push(format!(
                        "  {}:\n    node: \"node:{id}\"\n    role: view\n{obo}",
                        b.name
                    ));
                    bound.push((b.name.clone(), id.to_string()));
                }
                BindingKind::Folder(names) => {
                    let f = self
                        .node(c, root, &format!("{} {n}", b.name), NodeKind::Folder)
                        .await?;
                    for name in names {
                        self.file(c, f, name, format!("T6-{name}").as_bytes())
                            .await?;
                    }
                    lines.push(format!(
                        "  {}:\n    node: \"node:{f}\"\n    role: view\n{obo}",
                        b.name
                    ));
                    bound.push((b.name.clone(), f.to_string()));
                }
                BindingKind::App {
                    app_slug,
                    api,
                    tenant_slug: other,
                } => {
                    let target_tenant = other.clone().unwrap_or_else(|| tenant_slug.to_string());
                    let target = self
                        .seeds
                        .lock()
                        .expect("seeds")
                        .apps
                        .iter()
                        .find(|(_, s)| s.tenant_slug == target_tenant && s.slug == *app_slug)
                        .map(|(id, _)| id.clone())
                        .ok_or_else(|| WorldError::Failed("an unknown called app".into()))?;
                    let delegate = if b.delegate {
                        "    delegate: true\n"
                    } else {
                        ""
                    };
                    lines.push(format!(
                        "  {}:\n    node: \"node:{target}\"\n    api: {api}\n    scopes: [\"read\"]\n{delegate}",
                        b.name
                    ));
                }
            }
        }
        Ok((lines, bound))
    }

    /// Seeds an app of `tenant_slug` with a live and a preview deployment of `manifest`.
    pub async fn seed_with(
        &self,
        tenant_slug: &str,
        app_slug: &str,
        bindings: &[Binding],
        extra: &str,
    ) -> WorldResult<RtApp> {
        let (tenant, owner) = self.tenant(tenant_slug).await?;
        let c = Self::ctx(tenant, owner);
        let root = self.root(tenant).await?;
        let app = self
            .state
            .services
            .apps
            .create(
                &c,
                ruum_core::apps::CreateApp {
                    parent_id: root,
                    name: app_slug.to_string(),
                    settings: None,
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .map_err(|e| failed("createApp", e))?
            .value;
        let (lines, bound) = self.binding_lines(tenant_slug, &c, bindings).await?;
        let m = manifest(&lines, extra);
        let first = self.deploy(&c, app.node.id, &m, None).await?;
        let live = self.publish(&c, app.node.id, first).await?;
        let preview = self.deploy(&c, app.node.id, &m, None).await?;
        self.seeds.lock().expect("seeds").apps.insert(
            app.node.id.to_string(),
            AppSeed {
                tenant,
                tenant_slug: tenant_slug.to_string(),
                owner,
                app: app.node.id,
                slug: app.slug.clone(),
                live,
                preview,
            },
        );
        Ok(RtApp {
            id: app.node.id.to_string(),
            slug: app.slug.clone(),
            tenant_slug: tenant_slug.to_string(),
            preview_id: preview.to_string(),
            bound_nodes: bound,
        })
    }

    pub fn seed(&self, app: &RtApp) -> WorldResult<AppSeed> {
        self.seeds
            .lock()
            .expect("seeds")
            .apps
            .get(&app.id)
            .cloned()
            .ok_or_else(|| WorldError::Failed("an unknown app".into()))
    }

    pub fn identity(seed: &AppSeed, env: Env) -> WorkloadIdentity {
        WorkloadIdentity {
            tenant: seed.tenant,
            app: seed.app,
            deployment: match env {
                Env::Live => seed.live,
                Env::Preview => seed.preview,
            },
            environment: match env {
                Env::Live => Environment::Live,
                Env::Preview => Environment::Preview,
            },
        }
    }

    /// A guest of the app's tenant with `edit` on the app node.
    pub async fn visitor(&self, seed: &AppSeed) -> WorldResult<UserId> {
        let user = self.person("visitor").await?;
        self.state
            .services
            .tenants
            .add_guest_from_email_grant(seed.tenant, user, serde_json::json!({ "via": "r3" }))
            .await
            .map_err(|e| failed("a guest", e))?;
        self.grant(user, seed.app, Role::Edit).await?;
        Ok(user)
    }

    pub async fn grant(&self, user: UserId, node: NodeId, role: Role) -> WorldResult<()> {
        self.write(RelationshipUpdate::Touch(Relationship {
            object: ObjectRef::node(node),
            relation: role.relation().into(),
            subject: SubjectRef::user(user),
        }))
        .await
    }

    pub async fn revoke(&self, user: UserId, node: NodeId, role: Role) -> WorldResult<()> {
        self.write(RelationshipUpdate::Delete(Relationship {
            object: ObjectRef::node(node),
            relation: role.relation().into(),
            subject: SubjectRef::user(user),
        }))
        .await
    }

    /// Writes (or deletes) the binding tuple `node:N#viewer@app:<principal>` by hand, without a
    /// binding row (tests of the row and tuple rule).
    pub async fn app_tuple(&self, principal: Uuid, node: NodeId, add: bool) -> WorldResult<()> {
        let r = Relationship {
            object: ObjectRef::node(node),
            relation: Role::View.relation().into(),
            subject: SubjectRef::object(ObjectRef::new("app", principal)),
        };
        self.write(if add {
            RelationshipUpdate::Touch(r)
        } else {
            RelationshipUpdate::Delete(r)
        })
        .await
    }

    async fn write(&self, u: RelationshipUpdate) -> WorldResult<()> {
        self.state
            .services
            .authz
            .write(vec![u])
            .await
            .map(|_| ())
            .map_err(|e| failed("a grant", e))
    }

    /// The pairwise subject of `user` for the app (as the core's `session` route makes it).
    pub async fn subject(&self, seed: &AppSeed, user: UserId) -> WorldResult<String> {
        let key = self
            .state
            .services
            .tenant_keys
            .tenant_key(seed.tenant)
            .await
            .map_err(|e| failed("the tenant key", e))?;
        Ok(pairwise(key.as_bytes(), seed.app, user.0))
    }

    /// A user token of `user` for the app's `env`, with `actor`.
    pub async fn user_token_of(
        &self,
        seed: &AppSeed,
        env: Env,
        user: UserId,
        actor: &str,
    ) -> WorldResult<Secret> {
        let actor: &'static str = match actor {
            "user" => "user",
            "agent" => "agent",
            "app" => "app",
            "partner" => "partner",
            "anonymous" => "anonymous",
            _ => "robot",
        };
        let roles = self
            .state
            .services
            .apps
            .effective_roles(seed.tenant, seed.app, Some(user))
            .await
            .map_err(|e| failed("the roles", e))?;
        let subject = TokenSubject {
            sub: self.subject(seed, user).await?,
            actor,
            on_behalf_of: None,
            roles,
        };
        let (aud, env) = match env {
            Env::Live => (live_aud(seed.app), "live"),
            Env::Preview => (preview_aud(seed.app, seed.preview), "preview"),
        };
        let claims = self.users.claims(aud, env, &seed.tenant_slug, &subject);
        self.users
            .sign(&claims)
            .map(Secret)
            .map_err(|e| failed("a user token", e))
    }

    /// `POST /v1/token/exchange` with a workload token.
    pub async fn exchange(&self, workload: &str, user_token: &str) -> WorldResult<Resp> {
        self.send(
            Req::post(INTERNAL_HOST, "/v1/token/exchange")
                .header("Authorization", &format!("Bearer {workload}"))
                .json(&serde_json::json!({ "user_token": user_token })),
        )
        .await
    }

    /// An API token of the REST API (an agent token of `user` in `tenant`).
    pub async fn api_token(&self, tenant: TenantId, user: UserId) -> WorldResult<Secret> {
        use ruum_core::agents::{AgentScope, NewAgent};
        use ruum_core::identity::{AccessClaims, TokenType};
        let n = self.next();
        let agent = self
            .state
            .services
            .agents
            .register(
                &Self::ctx(tenant, user),
                NewAgent {
                    name: format!("r3 agent {n}"),
                    description: None,
                    scope: AgentScope {
                        subtree: None,
                        max_role: Role::Manage,
                        expires_at: None,
                        budget: Default::default(),
                    },
                },
            )
            .await
            .map_err(|e| failed("an agent", e))?;
        let tokens = &self.state.services.tokens;
        let mut c = AccessClaims::new(tokens.as_ref(), TokenType::Agent, agent.id.0, 300);
        c.tid = Some(tenant);
        c.agent = Some(agent.id);
        c.obo = Some(user);
        tokens
            .issue(&c)
            .map(Secret)
            .map_err(|e| failed("an agent token", e))
    }

    /// Runs one round of the provider hook deliveries (the server's worker runs every 5 s).
    pub async fn deliver(&self) -> WorldResult<()> {
        use ruum_core::providers::deliveries::{Backoff, DeliveryWorker};
        DeliveryWorker::new(
            self.state.services.typed_nodes.clone(),
            self.state.services.providers.clone(),
            Backoff::default(),
        )
        .run_once(100)
        .await
        .map(|_| ())
        .map_err(|e| failed("the deliveries", e))
    }

    /// A second runtime API process of this world: the same keys and the same core, but its
    /// own replay set and caps (a rolling deploy, or a restart). Its address.
    pub async fn second_runtime(&self) -> WorldResult<SocketAddr> {
        let core =
            HttpCore::new(&format!("http://{}", self.internal)).map_err(|e| failed("core", e))?;
        let rt = Arc::new(RuntimeApi::new(
            Config::new(ISSUER, INTERNAL_HOST),
            KeySet::from_jwks(&self.issuer.jwks()).map_err(|e| failed("workload keys", e))?,
            KeySet::from_jwks(&self.users.jwks()).map_err(|e| failed("user keys", e))?,
            Arc::new(core),
        ));
        let l = listen().await;
        let addr = l.local_addr().map_err(|e| failed("addr", e))?;
        self.tasks
            .lock()
            .expect("tasks")
            .push(tokio::spawn(async move {
                let _ = ruum_runtime_api::serve(l, rt).await;
            }));
        Ok(addr)
    }

    /// `POST /v1/token/exchange` at the runtime API process at `addr`.
    pub async fn exchange_at(
        &self,
        addr: SocketAddr,
        workload: &str,
        user_token: &str,
    ) -> WorldResult<Resp> {
        let req = Req::post(INTERNAL_HOST, "/v1/token/exchange")
            .header("Authorization", &format!("Bearer {workload}"))
            .json(&serde_json::json!({ "user_token": user_token }));
        ruum_testkit::gateway::http::send(addr, &req, Duration::from_secs(20))
            .await
            .map_err(|e| failed("a request to the runtime API", e))
    }

    /// Sends a request to the runtime API.
    pub async fn send(&self, req: Req) -> WorldResult<Resp> {
        ruum_testkit::gateway::http::send(self.runtime, &req, Duration::from_secs(20))
            .await
            .map_err(|e| failed("a request to the runtime API", e))
    }

    /// A workload token of `app` in `tenant` for `deployment` (live or preview).
    pub fn token_for(&self, seed: &AppSeed, deployment: Uuid, env: Env) -> String {
        let id = WorkloadIdentity {
            tenant: seed.tenant,
            app: seed.app,
            deployment,
            environment: match env {
                Env::Live => Environment::Live,
                Env::Preview => Environment::Preview,
            },
        };
        self.issuer
            .issue(&id)
            .expect("a workload token")
            .expose()
            .to_string()
    }

    /// `POST <route>` at the core's internal listener (the runtime API's routes), with `body`
    /// and an optional bearer: (status, body).
    pub async fn core_route(
        &self,
        route: &str,
        bearer: Option<&str>,
        body: &serde_json::Value,
    ) -> (u16, String) {
        let mut r = self
            .http
            .post(format!(
                "http://{}{}{route}",
                self.internal,
                ruum_core::apps::runtime_api::wire::PREFIX
            ))
            .json(body);
        if let Some(b) = bearer {
            r = r.bearer_auth(b);
        }
        let resp = r.send().await.expect("the core's internal listener");
        let status = resp.status().as_u16();
        (status, resp.text().await.unwrap_or_default())
    }

    /// `GET path` at the runtime API with a bearer.
    pub async fn get(&self, path: &str, bearer: &str) -> WorldResult<Resp> {
        self.send(
            Req::get(INTERNAL_HOST, path).header("Authorization", &format!("Bearer {bearer}")),
        )
        .await
    }
}

#[async_trait]
impl RuntimeApiWorld for World {
    async fn start(&self) -> WorldResult<RuntimeHandle> {
        Ok(RuntimeHandle {
            addr: self.runtime,
            internal_host: INTERNAL_HOST.into(),
            issuer: ISSUER.into(),
            // The core signs delegated tokens with the workload key.
            delegated_jwks: self.issuer.jwks(),
            user_token_jwks: self.users.jwks(),
        })
    }

    async fn seed_app(
        &self,
        tenant_slug: &str,
        app_slug: &str,
        bindings: &[Binding],
    ) -> WorldResult<RtApp> {
        self.seed_with(tenant_slug, app_slug, bindings, "").await
    }

    async fn workload_token(&self, app: &RtApp, env: Env) -> WorldResult<Secret> {
        let seed = self.seed(app)?;
        let t = self
            .issuer
            .issue(&Self::identity(&seed, env))
            .map_err(|e| failed("a workload token", e))?;
        Ok(Secret(t.expose().to_string()))
    }

    async fn user_token(&self, app: &RtApp, env: Env, actor: &str) -> WorldResult<Secret> {
        let seed = self.seed(app)?;
        let user = self.visitor(&seed).await?;
        self.user_token_of(&seed, env, user, actor).await
    }

    async fn set_upstream(&self, app: &RtApp, url: &str) -> WorldResult<()> {
        self.seeds
            .lock()
            .expect("seeds")
            .upstreams
            .insert(app.id.clone(), url.to_string());
        Ok(())
    }

    async fn seed_unbound_node(&self, tenant_slug: &str) -> WorldResult<String> {
        let (tenant, owner) = self.tenant(tenant_slug).await?;
        let c = Self::ctx(tenant, owner);
        let root = self.root(tenant).await?;
        let n = self.next();
        let id = self
            .file(&c, root, &format!("unbound {n}"), b"T6-UNBOUND")
            .await?;
        Ok(id.to_string())
    }

    async fn rest_api_status(&self, bearer: &str) -> WorldResult<u16> {
        let r = self
            .http
            .get(format!("http://{}/v1/me", self.platform))
            .header("authorization", format!("Bearer {bearer}"))
            .send()
            .await
            .map_err(|e| failed("GET /v1/me", e.without_url()))?;
        Ok(r.status().as_u16())
    }
}
