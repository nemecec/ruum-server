//! The T-6 world of the gateway (card G-1a; `crates/ruum-testkit/src/gateway/`, plan seam 4): it
//! starts `ruum-gateway` in process against the in-memory core and runs every T-6 case.
//!
//! - The core: `ruum-api` with in-memory services. Its public router (the platform side of the
//!   sign-in, `GET /v1/me`) and its internal router (the gateway's internal API) listen on
//!   127.0.0.1, so the gateway and the world talk to them over HTTP, as in production.
//! - The gateway: [`ruum_gateway::Gateway`] on its own 127.0.0.1 listener, with the in-memory
//!   host lookup (the world records each host as the SQL function would answer it; the function
//!   itself has its own Postgres tests in `tests/host_lookup_pg.rs`), the in-memory static sites
//!   and the core's internal API.
//! - Seeding goes through the core's stores: team tenants with exact slugs, people with a platform
//!   account (guests, without any right on an app), apps through `createApp`, deployments through
//!   the store, grants as tuples of the graph, agents through the agent store.
//! - The world tells the gateway when a host changes (a new live deployment, a reused host), as
//!   the journal feed does in production.

use async_trait::async_trait;
use bytes::Bytes;
use ruum_api::AppState;
use ruum_core::actor::{Actor, Context};
use ruum_core::agents::{AgentScope, NewAgent};
use ruum_core::apps::static_site::StaticSites;
use ruum_core::apps::store::{
    ApprovalChange, Archive, DeploymentSource, DeploymentState, Environment, NewDeployment,
};
use ruum_core::authz::{ObjectRef, Relationship, RelationshipUpdate, Role, SubjectRef};
use ruum_core::identity::{APPS_AGENT_AUDIENCE, AccessClaims, TokenType, VerifiedLogin};
use ruum_core::nodes::OnConflict;
use ruum_core::{NodeId, TenantId, UserId};
use ruum_gateway::host::{AppHost, MemoryHostLookup, Resolved};
use ruum_gateway::token::UserTokenSigner;
use ruum_gateway::{Config, Gateway};
use ruum_testkit::gateway::{
    self, Access, AgentAudience, App, Deployment, Env, GatewayHandle, GatewayWorld,
    MakeGatewayWorld, Relation, Req, Secret, Tenant, User, WorldError, WorldResult,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

const APPS_DOMAIN: &str = "ruumapps.net";
const ISSUER: &str = "https://ruum.localhost";

fn failed(what: &str, e: impl std::fmt::Display) -> WorldError {
    WorldError::Failed(format!("{what}: {e}"))
}

/// A deployment seeded but not yet in the store (a live one goes there at `set_live`).
#[derive(Clone)]
struct Seeded {
    tenant: TenantId,
    tenant_slug: String,
    app: NodeId,
    owner: UserId,
    short_id: String,
}

#[derive(Default)]
struct Seeds {
    /// tenant ID -> (slug, owner)
    tenants: HashMap<TenantId, (String, UserId)>,
    /// user ID -> (platform session ID, secret)
    sessions: HashMap<UserId, (Uuid, String)>,
    deployments: HashMap<Uuid, Seeded>,
    /// app ID -> slug the host of the app has now
    app_slugs: HashMap<NodeId, String>,
    count: u64,
}

struct World {
    state: AppState,
    hosts: Arc<MemoryHostLookup>,
    sites: StaticSites,
    gw: Arc<Gateway>,
    platform: SocketAddr,
    gateway: SocketAddr,
    http: reqwest::Client,
    seeds: Mutex<Seeds>,
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

impl World {
    async fn new() -> WorldResult<World> {
        let state = AppState::in_memory();
        let platform_l = listen().await;
        let internal_l = listen().await;
        let gateway_l = listen().await;
        let platform = platform_l.local_addr().expect("addr");
        let internal = internal_l.local_addr().expect("addr");
        let gateway = gateway_l.local_addr().expect("addr");
        let api = ruum_api::router(state.clone());
        let internal_router = ruum_api::internal::router(
            state.clone(),
            ruum_api::internal::TlsAskConfig::for_content_url("https://content.ruumapps.net", 20),
        );
        let hosts = Arc::new(MemoryHostLookup::default());
        let sites = StaticSites::in_memory();
        let core = ruum_gateway::core_api::HttpCore::new(&format!("http://{internal}"))
            .map_err(|e| failed("the core client", e))?;
        let gw = Arc::new(Gateway::new(
            Config {
                apps_domain: APPS_DOMAIN.into(),
                platform_url: format!("http://{platform}"),
                ui_origin: ISSUER.into(),
            },
            hosts.clone(),
            Arc::new(core),
            UserTokenSigner::generate(ISSUER),
            sites.clone(),
        ));
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
        let g = gw.clone();
        tasks.push(tokio::spawn(async move {
            let _ = ruum_gateway::serve(gateway_l, g).await;
        }));
        Ok(World {
            state,
            hosts,
            sites,
            gw,
            platform,
            gateway,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("client"),
            seeds: Mutex::new(Seeds::default()),
            tasks: Mutex::new(tasks),
        })
    }

    fn next(&self) -> u64 {
        let mut s = self.seeds.lock().expect("seeds");
        s.count += 1;
        s.count
    }

    async fn person(&self, label: &str) -> WorldResult<UserId> {
        let n = self.next();
        let email = format!("t6-{label}-{n}-{}@example.test", Uuid::now_v7().simple());
        let user = self
            .state
            .services
            .identity
            .upsert_from_login(&VerifiedLogin {
                issuer: "https://issuer.t6.test".into(),
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

    fn owner_ctx(&self, tenant: TenantId) -> WorldResult<Context> {
        let owner = self
            .seeds
            .lock()
            .expect("seeds")
            .tenants
            .get(&tenant)
            .map(|t| t.1)
            .ok_or_else(|| WorldError::Failed("an unknown tenant".into()))?;
        Ok(Context {
            tenant,
            actor: Actor::user(owner),
            scope: None,
            idempotency_key: None,
        })
    }

    fn tenant_id(t: &Tenant) -> WorldResult<TenantId> {
        t.id.parse().map_err(|e| failed("a tenant ID", e))
    }

    fn node_id(id: &str) -> WorldResult<NodeId> {
        id.parse().map_err(|e| failed("an app ID", e))
    }

    fn user_id(u: &User) -> WorldResult<UserId> {
        u.id.parse().map_err(|e| failed("a user ID", e))
    }

    async fn write(&self, updates: Vec<RelationshipUpdate>) -> WorldResult<()> {
        self.state
            .services
            .authz
            .write(updates)
            .await
            .map(|_| ())
            .map_err(|e| failed("a grant", e))
    }

    fn relation(user: UserId, app: NodeId, relation: &Relation) -> WorldResult<Relationship> {
        let rel = match relation {
            Relation::View => Role::View.relation(),
            Relation::Edit => Role::Edit.relation(),
            Relation::Role(_) => {
                return Err(WorldError::Pending {
                    card: "B-2".into(),
                    why: "app role grants come with card B-2 (apps::roles)".into(),
                });
            }
        };
        Ok(Relationship {
            object: ObjectRef::node(app),
            relation: rel.into(),
            subject: SubjectRef::user(user),
        })
    }

    /// A platform session of `user` (made once), as a sign-in to Ruum would make it.
    async fn platform_session(&self, user: UserId) -> WorldResult<String> {
        if let Some((_, s)) = self.seeds.lock().expect("seeds").sessions.get(&user) {
            return Ok(s.clone());
        }
        let (session, secret) = self
            .state
            .services
            .identity
            .create_session(user, None)
            .await
            .map_err(|e| failed("a platform session", e))?;
        self.seeds
            .lock()
            .expect("seeds")
            .sessions
            .insert(user, (session.id, secret.clone()));
        Ok(secret)
    }

    async fn gateway_get(
        &self,
        host: &str,
        target: &str,
        cookie: Option<&str>,
    ) -> WorldResult<gateway::Resp> {
        let mut req = Req::get(host, target);
        if let Some(c) = cookie {
            req = req.header("Cookie", c);
        }
        gateway::http::send(self.gateway, &req, Duration::from_secs(20))
            .await
            .map_err(|e| failed("a request to the gateway", e))
    }
}

/// The first `Set-Cookie` value of `name` in `resp`.
fn cookie_value(resp: &gateway::Resp, name: &str) -> Option<String> {
    resp.set_cookies()
        .iter()
        .filter_map(|c| gateway::SetCookie::parse(c))
        .find(|c| c.name == name)
        .map(|c| c.value)
}

#[async_trait]
impl GatewayWorld for World {
    async fn start(&self) -> WorldResult<GatewayHandle> {
        Ok(GatewayHandle {
            addr: self.gateway,
            apps_domain: APPS_DOMAIN.into(),
            issuer: ISSUER.into(),
            user_token_jwks: self.gw.tokens.jwks(),
            ui_origin: ISSUER.into(),
            platform_host: "ruum.localhost".into(),
        })
    }

    async fn seed_tenant(&self, slug: &str) -> WorldResult<Tenant> {
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
            .insert(t.id, (slug.to_string(), owner));
        Ok(Tenant {
            id: t.id.to_string(),
            slug: slug.to_string(),
        })
    }

    async fn seed_user(&self, tenant: &Tenant, name: &str) -> WorldResult<User> {
        let user = self.person(name).await?;
        let tid = Self::tenant_id(tenant)?;
        // A guest: a platform account in the tenant, without any right on its content.
        self.state
            .services
            .tenants
            .add_guest_from_email_grant(tid, user, serde_json::json!({ "via": "t6" }))
            .await
            .map_err(|e| failed("a guest", e))?;
        Ok(User {
            id: user.to_string(),
            name: name.to_string(),
            tenant: tenant.clone(),
        })
    }

    async fn seed_app(&self, tenant: &Tenant, slug: &str, access: Access) -> WorldResult<App> {
        let tid = Self::tenant_id(tenant)?;
        let ctx = self.owner_ctx(tid)?;
        let root = self
            .state
            .services
            .tenants
            .find(tid)
            .await
            .map_err(|e| failed("the tenant", e))?
            .root_node_id;
        let settings = ruum_core::apps::settings::SettingsInput {
            access: Some(match access {
                Access::Platform => ruum_core::apps::settings::Access::Platform,
                Access::Public => ruum_core::apps::settings::Access::Public,
            }),
            ..Default::default()
        };
        let app = self
            .state
            .services
            .apps
            .create(
                &ctx,
                ruum_core::apps::CreateApp {
                    parent_id: root,
                    name: slug.to_string(),
                    settings: Some(settings),
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .map_err(|e| failed("createApp", e))?
            .value;
        if app.slug != slug {
            return Err(WorldError::Failed(format!(
                "createApp gave the slug {}, expected {slug}",
                app.slug
            )));
        }
        self.seeds
            .lock()
            .expect("seeds")
            .app_slugs
            .insert(app.node.id, slug.to_string());
        Ok(App {
            id: app.node.id.to_string(),
            slug: slug.to_string(),
            tenant: tenant.clone(),
        })
    }

    async fn seed_deployment(
        &self,
        app: &App,
        env: Env,
        files: &[(String, Vec<u8>)],
        spa_fallback: Option<&str>,
    ) -> WorldResult<Deployment> {
        let tid = Self::tenant_id(&app.tenant)?;
        let node = Self::node_id(&app.id)?;
        let id = Uuid::now_v7();
        let short_id = ruum_core::apps::slugs::short_id();
        let list: Vec<(String, Bytes)> = files
            .iter()
            .map(|(p, b)| (p.clone(), Bytes::from(b.clone())))
            .collect();
        self.sites
            .write_site(tid, id, &list, spa_fallback.map(str::to_string))
            .await
            .map_err(|e| failed("the static site", e))?;
        let (owner, tenant_slug) = {
            let s = self.seeds.lock().expect("seeds");
            let (slug, owner) = s
                .tenants
                .get(&tid)
                .cloned()
                .ok_or_else(|| WorldError::Failed("an unknown tenant".into()))?;
            (owner, slug)
        };
        let seeded = Seeded {
            tenant: tid,
            tenant_slug: tenant_slug.clone(),
            app: node,
            owner,
            short_id: short_id.clone(),
        };
        self.seeds
            .lock()
            .expect("seeds")
            .deployments
            .insert(id, seeded.clone());
        if env == Env::Preview {
            self.insert(&seeded, id, Environment::Preview, DeploymentState::Ready)
                .await?;
            self.hosts.set(
                AppHost {
                    tenant_slug,
                    app_slug: app.slug.clone(),
                    short_id: Some(short_id.clone()),
                },
                Resolved {
                    tenant_id: tid,
                    app_id: node,
                    deployment_id: id,
                },
            );
        }
        Ok(Deployment {
            id: id.to_string(),
            short_id,
            env,
            app_id: app.id.clone(),
        })
    }

    async fn set_live(&self, app: &App, deployment: &Deployment) -> WorldResult<()> {
        let id: Uuid = deployment
            .id
            .parse()
            .map_err(|e| failed("a deployment ID", e))?;
        let seeded = self
            .seeds
            .lock()
            .expect("seeds")
            .deployments
            .get(&id)
            .cloned()
            .ok_or_else(|| WorldError::Failed("an unknown deployment".into()))?;
        self.insert(&seeded, id, Environment::Live, DeploymentState::Live)
            .await?;
        let slug = self
            .seeds
            .lock()
            .expect("seeds")
            .app_slugs
            .get(&seeded.app)
            .cloned()
            .unwrap_or(app.slug.clone());
        self.hosts.set(
            AppHost {
                tenant_slug: seeded.tenant_slug.clone(),
                app_slug: slug,
                short_id: None,
            },
            Resolved {
                tenant_id: seeded.tenant,
                app_id: seeded.app,
                deployment_id: id,
            },
        );
        // The journal event of the new release (W-2s's `app.deployed`) reaches the gateway.
        self.gw.hosts.invalidate_app(seeded.tenant, seeded.app);
        self.gw.hosts.invalidate_tenant_slug(&seeded.tenant_slug);
        Ok(())
    }

    async fn grant(&self, user: &User, app: &App, relation: &Relation) -> WorldResult<()> {
        let r = Self::relation(Self::user_id(user)?, Self::node_id(&app.id)?, relation)?;
        self.write(vec![RelationshipUpdate::Touch(r)]).await
    }

    async fn revoke(&self, user: &User, app: &App, relation: &Relation) -> WorldResult<()> {
        let r = Self::relation(Self::user_id(user)?, Self::node_id(&app.id)?, relation)?;
        self.write(vec![RelationshipUpdate::Delete(r)]).await
    }

    async fn seed_member(&self, user: &User, tenant: &Tenant) -> WorldResult<()> {
        // A guest of the other tenant: a platform account there, without any right on its apps.
        self.state
            .services
            .tenants
            .add_guest_from_email_grant(
                Self::tenant_id(tenant)?,
                Self::user_id(user)?,
                serde_json::json!({ "via": "t6" }),
            )
            .await
            .map_err(|e| failed("a membership", e))
    }

    async fn platform_authorize(&self, user: &User, login_location: &str) -> WorldResult<String> {
        let uid = Self::user_id(user)?;
        let secret = self.platform_session(uid).await?;
        if !login_location.starts_with(&format!("http://{}/v1/auth/login?", self.platform)) {
            return Err(WorldError::Failed(
                "the start did not send the browser to the platform's loginStart".into(),
            ));
        }
        let platform = self
            .http
            .get(login_location)
            .header("cookie", format!("ruum_session={secret}"))
            .send()
            .await
            .map_err(|e| failed("loginStart", e.without_url()))?;
        if platform.status().as_u16() != 302 {
            return Err(WorldError::Failed(format!(
                "loginStart with the app audience answered {}",
                platform.status()
            )));
        }
        Ok(platform
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string())
    }

    async fn sign_out_platform(&self, user: &User) -> WorldResult<()> {
        let uid = Self::user_id(user)?;
        let session = self.seeds.lock().expect("seeds").sessions.remove(&uid);
        if let Some((id, _)) = session {
            self.state
                .services
                .identity
                .end_session(uid, id)
                .await
                .map_err(|e| failed("the sign-out", e))?;
        }
        Ok(())
    }

    async fn agent_token(&self, user: &User, audience: AgentAudience) -> WorldResult<Secret> {
        let uid = Self::user_id(user)?;
        let tid = Self::tenant_id(&user.tenant)?;
        let n = self.next();
        let ctx = Context {
            tenant: tid,
            actor: Actor::user(uid),
            scope: None,
            idempotency_key: None,
        };
        let agent = self
            .state
            .services
            .agents
            .register(
                &ctx,
                NewAgent {
                    name: format!("t6 agent {n}"),
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
        c.tid = Some(tid);
        c.agent = Some(agent.id);
        c.obo = Some(uid);
        if audience == AgentAudience::Ruumapps {
            c.aud = APPS_AGENT_AUDIENCE.into();
        }
        tokens
            .issue(&c)
            .map(Secret)
            .map_err(|e| failed("an agent token", e))
    }

    async fn partner_key(&self, _app: &App, _api: &str) -> WorldResult<Secret> {
        Err(WorldError::Pending {
            card: "B-3".into(),
            why: "partner keys come with card B-3".into(),
        })
    }

    async fn set_upstream(&self, deployment: &Deployment, url: &str) -> WorldResult<()> {
        let id: Uuid = deployment
            .id
            .parse()
            .map_err(|e| failed("a deployment ID", e))?;
        self.gw.upstreams.set(id, url);
        Ok(())
    }

    async fn set_public_approval(&self, tenant: &Tenant, approved: bool) -> WorldResult<()> {
        self.state
            .services
            .apps
            .set_tenant_approval(
                Self::tenant_id(tenant)?,
                ApprovalChange {
                    public_apps: Some(approved),
                    custom_domains: None,
                    by: "t6@ruum.test".into(),
                },
            )
            .await
            .map(|_| ())
            .map_err(|e| failed("the public approval", e))
    }

    async fn reuse_host(&self, removed: &App, new: &App) -> WorldResult<App> {
        let tid = Self::tenant_id(&removed.tenant)?;
        let a = Self::node_id(&removed.id)?;
        let b = Self::node_id(&new.id)?;
        // A goes away (purged: its rows are gone).
        self.state
            .services
            .apps
            .store()
            .remove_app(tid, a)
            .await
            .map_err(|e| failed("the removal", e))?;
        self.hosts.remove_app(a);
        // B takes A's host name, past the retired-slug rule (a test-only step).
        let live = self
            .state
            .services
            .apps
            .store()
            .get_app(tid, b)
            .await
            .map_err(|e| failed("app B", e))?
            .and_then(|r| r.live_deployment_id)
            .ok_or_else(|| WorldError::Failed("app B has no live deployment".into()))?;
        let tenant_slug = removed.tenant.slug.clone();
        self.hosts.set(
            AppHost {
                tenant_slug: tenant_slug.clone(),
                app_slug: removed.slug.clone(),
                short_id: None,
            },
            Resolved {
                tenant_id: tid,
                app_id: b,
                deployment_id: live,
            },
        );
        self.seeds
            .lock()
            .expect("seeds")
            .app_slugs
            .insert(b, removed.slug.clone());
        self.gw.hosts.invalidate_tenant_slug(&tenant_slug);
        Ok(App {
            id: new.id.clone(),
            slug: removed.slug.clone(),
            tenant: new.tenant.clone(),
        })
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

impl World {
    async fn insert(
        &self,
        s: &Seeded,
        id: Uuid,
        environment: Environment,
        state: DeploymentState,
    ) -> WorldResult<()> {
        let ctx = Context {
            tenant: s.tenant,
            actor: Actor::user(s.owner),
            scope: None,
            idempotency_key: None,
        };
        let archive = format!("{id} archive");
        let manifest = serde_json::json!({ "static": { "root": "." } });
        self.state
            .services
            .apps
            .store()
            .insert_deployment(
                &ctx,
                NewDeployment {
                    id,
                    app_node_id: s.app,
                    environment,
                    state,
                    short_id: s.short_id.clone(),
                    label: None,
                    draft: None,
                    source: DeploymentSource {
                        kind: "folder".into(),
                        node_id: None,
                        version_id: None,
                        files: Some(Vec::new()),
                    },
                    archive: Archive {
                        content_hash: *blake3::hash(archive.as_bytes()).as_bytes(),
                        size: archive.len() as i64,
                        chunks: Vec::new(),
                    },
                    manifest: Some(manifest),
                    stages: serde_json::json!([]),
                    published_from: None,
                    expires_at: None,
                },
            )
            .await
            .map(|_| ())
            .map_err(|e| failed("the deployment", e))
    }
}

fn make() -> MakeGatewayWorld {
    Arc::new(|| {
        Box::pin(async {
            let w = World::new().await?;
            Ok(Arc::new(w) as Arc<dyn GatewayWorld>)
        })
    })
}

/// Every T-6 case with the G-1a world: none fails, and none reports planned (the world is
/// there; a case waits only for a named later card).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t6_gateway_cases_pass_with_the_g1a_world() {
    let cases = gateway::gateway_cases();
    let rows = gateway::run_gateway_cases(&cases, Some(make())).await;
    let (text, ok) = gateway::render("gateway (G-1a world, in-memory core)", &rows);
    println!("{text}");
    assert!(ok, "{text}");
    for (name, o) in &rows {
        assert!(
            !matches!(o, gateway::Outcome::Planned(_)),
            "{name} is planned with the G-1a world: {o:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The state of the code flow (card G-1a "Acceptance tests": expired, replayed, another host, a
// return_to off the origin) and the platform side of the app audience.
// ---------------------------------------------------------------------------------------------

/// A live platform app `garden` of `acme` with a member who has `view`.
async fn app_scene(w: &World) -> (Tenant, App, User, String) {
    let t = w.seed_tenant("acme").await.unwrap();
    let u = w.seed_user(&t, "mari").await.unwrap();
    let a = w.seed_app(&t, "garden", Access::Platform).await.unwrap();
    let d = w
        .seed_deployment(
            &a,
            Env::Live,
            &gateway::files(&[("index.html", b"hi")]),
            None,
        )
        .await
        .unwrap();
    w.set_live(&a, &d).await.unwrap();
    w.grant(&u, &a, &Relation::View).await.unwrap();
    let host = format!("garden.acme.{APPS_DOMAIN}");
    (t, a, u, host)
}

/// The gateway's start and the platform side: the callback target and the nonce cookie.
async fn code_for(w: &World, user: &User, host: &str) -> (String, String) {
    let start = w
        .gateway_get(host, "/.ruum/v1/auth/start?return_to=%2Fdocs%3Fa%3D1", None)
        .await
        .unwrap();
    assert_eq!(start.status, 302);
    let nonce = cookie_value(&start, "__Host-ruum_auth").unwrap();
    let login = start.header("location").unwrap().to_string();
    let back = w.platform_authorize(user, &login).await.unwrap();
    let back = url::Url::parse(&back).unwrap();
    assert_eq!(back.scheme(), "https");
    assert_eq!(back.host_str(), Some(host));
    assert_eq!(back.path(), "/.ruum/v1/auth/callback");
    (
        format!("{}?{}", back.path(), back.query().unwrap_or("")),
        nonce,
    )
}

fn has_session(resp: &gateway::Resp) -> bool {
    cookie_value(resp, "__Host-ruum_app").is_some_and(|v| !v.is_empty())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_code_works_once_and_returns_to_its_path() {
    let w = World::new().await.unwrap();
    let (_, _, u, host) = app_scene(&w).await;
    let (target, nonce) = code_for(&w, &u, &host).await;
    let cookie = format!("__Host-ruum_auth={nonce}");
    let first = w.gateway_get(&host, &target, Some(&cookie)).await.unwrap();
    assert_eq!(first.status, 302);
    assert_eq!(first.header("location"), Some("/docs?a=1"));
    assert!(has_session(&first));
    let again = w.gateway_get(&host, &target, Some(&cookie)).await.unwrap();
    assert_eq!(again.status, 400, "a replayed code");
    assert!(!has_session(&again));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_state_needs_its_host_and_the_browsers_nonce() {
    let w = World::new().await.unwrap();
    let (t, _, u, host) = app_scene(&w).await;
    let b = w.seed_app(&t, "orchard", Access::Platform).await.unwrap();
    let bd = w
        .seed_deployment(
            &b,
            Env::Live,
            &gateway::files(&[("index.html", b"b")]),
            None,
        )
        .await
        .unwrap();
    w.set_live(&b, &bd).await.unwrap();
    let other = format!("orchard.acme.{APPS_DOMAIN}");
    // Another host.
    let (target, nonce) = code_for(&w, &u, &host).await;
    let r = w
        .gateway_get(&other, &target, Some(&format!("__Host-ruum_auth={nonce}")))
        .await
        .unwrap();
    assert_eq!(r.status, 400, "a state of another host");
    assert!(!has_session(&r));
    // Without the browser's nonce, or with another one.
    let (target, _) = code_for(&w, &u, &host).await;
    for cookie in [
        None,
        Some("__Host-ruum_auth=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
    ] {
        let r = w.gateway_get(&host, &target, cookie).await.unwrap();
        assert_eq!(r.status, 400, "{cookie:?}");
        assert!(!has_session(&r));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_expired_state_is_refused() {
    let w = World::new().await.unwrap();
    let (_, _, u, host) = app_scene(&w).await;
    let (target, nonce) = code_for(&w, &u, &host).await;
    let url = url::Url::parse(&format!("https://{host}{target}")).unwrap();
    let code = url
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.to_string())
        .unwrap();
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.to_string())
        .unwrap();
    // The same claims, sealed with a time in the past.
    let claims = {
        use base64::Engine as _;
        let p = state.split('.').nth(1).unwrap();
        let v: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(p)
                .unwrap(),
        )
        .unwrap();
        v["v"].clone()
    };
    let expired = w.gw.tokens.seal(&claims, -5).unwrap();
    let target = format!("/.ruum/v1/auth/callback?code={code}&state={expired}");
    let r = w
        .gateway_get(&host, &target, Some(&format!("__Host-ruum_auth={nonce}")))
        .await
        .unwrap();
    assert_eq!(r.status, 400);
    assert!(!has_session(&r));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_platform_side_checks_the_app_and_the_session() {
    let w = World::new().await.unwrap();
    let (t, a, u, host) = app_scene(&w).await;
    let secret = w
        .platform_session(World::user_id(&u).unwrap())
        .await
        .unwrap();
    let start = w
        .gateway_get(&host, "/.ruum/v1/auth/start", None)
        .await
        .unwrap();
    let login = url::Url::parse(start.header("location").unwrap()).unwrap();
    let params: HashMap<String, String> = login.query_pairs().into_owned().collect();
    let send = |q: Vec<(&str, String)>, session: bool| {
        let mut url = login.clone();
        url.query_pairs_mut().clear().extend_pairs(q);
        let mut req = w.http.get(url);
        if session {
            req = req.header("cookie", format!("ruum_session={secret}"));
        }
        req.send()
    };
    let base = |k: &str| (k.to_string(), params[k].clone());
    // Each wrong value gets the same 400.
    let unknown = Uuid::now_v7().to_string();
    for (what, q) in [
        (
            "another tenant",
            vec![("app_tenant", unknown.clone()), ("app_id", a.id.clone())],
        ),
        (
            "another app",
            vec![("app_tenant", t.id.clone()), ("app_id", unknown.clone())],
        ),
    ] {
        let mut all: Vec<(&str, String)> = q;
        for k in ["app_deployment", "app_state"] {
            all.push((k, base(k).1));
        }
        let r = send(all, true).await.unwrap();
        assert_eq!(r.status().as_u16(), 400, "{what}");
        assert!(r.headers().get("location").is_none(), "{what}");
    }
    let mut bad_state: Vec<(&str, String)> = ["app_tenant", "app_id", "app_deployment"]
        .iter()
        .map(|k| (*k, params[*k].clone()))
        .collect();
    bad_state.push(("app_state", "not a state".into()));
    assert_eq!(send(bad_state, true).await.unwrap().status().as_u16(), 400);
    // Without a platform session: the normal sign-in first, never the app's callback.
    let all: Vec<(&str, String)> = ["app_tenant", "app_id", "app_deployment", "app_state"]
        .iter()
        .map(|k| (*k, params[*k].clone()))
        .collect();
    let r = send(all, false).await.unwrap();
    assert_eq!(r.status().as_u16(), 302);
    let loc = r.headers()["location"].to_str().unwrap().to_string();
    assert!(!loc.contains(".ruum/v1/auth/callback"), "{loc}");
    assert!(!loc.contains("code="), "{loc}");
}

/// The request path only selects an entry of the deployment's own site index: it never names a
/// deployment, an index or a file object (plan seam 1; card G-1a "Static files").
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_path_never_chooses_the_object() {
    let w = World::new().await.unwrap();
    let (_, a, u, host) = app_scene(&w).await;
    let other = w
        .seed_deployment(
            &a,
            Env::Preview,
            &gateway::files(&[("secret.txt", b"T6-SECRET-OF-ANOTHER-DEPLOYMENT")]),
            None,
        )
        .await
        .unwrap();
    let (target, nonce) = code_for(&w, &u, &host).await;
    let cb = w
        .gateway_get(&host, &target, Some(&format!("__Host-ruum_auth={nonce}")))
        .await
        .unwrap();
    let session = format!(
        "__Host-ruum_app={}",
        cookie_value(&cb, "__Host-ruum_app").unwrap()
    );
    let ok = w
        .gateway_get(&host, "/index.html", Some(&session))
        .await
        .unwrap();
    assert_eq!(ok.status, 200);
    let hash = blake3::hash(b"T6-SECRET-OF-ANOTHER-DEPLOYMENT")
        .to_hex()
        .to_string();
    for target in [
        format!("/{}/secret.txt", other.id),
        format!("/sites/{}.json", other.id),
        format!("/files/{hash}"),
        format!("/{hash}"),
        "/secret.txt".to_string(),
    ] {
        let r = w.gateway_get(&host, &target, Some(&session)).await.unwrap();
        assert_eq!(r.status, 404, "{target}");
        assert!(!r.text().contains("T6-SECRET"), "{target}");
    }
}
