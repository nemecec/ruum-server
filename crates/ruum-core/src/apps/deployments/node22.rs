//! node22 deployments (card I6-13; backlog I6-13, P-120 "W-2n"; milestone M2).
//!
//! A server runtime is off until the server calls [`AppService::set_node22`] with a
//! [`Builder`] (the Kubernetes builder on staging, `ruum_runtime::builder::kube`). Without it a
//! `runtime: node22` manifest stays `409 runtime_not_ready` before the answer, as before.
//!
//! With it, a node22 preview runs these stages after the `202` (`AppDeployment.stages`):
//!
//! - `bindings`: as for a static app (card B-1).
//! - `build`: the archive as uploaded goes to [`Builder::build`] with the build limits and the
//!   timeout of [`Node22Settings`]. **No secret and no binding reaches a build** ([`BuildInput`]
//!   cannot carry them). A failed build is `build_failed` with the end of the build log (the
//!   builder bounds it); a build that runs too long is `build_timed_out`.
//! - `test`: `skipped` (no test command in step 3).
//! - `image`: the digest of the pushed image is stored once on the deployment
//!   ([`super::super::store::AppStore::set_image_digest`]). The image is in the repository of
//!   the deployment's tenant and app only, and every start runs it by this digest.
//! - `start`: the instance is started through the runtime driver with the spec of
//!   [`workload_spec`], and must be `running` and `healthy` (the manifest's `health.path`, a
//!   probe **inside** the sandbox, decision 0006 G point 1) within the manifest's
//!   `health.timeout_seconds` (default 30 s) and at most [`Node22Settings::start_wait`]. It is
//!   stopped again: the sandbox directory starts it on the first request (scale to zero).
//! - `check`: the result of the start (one route, `health.path`); the browser check is O-4.
//!
//! A publish (or a rollback) of a node22 deployment copies its image digest to the new live
//! deployment and starts the image once as the live environment before the switch (healthy
//! within the same wait, but at most [`Node22Settings::publish_start_wait`], 60 s: it runs
//! inside the request), unless the
//! manifest has bindings: the live credentials of a binding exist only after the switch, and
//! the preview already passed the start with the same image and manifest (backlog I613-3).
//!
//! The spec ([`workload_spec`]): the image by digest, the image's own command (the builder
//! sets `/bin/sh -c <start>`), `PORT=8080` and the manifest's plain `env`, the binding
//! environment of [`AppService::binding_env_for_start`] (card B-1d; a database URL is an
//! [`EnvValue::Secret`] and adds the pooler route), the workload token (signed when the server
//! has a workload key, else a random value that the runtime API refuses), the egress proxy as
//! the only other route, the memory of `resources.memory_mb` (default the smaller of 256 MB and
//! the plan's), CPU in proportion (a quarter core per 256 MB) and the disk of the settings,
//! scale to zero after [`Node22Settings::idle_timeout`] (always-on instances are refused).
//!
//! Fail closed: parts of a node22 manifest whose cards are not built refuse the deployment
//! before the answer (`409 runtime_not_ready` with the pointer): `static` (the gateway does not
//! serve static files next to a server yet), `jobs` (R-5), `events` (R-6), `apis` (B-3) and
//! `egress.allow` (no runtime path narrows the egress proxy's allow list yet: the app would get
//! more than it asked for; also an empty list) and `resources.min_instances` above zero (an
//! always-on instance would outlive a retired deployment until a stop reaches it).

