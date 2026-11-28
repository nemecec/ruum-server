//! The T-6 world of the gateway, shared by the test binaries of this crate (`t6_world.rs`, the
//! T-6 cases and the code-flow tests; `sandbox_proxy.rs`, the proxy tests of card G-1b). The
//! world itself is described in `t6_world.rs`. Card G-1b adds the sandbox runtime: the core's
//! `SandboxDirectory` over the memory driver (counted, with an optional delay), a spec maker for
//! starts, and a locator that gives each running instance its own port on 127.0.0.1 forwarding
//! to the deployment's echo app; a stopped instance loses its port, as a container loses its
//! address.

// Each test binary uses a part of the world.
#![allow(dead_code)]

use async_trait::async_trait;
use bytes::Bytes;
use ruum_api::AppState;
use ruum_core::actor::{Actor, Context};
use ruum_core::agents::{AgentScope, NewAgent};
use ruum_core::apps::roles::AddRoleGrant;
use ruum_core::apps::static_site::StaticSites;
use ruum_core::apps::store::{
    ApprovalChange, Archive, DeploymentSource, DeploymentState, Environment, NewDeployment,
};
use ruum_core::authz::{ObjectRef, Relationship, RelationshipUpdate, Role, SubjectRef};
use ruum_core::identity::{APPS_AGENT_AUDIENCE, AccessClaims, TokenType, VerifiedLogin};
use ruum_core::nodes::OnConflict;
use ruum_core::sharing::Grantee;
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

use ruum_core::apps::sandboxes::{Locator, SandboxDirectory, SpecMaker, Upstream};
use ruum_core::apps::store::DeploymentRow;
use ruum_core::identity::app_sign_in::AppEnv;
use ruum_core::identity::app_sign_in::wire::UpstreamQuery;
use ruum_core::runtime::{
    ImageRef, InstanceId, InstanceState, InstanceStatus, Limits, LogLine, MemoryDriver,
    MemoryRegistry, MemoryUsage, OciDigest, RuntimeDriver, RuntimeError, SecretValue, ServiceSpec,
    WorkloadIdentity, WorkloadKind, WorkloadSpec,
};
use ruum_gateway::proxy::ProxyConfig;
use ruum_gateway::sandbox::{HoldConfig, HttpSandboxes};
use std::sync::atomic::{AtomicUsize, Ordering};

pub const APPS_DOMAIN: &str = "ruumapps.net";
pub const ISSUER: &str = "https://ruum.localhost";

pub fn failed(what: &str, e: impl std::fmt::Display) -> WorldError {
    WorldError::Failed(format!("{what}: {e}"))
}

/// A deployment seeded but not yet in the store (a live one goes there at `set_live`).
#[derive(Clone)]
pub struct Seeded {
    tenant: TenantId,
    tenant_slug: String,
    app: NodeId,
    owner: UserId,
    short_id: String,
    /// The address the instance of a node22 deployment answers as (card G-1b); `None` for a
    /// static site.
    runtime: Option<String>,
}

#[derive(Default)]
pub struct Seeds {
    /// tenant ID -> (slug, owner)
    tenants: HashMap<TenantId, (String, UserId)>,
    /// user ID -> (platform session ID, secret)
    sessions: HashMap<UserId, (Uuid, String)>,
    deployments: HashMap<Uuid, Seeded>,
    /// app ID -> slug the host of the app has now
    app_slugs: HashMap<NodeId, String>,
    /// (user, app, role) -> the ID of the app role grant
    role_grants: HashMap<(UserId, NodeId, String), Uuid>,
    count: u64,
}

pub struct World {
    pub state: AppState,
    pub hosts: Arc<MemoryHostLookup>,
    /// The core's static sites (the gateway never holds them).
    pub sites: StaticSites,
    /// The stand-in of the object store that the core's links point at.
    pub objects: ObjectServer,
    pub gw: Arc<Gateway>,
    pub platform: SocketAddr,
    pub gateway: SocketAddr,
    pub http: reqwest::Client,
    pub seeds: Mutex<Seeds>,
    pub tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// The runtime of the sandbox deployments (card G-1b): the memory driver, counted.
    pub driver: Arc<CountingDriver>,
    pub registry: Arc<MemoryRegistry>,
    /// The core's sandbox directory.
    pub directory: Arc<SandboxDirectory>,
    /// The addresses of the running instances.
    pub instances: Arc<Instances>,
    /// The core's internal listener.
    pub internal: SocketAddr,
}

