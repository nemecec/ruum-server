//! The Kubernetes runtime driver (backlog I6-1): [`KubeDriver`] implements
//! [`ruum_core::runtime::RuntimeDriver`] with pods of the runtime class `gvisor` in one namespace
//! (`ruum-apps` on staging), beside the Docker driver of the laptop. Same trait, same spec, same
//! limits; the crate README ("Kubernetes driver") lists every API call and the Role it needs.
//!
//! - **The sandbox** (STEP3 "How an app runs" rule 1): a restricted pod in gVisor
//!   (`runtimeClassName`), no service account token, no host namespace, user `10001`, read-only
//!   root, `drop: [ALL]`, no privilege escalation, `RuntimeDefault` seccomp, CPU and memory
//!   limits (requests = limits), a memory-backed `/tmp` of the spec's disk limit, no DNS,
//!   `restartPolicy: Never`. The driver reads the pod back and refuses it when the API server
//!   stored anything else ([`pod::verify`]).
//! - **Network** (rule 2): the namespace's network policies (charts/platform
//!   `networkpolicies.yaml`) let a sandbox reach only the egress proxy and be reached only by the
//!   app gateway; the driver sets the proxy variables and `/etc/hosts` lines for the spec's
//!   routes and registers the pod's address at the egress proxy ([`EgressRegistry`]).
//! - **Secrets** (rule 7; backlog S-89 and I6-20, `docs/STEP3.md` "The sandbox supervisor"): no
//!   Kubernetes Secret. The token and the secret values are sealed into the instance's record
//!   under the record key ([`seal`]); the sandbox supervisor, the pod's first process, fetches
//!   them once from the server's supervisor listener ([`KubeDriver::supervisor_launch`]) with the
//!   pod's projected service account token, and redacts the app's output before the kubelet
//!   writes it. The driver redacts what it reads again (`ruum-core`'s redactor).
//! - **State in the API**: the record of each instance is a ConfigMap, so every server process
//!   answers for every instance, and a restarted server adopts what runs ([`pod`]).
//! - **Readiness** is an exec probe inside the sandbox (decision 0006 G point 1: never from the
//!   node's network); the gateway reaches a ready pod at its address ([`KubeDriver::upstream`]).

mod api;
pub mod pod;
mod seal;

use crate::driver::{RESERVED_ENV, repository};
use crate::egress::{EgressNetwork, EgressRegistry};
use crate::logs::{LogBudget, Redactors, Ring};
use api::{Api, Kind, LogAnswer, Write};
use async_trait::async_trait;
use futures::StreamExt;
use pod::{HostAlias, KindRec, PodView, Record};
use ruum_core::ids::TenantId;
use ruum_core::runtime::redact::{MAX_KEPT_TOKENS, check_secret_set, check_secret_value};
use ruum_core::runtime::{
    Applied, EnvValue, ExitReason, ExitStatus, Health, ImageRef, InstanceId, InstanceState,
    InstanceStatus, Limits, LogLine, LogStream, NetworkId, OciDigest, Route, RuntimeDriver,
    RuntimeError, RuntimeUsage, RuntimeUsageSink, SecretValue, WorkloadIdentity, WorkloadKind,
    WorkloadSpec,
};
use ruum_supervisor::protocol::{LAUNCH_VERSION, Launch, TokenAnswer};
use seal::{MAX_LAUNCHED, Sealed};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use time::OffsetDateTime;
use tokio::task::JoinHandle;

pub use pod::{LABEL_RUN, SUPERVISOR_IN_IMAGE, object_name, pod_name};
pub use seal::RecordKey;

/// The label that marks every object of the driver, and its value.
pub const LABEL_MANAGED: &str = crate::driver::LABEL_MANAGED;
pub const MANAGED: &str = "ruum-runtime";
/// The most log bytes kept per instance.
const MAX_LOG_BYTES: usize = 4 * 1024 * 1024;
/// The tries of one egress unregistration in a state change; the sweep tries again later.
const UNREGISTER_TRIES: u32 = 3;
/// A job that the API server did not end this long after its timeout is ended by the driver.
const JOB_DEADLINE_SLACK: Duration = Duration::from_secs(5);
/// How long a run may have a record that says starting and no pod yet (the pod is made right
/// after the record).
const START_GRACE: Duration = Duration::from_secs(60);
/// How long a state change to exited waits for this process's log followers to read the end.
const FOLLOWER_DRAIN: Duration = Duration::from_secs(3);
/// How long the pod of an exited run stays, so the log followers read its end.
const EXITED_POD_KEEP: Duration = Duration::from_secs(60);
/// The tries of a sealed record write that loses a race.
const SEAL_TRIES: u32 = 6;
/// How often the supervisor asks for the current workload token.
const TOKEN_POLL_SECS: u64 = 30;
/// How long a positive `TokenReview` is kept at most (never past the token's `exp`), and how many.
const REVIEW_TTL: Duration = Duration::from_secs(60);
const REVIEW_CACHE_MAX: usize = 4096;
/// The supervisor requests one peer address may make per window.
const PEER_REQUESTS: u32 = 30;
const PEER_WINDOW: Duration = Duration::from_secs(10);

/// A shared service of a route (the egress proxy, the internal gateway, the pooler), as the
/// platform deploys it. The spec cannot name one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedService {
    /// An IP address or a DNS name that the driver resolves (in its own pod) when it makes a pod,
    /// such as `ruum-egress.ruum.svc.cluster.local`.
    pub address: String,
    /// The name the sandbox reaches it by (an `/etc/hosts` line), such as `ruum-egress`.
    pub alias: String,
    pub port: u16,
}

/// The settings of [`KubeDriver`]. The crate reads no environment: the server's wiring builds it.
#[derive(Clone, Debug)]
pub struct KubeConfig {
    /// The API server (`https://kubernetes.default.svc`).
    pub api_url: String,
    /// The bearer token file (a projected service account token), read for every request.
    pub token_file: Option<PathBuf>,
    /// The CA of the API server.
    pub ca_file: Option<PathBuf>,
    /// The namespace of the sandboxes (`ruum-apps`).
    pub namespace: String,
    /// The runtime class of every pod (`gvisor`). Required: the admission policy of the
    /// namespace refuses a pod without it (backlog S-83).
    pub runtime_class: String,
    /// The registry (`host:port`) the nodes pull app images from; `None`: no registry yet, so
    /// every OCI image is `image_not_found` (fails closed).
    pub registry: Option<String>,
    /// The URL the driver checks a digest at (`http://` + registry by default).
    pub registry_url: Option<String>,
    pub routes: BTreeMap<Route, SharedService>,
    /// The pod's termination grace (at most 30 s).
    pub stop_timeout: Duration,
    pub log_lines: usize,
    pub max_line_bytes: usize,
    /// The readiness command inside the sandbox; the URL is appended. Busybox `wget` without the
    /// proxy, as the Docker driver's health check.
    pub health_command: Vec<String>,
    /// How long a failing readiness probe may take before the instance is unhealthy, on top of
    /// the probe's own interval times its threshold.
    pub start_period: Duration,
    /// Extra labels on every object (tests set `ruum.test`); the sweep selects on them too.
    pub extra_labels: BTreeMap<String, String>,
    /// Pods only on these nodes (none on staging).
    pub node_selector: Option<BTreeMap<String, String>>,
    /// One API call at most (not a followed log stream).
    pub api_timeout: Duration,
    /// How often the sweep looks at every instance (exits, idle services, cleanup).
    pub sweep_every: Duration,
    /// How long an ended instance and its logs are kept.
    pub ended_retention: Duration,
    /// How often every running pod is registered at the egress proxy again (a proxy restart
    /// forgets every registration).
    pub reregister_every: Duration,
    /// Read standard output and standard error apart (`stream=Stdout|Stderr` of the pod log;
    /// the API server needs the feature gate `PodLogsQuerySplitStreams`). Off: both come
    /// together and are reported as standard output.
    pub split_log_streams: bool,
    /// The most instances this process keeps the logs of (running, scaled to zero and ended); a
    /// start beyond it forgets an ended one first (of the tenant that keeps the most) or is
    /// `unavailable`.
    pub max_instances: usize,
    /// The most instances of one tenant that are not ended (running or scaled to zero) this
    /// process keeps: a start beyond it is `unavailable`, so one tenant never takes every slot
    /// of `max_instances` (Kimi run 15, I6-1 finding 5; backlog R2A-9).
    pub max_tenant_instances: usize,
    /// The most ended instances whose logs this process keeps, and their log bytes in all; past
    /// either, the oldest ended instance of the tenant with the largest share is forgotten
    /// (as the Docker driver, backlog S-61). The logs of instances scaled to zero count to the
    /// byte cap too: past it, the oldest of them (of the tenant with the most bytes) loses its
    /// log lines and keeps the rest (its redaction values, for a wake and a token rotation).
    pub max_ended_instances: usize,
    pub max_ended_log_bytes: usize,
    /// The byte budget of the running instances' log rings in this process, and each tenant's
    /// share of it (backlog S-91; `crate::logs::LogBudget`).
    pub max_live_log_bytes: usize,
    pub max_tenant_live_log_bytes: usize,
    /// The record key (backlog S-89): seals the values in each instance record. Required.
    pub record_key: Option<RecordKey>,
    /// The sandbox supervisor's image, by digest (the init container copies the binary from
    /// [`SUPERVISOR_IN_IMAGE`]). Required.
    pub supervisor_image: String,
    /// The server's supervisor listener as the sandboxes reach it: an IP address or a DNS name
    /// that the driver resolves (the sandbox has no DNS), and its port.
    pub supervisor_address: String,
    pub supervisor_port: u16,
    /// The ServiceAccount of the sandbox pods (no rights) and the audience of their projected
    /// token.
    pub sandbox_service_account: String,
    pub supervisor_audience: String,
    /// `RLIMIT_NPROC` the supervisor sets for the app (gVisor counts per sandbox); `None`: none.
    pub pids_limit: Option<u64>,
    /// Node-to-node traffic is encrypted and authenticated (`RUUM_KUBE_SUPERVISOR_NETWORK=
    /// encrypted`): the launch channel is plain HTTP, so the driver refuses to start without it.
    pub network_encrypted: bool,
    /// The CPU architecture of the nodes (`amd64`): the platform of a multi-arch image index.
    pub node_arch: String,
}

impl KubeConfig {
    /// The defaults for everything but the API server, the namespace and the runtime class.
    pub fn new(
        api_url: impl Into<String>,
        namespace: impl Into<String>,
        runtime_class: impl Into<String>,
    ) -> Self {
        Self {
            api_url: api_url.into(),
            token_file: None,
            ca_file: None,
            namespace: namespace.into(),
            runtime_class: runtime_class.into(),
            registry: None,
            registry_url: None,
            routes: BTreeMap::new(),
            stop_timeout: Duration::from_secs(10),
            log_lines: 10_000,
            max_line_bytes: 16 * 1024,
            health_command: crate::config::DEFAULT_HEALTH_COMMAND[1..]
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            start_period: Duration::from_secs(10),
            extra_labels: BTreeMap::new(),
            node_selector: None,
            api_timeout: Duration::from_secs(30),
            sweep_every: Duration::from_secs(5),
            ended_retention: Duration::from_secs(15 * 60),
            reregister_every: Duration::from_secs(60),
            split_log_streams: false,
            max_instances: 4096,
            max_tenant_instances: 512,
            max_ended_instances: 1024,
            max_ended_log_bytes: 128 * 1024 * 1024,
            max_live_log_bytes: 64 * 1024 * 1024,
            max_tenant_live_log_bytes: 16 * 1024 * 1024,
            record_key: None,
            supervisor_image: String::new(),
            supervisor_address: String::new(),
            supervisor_port: 8091,
            sandbox_service_account: "ruum-sandbox".to_owned(),
            supervisor_audience: "ruum-supervisor".to_owned(),
            pids_limit: Some(512),
            network_encrypted: false,
            node_arch: "amd64".to_owned(),
        }
    }

