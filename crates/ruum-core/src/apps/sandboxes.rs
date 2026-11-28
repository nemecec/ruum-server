//! Where the instance of a sandbox deployment runs (card G-1b; `docs/STEP3.md` area 4
//! "Gateway"; decision 0006 G points 1 and 3, decision 25).
//!
//! The gateway asks the core's internal route `POST /internal/gateway/v1/upstream`
//! ([`UpstreamQuery`]) where to send a request that passed its access check. The
//! [`SandboxDirectory`] answers from the core's own records only:
//!
//! - **The record decides.** The deployment is read from the store with the query's tenant, app
//!   and deployment ID (every store read filters by the tenant, so a deployment of another
//!   tenant, or a forged ID, is not found). Its environment must be the query's; it must serve
//!   (the live deployment of the app, or a ready preview that has not expired); its manifest's
//!   `runtime` must not be `static`. Otherwise the answer is `not_running`, and nothing starts.
//! - **The key is the deployment and its environment.** The map deployment -> instance holds one
//!   instance per `(tenant, deployment, environment)`, and an instance is only taken when the
//!   driver's own status names exactly that tenant, app, deployment and environment. The driver's
//!   tenant checks (`status`, `wake` and `touch` take the tenant) are the second line.
//! - **Wake and hold.** A running instance is touched (its idle timer restarts) and answered with
//!   the URL that the **locator** gives for it ([`Locator`], set at construction: the Docker
//!   wiring of I-4 builds it on the container and the service port; tests point it at a local
//!   echo server). The locator's answer is used as it is. An instance that scaled to zero is
//!   woken ([`RuntimeDriver::wake`]); a deployment without an instance, or whose instance was
//!   stopped, is started ([`RuntimeDriver::start`]) when the directory has a [`SpecMaker`] (the
//!   deploy wiring of I-4; without it nothing starts). Wakes and starts are coalesced per key:
//!   one runs in the background, and every request in the meantime waits for the same one. A
//!   request waits at most [`WAKE_WAIT`] (below the gateway client's 5 s timeout) and then gets
//!   `starting`; the gateway asks again until its own hold limit.
//! - **Crashes are not restarted here.** An instance that exited answers `crashed`; restarting a
//!   crash loop is the supervisor's job, not the job of a visitor's request.
//!
//! No `RuntimeDriver` trait change: the directory uses `status`, `touch`, `wake` and `start` as
//! they are.

use crate::apps::store::Environment as RecordEnv;
use crate::apps::store::{AppRow, AppStore, DeploymentRow, DeploymentState};
use crate::identity::app_sign_in::AppEnv;
use crate::identity::app_sign_in::wire::{UpstreamAnswer, UpstreamQuery};
use crate::runtime::{
    Environment, InstanceId, InstanceState, InstanceStatus, RuntimeDriver, RuntimeError,
    WorkloadSpec,
};
use crate::{Error, Result, TenantId};
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

/// How long one request waits for a wake or a start before it answers `starting`.
pub const WAKE_WAIT: Duration = Duration::from_secs(4);
/// A wake or a start in the background gives up after this long.
pub const FLIGHT_LIMIT: Duration = Duration::from_secs(60);
/// How often a starting instance is polled.
const POLL: Duration = Duration::from_millis(50);

/// Where an instance listens, as the locator gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upstream {
    pub host: String,
    pub port: u16,
}

impl Upstream {
    /// `http://<host>:<port>` (an IPv6 address in brackets).
    pub fn url(&self) -> String {
        if self.host.contains(':') && !self.host.starts_with('[') {
            format!("http://[{}]:{}", self.host, self.port)
        } else {
            format!("http://{}:{}", self.host, self.port)
        }
    }
}

/// Turns the status of a running instance into the address the gateway connects to.
pub type Locator = Arc<dyn Fn(&InstanceStatus) -> Option<Upstream> + Send + Sync>;

/// Builds the workload spec of a sandbox deployment, for a start (`None`: it cannot start).
/// Async: the spec of a node22 deployment reads the plan and makes the binding environment of
/// this start ([`crate::apps::AppService::spec_maker`], card I6-13).
pub type SpecMaker =
    Arc<dyn Fn(DeploymentRow) -> BoxFuture<'static, Option<WorkloadSpec>> + Send + Sync>;

/// Whether a manifest runs in a sandbox: a `runtime` that is not `static`.
pub fn is_sandbox(manifest: Option<&Value>) -> bool {
    manifest
        .and_then(|m| m.get("runtime"))
        .and_then(Value::as_str)
        .is_some_and(|r| r != "static")
}