/// The settings of a world.
#[derive(Clone, Default)]
pub struct Opts {
    pub hold: HoldConfig,
    pub proxy: ProxyConfig,
    /// The core gets no link signer: it serves no static file (the server without S3).
    pub no_static_links: bool,
}

/// The memory driver, with counters of starts and wakes and a delay before each.
pub struct CountingDriver {
    inner: MemoryDriver,
    pub starts: AtomicUsize,
    pub wakes: AtomicUsize,
    delay: Mutex<Duration>,
}

impl CountingDriver {
    fn new(registry: Arc<MemoryRegistry>) -> Self {
        Self {
            inner: MemoryDriver::new(registry, Arc::new(MemoryUsage::new())),
            starts: AtomicUsize::new(0),
            wakes: AtomicUsize::new(0),
            delay: Mutex::new(Duration::ZERO),
        }
    }

    /// How long each later start and wake takes.
    pub fn set_delay(&self, d: Duration) {
        *self.delay.lock().expect("delay") = d;
    }

    pub fn starts(&self) -> usize {
        self.starts.load(Ordering::SeqCst)
    }

    async fn pause(&self) {
        let d = *self.delay.lock().expect("delay");
        if !d.is_zero() {
            tokio::time::sleep(d).await;
        }
    }
}

#[async_trait]
impl RuntimeDriver for CountingDriver {
    fn name(&self) -> &str {
        "counting-memory"
    }
    async fn start(&self, spec: WorkloadSpec) -> Result<InstanceStatus, RuntimeError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.pause().await;
        self.inner.start(spec).await
    }
    async fn status(&self, t: TenantId, i: InstanceId) -> Result<InstanceStatus, RuntimeError> {
        self.inner.status(t, i).await
    }
    async fn stop(&self, t: TenantId, i: InstanceId) -> Result<(), RuntimeError> {
        self.inner.stop(t, i).await
    }
    async fn touch(&self, t: TenantId, i: InstanceId) -> Result<(), RuntimeError> {
        self.inner.touch(t, i).await
    }
    async fn wake(&self, t: TenantId, i: InstanceId) -> Result<InstanceStatus, RuntimeError> {
        self.wakes.fetch_add(1, Ordering::SeqCst);
        self.pause().await;
        self.inner.wake(t, i).await
    }
    async fn logs(
        &self,
        t: TenantId,
        i: InstanceId,
        after: Option<u64>,
        limit: usize,
    ) -> Result<Vec<LogLine>, RuntimeError> {
        self.inner.logs(t, i, after, limit).await
    }
    async fn rotate_token(
        &self,
        t: TenantId,
        i: InstanceId,
        token: SecretValue,
    ) -> Result<(), RuntimeError> {
        self.inner.rotate_token(t, i, token).await
    }
}

/// The addresses of running instances: each running instance gets its own port on 127.0.0.1
/// that forwards to the deployment's echo app, and loses it when it stops, as a container's
/// address does. So a gateway that keeps an old address finds nothing there.
#[derive(Default)]
pub struct Instances {
    /// deployment -> the address of its echo app
    targets: Mutex<HashMap<Uuid, SocketAddr>>,
    /// instance -> (its port, the tasks that serve it)
    ports: Mutex<HashMap<InstanceId, (u16, Tasks)>>,
    /// How long a new instance's port stays closed after the instance runs (an app that is
    /// still booting; kimi-run12-g1b 2).
    listen_delay: Mutex<Duration>,
}

type Tasks = Vec<tokio::task::JoinHandle<()>>;

/// How long [`Instances::close`] waits after the sockets are closed (backlog T-88).
const CLOSE_SETTLE: Duration = Duration::from_millis(100);

impl Instances {
    /// How long each later instance's port stays closed after it runs.
    pub fn set_listen_delay(&self, d: Duration) {
        *self.listen_delay.lock().expect("listen delay") = d;
    }

    fn set_target(&self, deployment: Uuid, url: &str) -> WorldResult<()> {
        let addr: SocketAddr = url
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .parse()
            .map_err(|e| failed("an upstream address", e))?;
        self.targets
            .lock()
            .expect("targets")
            .insert(deployment, addr);
        Ok(())
    }