    fn check(&self) -> Result<(), RuntimeError> {
        let bad = |reason: &str| {
            Err(RuntimeError::Internal {
                reason: reason.to_owned(),
            })
        };
        if self.namespace.is_empty() || self.runtime_class.is_empty() {
            return bad("the Kubernetes driver needs a namespace and a runtime class");
        }
        if self.stop_timeout > Duration::from_secs(30) {
            return bad("the stop timeout is at most 30 s");
        }
        if self.log_lines == 0
            || self.max_line_bytes == 0
            || self.max_line_bytes > ruum_core::runtime::redact::MAX_LINE_BYTES
        {
            return bad(
                "the log ring and the line cap must be above zero and within the redactor's",
            );
        }
        if self.max_instances == 0 || self.max_tenant_instances == 0 {
            return bad("the instance caps must be above zero");
        }
        if self.max_tenant_instances > self.max_instances {
            return bad("a tenant's instance cap is at most the instance cap");
        }
        if self.health_command.is_empty() || self.sweep_every.is_zero() {
            return bad("the health command and the sweep interval must be set");
        }
        if self.max_tenant_live_log_bytes == 0
            || self.max_tenant_live_log_bytes > self.max_live_log_bytes
        {
            return bad("a tenant's live log share is above zero and at most the live log budget");
        }
        if self.record_key.is_none() {
            return bad("the Kubernetes driver needs the record key (backlog S-89)");
        }
        if !self.supervisor_image.contains("@sha256:") {
            return bad("the supervisor image is pinned by digest (name@sha256:...)");
        }
        if self.supervisor_address.is_empty() || self.supervisor_port == 0 {
            return bad("the Kubernetes driver needs the supervisor listener's address and port");
        }
        if self.sandbox_service_account.is_empty() || self.supervisor_audience.is_empty() {
            return bad("the sandbox ServiceAccount and the supervisor audience must be set");
        }
        if !self.network_encrypted {
            return bad(
                "the launch channel is plain HTTP: node-to-node traffic must be encrypted (RUUM_KUBE_SUPERVISOR_NETWORK=encrypted, decision 0006 I point 4)",
            );
        }
        if self.pids_limit.is_some_and(|p| p < 64) {
            return bad("the PID limit is at least 64");
        }
        for (r, s) in &self.routes {
            if s.address.is_empty() || s.alias.is_empty() || s.port == 0 {
                return Err(RuntimeError::Internal {
                    reason: format!("the shared service of {} is incomplete", r.as_str()),
                });
            }
        }
        Ok(())
    }
}

/// What this process keeps of an instance it started or woke: the log ring and the redaction
/// values of the log followers.
struct Local {
    id: InstanceId,
    identity: WorkloadIdentity,
    /// When this process made it (a start in progress has a record only after this).
    made: Instant,
    /// One token rotation at a time.
    rotating: tokio::sync::Mutex<()>,
    redactors: Mutex<Option<Arc<Mutex<Redactors>>>>,
    st: Mutex<LocalSt>,
}

struct LocalSt {
    ring: Ring,
    base: Vec<String>,
    tokens: VecDeque<String>,
    followed_run: u32,
    followers: Vec<JoinHandle<()>>,
    /// The last request (`touch`) this process saw.
    touched_ms: i64,
    ended_at: Option<Instant>,
    /// Since when the instance is scaled to zero (`None` while a run is live or after its end).
    parked_at: Option<Instant>,
}

impl LocalSt {
    fn values(&self) -> Vec<String> {
        let mut v = self.base.clone();
        v.extend(self.tokens.iter().cloned());
        v
    }

    /// The instance ended: its logs leave the live budget (S-91) and start their retention.
    fn set_ended(&mut self) {
        self.parked_at = None;
        if self.ended_at.is_none() {
            self.ended_at = Some(Instant::now());
        }
        self.ring.leave_budget();
    }

    /// The instance scaled to zero: its logs leave the live budget and count to the ended cap.
    fn set_parked(&mut self) {
        if self.parked_at.is_none() {
            self.parked_at = Some(Instant::now());
        }
        self.ring.leave_budget();
    }
}

struct StaticInst {
    identity: WorkloadIdentity,
    kind: &'static str,
    limits: Limits,
    started_at: OffsetDateTime,
    state: InstanceState,
}

struct Inner {
    cfg: KubeConfig,
    api: Api,
    registry_http: reqwest::Client,
    egress: Arc<dyn EgressRegistry>,
    usage: Arc<dyn RuntimeUsageSink>,
    statics: Mutex<HashMap<InstanceId, StaticInst>>,
    locals: Mutex<HashMap<InstanceId, Arc<Local>>>,
    /// The address of each running instance this process saw (for the gateway), with the run
    /// it belongs to: an observation of another run or state forgets it, so the gateway never
    /// gets the address of an earlier run (which can be another pod by now).
    addrs: Mutex<HashMap<InstanceId, (u32, String, u16)>>,
    last_register: Mutex<HashMap<String, Instant>>,
    /// The byte budget of the running instances' log rings (S-91).
    live: Arc<LogBudget>,
    /// Positive `TokenReview`s: the token's SHA-256, the pod name and uid, and until when.
    reviews: Mutex<HashMap<[u8; 32], (String, String, Instant)>>,
    /// The highest seal generation this process saw per instance (a rollback is refused).
    gens: Mutex<HashMap<InstanceId, u64>>,
    /// Supervisor requests per peer address in the current window.
    peers: Mutex<HashMap<IpAddr, (Instant, u32)>>,
}

/// The Kubernetes driver (see the module docs).
pub struct KubeDriver {
    inner: Arc<Inner>,
    sweeper: Option<JoinHandle<()>>,
}

impl Drop for KubeDriver {
    fn drop(&mut self) {
        if let Some(s) = self.sweeper.take() {
            s.abort();
        }
    }
}

fn not_found(instance: InstanceId) -> RuntimeError {
    RuntimeError::NotFound { instance }
}

fn state_err(operation: &str, state: &InstanceState) -> RuntimeError {
    RuntimeError::InvalidState {
        operation: operation.to_owned(),
        state: state.name().to_owned(),
    }
}

fn ms(d: Duration) -> i64 {
    d.as_millis().min(i64::MAX as u128) as i64
}

impl KubeDriver {
    /// A driver over the API of `cfg`. It makes no API call here; a sweep task starts on the
    /// current Tokio runtime.
    pub fn new(
        cfg: KubeConfig,
        egress: Arc<dyn EgressRegistry>,
        usage: Arc<dyn RuntimeUsageSink>,
    ) -> Result<Self, RuntimeError> {
        cfg.check()?;
        let api = Api::new(
            &cfg.api_url,
            &cfg.namespace,
            cfg.token_file.clone(),
            cfg.ca_file.as_deref(),
            cfg.api_timeout,
        )?;
        let registry_http = reqwest::Client::builder()
            .no_proxy()
            .timeout(cfg.api_timeout)
            .build()
            .map_err(|e| RuntimeError::Internal {
                reason: format!("the registry client: {e}"),
            })?;
        let live = LogBudget::new(cfg.max_live_log_bytes, cfg.max_tenant_live_log_bytes);
        let inner = Arc::new(Inner {
            cfg,
            api,
            registry_http,
            egress,
            usage,
            statics: Mutex::new(HashMap::new()),
            locals: Mutex::new(HashMap::new()),
            addrs: Mutex::new(HashMap::new()),
            last_register: Mutex::new(HashMap::new()),
            live,
            reviews: Mutex::new(HashMap::new()),
            gens: Mutex::new(HashMap::new()),
            peers: Mutex::new(HashMap::new()),
        });
        let sweeper = tokio::runtime::Handle::try_current()
            .ok()
            .map(|h| h.spawn(sweep_loop(Arc::downgrade(&inner))));
        Ok(Self { inner, sweeper })
    }

    /// Where the gateway reaches a running instance: the pod's address and the service port
    /// that the last `status` of this process saw. The core's sandbox directory calls `status`
    /// before it asks (its locator).
    pub fn upstream(&self, instance: InstanceId) -> Option<(String, u16)> {
        self.inner
            .addrs
            .lock()
            .expect("addrs lock")
            .get(&instance)
            .map(|(_, ip, port)| (ip.clone(), *port))
    }

    /// How many instances this process keeps the logs of (the tests).
    #[doc(hidden)]
    pub fn kept_instances(&self) -> usize {
        self.inner.locals.lock().expect("locals lock").len()
    }

    /// One sweep now (the tests; the task runs it every `sweep_every`).
    pub async fn sweep(&self) {
        self.inner.sweep().await;
    }

    /// The bytes of the running instances' log rings, in all or of one tenant (the tests).
    #[doc(hidden)]
    pub fn live_log_bytes(&self, tenant: Option<TenantId>) -> usize {
        self.inner.live.used(tenant)
    }

    /// `POST /v1/launch` of the supervisor listener (backlog S-89; `docs/STEP3.md` "The sandbox
    /// supervisor" 4): the launch data, as JSON, for the pod whose projected token is `bearer`,
    /// from the connection's peer address `peer`, once per pod. Every refusal names no value.
    pub async fn supervisor_launch(
        &self,
        bearer: &str,
        peer: IpAddr,
    ) -> Result<Vec<u8>, LaunchRefusal> {
        self.inner.supervisor(bearer, peer, true).await
    }

    /// `POST /v1/token` of the supervisor listener: the current workload token, as JSON, for a
    /// pod that took its launch data.
    pub async fn supervisor_token(
        &self,
        bearer: &str,
        peer: IpAddr,
    ) -> Result<Vec<u8>, LaunchRefusal> {
        self.inner.supervisor(bearer, peer, false).await
    }
}

/// Why the supervisor listener refuses a request. The server answers `403`, `503` or `429`
/// without a detail; the driver logs the reason (never a value).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchRefusal {
    /// Not this pod's, not now, not again: `403`.
    Forbidden,
    /// The pod's run is not sealed yet, or the API server cannot be asked: `503` (the supervisor
    /// tries again).
    NotReady,
    /// Too many requests from the peer address: `429`.
    TooMany,
}

