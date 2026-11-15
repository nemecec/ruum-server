//! The configuration of the Docker driver. Every value that reaches the Docker API comes from
//! here or from a checked [`ruum_core::runtime::WorkloadSpec`], never from the app.

use ruum_core::runtime::{Route, RuntimeError};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

/// The OCI runtime of the sandboxes (`RUUM_RUNTIME_DRIVER`, STEP3 "Infrastructure work" 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RuntimeKind {
    /// Plain Docker (`runc`): `RUUM_RUNTIME_DRIVER=docker`.
    Runc,
    /// gVisor: `RUUM_RUNTIME_DRIVER=runsc`. The engine must have the runtime `runsc` registered;
    /// the driver refuses to start without it.
    Runsc,
}

impl RuntimeKind {
    /// Reads `RUUM_RUNTIME_DRIVER` (`docker` or `runsc`).
    pub fn parse(value: &str) -> Result<Self, RuntimeError> {
        match value {
            "docker" | "runc" => Ok(RuntimeKind::Runc),
            "runsc" => Ok(RuntimeKind::Runsc),
            other => Err(RuntimeError::Internal {
                reason: format!("RUUM_RUNTIME_DRIVER must be docker or runsc, not {other:?}"),
            }),
        }
    }

    /// The runtime name for the Docker API (`HostConfig.Runtime`).
    pub fn docker_name(self) -> &'static str {
        match self {
            RuntimeKind::Runc => "runc",
            RuntimeKind::Runsc => "runsc",
        }
    }

    /// The driver name in reports: `docker` or `docker-runsc`.
    pub fn driver_name(self) -> &'static str {
        match self {
            RuntimeKind::Runc => "docker",
            RuntimeKind::Runsc => "docker-runsc",
        }
    }
}

/// A shared container that the driver attaches to an app network for one route (the egress
/// proxy, the internal gateway, the pooler). The names are the platform's; a spec cannot name a
/// container.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedContainer {
    /// The container name or ID at the engine.
    pub container: String,
    /// The name the app reaches it by on the app network (a network alias), such as
    /// `ruum-egress` or `ruum.internal`.
    pub alias: String,
    /// The service port it listens on.
    pub port: u16,
}

/// The settings of [`crate::DockerDriver`].
#[derive(Clone, Debug)]
pub struct DockerConfig {
    /// The engine endpoint (`RUUM_DOCKER_HOST`): `unix:///path/to/docker.sock` or
    /// `tcp://host:port` (I-4's socket proxy). No default outside tests.
    pub docker_host: String,
    pub runtime: RuntimeKind,
    /// The local registry (`RUUM_REGISTRY`, `host:port`). Images are pulled only from the
    /// repository of the spec's tenant and app in it ([`crate::repository`]).
    pub registry: String,
    /// The registry URL the driver checks a digest at (`http://` + `registry` by default); the
    /// engine pulls from `registry` itself.
    pub registry_url: String,
    /// A folder on the engine's host for the workload token files: one private (0700) folder per
    /// instance. The engine bind-mounts the token file from it, so it must be on the engine's
    /// host (the driver and the engine share a host in step 3).
    pub state_dir: PathBuf,
    /// The shared container per route. A spec with a route that has none here is refused.
    pub routes: BTreeMap<Route, SharedContainer>,
    /// The process limit of a sandbox (`pids-limit`). Under runsc it bounds the sandbox's host
    /// threads too, so it must leave room for gVisor's own (64 is too few).
    pub pids_limit: i64,
    /// How long a stop waits for the process before it kills it (`stop-timeout`, at most 30 s).
    pub stop_timeout: Duration,
    /// The log lines kept per instance (the ring; the oldest go first).
    pub log_lines: usize,
    /// The longest log line in bytes; a longer one is cut at the cap.
    pub max_line_bytes: usize,
    /// The health check command inside the sandbox; the URL of the check is appended as the
    /// last argument. The default runs a short Node script (`node22` images have Node).
    pub health_command: Vec<String>,
    /// How often the health check runs while the instance starts (Docker's `start-interval`),
    /// and how long the start period lasts. A wake waits for the first passing check.
    pub start_interval: Duration,
    pub start_period: Duration,
    /// Extra labels on everything the driver creates (tests set `ruum.test`).
    pub extra_labels: BTreeMap<String, String>,
    /// The timeout of one Docker API call (not of the long-lived attach and wait streams).
    pub api_timeout: Duration,
}