    /// The port of a running instance (made on first use).
    fn locate(self: &Arc<Self>, st: &InstanceStatus) -> Option<Upstream> {
        if st.state != InstanceState::Running {
            return None;
        }
        if let Some((port, _)) = self.ports.lock().expect("ports").get(&st.id) {
            return Some(Upstream {
                host: "127.0.0.1".into(),
                port: *port,
            });
        }
        let target = *self
            .targets
            .lock()
            .expect("targets")
            .get(&st.identity.deployment)?;
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").ok()?;
        std_listener.set_nonblocking(true).ok()?;
        let port = std_listener.local_addr().ok()?.port();
        let delay = *self.listen_delay.lock().expect("listen delay");
        // With a listen delay the port is closed (nothing listens) until the app is up.
        let early = if delay.is_zero() {
            Some(tokio::net::TcpListener::from_std(std_listener).ok()?)
        } else {
            drop(std_listener);
            None
        };
        let me = self.clone();
        let id = st.id;
        // The entry first, so every connection task is recorded and ends at `close`.
        self.ports
            .lock()
            .expect("ports")
            .insert(id, (port, Vec::new()));
        let accept = tokio::spawn(async move {
            let listener = match early {
                Some(l) => l,
                None => {
                    tokio::time::sleep(delay).await;
                    match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
                        Ok(l) => l,
                        Err(_) => return,
                    }
                }
            };
            while let Ok((mut inbound, _)) = listener.accept().await {
                let t = tokio::spawn(async move {
                    if let Ok(mut outbound) = tokio::net::TcpStream::connect(target).await {
                        let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                    }
                });
                // After `close` the entry is gone: a connection accepted in that moment ends
                // too, never outlives the stop.
                match me.ports.lock().expect("ports").get_mut(&id) {
                    Some((_, tasks)) => tasks.push(t),
                    None => t.abort(),
                }
            }
        });
        if let Some((_, tasks)) = self.ports.lock().expect("ports").get_mut(&id) {
            tasks.push(accept);
        }
        Some(Upstream {
            host: "127.0.0.1".into(),
            port,
        })
    }

    /// The instance's address goes away (its listener and every connection). It returns when
    /// the listener and the sockets are closed: `abort` only asks a task to stop, and under load
    /// (backlog T-88: a strict mutation baseline beside two builds) a connection that the
    /// gateway kept from before the stop was still open when the next request came, so the
    /// gateway sent that request into it and answered 502 when it closed under the request,
    /// instead of a cold start.
    async fn close(&self, id: InstanceId) {
        let tasks = self
            .ports
            .lock()
            .expect("ports")
            .remove(&id)
            .map(|(_, tasks)| tasks)
            .unwrap_or_default();
        for t in &tasks {
            t.abort();
        }
        for t in tasks {
            // An aborted task's future (and its sockets) is dropped before the handle is ready.
            let _ = t.await;
        }
        // The gateway's own end of a kept connection learns of the close when its connection
        // task runs; give it that turn before the test sends the next request.
        tokio::time::sleep(CLOSE_SETTLE).await;
    }
}

impl Drop for Instances {
    fn drop(&mut self) {
        if let Ok(mut p) = self.ports.lock() {
            for (_, (_, tasks)) in p.drain() {
                for t in tasks {
                    t.abort();
                }
            }
        }
    }
}

/// The OCI image of every sandbox deployment of the world.
fn image() -> OciDigest {
    OciDigest::of(b"t6 g1b node22 image")
}

fn identity_of(row: &DeploymentRow) -> WorkloadIdentity {
    WorkloadIdentity {
        tenant: row.tenant_id,
        app: row.app_node_id,
        deployment: row.id,
        environment: match row.environment {
            Environment::Live => ruum_core::runtime::Environment::Live,
            Environment::Preview => ruum_core::runtime::Environment::Preview,
        },
    }
}

/// The workload of a sandbox deployment (`command` empty: it serves).
pub fn spec_of(identity: WorkloadIdentity, command: Vec<String>) -> WorkloadSpec {
    WorkloadSpec {
        identity,
        kind: WorkloadKind::Service(ServiceSpec {
            port: 3000,
            health: None,
            idle_timeout: None,
        }),
        image: ImageRef::Oci(image()),
        command,
        env: Vec::new(),
        previous_secrets: Vec::new(),
        workload_token: SecretValue::new(format!("t6-workload-{}", Uuid::now_v7().simple())),
        limits: Limits {
            cpu_millis: 250,
            memory_bytes: 64 << 20,
            disk_bytes: 64 << 20,
        },
        routes: Vec::new(),
    }
}