impl Inner {
    fn local(&self, id: InstanceId) -> Option<Arc<Local>> {
        self.locals.lock().expect("locals lock").get(&id).cloned()
    }

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
                    reason: format!("the driver has no shared service for {}", r.as_str()),
                });
            }
        }
        Ok(())
    }

    /// The image reference after the digest was found in the repository of the spec's tenant
    /// and app, at the registry itself.
    async fn image(
        &self,
        identity: &WorkloadIdentity,
        digest: &OciDigest,
    ) -> Result<String, RuntimeError> {
        let Some(registry) = &self.cfg.registry else {
            return Err(RuntimeError::ImageNotFound {
                image: digest.to_string(),
            });
        };
        let base = self
            .cfg
            .registry_url
            .clone()
            .unwrap_or_else(|| format!("http://{registry}"));
        let url = format!(
            "{}/v2/{}/manifests/{}",
            base.trim_end_matches('/'),
            repository(identity),
            digest
        );
        let accept = "application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.docker.distribution.manifest.v2+json";
        let r = self
            .registry_http
            .head(&url)
            .header(reqwest::header::ACCEPT, accept)
            .send()
            .await
            .map_err(|e| RuntimeError::Unavailable {
                reason: format!("the registry: {}", e.without_url()),
            })?;
        match r.status() {
            s if s.is_success() => Ok(format!("{registry}/{}@{digest}", repository(identity))),
            reqwest::StatusCode::NOT_FOUND => Err(RuntimeError::ImageNotFound {
                image: digest.to_string(),
            }),
            s => Err(RuntimeError::Unavailable {
                reason: format!("the registry answered {s}"),
            }),
        }
    }

    /// `GET` a registry object under a cap, checked against its digest (`sha256:<hex>`).
    async fn registry_get(
        &self,
        url: &str,
        accept: &str,
        digest: &str,
    ) -> Result<Vec<u8>, RuntimeError> {
        const MAX: usize = 4 * 1024 * 1024;
        let mut r = self
            .registry_http
            .get(url)
            .header(reqwest::header::ACCEPT, accept)
            .send()
            .await
            .map_err(|e| RuntimeError::Unavailable {
                reason: format!("the registry: {}", e.without_url()),
            })?;
        if r.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(RuntimeError::ImageNotFound {
                image: digest.to_owned(),
            });
        }
        if !r.status().is_success() {
            return Err(RuntimeError::Unavailable {
                reason: format!("the registry answered {}", r.status()),
            });
        }
        let mut body = Vec::new();
        while let Some(chunk) = r.chunk().await.map_err(|e| RuntimeError::Unavailable {
            reason: format!("the registry: {}", e.without_url()),
        })? {
            body.extend_from_slice(&chunk);
            if body.len() > MAX {
                return Err(RuntimeError::UnsupportedImage {
                    reason: "an image manifest or config is over 4 MiB".to_owned(),
                });
            }
        }
        if OciDigest::of(&body).as_str() != digest {
            return Err(RuntimeError::UnsupportedImage {
                reason: "an image object does not match its digest".to_owned(),
            });
        }
        Ok(body)
    }

    /// The image's `Entrypoint` and `Cmd` (from its config at the registry, each object checked
    /// against its digest; for a multi-arch index the nodes' platform), for the supervisor's
    /// launch data (I6-20).
    async fn image_command(
        &self,
        identity: &WorkloadIdentity,
        digest: &OciDigest,
    ) -> Result<(Vec<String>, Vec<String>), RuntimeError> {
        let Some(registry) = &self.cfg.registry else {
            return Err(RuntimeError::ImageNotFound {
                image: digest.to_string(),
            });
        };
        let base = self
            .cfg
            .registry_url
            .clone()
            .unwrap_or_else(|| format!("http://{registry}"));
        let base = base.trim_end_matches('/');
        let repo = repository(identity);
        let accept = "application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.docker.distribution.manifest.v2+json";
        let bad = |what: &str| RuntimeError::UnsupportedImage {
            reason: format!("the image {what}"),
        };
        let raw = self
            .registry_get(
                &format!("{base}/v2/{repo}/manifests/{digest}"),
                accept,
                digest.as_str(),
            )
            .await?;
        let mut manifest: Value =
            serde_json::from_slice(&raw).map_err(|_| bad("manifest is not JSON"))?;
        if let Some(list) = manifest.get("manifests").and_then(Value::as_array) {
            let pick = list
                .iter()
                .find(|m| {
                    m.pointer("/platform/os").and_then(Value::as_str) == Some("linux")
                        && m.pointer("/platform/architecture").and_then(Value::as_str)
                            == Some(self.cfg.node_arch.as_str())
                })
                .and_then(|m| m.get("digest").and_then(Value::as_str))
                .ok_or_else(|| bad("has no manifest for the nodes' platform"))?
                .to_owned();
            let raw = self
                .registry_get(&format!("{base}/v2/{repo}/manifests/{pick}"), accept, &pick)
                .await?;
            manifest = serde_json::from_slice(&raw).map_err(|_| bad("manifest is not JSON"))?;
        }
        let config = manifest
            .pointer("/config/digest")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("manifest names no config"))?
            .to_owned();
        let raw = self
            .registry_get(&format!("{base}/v2/{repo}/blobs/{config}"), "*/*", &config)
            .await?;
        let cfg: Value = serde_json::from_slice(&raw).map_err(|_| bad("config is not JSON"))?;
        let strings = |p: &str| -> Vec<String> {
            cfg.pointer(p)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        };
        Ok((strings("/config/Entrypoint"), strings("/config/Cmd")))
    }

    /// The `/etc/hosts` lines of the record's routes: each shared service's address, resolved
    /// in this process.
    async fn hosts(&self, rec: &Record) -> Result<Vec<HostAlias>, RuntimeError> {
        let mut out = Vec::new();
        for r in &rec.routes {
            let s = self
                .cfg
                .routes
                .get(r)
                .ok_or_else(|| RuntimeError::Unavailable {
                    reason: format!("the driver has no shared service for {}", r.as_str()),
                })?;
            let ip = match s.address.parse::<std::net::IpAddr>() {
                Ok(ip) => ip,
                Err(_) => tokio::net::lookup_host((s.address.as_str(), s.port))
                    .await
                    .ok()
                    .and_then(|mut a| a.next())
                    .map(|a| a.ip())
                    .ok_or_else(|| RuntimeError::Unavailable {
                        reason: format!("the shared service of {} does not resolve", r.as_str()),
                    })?,
            };
            out.push(HostAlias {
                ip: ip.to_string(),
                alias: s.alias.clone(),
            });
        }
        Ok(out)
    }

    fn network(&self, id: &WorkloadIdentity) -> NetworkId {
        // Every pod is its own network (no pod reaches another); the ID names the app and
        // environment, so two apps, or the live and the preview of one app, never share one.
        NetworkId(format!(
            "{}/{}/{}/{}",
            self.cfg.namespace,
            id.tenant.0.simple(),
            id.app.0.simple(),
            id.environment.as_str()
        ))
    }

    fn status_of(&self, rec: &Record, health: Health) -> InstanceStatus {
        InstanceStatus {
            id: rec.instance,
            identity: rec.identity,
            kind: rec.kind.name(),
            state: rec.state,
            health,
            applied: Applied {
                routes: rec.routes.clone(),
                network: Some(self.network(&rec.identity)),
                limits: rec.limits,
            },
            started_at: pod::from_ms(rec.started_at_ms),
        }
    }

    /// The record of an instance of `tenant`: `NotFound` for a missing one or one of another
    /// tenant.
    async fn record(
        &self,
        tenant: TenantId,
        id: InstanceId,
    ) -> Result<(Record, String), RuntimeError> {
        let cm = self
            .api
            .get(Kind::ConfigMap, &object_name(id))
            .await?
            .ok_or_else(|| not_found(id))?;
        match pod::read_record(&cm) {
            Some((rec, rv)) if rec.identity.tenant == tenant && rec.instance == id => Ok((rec, rv)),
            _ => Err(not_found(id)),
        }
    }

    /// Replaces the record if nobody changed it since `rv`. `Ok(false)`: somebody did.
    async fn put(&self, rec: &Record, rv: &str) -> Result<bool, RuntimeError> {
        match self
            .api
            .replace(
                Kind::ConfigMap,
                &object_name(rec.instance),
                &pod::record_body(&self.cfg, rec, Some(rv)),
            )
            .await?
        {
            Write::Done(_) => Ok(true),
            Write::Conflict => Ok(false),
            Write::Gone => Err(not_found(rec.instance)),
        }
    }

    async fn unregister(&self, pod: &str) -> bool {
        for _ in 0..UNREGISTER_TRIES {
            if self.egress.unregister(pod).await.is_ok() {
                self.last_register
                    .lock()
                    .expect("register lock")
                    .remove(pod);
                return true;
            }
        }
        tracing::warn!(target: "ruum::runtime", pod, "the egress unregistration failed; the sweep tries again");
        false
    }

    fn record_usage(&self, rec: &Record) {
        let ran = (pod::now_ms() - rec.run_started_ms).max(1) as u64;
        self.usage.record(RuntimeUsage::Instance {
            identity: rec.identity,
            instance: rec.instance,
            ran: Duration::from_millis(ran),
            memory_bytes: rec.limits.memory_bytes,
        });
    }

    /// Ends the current run: the record moves to `to` (only if nobody changed it), and the
    /// process that made the change does the rest: usage, the unregistration, then the pod.
    /// `Ok(None)`: another process was first.
    async fn end_run(
        &self,
        mut rec: Record,
        rv: &str,
        to: InstanceState,
    ) -> Result<Option<Record>, RuntimeError> {
        let was_running = matches!(rec.state, InstanceState::Starting | InstanceState::Running);
        let pod = pod_name(rec.instance, rec.run);
        let registered = rec.registered.clone();
        rec.state = to;
        if matches!(to, InstanceState::Stopped | InstanceState::Exited(_)) {
            rec.ended_ms = Some(pod::now_ms());
        }
        if !self.put(&rec, rv).await? {
            return Ok(None);
        }
        if was_running {
            self.record_usage(&rec);
        }
        self.addrs.lock().expect("addrs lock").remove(&rec.instance);
        // An exited pod holds no address and no resources; it stays until the logs are read
        // (the sweep removes it after `EXITED_POD_KEEP`).
        let delete_pod = !matches!(to, InstanceState::Exited(_));
        self.finish_cleanup(rec.clone(), registered, delete_pod.then_some(pod.as_str()))
            .await;
        if let Some(l) = self.local(rec.instance) {
            let mut st = l.st.lock().expect("local lock");
            match to {
                InstanceState::Stopped | InstanceState::Exited(_) => st.set_ended(),
                InstanceState::ScaledToZero => st.set_parked(),
                _ => {}
            }
        }
        Ok(Some(rec))
    }

    /// The record of `id` with its ConfigMap's uid (the seal's associated data).
    async fn record_with_uid(
        &self,
        id: InstanceId,
    ) -> Result<(Record, String, String), RuntimeError> {
        let cm = self.reload(id).await?;
        let uid = cm
            .pointer("/metadata/uid")
            .and_then(Value::as_str)
            .filter(|u| !u.is_empty())
            .ok_or_else(|| RuntimeError::Internal {
                reason: "an instance record has no uid".to_owned(),
            })?
            .to_owned();
        let (rec, rv) = pod::read_record(&cm).ok_or_else(|| RuntimeError::Internal {
            reason: "an instance record does not read".to_owned(),
        })?;
        Ok((rec, rv, uid))
    }

    fn key(&self) -> &RecordKey {
        // `KubeConfig::check` refuses a configuration without it.
        self.cfg.record_key.as_ref().expect("the record key")
    }

    /// The sealed part of a record, opened and checked against the record's fixed fields.
    /// `None`: no seal yet, or one that does not open or does not match (a writer without the
    /// record key changed the record).
    fn unsealed(&self, rec: &Record, record_uid: &str) -> Option<Sealed> {
        let s = seal::unseal(
            self.key(),
            &object_name(rec.instance),
            record_uid,
            rec.sealed.as_deref()?,
        )?;
        (s.digest == pod::fixed_digest(rec)).then_some(s)
    }

    /// Notes the generation of a seal this process read or wrote; `false` for a generation
    /// below the highest it saw (a seal put back: refused).
    fn fresh_gen(&self, id: InstanceId, generation: u64) -> bool {
        let mut g = self.gens.lock().expect("gens lock");
        let seen = g.entry(id).or_insert(0);
        if generation < *seen {
            return false;
        }
        *seen = generation;
        true
    }

    /// Changes the sealed part of the record of `id` with `change` (given the opened seal, or
    /// `fresh` for a record without one), one generation up, and writes it with the record's
    /// `resourceVersion`; a lost race reads the record again. `change` returns `false` to stop
    /// (the record moved on).
    async fn reseal(
        &self,
        id: InstanceId,
        fresh: Option<&Sealed>,
        change: impl Fn(&Record, &mut Sealed) -> bool,
    ) -> Result<(), RuntimeError> {
        for _ in 0..SEAL_TRIES {
            let (mut rec, rv, uid) = self.record_with_uid(id).await?;
            let mut s = match (rec.sealed.is_some(), fresh) {
                (true, _) => self
                    .unsealed(&rec, &uid)
                    .ok_or_else(|| RuntimeError::Internal {
                        reason: "an instance record's seal does not open or does not match it"
                            .to_owned(),
                    })?,
                (false, Some(f)) => f.clone(),
                (false, None) => {
                    return Err(RuntimeError::Internal {
                        reason: "an instance record has no seal".to_owned(),
                    });
                }
            };
            if !change(&rec, &mut s) {
                return Err(RuntimeError::Unavailable {
                    reason: "the instance changed while its seal was written".to_owned(),
                });
            }
            s.generation += 1;
            s.digest = pod::fixed_digest(&rec);
            rec.sealed = Some(
                seal::seal(self.key(), &object_name(id), &uid, &s)
                    .map_err(|reason| RuntimeError::Internal { reason })?,
            );
            if self.put(&rec, &rv).await? {
                self.fresh_gen(id, s.generation);
                return Ok(());
            }
        }
        Err(RuntimeError::Unavailable {
            reason: "the instance record changes too often".to_owned(),
        })
    }

    /// The unregistration, then the pod. With a failed unregistration the pod stays (so its
    /// address is not reused while the proxy still names this app for it) and the record keeps
    /// `registered`, for the sweep.
    async fn finish_cleanup(&self, rec: Record, registered: Option<String>, pod: Option<&str>) {
        if let Some(reg) = registered {
            if !self.unregister(&reg).await {
                return;
            }
            // Clear `registered` (best effort: a lost race leaves it for the sweep, and an
            // unregistration is idempotent).
            if let Ok((mut now, rv)) = self.record(rec.identity.tenant, rec.instance).await
                && now.registered.as_deref() == Some(reg.as_str())
            {
                now.registered = None;
                let _ = self.put(&now, &rv).await;
            }
        }
        let Some(pod) = pod else { return };
        let grace = self.cfg.stop_timeout.as_secs();
        if let Err(e) = self.api.delete(Kind::Pod, pod, Some(grace)).await {
            tracing::warn!(target: "ruum::runtime", code = e.code(), "deleting a sandbox pod failed; the sweep tries again");
        }
    }

    /// Keeps the gateway's address of an instance only when it belongs to `run` (`None`: forgets
    /// it).
    fn keep_addr(&self, id: InstanceId, run: Option<u32>) {
        let mut addrs = self.addrs.lock().expect("addrs lock");
        if addrs.get(&id).is_some_and(|(r, _, _)| Some(*r) != run) {
            addrs.remove(&id);
        }
    }

    /// The status of an instance from its record and its pod, making the state changes the pod
    /// shows (exit, idle, registration). Every process may run it; the record's
    /// resourceVersion lets only one make each change.
    async fn observe(&self, mut cm: Value) -> Result<InstanceStatus, RuntimeError> {
        for _ in 0..4 {
            let Some((mut rec, mut rv)) = pod::read_record(&cm) else {
                return Err(RuntimeError::Internal {
                    reason: "an instance record does not read".to_owned(),
                });
            };
            let id = rec.instance;
            let live = matches!(rec.state, InstanceState::Starting | InstanceState::Running);
            self.keep_addr(id, live.then_some(rec.run));
            if !live {
                return Ok(self.status_of(&rec, Health::Unknown));
            }
            let pod_n = pod_name(id, rec.run);
            let pod = self.api.get(Kind::Pod, &pod_n).await?;
            let view = pod.as_ref().map(pod::view);
            if !matches!(view, Some(PodView::Running { .. })) {
                self.keep_addr(id, None);
            }
            let now = pod::now_ms();
            let changed = match view {
                // A run that is still being made (the record is written before its pod, by this
                // or another process).
                None if rec.state == InstanceState::Starting
                    && now - rec.run_started_ms < ms(START_GRACE) =>
                {
                    return Ok(self.status_of(&rec, Health::Unknown));
                }
                None => {
                    // The pod is gone (an eviction, a node loss, a delete by hand).
                    self.end_run(
                        rec,
                        &rv,
                        InstanceState::Exited(ExitStatus::of(ExitReason::Killed)),
                    )
                    .await?
                }
                Some(PodView::Pending { broken: true }) => {
                    self.end_run(
                        rec,
                        &rv,
                        InstanceState::Exited(ExitStatus::of(ExitReason::Killed)),
                    )
                    .await?
                }
                Some(PodView::Ended(exit)) => {
                    // The log of the run is read to its end first (by this process's followers).
                    self.drain(id, FOLLOWER_DRAIN).await;
                    self.end_run(rec, &rv, InstanceState::Exited(exit)).await?
                }
                // Not running yet, or being deleted (by another process's state change, or by
                // hand: once it is gone the run ends as killed).
                Some(PodView::Pending { broken: false }) | Some(PodView::Terminating) => {
                    return Ok(self.status_of(&rec, Health::Unknown));
                }
                Some(PodView::Running { ip, ready, started }) => {
                    if let KindRec::Job { timeout_ms } = rec.kind
                        && now - rec.run_started_ms > timeout_ms as i64 + ms(JOB_DEADLINE_SLACK)
                    {
                        self.end_run(
                            rec,
                            &rv,
                            InstanceState::Exited(ExitStatus::of(ExitReason::TimedOut)),
                        )
                        .await?
                    } else if self.idle(&rec, now) {
                        self.end_run(rec, &rv, InstanceState::ScaledToZero).await?
                    } else {
                        let Some(ip) = ip else {
                            return Ok(self.status_of(&rec, Health::Unknown));
                        };
                        if rec.routes.contains(&Route::EgressProxy)
                            && (rec.registered.as_deref() != Some(pod_n.as_str())
                                || rec.state == InstanceState::Starting)
                        {
                            // Registered before the instance counts as running; until then it is
                            // starting (the proxy refuses it anyway: fails closed). The record
                            // names the registration first, so a process that ends the run
                            // meanwhile always unregisters it (an unregistration is idempotent).
                            if rec.registered.as_deref() != Some(pod_n.as_str()) {
                                if let Some(old) = rec.registered.clone()
                                    && !self.unregister(&old).await
                                {
                                    return Ok(self.status_of(&rec, Health::Unknown));
                                }
                                rec.registered = Some(pod_n.clone());
                                if !self.put(&rec, &rv).await? {
                                    cm = self.reload(id).await?;
                                    continue;
                                }
                            }
                            if let Err(e) = self.register(&rec, &pod_n, &ip).await {
                                tracing::warn!(target: "ruum::runtime", code = e.code(), "registering a sandbox at the egress proxy failed");
                                return Ok(self.status_of(&rec, Health::Unknown));
                            }
                            // The run may have ended while the proxy was asked: then the
                            // registration goes again.
                            match self.confirm(&rec, &pod_n).await? {
                                Some((now_rec, now_rv)) => {
                                    rec = now_rec;
                                    rv = now_rv;
                                }
                                None => {
                                    cm = self.reload(id).await?;
                                    continue;
                                }
                            }
                        }
                        if rec.state == InstanceState::Starting {
                            rec.state = InstanceState::Running;
                            if !self.put(&rec, &rv).await? {
                                cm = self.reload(id).await?;
                                continue;
                            }
                        }
                        if let KindRec::Service { port, .. } = rec.kind {
                            self.addrs
                                .lock()
                                .expect("addrs lock")
                                .insert(id, (rec.run, ip.clone(), port));
                        }
                        let health = self.health(&rec, ready, started);
                        return Ok(self.status_of(&rec, health));
                    }
                }
            };
            match changed {
                Some(rec) => return Ok(self.status_of(&rec, Health::Unknown)),
                None => cm = self.reload(id).await?,
            }
        }
        Err(RuntimeError::Unavailable {
            reason: "the instance record changes too often".to_owned(),
        })
    }

    /// After a registration of `pod` (the run of `rec`): the record read again. `Some` when it
    /// still names that registration for a live run of the same run number; otherwise the run
    /// ended meanwhile (its process may have unregistered before this registration), so this
    /// unregisters it, and when that fails puts the name back on the ended record for the sweep.
    async fn confirm(
        &self,
        rec: &Record,
        pod: &str,
    ) -> Result<Option<(Record, String)>, RuntimeError> {
        let now = match self.record(rec.identity.tenant, rec.instance).await {
            Ok(r) => Some(r),
            Err(RuntimeError::NotFound { .. }) => None,
            Err(e) => {
                // Unknown: take the registration back, the next observation registers again.
                self.unregister(pod).await;
                return Err(e);
            }
        };
        if let Some((r, rv)) = &now
            && matches!(r.state, InstanceState::Starting | InstanceState::Running)
            && r.run == rec.run
            && r.registered.as_deref() == Some(pod)
        {
            return Ok(Some((r.clone(), rv.clone())));
        }
        if !self.unregister(pod).await
            && let Some((mut r, rv)) = now
            && r.registered.is_none()
            && !matches!(r.state, InstanceState::Starting | InstanceState::Running)
        {
            r.registered = Some(pod.to_owned());
            let _ = self.put(&r, &rv).await;
        }
        Ok(None)
    }

    /// Waits at most `bound` until the log followers of this process for the instance ended.
    async fn drain(&self, id: InstanceId, bound: Duration) {
        let Some(local) = self.local(id) else { return };
        let deadline = Instant::now() + bound;
        loop {
            let done = local
                .st
                .lock()
                .expect("local lock")
                .followers
                .iter()
                .all(JoinHandle::is_finished);
            if done || Instant::now() > deadline {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn reload(&self, id: InstanceId) -> Result<Value, RuntimeError> {
        self.api
            .get(Kind::ConfigMap, &object_name(id))
            .await?
            .ok_or_else(|| not_found(id))
    }

    /// Whether a service is past its idle timeout (by the newest touch of the record and of
    /// this process).
    fn idle(&self, rec: &Record, now: i64) -> bool {
        let KindRec::Service {
            idle_timeout_ms: Some(idle),
            ..
        } = rec.kind
        else {
            return false;
        };
        let local = self
            .local(rec.instance)
            .map(|l| l.st.lock().expect("local lock").touched_ms)
            .unwrap_or(0);
        let touched = rec.last_touch_ms.max(local).max(rec.run_started_ms);
        now - touched > idle as i64
    }

    fn health(&self, rec: &Record, ready: bool, started: Option<OffsetDateTime>) -> Health {
        let KindRec::Service {
            health: Some(h), ..
        } = &rec.kind
        else {
            return Health::Unknown;
        };
        if ready {
            return Health::Healthy;
        }
        let period = h.interval_ms.div_ceil(1000).max(1) * 1000;
        let allowed = ms(self.cfg.start_period) + (period * u64::from(h.failure_threshold)) as i64;
        let since = started
            .map(|t| (t.unix_timestamp_nanos() / 1_000_000) as i64)
            .unwrap_or(rec.run_started_ms);
        if pod::now_ms() - since > allowed {
            Health::Unhealthy
        } else {
            Health::Unknown
        }
    }

    async fn register(&self, rec: &Record, pod: &str, ip: &str) -> Result<(), RuntimeError> {
        let subnet = if ip.contains(':') {
            format!("{ip}/128")
        } else {
            format!("{ip}/32")
        };
        self.egress
            .register(&EgressNetwork {
                network: pod.to_owned(),
                subnet,
                identity: rec.identity,
                allow: None,
            })
            .await?;
        self.last_register
            .lock()
            .expect("register lock")
            .insert(pod.to_owned(), Instant::now());
        Ok(())
    }

    /// The supervisor listener as the sandboxes reach it: `http://<ip>:<port>` (the address is
    /// resolved in this process; the sandbox has no DNS).
    async fn launch_url(&self) -> Result<String, RuntimeError> {
        let a = &self.cfg.supervisor_address;
        let ip = match a.parse::<IpAddr>() {
            Ok(ip) => ip,
            Err(_) => tokio::net::lookup_host((a.as_str(), self.cfg.supervisor_port))
                .await
                .ok()
                .and_then(|mut x| x.next())
                .map(|x| x.ip())
                .ok_or_else(|| RuntimeError::Unavailable {
                    reason: "the supervisor listener's address does not resolve".to_owned(),
                })?,
        };
        Ok(format!(
            "http://{}",
            std::net::SocketAddr::new(ip, self.cfg.supervisor_port)
        ))
    }

    /// The pod the driver makes for the record's run (also the reference of the read-back).
    async fn expected_pod(&self, rec: &Record) -> Result<Value, RuntimeError> {
        let hosts = self.hosts(rec).await?;
        let url = self.launch_url().await?;
        Ok(pod::pod_body(&self.cfg, rec, &hosts, &url))
    }

    /// Creates the pod of the record's run, checks what the API server stored, seals the pod's
    /// uid as the one that may take the run's launch data (`fresh`: the first seal of a new
    /// instance), and starts the log followers of this process.
    async fn begin_run(
        self: &Arc<Self>,
        rec: &Record,
        fresh: Option<&Sealed>,
    ) -> Result<(), RuntimeError> {
        let body = self.expected_pod(rec).await?;
        let name = pod_name(rec.instance, rec.run);
        let stored = match self.api.create(Kind::Pod, &body).await? {
            Write::Done(v) => v,
            Write::Conflict | Write::Gone => {
                return Err(RuntimeError::Internal {
                    reason: format!("the pod {name} exists already"),
                });
            }
        };
        if let Err(what) = pod::verify(&self.cfg, &body, &stored) {
            let _ = self.api.delete(Kind::Pod, &name, Some(0)).await;
            return Err(RuntimeError::Internal {
                reason: format!("the API server did not keep the sandbox: {what}"),
            });
        }
        let Some(pod_uid) = stored
            .pointer("/metadata/uid")
            .and_then(Value::as_str)
            .filter(|u| !u.is_empty())
            .map(str::to_owned)
        else {
            let _ = self.api.delete(Kind::Pod, &name, Some(0)).await;
            return Err(RuntimeError::Internal {
                reason: format!("the API server gave the pod {name} no uid"),
            });
        };
        // The provenance of the pod (S-89): only a server process can write the seal, and a
        // pod's uid is never reused. The supervisor's launch waits for it (503 until then).
        let run = rec.run;
        let sealed = self
            .reseal(rec.instance, fresh, |now, s| {
                if now.run != run
                    || !matches!(now.state, InstanceState::Starting | InstanceState::Running)
                {
                    return false;
                }
                s.run = run;
                s.pod_uid = Some(pod_uid.clone());
                true
            })
            .await;
        if let Err(e) = sealed {
            let _ = self.api.delete(Kind::Pod, &name, Some(0)).await;
            return Err(e);
        }
        if let Some(local) = self.local(rec.instance) {
            self.follow(&local, rec.run);
        }
        Ok(())
    }

    /// Starts the followers of a run's log (one per stream, or one for both).
    fn follow(self: &Arc<Self>, local: &Arc<Local>, run: u32) {
        let mut st = local.st.lock().expect("local lock");
        if st.followed_run >= run {
            return;
        }
        st.followed_run = run;
        st.parked_at = None;
        st.ring.join_budget();
        let redactors = Arc::new(Mutex::new(Redactors::new(
            &st.values(),
            self.cfg.max_line_bytes,
        )));
        *local.redactors.lock().expect("redactors lock") = Some(redactors.clone());
        let pod = pod_name(local.id, run);
        let streams: Vec<(Option<&'static str>, LogStream)> = if self.cfg.split_log_streams {
            vec![
                (Some("Stdout"), LogStream::Stdout),
                (Some("Stderr"), LogStream::Stderr),
            ]
        } else {
            vec![(None, LogStream::Stdout)]
        };
        for (q, stream) in streams {
            let task = tokio::spawn(follow_log(
                self.clone(),
                local.clone(),
                redactors.clone(),
                pod.clone(),
                q,
                stream,
            ));
            st.followers.push(task);
        }
        st.followers.retain(|t| !t.is_finished());
    }

    /// One round: every instance of this driver (by its labels), the state changes their pods
    /// show, cleanup of what a failed step or a crash left, and the retention.
    async fn sweep(self: &Arc<Self>) {
        let mut sel: Vec<(&str, &str)> = vec![(LABEL_MANAGED, MANAGED)];
        for (k, v) in &self.cfg.extra_labels {
            sel.push((k.as_str(), v.as_str()));
        }
        let records = match self.api.list(Kind::ConfigMap, &sel).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(target: "ruum::runtime", code = e.code(), error = %e, "the sweep cannot list the instance records");
                return;
            }
        };
        let mut live_pods: HashMap<String, bool> = HashMap::new();
        let mut seen: std::collections::HashSet<InstanceId> = std::collections::HashSet::new();
        let now = pod::now_ms();
        for cm in records {
            let Some((rec, _)) = pod::read_record(&cm) else {
                continue;
            };
            let id = rec.instance;
            seen.insert(id);
            match rec.state {
                InstanceState::Starting | InstanceState::Running => {
                    live_pods.insert(pod_name(id, rec.run), true);
                    match self.observe(cm).await {
                        Ok(st) => {
                            if let Some(l) = self.local(id)
                                && matches!(
                                    st.state,
                                    InstanceState::Starting | InstanceState::Running
                                )
                            {
                                let run = self
                                    .record(rec.identity.tenant, id)
                                    .await
                                    .map(|(r, _)| r.run)
                                    .unwrap_or(rec.run);
                                self.follow(&l, run);
                            }
                            if st.state == InstanceState::Running {
                                self.reregister(&rec).await;
                            }
                        }
                        Err(e) => {
                            tracing::debug!(target: "ruum::runtime", code = e.code(), "the sweep could not observe an instance")
                        }
                    }
                }
                _ => {
                    // Another process may have ended it: its logs here start their retention. Or
                    // scaled it to zero: its logs here count to the byte cap.
                    if let Some(l) = self.local(id) {
                        let mut st = l.st.lock().expect("local lock");
                        if matches!(rec.state, InstanceState::Stopped | InstanceState::Exited(_)) {
                            st.set_ended();
                        } else if rec.state == InstanceState::ScaledToZero {
                            st.set_parked();
                        }
                    }
                    // A registration or a pod that a failed step left.
                    if let Some(reg) = rec.registered.clone() {
                        live_pods.insert(reg.clone(), true);
                        let pod = pod_name(id, rec.run);
                        self.finish_cleanup(rec.clone(), Some(reg), Some(&pod))
                            .await;
                    }
                    // An exited pod stays a short time, for the log followers.
                    if matches!(rec.state, InstanceState::Exited(_))
                        && rec.ended_ms.is_some_and(|t| now - t < ms(EXITED_POD_KEEP))
                    {
                        live_pods.insert(pod_name(id, rec.run), true);
                    }
                    let expired = rec
                        .ended_ms
                        .is_some_and(|t| now - t > ms(self.cfg.ended_retention));
                    if expired && rec.registered.is_none() {
                        let _ = self
                            .api
                            .delete(Kind::ConfigMap, &object_name(id), None)
                            .await;
                        self.drop_local(id);
                    }
                }
            }
        }
        // Pods without a running record (a crash between the record and the delete).
        if let Ok(pods) = self.api.list(Kind::Pod, &sel).await {
            for p in pods {
                let Some(name) = p.pointer("/metadata/name").and_then(Value::as_str) else {
                    continue;
                };
                // The unregistration first (idempotent; the pod of an earlier run may still be
                // registered when its record moved on): a failure keeps the pod, so its address
                // is not reused while the proxy may still name this app for it.
                if !live_pods.contains_key(name)
                    && self.orphan(name).await
                    && self.unregister(name).await
                {
                    let grace = self.cfg.stop_timeout.as_secs();
                    let _ = self.api.delete(Kind::Pod, name, Some(grace)).await;
                }
            }
        }
        // The logs of instances whose record is gone (another process removed it after the
        // retention). The record is read again: a start since the list made it after the list.
        let unseen: Vec<Arc<Local>> = self
            .locals
            .lock()
            .expect("locals lock")
            .values()
            .filter(|l| !seen.contains(&l.id) && l.made.elapsed() >= START_GRACE)
            .cloned()
            .collect();
        for l in unseen {
            if let Ok(None) = self.api.get(Kind::ConfigMap, &object_name(l.id)).await {
                self.drop_local(l.id);
            }
        }
        // Logs of ended instances after the retention, and within the caps.
        let _ = self.trim_locals(None);
        self.shed_live();
    }

    /// Brings the live log budget back under its bound (S-91): the oldest lines of the tenant
    /// with the most live log bytes go first, from its largest ring.
    fn shed_live(&self) {
        let locals: Vec<Arc<Local>> = self
            .locals
            .lock()
            .expect("locals lock")
            .values()
            .cloned()
            .collect();
        for _ in 0..locals.len().max(1) {
            let Some((excess, top)) = self.live.over_total() else {
                return;
            };
            let largest = locals
                .iter()
                .filter(|l| l.identity.tenant == top)
                .map(|l| {
                    let st = l.st.lock().expect("local lock");
                    (
                        if st.ring.counted() {
                            st.ring.memory()
                        } else {
                            0
                        },
                        l.clone(),
                    )
                })
                .max_by_key(|(b, _)| *b);
            match largest {
                Some((b, l)) if b > 0 => {
                    l.st.lock().expect("local lock").ring.shed(excess);
                }
                _ => return,
            }
        }
    }

    /// One more supervisor request of `peer` in the current window; `false` past the limit.
    fn peer_ok(&self, peer: IpAddr) -> bool {
        let mut m = self.peers.lock().expect("peers lock");
        if m.len() > 65_536 {
            m.retain(|_, (at, _)| at.elapsed() < PEER_WINDOW);
        }
        let e = m.entry(peer).or_insert((Instant::now(), 0));
        if e.0.elapsed() >= PEER_WINDOW {
            *e = (Instant::now(), 0);
        }
        e.1 += 1;
        e.1 <= PEER_REQUESTS
    }

    /// The pod (name, uid) of a projected token, by a `TokenReview` (kept a short time), with
    /// every field of the answer checked (`docs/STEP3.md` "The sandbox supervisor" 4).
    async fn review(&self, bearer: &str) -> Result<(String, String), LaunchRefusal> {
        use sha2::{Digest, Sha256};
        let refuse = |why: &str| {
            tracing::warn!(target: "ruum::runtime", reason = why, "a supervisor token was refused");
            LaunchRefusal::Forbidden
        };
        if bearer.is_empty() || bearer.len() > 16 * 1024 {
            return Err(refuse("no token or an oversized one"));
        }
        let hash: [u8; 32] = Sha256::digest(bearer.as_bytes()).into();
        if let Some((name, uid, until)) = self.reviews.lock().expect("reviews lock").get(&hash)
            && Instant::now() < *until
        {
            return Ok((name.clone(), uid.clone()));
        }
        let status = self
            .api
            .token_review(bearer, &self.cfg.supervisor_audience)
            .await
            .map_err(|e| {
                tracing::warn!(target: "ruum::runtime", code = e.code(), "a TokenReview failed");
                LaunchRefusal::NotReady
            })?;
        if status.get("authenticated") != Some(&Value::Bool(true)) {
            return Err(refuse("not authenticated"));
        }
        if status
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|e| !e.is_empty())
        {
            return Err(refuse("the review has an error"));
        }
        let aud_ok = status
            .get("audiences")
            .and_then(Value::as_array)
            .is_some_and(|a| {
                a.iter()
                    .any(|x| x.as_str() == Some(self.cfg.supervisor_audience.as_str()))
            });
        if !aud_ok {
            return Err(refuse("the audience was not granted"));
        }
        let user = format!(
            "system:serviceaccount:{}:{}",
            self.cfg.namespace, self.cfg.sandbox_service_account
        );
        if status.pointer("/user/username").and_then(Value::as_str) != Some(user.as_str()) {
            return Err(refuse("not the sandbox ServiceAccount"));
        }
        let one = |k: &str| -> Option<String> {
            let a = status.pointer("/user/extra")?.get(k)?.as_array()?;
            match a.as_slice() {
                [Value::String(v)] if !v.is_empty() => Some(v.clone()),
                _ => None,
            }
        };
        let (Some(name), Some(uid)) = (
            one("authentication.kubernetes.io/pod-name"),
            one("authentication.kubernetes.io/pod-uid"),
        ) else {
            return Err(refuse("not exactly one pod name and pod uid"));
        };
        // Kept at most REVIEW_TTL and never past the token's own expiry.
        let left = jwt_exp(bearer)
            .map(|exp| exp - pod::now_ms() / 1000)
            .unwrap_or(0);
        if left <= 0 {
            return Err(refuse("the token has no future expiry"));
        }
        let keep = REVIEW_TTL.min(Duration::from_secs(left as u64));
        let mut cache = self.reviews.lock().expect("reviews lock");
        if cache.len() >= REVIEW_CACHE_MAX {
            let now = Instant::now();
            cache.retain(|_, (_, _, until)| *until > now);
            if cache.len() >= REVIEW_CACHE_MAX {
                cache.clear();
            }
        }
        cache.insert(hash, (name.clone(), uid.clone(), Instant::now() + keep));
        Ok((name, uid))
    }

    /// The checks of one supervisor request after the token review: the pod exists now with
    /// that uid, is not being deleted and has the peer address; its record unseals and matches;
    /// the sealed run and pod uid are this pod's; the generation is not old; and the stored pod
    /// is exactly the driver's pod for the sealed record. Returns the opened seal and the record.
    async fn authorize(
        &self,
        pod_n: &str,
        pod_uid: &str,
        peer: IpAddr,
    ) -> Result<(Sealed, Record, String, String), LaunchRefusal> {
        let refuse = |why: &str| {
            tracing::warn!(target: "ruum::runtime", pod = pod_n, reason = why, "a supervisor request was refused");
            LaunchRefusal::Forbidden
        };
        let not_ready = |why: &str| {
            tracing::info!(target: "ruum::runtime", pod = pod_n, reason = why, "a supervisor request must wait");
            LaunchRefusal::NotReady
        };
        let (object, run) = pod_n
            .rsplit_once('-')
            .ok_or_else(|| refuse("not a sandbox pod name"))?;
        let run: u32 = run.parse().map_err(|_| refuse("not a sandbox pod name"))?;
        let id = object
            .strip_prefix("ruum-i-")
            .filter(|h| h.len() == 32)
            .and_then(|h| uuid::Uuid::parse_str(h).ok())
            .map(InstanceId)
            .ok_or_else(|| refuse("not a sandbox pod name"))?;
        let pod = self
            .api
            .get(Kind::Pod, pod_n)
            .await
            .map_err(|_| not_ready("the pod cannot be read"))?
            .ok_or_else(|| refuse("the pod does not exist"))?;
        if pod.pointer("/metadata/uid").and_then(Value::as_str) != Some(pod_uid) {
            return Err(refuse("the pod has another uid"));
        }
        if pod.pointer("/metadata/deletionTimestamp").is_some() {
            return Err(refuse("the pod is being deleted"));
        }
        let mut ips: Vec<IpAddr> = pod
            .pointer("/status/podIPs")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.get("ip").and_then(Value::as_str))
                    .filter_map(|x| x.parse::<IpAddr>().ok())
                    .collect()
            })
            .unwrap_or_default();
        if let Some(ip) = pod
            .pointer("/status/podIP")
            .and_then(Value::as_str)
            .and_then(|x| x.parse::<IpAddr>().ok())
        {
            ips.push(ip);
        }
        if !ips.iter().any(|ip| canonical(*ip) == peer) {
            return Err(refuse("the request does not come from the pod's address"));
        }
        let (rec, rv, rec_uid) = match self.record_with_uid(id).await {
            Ok(r) => r,
            Err(RuntimeError::NotFound { .. }) => return Err(refuse("the instance has no record")),
            Err(_) => return Err(not_ready("the record cannot be read")),
        };
        if rec.instance != id {
            return Err(refuse("the record is another instance's"));
        }
        if rec.sealed.is_none() {
            return Err(not_ready("the record is not sealed yet"));
        }
        let s = self
            .unsealed(&rec, &rec_uid)
            .ok_or_else(|| refuse("the seal does not open or does not match the record"))?;
        if !self.fresh_gen(id, s.generation) {
            return Err(refuse("an older seal was put back"));
        }
        if s.pod_uid.as_deref() != Some(pod_uid) {
            // The record's run counter names this pod's run, whose seal comes right after the
            // pod: wait. (A writer of the record can make that so for a pod of its own; it then
            // waits for nothing.)
            let pending = rec.run == run && (s.run < run || s.pod_uid.is_none());
            return Err(if pending {
                not_ready("the pod's run is not sealed yet")
            } else {
                refuse("the seal names another pod")
            });
        }
        if s.run != run {
            return Err(refuse("the seal names another run"));
        }
        if !matches!(rec.state, InstanceState::Starting | InstanceState::Running) {
            return Err(refuse("the instance does not run"));
        }
        let mut sealed_rec = rec.clone();
        sealed_rec.run = s.run;
        let want = self
            .expected_pod(&sealed_rec)
            .await
            .map_err(|_| not_ready("the expected pod cannot be made"))?;
        if let Err(what) = pod::verify(&self.cfg, &want, &pod) {
            tracing::warn!(target: "ruum::runtime", pod = pod_n, differs = %what, "a supervisor request was refused: the pod is not the driver's");
            return Err(LaunchRefusal::Forbidden);
        }
        Ok((s, rec, rv, rec_uid))
    }

    /// The supervisor listener (`launch`: `/v1/launch`, else `/v1/token`).
    async fn supervisor(
        &self,
        bearer: &str,
        peer: IpAddr,
        launch: bool,
    ) -> Result<Vec<u8>, LaunchRefusal> {
        let peer = canonical(peer);
        if !self.peer_ok(peer) {
            tracing::warn!(target: "ruum::runtime", %peer, "too many supervisor requests from one address");
            return Err(LaunchRefusal::TooMany);
        }
        let (pod_n, pod_uid) = self.review(bearer).await?;
        for _ in 0..SEAL_TRIES {
            let (s, mut rec, rv, rec_uid) = self.authorize(&pod_n, &pod_uid, peer).await?;
            let launched = s.launched.contains(&pod_uid);
            if !launch {
                if !launched {
                    tracing::warn!(target: "ruum::runtime", pod = %pod_n, "a token request before the launch was refused");
                    return Err(LaunchRefusal::Forbidden);
                }
                return serde_json::to_vec(&TokenAnswer {
                    v: LAUNCH_VERSION,
                    token: s.token,
                })
                .map_err(|_| LaunchRefusal::NotReady);
            }
            if launched {
                tracing::warn!(target: "ruum::runtime", pod = %pod_n, "a second launch of a pod was refused");
                return Err(LaunchRefusal::Forbidden);
            }
            // Once per pod: the pod's uid is sealed into the launched list before the answer.
            let mut next = s.clone();
            next.launched.push(pod_uid.clone());
            while next.launched.len() > MAX_LAUNCHED {
                next.launched.remove(0);
            }
            next.generation += 1;
            let Ok(text) = seal::seal(self.key(), &object_name(rec.instance), &rec_uid, &next)
            else {
                return Err(LaunchRefusal::NotReady);
            };
            rec.sealed = Some(text);
            match self.put(&rec, &rv).await {
                Ok(true) => {
                    self.fresh_gen(rec.instance, next.generation);
                    let answer = Launch {
                        v: LAUNCH_VERSION,
                        argv: s.argv,
                        env: s.env,
                        redact: s.previous,
                        token: Some(s.token),
                        pids_limit: self.cfg.pids_limit,
                        token_poll_secs: Some(TOKEN_POLL_SECS),
                    };
                    return serde_json::to_vec(&answer).map_err(|_| LaunchRefusal::NotReady);
                }
                Ok(false) => continue,
                Err(_) => return Err(LaunchRefusal::NotReady),
            }
        }
        Err(LaunchRefusal::NotReady)
    }

    /// Forgets the logs of ended instances past the retention; then, while more than
    /// `max_ended_instances` are kept or the logs of ended and scaled-to-zero instances take more
    /// than `max_ended_log_bytes`, the oldest of the tenant with the largest share (bytes when
    /// the byte cap is over, else ended instances): an ended one is forgotten, one scaled to zero
    /// loses its log lines only (it keeps its values for a wake and a token rotation). With
    /// `room` (a tenant that starts an instance): `unavailable` when the tenant keeps
    /// `max_tenant_instances` that are not ended (Kimi run 15, I6-1 finding 5); with
    /// `max_instances` kept, the oldest ended instance of the tenant that keeps the most (the new
    /// one counted for the starter), else one of the starter's own, else `unavailable`. So one
    /// tenant's load evicts its own ended instances first (as the Docker driver, S-61, S-67), and
    /// no tenant takes every slot.
    ///
    /// With `room`, the new instance's `Local` is added under the same lock when there is room,
    /// so two starts at once cannot both pass the caps (Kimi review of this fix).
    fn trim_locals(&self, room: Option<&Arc<Local>>) -> Result<(), RuntimeError> {
        let mut map = self.locals.lock().expect("locals lock");
        let r = self.trim_in(&mut map, room.map(|l| l.identity.tenant));
        if r.is_ok()
            && let Some(l) = room
        {
            map.insert(l.id, l.clone());
        }
        r
    }

    fn trim_in(
        &self,
        map: &mut HashMap<InstanceId, Arc<Local>>,
        room: Option<TenantId>,
    ) -> Result<(), RuntimeError> {
        type Group = (usize, Vec<(Instant, InstanceId)>);
        let retention = self.cfg.ended_retention;
        map.retain(|_, l| {
            l.st.lock()
                .expect("local lock")
                .ended_at
                .is_none_or(|t| t.elapsed() < retention)
        });
        if let Some(starter) = room {
            let live = map
                .values()
                .filter(|l| {
                    l.identity.tenant == starter
                        && l.st.lock().expect("local lock").ended_at.is_none()
                })
                .count();
            if live >= self.cfg.max_tenant_instances {
                return Err(RuntimeError::Unavailable {
                    reason: format!(
                        "the tenant keeps its maximum of {} instances on this server process",
                        self.cfg.max_tenant_instances
                    ),
                });
            }
        }
        loop {
            let mut ended: HashMap<TenantId, Group> = HashMap::new();
            let mut parked: HashMap<TenantId, Group> = HashMap::new();
            let mut totals: HashMap<TenantId, usize> = HashMap::new();
            if let Some(t) = room {
                totals.insert(t, 0);
            }
            for (id, l) in map.iter() {
                *totals.entry(l.identity.tenant).or_default() += 1;
                let st = l.st.lock().expect("local lock");
                if let Some(at) = st.ended_at {
                    let e = ended.entry(l.identity.tenant).or_default();
                    e.0 += st.ring.memory();
                    e.1.push((at, *id));
                } else if let Some(at) = st.parked_at
                    && st.ring.memory() > 0
                {
                    let e = parked.entry(l.identity.tenant).or_default();
                    e.0 += st.ring.memory();
                    e.1.push((at, *id));
                }
            }
            let count: usize = ended.values().map(|e| e.1.len()).sum();
            let bytes: usize = ended.values().chain(parked.values()).map(|e| e.0).sum();
            let oldest = |e: &Group| e.1.iter().map(|(at, _)| std::cmp::Reverse(*at)).max();
            let bytes_of =
                |t: &TenantId| ended.get(t).map_or(0, |e| e.0) + parked.get(t).map_or(0, |e| e.0);
            if bytes > self.cfg.max_ended_log_bytes {
                let Some(t) = ended
                    .keys()
                    .chain(parked.keys())
                    .max_by_key(|t| bytes_of(t))
                    .copied()
                else {
                    return Ok(());
                };
                let first = |g: Option<&Group>| {
                    g.and_then(|e| e.1.iter().min_by_key(|(at, _)| *at).copied())
                };
                let (e, p) = (first(ended.get(&t)), first(parked.get(&t)));
                match (e, p) {
                    (Some((ea, eid)), Some((pa, _))) if ea <= pa => {
                        Self::forget(map, eid);
                    }
                    (_, Some((_, pid))) => {
                        if let Some(l) = map.get(&pid) {
                            l.st.lock().expect("local lock").ring.clear();
                        }
                    }
                    (Some((_, eid)), None) => Self::forget(map, eid),
                    (None, None) => return Ok(()),
                }
                continue;
            }
            let victim = if count > self.cfg.max_ended_instances {
                ended
                    .iter()
                    .max_by_key(|(_, e)| (e.1.len(), oldest(e)))
                    .map(|(t, _)| *t)
            } else if let Some(starter) = room
                && map.len() >= self.cfg.max_instances
            {
                let top = totals
                    .iter()
                    .max_by_key(|(t, n)| {
                        (
                            **n + usize::from(**t == starter),
                            ended.contains_key(*t),
                            ended.get(*t).and_then(oldest),
                        )
                    })
                    .map(|(t, _)| *t);
                match top {
                    Some(t) if ended.contains_key(&t) => Some(t),
                    _ if ended.contains_key(&starter) => Some(starter),
                    _ => {
                        return Err(RuntimeError::Unavailable {
                            reason: format!(
                                "the driver keeps its maximum of {} instances",
                                self.cfg.max_instances
                            ),
                        });
                    }
                }
            } else {
                return Ok(());
            };
            let Some(id) = victim
                .and_then(|t| ended.get(&t))
                .and_then(|e| e.1.iter().min_by_key(|(at, _)| *at).map(|(_, id)| *id))
            else {
                return Ok(());
            };
            Self::forget(map, id);
        }
    }

    /// Forgets the logs of one instance (the caller holds the map).
    fn forget(map: &mut HashMap<InstanceId, Arc<Local>>, id: InstanceId) {
        if let Some(l) = map.remove(&id) {
            for t in l.st.lock().expect("local lock").followers.drain(..) {
                t.abort();
            }
        }
    }

    /// Whether a pod that the sweep's first list did not name is really left over. The record is
    /// read again: a start or a wake since that list made both after it. A pod is left over when
    /// its record is gone, or names another run or a state without a pod (an exited pod stays
    /// `EXITED_POD_KEEP`). An error keeps the pod (the next sweep looks again).
    async fn orphan(&self, pod: &str) -> bool {
        let Some((object, run)) = pod.rsplit_once('-') else {
            return false;
        };
        let Ok(run) = run.parse::<u32>() else {
            return false;
        };
        let rec = match self.api.get(Kind::ConfigMap, object).await {
            Ok(Some(cm)) => match pod::read_record(&cm) {
                Some((rec, _)) => rec,
                None => return false,
            },
            Ok(None) => return true,
            Err(_) => return false,
        };
        if rec.run != run || rec.registered.as_deref() == Some(pod) {
            return rec.run != run && rec.registered.as_deref() != Some(pod);
        }
        match rec.state {
            InstanceState::Starting | InstanceState::Running => false,
            InstanceState::Exited(_) => rec
                .ended_ms
                .is_none_or(|t| pod::now_ms() - t >= ms(EXITED_POD_KEEP)),
            _ => true,
        }
    }

    async fn reregister(&self, rec: &Record) {
        let Some(pod) = rec.registered.clone() else {
            return;
        };
        let due = self
            .last_register
            .lock()
            .expect("register lock")
            .get(&pod)
            .is_none_or(|t| t.elapsed() >= self.cfg.reregister_every);
        if !due {
            return;
        }
        // A pod being deleted (`Terminating`) is not registered again.
        let ip = match self.api.get(Kind::Pod, &pod).await {
            Ok(Some(p)) => match pod::view(&p) {
                PodView::Running { ip: Some(ip), .. } => ip,
                _ => return,
            },
            _ => return,
        };
        if let Err(e) = self.register(rec, &pod, &ip).await {
            tracing::warn!(target: "ruum::runtime", code = e.code(), "registering a sandbox at the egress proxy again failed");
            return;
        }
        // The list of this sweep may be old: a stop since then may have unregistered the pod
        // before this registration. Then it goes again.
        if let Err(e) = self.confirm(rec, &pod).await {
            tracing::debug!(target: "ruum::runtime", code = e.code(), "a registration could not be confirmed; it was taken back");
        }
    }

    fn drop_local(&self, id: InstanceId) {
        if let Some(l) = self.locals.lock().expect("locals lock").remove(&id) {
            for t in l.st.lock().expect("local lock").followers.drain(..) {
                t.abort();
            }
        }
        self.addrs.lock().expect("addrs lock").remove(&id);
    }
}

