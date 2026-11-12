//! The runtime border (design 18.1, `docs/STEP3.md` area 3): the [`RuntimeDriver`] trait that
//! runs app workloads, and the [`Builder`] trait (in [`builder`]) that turns an app archive into
//! an image.
//!
//! Every driver (this module's in-memory driver, the Docker driver of card R-2a with its runsc
//! option, Kubernetes with gVisor on staging) implements the same trait and passes the same
//! conformance suite ([`conformance::run`] and [`conformance::run_builder`]). A signature change
//! needs the agreement of the `runtime` and `apps` areas.
//!
//! # The rules the trait carries (`docs/STEP3.md`, "How an app runs")
//!
//! **Rule 1, isolation.** App code and build code are untrusted. A driver runs each instance in
//! its own sandbox with the [`Limits`] of the spec (CPU, memory, disk) and, for a job, the time
//! limit of [`JobSpec::timeout`]. The status reports the limits that the driver applied
//! ([`Applied::limits`]); they are exactly the requested ones. Two apps never share a network,
//! and the live and the preview instances of one app never share one either
//! ([`Applied::network`]). The identity of an instance (tenant, app, deployment, environment) is
//! given by the platform in [`WorkloadSpec::identity`], never by the app: the driver attaches it
//! to every log line and usage record itself.
//!
//! **Rule 2, network: default deny.** The routes of a workload are data
//! ([`WorkloadSpec::routes`]): the internal gateway, the egress proxy, the pooler, nothing else.
//! The driver applies exactly these routes and reports them in [`Applied::routes`]. No driver
//! adds a route on its own; a spec without routes gets none. Builds get only the egress proxy
//! ([`builder::BUILD_ROUTES`]).
//!
//! **Rule 7, secrets are write-only.** Environment values marked secret ([`EnvValue::Secret`])
//! reach the workload at start only. The log stream of a workload carries no exact match of a
//! current secret value, of a previous one ([`WorkloadSpec::previous_secrets`]) or of the
//! workload token (also after [`RuntimeDriver::rotate_token`]). The redaction runs inside the
//! driver, before a line leaves it, also when a value is split across two writes
//! ([`redact::LineRedactor`]); the marker is [`redact::REDACTED`]. [`SecretValue`] has no
//! `Serialize` and a hidden `Debug`. A build gets no secrets and no bindings: the
//! [`builder::BuildInput`] type has no field that could hold them.
//!
//! # Workload kinds and images
//!
//! [`WorkloadKind`] is an open set (`#[non_exhaustive]`): `service` (HTTP, health check, scale to
//! zero with an idle timer, wake on demand) and `job` (run to completion with an exit status and
//! a timeout). Step 4 builds and step 5 session sandboxes add kinds, not a trait change; a
//! driver refuses a kind it does not know with [`RuntimeError::UnsupportedKind`].
//!
//! [`ImageRef`] has two kinds: `static` (a content-addressed file tree in the blob store; the
//! driver runs no code for it, the gateway serves the files) and `oci` (a digest in the local
//! registry, made by a [`Builder`]). The registry has one repository per tenant and app: a
//! builder pushes to the repository of [`builder::BuildInput::identity`], and a driver looks a
//! digest up only in the repository of [`WorkloadSpec::identity`] (tenant and app; the live and
//! the preview deployments of one app share it). The digest of another app's image is
//! [`RuntimeError::ImageNotFound`], also when the driver could reach it.
//!
//! # The workload token
//!
//! The driver mounts the workload token as a file at [`TOKEN_FILE`] and sets the variable
//! [`TOKEN_FILE_ENV`] to that path (`contracts/apps/runtime.openapi.yaml`, "The app's
//! credential"). The token is never an environment variable: [`RuntimeDriver::rotate_token`]
//! rewrites the file in place, so a running process reads the new token at its next read.
//!
//! # Logs and privacy
//!
//! The text of a log line comes from the app, so it is `personal` data until the log processor
//! classifies and pseudonymizes it (design 8.5, decisions 0004 and 0005). The driver's buffer is
//! short-lived: only the log shipper or the processor reads it ([`RuntimeDriver::logs`]), and no
//! operation or UI path reads driver logs past the processor. A driver bounds the buffer (a ring
//! per instance); the oldest lines go first, so `seq` can start with a gap.
//!
//! # Usage
//!
//! A driver reports instance time and memory per instance, and a builder reports build time,
//! through a [`RuntimeUsageSink`] given at construction. The deploy pipeline (card W-2) forwards
//! the records to the platform's usage meters.