use super::super::AppService;
use super::super::plan::AppPlanLimits;
use super::super::store::DeploymentRow;
use super::{StageError, not_ready};
use crate::error::{ErrorCode, Result};
use crate::ids::TenantId;
use crate::runtime::{
    BuildInput, Builder, EnvValue, EnvVar, Environment, HealthCheck, ImageRef, InstanceState,
    Limits, OciDigest, Route, RuntimeDriver, RuntimeError, SecretValue, ServiceSpec,
    WorkloadIdentity, WorkloadKind, WorkloadSpec,
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

/// The runtime name of a node22 manifest.
pub const RUNTIME: &str = "node22";

/// The port a node22 server listens on (`$PORT`).
pub const PORT: u16 = 8080;

/// The memory of an instance when the manifest names none (at most the plan's).
pub const DEFAULT_MEMORY_MB: u32 = 256;

/// The health check's timeout when the manifest names none (`health.timeout_seconds`).
pub const DEFAULT_HEALTH_TIMEOUT: Duration = Duration::from_secs(30);

/// The settings of node22 builds and instances.
#[derive(Clone, Debug)]
pub struct Node22Settings {
    /// A build that runs longer fails (`build_timed_out`).
    pub build_timeout: Duration,
    /// The limits of a build (the builder's sandbox).
    pub build_limits: Limits,
    /// The longest the stage `start` waits for a healthy instance.
    pub start_wait: Duration,
    /// The longest the start before a publish (or a rollback) waits for a healthy instance,
    /// counted from the start request. It runs inside the request, so it stays well below the
    /// request timeout (`RUUM_REQUEST_TIMEOUT_SECONDS`, default 120 s; review I6-13, 3).
    pub publish_start_wait: Duration,
    /// An instance without a request for this long scales to zero.
    pub idle_timeout: Duration,
    /// The writable disk of an instance (`/tmp`).
    pub disk_bytes: u64,
    /// How often the health check runs.
    pub health_interval: Duration,
}

impl Default for Node22Settings {
    fn default() -> Self {
        Self {
            build_timeout: Duration::from_secs(10 * 60),
            build_limits: Limits {
                cpu_millis: 1000,
                memory_bytes: 1024 * 1024 * 1024,
                disk_bytes: 2 * 1024 * 1024 * 1024,
            },
            start_wait: Duration::from_secs(120),
            publish_start_wait: Duration::from_secs(60),
            idle_timeout: Duration::from_secs(15 * 60),
            disk_bytes: 256 * 1024 * 1024,
            health_interval: Duration::from_secs(5),
        }
    }
}

/// What turns node22 deployments on ([`AppService::set_node22`]).
pub struct Node22 {
    pub builder: Arc<dyn Builder>,
    pub settings: Node22Settings,
    /// Signs the workload tokens (`Services::workload_tokens`); `None`: a random token.
    pub tokens: Option<Arc<crate::apps::workload_tokens::WorkloadTokenIssuer>>,
}

/// How long a start check waits for a healthy instance, counted from the start request: the
/// manifest's health timeout and 30 s, at most [`Node22Settings::start_wait`] for the stage
/// `start` of a preview, and at most [`Node22Settings::publish_start_wait`] too for the start
/// before a publish, which runs inside the request (review I6-13, 3).
pub fn start_check_wait(manifest: &Value, settings: &Node22Settings, publish: bool) -> Duration {
    let wait = health_timeout(manifest)
        .saturating_add(Duration::from_secs(30))
        .min(settings.start_wait);
    if publish {
        wait.min(settings.publish_start_wait)
    } else {
        wait
    }
}

/// Whether a manifest is a node22 server.
pub fn is_server(m: &Value) -> bool {
    m.get("runtime").and_then(Value::as_str) == Some(RUNTIME)
}

/// The parts of a node22 manifest whose cards are not built yet (see the module docs).
pub(crate) fn refuse_unbuilt(m: &Value) -> Result<()> {
    let present = |k: &str| match m.get(k) {
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Null) | None => false,
        Some(_) => true,
    };
    for (key, what) in [
        ("static", "static files next to a node22 server"),
        ("jobs", "scheduled jobs"),
        ("events", "events"),
        ("apis", "app APIs"),
    ] {
        if present(key) {
            return Err(not_ready(what, "runtime_not_ready", &format!("/{key}")));
        }
    }
    // Any allow list, also an empty one (which asks for no egress at all): no runtime path
    // carries it to the proxy yet, so the app would get more than it asked for (Kimi I6-13, 2).
    if m.pointer("/egress/allow").is_some() {
        return Err(not_ready(
            "egress allow lists",
            "runtime_not_ready",
            "/egress/allow",
        ));
    }
    // Always on: an instance that never scales to zero would outlive its deployment until a
    // stop reaches it (Kimi I6-13, 1); the sandbox directory's sweep and the idle timeout bound
    // a retired instance only when it scales to zero.
    if m.pointer("/resources/min_instances")
        .and_then(Value::as_u64)
        .is_some_and(|n| n > 0)
    {
        return Err(not_ready(
            "always-on instances (resources.min_instances)",
            "runtime_not_ready",
            "/resources/min_instances",
        ));
    }
    Ok(())
}

/// The memory of an instance in MB: `resources.memory_mb`, else the smaller of
/// [`DEFAULT_MEMORY_MB`] and the plan's (the validation already capped the manifest's value).
pub fn memory_mb(m: &Value, plan: &AppPlanLimits) -> u32 {
    m.pointer("/resources/memory_mb")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(DEFAULT_MEMORY_MB.min(plan.memory_mb))
        .min(plan.memory_mb)
        .max(64)
}

