//! [`DockerDriver`]: the runtime border on a Docker engine (see the crate docs).

use crate::config::{DockerConfig, RuntimeKind};
use crate::egress::{EgressNetwork, EgressRegistry};
use crate::logs::{LogBudget, Redactors, Ring};
use async_trait::async_trait;
use bollard::Docker;
use bollard::container::LogOutput;
use bollard::models::{
    ContainerCreateBody, ContainerInspectResponse, EndpointSettings, HealthConfig,
    HealthStatusEnum, HostConfig, HostConfigLogConfig, NetworkConnectRequest, NetworkCreateRequest,
    NetworkDisconnectRequest,
};
use bollard::query_parameters::{
    AttachContainerOptionsBuilder, CreateContainerOptionsBuilder, CreateImageOptionsBuilder,
    KillContainerOptionsBuilder, RemoveContainerOptionsBuilder, StopContainerOptionsBuilder,
};
use futures::StreamExt;
use ruum_core::ids::TenantId;
use ruum_core::runtime::redact::{MAX_KEPT_TOKENS, check_secret_set, check_secret_value, redact};
use ruum_core::runtime::{
    Applied, EnvValue, ExitReason, ExitStatus, Health, ImageRef, InstanceId, InstanceState,
    InstanceStatus, Limits, LogLine, LogStream, NetworkId, OciDigest, Route, RuntimeDriver,
    RuntimeError, RuntimeUsage, RuntimeUsageSink, SecretValue, TOKEN_FILE, TOKEN_FILE_ENV,
    WorkloadIdentity, WorkloadKind, WorkloadSpec,
};
use ruum_supervisor::protocol::{LAUNCH_VERSION, Launch, SUPERVISOR_PATH};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use time::OffsetDateTime;
use tokio::task::JoinHandle;

/// The user every sandbox runs as. The image's `USER` is ignored.
pub const SANDBOX_USER: &str = "10001:10001";

/// The label keys the driver sets (README, "Docker API surface").
pub const LABEL_APP: &str = "ruum.app";
pub const LABEL_TENANT: &str = "ruum.tenant";
pub const LABEL_ENV: &str = "ruum.env";
pub const LABEL_DEPLOYMENT: &str = "ruum.deployment";
pub const LABEL_INSTANCE: &str = "ruum.instance";
pub const LABEL_MANAGED: &str = "ruum.managed";

/// The environment variables the driver sets itself; a spec that sets one is `invalid_spec`.
pub const RESERVED_ENV: &[&str] = &[
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "https_proxy",
    "http_proxy",
    "NO_PROXY",
    "no_proxy",
    "NODE_USE_ENV_PROXY",
];

const INHIBIT_IPV4: &str = "com.docker.network.bridge.inhibit_ipv4";
const TICK: Duration = Duration::from_millis(100);
const MAX_LOG_BYTES: usize = 4 * 1024 * 1024;
/// How often the supervisor sends SIGKILL again while a container does not end.
const KILL_RETRY: Duration = Duration::from_secs(1);
/// How long after the first SIGKILL the supervisor removes a container that still runs.
const FORCE_AFTER: Duration = Duration::from_secs(10);
/// The tries of one egress unregistration before the network is kept for a later try.
const UNREGISTER_TRIES: u32 = 3;

/// The registry repository of a tenant and app: `ruum/<tenant>/<app>`. Builders push there; the
/// driver pulls only from there.
pub fn repository(identity: &WorkloadIdentity) -> String {
    format!("ruum/{}/{}", identity.tenant.0, identity.app.0)
}

/// The Docker network of an app and environment.
pub fn network_name(identity: &WorkloadIdentity) -> String {
    format!(
        "ruum-app-{}-{}-{}",
        identity.tenant.0.simple(),
        identity.app.0.simple(),
        identity.environment.as_str()
    )
}