impl Drop for World {
    fn drop(&mut self) {
        for t in self.tasks.lock().expect("tasks").drain(..) {
            t.abort();
        }
    }
}

/// The stand-in of the object store for the core's links (decision 0008-4): `GET /<key>` of the
/// core's own object store, only with an `expires` in the future and the `sig` that the
/// signer made for that key and time. It counts the requests that reached it.
#[derive(Clone)]
pub struct ObjectServer {
    store: Arc<dyn object_store::ObjectStore>,
    addr: SocketAddr,
    key: [u8; 32],
    pub gets: Arc<AtomicUsize>,
}

/// The link signer of an [`ObjectServer`].
#[derive(Debug)]
pub struct ObjectSigner {
    addr: SocketAddr,
    key: [u8; 32],
}

fn object_sig(key: &[u8; 32], path: &str, expires: u64) -> String {
    blake3::keyed_hash(key, format!("{path}\n{expires}").as_bytes())
        .to_hex()
        .to_string()
}

#[async_trait]
impl object_store::signer::Signer for ObjectSigner {
    async fn signed_url(
        &self,
        method: object_store::signer::Method,
        path: &object_store::path::Path,
        expires_in: Duration,
    ) -> object_store::Result<url::Url> {
        assert_eq!(
            method,
            object_store::signer::Method::GET,
            "the core signs reads only"
        );
        let expires = ruum_core::now().unix_timestamp() as u64 + expires_in.as_secs();
        let sig = object_sig(&self.key, path.as_ref(), expires);
        Ok(url::Url::parse(&format!(
            "http://{}/{path}?expires={expires}&sig={sig}",
            self.addr
        ))
        .expect("url"))
    }
}

impl ObjectServer {
    pub fn new(store: Arc<dyn object_store::ObjectStore>, addr: SocketAddr) -> Self {
        let mut key = [0u8; 32];
        rand::fill(&mut key[..]);
        Self {
            store,
            addr,
            key,
            gets: Arc::default(),
        }
    }

    pub fn signer(&self) -> ObjectSigner {
        ObjectSigner {
            addr: self.addr,
            key: self.key,
        }
    }

    /// A link for `path` that expired `ago` seconds ago, signed like a real one.
    pub fn expired_link(&self, path: &str, ago: u64) -> String {
        let expires = ruum_core::now().unix_timestamp() as u64 - ago;
        let sig = object_sig(&self.key, path, expires);
        format!("http://{}/{path}?expires={expires}&sig={sig}", self.addr)
    }

    pub fn router(&self) -> axum::Router {
        use axum::extract::{Path as AxPath, Query, State};
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use object_store::ObjectStoreExt;
        async fn get(
            State(s): State<ObjectServer>,
            AxPath(key): AxPath<String>,
            Query(q): Query<HashMap<String, String>>,
        ) -> axum::response::Response {
            s.gets.fetch_add(1, Ordering::SeqCst);
            let expires = q.get("expires").and_then(|e| e.parse::<u64>().ok());
            let ok = expires.is_some_and(|e| {
                e > ruum_core::now().unix_timestamp() as u64
                    && q.get("sig") == Some(&object_sig(&s.key, &key, e))
            });
            if !ok {
                return StatusCode::FORBIDDEN.into_response();
            }
            match s.store.get(&object_store::path::Path::from(key)).await {
                Ok(r) => match r.bytes().await {
                    Ok(b) => b.into_response(),
                    Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                },
                Err(_) => StatusCode::NOT_FOUND.into_response(),
            }
        }
        axum::Router::new()
            .route("/{*key}", axum::routing::get(get))
            .with_state(self.clone())
    }
}

pub async fn listen() -> tokio::net::TcpListener {
    tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind 127.0.0.1")
}

impl World {
    pub async fn new() -> WorldResult<World> {
        Self::with(Opts::default()).await
    }