pub mod builder;
pub mod conformance;
pub mod memory;
pub mod program;
pub mod redact;

#[cfg(test)]
mod tests;

pub use builder::{BUILD_ROUTES, BuildInput, BuildLogLine, BuildOutput, Builder};
pub use memory::{MemoryBuilder, MemoryDriver, MemoryRegistry};

/// Where the driver mounts the workload token inside the sandbox
/// (`contracts/apps/runtime.openapi.yaml`).
pub const TOKEN_FILE: &str = "/var/run/ruum/token";

/// The environment variable that holds [`TOKEN_FILE`]. The platform sets it; a spec that sets it
/// is [`RuntimeError::InvalidSpec`].
pub const TOKEN_FILE_ENV: &str = "RUUM_TOKEN_FILE";

/// The longest health check path [`WorkloadSpec::validate`] accepts, in bytes.
pub const MAX_HEALTH_PATH_BYTES: usize = 1024;

use crate::blobs::Hash;
use crate::ids::{NodeId, TenantId};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use std::sync::Mutex;
use std::time::Duration;
use time::OffsetDateTime;
use uuid::Uuid;

// ------------------------------------------------------------------------------ identity

/// The ID of one instance (UUIDv7). The driver gives it at start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InstanceId(pub Uuid);

impl InstanceId {
    /// Makes a new UUIDv7 ID.
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for InstanceId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for InstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The environment of a deployment: the live one or a preview (design 9.2, rule 6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Environment {
    Live,
    Preview,
}

impl Environment {
    pub fn as_str(self) -> &'static str {
        match self {
            Environment::Live => "live",
            Environment::Preview => "preview",
        }
    }
}

/// Who an instance or a build belongs to. The platform gives it; the app never does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkloadIdentity {
    pub tenant: TenantId,
    /// The app node.
    pub app: NodeId,
    /// The deployment (`app_deployments.id`).
    pub deployment: Uuid,
    pub environment: Environment,
}

// ------------------------------------------------------------------------------ secrets

/// A secret value: a secret environment value or a workload token. It has no `Serialize`, and
/// its `Debug` shows the marker only, so a value never ends up in a log or an API answer by
/// accident.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(String);

impl SecretValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value. Only a driver calls this, to hand the value to the workload.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretValue({})", redact::REDACTED)
    }
}

/// One environment variable of a workload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvVar {
    pub name: String,
    pub value: EnvValue,
}

impl EnvVar {
    pub fn plain(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: EnvValue::Plain(value.into()),
        }
    }

    pub fn secret(name: impl Into<String>, value: SecretValue) -> Self {
        Self {
            name: name.into(),
            value: EnvValue::Secret(value),
        }
    }
}

/// The value of an environment variable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvValue {
    /// A value from the manifest's `env`.
    Plain(String),
    /// A secret value (rule 7): passed at start only, redacted from the logs.
    Secret(SecretValue),
}

// ------------------------------------------------------------------------------ the spec

/// Where a workload may connect (rule 2). Nothing else is reachable. The set is open: a new
/// route is a decision of the runtime area, not something a driver invents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Route {
    /// The internal gateway at `ruum.internal`: the runtime API and app-to-app calls.
    InternalGateway,
    /// The egress proxy: the only way to the internet.
    EgressProxy,
    /// The database pooler in front of the data cluster (the app's bound databases).
    Pooler,
}

impl Route {
    pub fn as_str(self) -> &'static str {
        match self {
            Route::InternalGateway => "internal_gateway",
            Route::EgressProxy => "egress_proxy",
            Route::Pooler => "pooler",
        }
    }
}