fn container_name(id: InstanceId) -> String {
    format!("ruum-i-{}", id.0.simple())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Intent {
    None,
    Stop,
    ScaleToZero,
    Timeout,
}

struct St {
    state: InstanceState,
    intent: Intent,
    /// When `intent` was last set (the supervisor escalates a stop or kill that does not end
    /// the container).
    intent_at: Instant,
    last_touch: Instant,
    run_started: Instant,
    ring: Ring,
    /// The spec's secret values and previous values (redacted for the instance's life).
    base: Vec<String>,
    /// The workload tokens, oldest first: the spec's and each rotation's, at most
    /// [`MAX_KEPT_TOKENS`] (an older token is no longer redacted).
    tokens: VecDeque<String>,
    redactors: Option<Arc<Mutex<Redactors>>>,
    supervisor: Option<JoinHandle<()>>,
    torn_down: bool,
    /// When the instance ended (stopped or exited, its sandbox freed): the start of its
    /// retention ([`DockerConfig::ended_retention`]).
    ended: Option<Instant>,
    /// When the logs of the ended instance were read to the end
    /// ([`DockerConfig::drained_retention`]).
    drained: Option<Instant>,
    /// When the instance scaled to zero (backlog I620-11): its logs left the live budget and
    /// count to the ended byte cap, as in the Kubernetes driver. `None` while it runs.
    parked: Option<Instant>,
}

struct Instance {
    id: InstanceId,
    identity: WorkloadIdentity,
    kind: WorkloadKind,
    routes: Vec<Route>,
    limits: Limits,
    network: Option<String>,
    started_at: OffsetDateTime,
    container: Option<String>,
    token_dir: Option<PathBuf>,
    /// The launch data for the sandbox supervisor (backlog I6-20), given on standard input at
    /// each start; `None` without a supervisor.
    launch: Option<Launch>,
    op: tokio::sync::Mutex<()>,
    st: Mutex<St>,
}

impl St {
    /// Every value to redact: the spec's secrets, previous values and the kept tokens.
    fn secrets(&self) -> Vec<String> {
        self.base
            .iter()
            .chain(self.tokens.iter())
            .cloned()
            .collect()
    }

    fn set_intent(&mut self, intent: Intent) {
        self.intent = intent;
        self.intent_at = Instant::now();
    }

    /// True when nothing of the instance runs any more: stopped or exited, its sandbox freed
    /// (or it never had one), its supervisor done. Only such an instance can be forgotten.
    fn is_ended(&self, has_container: bool) -> bool {
        matches!(
            self.state,
            InstanceState::Stopped | InstanceState::Exited(_)
        ) && (self.torn_down || !has_container)
            && self.supervisor.as_ref().is_none_or(|s| s.is_finished())
    }

    /// Notes the end of the instance (once).
    fn mark_ended(&mut self, has_container: bool) {
        if self.ended.is_none() && self.is_ended(has_container) {
            self.ended = Some(Instant::now());
            self.parked = None;
            // Its logs leave the live budget and count to the ended cap (S-91).
            self.ring.leave_budget();
        }
    }

    /// The instance scaled to zero (backlog I620-11): its logs leave the live budget and count
    /// to the ended byte cap; past the cap they go (the instance keeps the rest, for a wake).
    fn park(&mut self) {
        if self.parked.is_none() {
            self.parked = Some(Instant::now());
        }
        self.ring.leave_budget();
    }

    /// The instance runs again (a wake): its logs count to the live budget again.
    fn unpark(&mut self) {
        self.parked = None;
        self.ring.join_budget();
    }
}

impl Instance {
    fn lock(&self) -> std::sync::MutexGuard<'_, St> {
        self.st.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn secrets(&self) -> Vec<String> {
        self.lock().secrets()
    }

    /// Sets the state `stopped` after a stop and notes the end.
    fn stopped(&self) {
        let mut st = self.lock();
        st.state = InstanceState::Stopped;
        st.mark_ended(self.container.is_some());
    }

    fn health_check(&self) -> bool {
        matches!(&self.kind, WorkloadKind::Service(s) if s.health.is_some())
    }
}

struct NetState {
    id: String,
    routes: Vec<Route>,
    users: HashSet<InstanceId>,
    egress: bool,
    /// `alias:ip` of each attached shared container, for the sandbox's `/etc/hosts`: gVisor
    /// does not reach Docker's embedded DNS (127.0.0.11), so names must not depend on it.
    hosts: Vec<String>,
}

/// The ended instances of one tenant in a sweep ([`Inner::sweep`]): `(ended at, id, log
/// bytes)`, and the sum of their log bytes.
#[derive(Default)]
struct Ended {
    list: Vec<(Instant, InstanceId, usize)>,
    bytes: usize,
}

struct Inner {
    docker: Docker,
    cfg: DockerConfig,
    usage: Arc<dyn RuntimeUsageSink>,
    egress: Arc<dyn EgressRegistry>,
    http: reqwest::Client,
    instances: Mutex<HashMap<InstanceId, Arc<Instance>>>,
    /// The byte budget of the live instances' log rings (backlog S-91).
    live: Arc<LogBudget>,
    networks: tokio::sync::Mutex<HashMap<String, NetState>>,
    /// Networks whose egress unregistration failed: kept (so their subnet stays taken and no
    /// new network gets an overlapping one) and tried again at the next network creation.
    /// Taken only while `networks` is held.
    stale: Mutex<Vec<StaleNet>>,
}

struct StaleNet {
    name: String,
    id: String,
    routes: Vec<Route>,
}

/// The Docker runtime driver (card R-2a). See the crate documentation for the sandbox it
/// builds and the Docker API calls it makes.
pub struct DockerDriver {
    inner: Arc<Inner>,
}

fn sorted(routes: &[Route]) -> Vec<Route> {
    let mut r = routes.to_vec();
    r.sort();
    r
}

fn state_err(operation: &str, state: &InstanceState) -> RuntimeError {
    RuntimeError::InvalidState {
        operation: operation.to_owned(),
        state: state.name().to_owned(),
    }
}

fn status_of(e: &bollard::errors::Error) -> Option<u16> {
    match e {
        bollard::errors::Error::DockerResponseServerError { status_code, .. } => Some(*status_code),
        _ => None,
    }
}

/// True when the network endpoint `(id, c)` is the container `want` (its name or its full ID;
/// never a prefix, which a name could look like).
fn same_container(id: &str, c: &bollard::models::EndpointResource, want: &str) -> bool {
    c.name.as_deref().map(|n| n.trim_start_matches('/')) == Some(want) || id == want
}

fn is_attached(
    containers: &HashMap<String, bollard::models::EndpointResource>,
    container: &str,
) -> bool {
    let want = container.trim_start_matches('/');
    containers.iter().any(|(id, c)| same_container(id, c, want))
}

impl DockerDriver {
    /// Connects to the engine of `cfg.docker_host`, checks the configuration, and (for runsc)
    /// that the engine has the runtime `runsc`: without it the driver refuses to start.
    pub async fn connect(
        cfg: DockerConfig,
        usage: Arc<dyn RuntimeUsageSink>,
        egress: Arc<dyn EgressRegistry>,
    ) -> Result<Self, RuntimeError> {
        cfg.check()?;
        let unavailable = |what: &str, e: bollard::errors::Error| RuntimeError::Unavailable {
            reason: format!("{what}: {e}"),
        };
        let timeout = cfg.api_timeout.as_secs().max(1);
        let docker = if let Some(path) = cfg.docker_host.strip_prefix("unix://") {
            Docker::connect_with_unix(path, timeout, bollard::API_DEFAULT_VERSION)
        } else if cfg.docker_host.starts_with("tcp://") || cfg.docker_host.starts_with("http://") {
            Docker::connect_with_http(&cfg.docker_host, timeout, bollard::API_DEFAULT_VERSION)
        } else {
            return Err(RuntimeError::Internal {
                reason: "RUUM_DOCKER_HOST must start with unix://, tcp:// or http://".to_owned(),
            });
        }
        .map_err(|e| unavailable("the Docker engine", e))?;
        let docker = docker
            .negotiate_version()
            .await
            .map_err(|e| unavailable("the Docker engine", e))?;
        if cfg.runtime == RuntimeKind::Runsc {
            let info = docker
                .info()
                .await
                .map_err(|e| unavailable("the Docker engine", e))?;
            let has = info
                .runtimes
                .as_ref()
                .is_some_and(|r| r.contains_key("runsc"));
            if !has {
                return Err(RuntimeError::Unavailable {
                    reason: "RUUM_RUNTIME_DRIVER=runsc, but the engine has no runtime runsc"
                        .to_owned(),
                });
            }
            // Without --oci-seccomp, runsc does not apply Docker's seccomp profile inside the
            // sandbox (gVisor's own kernel filters the calls instead). Infrastructure (I-6)
            // decides on the flag; the driver says which way the engine runs.
            let oci_seccomp = info
                .runtimes
                .as_ref()
                .and_then(|r| r.get("runsc"))
                .and_then(|r| r.runtime_args.as_ref())
                .is_some_and(|a| a.iter().any(|x| x == "--oci-seccomp"));
            if !oci_seccomp {
                tracing::warn!(
                    "runsc runs without --oci-seccomp: Docker's seccomp profile is not applied inside gVisor"
                );
            }
        }
        if let Some(p) = &cfg.supervisor {
            let ok = std::fs::metadata(p).is_ok_and(|m| m.is_file());
            if !ok {
                return Err(RuntimeError::Internal {
                    reason: format!("the supervisor binary {} is not a file", p.display()),
                });
            }
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&cfg.state_dir)
            .map_err(|e| RuntimeError::Internal {
                reason: format!("the state folder: {e}"),
            })?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .no_proxy()
            .build()
            .map_err(|e| RuntimeError::Internal {
                reason: format!("the registry client: {e}"),
            })?;
        let live = LogBudget::new(cfg.max_live_log_bytes, cfg.max_tenant_live_log_bytes);
        Ok(Self {
            inner: Arc::new(Inner {
                docker,
                cfg,
                usage,
                egress,
                http,
                instances: Mutex::new(HashMap::new()),
                live,
                networks: tokio::sync::Mutex::new(HashMap::new()),
                stale: Mutex::new(Vec::new()),
            }),
        })
    }

    /// The instances the driver keeps, running and ended (for tests and operations).
    pub fn kept_instances(&self) -> usize {
        self.inner
            .instances
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .len()
    }

    /// The memory of the log rings of every kept instance (for tests and operations).
    pub fn kept_log_bytes(&self) -> usize {
        let all: Vec<Arc<Instance>> = self
            .inner
            .instances
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .cloned()
            .collect();
        all.iter().map(|i| i.lock().ring.memory()).sum()
    }

    /// The container name of an instance (for tests and operations; never for an app).
    pub fn container_of(&self, instance: InstanceId) -> Option<String> {
        self.inner.get(instance).and_then(|i| i.container.clone())
    }
}

impl Inner {
    fn get(&self, id: InstanceId) -> Option<Arc<Instance>> {
        self.instances
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&id)
            .cloned()
    }

    fn get_for(&self, tenant: TenantId, id: InstanceId) -> Result<Arc<Instance>, RuntimeError> {
        match self.get(id) {
            Some(i) if i.identity.tenant == tenant => Ok(i),
            _ => Err(RuntimeError::NotFound { instance: id }),
        }
    }

    /// An error of the engine, with every secret value of the instance redacted from its text.
    fn docker_err(
        &self,
        what: &str,
        e: &bollard::errors::Error,
        secrets: &[String],
    ) -> RuntimeError {
        let refs: Vec<&str> = secrets.iter().map(String::as_str).collect();
        RuntimeError::Unavailable {
            reason: redact(&format!("{what}: {e}"), &refs),
        }
    }

    fn labels(&self, identity: &WorkloadIdentity) -> HashMap<String, String> {
        let mut l: HashMap<String, String> = self
            .cfg
            .extra_labels
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        l.insert(LABEL_APP.to_owned(), identity.app.0.to_string());
        l.insert(LABEL_TENANT.to_owned(), identity.tenant.0.to_string());
        l.insert(
            LABEL_ENV.to_owned(),
            identity.environment.as_str().to_owned(),
        );
        l.insert(LABEL_MANAGED.to_owned(), "ruum-runtime".to_owned());
        l
    }

    /// The driver's own checks on top of [`WorkloadSpec::validate`].
    fn check_spec(&self, spec: &WorkloadSpec) -> Result<(), RuntimeError> {
        for v in &spec.env {
            if RESERVED_ENV.contains(&v.name.as_str()) {
                return Err(RuntimeError::invalid(
                    "env",
                    format!("{} is set by the platform", v.name),
                ));
            }
            if let EnvValue::Secret(s) = &v.value {
                check_secret_value("env", s.expose())?;
            }
        }
        for s in &spec.previous_secrets {
            check_secret_value("previous_secrets", s.expose())?;
        }
        check_secret_value("workload_token", spec.workload_token.expose())?;
        check_secret_set(&spec.redacted_values())?;
        // Docker takes the limits as signed 64-bit numbers: a larger one would wrap.
        for (field, v) in [
            ("limits.memory_bytes", spec.limits.memory_bytes),
            ("limits.disk_bytes", spec.limits.disk_bytes),
        ] {
            if i64::try_from(v).is_err() {
                return Err(RuntimeError::invalid(field, "too large"));
            }
        }
        match &spec.kind {
            WorkloadKind::Service(_) | WorkloadKind::Job(_) => {}
            other => {
                return Err(RuntimeError::UnsupportedKind {
                    kind: other.name().to_owned(),
                });
            }
        }
        for r in &spec.routes {
            if !self.cfg.routes.contains_key(r) {
                return Err(RuntimeError::Unavailable {
                    reason: format!("the driver has no shared container for {}", r.as_str()),
                });
            }
        }
        Ok(())
    }

    /// Looks the digest up in the repository of the spec's tenant and app, at the registry
    /// itself: the engine's local store may hold the same digest for another app.
    async fn check_registry(
        &self,
        identity: &WorkloadIdentity,
        digest: &OciDigest,
    ) -> Result<(), RuntimeError> {
        let url = format!(
            "{}/v2/{}/manifests/{}",
            self.cfg.registry_url.trim_end_matches('/'),
            repository(identity),
            digest
        );
        let accept = "application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.docker.distribution.manifest.v2+json";
        let r = self
            .http
            .head(&url)
            .header(reqwest::header::ACCEPT, accept)
            .send()
            .await
            .map_err(|e| RuntimeError::Unavailable {
                reason: format!("the registry: {}", e.without_url()),
            })?;
        match r.status() {
            s if s.is_success() => Ok(()),
            reqwest::StatusCode::NOT_FOUND => Err(RuntimeError::ImageNotFound {
                image: digest.to_string(),
            }),
            s => Err(RuntimeError::Unavailable {
                reason: format!("the registry answered {s}"),
            }),
        }
    }

    async fn ensure_image(&self, reference: &str, digest: &OciDigest) -> Result<(), RuntimeError> {
        if self.docker.inspect_image(reference).await.is_ok() {
            return Ok(());
        }
        let opts = CreateImageOptionsBuilder::new()
            .from_image(reference)
            .build();
        let mut pull = self.docker.create_image(Some(opts), None, None);
        while let Some(item) = pull.next().await {
            if let Err(e) = item {
                return Err(match status_of(&e) {
                    Some(404) => RuntimeError::ImageNotFound {
                        image: digest.to_string(),
                    },
                    _ => RuntimeError::Unavailable {
                        reason: format!("pulling the image: {e}"),
                    },
                });
            }
        }
        Ok(())
    }

    /// Refuses an image that declares volumes: Docker would give each `VOLUME` path an
    /// anonymous, writable volume on the engine's disk, with no size limit, also under a
    /// read-only root. [`Inner::verify`] checks the container's mounts too. Returns the image's
    /// `Entrypoint` and `Cmd` (the supervisor runs them, I6-20).
    async fn check_image(
        &self,
        reference: &str,
    ) -> Result<(Vec<String>, Vec<String>), RuntimeError> {
        let image = self
            .docker
            .inspect_image(reference)
            .await
            .map_err(|e| self.docker_err("inspecting the image", &e, &[]))?;
        let config = image.config.unwrap_or_default();
        let entrypoint = config.entrypoint.clone().unwrap_or_default();
        let cmd = config.cmd.clone().unwrap_or_default();
        let volumes = config.volumes.unwrap_or_default();
        if !volumes.is_empty() {
            return Err(RuntimeError::UnsupportedImage {
                reason: format!(
                    "the image declares volumes ({}); a sandbox has no writable folder but /tmp",
                    volumes.join(", ")
                ),
            });
        }
        Ok((entrypoint, cmd))
    }

    /// Joins the instance to the network of its app and environment, creating it (and attaching
    /// the shared containers of its routes, and registering it with the egress proxy) when it is
    /// the first. A network of the same name that an earlier run of the server left (a restart)
    /// is adopted when its labels name this app and environment and it reads back as the driver
    /// makes it.
    async fn acquire_network(
        &self,
        id: InstanceId,
        spec: &WorkloadSpec,
    ) -> Result<(String, Vec<String>), RuntimeError> {
        let name = network_name(&spec.identity);
        let routes = sorted(&spec.routes);
        let mut nets = self.networks.lock().await;
        if let Some(n) = nets.get_mut(&name) {
            if n.routes != routes {
                return Err(RuntimeError::invalid(
                    "routes",
                    "another instance of this app and environment runs with other routes",
                ));
            }
            // The addresses again: a shared container that restarted can have a new one.
            if !routes.is_empty() {
                n.hosts = self.shared_hosts(&name, &routes).await?;
            }
            n.users.insert(id);
            return Ok((name, n.hosts.clone()));
        }
        self.retry_stale().await;
        let mut labels = self.labels(&spec.identity);
        labels.remove(LABEL_INSTANCE);
        let req = NetworkCreateRequest {
            name: name.clone(),
            driver: Some("bridge".to_owned()),
            internal: Some(true),
            attachable: Some(false),
            options: Some(HashMap::from([(
                INHIBIT_IPV4.to_owned(),
                "true".to_owned(),
            )])),
            labels: Some(labels),
            ..Default::default()
        };
        let (net_id, adopted) = match self.docker.create_network(req).await {
            Ok(created) => (created.id, false),
            Err(e) if status_of(&e) == Some(409) => {
                (self.adopt_network(&name, &spec.identity).await?, true)
            }
            Err(e) => return Err(self.docker_err("creating the network", &e, &[])),
        };
        let result = self.finish_network(&name, &net_id, spec, &routes).await;
        match result {
            Ok((egress, hosts)) => {
                self.stale
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .retain(|s| s.id != net_id);
                nets.insert(
                    name.clone(),
                    NetState {
                        id: net_id,
                        routes,
                        users: HashSet::from([id]),
                        egress,
                        hosts: hosts.clone(),
                    },
                );
                Ok((name, hosts))
            }
            Err(e) => {
                // An adopted network may still hold containers of the earlier run: leave it.
                // A new one goes, with an unregistration in case the proxy took the
                // registration but its answer was lost (DELETE is idempotent).
                if !adopted {
                    let egress = routes.contains(&Route::EgressProxy);
                    self.drop_network(&name, &net_id, &routes, egress).await;
                }
                Err(e)
            }
        }
    }

    /// The ID of an existing network of this name, when it is the driver's network of this app
    /// and environment (its labels); [`Inner::finish_network`] then reads its settings back.
    async fn adopt_network(
        &self,
        name: &str,
        identity: &WorkloadIdentity,
    ) -> Result<String, RuntimeError> {
        let n = self
            .docker
            .inspect_network(
                name,
                None::<bollard::query_parameters::InspectNetworkOptions>,
            )
            .await
            .map_err(|e| self.docker_err("inspecting the existing network", &e, &[]))?;
        let have = n.labels.unwrap_or_default();
        let want = self.labels(identity);
        let ours = [LABEL_MANAGED, LABEL_TENANT, LABEL_APP, LABEL_ENV]
            .iter()
            .all(|k| have.contains_key(*k) && have.get(*k) == want.get(*k));
        if !ours || n.driver.as_deref() != Some("bridge") {
            return Err(RuntimeError::Internal {
                reason: format!(
                    "network {name} exists and is not the driver's network of this app and environment"
                ),
            });
        }
        tracing::info!(network = name, "adopting the network of an earlier run");
        n.id.ok_or_else(|| RuntimeError::Internal {
            reason: format!("network {name} has no ID"),
        })
    }

    /// Tries the unregistration of the kept networks again (once each); a network whose
    /// unregistration passes is removed.
    async fn retry_stale(&self) {
        let stale = std::mem::take(&mut *self.stale.lock().unwrap_or_else(|p| p.into_inner()));
        for s in stale {
            self.drop_network_tries(&s.name, &s.id, &s.routes, true, 1)
                .await;
        }
    }

    async fn finish_network(
        &self,
        name: &str,
        net_id: &str,
        spec: &WorkloadSpec,
        routes: &[Route],
    ) -> Result<(bool, Vec<String>), RuntimeError> {
        let inspect = self
            .docker
            .inspect_network(
                name,
                None::<bollard::query_parameters::InspectNetworkOptions>,
            )
            .await
            .map_err(|e| self.docker_err("inspecting the network", &e, &[]))?;
        let inhibit = inspect
            .options
            .as_ref()
            .and_then(|o| o.get(INHIBIT_IPV4))
            .map(String::as_str);
        if inspect.internal != Some(true)
            || inhibit != Some("true")
            || inspect.enable_ipv6 == Some(true)
        {
            return Err(RuntimeError::Internal {
                reason: format!(
                    "the engine made network {name} with internal={:?}, inhibit_ipv4={inhibit:?}, ipv6={:?}",
                    inspect.internal, inspect.enable_ipv6
                ),
            });
        }
        let subnet = inspect
            .ipam
            .as_ref()
            .and_then(|i| i.config.as_ref())
            .and_then(|c| c.iter().find_map(|x| x.subnet.clone()))
            .ok_or_else(|| RuntimeError::Internal {
                reason: format!("network {name} has no subnet"),
            })?;
        let attached = inspect.containers.unwrap_or_default();
        // An adopted network can hold shared containers of other routes: they leave.
        for (r, shared) in &self.cfg.routes {
            if !routes.contains(r) && is_attached(&attached, &shared.container) {
                self.docker
                    .disconnect_network(
                        net_id,
                        NetworkDisconnectRequest {
                            container: shared.container.clone(),
                            force: Some(true),
                        },
                    )
                    .await
                    .map_err(|e| self.docker_err(&format!("detaching {}", r.as_str()), &e, &[]))?;
            }
        }
        for r in routes {
            let shared = &self.cfg.routes[r];
            if is_attached(&attached, &shared.container) {
                continue;
            }
            self.docker
                .connect_network(
                    net_id,
                    NetworkConnectRequest {
                        container: shared.container.clone(),
                        endpoint_config: Some(EndpointSettings {
                            aliases: Some(vec![shared.alias.clone()]),
                            ..Default::default()
                        }),
                    },
                )
                .await
                .map_err(|e| self.docker_err(&format!("attaching {}", r.as_str()), &e, &[]))?;
        }
        let hosts = if routes.is_empty() {
            Vec::new()
        } else {
            self.shared_hosts(name, routes).await?
        };
        if routes.contains(&Route::EgressProxy) {
            self.egress
                .register(&EgressNetwork {
                    network: net_id.to_owned(),
                    subnet,
                    identity: spec.identity,
                    allow: None,
                })
                .await?;
            return Ok((true, hosts));
        }
        Ok((false, hosts))
    }

    /// Reads the addresses of the attached shared containers back from the engine, as
    /// `alias:ip` lines for the sandbox's `/etc/hosts`.
    async fn shared_hosts(
        &self,
        name: &str,
        routes: &[Route],
    ) -> Result<Vec<String>, RuntimeError> {
        let inspect = self
            .docker
            .inspect_network(
                name,
                None::<bollard::query_parameters::InspectNetworkOptions>,
            )
            .await
            .map_err(|e| self.docker_err("inspecting the network", &e, &[]))?;
        let containers = inspect.containers.unwrap_or_default();
        let mut hosts = Vec::new();
        for r in routes {
            let shared = &self.cfg.routes[r];
            let want = shared.container.trim_start_matches('/');
            let ip = containers
                .iter()
                .find(|(id, c)| same_container(id, c, want))
                .and_then(|(_, c)| c.ipv4_address.as_deref())
                .and_then(|a| a.split('/').next())
                .and_then(|a| a.parse::<std::net::Ipv4Addr>().ok())
                .ok_or_else(|| RuntimeError::Internal {
                    reason: format!(
                        "{} ({want}) has no address on network {name} (attached: {:?})",
                        r.as_str(),
                        containers
                            .values()
                            .map(|c| (c.name.clone(), c.ipv4_address.clone()))
                            .collect::<Vec<_>>()
                    ),
                })?;
            hosts.push(format!("{}:{ip}", shared.alias));
        }
        Ok(hosts)
    }

    async fn drop_network(&self, name: &str, net_id: &str, routes: &[Route], egress: bool) {
        self.drop_network_tries(name, net_id, routes, egress, UNREGISTER_TRIES)
            .await;
    }

    /// Unregisters the network at the egress proxy (up to `tries` times), then detaches the
    /// shared containers and removes it. When the unregistration keeps failing, the network
    /// stays (its subnet stays taken, so no new network gets an overlapping one that the proxy
    /// would refuse) and goes to `stale` for a later try.
    async fn drop_network_tries(
        &self,
        name: &str,
        net_id: &str,
        routes: &[Route],
        egress: bool,
        tries: u32,
    ) {
        if egress {
            let mut done = false;
            for attempt in 0..tries {
                match self.egress.unregister(net_id).await {
                    Ok(()) => {
                        done = true;
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(network = name, attempt, error = %e, "unregistering the network at the egress proxy");
                        if attempt + 1 < tries {
                            tokio::time::sleep(Duration::from_millis(250 << attempt)).await;
                        }
                    }
                }
            }
            if !done {
                self.stale
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(StaleNet {
                        name: name.to_owned(),
                        id: net_id.to_owned(),
                        routes: routes.to_vec(),
                    });
                return;
            }
        }
        for r in routes {
            if let Some(shared) = self.cfg.routes.get(r) {
                let _ = self
                    .docker
                    .disconnect_network(
                        net_id,
                        NetworkDisconnectRequest {
                            container: shared.container.clone(),
                            force: Some(true),
                        },
                    )
                    .await;
            }
        }
        if let Err(e) = self.docker.remove_network(net_id).await
            && status_of(&e) != Some(404)
        {
            tracing::warn!(network = name, error = %e, "removing the network");
        }
    }

    async fn release_network(&self, id: InstanceId, name: &str) {
        let mut nets = self.networks.lock().await;
        let gone = match nets.get_mut(name) {
            Some(n) => {
                n.users.remove(&id);
                n.users.is_empty()
            }
            None => false,
        };
        if gone && let Some(n) = nets.remove(name) {
            self.drop_network(name, &n.id, &n.routes, n.egress).await;
        }
    }

    fn write_token(path: &std::path::Path, token: &str, create: bool) -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(create)
            .truncate(false)
            .mode(0o644)
            .open(path)?;
        // In place (same inode): the engine bind-mounted this file, so a new file would not
        // reach the sandbox.
        f.seek(SeekFrom::Start(0))?;
        f.write_all(token.as_bytes())?;
        f.set_len(token.len() as u64)?;
        f.sync_all()
    }

    /// The binds of a sandbox: the token file, and with a supervisor its binary (both read-only).
    fn binds(&self, token_file: &std::path::Path) -> Vec<String> {
        let mut b = vec![format!("{}:{TOKEN_FILE}:ro", token_file.display())];
        if let Some(sup) = &self.cfg.supervisor {
            b.push(format!("{}:{SUPERVISOR_PATH}:ro", sup.display()));
        }
        b
    }

    /// The entrypoint with a supervisor: it reads the launch data on standard input.
    fn supervisor_entrypoint() -> Vec<String> {
        [
            SUPERVISOR_PATH,
            "run",
            "--launch-stdin",
            "--token-file",
            TOKEN_FILE,
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
    }

    fn container_body(
        &self,
        spec: &WorkloadSpec,
        image: &str,
        network: &str,
        hosts: &[String],
        token_file: &std::path::Path,
        id: InstanceId,
    ) -> ContainerCreateBody {
        let supervised = self.cfg.supervisor.is_some();
        // With a supervisor the secret values come on standard input, never in Config.Env.
        let mut env: Vec<String> = spec
            .env
            .iter()
            .filter_map(|v| match &v.value {
                EnvValue::Plain(p) => Some(format!("{}={p}", v.name)),
                EnvValue::Secret(_) if supervised => None,
                EnvValue::Secret(s) => Some(format!("{}={}", v.name, s.expose())),
            })
            .collect();
        env.push(format!("{TOKEN_FILE_ENV}={TOKEN_FILE}"));
        if spec.routes.contains(&Route::EgressProxy) {
            let p = &self.cfg.routes[&Route::EgressProxy];
            let proxy = format!("http://{}:{}", p.alias, p.port);
            for name in ["HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy"] {
                env.push(format!("{name}={proxy}"));
            }
            let mut no: Vec<String> = vec!["localhost".to_owned(), "127.0.0.1".to_owned()];
            for r in &spec.routes {
                if *r != Route::EgressProxy {
                    no.push(self.cfg.routes[r].alias.clone());
                }
            }
            env.push(format!("NO_PROXY={}", no.join(",")));
            env.push(format!("no_proxy={}", no.join(",")));
            env.push("NODE_USE_ENV_PROXY=1".to_owned());
        }
        let healthcheck = match &spec.kind {
            WorkloadKind::Service(s) => match &s.health {
                Some(h) => {
                    let mut test = self.cfg.health_command.clone();
                    test.push(format!("http://127.0.0.1:{}{}", s.port, h.path));
                    let interval = h.interval.max(Duration::from_millis(1));
                    Some(HealthConfig {
                        test: Some(test),
                        interval: Some(interval.as_nanos().min(i64::MAX as u128) as i64),
                        timeout: Some(Duration::from_secs(5).as_nanos() as i64),
                        retries: Some(i64::from(h.failure_threshold)),
                        start_period: Some(self.cfg.start_period.as_nanos() as i64),
                        start_interval: Some(self.cfg.start_interval.as_nanos() as i64),
                    })
                }
                None => Some(HealthConfig {
                    test: Some(vec!["NONE".to_owned()]),
                    ..Default::default()
                }),
            },
            _ => Some(HealthConfig {
                test: Some(vec!["NONE".to_owned()]),
                ..Default::default()
            }),
        };
        let mut labels = self.labels(&spec.identity);
        labels.insert(
            LABEL_DEPLOYMENT.to_owned(),
            spec.identity.deployment.to_string(),
        );
        labels.insert(LABEL_INSTANCE.to_owned(), id.to_string());
        let host = HostConfig {
            readonly_rootfs: Some(true),
            cap_drop: Some(vec!["ALL".to_owned()]),
            security_opt: Some(vec!["no-new-privileges".to_owned()]),
            privileged: Some(false),
            memory: Some(spec.limits.memory_bytes as i64),
            memory_swap: Some(spec.limits.memory_bytes as i64),
            nano_cpus: Some(i64::from(spec.limits.cpu_millis) * 1_000_000),
            pids_limit: Some(self.cfg.pids_limit),
            tmpfs: Some(HashMap::from([(
                "/tmp".to_owned(),
                format!("rw,noexec,nosuid,nodev,size={}", spec.limits.disk_bytes),
            )])),
            binds: Some(self.binds(token_file)),
            network_mode: Some(network.to_owned()),
            extra_hosts: if hosts.is_empty() {
                None
            } else {
                Some(hosts.to_vec())
            },
            ipc_mode: Some("private".to_owned()),
            init: Some(true),
            publish_all_ports: Some(false),
            log_config: Some(HostConfigLogConfig {
                typ: Some("none".to_owned()),
                config: None,
            }),
            runtime: Some(self.cfg.runtime.docker_name().to_owned()),
            ..Default::default()
        };
        let (entrypoint, cmd) = if supervised {
            // The supervisor runs the app's command from the launch data.
            (Some(Self::supervisor_entrypoint()), None)
        } else if spec.command.is_empty() {
            (None, None)
        } else {
            (None, Some(spec.command.clone()))
        };
        ContainerCreateBody {
            image: Some(image.to_owned()),
            entrypoint,
            cmd,
            env: Some(env),
            user: Some(SANDBOX_USER.to_owned()),
            labels: Some(labels),
            healthcheck,
            stop_timeout: Some(self.cfg.stop_timeout.as_secs() as i64),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            attach_stdin: Some(supervised),
            tty: Some(false),
            open_stdin: Some(supervised),
            stdin_once: Some(supervised),
            network_disabled: Some(false),
            host_config: Some(host),
            ..Default::default()
        }
    }

    /// Reads the sandbox back from the engine: every flag as asked, and the limits that the
    /// engine applied (a mismatch is an error, not a report).
    fn verify(
        &self,
        c: &ContainerInspectResponse,
        spec: &WorkloadSpec,
        network: &str,
        hosts: &[String],
        token_file: &std::path::Path,
    ) -> Result<Limits, String> {
        let h = c.host_config.as_ref().ok_or("no host config")?;
        let cfg = c.config.as_ref().ok_or("no config")?;
        let mut bad = Vec::new();
        let mut want = |ok: bool, what: &str| {
            if !ok {
                bad.push(what.to_owned());
            }
        };
        want(h.readonly_rootfs == Some(true), "read-only root");
        want(
            h.cap_drop.as_deref() == Some(&["ALL".to_owned()][..]),
            "cap-drop ALL",
        );
        want(
            h.cap_add.as_ref().is_none_or(|c| c.is_empty()),
            "no added capability",
        );
        let sec = h.security_opt.clone().unwrap_or_default();
        want(
            sec.iter()
                .any(|s| s == "no-new-privileges" || s == "no-new-privileges:true"),
            "no-new-privileges",
        );
        want(
            !sec.iter().any(|s| s.contains("unconfined")),
            "no unconfined profile",
        );
        want(h.privileged != Some(true), "not privileged");
        want(
            h.network_mode.as_deref() == Some(network),
            "the app network",
        );
        want(
            h.pid_mode.as_deref().unwrap_or("").is_empty(),
            "own PID namespace",
        );
        want(
            h.uts_mode.as_deref().unwrap_or("").is_empty(),
            "own UTS namespace",
        );
        want(
            h.userns_mode.as_deref().unwrap_or("").is_empty(),
            "no host user namespace",
        );
        want(h.ipc_mode.as_deref() == Some("private"), "private IPC");
        want(
            h.extra_hosts.as_deref().unwrap_or_default() == hosts,
            "only the shared containers in /etc/hosts",
        );
        want(h.init == Some(true), "init");
        want(h.publish_all_ports != Some(true), "no published ports");
        want(
            h.port_bindings.as_ref().is_none_or(|p| p.is_empty()),
            "no port bindings",
        );
        want(
            h.devices.as_ref().is_none_or(|d| d.is_empty()),
            "no devices",
        );
        want(h.mounts.as_ref().is_none_or(|m| m.is_empty()), "no mounts");
        want(
            h.binds.as_deref() == Some(&self.binds(token_file)[..]),
            "only the token file (and the supervisor) bound",
        );
        // The mounts the engine made: the token bind (and the supervisor's) only. An anonymous
        // volume (an image `VOLUME`) shows only here, not in `HostConfig.Mounts` or `Binds`;
        // tmpfs is not here.
        let supervised = self.cfg.supervisor.is_some();
        let mut dests = vec![TOKEN_FILE];
        if supervised {
            dests.push(SUPERVISOR_PATH);
        }
        let mounts = c.mounts.as_deref().unwrap_or_default();
        let mounts_ok = mounts.len() == dests.len()
            && mounts.iter().all(|m| {
                m.typ.as_deref() == Some("bind")
                    && m.destination.as_deref().is_some_and(|d| dests.contains(&d))
                    && m.rw == Some(false)
            });
        want(
            mounts_ok,
            "only the token file (and the supervisor) mounted (no volume)",
        );
        if supervised {
            want(
                cfg.entrypoint.as_deref() == Some(&Self::supervisor_entrypoint()[..]),
                "the supervisor as the entrypoint",
            );
            want(
                cfg.open_stdin == Some(true) && cfg.stdin_once == Some(true),
                "standard input open once (the launch data)",
            );
            // No secret value in Config.Env.
            let env = cfg.env.clone().unwrap_or_default();
            let leaked = spec.env.iter().any(|v| match &v.value {
                EnvValue::Secret(s) => env.contains(&format!("{}={}", v.name, s.expose())),
                EnvValue::Plain(_) => false,
            });
            want(!leaked, "no secret value in the container's environment");
        }
        want(h.pids_limit == Some(self.cfg.pids_limit), "pids limit");
        want(h.memory_swap == h.memory, "swap equals memory");
        want(
            h.runtime.as_deref() == Some(self.cfg.runtime.docker_name()),
            "the runtime",
        );
        want(cfg.user.as_deref() == Some(SANDBOX_USER), "user 10001");
        want(
            c.app_armor_profile.as_deref() != Some("unconfined"),
            "no AppArmor unconfined",
        );
        let disk = h
            .tmpfs
            .as_ref()
            .and_then(|t| t.get("/tmp"))
            .and_then(|o| o.split(',').find_map(|x| x.strip_prefix("size=")))
            .and_then(|s| s.parse::<u64>().ok());
        let applied = Limits {
            cpu_millis: h.nano_cpus.map_or(0, |n| (n / 1_000_000) as u32),
            memory_bytes: h.memory.map_or(0, |m| m as u64),
            disk_bytes: disk.unwrap_or(0),
        };
        want(applied == spec.limits, "the limits");
        if bad.is_empty() {
            Ok(applied)
        } else {
            Err(bad.join(", "))
        }
    }

    async fn remove_container(&self, name: &str) {
        let opts = RemoveContainerOptionsBuilder::new()
            .force(true)
            .v(true)
            .build();
        if let Err(e) = self.docker.remove_container(name, Some(opts)).await
            && status_of(&e) != Some(404)
        {
            tracing::warn!(container = name, error = %e, "removing a container");
        }
    }

    /// Frees the sandbox of an instance: the container, the token folder and its place on the
    /// network (the last one removes the network).
    async fn teardown(&self, inst: &Instance) {
        {
            let mut st = inst.lock();
            if st.torn_down {
                return;
            }
            st.torn_down = true;
        }
        if let Some(c) = &inst.container {
            self.remove_container(c).await;
        }
        if let Some(d) = &inst.token_dir {
            let _ = std::fs::remove_dir_all(d);
        }
        if let Some(n) = &inst.network {
            self.release_network(inst.id, n).await;
        }
    }

    /// One run of the container: attach, start, then the supervisor.
    async fn begin_run(self: &Arc<Self>, inst: &Arc<Instance>) -> Result<(), RuntimeError> {
        let name = inst
            .container
            .clone()
            .ok_or_else(|| RuntimeError::Internal {
                reason: "no container".to_owned(),
            })?;
        let secrets = inst.secrets();
        // A wake: the logs of the instance count to the live budget again (backlog I620-11).
        inst.lock().unpark();
        let opts = AttachContainerOptionsBuilder::new()
            .stream(true)
            .stdin(inst.launch.is_some())
            .stdout(true)
            .stderr(true)
            .logs(false)
            .build();
        let attached = self
            .docker
            .attach_container(&name, Some(opts))
            .await
            .map_err(|e| self.docker_err("attaching to the container", &e, &secrets))?;
        let mut input = attached.input;
        let redactors = Arc::new(Mutex::new(Redactors::new(
            &secrets,
            self.cfg.max_line_bytes,
        )));
        let pump = tokio::spawn(pump(inst.clone(), redactors.clone(), attached.output));
        if let Err(e) = self.docker.start_container(&name, None).await {
            pump.abort();
            return Err(self.docker_err("starting the container", &e, &secrets));
        }
        // The launch data on standard input, then its end (StdinOnce closes the container's
        // standard input): the supervisor reads it before the app starts (I6-20).
        if let Some(launch) = &inst.launch {
            use tokio::io::AsyncWriteExt;
            let data = serde_json::to_vec(launch).map_err(|_| RuntimeError::Internal {
                reason: "the launch data does not serialize".to_owned(),
            })?;
            let sent = tokio::time::timeout(self.cfg.api_timeout, async {
                input.write_all(&data).await?;
                input.flush().await?;
                input.shutdown().await
            })
            .await;
            if !matches!(sent, Ok(Ok(()))) {
                pump.abort();
                self.kill(&name).await;
                return Err(RuntimeError::Unavailable {
                    reason: "the launch data did not reach the sandbox supervisor".to_owned(),
                });
            }
        } else {
            drop(input);
        }
        {
            let mut st = inst.lock();
            st.state = InstanceState::Running;
            st.set_intent(Intent::None);
            st.run_started = Instant::now();
            st.last_touch = Instant::now();
            st.redactors = Some(redactors);
        }
        let sup = tokio::spawn(supervise(self.clone(), inst.clone(), pump));
        inst.lock().supervisor = Some(sup);
        Ok(())
    }

    /// Forgets ended instances (backlog R2A-5): those past [`DockerConfig::ended_retention`],
    /// or past [`DockerConfig::drained_retention`] after their logs were read to the end; then,
    /// while more than [`DockerConfig::max_ended_instances`] are kept or their logs take more
    /// than [`DockerConfig::max_ended_log_bytes`], the oldest ended instance of the tenant with
    /// the largest share: the most ended log bytes when the byte cap is over, else the most
    /// ended instances. So one tenant's load evicts its own ended instances first, not another
    /// tenant's (S-61).
    ///
    /// With `room` (the tenant that starts an instance), while [`DockerConfig::max_instances`]
    /// or more are kept: the oldest ended instance of the tenant that keeps the most instances,
    /// running and ended, with the new one counted for the starting tenant; when that tenant
    /// has no ended instance, only one of the starting tenant's own. So a tenant that fills the
    /// driver with running instances does not forget another tenant's ended instance (its status
    /// and logs); its start is `unavailable` instead (Kimi review of run 12, K11 finding 3,
    /// backlog S-67).
    ///
    /// An instance with an operation in progress is left alone. Called with the map held.
    fn sweep(&self, map: &mut HashMap<InstanceId, Arc<Instance>>, room: Option<TenantId>) {
        let now = Instant::now();
        let cfg = &self.cfg;
        let mut evict = Vec::new();
        let mut ended: HashMap<TenantId, Ended> = HashMap::new();
        // The rings of instances scaled to zero (backlog I620-11): they count to the byte cap,
        // and past it the oldest of them (of the tenant with the most bytes) loses its lines.
        let mut parked: HashMap<TenantId, Ended> = HashMap::new();
        // The instances each tenant keeps after this sweep, running and ended.
        let mut totals: HashMap<TenantId, usize> = HashMap::new();
        if let Some(t) = room {
            totals.insert(t, 0);
        }
        for (id, inst) in map.iter() {
            *totals.entry(inst.identity.tenant).or_default() += 1;
            let Ok(_op) = inst.op.try_lock() else {
                continue;
            };
            let mut st = inst.lock();
            st.mark_ended(inst.container.is_some());
            let Some(at) = st.ended else {
                if let Some(at) = st.parked
                    && st.ring.memory() > 0
                {
                    let p = parked.entry(inst.identity.tenant).or_default();
                    let b = st.ring.memory();
                    p.list.push((at, *id, b));
                    p.bytes += b;
                }
                continue;
            };
            let expired = now.duration_since(at) >= cfg.ended_retention
                || st
                    .drained
                    .is_some_and(|d| now.duration_since(d) >= cfg.drained_retention);
            if expired {
                evict.push(*id);
                if let Some(n) = totals.get_mut(&inst.identity.tenant) {
                    *n -= 1;
                }
            } else {
                let e = ended.entry(inst.identity.tenant).or_default();
                let b = st.ring.memory();
                e.list.push((at, *id, b));
                e.bytes += b;
            }
        }
        for e in ended.values_mut().chain(parked.values_mut()) {
            // Newest first, so the oldest is popped from the end.
            e.list.sort_by_key(|(at, _, _)| std::cmp::Reverse(*at));
        }
        let mut count: usize = ended.values().map(|e| e.list.len()).sum();
        let mut bytes: usize = ended.values().chain(parked.values()).map(|e| e.bytes).sum();
        let mut kept = map.len() - evict.len();
        let oldest = |e: &Ended| e.list.last().map(|(at, _, _)| std::cmp::Reverse(*at));
        loop {
            let by_bytes = bytes > cfg.max_ended_log_bytes;
            let by_count = count > cfg.max_ended_instances;
            let bytes_of = |t: &TenantId| {
                ended.get(t).map_or(0, |e| e.bytes) + parked.get(t).map_or(0, |e| e.bytes)
            };
            if by_bytes {
                // Past the byte cap the tenant with the most ended and parked bytes gives first:
                // its oldest parked ring loses its lines when it is older than the tenant's
                // oldest ended instance (backlog I620-11, as the Kubernetes driver).
                let top = ended
                    .iter()
                    .chain(parked.iter())
                    .filter(|(_, e)| !e.list.is_empty())
                    .map(|(t, _)| *t)
                    .max_by_key(|t| bytes_of(t));
                if let Some(t) = top
                    && let Some((pat, pid, pb)) =
                        parked.get(&t).and_then(|p| p.list.last().copied())
                    && ended
                        .get(&t)
                        .and_then(|e| e.list.last())
                        .is_none_or(|(eat, _, _)| pat < *eat)
                {
                    if let Some(i) = map.get(&pid) {
                        i.lock().ring.clear();
                    }
                    if let Some(p) = parked.get_mut(&t) {
                        p.list.pop();
                        p.bytes -= pb;
                    }
                    bytes -= pb;
                    continue;
                }
            }
            let victim = if by_bytes || by_count {
                // The tenant with the largest share (ended and parked bytes, or ended
                // instances); on a tie, the one with the oldest instance.
                ended
                    .iter()
                    .filter(|(_, e)| !e.list.is_empty())
                    .max_by_key(|(t, e)| {
                        let share = if by_bytes { bytes_of(t) } else { e.list.len() };
                        (share, oldest(e))
                    })
                    .map(|(t, _)| *t)
            } else if let Some(starter) = room
                && kept >= cfg.max_instances
            {
                let has_ended = |t: &TenantId| ended.get(t).is_some_and(|e| !e.list.is_empty());
                // The tenant that keeps the most, the new instance counted for the starter; on
                // a tie, one with an ended instance, then the one with the oldest.
                let top = totals
                    .iter()
                    .max_by_key(|(t, n)| {
                        let n = **n + usize::from(**t == starter);
                        (n, has_ended(t), ended.get(*t).and_then(oldest))
                    })
                    .map(|(t, _)| *t);
                match top {
                    Some(t) if has_ended(&t) => Some(t),
                    _ if has_ended(&starter) => Some(starter),
                    _ => None,
                }
            } else {
                break;
            };
            let Some(tenant) = victim else {
                break;
            };
            let Some(e) = ended.get_mut(&tenant) else {
                break;
            };
            let Some((_, id, b)) = e.list.pop() else {
                break;
            };
            e.bytes -= b;
            if let Some(n) = totals.get_mut(&tenant) {
                *n -= 1;
            }
            evict.push(id);
            count -= 1;
            bytes -= b;
            kept -= 1;
        }
        for id in evict {
            map.remove(&id);
        }
        self.shed_live(map);
    }

    /// Brings the live log budget back under its bound (S-91): the oldest lines of the tenant
    /// with the most live log bytes go first, from its largest ring.
    fn shed_live(&self, map: &HashMap<InstanceId, Arc<Instance>>) {
        for _ in 0..map.len().max(1) {
            let Some((excess, top)) = self.live.over_total() else {
                return;
            };
            let largest = map
                .values()
                .filter(|i| i.identity.tenant == top)
                .map(|i| {
                    let st = i.lock();
                    (
                        if st.ring.counted() {
                            st.ring.memory()
                        } else {
                            0
                        },
                        i.clone(),
                    )
                })
                .max_by_key(|(b, _)| *b);
            match largest {
                Some((b, inst)) if b > 0 => {
                    inst.lock().ring.shed(excess);
                }
                _ => return,
            }
        }
    }

    /// Forgets the ended instances that are due ([`Inner::sweep`]).
    fn sweep_now(&self) {
        let mut map = self.instances.lock().unwrap_or_else(|p| p.into_inner());
        self.sweep(&mut map, None);
    }

    /// Makes room for one more instance of `tenant`: `unavailable` when
    /// [`DockerConfig::max_instances`] instances are kept and none of them may be forgotten for
    /// it ([`Inner::sweep`]).
    fn check_room(&self, tenant: TenantId) -> Result<(), RuntimeError> {
        let mut map = self.instances.lock().unwrap_or_else(|p| p.into_inner());
        self.sweep(&mut map, Some(tenant));
        self.room_in(&map)
    }

    fn room_in(&self, map: &HashMap<InstanceId, Arc<Instance>>) -> Result<(), RuntimeError> {
        if map.len() >= self.cfg.max_instances {
            return Err(RuntimeError::Unavailable {
                reason: format!(
                    "the driver keeps its maximum of {} instances",
                    self.cfg.max_instances
                ),
            });
        }
        Ok(())
    }

    /// Keeps a new instance, within [`DockerConfig::max_instances`].
    fn keep(&self, inst: &Arc<Instance>) -> Result<(), RuntimeError> {
        let mut map = self.instances.lock().unwrap_or_else(|p| p.into_inner());
        self.sweep(&mut map, Some(inst.identity.tenant));
        self.room_in(&map)?;
        map.insert(inst.id, inst.clone());
        Ok(())
    }

    async fn health(&self, inst: &Instance) -> Health {
        let Some(c) = &inst.container else {
            return Health::Healthy;
        };
        if !inst.health_check() {
            return Health::Unknown;
        }
        match self
            .docker
            .inspect_container(
                c,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(r) => match r.state.and_then(|s| s.health).and_then(|h| h.status) {
                Some(HealthStatusEnum::HEALTHY) => Health::Healthy,
                Some(HealthStatusEnum::UNHEALTHY) => Health::Unhealthy,
                _ => Health::Unknown,
            },
            Err(_) => Health::Unknown,
        }
    }

    async fn status(&self, inst: &Instance) -> InstanceStatus {
        let state = inst.lock().state;
        let health = match (&inst.container, state) {
            (None, _) => Health::Healthy,
            (Some(_), InstanceState::Running) => self.health(inst).await,
            _ => Health::Unknown,
        };
        InstanceStatus {
            id: inst.id,
            identity: inst.identity,
            kind: inst.kind.name(),
            state,
            health,
            applied: Applied {
                routes: inst.routes.clone(),
                network: inst.network.clone().map(NetworkId),
                limits: inst.limits,
            },
            started_at: inst.started_at,
        }
    }
}