/// Whether a deployment serves: the live deployment of its app, or a ready preview that has not
/// expired (the same rule as the gateway's `deployment` route).
pub fn serving(app: &AppRow, dep: &DeploymentRow, now: time::OffsetDateTime) -> bool {
    match dep.environment {
        RecordEnv::Live => {
            dep.state == DeploymentState::Live && app.live_deployment_id == Some(dep.id)
        }
        RecordEnv::Preview => {
            dep.state == DeploymentState::Ready && dep.expires_at.is_none_or(|t| t > now)
        }
    }
}

fn runtime_env(e: AppEnv) -> Environment {
    match e {
        AppEnv::Live => Environment::Live,
        AppEnv::Preview => Environment::Preview,
    }
}

fn record_env(e: AppEnv) -> RecordEnv {
    match e {
        AppEnv::Live => RecordEnv::Live,
        AppEnv::Preview => RecordEnv::Preview,
    }
}

/// Whether the driver's status names exactly the query's tenant, app, deployment and environment.
fn same_identity(st: &InstanceStatus, q: &UpstreamQuery) -> bool {
    st.identity.tenant == q.tenant_id
        && st.identity.app == q.app_id
        && st.identity.deployment == q.deployment_id
        && st.identity.environment == runtime_env(q.environment)
}

type Key = (TenantId, Uuid, Environment);

/// The result of a wake or a start.
#[derive(Clone, Debug)]
enum Flight {
    Status(Box<InstanceStatus>),
    NotRunning,
}

type SharedFlight = Shared<BoxFuture<'static, Flight>>;

#[derive(Default)]
struct Slot {
    instance: Option<InstanceId>,
    /// Grows by one when a wake or a start ends, so a request that read the slot before does not
    /// start a second instance.
    epoch: u64,
    flight: Option<SharedFlight>,
}

#[derive(Clone, Copy, Debug)]
enum Action {
    Wake(InstanceId),
    Poll(InstanceId),
    Start,
}

struct Inner {
    store: Arc<dyn AppStore>,
    driver: Arc<dyn RuntimeDriver>,
    locator: Locator,
    spec: Option<SpecMaker>,
    wait: Duration,
    slots: Mutex<HashMap<Key, Slot>>,
}

/// The map deployment -> instance of the running sandboxes (see the module docs).
#[derive(Clone)]
pub struct SandboxDirectory {
    inner: Arc<Inner>,
}