/// The resource limits of a sandbox (rule 1). Every value is required and above zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Limits {
    /// CPU in thousandths of a core (`250` is a quarter core).
    pub cpu_millis: u32,
    /// The memory limit in bytes. Going over it ends the instance ([`ExitReason::OutOfMemory`]).
    pub memory_bytes: u64,
    /// The writable disk (scratch space) in bytes. The root file system is read-only.
    pub disk_bytes: u64,
}

impl Limits {
    fn validate(&self) -> Result<(), RuntimeError> {
        for (field, zero) in [
            ("limits.cpu_millis", self.cpu_millis == 0),
            ("limits.memory_bytes", self.memory_bytes == 0),
            ("limits.disk_bytes", self.disk_bytes == 0),
        ] {
            if zero {
                return Err(RuntimeError::invalid(field, "must be above zero"));
            }
        }
        Ok(())
    }
}

/// The health check of a service: an HTTP `GET` of `path` on the service port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HealthCheck {
    /// Starts with `/`; printable ASCII without spaces; at most [`MAX_HEALTH_PATH_BYTES`].
    pub path: String,
    pub interval: Duration,
    /// The number of failed checks in a row that make the instance unhealthy.
    pub failure_threshold: u32,
}

/// A long-running HTTP service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceSpec {
    /// The port the service listens on inside the sandbox (not 0).
    pub port: u16,
    pub health: Option<HealthCheck>,
    /// Scale to zero after this much time without a request ([`RuntimeDriver::touch`]);
    /// `None` keeps the instance running.
    pub idle_timeout: Option<Duration>,
}

/// A run-to-completion job (jobs, the O-4 browser check). A preview may run a job (the O-4
/// check runs against previews); "previews run no scheduled jobs" (rule 6) is the scheduler's
/// rule (card R-5), not the driver's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobSpec {
    /// The job ends with [`ExitReason::TimedOut`] when it runs longer.
    pub timeout: Duration,
}

/// The kind of a workload. Later steps add kinds; a driver refuses an unknown kind with
/// [`RuntimeError::UnsupportedKind`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkloadKind {
    Service(ServiceSpec),
    Job(JobSpec),
}

impl WorkloadKind {
    pub fn name(&self) -> &'static str {
        match self {
            WorkloadKind::Service(_) => "service",
            WorkloadKind::Job(_) => "job",
        }
    }
}

/// One file of a static image.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct StaticFile {
    /// The path in the tree, relative, with `/` separators (`assets/app.js`).
    pub path: String,
    /// The content hash of the file in the blob store.
    pub content: Hash,
    pub size: u64,
}

/// A static image: an unpacked file tree in the blob store (made by the image stage of card
/// W-2). The driver runs no code for it; the gateway serves the files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaticImage {
    /// The content hash of the tree: [`StaticImage::tree_hash`] of [`StaticImage::files`].
    pub tree: Hash,
    /// The files, sorted by path.
    pub files: Vec<StaticFile>,
}

impl StaticImage {
    /// Makes the image of a file list: sorts the files and computes the tree hash.
    pub fn new(mut files: Vec<StaticFile>) -> Self {
        files.sort();
        let tree = Self::tree_hash(&files);
        Self { tree, files }
    }

    /// The tree hash: BLAKE3 of one line per file, `path NUL content-hex NUL size LF`, in path
    /// order.
    pub fn tree_hash(files: &[StaticFile]) -> Hash {
        let mut text = String::new();
        for f in files {
            text.push_str(&f.path);
            text.push('\0');
            text.push_str(&f.content.to_hex());
            text.push('\0');
            text.push_str(&f.size.to_string());
            text.push('\n');
        }
        Hash::of(text.as_bytes())
    }