    pub async fn with(opts: Opts) -> WorldResult<World> {
        let mut state = AppState::in_memory();
        // The sandbox runtime of card G-1b: the core's directory over the world's driver.
        let registry = MemoryRegistry::new();
        let driver = Arc::new(CountingDriver::new(registry.clone()));
        let instances = Arc::new(Instances::default());
        let locate = instances.clone();
        let locator: Locator = Arc::new(move |st: &InstanceStatus| locate.locate(st));
        let spec: SpecMaker = Arc::new(|row: DeploymentRow| {
            let spec = Some(spec_of(identity_of(&row), Vec::new()));
            Box::pin(async move { spec })
        });
        let directory = Arc::new(
            SandboxDirectory::new(state.services.apps.store().clone(), driver.clone(), locator)
                .with_spec_maker(spec),
        );
        state.services.sandboxes = Some(directory.clone());
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
        // The core's static sites, with links to a small object server that stands for the
        // object store (decision 0008-4: the gateway holds no key and reads only through them).
        let sites = state.services.apps.sites().clone();
        let objects_l = listen().await;
        let objects =
            ObjectServer::new(sites.object_store(), objects_l.local_addr().expect("addr"));
        if !opts.no_static_links {
            sites.set_links(Arc::new(objects.signer()));
        }
        let site_files =
            ruum_gateway::static_files::HttpSiteFiles::new(&format!("http://{internal}"))
                .map_err(|e| failed("the static file client", e))?;
        let core = ruum_gateway::core_api::HttpCore::new(&format!("http://{internal}"))
            .map_err(|e| failed("the core client", e))?;
        let sandboxes = HttpSandboxes::new(&format!("http://{internal}"))
            .map_err(|e| failed("the sandbox client", e))?;
        let gw = Arc::new(
            Gateway::new(
                Config {
                    apps_domain: APPS_DOMAIN.into(),
                    platform_url: format!("http://{platform}"),
                    ui_origin: ISSUER.into(),
                },
                hosts.clone(),
                Arc::new(core),
                UserTokenSigner::generate(ISSUER),
                Arc::new(site_files),
            )
            .with_sandboxes(Arc::new(sandboxes), opts.hold)
            .with_proxy(opts.proxy),
        );
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
        let router = objects.router();
        tasks.push(tokio::spawn(async move {
            let _ = axum::serve(objects_l, router.into_make_service()).await;
        }));
        Ok(World {
            state,
            hosts,
            sites,
            objects,
            gw,
            platform,
            gateway,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("client"),
            seeds: Mutex::new(Seeds::default()),
            tasks: Mutex::new(tasks),
            driver,
            registry,
            directory,
            instances,
            internal,
        })
    }

    fn deployment_id(d: &Deployment) -> WorldResult<Uuid> {
        d.id.parse().map_err(|e| failed("a deployment ID", e))
    }

    /// The query of a sandbox deployment, as the gateway asks it.
    pub fn query(&self, d: &Deployment) -> WorldResult<UpstreamQuery> {
        let id = Self::deployment_id(d)?;
        let s = self
            .seeds
            .lock()
            .expect("seeds")
            .deployments
            .get(&id)
            .cloned()
            .ok_or_else(|| WorldError::Failed("an unknown deployment".into()))?;
        Ok(UpstreamQuery {
            tenant_id: s.tenant,
            app_id: s.app,
            deployment_id: id,
            environment: match d.env {
                Env::Live => AppEnv::Live,
                Env::Preview => AppEnv::Preview,
            },
        })
    }

    /// Starts an instance of a stored sandbox deployment with `command` and records it in the
    /// directory, as the deploy pipeline would.
    pub async fn start_instance(&self, d: &Deployment, command: Vec<String>) -> WorldResult<()> {
        let q = self.query(d)?;
        let identity = WorkloadIdentity {
            tenant: q.tenant_id,
            app: q.app_id,
            deployment: q.deployment_id,
            environment: match q.environment {
                AppEnv::Live => ruum_core::runtime::Environment::Live,
                AppEnv::Preview => ruum_core::runtime::Environment::Preview,
            },
        };
        let st = self
            .driver
            .start(spec_of(identity, command))
            .await
            .map_err(|e| failed("an instance", e))?;
        self.directory
            .register(q.tenant_id, st.id)
            .await
            .map_err(|e| failed("the sandbox directory", e))
    }