/// Copies the container's output through the redactors into the ring, line by line.
async fn pump(
    inst: Arc<Instance>,
    redactors: Arc<Mutex<Redactors>>,
    mut output: std::pin::Pin<
        Box<dyn futures::Stream<Item = Result<LogOutput, bollard::errors::Error>> + Send>,
    >,
) {
    let push = |inst: &Instance, stream: LogStream, lines: Vec<(OffsetDateTime, String)>| {
        if lines.is_empty() {
            return;
        }
        let mut st = inst.lock();
        for (at, text) in lines {
            st.ring.push(inst.id, inst.identity, stream, at, text);
        }
    };
    while let Some(item) = output.next().await {
        let Ok(item) = item else { break };
        let now = OffsetDateTime::now_utc();
        let (stream, lines) = {
            let mut r = redactors.lock().unwrap_or_else(|p| p.into_inner());
            match item {
                LogOutput::StdOut { message } | LogOutput::Console { message } => {
                    (LogStream::Stdout, r.out.push_bytes(&message, now))
                }
                LogOutput::StdErr { message } => {
                    (LogStream::Stderr, r.err.push_bytes(&message, now))
                }
                LogOutput::StdIn { .. } => continue,
            }
        };
        push(&inst, stream, lines);
    }
    let now = OffsetDateTime::now_utc();
    let (out, err) = {
        let mut r = redactors.lock().unwrap_or_else(|p| p.into_inner());
        (r.out.finish(now), r.err.finish(now))
    };
    push(&inst, LogStream::Stdout, out);
    push(&inst, LogStream::Stderr, err);
}