    /// Checks the paths (relative, no `..`, unique, sorted) and the tree hash.
    pub fn verify(&self) -> Result<(), RuntimeError> {
        let mut seen = HashSet::new();
        for f in &self.files {
            let bad = f.path.is_empty()
                || f.path.starts_with('/')
                || f.path.contains('\\')
                || f.path.contains('\0')
                || f.path
                    .split('/')
                    .any(|part| part.is_empty() || part == "." || part == "..");
            if bad {
                return Err(RuntimeError::invalid(
                    "image.files",
                    format!("the path {:?} is not a relative path", f.path),
                ));
            }
            if !seen.insert(f.path.as_str()) {
                return Err(RuntimeError::invalid(
                    "image.files",
                    format!("the path {:?} is listed twice", f.path),
                ));
            }
        }
        if !self.files.windows(2).all(|w| w[0] <= w[1]) {
            return Err(RuntimeError::invalid(
                "image.files",
                "the files are not sorted",
            ));
        }
        if Self::tree_hash(&self.files) != self.tree {
            return Err(RuntimeError::invalid(
                "image.tree",
                "the tree hash does not match the files",
            ));
        }
        Ok(())
    }
}

/// A digest of an OCI image in the local registry: `sha256:` and 64 lowercase hex characters.
///
/// A digest is not a capability: it is a content hash, so it can be guessed or can leak. A driver
/// resolves it only in the repository of the spec's tenant and app (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OciDigest(String);

impl OciDigest {
    pub fn parse(s: &str) -> Result<Self, RuntimeError> {
        let ok = s.strip_prefix("sha256:").is_some_and(|h| {
            h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        });
        if ok {
            Ok(Self(s.to_owned()))
        } else {
            Err(RuntimeError::invalid(
                "image.digest",
                "expected sha256: and 64 lowercase hex characters",
            ))
        }
    }

    /// The digest of bytes (SHA-256).
    pub fn of(data: &[u8]) -> Self {
        use sha2::{Digest, Sha256};
        Self(format!("sha256:{}", hex::encode(Sha256::digest(data))))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for OciDigest {
    type Error = RuntimeError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(&s)
    }
}

impl From<OciDigest> for String {
    fn from(d: OciDigest) -> String {
        d.0
    }
}

impl fmt::Display for OciDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a workload runs.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImageRef {
    /// Files served by the gateway; no code runs.
    Static(StaticImage),
    /// An OCI image in the local registry (made by a [`Builder`]), in the repository of the
    /// spec's tenant and app.
    Oci(OciDigest),
}

impl ImageRef {
    pub fn kind(&self) -> &'static str {
        match self {
            ImageRef::Static(_) => "static",
            ImageRef::Oci(_) => "oci",
        }
    }
}

/// Everything a driver needs to start an instance. The platform fills it from the deployment;
/// nothing in it comes from the app's own code.
#[derive(Clone, Debug)]
pub struct WorkloadSpec {
    pub identity: WorkloadIdentity,
    pub kind: WorkloadKind,
    pub image: ImageRef,
    /// The command line; empty means the image's default. Empty for a static image.
    pub command: Vec<String>,
    /// The environment, given at start only.
    pub env: Vec<EnvVar>,
    /// Earlier values of the app's secrets (after a rotation). They do not reach the workload,
    /// but the driver redacts them from the logs too.
    pub previous_secrets: Vec<SecretValue>,
    /// The workload token (the identity of the instance at the runtime API). The driver mounts
    /// it as the file [`TOKEN_FILE`], sets [`TOKEN_FILE_ENV`] to its path, and redacts it from
    /// the logs. It is never an environment variable.
    pub workload_token: SecretValue,
    pub limits: Limits,
    /// The routes of rule 2. Empty for a static image.
    pub routes: Vec<Route>,
}