impl SandboxDirectory {
    /// A directory over the app records of `store` and the instances of `driver`.
    pub fn new(store: Arc<dyn AppStore>, driver: Arc<dyn RuntimeDriver>, locator: Locator) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                driver,
                locator,
                spec: None,
                wait: WAKE_WAIT,
                slots: Mutex::new(HashMap::new()),
            }),
        }
    }

    fn rebuild(self, f: impl FnOnce(&mut Inner)) -> Self {
        let mut inner = Arc::try_unwrap(self.inner).unwrap_or_else(|_| {
            panic!("SandboxDirectory settings are set before the directory is shared")
        });
        f(&mut inner);
        Self {
            inner: Arc::new(inner),
        }
    }

    /// Lets the directory start a deployment that has no running instance.
    pub fn with_spec_maker(self, spec: SpecMaker) -> Self {
        self.rebuild(|i| i.spec = Some(spec))
    }

    /// How long one request waits for a wake or a start (default [`WAKE_WAIT`]).
    pub fn with_wait(self, wait: Duration) -> Self {
        self.rebuild(|i| i.wait = wait)
    }

    /// Records a running instance (the deploy pipeline, after a start). The key comes from the
    /// driver's own status of the instance in `tenant`; an instance of another tenant is not
    /// found.
    pub async fn register(&self, tenant: TenantId, instance: InstanceId) -> Result<()> {
        let st = match self.inner.driver.status(tenant, instance).await {
            Ok(st) => st,
            Err(RuntimeError::NotFound { .. }) => return Err(Error::not_found("instance")),
            Err(e) => return Err(Error::Internal(format!("runtime: {}", e.code()))),
        };
        if st.identity.tenant != tenant {
            return Err(Error::not_found("instance"));
        }
        let key = (tenant, st.identity.deployment, st.identity.environment);
        let mut slots = self.inner.slots.lock().expect("sandbox slots lock");
        let slot = slots.entry(key).or_default();
        slot.instance = Some(instance);
        slot.epoch += 1;
        Ok(())
    }

    /// Forgets the instance of a deployment (it was removed).
    pub fn forget(&self, tenant: TenantId, deployment: Uuid, environment: Environment) {
        let mut slots = self.inner.slots.lock().expect("sandbox slots lock");
        if let Some(slot) = slots.get_mut(&(tenant, deployment, environment)) {
            slot.instance = None;
            slot.epoch += 1;
        }
    }

    /// The instance a query may use, if any (for tests and operators).
    pub fn instance_of(&self, q: &UpstreamQuery) -> Option<InstanceId> {
        let slots = self.inner.slots.lock().expect("sandbox slots lock");
        slots
            .get(&(q.tenant_id, q.deployment_id, runtime_env(q.environment)))
            .and_then(|s| s.instance)
    }

    /// The deployment record of a query when it may have a sandbox (see the module docs).
    async fn record(&self, q: &UpstreamQuery) -> Result<Option<DeploymentRow>> {
        let store = &self.inner.store;
        let Some(app) = store.get_app(q.tenant_id, q.app_id).await? else {
            return Ok(None);
        };
        let Some(dep) = store
            .get_deployment(q.tenant_id, q.app_id, q.deployment_id)
            .await?
        else {
            return Ok(None);
        };
        let ok = dep.tenant_id == q.tenant_id
            && dep.app_node_id == q.app_id
            && dep.environment == record_env(q.environment)
            && serving(&app, &dep, crate::now())
            && is_sandbox(dep.manifest.as_ref());
        Ok(ok.then_some(dep))
    }

    /// Where the gateway sends a request of `q` (see the module docs).
    pub async fn upstream(&self, q: &UpstreamQuery) -> Result<UpstreamAnswer> {
        let Some(row) = self.record(q).await? else {
            return Ok(UpstreamAnswer::NotRunning);
        };
        let key = (q.tenant_id, q.deployment_id, runtime_env(q.environment));
        // At most a few rounds: a round repeats only when a wake or a start ended between the
        // read of the slot and the decision.
        for _ in 0..3 {
            let (instance, epoch) = self.inner.current(&key);
            let action = match instance {
                None => Action::Start,
                Some(id) => match self.inner.driver.status(q.tenant_id, id).await {
                    Ok(st) if !same_identity(&st, q) => {
                        tracing::warn!(
                            target: "ruum::security",
                            event = "sandbox.identity_mismatch",
                            "a sandbox instance does not belong to its deployment"
                        );
                        self.inner.drop_instance(&key, id);
                        return Ok(UpstreamAnswer::NotRunning);
                    }
                    Ok(st) => match st.state {
                        InstanceState::Running => return Ok(self.inner.ready(q, &st).await),
                        InstanceState::Starting => Action::Poll(id),
                        InstanceState::ScaledToZero => Action::Wake(id),
                        InstanceState::Stopped => Action::Start,
                        InstanceState::Exited(_) => return Ok(UpstreamAnswer::Crashed),
                    },
                    Err(RuntimeError::NotFound { .. }) => {
                        self.inner.drop_instance(&key, id);
                        Action::Start
                    }
                    Err(e) => return Err(Error::Internal(format!("runtime: {}", e.code()))),
                },
            };
            if matches!(action, Action::Start) && self.inner.spec.is_none() {
                return Ok(UpstreamAnswer::NotRunning);
            }
            let Some(flight) = self.inner.clone().flight(key, epoch, action, &row) else {
                continue;
            };
            return Ok(match tokio::time::timeout(self.inner.wait, flight).await {
                Err(_) => UpstreamAnswer::Starting,
                Ok(Flight::NotRunning) => UpstreamAnswer::NotRunning,
                Ok(Flight::Status(st)) if !same_identity(&st, q) => UpstreamAnswer::NotRunning,
                Ok(Flight::Status(st)) => match st.state {
                    InstanceState::Running => self.inner.ready(q, &st).await,
                    InstanceState::Starting | InstanceState::ScaledToZero => {
                        UpstreamAnswer::Starting
                    }
                    InstanceState::Exited(_) => UpstreamAnswer::Crashed,
                    InstanceState::Stopped => UpstreamAnswer::NotRunning,
                },
            });
        }
        Ok(UpstreamAnswer::Starting)
    }
}

impl Inner {
    fn current(&self, key: &Key) -> (Option<InstanceId>, u64) {
        let slots = self.slots.lock().expect("sandbox slots lock");
        slots
            .get(key)
            .map(|s| (s.instance, s.epoch))
            .unwrap_or((None, 0))
    }