impl Inner {
    /// Sends SIGKILL; an error other than "gone" or "not running" is logged.
    async fn kill(&self, name: &str) {
        let opts = KillContainerOptionsBuilder::new().signal("SIGKILL").build();
        if let Err(e) = self.docker.kill_container(name, Some(opts)).await
            && !matches!(status_of(&e), Some(404 | 409))
        {
            tracing::warn!(container = name, error = %e, "killing a container");
        }
    }

    /// Asks the container to stop (SIGTERM, SIGKILL after the stop timeout). When the engine
    /// refuses, SIGKILL at once; the supervisor escalates further.
    async fn stop_container(&self, name: &str) {
        let t = self.cfg.stop_timeout.as_secs() as i32;
        let opts = StopContainerOptionsBuilder::new().t(t).build();
        if let Err(e) = self.docker.stop_container(name, Some(opts)).await
            && !matches!(status_of(&e), Some(304 | 404))
        {
            tracing::warn!(container = name, error = %e, "stopping a container");
            self.kill(name).await;
        }
    }

    /// True unless the engine says the container is gone or not running. An engine that does
    /// not answer counts as running: the supervisor waits again.
    async fn still_running(&self, name: &str) -> bool {
        match self
            .docker
            .inspect_container(
                name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(c) => c.state.and_then(|s| s.running) == Some(true),
            Err(e) => status_of(&e) != Some(404),
        }
    }