/// The default health check: `node -e <script> <url>`. The URL is an argument, never part of the
/// script, so no character of the health path can change the script.
pub const NODE_HEALTH_SCRIPT: &str = "require('http').get(process.argv[1],{timeout:2000},r=>{r.resume();process.exit(r.statusCode>=200&&r.statusCode<400?0:1)}).on('error',()=>process.exit(1)).on('timeout',()=>process.exit(1))";

impl DockerConfig {
    /// A configuration with the defaults for everything but the endpoint, the registry and the
    /// state folder.
    pub fn new(
        docker_host: impl Into<String>,
        registry: impl Into<String>,
        state_dir: PathBuf,
    ) -> Self {
        let registry = registry.into();
        Self {
            docker_host: docker_host.into(),
            runtime: RuntimeKind::Runc,
            registry_url: format!("http://{registry}"),
            registry,
            state_dir,
            routes: BTreeMap::new(),
            pids_limit: 256,
            stop_timeout: Duration::from_secs(10),
            log_lines: 10_000,
            max_line_bytes: 16 * 1024,
            health_command: vec![
                "CMD".to_owned(),
                "node".to_owned(),
                "-e".to_owned(),
                NODE_HEALTH_SCRIPT.to_owned(),
            ],
            start_interval: Duration::from_millis(250),
            start_period: Duration::from_secs(10),
            extra_labels: BTreeMap::new(),
            api_timeout: Duration::from_secs(120),
        }
    }

    /// Reads the settings of the environment: `RUUM_DOCKER_HOST`, `RUUM_RUNTIME_DRIVER`
    /// (default `docker`), `RUUM_REGISTRY` and `RUUM_RUNTIME_STATE_DIR` are required or have the
    /// defaults named; the shared containers are added by the wiring (I-4).
    pub fn from_env() -> Result<Self, RuntimeError> {
        let need = |name: &str| {
            std::env::var(name).map_err(|_| RuntimeError::Internal {
                reason: format!("{name} is not set"),
            })
        };
        let mut c = Self::new(
            need("RUUM_DOCKER_HOST")?,
            need("RUUM_REGISTRY")?,
            PathBuf::from(need("RUUM_RUNTIME_STATE_DIR")?),
        );
        if let Ok(v) = std::env::var("RUUM_RUNTIME_DRIVER") {
            c.runtime = RuntimeKind::parse(&v)?;
        }
        Ok(c)
    }

    pub(crate) fn check(&self) -> Result<(), RuntimeError> {
        let bad = |reason: String| Err(RuntimeError::Internal { reason });
        if self.stop_timeout > Duration::from_secs(30) {
            return bad("the stop timeout is at most 30 s".to_owned());
        }
        if self.pids_limit < 32 {
            return bad("the pids limit is at least 32".to_owned());
        }
        if self.log_lines == 0 || self.max_line_bytes == 0 {
            return bad("the log ring and the line cap must be above zero".to_owned());
        }
        if self.max_line_bytes > ruum_core::runtime::redact::MAX_LINE_BYTES {
            return bad("the line cap is at most the redactor's line size".to_owned());
        }
        if self.health_command.first().map(String::as_str) != Some("CMD") {
            return bad("the health command starts with CMD (no shell)".to_owned());
        }
        for (route, s) in &self.routes {
            if s.container.is_empty() || s.alias.is_empty() || s.port == 0 {
                return bad(format!(
                    "the shared container of {} is incomplete",
                    route.as_str()
                ));
            }
        }
        Ok(())
    }
}