impl WorkloadSpec {
    /// The checks every driver makes before it starts anything. A driver may add its own. The
    /// spec comes from the manifest, which the app's author controls, so every value that
    /// reaches a process, a probe or the Docker API is checked here.
    pub fn validate(&self) -> Result<(), RuntimeError> {
        self.limits.validate()?;
        if self.command.iter().any(|a| a.contains('\0')) {
            return Err(RuntimeError::invalid(
                "command",
                "an argument holds a NUL byte",
            ));
        }
        let mut names = HashSet::new();
        for var in &self.env {
            let mut chars = var.name.chars();
            let ok = chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !ok {
                return Err(RuntimeError::invalid(
                    "env",
                    format!("{:?} is not a valid environment variable name", var.name),
                ));
            }
            if !names.insert(var.name.as_str()) {
                return Err(RuntimeError::invalid(
                    "env",
                    format!("{} is set twice", var.name),
                ));
            }
            if var.name == TOKEN_FILE_ENV {
                return Err(RuntimeError::invalid(
                    "env",
                    format!("{TOKEN_FILE_ENV} is set by the platform"),
                ));
            }
            let value = match &var.value {
                EnvValue::Plain(v) => v.as_str(),
                EnvValue::Secret(v) => v.expose(),
            };
            if value.contains('\0') {
                return Err(RuntimeError::invalid(
                    "env",
                    format!("the value of {} holds a NUL byte", var.name),
                ));
            }
        }
        let mut routes = HashSet::new();
        for r in &self.routes {
            if !routes.insert(*r) {
                return Err(RuntimeError::invalid(
                    "routes",
                    format!("{} is listed twice", r.as_str()),
                ));
            }
        }
        if self.workload_token.expose().is_empty() {
            return Err(RuntimeError::invalid("workload_token", "must not be empty"));
        }
        match &self.kind {
            WorkloadKind::Service(s) => {
                if s.port == 0 {
                    return Err(RuntimeError::invalid("kind.port", "must not be 0"));
                }
                if let Some(h) = &s.health {
                    if h.interval.is_zero() || h.failure_threshold == 0 {
                        return Err(RuntimeError::invalid(
                            "kind.health",
                            "the interval and the failure threshold must be above zero",
                        ));
                    }
                    if h.interval.checked_mul(h.failure_threshold).is_none() {
                        return Err(RuntimeError::invalid(
                            "kind.health",
                            "the interval times the failure threshold is too long",
                        ));
                    }
                    if !h.path.starts_with('/') {
                        return Err(RuntimeError::invalid(
                            "kind.health.path",
                            "must start with /",
                        ));
                    }
                    if h.path.len() > MAX_HEALTH_PATH_BYTES {
                        return Err(RuntimeError::invalid(
                            "kind.health.path",
                            format!("must be at most {MAX_HEALTH_PATH_BYTES} bytes"),
                        ));
                    }
                    // The path goes into an HTTP request line: no spaces, no control characters,
                    // no line ends.
                    if !h.path.bytes().all(|b| b.is_ascii_graphic()) {
                        return Err(RuntimeError::invalid(
                            "kind.health.path",
                            "must be printable ASCII without spaces",
                        ));
                    }
                }
                if s.idle_timeout.is_some_and(|d| d.is_zero()) {
                    return Err(RuntimeError::invalid(
                        "kind.idle_timeout",
                        "must be above zero",
                    ));
                }
            }
            WorkloadKind::Job(j) => {
                if j.timeout.is_zero() {
                    return Err(RuntimeError::invalid("kind.timeout", "must be above zero"));
                }
            }
        }
        if let ImageRef::Static(img) = &self.image {
            img.verify()?;
            if !matches!(self.kind, WorkloadKind::Service(_)) {
                return Err(RuntimeError::UnsupportedImage {
                    reason: format!(
                        "a static image runs no code, so it cannot be a {}",
                        self.kind.name()
                    ),
                });
            }
            if !self.command.is_empty() {
                return Err(RuntimeError::invalid(
                    "command",
                    "a static image runs no code",
                ));
            }
            if !self.routes.is_empty() {
                return Err(RuntimeError::invalid(
                    "routes",
                    "a static image runs no code, so it gets no routes",
                ));
            }
        }
        Ok(())
    }

    /// Every value the driver must redact from the logs: the secret environment values, the
    /// previous secret values and the workload token. Empty values are left out.
    pub fn redacted_values(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self
            .env
            .iter()
            .filter_map(|v| match &v.value {
                EnvValue::Secret(s) => Some(s.expose()),
                EnvValue::Plain(_) => None,
            })
            .chain(self.previous_secrets.iter().map(SecretValue::expose))
            .chain(std::iter::once(self.workload_token.expose()))
            .filter(|s| !s.is_empty())
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }
}