    fn drop_instance(&self, key: &Key, id: InstanceId) {
        let mut slots = self.slots.lock().expect("sandbox slots lock");
        if let Some(slot) = slots.get_mut(key)
            && slot.instance == Some(id)
        {
            slot.instance = None;
            slot.epoch += 1;
        }
    }

    /// A running instance: touched, then located.
    async fn ready(&self, q: &UpstreamQuery, st: &InstanceStatus) -> UpstreamAnswer {
        if let Err(e) = self.driver.touch(q.tenant_id, st.id).await {
            tracing::debug!(target: "ruum::apps", code = e.code(), "a sandbox touch failed");
        }
        match (self.locator)(st) {
            Some(u) => UpstreamAnswer::Ready { url: u.url() },
            None => {
                tracing::warn!(target: "ruum::apps", "the locator has no address for an instance");
                UpstreamAnswer::NotRunning
            }
        }
    }

    /// The running wake or start of `key`, or a new one. `None` when the slot changed since the
    /// caller read it at `epoch` (the caller looks again).
    fn flight(
        self: Arc<Self>,
        key: Key,
        epoch: u64,
        action: Action,
        row: &DeploymentRow,
    ) -> Option<SharedFlight> {
        let mut slots = self.slots.lock().expect("sandbox slots lock");
        let slot = slots.entry(key).or_default();
        if let Some(f) = &slot.flight {
            return Some(f.clone());
        }
        if slot.epoch != epoch {
            return None;
        }
        let me = self.clone();
        let row = row.clone();
        let task = tokio::spawn(async move {
            let (out, started) = tokio::time::timeout(FLIGHT_LIMIT, me.run(key, action, &row))
                .await
                .unwrap_or((Flight::NotRunning, None));
            me.land(&key, started);
            out
        });
        let shared = async move { task.await.unwrap_or(Flight::NotRunning) }
            .boxed()
            .shared();
        slot.flight = Some(shared.clone());
        Some(shared)
    }

    /// Ends a flight: a started instance becomes the slot's instance.
    fn land(&self, key: &Key, started: Option<InstanceId>) {
        let mut slots = self.slots.lock().expect("sandbox slots lock");
        let slot = slots.entry(*key).or_default();
        if let Some(id) = started {
            slot.instance = Some(id);
        }
        slot.epoch += 1;
        slot.flight = None;
    }