/// The limits of an instance (see the module docs).
pub fn instance_limits(m: &Value, plan: &AppPlanLimits, settings: &Node22Settings) -> Limits {
    let mb = memory_mb(m, plan);
    Limits {
        cpu_millis: (mb * 1000 / 1024).clamp(125, 1000),
        memory_bytes: u64::from(mb) * 1024 * 1024,
        disk_bytes: settings.disk_bytes,
    }
}

/// The health check's timeout of a manifest (`health.timeout_seconds`, default 30 s).
pub fn health_timeout(m: &Value) -> Duration {
    m.pointer("/health/timeout_seconds")
        .and_then(Value::as_u64)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_HEALTH_TIMEOUT)
}

/// The health check of a manifest: `health.path` (default `/`), failing after its timeout.
pub fn health_check(m: &Value, settings: &Node22Settings) -> HealthCheck {
    let path = m
        .pointer("/health/path")
        .and_then(Value::as_str)
        .unwrap_or("/")
        .to_owned();
    let interval = settings.health_interval.max(Duration::from_secs(1));
    let failures = health_timeout(m)
        .as_secs()
        .div_ceil(interval.as_secs())
        .max(1);
    HealthCheck {
        path,
        interval,
        failure_threshold: u32::try_from(failures).unwrap_or(u32::MAX),
    }
}