// ------------------------------------------------------------------------------ status

/// The network of an instance as the driver names it (a Docker network, a Kubernetes
/// namespace). Equal IDs mean a shared network.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NetworkId(pub String);

/// What the driver applied, as it observes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    /// The routes in force: exactly [`WorkloadSpec::routes`].
    pub routes: Vec<Route>,
    /// The network of the instance; `None` for a static image (no code runs).
    pub network: Option<NetworkId>,
    /// The limits in force: exactly [`WorkloadSpec::limits`].
    pub limits: Limits,
}

/// Why a process ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExitReason {
    /// The process ended by itself; [`ExitStatus::code`] says how.
    Completed,
    /// A job ran longer than [`JobSpec::timeout`].
    TimedOut,
    /// The process went over [`Limits::memory_bytes`].
    OutOfMemory,
    /// The driver or the system ended it for another reason.
    Killed,
}

/// How a process ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExitStatus {
    pub reason: ExitReason,
    /// The exit code when the process ended by itself.
    pub code: Option<i32>,
}

impl ExitStatus {
    pub fn completed(code: i32) -> Self {
        Self {
            reason: ExitReason::Completed,
            code: Some(code),
        }
    }

    pub fn of(reason: ExitReason) -> Self {
        Self { reason, code: None }
    }

    /// A job succeeded: it completed with code 0.
    pub fn success(&self) -> bool {
        self.reason == ExitReason::Completed && self.code == Some(0)
    }
}

/// The state of an instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum InstanceState {
    /// Started, not yet running.
    Starting,
    Running,
    /// A service with no process after its idle time; [`RuntimeDriver::wake`] starts it again.
    ScaledToZero,
    /// The process ended (a job, or a service that stopped by itself).
    Exited(ExitStatus),
    /// Stopped by [`RuntimeDriver::stop`].
    Stopped,
}

impl InstanceState {
    pub fn name(&self) -> &'static str {
        match self {
            InstanceState::Starting => "starting",
            InstanceState::Running => "running",
            InstanceState::ScaledToZero => "scaled_to_zero",
            InstanceState::Exited(_) => "exited",
            InstanceState::Stopped => "stopped",
        }
    }
}

/// The health of an instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    /// No check result yet, or no health check.
    Unknown,
    Healthy,
    /// The health check failed [`HealthCheck::failure_threshold`] times in a row.
    Unhealthy,
}

/// The status of an instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstanceStatus {
    pub id: InstanceId,
    pub identity: WorkloadIdentity,
    /// [`WorkloadKind::name`].
    pub kind: &'static str,
    pub state: InstanceState,
    pub health: Health,
    pub applied: Applied,
    pub started_at: OffsetDateTime,
}

/// The output stream a log line came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    Stdout,
    Stderr,
}

/// One log line of an instance, already redacted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogLine {
    /// The position in the instance's stream; it grows by one per line, over cold starts too.
    pub seq: u64,
    pub at: OffsetDateTime,
    pub instance: InstanceId,
    /// Attached by the driver from the spec.
    pub identity: WorkloadIdentity,
    pub stream: LogStream,
    /// The line without its line end.
    pub text: String,
}

// ------------------------------------------------------------------------------ errors