    /// How long a supervisor can take to end after a stop, a scale to zero or a kill: the stop
    /// timeout, the SIGKILL retries and the forced removal, with a margin.
    fn end_bound(&self) -> Duration {
        self.cfg.stop_timeout + KILL_RETRY * 2 + FORCE_AFTER + Duration::from_secs(10)
    }
}

/// Waits for a supervisor, at most `bound`; false (and the task aborted) when it did not end.
async fn join_bounded(mut task: JoinHandle<()>, bound: Duration) -> bool {
    match tokio::time::timeout(bound, &mut task).await {
        Ok(_) => true,
        Err(_) => {
            task.abort();
            false
        }
    }
}

/// Watches one run: the job timeout, the idle timer of a service, the end of the process.
/// A stop, scale to zero or kill that does not end the container is escalated: SIGKILL again
/// each [`KILL_RETRY`], and after [`FORCE_AFTER`] the container is removed by force. An error
/// on the wait stream (a short failure of the engine or its socket proxy) ends the run only
/// when the container no longer runs.
async fn supervise(inner: Arc<Inner>, inst: Arc<Instance>, pump: JoinHandle<()>) {
    let Some(name) = inst.container.clone() else {
        return;
    };
    let new_wait = || {
        Box::pin(inner.docker.wait_container(
            &name,
            None::<bollard::query_parameters::WaitContainerOptions>,
        ))
    };
    let mut wait = new_wait();
    let mut tick = tokio::time::interval(TICK);
    let mut ready = !inst.health_check();
    let mut last_kill: Option<Instant> = None;
    let mut forced = false;
    loop {
        tokio::select! {
            item = wait.next() => {
                // A non-zero exit is an error item too; the engine's state decides.
                let clean = matches!(item, Some(Ok(_)));
                if !clean && !forced && inner.still_running(&name).await {
                    tracing::warn!(container = %name, "the wait stream ended while the container runs; waiting again");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    wait = new_wait();
                    continue;
                }
                break;
            }
            _ = tick.tick() => {
                // The idle clock of a service starts when the run is ready (its first passing
                // health check), so a slow start (runsc at a small CPU limit) is not taken for
                // idle time. A run that never gets healthy counts as ready after the start period.
                if !ready {
                    let started = inst.lock().run_started.elapsed();
                    if started >= inner.cfg.start_period
                        || inner.health(&inst).await == Health::Healthy
                    {
                        ready = true;
                        inst.lock().last_touch = Instant::now();
                    }
                }
                let action = {
                    let mut st = inst.lock();
                    if st.intent != Intent::None || st.state != InstanceState::Running {
                        None
                    } else {
                        match &inst.kind {
                            WorkloadKind::Job(j) if st.run_started.elapsed() >= j.timeout => {
                                st.set_intent(Intent::Timeout);
                                Some(Intent::Timeout)
                            }
                            WorkloadKind::Service(s) => match s.idle_timeout {
                                Some(idle) if ready && st.last_touch.elapsed() >= idle => {
                                    st.set_intent(Intent::ScaleToZero);
                                    Some(Intent::ScaleToZero)
                                }
                                _ => None,
                            },
                            _ => None,
                        }
                    }
                };
                match action {
                    Some(Intent::Timeout) => {
                        inner.kill(&name).await;
                        last_kill = Some(Instant::now());
                    }
                    Some(Intent::ScaleToZero) => inner.stop_container(&name).await,
                    _ => {}
                }
                // Escalation of an end that does not come.
                let pending = {
                    let st = inst.lock();
                    (st.intent != Intent::None).then(|| (st.intent, st.intent_at.elapsed()))
                };
                if let Some((intent, since)) = pending
                    && !forced
                {
                    let kill_after = if intent == Intent::Timeout {
                        Duration::ZERO
                    } else {
                        inner.cfg.stop_timeout + KILL_RETRY * 2
                    };
                    if since >= kill_after + FORCE_AFTER {
                        tracing::warn!(container = %name, "the container did not end after SIGKILL; removing it");
                        inner.remove_container(&name).await;
                        forced = true;
                    } else if since >= kill_after
                        && last_kill.is_none_or(|t| t.elapsed() >= KILL_RETRY)
                    {
                        inner.kill(&name).await;
                        last_kill = Some(Instant::now());
                    }
                }
            }
        }
    }
    // Every line of the run is in the ring before the state says the run ended.
    let _ = tokio::time::timeout(Duration::from_secs(5), pump).await;
    // The engine can report the exit before it handles the out-of-memory event, so a SIGKILL
    // exit (137) without the flag is read again for a short time.
    let mut exit = (-1, false);
    for attempt in 0..10 {
        let inspect = inner
            .docker
            .inspect_container(
                &name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .ok();
        exit = inspect
            .and_then(|c| c.state)
            .map(|s| (s.exit_code.unwrap_or(-1), s.oom_killed == Some(true)))
            .unwrap_or((-1, false));
        let unsure = exit == (137, false) && inst.lock().intent == Intent::None && !forced;
        if !unsure || attempt == 9 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (code, oom) = exit;
    let (intent, ran) = {
        let st = inst.lock();
        (st.intent, st.run_started.elapsed())
    };
    inner.usage.record(RuntimeUsage::Instance {
        identity: inst.identity,
        instance: inst.id,
        ran,
        memory_bytes: inst.limits.memory_bytes,
    });
    let next = match intent {
        Intent::Stop => InstanceState::Stopped,
        // The container was removed by force: nothing is left to wake.
        Intent::ScaleToZero if forced => InstanceState::Exited(ExitStatus::of(ExitReason::Killed)),
        Intent::ScaleToZero => InstanceState::ScaledToZero,
        Intent::Timeout => InstanceState::Exited(ExitStatus::of(ExitReason::TimedOut)),
        Intent::None if oom => InstanceState::Exited(ExitStatus::of(ExitReason::OutOfMemory)),
        Intent::None => InstanceState::Exited(ExitStatus::completed(code as i32)),
    };
    if next != InstanceState::ScaledToZero {
        inner.teardown(&inst).await;
    }
    {
        let mut st = inst.lock();
        st.state = next;
        st.set_intent(Intent::None);
        st.redactors = None;
        if next != InstanceState::ScaledToZero {
            // The supervisor (this task) is about to end.
            st.ended.get_or_insert_with(Instant::now);
        } else {
            st.park();
        }
    }
    inner.sweep_now();
}

#[async_trait]
impl RuntimeDriver for DockerDriver {
    fn name(&self) -> &str {
        self.inner.cfg.runtime.driver_name()
    }

    async fn start(&self, spec: WorkloadSpec) -> Result<InstanceStatus, RuntimeError> {
        spec.validate()?;
        let inner = &self.inner;
        inner.check_spec(&spec)?;
        // Refused before any engine work when the driver is full of running instances.
        inner.check_room(spec.identity.tenant)?;
        let id = InstanceId::new();
        let secrets: Vec<String> = spec
            .redacted_values()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let token = spec.workload_token.expose().to_owned();
        let base: Vec<String> = secrets.iter().filter(|v| **v != token).cloned().collect();
        let new_st = |state| St {
            state,
            intent: Intent::None,
            intent_at: Instant::now(),
            last_touch: Instant::now(),
            run_started: Instant::now(),
            ring: Ring::new(inner.cfg.log_lines, MAX_LOG_BYTES, inner.cfg.max_line_bytes)
                .with_budget(inner.live.clone(), spec.identity.tenant),
            base: base.clone(),
            tokens: VecDeque::from([token.clone()]),
            redactors: None,
            supervisor: None,
            torn_down: false,
            ended: None,
            drained: None,
            parked: None,
        };
        let digest = match &spec.image {
            ImageRef::Static(_) => {
                // No code runs: the gateway serves the files.
                let inst = Arc::new(Instance {
                    id,
                    identity: spec.identity,
                    kind: spec.kind.clone(),
                    routes: Vec::new(),
                    limits: spec.limits,
                    network: None,
                    started_at: OffsetDateTime::now_utc(),
                    container: None,
                    token_dir: None,
                    launch: None,
                    op: tokio::sync::Mutex::new(()),
                    st: Mutex::new(new_st(InstanceState::Running)),
                });
                inner.keep(&inst)?;
                return Ok(inner.status(&inst).await);
            }
            ImageRef::Oci(d) => d.clone(),
            other => {
                return Err(RuntimeError::UnsupportedImage {
                    reason: format!("the Docker driver does not run {} images", other.kind()),
                });
            }
        };
        inner.check_registry(&spec.identity, &digest).await?;
        let reference = format!(
            "{}/{}@{}",
            inner.cfg.registry,
            repository(&spec.identity),
            digest
        );
        inner.ensure_image(&reference, &digest).await?;
        let (entrypoint, cmd) = inner.check_image(&reference).await?;
        // The supervisor's launch data (I6-20): the command as the engine would run it, the
        // secret values, and the previous values to redact.
        let launch = match &inner.cfg.supervisor {
            None => None,
            Some(_) => {
                let mut argv = entrypoint;
                argv.extend(if spec.command.is_empty() {
                    cmd
                } else {
                    spec.command.clone()
                });
                if argv.is_empty() {
                    return Err(RuntimeError::UnsupportedImage {
                        reason: "the image has no command and the spec gives none".to_owned(),
                    });
                }
                let launch = Launch {
                    v: LAUNCH_VERSION,
                    argv,
                    env: spec
                        .env
                        .iter()
                        .filter_map(|v| match &v.value {
                            EnvValue::Secret(s) => Some((v.name.clone(), s.expose().to_owned())),
                            EnvValue::Plain(_) => None,
                        })
                        .collect(),
                    redact: spec
                        .previous_secrets
                        .iter()
                        .map(|s| s.expose().to_owned())
                        .filter(|s| !s.is_empty())
                        .collect(),
                    token: None,
                    pids_limit: None,
                    token_poll_secs: None,
                };
                launch
                    .check()
                    .map_err(|why| RuntimeError::invalid("spec", why))?;
                Some(launch)
            }
        };

        let token_dir = inner.cfg.state_dir.join(id.0.simple().to_string());
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&token_dir)
            .map_err(|e| RuntimeError::Internal {
                reason: format!("the token folder: {e}"),
            })?;
        let token_file = token_dir.join("token");
        if let Err(e) = Inner::write_token(&token_file, spec.workload_token.expose(), true) {
            let _ = std::fs::remove_dir_all(&token_dir);
            return Err(RuntimeError::Internal {
                reason: format!("the token file: {e}"),
            });
        }
        let (network, hosts) = match inner.acquire_network(id, &spec).await {
            Ok(n) => n,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&token_dir);
                return Err(e);
            }
        };
        let name = container_name(id);
        let inst = Arc::new(Instance {
            id,
            identity: spec.identity,
            kind: spec.kind.clone(),
            routes: sorted(&spec.routes),
            limits: spec.limits,
            network: Some(network.clone()),
            started_at: OffsetDateTime::now_utc(),
            container: Some(name.clone()),
            token_dir: Some(token_dir),
            launch,
            op: tokio::sync::Mutex::new(()),
            st: Mutex::new(new_st(InstanceState::Starting)),
        });
        let body = inner.container_body(&spec, &reference, &network, &hosts, &token_file, id);
        let opts = CreateContainerOptionsBuilder::new().name(&name).build();
        let created = inner.docker.create_container(Some(opts), body).await;
        if let Err(e) = created {
            inner.teardown(&inst).await;
            return Err(match status_of(&e) {
                Some(400) => {
                    let refs: Vec<&str> = secrets.iter().map(String::as_str).collect();
                    RuntimeError::invalid("spec", redact(&e.to_string(), &refs))
                }
                _ => inner.docker_err("creating the container", &e, &secrets),
            });
        }
        let checked = match inner
            .docker
            .inspect_container(
                &name,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(c) => inner
                .verify(&c, &spec, &network, &hosts, &token_file)
                .map_err(|what| RuntimeError::Internal {
                    reason: format!("the engine did not apply the sandbox: {what}"),
                }),
            Err(e) => Err(inner.docker_err("inspecting the container", &e, &secrets)),
        };
        if let Err(e) = checked {
            inner.teardown(&inst).await;
            return Err(e);
        }
        if let Err(e) = inner.keep(&inst) {
            inner.teardown(&inst).await;
            return Err(e);
        }
        let _op = inst.op.lock().await;
        if let Err(e) = inner.begin_run(&inst).await {
            inner.teardown(&inst).await;
            inst.lock().state = InstanceState::Exited(ExitStatus::of(ExitReason::Killed));
            return Err(e);
        }
        Ok(inner.status(&inst).await)
    }

    async fn status(
        &self,
        tenant: TenantId,
        instance: InstanceId,
    ) -> Result<InstanceStatus, RuntimeError> {
        let inst = self.inner.get_for(tenant, instance)?;
        Ok(self.inner.status(&inst).await)
    }

    async fn stop(&self, tenant: TenantId, instance: InstanceId) -> Result<(), RuntimeError> {
        let inner = &self.inner;
        let inst = inner.get_for(tenant, instance)?;
        let _op = inst.op.lock().await;
        let (state, sup) = {
            let mut st = inst.lock();
            let s = st.state;
            if matches!(s, InstanceState::Running | InstanceState::Starting) {
                st.set_intent(Intent::Stop);
            }
            (s, st.supervisor.take())
        };
        let bound = inner.end_bound();
        match state {
            InstanceState::Stopped | InstanceState::Exited(_) => {
                if let Some(s) = sup {
                    join_bounded(s, bound).await;
                }
                Ok(())
            }
            InstanceState::ScaledToZero => {
                if let Some(s) = sup {
                    join_bounded(s, bound).await;
                }
                inner.teardown(&inst).await;
                inst.stopped();
                Ok(())
            }
            InstanceState::Running | InstanceState::Starting => {
                if inst.container.is_none() {
                    inst.stopped();
                    return Ok(());
                }
                let name = inst.container.clone().unwrap_or_default();
                inner.stop_container(&name).await;
                let ended = match sup {
                    Some(s) => join_bounded(s, bound).await,
                    None => true,
                };
                // A scale to zero that won the race already ended the run. Teardown removes
                // the container by force when the supervisor did not end.
                inner.teardown(&inst).await;
                inst.stopped();
                if ended {
                    Ok(())
                } else {
                    Err(RuntimeError::Unavailable {
                        reason:
                            "the engine did not end the container in time; it was removed by force"
                                .to_owned(),
                    })
                }
            }
        }
    }

    async fn touch(&self, tenant: TenantId, instance: InstanceId) -> Result<(), RuntimeError> {
        let inst = self.inner.get_for(tenant, instance)?;
        if !matches!(inst.kind, WorkloadKind::Service(_)) {
            let st = inst.lock();
            return Err(state_err("touch", &st.state));
        }
        let mut st = inst.lock();
        match st.state {
            InstanceState::Running | InstanceState::Starting => {
                st.last_touch = Instant::now();
                Ok(())
            }
            InstanceState::ScaledToZero => Ok(()),
            s => Err(state_err("touch", &s)),
        }
    }

    async fn wake(
        &self,
        tenant: TenantId,
        instance: InstanceId,
    ) -> Result<InstanceStatus, RuntimeError> {
        let inner = &self.inner;
        let inst = inner.get_for(tenant, instance)?;
        let _op = inst.op.lock().await;
        if !matches!(inst.kind, WorkloadKind::Service(_)) {
            let s = inst.lock().state;
            return Err(state_err("wake", &s));
        }
        // A scale to zero in progress ends first; then the instance starts again.
        let pending = {
            let mut st = inst.lock();
            if st.intent == Intent::ScaleToZero {
                st.supervisor.take()
            } else {
                None
            }
        };
        if let Some(p) = pending
            && !join_bounded(p, inner.end_bound()).await
        {
            return Err(RuntimeError::Unavailable {
                reason: "the scale to zero did not end in time".to_owned(),
            });
        }
        let state = inst.lock().state;
        match state {
            InstanceState::Running => {
                inst.lock().last_touch = Instant::now();
                return Ok(inner.status(&inst).await);
            }
            InstanceState::ScaledToZero => {}
            s => return Err(state_err("wake", &s)),
        }
        let old = inst.lock().supervisor.take();
        if let Some(old) = old
            && !join_bounded(old, inner.end_bound()).await
        {
            return Err(RuntimeError::Unavailable {
                reason: "the last run did not end in time".to_owned(),
            });
        }
        inner.begin_run(&inst).await?;
        // Hold until the service answers its health check (or runs, without one).
        let deadline = Instant::now() + inner.cfg.start_period + Duration::from_secs(5);
        loop {
            let s = inner.status(&inst).await;
            let ready = s.state != InstanceState::Running
                || !inst.health_check()
                || s.health == Health::Healthy;
            if ready || Instant::now() > deadline {
                return Ok(s);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn logs(
        &self,
        tenant: TenantId,
        instance: InstanceId,
        after: Option<u64>,
        limit: usize,
    ) -> Result<Vec<LogLine>, RuntimeError> {
        let inst = self.inner.get_for(tenant, instance)?;
        let mut st = inst.lock();
        let lines = st.ring.after(after, limit);
        // An ended instance whose logs were read to the end is kept only a short time more.
        let read_to = lines.last().map_or(after.unwrap_or(0), |l| l.seq);
        st.mark_ended(inst.container.is_some());
        if st.ended.is_some() && st.drained.is_none() && read_to >= st.ring.last_seq() {
            st.drained = Some(Instant::now());
        }
        Ok(lines)
    }

    async fn rotate_token(
        &self,
        tenant: TenantId,
        instance: InstanceId,
        token: SecretValue,
    ) -> Result<(), RuntimeError> {
        if token.expose().is_empty() {
            return Err(RuntimeError::invalid("workload_token", "must not be empty"));
        }
        check_secret_value("workload_token", token.expose())?;
        let inst = self.inner.get_for(tenant, instance)?;
        let _op = inst.op.lock().await;
        let state = inst.lock().state;
        if !matches!(
            state,
            InstanceState::Starting | InstanceState::Running | InstanceState::ScaledToZero
        ) {
            return Err(state_err("rotate the token of", &state));
        }
        // Redact the new value before the process can read it. The oldest kept token goes
        // when there are more than MAX_KEPT_TOKENS (it expired long before).
        {
            let mut st = inst.lock();
            st.tokens.push_back(token.expose().to_owned());
            while st.tokens.len() > MAX_KEPT_TOKENS {
                st.tokens.pop_front();
            }
            let all = st.secrets();
            if let Some(r) = &st.redactors {
                r.lock().unwrap_or_else(|p| p.into_inner()).set(&all);
            }
        }
        if let Some(dir) = &inst.token_dir {
            Inner::write_token(&dir.join("token"), token.expose(), false).map_err(|e| {
                RuntimeError::Internal {
                    reason: format!("rewriting the token file: {e}"),
                }
            })?;
        }
        Ok(())
    }
}

/// The labels the driver sets on networks and containers (for the README and I-4).
pub fn label_keys() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        (LABEL_APP, "the app node ID (networks and containers)"),
        (LABEL_TENANT, "the tenant ID (networks and containers)"),
        (LABEL_ENV, "live or preview (networks and containers)"),
        (LABEL_DEPLOYMENT, "the deployment ID (containers)"),
        (LABEL_INSTANCE, "the instance ID (containers)"),
        (LABEL_MANAGED, "ruum-runtime (networks and containers)"),
    ])
}