/// The plain environment of a manifest: `PORT` and the manifest's `env` (the schema refuses
/// `PORT` and the `RUUM_` prefix there), sorted by name.
pub fn plain_env(m: &Value) -> Vec<EnvVar> {
    let mut env = vec![EnvVar::plain("PORT", PORT.to_string())];
    if let Some(o) = m.get("env").and_then(Value::as_object) {
        let mut names: Vec<&String> = o.keys().collect();
        names.sort();
        for k in names {
            let v = match &o[k] {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            env.push(EnvVar::plain(k.clone(), v));
        }
    }
    env
}

/// The spec of a node22 instance (see the module docs). `extra` is the binding environment;
/// a secret value in it adds the pooler route. `Err` with a reason when the deployment cannot
/// start (no image, not a node22 manifest, a bad digest).
pub fn workload_spec(
    identity: WorkloadIdentity,
    manifest: &Value,
    image_digest: Option<&str>,
    plan: &AppPlanLimits,
    settings: &Node22Settings,
    workload_token: SecretValue,
    extra: Vec<EnvVar>,
) -> std::result::Result<WorkloadSpec, String> {
    if !is_server(manifest) {
        return Err("the manifest is not a node22 server".to_owned());
    }
    let digest = image_digest.ok_or("the deployment has no image yet")?;
    let digest = OciDigest::parse(digest).map_err(|e| format!("the image digest: {e}"))?;
    let mut routes = vec![Route::EgressProxy];
    if extra.iter().any(|e| matches!(e.value, EnvValue::Secret(_))) {
        routes.push(Route::Pooler);
    }
    let mut env = plain_env(manifest);
    env.extend(extra);
    Ok(WorkloadSpec {
        identity,
        kind: WorkloadKind::Service(ServiceSpec {
            port: PORT,
            health: Some(health_check(manifest, settings)),
            // Always scales to zero (always-on is refused before the answer).
            idle_timeout: Some(settings.idle_timeout),
        }),
        image: ImageRef::Oci(digest),
        command: Vec::new(),
        env,
        previous_secrets: Vec::new(),
        workload_token,
        limits: instance_limits(manifest, plan, settings),
        routes,
    })
}

/// The stage error of a refused runtime call.
fn runtime_stage_error(
    stage: &'static str,
    reason: &'static str,
    what: &str,
    e: &RuntimeError,
) -> StageError {
    let mut s = StageError::new(stage, reason, format!("{what}: {e}"));
    s.details = json!({ "runtime_error": e.code() });
    s
}

/// The stage error of a failed build: the builder's reason (bounded, it holds the end of the
/// build log, which holds no secret: the build has none).
pub(crate) fn build_error(e: &RuntimeError) -> StageError {
    match e {
        RuntimeError::BuildFailed { reason } => {
            let mut s = StageError::new(
                "build",
                "build_failed",
                format!("the build failed: {reason}"),
            );
            s.code = ErrorCode::ValidationFailed;
            s.details = json!({ "runtime_error": e.code() });
            s
        }
        RuntimeError::BuildTimedOut => {
            let mut s = StageError::new(
                "build",
                "build_timed_out",
                "the build ran longer than its time limit",
            );
            s.details = json!({ "runtime_error": e.code() });
            s
        }
        RuntimeError::Unavailable { .. } => {
            // Also a build that found no free build slot in time (review I6-13, 1).
            let mut s = runtime_stage_error(
                "build",
                "build_unavailable",
                "the builder is unavailable",
                e,
            );
            s.hint = Some("try again in a few minutes".into());
            s
        }
        other => runtime_stage_error("build", "build_unavailable", "the builder failed", other),
    }
}

impl AppService {
    /// The workload token of an instance: signed with the server's workload key, else random
    /// (the runtime API refuses it).
    fn node22_token(node22: &Node22, identity: &WorkloadIdentity) -> SecretValue {
        if let Some(issuer) = &node22.tokens {
            match issuer.issue(identity) {
                Ok(t) => return t,
                Err(e) => {
                    tracing::warn!(target: "ruum::apps", error = %e, "the workload token could not be signed; the instance gets a random one");
                }
            }
        }
        SecretValue::new(hex::encode(rand::random::<[u8; 32]>()))
    }

    /// The spec of a node22 deployment's instance for a start by the sandbox directory (the
    /// spec maker of card I6-13): the binding environment of this start and a fresh workload
    /// token. `Ok(None)` when node22 deployments are off or the deployment cannot start.
    pub async fn server_spec(&self, row: &DeploymentRow) -> Result<Option<WorkloadSpec>> {
        let Some(node22) = self.node22() else {
            return Ok(None);
        };
        let Some(manifest) = row.manifest.as_ref() else {
            return Ok(None);
        };
        if !is_server(manifest) || row.image_digest.is_none() {
            return Ok(None);
        }
        let plan = self.plan_limits(row.tenant_id).await?;
        let extra = self
            .binding_env_for_start(row.tenant_id, row.app_node_id, row.id)
            .await?;
        let identity = WorkloadIdentity {
            tenant: row.tenant_id,
            app: row.app_node_id,
            deployment: row.id,
            environment: match row.environment {
                super::super::store::Environment::Live => Environment::Live,
                super::super::store::Environment::Preview => Environment::Preview,
            },
        };
        let token = Self::node22_token(&node22, &identity);
        match workload_spec(
            identity,
            manifest,
            row.image_digest.as_deref(),
            &plan,
            &node22.settings,
            token,
            extra,
        ) {
            Ok(spec) => Ok(Some(spec)),
            Err(reason) => {
                tracing::warn!(target: "ruum::apps", tenant = %row.tenant_id, deployment = %row.id, reason, "a node22 deployment cannot start");
                Ok(None)
            }
        }
    }

    /// The spec maker of the sandbox directory over [`Self::server_spec`].
    pub fn spec_maker(self: &Arc<Self>) -> super::super::sandboxes::SpecMaker {
        let me = self.clone();
        Arc::new(move |row: DeploymentRow| {
            let me = me.clone();
            Box::pin(async move {
                match me.server_spec(&row).await {
                    Ok(spec) => spec,
                    Err(e) => {
                        tracing::warn!(target: "ruum::apps", tenant = %row.tenant_id, deployment = %row.id, error = %e, "the spec of a node22 deployment failed");
                        None
                    }
                }
            })
        })
    }

    /// The stage `build` of a node22 preview: the image digest.
    pub(crate) async fn node22_build(
        &self,
        identity: WorkloadIdentity,
        archive: bytes::Bytes,
    ) -> std::result::Result<OciDigest, StageError> {
        let Some(node22) = self.node22() else {
            return Err(StageError::new(
                "build",
                "runtime_not_ready",
                "node22 deployments are off on this server",
            ));
        };
        let out = node22
            .builder
            .build(BuildInput {
                identity,
                runtime: RUNTIME.to_owned(),
                archive,
                limits: node22.settings.build_limits,
                timeout: node22.settings.build_timeout,
            })
            .await
            .map_err(|e| build_error(&e))?;
        match out.image {
            ImageRef::Oci(d) => Ok(d),
            _ => Err(StageError::new(
                "build",
                "build_unavailable",
                "the builder returned no OCI image",
            )),
        }
    }

    /// Starts a node22 instance once and stops it again (the stage `start`, and the start
    /// before a publish when `publish` is true): it must be running and healthy in time. The
    /// check result (`AppCheckResult`) on success.
    pub(crate) async fn node22_start_check(
        &self,
        spec: WorkloadSpec,
        manifest: &Value,
        publish: bool,
    ) -> std::result::Result<Value, StageError> {
        let Some(node22) = self.node22() else {
            return Err(StageError::new(
                "start",
                "runtime_not_ready",
                "node22 deployments are off on this server",
            ));
        };
        let wait = start_check_wait(manifest, &node22.settings, publish);
        start_and_check(self.deps.runtime.clone(), spec, wait).await
    }

    /// The spec of a start check (the stage `start` of a preview, or the start before a
    /// publish, where `bindings` says whether the binding environment is asked for).
    pub(crate) async fn node22_check_spec(
        &self,
        tenant: TenantId,
        identity: WorkloadIdentity,
        manifest: &Value,
        image_digest: &str,
        bindings: bool,
    ) -> std::result::Result<WorkloadSpec, StageError> {
        let Some(node22) = self.node22() else {
            return Err(StageError::new(
                "start",
                "runtime_not_ready",
                "node22 deployments are off on this server",
            ));
        };
        let plan = self.plan_limits(tenant).await.map_err(|e| {
            tracing::warn!(target: "ruum::apps", error = %e, "the plan of a node22 start");
            StageError::new(
                "start",
                "start_failed",
                "the plan of the space cannot be read",
            )
        })?;
        let extra = if bindings {
            self.binding_env_for_start(tenant, identity.app, identity.deployment)
                .await
                .map_err(|e| {
                    tracing::warn!(target: "ruum::apps", error = %e, "the binding environment of a node22 start");
                    StageError::new(
                        "start",
                        "bindings_failed",
                        "the environment of the bindings cannot be made",
                    )
                })?
        } else {
            Vec::new()
        };
        let token = Self::node22_token(&node22, &identity);
        workload_spec(
            identity,
            manifest,
            Some(image_digest),
            &plan,
            &node22.settings,
            token,
            extra,
        )
        .map_err(|reason| StageError::new("start", "start_failed", reason))
    }
}

/// How long a start check waits for the stop of its instance before it lets the stop go on in
/// the background (the start before a publish runs inside the request).
const STOP_WAIT: Duration = Duration::from_secs(10);

/// A start that runs in its own task. Dropped while the task still runs (a late start, or a
/// dropped caller), it lets the start finish and stops the instance it made.
struct PendingStart {
    handle: Option<
        tokio::task::JoinHandle<
            std::result::Result<crate::runtime::InstanceStatus, crate::runtime::RuntimeError>,
        >,
    >,
    driver: Arc<dyn RuntimeDriver>,
    tenant: TenantId,
}

impl Drop for PendingStart {
    fn drop(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        let (driver, tenant) = (self.driver.clone(), self.tenant);
        let cleanup = async move {
            if let Ok(Ok(st)) = handle.await
                && let Err(e) = driver.stop(tenant, st.id).await
            {
                tracing::warn!(target: "ruum::apps", %tenant, error = %e, "could not stop a late node22 start");
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                rt.spawn(cleanup);
            }
            Err(_) => {
                tracing::warn!(target: "ruum::apps", %tenant, "a node22 start was dropped outside a runtime; its instance scales to zero after its idle time");
            }
        }
    }
}

/// Starts `spec`, waits until it is running and healthy (at most `wait`), and stops it.
pub(crate) async fn start_and_check(
    driver: Arc<dyn RuntimeDriver>,
    spec: WorkloadSpec,
    wait: Duration,
) -> std::result::Result<Value, StageError> {
    let tenant = spec.identity.tenant;
    let path = match &spec.kind {
        WorkloadKind::Service(s) => s.health.as_ref().map(|h| h.path.clone()),
        _ => None,
    }
    .unwrap_or_else(|| "/".to_owned());
    // The wait counts from the start request and bounds the start too (Kimi re-check of the
    // review of I6-13): the start runs in a task of its own, so a start that is late, or whose
    // caller is dropped, never cuts the driver off half way; its instance is stopped once the
    // start returns.
    let deadline = tokio::time::Instant::now() + wait;
    let mut pending = PendingStart {
        handle: Some(tokio::spawn({
            let driver = driver.clone();
            async move { driver.start(spec).await }
        })),
        driver: driver.clone(),
        tenant,
    };
    let joined = match pending.handle.as_mut() {
        Some(h) => tokio::time::timeout_at(deadline, h).await,
        None => unreachable!("the start task is set above"),
    };
    let status = match joined {
        Err(_) => {
            // `pending` drops here: the start goes on and its instance is stopped.
            return Err(StageError::new(
                "start",
                "not_ready",
                format!(
                    "the runtime did not start the server within {} s",
                    wait.as_secs()
                ),
            ));
        }
        Ok(joined) => {
            pending.handle = None;
            joined
                .map_err(|e| {
                    StageError::new("start", "runtime_refused", format!("the start failed: {e}"))
                })?
                .map_err(|e| {
                    runtime_stage_error(
                        "start",
                        "runtime_refused",
                        "the runtime refused the instance",
                        &e,
                    )
                })?
        }
    };
    let id = status.id;
    // Stops the instance also when this future is dropped (a cancelled publish; Kimi I6-13, 1).
    struct StopOnDrop {
        driver: Arc<dyn RuntimeDriver>,
        tenant: TenantId,
        id: crate::runtime::InstanceId,
        armed: bool,
    }
    impl Drop for StopOnDrop {
        fn drop(&mut self) {
            if !self.armed {
                return;
            }
            let (driver, tenant, id) = (self.driver.clone(), self.tenant, self.id);
            match tokio::runtime::Handle::try_current() {
                Ok(rt) => {
                    rt.spawn(async move {
                        if let Err(e) = driver.stop(tenant, id).await {
                            tracing::warn!(target: "ruum::apps", %tenant, error = %e, "could not stop a cancelled node22 start check");
                        }
                    });
                }
                Err(_) => {
                    tracing::warn!(target: "ruum::apps", %tenant, "a node22 start check was dropped outside a runtime; its instance scales to zero after its idle time");
                }
            }
        }
    }
    let mut guard = StopOnDrop {
        driver: driver.clone(),
        tenant,
        id,
        armed: true,
    };
    let mut st = status;
    let outcome = loop {
        // A healthy answer after the deadline does not count either.
        if tokio::time::Instant::now() >= deadline {
            break Err(StageError::new(
                "start",
                "not_ready",
                format!(
                    "the server was not healthy at {path} within {} s (state {})",
                    wait.as_secs(),
                    st.state.name()
                ),
            ));
        }
        match st.state {
            InstanceState::Running if st.health == crate::runtime::Health::Healthy => break Ok(()),
            InstanceState::Running if st.health == crate::runtime::Health::Unhealthy => {
                break Err(StageError::new(
                    "start",
                    "unhealthy",
                    format!("the health check of {path} failed"),
                ));
            }
            InstanceState::Exited(exit) => {
                let mut e = StageError::new(
                    "start",
                    "crashed",
                    format!(
                        "the server ended before it was healthy ({:?}, code {:?})",
                        exit.reason, exit.code
                    ),
                );
                e.details = json!({ "exit_code": exit.code });
                break Err(e);
            }
            InstanceState::Stopped | InstanceState::ScaledToZero => {
                break Err(StageError::new(
                    "start",
                    "not_running",
                    format!("the instance did not start (state {})", st.state.name()),
                ));
            }
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        // Every status request ends at the deadline too (Kimi re-check 2 of the review of
        // I6-13): a slow runtime API never holds the request past the cap.
        st = match tokio::time::timeout_at(deadline, driver.status(tenant, id)).await {
            Err(_) => {
                break Err(StageError::new(
                    "start",
                    "not_ready",
                    format!(
                        "the runtime did not report the server healthy within {} s",
                        wait.as_secs()
                    ),
                ));
            }
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                break Err(runtime_stage_error(
                    "start",
                    "runtime_refused",
                    "the runtime lost the instance",
                    &e,
                ));
            }
        };
    };
    // The stop is bounded as well: after STOP_WAIT the guard stays armed and its drop sends the
    // stop again in a task of its own.
    match tokio::time::timeout(STOP_WAIT, guard.driver.stop(tenant, id)).await {
        Ok(Ok(())) => guard.armed = false,
        Ok(Err(e)) => {
            tracing::warn!(target: "ruum::apps", %tenant, error = %e, "could not stop a node22 start check");
            guard.armed = false;
        }
        Err(_) => {
            tracing::warn!(target: "ruum::apps", %tenant, "the stop of a node22 start check is slow; it goes on in the background");
        }
    }
    outcome.map(|()| {
        json!({
            "passed": true,
            "routes": 1,
            "console_errors": 0,
            "failed_requests": 0,
            "server_errors": 0,
            "new_issue_ids": [],
            "details": [],
        })
    })
}