/// The errors of a driver or a builder. Typed and machine-readable: [`RuntimeError::code`] and
/// [`RuntimeError::details`] map to a stage error with `error.details`. No variant holds a
/// secret value.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RuntimeError {
    /// No such instance for this tenant.
    #[error("no instance {instance}")]
    NotFound { instance: InstanceId },
    /// The spec or the build input is not valid.
    #[error("{field}: {reason}")]
    InvalidSpec { field: String, reason: String },
    /// The driver does not run this workload kind.
    #[error("the driver does not run workloads of kind {kind}")]
    UnsupportedKind { kind: String },
    /// The driver cannot run this image for this workload.
    #[error("unsupported image: {reason}")]
    UnsupportedImage { reason: String },
    /// The image is not in the registry.
    #[error("no image {image} in the registry")]
    ImageNotFound { image: String },
    /// The operation does not apply in the instance's current state.
    #[error("cannot {operation} an instance that is {state}")]
    InvalidState { operation: String, state: String },
    /// The builder has no such runtime.
    #[error("the builder has no runtime {runtime}")]
    UnsupportedRuntime { runtime: String },
    /// The build ran and failed. `reason` is text from untrusted build output: a builder bounds
    /// its length ([`builder::MAX_BUILD_FAILED_REASON_BYTES`]) and redacts its own credentials
    /// (the registry push credential) from it; a UI escapes it where it shows it.
    #[error("the build failed: {reason}")]
    BuildFailed { reason: String },
    /// The build ran longer than its timeout.
    #[error("the build ran longer than its timeout")]
    BuildTimedOut,
    /// The driver or its backend (Docker, Kubernetes) cannot be reached; retry later.
    #[error("the runtime is unavailable: {reason}")]
    Unavailable { reason: String },
    #[error("runtime error: {reason}")]
    Internal { reason: String },
}

impl RuntimeError {
    pub fn invalid(field: impl Into<String>, reason: impl Into<String>) -> Self {
        RuntimeError::InvalidSpec {
            field: field.into(),
            reason: reason.into(),
        }
    }

    pub(crate) fn state(operation: &str, state: &InstanceState) -> Self {
        RuntimeError::InvalidState {
            operation: operation.to_owned(),
            state: state.name().to_owned(),
        }
    }

    /// The machine-readable code (snake case).
    pub fn code(&self) -> &'static str {
        match self {
            RuntimeError::NotFound { .. } => "instance_not_found",
            RuntimeError::InvalidSpec { .. } => "invalid_spec",
            RuntimeError::UnsupportedKind { .. } => "unsupported_kind",
            RuntimeError::UnsupportedImage { .. } => "unsupported_image",
            RuntimeError::ImageNotFound { .. } => "image_not_found",
            RuntimeError::InvalidState { .. } => "invalid_state",
            RuntimeError::UnsupportedRuntime { .. } => "unsupported_runtime",
            RuntimeError::BuildFailed { .. } => "build_failed",
            RuntimeError::BuildTimedOut => "build_timed_out",
            RuntimeError::Unavailable { .. } => "runtime_unavailable",
            RuntimeError::Internal { .. } => "internal",
        }
    }

    /// The fields of the error as a JSON object, for `error.details`.
    pub fn details(&self) -> serde_json::Value {
        use serde_json::json;
        match self {
            RuntimeError::NotFound { instance } => json!({ "instance": instance }),
            RuntimeError::InvalidSpec { field, reason } => {
                json!({ "field": field, "reason": reason })
            }
            RuntimeError::UnsupportedKind { kind } => json!({ "kind": kind }),
            RuntimeError::UnsupportedImage { reason } => json!({ "reason": reason }),
            RuntimeError::ImageNotFound { image } => json!({ "image": image }),
            RuntimeError::InvalidState { operation, state } => {
                json!({ "operation": operation, "state": state })
            }
            RuntimeError::UnsupportedRuntime { runtime } => json!({ "runtime": runtime }),
            RuntimeError::BuildFailed { reason } => json!({ "reason": reason }),
            RuntimeError::BuildTimedOut => json!({}),
            RuntimeError::Unavailable { reason } => json!({ "reason": reason }),
            RuntimeError::Internal { reason } => json!({ "reason": reason }),
        }
    }
}

// ------------------------------------------------------------------------------ usage

/// A usage record of the runtime (design 21; `docs/STEP3.md` "Costs and metering"). The deploy
/// pipeline forwards it to the platform's usage meters, which charge live instances to the app
/// and previews and builds to the deployer.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RuntimeUsage {
    /// Instance time: how long a run of an instance ran and its memory limit. A driver sends
    /// one at the end of each run (stop, scale to zero, exit). It may also send interim records
    /// while a run goes on (each with the time since the last record of that run), so a service
    /// that never scales to zero is charged before it stops and a platform crash loses little;
    /// the meters add the records up.
    Instance {
        identity: WorkloadIdentity,
        instance: InstanceId,
        ran: Duration,
        memory_bytes: u64,
    },
    /// One build ended (also a failed one).
    Build {
        identity: WorkloadIdentity,
        took: Duration,
    },
}