#[cfg(test)]
mod tests {
    //! The retention of ended instances (backlog R2A-5, Kimi review of run 11, finding 3), on
    //! static instances: they need no engine, so the driver runs here without one.

    use super::*;
    use crate::egress::NoEgress;
    use ruum_core::ids::NodeId;
    use ruum_core::runtime::{Environment, MemoryUsage, ServiceSpec, StaticFile, StaticImage};

    fn offline(cfg: DockerConfig) -> DockerDriver {
        let cfg_live = (cfg.max_live_log_bytes, cfg.max_tenant_live_log_bytes);
        let docker =
            Docker::connect_with_http("http://127.0.0.1:9", 1, bollard::API_DEFAULT_VERSION)
                .expect("a client (it never connects here)");
        DockerDriver {
            inner: Arc::new(Inner {
                docker,
                cfg,
                usage: Arc::new(MemoryUsage::new()),
                egress: Arc::new(NoEgress),
                http: reqwest::Client::new(),
                instances: Mutex::new(HashMap::new()),
                live: LogBudget::new(cfg_live.0, cfg_live.1),
                networks: tokio::sync::Mutex::new(HashMap::new()),
                stale: Mutex::new(Vec::new()),
            }),
        }
    }

    fn config() -> DockerConfig {
        DockerConfig::new(
            "http://127.0.0.1:9",
            "127.0.0.1:5000",
            std::env::temp_dir().join("ruum-runtime-retention-unused"),
        )
    }