    /// Waits while an instance is starting.
    async fn settle(&self, tenant: TenantId, mut st: InstanceStatus) -> Flight {
        while st.state == InstanceState::Starting {
            tokio::time::sleep(POLL).await;
            st = match self.driver.status(tenant, st.id).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(target: "ruum::apps", code = e.code(), "a sandbox status failed");
                    return Flight::NotRunning;
                }
            };
        }
        Flight::Status(Box::new(st))
    }

    /// One wake, poll or start. Returns the result and the ID of a started instance.
    async fn run(
        &self,
        key: Key,
        action: Action,
        row: &DeploymentRow,
    ) -> (Flight, Option<InstanceId>) {
        let tenant = key.0;
        match action {
            Action::Wake(id) => match self.driver.wake(tenant, id).await {
                Ok(st) => (self.settle(tenant, st).await, None),
                Err(e) => {
                    tracing::warn!(target: "ruum::apps", code = e.code(), "a sandbox wake failed");
                    (Flight::NotRunning, None)
                }
            },
            Action::Poll(id) => match self.driver.status(tenant, id).await {
                Ok(st) => (self.settle(tenant, st).await, None),
                Err(_) => (Flight::NotRunning, None),
            },
            Action::Start => {
                let spec = match self.spec.as_ref() {
                    Some(f) => f(row.clone()).await,
                    None => None,
                };
                let Some(spec) = spec else {
                    return (Flight::NotRunning, None);
                };
                let id = spec.identity;
                if id.tenant != tenant
                    || id.app != row.app_node_id
                    || id.deployment != row.id
                    || id.environment != key.2
                {
                    tracing::warn!(
                        target: "ruum::security",
                        event = "sandbox.spec_mismatch",
                        "a sandbox spec does not belong to its deployment"
                    );
                    return (Flight::NotRunning, None);
                }
                match self.driver.start(spec).await {
                    Ok(st) => {
                        let started = st.id;
                        (self.settle(tenant, st).await, Some(started))
                    }
                    Err(e) => {
                        tracing::warn!(target: "ruum::apps", code = e.code(), "a sandbox start failed");
                        (Flight::NotRunning, None)
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;
    use crate::actor::{Actor, Context};
    use crate::apps::store::{Archive, DeploymentSource, NewDeployment};
    use crate::identity::VerifiedLogin;
    use crate::nodes::OnConflict;
    use crate::runtime::{
        ImageRef, Limits, MemoryDriver, MemoryRegistry, MemoryUsage, OciDigest, SecretValue,
        ServiceSpec, WorkloadIdentity, WorkloadKind,
    };
    use crate::{NodeId, UserId};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The memory driver with counters of starts and wakes.
    struct Counting {
        inner: MemoryDriver,
        starts: AtomicUsize,
        wakes: AtomicUsize,
    }

    #[async_trait]
    impl RuntimeDriver for Counting {
        fn name(&self) -> &str {
            "counting"
        }
        async fn start(
            &self,
            spec: WorkloadSpec,
        ) -> std::result::Result<InstanceStatus, RuntimeError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            self.inner.start(spec).await
        }
        async fn status(
            &self,
            t: TenantId,
            i: InstanceId,
        ) -> std::result::Result<InstanceStatus, RuntimeError> {
            self.inner.status(t, i).await
        }
        async fn stop(&self, t: TenantId, i: InstanceId) -> std::result::Result<(), RuntimeError> {
            self.inner.stop(t, i).await
        }
        async fn touch(&self, t: TenantId, i: InstanceId) -> std::result::Result<(), RuntimeError> {
            self.inner.touch(t, i).await
        }
        async fn wake(
            &self,
            t: TenantId,
            i: InstanceId,
        ) -> std::result::Result<InstanceStatus, RuntimeError> {
            self.wakes.fetch_add(1, Ordering::SeqCst);
            self.inner.wake(t, i).await
        }
        async fn logs(
            &self,
            t: TenantId,
            i: InstanceId,
            after: Option<u64>,
            limit: usize,
        ) -> std::result::Result<Vec<crate::runtime::LogLine>, RuntimeError> {
            self.inner.logs(t, i, after, limit).await
        }
        async fn rotate_token(
            &self,
            t: TenantId,
            i: InstanceId,
            token: SecretValue,
        ) -> std::result::Result<(), RuntimeError> {
            self.inner.rotate_token(t, i, token).await
        }
    }

    struct World {
        s: Services,
        registry: Arc<MemoryRegistry>,
        driver: Arc<Counting>,
    }

    struct Seeded {
        tenant: TenantId,
        app: NodeId,
    }

    fn digest() -> OciDigest {
        OciDigest::of(b"g1b test image")
    }

    fn spec_for(identity: WorkloadIdentity, command: Vec<String>) -> WorkloadSpec {
        WorkloadSpec {
            identity,
            kind: WorkloadKind::Service(ServiceSpec {
                port: 3000,
                health: None,
                idle_timeout: None,
            }),
            image: ImageRef::Oci(digest()),
            command,
            env: Vec::new(),
            previous_secrets: Vec::new(),
            workload_token: SecretValue::new("g1b-token"),
            limits: Limits {
                cpu_millis: 250,
                memory_bytes: 64 << 20,
                disk_bytes: 64 << 20,
            },
            routes: Vec::new(),
        }
    }

    fn identity_of(row: &DeploymentRow) -> WorkloadIdentity {
        WorkloadIdentity {
            tenant: row.tenant_id,
            app: row.app_node_id,
            deployment: row.id,
            environment: match row.environment {
                RecordEnv::Live => Environment::Live,
                RecordEnv::Preview => Environment::Preview,
            },
        }
    }

    async fn world() -> World {
        let registry = MemoryRegistry::new();
        let driver = Arc::new(Counting {
            inner: MemoryDriver::new(registry.clone(), Arc::new(MemoryUsage::new())),
            starts: AtomicUsize::new(0),
            wakes: AtomicUsize::new(0),
        });
        World {
            s: Services::in_memory(),
            registry,
            driver,
        }
    }

    impl World {
        async fn tenant_app(&self, slug: &str) -> (Seeded, UserId) {
            let run = Uuid::now_v7().simple().to_string();
            let owner = self
                .s
                .identity
                .upsert_from_login(&VerifiedLogin {
                    issuer: "https://issuer.g1b.test".into(),
                    subject: format!("{slug}-{run}"),
                    email: Some(format!("{slug}-{run}@example.test")),
                    email_verified: true,
                    name: None,
                    id_token: None,
                })
                .await
                .unwrap();
            let t = self
                .s
                .tenants
                .create_team(&Actor::user(owner.id), slug.into())
                .await
                .unwrap();
            self.s
                .apps
                .store()
                .ensure_tenant_settings(t.id, &[slug.to_string()])
                .await
                .unwrap();
            let ctx = Context {
                tenant: t.id,
                actor: Actor::user(owner.id),
                scope: None,
                idempotency_key: None,
            };
            let app = self
                .s
                .apps
                .create(
                    &ctx,
                    crate::apps::CreateApp {
                        parent_id: t.root_node_id,
                        name: "garden".into(),
                        settings: None,
                        on_conflict: OnConflict::Fail,
                    },
                )
                .await
                .unwrap()
                .value;
            (
                Seeded {
                    tenant: t.id,
                    app: app.node.id,
                },
                owner.id,
            )
        }

        async fn deployment(
            &self,
            seeded: &Seeded,
            owner: UserId,
            environment: RecordEnv,
            state: DeploymentState,
            runtime: &str,
        ) -> DeploymentRow {
            let id = Uuid::now_v7();
            let ctx = Context {
                tenant: seeded.tenant,
                actor: Actor::user(owner),
                scope: None,
                idempotency_key: None,
            };
            let archive = format!("{id} archive");
            let row = self
                .s
                .apps
                .store()
                .insert_deployment(
                    &ctx,
                    NewDeployment {
                        image_digest: None,
                        id,
                        app_node_id: seeded.app,
                        environment,
                        state,
                        short_id: crate::apps::slugs::short_id(),
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
                        manifest: Some(serde_json::json!({ "runtime": runtime, "start": "x" })),
                        stages: serde_json::json!([]),
                        published_from: None,
                        expires_at: None,
                    },
                )
                .await
                .unwrap();
            self.registry.push(&identity_of(&row), &digest());
            row
        }

        fn directory(&self, spec: Option<SpecMaker>) -> SandboxDirectory {
            let locator: Locator = Arc::new(|st: &InstanceStatus| {
                Some(Upstream {
                    host: format!("sandbox-{}.internal", st.identity.deployment.simple()),
                    port: 3000,
                })
            });
            let d =
                SandboxDirectory::new(self.s.apps.store().clone(), self.driver.clone(), locator)
                    .with_wait(Duration::from_secs(2));
            match spec {
                Some(s) => d.with_spec_maker(s),
                None => d,
            }
        }
    }

    fn query(row: &DeploymentRow, env: AppEnv) -> UpstreamQuery {
        UpstreamQuery {
            tenant_id: row.tenant_id,
            app_id: row.app_node_id,
            deployment_id: row.id,
            environment: env,
        }
    }

    fn counting_spec(calls: Arc<AtomicUsize>) -> SpecMaker {
        Arc::new(move |row: DeploymentRow| {
            calls.fetch_add(1, Ordering::SeqCst);
            let spec = Some(spec_for(identity_of(&row), Vec::new()));
            Box::pin(async move { spec })
        })
    }

    #[tokio::test]
    async fn the_locator_result_is_used_as_is_and_a_running_instance_is_touched() {
        let w = world().await;
        let (a, owner) = w.tenant_app("acme").await;
        let row = w
            .deployment(
                &a,
                owner,
                RecordEnv::Preview,
                DeploymentState::Ready,
                "node22",
            )
            .await;
        let st = w
            .driver
            .start(spec_for(identity_of(&row), Vec::new()))
            .await
            .unwrap();
        let d = w.directory(None);
        d.register(a.tenant, st.id).await.unwrap();
        let answer = d.upstream(&query(&row, AppEnv::Preview)).await.unwrap();
        assert_eq!(
            answer,
            UpstreamAnswer::Ready {
                url: format!("http://sandbox-{}.internal:3000", row.id.simple())
            }
        );
        assert_eq!(w.driver.starts.load(Ordering::SeqCst), 1);
        assert_eq!(w.driver.wakes.load(Ordering::SeqCst), 0);
        // The environment is part of the key and is checked against the record.
        assert_eq!(
            d.upstream(&query(&row, AppEnv::Live)).await.unwrap(),
            UpstreamAnswer::NotRunning
        );
        assert_eq!(
            Upstream {
                host: "fd00::1".into(),
                port: 8080
            }
            .url(),
            "http://[fd00::1]:8080"
        );
    }

    #[tokio::test]
    async fn lookups_are_tenant_bound() {
        let w = world().await;
        let (a, owner_a) = w.tenant_app("acme").await;
        let (b, _) = w.tenant_app("other").await;
        let row = w
            .deployment(
                &a,
                owner_a,
                RecordEnv::Preview,
                DeploymentState::Ready,
                "node22",
            )
            .await;
        let st = w
            .driver
            .start(spec_for(identity_of(&row), Vec::new()))
            .await
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let d = w.directory(Some(counting_spec(calls.clone())));
        d.register(a.tenant, st.id).await.unwrap();
        // Positive control: the owner's tenant reaches the instance.
        assert!(matches!(
            d.upstream(&query(&row, AppEnv::Preview)).await.unwrap(),
            UpstreamAnswer::Ready { .. }
        ));
        // Another tenant with the same app and deployment IDs, another tenant's app, a forged
        // deployment ID: no upstream, no start.
        for q in [
            UpstreamQuery {
                tenant_id: b.tenant,
                ..query(&row, AppEnv::Preview)
            },
            UpstreamQuery {
                app_id: b.app,
                ..query(&row, AppEnv::Preview)
            },
            UpstreamQuery {
                tenant_id: b.tenant,
                app_id: b.app,
                ..query(&row, AppEnv::Preview)
            },
            UpstreamQuery {
                deployment_id: Uuid::now_v7(),
                ..query(&row, AppEnv::Preview)
            },
        ] {
            assert_eq!(d.upstream(&q).await.unwrap(), UpstreamAnswer::NotRunning);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(w.driver.starts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_static_or_not_serving_deployment_never_starts_an_instance() {
        let w = world().await;
        let (a, owner) = w.tenant_app("acme").await;
        let calls = Arc::new(AtomicUsize::new(0));
        let d = w.directory(Some(counting_spec(calls.clone())));
        let cases = [
            (RecordEnv::Preview, DeploymentState::Ready, "static"),
            (RecordEnv::Preview, DeploymentState::Pending, "node22"),
            (RecordEnv::Preview, DeploymentState::Failed, "node22"),
            (RecordEnv::Preview, DeploymentState::Expired, "node22"),
            // A live row that is not the app's live deployment.
            (RecordEnv::Live, DeploymentState::Superseded, "node22"),
        ];
        for (env, state, runtime) in cases {
            let row = w.deployment(&a, owner, env, state, runtime).await;
            let e = match env {
                RecordEnv::Live => AppEnv::Live,
                RecordEnv::Preview => AppEnv::Preview,
            };
            assert_eq!(
                d.upstream(&query(&row, e)).await.unwrap(),
                UpstreamAnswer::NotRunning,
                "{env:?} {state:?} {runtime}"
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(w.driver.starts.load(Ordering::SeqCst), 0);
        // Positive control: a ready node22 preview starts.
        let row = w
            .deployment(
                &a,
                owner,
                RecordEnv::Preview,
                DeploymentState::Ready,
                "node22",
            )
            .await;
        assert!(matches!(
            d.upstream(&query(&row, AppEnv::Preview)).await.unwrap(),
            UpstreamAnswer::Ready { .. }
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(w.driver.starts.load(Ordering::SeqCst), 1);
        // Without a spec maker nothing starts.
        let row = w
            .deployment(
                &a,
                owner,
                RecordEnv::Preview,
                DeploymentState::Ready,
                "node22",
            )
            .await;
        assert_eq!(
            w.directory(None)
                .upstream(&query(&row, AppEnv::Preview))
                .await
                .unwrap(),
            UpstreamAnswer::NotRunning
        );
        assert_eq!(w.driver.starts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_instance_of_another_tenant_is_refused() {
        let w = world().await;
        let (a, owner_a) = w.tenant_app("acme").await;
        let (b, owner_b) = w.tenant_app("other").await;
        let row_a = w
            .deployment(
                &a,
                owner_a,
                RecordEnv::Preview,
                DeploymentState::Ready,
                "node22",
            )
            .await;
        let row_b = w
            .deployment(
                &b,
                owner_b,
                RecordEnv::Preview,
                DeploymentState::Ready,
                "node22",
            )
            .await;
        let st_b = w
            .driver
            .start(spec_for(identity_of(&row_b), Vec::new()))
            .await
            .unwrap();
        let d = w.directory(None);
        // B's instance under A's tenant: the driver does not find it.
        assert!(d.register(a.tenant, st_b.id).await.is_err());
        assert_eq!(d.instance_of(&query(&row_a, AppEnv::Preview)), None);
        assert_eq!(
            d.upstream(&query(&row_a, AppEnv::Preview)).await.unwrap(),
            UpstreamAnswer::NotRunning
        );
        // A spec maker that names another tenant's deployment is refused before the driver.
        let bad: SpecMaker = {
            let other = identity_of(&row_b);
            Arc::new(move |_row: DeploymentRow| {
                let spec = Some(spec_for(other, Vec::new()));
                Box::pin(async move { spec })
            })
        };
        let d = w.directory(Some(bad));
        assert_eq!(
            d.upstream(&query(&row_a, AppEnv::Preview)).await.unwrap(),
            UpstreamAnswer::NotRunning
        );
        assert_eq!(w.driver.starts.load(Ordering::SeqCst), 1);
        // The driver refuses a wake across tenants too (the second line).
        assert!(matches!(
            w.driver.wake(a.tenant, st_b.id).await,
            Err(RuntimeError::NotFound { .. })
        ));
        // Positive control: B's own registration works.
        d.register(b.tenant, st_b.id).await.unwrap();
        assert!(matches!(
            d.upstream(&query(&row_b, AppEnv::Preview)).await.unwrap(),
            UpstreamAnswer::Ready { .. }
        ));
    }

    #[tokio::test]
    async fn a_stopped_instance_starts_once_for_many_requests_and_a_crash_is_reported() {
        let w = world().await;
        let (a, owner) = w.tenant_app("acme").await;
        let row = w
            .deployment(
                &a,
                owner,
                RecordEnv::Preview,
                DeploymentState::Ready,
                "node22",
            )
            .await;
        let calls = Arc::new(AtomicUsize::new(0));
        let d = w.directory(Some(counting_spec(calls.clone())));
        let st = w
            .driver
            .start(spec_for(identity_of(&row), Vec::new()))
            .await
            .unwrap();
        d.register(a.tenant, st.id).await.unwrap();
        w.driver.stop(a.tenant, st.id).await.unwrap();
        let q = query(&row, AppEnv::Preview);
        let answers = futures::future::join_all((0..20).map(|_| d.upstream(&q))).await;
        for a in answers {
            assert!(matches!(a.unwrap(), UpstreamAnswer::Ready { .. }));
        }
        // One start at seeding, one for the twenty requests.
        assert_eq!(w.driver.starts.load(Ordering::SeqCst), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_ne!(d.instance_of(&q), Some(st.id));
        // A crashed instance: `crashed`, and no restart.
        let crashing = w
            .driver
            .start(spec_for(identity_of(&row), vec!["exit:1".into()]))
            .await
            .unwrap();
        d.register(a.tenant, crashing.id).await.unwrap();
        for _ in 0..100 {
            if matches!(
                w.driver.status(a.tenant, crashing.id).await.unwrap().state,
                InstanceState::Exited(_)
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(d.upstream(&q).await.unwrap(), UpstreamAnswer::Crashed);
        assert_eq!(w.driver.starts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn an_instance_that_scaled_to_zero_is_woken_once() {
        let w = world().await;
        let (a, owner) = w.tenant_app("acme").await;
        let row = w
            .deployment(
                &a,
                owner,
                RecordEnv::Preview,
                DeploymentState::Ready,
                "node22",
            )
            .await;
        let mut spec = spec_for(identity_of(&row), Vec::new());
        spec.kind = WorkloadKind::Service(ServiceSpec {
            port: 3000,
            health: None,
            idle_timeout: Some(Duration::from_millis(200)),
        });
        let st = w.driver.start(spec).await.unwrap();
        let d = w.directory(None);
        d.register(a.tenant, st.id).await.unwrap();
        for _ in 0..200 {
            if w.driver.status(a.tenant, st.id).await.unwrap().state == InstanceState::ScaledToZero
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            w.driver.status(a.tenant, st.id).await.unwrap().state,
            InstanceState::ScaledToZero
        );
        let q = query(&row, AppEnv::Preview);
        let answers = futures::future::join_all((0..10).map(|_| d.upstream(&q))).await;
        for a in answers {
            assert!(matches!(a.unwrap(), UpstreamAnswer::Ready { .. }));
        }
        assert_eq!(w.driver.wakes.load(Ordering::SeqCst), 1);
        assert_eq!(d.instance_of(&q), Some(st.id));
    }
}