/// Where a driver or a builder reports usage. Kept local to this module, so the runtime does
/// not depend on the usage area; an adapter joins the two.
pub trait RuntimeUsageSink: Send + Sync {
    fn record(&self, usage: RuntimeUsage);
}

/// A sink that keeps every record, for tests and the conformance suite.
#[derive(Default)]
pub struct MemoryUsage {
    records: Mutex<Vec<RuntimeUsage>>,
}

impl MemoryUsage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn records(&self) -> Vec<RuntimeUsage> {
        self.records.lock().expect("usage lock").clone()
    }
}

impl RuntimeUsageSink for MemoryUsage {
    fn record(&self, usage: RuntimeUsage) {
        self.records.lock().expect("usage lock").push(usage);
    }
}

// ------------------------------------------------------------------------------ the trait

/// The runtime driver: runs workloads in sandboxes (module border, design 18.1). See the module
/// documentation for the rules every implementation keeps.
///
/// Every call after `start` names the tenant; an instance of another tenant is
/// [`RuntimeError::NotFound`].
#[async_trait]
pub trait RuntimeDriver: Send + Sync {
    /// A short name for reports (`memory`, `docker`, `docker-runsc`).
    fn name(&self) -> &str;

    /// Checks the spec ([`WorkloadSpec::validate`]) and starts an instance. A service runs until
    /// it is stopped or scales to zero; a job runs until it ends or times out. Returns the first
    /// status (state `starting` or `running`).
    ///
    /// `start` is not idempotent: each call starts a new instance. A caller that retries after a
    /// timeout (W-2) keeps the instance ID it got before it reports success, and stops an
    /// instance it does not keep.
    async fn start(&self, spec: WorkloadSpec) -> Result<InstanceStatus, RuntimeError>;

    async fn status(
        &self,
        tenant: TenantId,
        instance: InstanceId,
    ) -> Result<InstanceStatus, RuntimeError>;

    /// Stops the instance and frees its sandbox. Stopping a stopped instance does nothing. The
    /// logs stay readable while the driver keeps the instance.
    async fn stop(&self, tenant: TenantId, instance: InstanceId) -> Result<(), RuntimeError>;

    /// Records a request to a service (the gateway calls it), which restarts its idle timer. On
    /// a service that scaled to zero it returns `Ok` and does not start it: the gateway calls
    /// [`RuntimeDriver::wake`] for that.
    async fn touch(&self, tenant: TenantId, instance: InstanceId) -> Result<(), RuntimeError>;

    /// Starts a service that scaled to zero again (a cold start) and returns when it runs; the
    /// gateway holds the request meanwhile. A running service only gets a touch. Other states
    /// are [`RuntimeError::InvalidState`].
    async fn wake(
        &self,
        tenant: TenantId,
        instance: InstanceId,
    ) -> Result<InstanceStatus, RuntimeError>;

    /// The log lines of an instance after the position `after` (all when `None`), in order, at
    /// most `limit`. The lines are redacted and carry the instance identity. Only the log
    /// shipper or the log processor calls this (see "Logs and privacy" in the module docs); the
    /// buffer is bounded, so the first line can have a `seq` above `after + 1`.
    async fn logs(
        &self,
        tenant: TenantId,
        instance: InstanceId,
        after: Option<u64>,
        limit: usize,
    ) -> Result<Vec<LogLine>, RuntimeError>;

    /// Gives an instance a new workload token (renewal): the driver rewrites [`TOKEN_FILE`] in
    /// place, so the running process reads the new token at its next read, and a cold start
    /// after a scale to zero gets the new token too. The driver keeps redacting the old token.
    async fn rotate_token(
        &self,
        tenant: TenantId,
        instance: InstanceId,
        token: SecretValue,
    ) -> Result<(), RuntimeError>;
}