    fn spec(tenant: TenantId) -> WorkloadSpec {
        WorkloadSpec {
            identity: WorkloadIdentity {
                tenant,
                app: NodeId::new(),
                deployment: uuid::Uuid::now_v7(),
                environment: Environment::Live,
            },
            kind: WorkloadKind::Service(ServiceSpec {
                port: 8080,
                health: None,
                idle_timeout: None,
            }),
            image: ImageRef::Static(StaticImage::new(vec![StaticFile {
                path: "index.html".to_owned(),
                content: ruum_core::blobs::Hash::of(b"<h1>hi</h1>"),
                size: 11,
            }])),
            command: Vec::new(),
            env: Vec::new(),
            previous_secrets: Vec::new(),
            workload_token: SecretValue::new(format!("wt_{}", uuid::Uuid::now_v7().simple())),
            limits: Limits {
                cpu_millis: 500,
                memory_bytes: 128 * 1024 * 1024,
                disk_bytes: 16 * 1024 * 1024,
            },
            routes: Vec::new(),
        }
    }

    /// Fills the ring of an instance with `lines` lines of 1 KiB, as the pump would.
    fn fill(d: &DockerDriver, id: InstanceId, lines: usize) {
        let inst = d.inner.get(id).expect("kept");
        let mut st = inst.lock();
        for _ in 0..lines {
            st.ring.push(
                inst.id,
                inst.identity,
                LogStream::Stdout,
                OffsetDateTime::now_utc(),
                "x".repeat(1024),
            );
        }
    }

    /// Starts a static instance, gives it `lines` log lines and stops it.
    async fn start_and_end(d: &DockerDriver, tenant: TenantId, lines: usize) -> InstanceId {
        let s = d.start(spec(tenant)).await.expect("start");
        fill(d, s.id, lines);
        d.stop(tenant, s.id).await.expect("stop");
        s.id
    }

    fn gone(r: Result<InstanceStatus, RuntimeError>) -> bool {
        matches!(r, Err(RuntimeError::NotFound { .. }))
    }