async fn sweep_loop(inner: Weak<Inner>) {
    loop {
        let every = match inner.upgrade() {
            Some(i) => i.cfg.sweep_every,
            None => return,
        };
        tokio::time::sleep(every).await;
        let Some(i) = inner.upgrade() else { return };
        i.sweep().await;
    }
}

/// Follows one stream of a run's log into the ring, through the run's redactor. Ends when the
/// container ended and its log is read, or the pod is gone.
async fn follow_log(
    inner: Arc<Inner>,
    local: Arc<Local>,
    redactors: Arc<Mutex<Redactors>>,
    pod: String,
    query: Option<&'static str>,
    stream: LogStream,
) {
    let id = local.id;
    let mut reconnect = false;
    let mut ended_once = false;
    loop {
        match inner.api.follow_log(&pod, query, reconnect).await {
            Ok(LogAnswer::Stream(r)) => {
                let mut body = r.bytes_stream();
                while let Some(chunk) = body.next().await {
                    let Ok(bytes) = chunk else { break };
                    let now = OffsetDateTime::now_utc();
                    let lines = {
                        let mut red = redactors.lock().unwrap_or_else(|p| p.into_inner());
                        match stream {
                            LogStream::Stdout => red.out.push_bytes(&bytes, now),
                            LogStream::Stderr => red.err.push_bytes(&bytes, now),
                        }
                    };
                    push_lines(&local, id, stream, lines);
                }
                match inner.api.get(Kind::Pod, &pod).await {
                    Ok(Some(p)) if matches!(pod::view(&p), PodView::Running { .. }) => {
                        // The stream broke while the container runs: go on from now (lines of
                        // the gap are lost, none is read twice).
                        reconnect = true;
                    }
                    Ok(_) => break,
                    Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
                }
            }
            Ok(LogAnswer::NotReady) => match inner.api.get(Kind::Pod, &pod).await {
                Ok(Some(p)) if !matches!(pod::view(&p), PodView::Ended(_)) => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                // The container ran and ended between the log request and this look (a short
                // job): its log is still there, so ask once more before giving up.
                Ok(Some(_)) if !ended_once => ended_once = true,
                Ok(_) => break,
                Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
            },
            Ok(LogAnswer::Gone) => break,
            Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    }
    let now = OffsetDateTime::now_utc();
    let rest = {
        let mut red = redactors.lock().unwrap_or_else(|p| p.into_inner());
        match stream {
            LogStream::Stdout => red.out.finish(now),
            LogStream::Stderr => red.err.finish(now),
        }
    };
    push_lines(&local, id, stream, rest);
}

fn push_lines(
    local: &Local,
    id: InstanceId,
    stream: LogStream,
    lines: Vec<(OffsetDateTime, String)>,
) {
    if lines.is_empty() {
        return;
    }
    let mut st = local.st.lock().expect("local lock");
    for (at, text) in lines {
        st.ring.push(id, local.identity, stream, at, text);
    }
}

#[async_trait]
impl RuntimeDriver for KubeDriver {
    fn name(&self) -> &str {
        "kubernetes"
    }

    async fn start(&self, spec: WorkloadSpec) -> Result<InstanceStatus, RuntimeError> {
        spec.validate()?;
        let inner = &self.inner;
        inner.check_spec(&spec)?;
        let id = InstanceId::new();
        let digest = match &spec.image {
            ImageRef::Static(_) => {
                // No code runs: the gateway serves the files. Nothing in the API.
                let st = StaticInst {
                    identity: spec.identity,
                    kind: spec.kind.name(),
                    limits: spec.limits,
                    started_at: OffsetDateTime::now_utc(),
                    state: InstanceState::Running,
                };
                let status = static_status(id, &st);
                inner.statics.lock().expect("statics lock").insert(id, st);
                return Ok(status);
            }
            ImageRef::Oci(d) => d.clone(),
            other => {
                return Err(RuntimeError::UnsupportedImage {
                    reason: format!("the Kubernetes driver does not run {} images", other.kind()),
                });
            }
        };
        let image = inner.image(&spec.identity, &digest).await?;
        // The supervisor's launch data (I6-20): the command as the runtime would run it.
        let (entrypoint, cmd) = inner.image_command(&spec.identity, &digest).await?;
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
        let rec = Record::new(&spec, id, image);
        let name = object_name(id);
        let token = spec.workload_token.expose().to_owned();
        let fresh = Sealed {
            generation: 0,
            digest: pod::fixed_digest(&rec),
            argv,
            env: pod::secret_env(&spec),
            previous: spec
                .previous_secrets
                .iter()
                .map(|v| v.expose().to_owned())
                .filter(|v| !v.is_empty())
                .collect(),
            token: token.clone(),
            run: rec.run,
            pod_uid: None,
            launched: Vec::new(),
        };
        // The launch data must pass the supervisor's own checks.
        Launch {
            v: LAUNCH_VERSION,
            argv: fresh.argv.clone(),
            env: fresh.env.clone(),
            redact: fresh.previous.clone(),
            token: Some(token.clone()),
            pids_limit: inner.cfg.pids_limit,
            token_poll_secs: Some(TOKEN_POLL_SECS),
        }
        .check()
        .map_err(|why| RuntimeError::invalid("spec", why))?;
        let base: Vec<String> = spec
            .redacted_values()
            .into_iter()
            .filter(|v| *v != token)
            .map(str::to_owned)
            .collect();
        let local = Arc::new(Local {
            id,
            identity: spec.identity,
            made: Instant::now(),
            rotating: tokio::sync::Mutex::new(()),
            redactors: Mutex::new(None),
            st: Mutex::new(LocalSt {
                ring: Ring::new(inner.cfg.log_lines, MAX_LOG_BYTES, inner.cfg.max_line_bytes)
                    .with_budget(inner.live.clone(), spec.identity.tenant),
                base,
                tokens: VecDeque::from([token]),
                followed_run: 0,
                followers: Vec::new(),
                touched_ms: rec.last_touch_ms,
                ended_at: None,
                parked_at: None,
            }),
        });
        // Room for its logs in this process, before anything is made (the Local is added).
        inner.trim_locals(Some(&local))?;
        // The record first (unsealed): a crash after it leaves a record that the sweep ends and
        // removes. The pod comes next, then the seal that names the pod (`begin_run`).
        let made = inner
            .api
            .create(Kind::ConfigMap, &pod::record_body(&inner.cfg, &rec, None))
            .await;
        match made {
            Ok(Write::Done(_)) => {}
            other => {
                inner.locals.lock().expect("locals lock").remove(&id);
                return Err(other.err().unwrap_or_else(|| RuntimeError::Internal {
                    reason: format!("the instance record {name} exists already"),
                }));
            }
        }
        if let Err(e) = inner.begin_run(&rec, Some(&fresh)).await {
            let _ = inner.api.delete(Kind::Pod, &pod_name(id, 1), Some(0)).await;
            let _ = inner
                .api
                .delete(Kind::ConfigMap, &object_name(id), None)
                .await;
            inner.locals.lock().expect("locals lock").remove(&id);
            return Err(e);
        }
        Ok(inner.status_of(&rec, Health::Unknown))
    }

    async fn status(
        &self,
        tenant: TenantId,
        instance: InstanceId,
    ) -> Result<InstanceStatus, RuntimeError> {
        if let Some(s) = self
            .inner
            .statics
            .lock()
            .expect("statics lock")
            .get(&instance)
        {
            return if s.identity.tenant == tenant {
                Ok(static_status(instance, s))
            } else {
                Err(not_found(instance))
            };
        }
        let cm = self
            .inner
            .api
            .get(Kind::ConfigMap, &object_name(instance))
            .await?
            .ok_or_else(|| not_found(instance))?;
        match pod::read_record(&cm) {
            Some((rec, _)) if rec.identity.tenant == tenant && rec.instance == instance => {}
            _ => return Err(not_found(instance)),
        }
        self.inner.observe(cm).await
    }

    async fn stop(&self, tenant: TenantId, instance: InstanceId) -> Result<(), RuntimeError> {
        {
            let mut statics = self.inner.statics.lock().expect("statics lock");
            if let Some(s) = statics.get(&instance) {
                if s.identity.tenant != tenant {
                    return Err(not_found(instance));
                }
                statics.remove(&instance);
                return Ok(());
            }
        }
        let inner = &self.inner;
        for _ in 0..4 {
            let (rec, rv) = inner.record(tenant, instance).await?;
            match rec.state {
                InstanceState::Stopped | InstanceState::Exited(_) => return Ok(()),
                _ => {}
            }
            // No output after the stop: the followers end now.
            if let Some(l) = inner.local(instance) {
                for t in l.st.lock().expect("local lock").followers.drain(..) {
                    t.abort();
                }
            }
            if inner
                .end_run(rec, &rv, InstanceState::Stopped)
                .await?
                .is_some()
            {
                return Ok(());
            }
        }
        Err(RuntimeError::Unavailable {
            reason: "the instance record changes too often".to_owned(),
        })
    }

    async fn touch(&self, tenant: TenantId, instance: InstanceId) -> Result<(), RuntimeError> {
        if let Some(s) = self
            .inner
            .statics
            .lock()
            .expect("statics lock")
            .get(&instance)
        {
            return if s.identity.tenant == tenant {
                Ok(())
            } else {
                Err(not_found(instance))
            };
        }
        let inner = &self.inner;
        let (mut rec, rv) = inner.record(tenant, instance).await?;
        if !matches!(rec.kind, KindRec::Service { .. }) {
            return Err(state_err("touch", &rec.state));
        }
        match rec.state {
            InstanceState::Running | InstanceState::Starting => {}
            InstanceState::ScaledToZero => return Ok(()),
            s => return Err(state_err("touch", &s)),
        }
        let now = pod::now_ms();
        if let Some(l) = inner.local(instance) {
            l.st.lock().expect("local lock").touched_ms = now;
        }
        // The record holds the touch for the other processes, written at most every quarter of
        // the idle timeout (so their idle check never runs ahead of a visited service).
        if let KindRec::Service {
            idle_timeout_ms: Some(idle),
            ..
        } = rec.kind
        {
            let every = (idle as i64 / 4).clamp(1, 30_000);
            if now - rec.last_touch_ms >= every {
                rec.last_touch_ms = now;
                let _ = inner.put(&rec, &rv).await;
            }
        }
        Ok(())
    }

    async fn wake(
        &self,
        tenant: TenantId,
        instance: InstanceId,
    ) -> Result<InstanceStatus, RuntimeError> {
        if let Some(s) = self
            .inner
            .statics
            .lock()
            .expect("statics lock")
            .get(&instance)
        {
            return if s.identity.tenant == tenant {
                Err(state_err("wake", &s.state))
            } else {
                Err(not_found(instance))
            };
        }
        let inner = &self.inner;
        for _ in 0..4 {
            let (mut rec, rv) = inner.record(tenant, instance).await?;
            if !matches!(rec.kind, KindRec::Service { .. }) {
                return Err(state_err("wake", &rec.state));
            }
            match rec.state {
                InstanceState::Running | InstanceState::Starting => {
                    self.touch(tenant, instance).await?;
                    // Observed, not the bare record: another process may have woken a new run,
                    // and the gateway's address must be this run's.
                    return self.status(tenant, instance).await;
                }
                InstanceState::ScaledToZero => {}
                s => return Err(state_err("wake", &s)),
            }
            // The previous run's registration goes first: a new run would forget it, and its
            // pod would be deleted while the proxy still names this app for its address.
            if let Some(reg) = rec.registered.clone() {
                if !inner.unregister(&reg).await {
                    return Err(RuntimeError::Unavailable {
                        reason: "the previous run's egress registration is not removed yet"
                            .to_owned(),
                    });
                }
                rec.registered = None;
            }
            let now = pod::now_ms();
            rec.state = InstanceState::Starting;
            rec.run += 1;
            rec.run_started_ms = now;
            rec.last_touch_ms = now;
            rec.registered = None;
            if !inner.put(&rec, &rv).await? {
                continue;
            }
            if let Some(l) = inner.local(instance) {
                l.st.lock().expect("local lock").touched_ms = now;
            }
            if let Err(e) = inner.begin_run(&rec, None).await {
                // The run did not start: the instance ended (a wake does not leave a record
                // that says starting with no pod; the sweep would end it anyway).
                if let Ok((r, rv)) = inner.record(tenant, instance).await {
                    let _ = inner
                        .end_run(
                            r,
                            &rv,
                            InstanceState::Exited(ExitStatus::of(ExitReason::Killed)),
                        )
                        .await;
                }
                return Err(e);
            }
            return Ok(inner.status_of(&rec, Health::Unknown));
        }
        Err(RuntimeError::Unavailable {
            reason: "the instance record changes too often".to_owned(),
        })
    }

    async fn logs(
        &self,
        tenant: TenantId,
        instance: InstanceId,
        after: Option<u64>,
        limit: usize,
    ) -> Result<Vec<LogLine>, RuntimeError> {
        if let Some(s) = self
            .inner
            .statics
            .lock()
            .expect("statics lock")
            .get(&instance)
        {
            return if s.identity.tenant == tenant {
                Ok(Vec::new())
            } else {
                Err(not_found(instance))
            };
        }
        let local = self.inner.local(instance);
        match local {
            Some(l) if l.identity.tenant == tenant => {
                let lines = l.st.lock().expect("local lock").ring.after(after, limit);
                Ok(lines)
            }
            Some(_) => Err(not_found(instance)),
            None => {
                // Another process started it (or this one restarted): only that process knows
                // the values to redact, so this one reads no log of it.
                self.inner.record(tenant, instance).await?;
                Err(RuntimeError::Unavailable {
                    reason: "the logs of this instance are with the server process that started it"
                        .to_owned(),
                })
            }
        }
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
        if let Some(s) = self
            .inner
            .statics
            .lock()
            .expect("statics lock")
            .get(&instance)
        {
            return if s.identity.tenant == tenant {
                Err(state_err("rotate the token of", &s.state))
            } else {
                Err(not_found(instance))
            };
        }
        let inner = &self.inner;
        let (rec, _) = inner.record(tenant, instance).await?;
        if !matches!(
            rec.state,
            InstanceState::Starting | InstanceState::Running | InstanceState::ScaledToZero
        ) {
            return Err(state_err("rotate the token of", &rec.state));
        }
        // Redact the new value before the process can read it. Only the process that follows the
        // log can (it holds the values): another one refuses, so a new token never reaches a
        // log that does not redact it (fails closed; the renewal tries again). The supervisor
        // redacts it in the sandbox too, before it writes the token file.
        let Some(l) = inner.local(instance) else {
            return Err(RuntimeError::Unavailable {
                reason:
                    "the token of this instance is rotated by the server process that started it"
                        .to_owned(),
            });
        };
        let _one = l.rotating.lock().await;
        {
            let all = {
                let mut st = l.st.lock().expect("local lock");
                st.tokens.push_back(token.expose().to_owned());
                while st.tokens.len() > MAX_KEPT_TOKENS {
                    st.tokens.pop_front();
                }
                st.values()
            };
            if let Some(r) = l.redactors.lock().expect("redactors lock").as_ref() {
                r.lock().unwrap_or_else(|p| p.into_inner()).set(&all);
            }
        }
        // The new token into the seal; the supervisor fetches it at its next token request.
        let new = token.expose().to_owned();
        inner
            .reseal(instance, None, |now, s| {
                if !matches!(
                    now.state,
                    InstanceState::Starting | InstanceState::Running | InstanceState::ScaledToZero
                ) {
                    return false;
                }
                s.token = new.clone();
                true
            })
            .await
    }
}

/// An address in one form: an IPv4-mapped IPv6 address as IPv4.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

/// The `exp` claim of a JWT (seconds), read without checking the signature: the `TokenReview`
/// checked the token; this only bounds how long its answer is kept.
fn jwt_exp(token: &str) -> Option<i64> {
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice::<Value>(&raw)
        .ok()?
        .get("exp")?
        .as_i64()
}

fn static_status(id: InstanceId, s: &StaticInst) -> InstanceStatus {
    InstanceStatus {
        id,
        identity: s.identity,
        kind: s.kind,
        state: s.state,
        health: Health::Healthy,
        applied: Applied {
            routes: Vec::new(),
            network: None,
            limits: s.limits,
        },
        started_at: s.started_at,
    }
}