    /// Replaces the instance of a deployment with one that exits at once (a crash).
    pub async fn crash_instance(&self, d: &Deployment) -> WorldResult<InstanceId> {
        self.stop_instance(d).await?;
        self.start_instance(d, vec!["exit:1".into()]).await?;
        let q = self.query(d)?;
        let id = self
            .directory
            .instance_of(&q)
            .ok_or_else(|| WorldError::Failed("no instance".into()))?;
        for _ in 0..200 {
            let st = self
                .driver
                .status(q.tenant_id, id)
                .await
                .map_err(|e| failed("a status", e))?;
            if matches!(st.state, InstanceState::Exited(_)) {
                return Ok(id);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Err(WorldError::Failed("the instance did not exit".into()))
    }

    pub fn next(&self) -> u64 {
        let mut s = self.seeds.lock().expect("seeds");
        s.count += 1;
        s.count
    }

    pub async fn person(&self, label: &str) -> WorldResult<UserId> {
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

    pub fn owner_ctx(&self, tenant: TenantId) -> WorldResult<Context> {
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

    pub fn tenant_id(t: &Tenant) -> WorldResult<TenantId> {
        t.id.parse().map_err(|e| failed("a tenant ID", e))
    }

    pub fn node_id(id: &str) -> WorldResult<NodeId> {
        id.parse().map_err(|e| failed("an app ID", e))
    }

    pub fn user_id(u: &User) -> WorldResult<UserId> {
        u.id.parse().map_err(|e| failed("a user ID", e))
    }

    pub async fn write(&self, updates: Vec<RelationshipUpdate>) -> WorldResult<()> {
        self.state
            .services
            .authz
            .write(updates)
            .await
            .map(|_| ())
            .map_err(|e| failed("a grant", e))
    }

    /// The tuple of `view` or `edit` (an app role goes through B-2's grants instead).
    pub fn relation(user: UserId, app: NodeId, relation: &Role) -> Relationship {
        Relationship {
            object: ObjectRef::node(app),
            relation: relation.relation().into(),
            subject: SubjectRef::user(user),
        }
    }

    /// Grants the app role `role` of the live manifest to `user` (`addAppRoleGrant` as the
    /// tenant's owner; the role must be live, as in production).
    pub async fn grant_role(&self, user: UserId, app: &App, role: &str) -> WorldResult<()> {
        let tid = Self::tenant_id(&app.tenant)?;
        let node = Self::node_id(&app.id)?;
        let ctx = self.owner_ctx(tid)?;
        let grant = self
            .state
            .services
            .apps
            .add_app_role_grant(
                &ctx,
                node,
                AddRoleGrant {
                    role: role.to_string(),
                    grantee: Grantee::User { id: user },
                    notify: false,
                },
            )
            .await
            .map_err(|e| failed("addAppRoleGrant", e))?
            .value;
        self.seeds
            .lock()
            .expect("seeds")
            .role_grants
            .insert((user, node, role.to_string()), grant.id);
        Ok(())
    }

    /// Removes the grant of `role` to `user` (`removeAppRoleGrant` as the tenant's owner).
    pub async fn revoke_role(&self, user: UserId, app: &App, role: &str) -> WorldResult<()> {
        let tid = Self::tenant_id(&app.tenant)?;
        let node = Self::node_id(&app.id)?;
        let ctx = self.owner_ctx(tid)?;
        let id = self
            .seeds
            .lock()
            .expect("seeds")
            .role_grants
            .remove(&(user, node, role.to_string()))
            .ok_or_else(|| WorldError::Failed(format!("no grant of the role {role} to remove")))?;
        self.state
            .services
            .apps
            .remove_app_role_grant(&ctx, node, id)
            .await
            .map(|_| ())
            .map_err(|e| failed("removeAppRoleGrant", e))
    }

    /// A platform session of `user` (made once), as a sign-in to Ruum would make it.
    pub async fn platform_session(&self, user: UserId) -> WorldResult<String> {
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

    pub async fn gateway_get(
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
pub fn cookie_value(resp: &gateway::Resp, name: &str) -> Option<String> {
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
            runtime: None,
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
        if seeded.runtime.is_some() {
            self.start_instance(deployment, Vec::new()).await?;
        }
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
        // The live-release hook of B-2 (the active flags of the live manifest's roles): the
        // world writes the live row through the store, so it runs the hook's sync itself.
        self.state
            .services
            .apps
            .sync_active_roles(seeded.tenant, seeded.app)
            .await
            .map_err(|e| failed("the active app roles", e))
    }

    async fn grant(&self, user: &User, app: &App, relation: &Relation) -> WorldResult<()> {
        let uid = Self::user_id(user)?;
        let role = match relation {
            Relation::View => Role::View,
            Relation::Edit => Role::Edit,
            Relation::Role(name) => return self.grant_role(uid, app, name).await,
        };
        let r = Self::relation(uid, Self::node_id(&app.id)?, &role);
        self.write(vec![RelationshipUpdate::Touch(r)]).await
    }

    async fn revoke(&self, user: &User, app: &App, relation: &Relation) -> WorldResult<()> {
        let uid = Self::user_id(user)?;
        let role = match relation {
            Relation::View => Role::View,
            Relation::Edit => Role::Edit,
            Relation::Role(name) => return self.revoke_role(uid, app, name).await,
        };
        let r = Self::relation(uid, Self::node_id(&app.id)?, &role);
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

    async fn seed_node_deployment(
        &self,
        app: &App,
        env: Env,
        upstream: &str,
    ) -> WorldResult<Deployment> {
        let tid = Self::tenant_id(&app.tenant)?;
        let node = Self::node_id(&app.id)?;
        let id = Uuid::now_v7();
        let short_id = ruum_core::apps::slugs::short_id();
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
            runtime: Some(upstream.to_string()),
        };
        self.instances.set_target(id, upstream)?;
        self.seeds
            .lock()
            .expect("seeds")
            .deployments
            .insert(id, seeded.clone());
        let environment = match env {
            Env::Live => ruum_core::runtime::Environment::Live,
            Env::Preview => ruum_core::runtime::Environment::Preview,
        };
        self.registry.push(
            &WorkloadIdentity {
                tenant: tid,
                app: node,
                deployment: id,
                environment,
            },
            &image(),
        );
        let d = Deployment {
            id: id.to_string(),
            short_id: short_id.clone(),
            env,
            app_id: app.id.clone(),
        };
        if env == Env::Preview {
            self.insert(&seeded, id, Environment::Preview, DeploymentState::Ready)
                .await?;
            self.start_instance(&d, Vec::new()).await?;
            self.hosts.set(
                AppHost {
                    tenant_slug,
                    app_slug: app.slug.clone(),
                    short_id: Some(short_id),
                },
                Resolved {
                    tenant_id: tid,
                    app_id: node,
                    deployment_id: id,
                },
            );
        }
        Ok(d)
    }

    async fn stop_instance(&self, deployment: &Deployment) -> WorldResult<()> {
        let q = self.query(deployment)?;
        let id = self
            .directory
            .instance_of(&q)
            .ok_or_else(|| WorldError::Failed("the deployment has no instance".into()))?;
        self.driver
            .stop(q.tenant_id, id)
            .await
            .map_err(|e| failed("the stop", e))?;
        self.instances.close(id).await;
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
    pub async fn insert(
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
        // Two app roles (B-2) that the role cases grant; neither is the default role, so a
        // visitor with `view` alone has no role.
        let roles = serde_json::json!({
            "member": { "description": "A member of the app" },
            "approver": { "description": "Approves requests" },
        });
        let manifest = match &s.runtime {
            None => serde_json::json!({ "static": { "root": "." }, "roles": roles }),
            Some(_) => serde_json::json!({
                "runtime": "node22",
                "start": "node server.js",
                "roles": roles,
            }),
        };
        self.state
            .services
            .apps
            .store()
            .insert_deployment(
                &ctx,
                NewDeployment {
                    image_digest: None,
                    id,
                    app_node_id: s.app,
                    environment,
                    state,
                    short_id: s.short_id.clone(),
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

pub fn make() -> MakeGatewayWorld {
    Arc::new(|| {
        Box::pin(async {
            let w = World::new().await?;
            Ok(Arc::new(w) as Arc<dyn GatewayWorld>)
        })
    })
}

/// A live platform app `garden` of `acme` with a member who has `view`.
pub async fn app_scene(w: &World) -> (Tenant, App, User, String) {
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
pub async fn code_for(w: &World, user: &User, host: &str) -> (String, String) {
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

pub fn has_session(resp: &gateway::Resp) -> bool {
    cookie_value(resp, "__Host-ruum_app").is_some_and(|v| !v.is_empty())
}