    #[tokio::test]
    async fn repeated_start_and_exit_keeps_memory_bounded() {
        let mut cfg = config();
        cfg.max_ended_instances = 8;
        let d = offline(cfg);
        let tenant = TenantId::new();
        start_and_end(&d, tenant, 64).await;
        let one = d.kept_log_bytes();
        let first = d.kept_instances();
        assert_eq!(first, 1);
        let mut ids = Vec::new();
        for i in 0..3000 {
            ids.push(start_and_end(&d, tenant, 64).await);
            let kept = d.kept_instances();
            assert!(kept <= 9, "round {i}: {kept} instances kept");
            assert!(
                d.kept_log_bytes() <= 9 * one,
                "round {i}: {} log bytes kept",
                d.kept_log_bytes()
            );
        }
        // The oldest are forgotten, the newest still answer.
        assert!(gone(d.status(tenant, ids[0]).await));
        let last = *ids.last().unwrap();
        assert_eq!(
            d.status(tenant, last).await.expect("kept").state,
            InstanceState::Stopped
        );
        assert_eq!(d.logs(tenant, last, None, 1000).await.unwrap().len(), 64);
    }

    /// One tenant's load evicts its own ended instances, not another tenant's (review of K11,
    /// S-61): tenant B's ended instance, unread and within its retention, stays while tenant A
    /// fills the count cap, the byte cap and the instance cap.
    #[tokio::test]
    async fn eviction_takes_from_the_largest_tenant_first() {
        let a = TenantId::new();
        let b = TenantId::new();
        let probe = offline(config());
        let id = start_and_end(&probe, a, 100).await;
        let one = probe.inner.get(id).unwrap().lock().ring.memory();
        for (count, bytes, max) in [
            (4, usize::MAX, usize::MAX),
            (usize::MAX, 3 * one + one / 2, usize::MAX),
            (usize::MAX, usize::MAX, 6),
        ] {
            let mut cfg = config();
            cfg.max_ended_instances = count;
            cfg.max_ended_log_bytes = bytes;
            cfg.max_instances = max;
            let d = offline(cfg);
            // B's instance is the oldest and holds a smaller share than A's.
            let kept = start_and_end(&d, b, 10).await;
            let mut ids = Vec::new();
            for _ in 0..40 {
                ids.push(start_and_end(&d, a, 100).await);
            }
            d.start(spec(a)).await.expect("start");
            assert!(gone(d.status(a, ids[0]).await), "A's oldest went");
            assert!(!gone(d.status(a, *ids.last().unwrap()).await));
            assert_eq!(
                d.status(b, kept).await.expect("B's instance is kept").state,
                InstanceState::Stopped
            );
            assert_eq!(d.logs(b, kept, None, 100).await.unwrap().len(), 10);
            assert!(d.kept_instances() <= 6, "{count} {bytes} {max}");
        }
    }

    #[tokio::test]
    async fn ended_log_bytes_are_capped() {
        let mut cfg = config();
        let tenant = TenantId::new();
        // Room for the logs of about three ended instances.
        let probe = offline(config());
        let id = start_and_end(&probe, tenant, 100).await;
        let one = probe.inner.get(id).unwrap().lock().ring.memory();
        cfg.max_ended_log_bytes = 3 * one + one / 2;
        let d = offline(cfg);
        for _ in 0..50 {
            start_and_end(&d, tenant, 100).await;
        }
        // The sweep runs at the next start: three ended and the new one.
        let s = d.start(spec(tenant)).await.unwrap();
        assert_eq!(d.kept_instances(), 4);
        assert!(d.kept_log_bytes() <= 3 * one + one / 2);
        assert_eq!(
            d.status(tenant, s.id).await.unwrap().state,
            InstanceState::Running
        );
    }

    #[tokio::test]
    async fn read_logs_shorten_the_retention() {
        let mut cfg = config();
        cfg.drained_retention = Duration::ZERO;
        let d = offline(cfg);
        let tenant = TenantId::new();
        let read = start_and_end(&d, tenant, 10).await;
        let unread = start_and_end(&d, tenant, 10).await;
        // Read only part of the logs: not drained yet.
        let part = d.logs(tenant, read, None, 4).await.unwrap();
        d.start(spec(tenant)).await.unwrap();
        assert!(!gone(d.status(tenant, read).await), "read in part");
        // Read to the end: forgotten at the next sweep.
        let rest = d
            .logs(tenant, read, part.last().map(|l| l.seq), 100)
            .await
            .unwrap();
        assert_eq!(part.len() + rest.len(), 10);
        d.start(spec(tenant)).await.unwrap();
        assert!(gone(d.status(tenant, read).await), "read to the end");
        assert!(!gone(d.status(tenant, unread).await), "never read");
        // Logs read while the instance runs do not count.
        let s = d.start(spec(tenant)).await.unwrap();
        d.logs(tenant, s.id, None, 100).await.unwrap();
        d.start(spec(tenant)).await.unwrap();
        assert_eq!(
            d.status(tenant, s.id).await.unwrap().state,
            InstanceState::Running
        );
    }

    #[tokio::test]
    async fn ended_instances_go_after_the_retention() {
        let mut cfg = config();
        cfg.ended_retention = Duration::ZERO;
        let d = offline(cfg);
        let tenant = TenantId::new();
        let ended = start_and_end(&d, tenant, 1).await;
        let running = d.start(spec(tenant)).await.unwrap().id;
        assert!(gone(d.status(tenant, ended).await));
        d.start(spec(tenant)).await.unwrap();
        assert_eq!(
            d.status(tenant, running).await.unwrap().state,
            InstanceState::Running,
            "a running instance is never forgotten"
        );
    }

    /// A tenant whose running instances fill the driver does not forget another tenant's ended
    /// instance to start one more (Kimi review of run 12, K11 finding 3, S-67): its start is
    /// `unavailable`, and the other tenant's status and logs stay. The other tenant may still
    /// give up its own ended instance for a start.
    #[tokio::test]
    async fn room_is_not_taken_from_another_tenant() {
        let mut cfg = config();
        cfg.max_instances = 3;
        let d = offline(cfg);
        let a = TenantId::new();
        let b = TenantId::new();
        let kept = start_and_end(&d, b, 10).await;
        d.start(spec(a)).await.expect("A's first");
        d.start(spec(a)).await.expect("A's second");
        assert!(matches!(
            d.start(spec(a)).await,
            Err(RuntimeError::Unavailable { .. })
        ));
        assert_eq!(
            d.status(b, kept).await.expect("B's instance is kept").state,
            InstanceState::Stopped
        );
        assert_eq!(d.logs(b, kept, None, 100).await.unwrap().len(), 10);
        // B starts again: its own ended instance makes the room.
        let again = d.start(spec(b)).await.expect("B's start");
        assert!(gone(d.status(b, kept).await));
        assert_eq!(
            d.status(b, again.id).await.unwrap().state,
            InstanceState::Running
        );
        assert_eq!(d.kept_instances(), 3);
        // A tenant that keeps more than the starter gives up its ended instances first.
        let c = TenantId::new();
        let mut cfg = config();
        cfg.max_instances = 4;
        let d = offline(cfg);
        let a_ended = start_and_end(&d, a, 1).await;
        start_and_end(&d, a, 1).await;
        d.start(spec(a)).await.unwrap();
        let b_ended = start_and_end(&d, b, 1).await;
        d.start(spec(c)).await.expect("room from A");
        assert!(gone(d.status(a, a_ended).await), "A keeps the most");
        assert!(!gone(d.status(b, b_ended).await), "B keeps one");
    }

    /// S-91: the live rings of many noisy instances stay within the live budget and each
    /// tenant's share; a quiet tenant keeps its lines.
    #[tokio::test]
    async fn live_logs_stay_within_the_budget() {
        let mut cfg = config();
        cfg.max_live_log_bytes = 2 * 1024 * 1024;
        cfg.max_tenant_live_log_bytes = 512 * 1024;
        let d = offline(cfg);
        let (noisy, quiet) = (TenantId::new(), TenantId::new());
        let q = d.start(spec(quiet)).await.expect("start").id;
        fill(&d, q, 100);
        let quiet_lines = d.logs(quiet, q, None, 10_000).await.unwrap().len();
        let mut ids = Vec::new();
        for _ in 0..32 {
            ids.push(d.start(spec(noisy)).await.expect("start").id);
        }
        for _ in 0..4 {
            for id in &ids {
                fill(&d, *id, 64);
            }
        }
        let live = &d.inner.live;
        assert!(
            live.used(Some(noisy)) <= 512 * 1024,
            "{}",
            live.used(Some(noisy))
        );
        assert!(live.used(None) <= 2 * 1024 * 1024 + 512 * 1024);
        assert!(
            d.kept_log_bytes() <= 2 * 1024 * 1024 + 512 * 1024,
            "{}",
            d.kept_log_bytes()
        );
        assert_eq!(
            d.logs(quiet, q, None, 10_000).await.unwrap().len(),
            quiet_lines
        );
    }

    /// Backlog I620-11: the ring of an instance scaled to zero leaves the live budget and counts
    /// to the ended byte cap; past the cap it loses its lines (the instance stays, for a wake)
    /// before another tenant's ended instance goes, and a wake counts it to the live budget again.
    #[tokio::test]
    async fn scaled_to_zero_logs_count_to_the_byte_cap() {
        let mut cfg = config();
        cfg.max_ended_log_bytes = 96 * 1024;
        let d = offline(cfg);
        let (noisy, quiet) = (TenantId::new(), TenantId::new());
        let quiet_ended = start_and_end(&d, quiet, 8).await;
        let p = d.start(spec(noisy)).await.expect("start").id;
        fill(&d, p, 64);
        let live = &d.inner.live;
        assert!(live.used(Some(noisy)) >= 64 * 1024);
        {
            let inst = d.inner.get(p).expect("kept");
            let mut st = inst.lock();
            st.state = InstanceState::ScaledToZero;
            st.park();
        }
        assert_eq!(
            live.used(Some(noisy)),
            0,
            "a parked ring leaves the live budget"
        );
        // Within the cap nothing goes.
        d.inner.sweep_now();
        assert!(!d.logs(noisy, p, None, 10_000).await.unwrap().is_empty());
        // Past the cap the parked ring of the tenant with the most bytes loses its lines; the
        // other tenant's ended instance and the parked instance itself stay.
        fill(&d, p, 64);
        d.inner.sweep_now();
        assert!(d.logs(noisy, p, None, 10_000).await.unwrap().is_empty());
        assert!(!gone(d.status(noisy, p).await), "the parked instance stays");
        assert!(
            !d.logs(quiet, quiet_ended, None, 10_000)
                .await
                .unwrap()
                .is_empty(),
            "the quiet tenant's ended instance keeps its logs"
        );
        assert!(d.kept_log_bytes() <= 96 * 1024, "{}", d.kept_log_bytes());
        // A wake: its new lines count to the live budget again.
        d.inner.get(p).expect("kept").lock().unpark();
        fill(&d, p, 4);
        assert!(live.used(Some(noisy)) >= 4 * 1024);
    }

    #[tokio::test]
    async fn a_full_driver_refuses_a_start() {
        let mut cfg = config();
        cfg.max_instances = 3;
        let d = offline(cfg);
        let tenant = TenantId::new();
        let mut running = Vec::new();
        for _ in 0..3 {
            running.push(d.start(spec(tenant)).await.unwrap().id);
        }
        assert!(matches!(
            d.start(spec(tenant)).await,
            Err(RuntimeError::Unavailable { .. })
        ));
        assert_eq!(d.kept_instances(), 3);
        // An ended instance makes room, also within its retention.
        d.stop(tenant, running[0]).await.unwrap();
        d.start(spec(tenant)).await.expect("room after a stop");
        assert_eq!(d.kept_instances(), 3);
        assert!(gone(d.status(tenant, running[0]).await));
        for id in &running[1..] {
            assert_eq!(
                d.status(tenant, *id).await.unwrap().state,
                InstanceState::Running
            );
        }
    }
}
