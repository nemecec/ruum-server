//! The `node22` builder (card R-2b): [`DockerBuilder`] implements
//! [`ruum_core::runtime::Builder`] with the fixed recipe of decision 0006 G point 5. Spike S-2
//! showed that rootless BuildKit does not start under the driver's flags with runc, so a build
//! is a **sandbox job** (`npm ci` with postinstall scripts, then the optional `build` command
//! of the manifest) followed by **daemonless image assembly** and **one push** to the build's
//! own repository. The crate README ("Builder") lists the settings and every Docker API call.
//!
//! A build, in order:
//!
//! 1. the input is checked ([`BuildInput::validate`]), the runtime must be `node22`
//!    (`unsupported_runtime` otherwise), and the build waits for a slot: at most
//!    [`BuilderConfig::max_parallel`] builds run at once, at most
//!    [`BuilderConfig::per_tenant`] of one tenant (backlog S-61);
//! 2. the archive is unpacked in memory by W-2s's reader (`ruum_core::apps::archive`; decision
//!    0006 H: no `..`, no absolute path, no `\`, no drive letter, no link or device) and
//!    `app.yaml` is read (`runtime: node22` and `start` required; `build` optional); a refused
//!    archive or manifest is `build_failed` with the reason;
//! 3. the job runs in the sandbox ([`job`]) on a network of its own, registered at the egress
//!    proxy; its output leaves as a byte-capped tar on standard output ([`output`]);
//! 4. the image is assembled without a daemon ([`oci`]) on the base image pinned by the digest
//!    of its multi-arch index ([`NODE22_BASE_INDEX`]), the platform of the engine;
//! 5. the blobs and then the manifest are pushed to `ruum/<tenant>/<app>` only
//!    ([`registry::RepositoryPush`]); the manifest push is the commit point: it runs after the
//!    build's timeout (with a short bound of its own), so a timeout never cuts it half-way;
//! 6. the container, the network (unregistered at the proxy first) and the work data are
//!    removed after every outcome, also after a timeout and when the caller drops the build
//!    (a drop guard spawns the cleanup); a failed build pushes nothing; the build time goes to
//!    the usage sink for every outcome.
//!
//! **No secret reaches a build**: the job's environment is a fixed list ([`job::job_env`]); the
//! registry credential and the egress control token live only in this process; the job has no
//! bind mount and no Docker socket. The conformance case `build_sees_no_platform_secrets`
//! checks it with canaries, hex-encoded too (backlog S-37), and the built image's config and
//! layer must not hold one.

pub mod job;
pub mod oci;
pub mod output;
pub mod registry;

use crate::config::DockerConfig;
use crate::driver::repository;
use crate::egress::EgressRegistry;
use async_trait::async_trait;
use bollard::Docker;
use bytes::Bytes;
use ruum_core::apps::archive;
use ruum_core::ids::TenantId;
use ruum_core::runtime::builder::{MAX_BUILD_FAILED_REASON_BYTES, MAX_BUILD_LOG_LINES};
use ruum_core::runtime::{
    BUILD_ROUTES, BuildInput, BuildLogLine, BuildOutput, Builder, ImageRef, OciDigest,
    RuntimeError, RuntimeUsage, RuntimeUsageSink,
};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use job::{LABEL_BUILD, MANAGED_BY, RECIPE};
pub use registry::RegistryCredential;

/// The digest of the multi-arch index of `node:22-alpine` the builder builds on (as in
/// `testdata/fixture/Dockerfile`). The index itself is embedded (`node22-index.json`, checked
/// against this digest at connect), so the local registry needs only the platform's image.
pub const NODE22_BASE_INDEX: &str =
    "sha256:1314fb30ec37592d5845e5a160d522c0e680972aa534549940b8d73bde88481a";

/// The base repository in the local registry (`RUUM_REGISTRY/ruum-base/node22@<digest>`).
pub const BASE_REPOSITORY: &str = "ruum-base/node22";

/// The default cap of a build's output (`RUUM_BUILD_OUTPUT_MAX`): 256 MiB.
pub const BUILD_OUTPUT_MAX: u64 = 256 * 1024 * 1024;

/// The longest build log line the builder keeps, in bytes.
pub const MAX_BUILD_LOG_LINE_BYTES: usize = 4096;

/// The npm registry, the only host of a build's allow list by default.
pub const NPM_REGISTRY: &str = "https://registry.npmjs.org/";

const EMBEDDED_INDEX: &[u8] = include_bytes!("node22-index.json");

/// The settings of [`DockerBuilder`].
#[derive(Clone, Debug)]
pub struct BuilderConfig {
    /// The engine, the OCI runtime, the registry, the egress proxy (`routes`, only
    /// [`ruum_core::runtime::Route::EgressProxy`] is used), the pids limit, the extra labels
    /// and the API timeout: the same settings as the driver's.
    pub docker: DockerConfig,
    /// The repository of the base image in the local registry.
    pub base_repository: String,
    /// The pinned index of the base image.
    pub base_index: String,
    /// The cap of the output copied out of a job (`RUUM_BUILD_OUTPUT_MAX`).
    pub output_max: u64,
    /// The most builds at once (`RUUM_BUILD_MAX_PARALLEL`).
    pub max_parallel: usize,
    /// The most builds of one tenant at once (`RUUM_BUILD_TENANT_MAX`; at most
    /// `max_parallel`).
    pub per_tenant: usize,
    /// The allow list of a build at the egress proxy: the npm registry hosts.
    pub npm_hosts: Vec<String>,
    /// The npm registry URL the job uses.
    pub npm_registry: String,
    /// The push credential (I-4); `None` for a registry without auth.
    pub credential: Option<RegistryCredential>,
    /// The token service of the registry when it is not on the registry's own origin (an
    /// `https` URL, I-4). A `Bearer` challenge naming any other realm gets no credential.
    pub token_realm: Option<String>,
}

impl BuilderConfig {
    pub fn new(docker: DockerConfig) -> Self {
        Self {
            docker,
            base_repository: BASE_REPOSITORY.to_owned(),
            base_index: NODE22_BASE_INDEX.to_owned(),
            output_max: BUILD_OUTPUT_MAX,
            max_parallel: 4,
            per_tenant: 2,
            npm_hosts: vec!["registry.npmjs.org".to_owned()],
            npm_registry: NPM_REGISTRY.to_owned(),
            credential: None,
            token_realm: None,
        }
    }

    fn check(&self) -> Result<(), RuntimeError> {
        let bad = |reason: &str| {
            Err(RuntimeError::Internal {
                reason: reason.to_owned(),
            })
        };
        self.docker.check()?;
        if self.max_parallel == 0 || self.per_tenant == 0 || self.per_tenant > self.max_parallel {
            return bad("the build slots: 0 < per_tenant <= max_parallel");
        }
        if self.output_max == 0 {
            return bad("the output cap must be above zero");
        }
        if self.npm_hosts.is_empty() {
            return bad("a build needs the npm registry on its allow list");
        }
        if !self
            .docker
            .routes
            .contains_key(&ruum_core::runtime::Route::EgressProxy)
        {
            return bad("the builder needs the egress proxy (DockerConfig::routes)");
        }
        OciDigest::parse(&self.base_index)?;
        Ok(())
    }
}

/// The build slots (backlog S-61): a global cap and a per-tenant share of it. A build takes its
/// tenant's slot first, so builds of one tenant that wait do not hold global slots.
pub struct Slots {
    global: Arc<tokio::sync::Semaphore>,
    per_tenant: usize,
    tenants: Mutex<HashMap<TenantId, Arc<tokio::sync::Semaphore>>>,
}

/// A held slot; dropping it frees it.
pub struct Slot {
    _tenant: tokio::sync::OwnedSemaphorePermit,
    _global: tokio::sync::OwnedSemaphorePermit,
}

impl Slots {
    pub fn new(max_parallel: usize, per_tenant: usize) -> Self {
        Self {
            global: Arc::new(tokio::sync::Semaphore::new(max_parallel)),
            per_tenant,
            tenants: Mutex::new(HashMap::new()),
        }
    }

    pub async fn acquire(&self, tenant: TenantId) -> Slot {
        let t = {
            let mut m = self.tenants.lock().unwrap_or_else(|p| p.into_inner());
            // Forget the tenants with no build: every permit back and no build about to take
            // one (a waiting or starting build holds a clone of the semaphore).
            m.retain(|_, s| Arc::strong_count(s) > 1 || s.available_permits() < self.per_tenant);
            m.entry(tenant)
                .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(self.per_tenant)))
                .clone()
        };
        // The semaphores are never closed.
        let tenant = t.acquire_owned().await.expect("open semaphore");
        let global = self
            .global
            .clone()
            .acquire_owned()
            .await
            .expect("open semaphore");
        Slot {
            _tenant: tenant,
            _global: global,
        }
    }
}

/// The builder (see the module docs).
pub struct DockerBuilder {
    docker: Docker,
    cfg: BuilderConfig,
    usage: Arc<dyn RuntimeUsageSink>,
    egress: Arc<dyn EgressRegistry>,
    slots: Slots,
    /// The image manifest of the base for the engine's platform.
    base_manifest: OciDigest,
}

fn failed(reason: impl Into<String>) -> RuntimeError {
    RuntimeError::BuildFailed {
        reason: bounded(reason.into()),
    }
}

/// Cuts a reason at [`MAX_BUILD_FAILED_REASON_BYTES`] on a character boundary.
fn bounded(mut s: String) -> String {
    if s.len() > MAX_BUILD_FAILED_REASON_BYTES {
        let mut cut = MAX_BUILD_FAILED_REASON_BYTES;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    s
}

/// What `app.yaml` gives the builder.
#[derive(Debug, PartialEq, Eq)]
pub struct Recipe {
    pub start: String,
    pub build: Option<String>,
}

/// Reads `app.yaml` of the archive: `runtime` must be `node22` and `start` a non-empty
/// string of at most 500 bytes; `build` is optional (at most 500 bytes).
pub fn read_manifest(files: &archive::Unpacked) -> Result<Recipe, String> {
    let entry = files
        .get("app.yaml")
        .ok_or("the archive has no app.yaml at its root")?;
    let text = std::str::from_utf8(&entry.data).map_err(|_| "app.yaml is not UTF-8")?;
    let v = ruum_core::apps::deployments::yaml::parse(text)
        .map_err(|e| format!("app.yaml cannot be read: {e}"))?;
    let runtime = v.get("runtime").and_then(|r| r.as_str());
    if runtime != Some("node22") {
        return Err("app.yaml: runtime must be node22 for this builder".to_owned());
    }
    let field = |name: &str| -> Result<Option<String>, String> {
        match v.get(name) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::String(s)) if s.len() <= 500 && !s.contains('\0') => {
                Ok(Some(s.clone()))
            }
            Some(_) => Err(format!(
                "app.yaml: {name} must be a string of at most 500 bytes"
            )),
        }
    };
    let start = field("start")?
        .filter(|s| !s.trim().is_empty())
        .ok_or("app.yaml: start is required for node22 (the command that listens on $PORT)")?;
    let build = field("build")?.filter(|s| !s.trim().is_empty());
    Ok(Recipe { start, build })
}

/// The most bytes of the source's names, with the folders above them, in the source tar
/// (the same bound as the output's, [`output::MAX_NAME_TOTAL_BYTES`]).
pub const MAX_SOURCE_NAME_TOTAL_BYTES: usize = output::MAX_NAME_TOTAL_BYTES;

/// The most entries of the source tar, folders included (the same bound as the output's,
/// [`output::MAX_ENTRIES`]: a source over it could not come back as an output either).
pub const MAX_SOURCE_ENTRIES: usize = output::MAX_ENTRIES;

/// The bounds of the source tar: its names (folders included), its entries (folders included)
/// and its own size in bytes (`RUUM_BUILD_OUTPUT_MAX`, as for the layer).
#[derive(Clone, Copy, Debug)]
struct SourceBounds {
    names: usize,
    entries: usize,
    bytes: u64,
}

/// The source tar the job unpacks: the checked files of the archive, folders first. The folders
/// above the files are added once each, walking up from a file only to the first folder that is
/// already there, and all names together (folders included) stay under `bounds.names` bytes: a
/// deep path cannot make the builder copy each of its parents for every file (Kimi, run 12).
/// The entries (folders included) stay under `bounds.entries` and the tar itself under
/// `bounds.bytes`, checked as it is written: many short paths with folders of their own cannot
/// make a source tar of headers far larger than the archive (review of Kimi run 12 fixes).
fn source_tar(files: &archive::Unpacked, bounds: SourceBounds) -> Result<Bytes, String> {
    let max_names = bounds.names;
    let mut dirs: std::collections::BTreeSet<&[u8]> = std::collections::BTreeSet::new();
    let mut entries: BTreeMap<&[u8], &Bytes> = BTreeMap::new();
    let mut name_bytes: usize = 0;
    let over = || {
        format!(
            "the archive's paths, with the folders above them, are over {max_names} bytes in all"
        )
    };
    let too_many = || {
        format!(
            "the archive has more than {} entries with the folders above them",
            bounds.entries
        )
    };
    for e in &files.entries {
        let p = e.name.as_bytes();
        name_bytes += p.len();
        if name_bytes > max_names {
            return Err(over());
        }
        let mut end = p.len();
        while let Some(i) = p[..end].iter().rposition(|b| *b == b'/') {
            if !dirs.insert(&p[..i]) {
                // This folder was added with every folder above it.
                break;
            }
            name_bytes += i;
            if name_bytes > max_names {
                return Err(over());
            }
            if dirs.len() + entries.len() >= bounds.entries {
                return Err(too_many());
            }
            end = i;
        }
        entries.insert(p, &e.data);
        if dirs.len() + entries.len() > bounds.entries {
            return Err(too_many());
        }
    }
    let mut w = oci::TarWriter::new();
    let too_big = || {
        format!(
            "the source tar of the archive is over {} bytes",
            bounds.bytes
        )
    };
    for d in &dirs {
        w.dir(d);
        if w.written() as u64 > bounds.bytes {
            return Err(too_big());
        }
    }
    for (p, data) in entries {
        w.file(p, 0o644, data);
        // Checked after each entry: the buffer passes the bound by at most one entry.
        if w.written() as u64 > bounds.bytes {
            return Err(too_big());
        }
    }
    Ok(Bytes::from(w.finish()))
}

/// The state a build's cleanup needs, also when the build future was dropped at its timeout
/// or by the caller: the network to unregister, kept until its unregistration is done.
#[derive(Default)]
struct Trace {
    network: Mutex<Option<String>>,
}

/// Removes the job container and the build network (unregistered first). The network's ID
/// stays in the trace when its unregistration failed, so a later cleanup tries again.
async fn cleanup(
    docker: &Docker,
    cfg: &BuilderConfig,
    egress: &dyn EgressRegistry,
    build: uuid::Uuid,
    trace: &Trace,
) {
    job::remove_container(docker, build).await;
    let registered = trace
        .network
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    if job::remove_network(docker, cfg, egress, build, registered).await {
        *trace.network.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

/// The end of a build, after every outcome: the cleanup and the usage record. When the build
/// future is dropped before [`Finish::run`] completes (the caller cancelled, the task was
/// aborted), `Drop` spawns them on the runtime, so the job, the network and the egress
/// registration never outlive a cancelled build (card point 9). It holds the build's slot until
/// the cleanup is done.
struct Finish {
    docker: Docker,
    cfg: BuilderConfig,
    egress: Arc<dyn EgressRegistry>,
    usage: Arc<dyn RuntimeUsageSink>,
    build: uuid::Uuid,
    identity: ruum_core::runtime::WorkloadIdentity,
    begun: Instant,
    trace: Arc<Trace>,
    slot: Option<Slot>,
    done: bool,
}

impl Finish {
    /// Cleans up and records the usage; the time the build took.
    async fn run(&mut self) -> Duration {
        cleanup(
            &self.docker,
            &self.cfg,
            self.egress.as_ref(),
            self.build,
            &self.trace,
        )
        .await;
        let took = self.begun.elapsed().max(Duration::from_millis(1));
        self.usage.record(RuntimeUsage::Build {
            identity: self.identity,
            took,
        });
        self.done = true;
        self.slot = None;
        took
    }
}

impl Drop for Finish {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let docker = self.docker.clone();
        let cfg = self.cfg.clone();
        let egress = self.egress.clone();
        let usage = self.usage.clone();
        let (build, identity, begun) = (self.build, self.identity, self.begun);
        let trace = self.trace.clone();
        let slot = self.slot.take();
        let record = move || {
            usage.record(RuntimeUsage::Build {
                identity,
                took: begun.elapsed().max(Duration::from_millis(1)),
            });
        };
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                rt.spawn(async move {
                    cleanup(&docker, &cfg, egress.as_ref(), build, &trace).await;
                    record();
                    drop(slot);
                });
            }
            Err(_) => {
                tracing::warn!(build = %build, "a build was dropped outside a runtime: its job and network are left");
                record();
            }
        }
    }
}

/// What the build has ready when its timeout ends: the push and the manifest to commit.
struct Ready {
    push: registry::RepositoryPush,
    manifest: Bytes,
}

impl DockerBuilder {
    /// Connects to the engine, checks the configuration and the embedded base index, and picks
    /// the base image of the engine's platform.
    pub async fn connect(
        cfg: BuilderConfig,
        usage: Arc<dyn RuntimeUsageSink>,
        egress: Arc<dyn EgressRegistry>,
    ) -> Result<Self, RuntimeError> {
        cfg.check()?;
        let unavailable = |what: &str, e: bollard::errors::Error| RuntimeError::Unavailable {
            reason: format!("{what}: {e}"),
        };
        let timeout = cfg.docker.api_timeout.as_secs().max(1);
        let host = &cfg.docker.docker_host;
        let docker = if let Some(path) = host.strip_prefix("unix://") {
            Docker::connect_with_unix(path, timeout, bollard::API_DEFAULT_VERSION)
        } else if host.starts_with("tcp://") || host.starts_with("http://") {
            Docker::connect_with_http(host, timeout, bollard::API_DEFAULT_VERSION)
        } else {
            return Err(RuntimeError::Internal {
                reason: "RUUM_DOCKER_HOST must start with unix://, tcp:// or http://".to_owned(),
            });
        }
        .map_err(|e| unavailable("the Docker engine", e))?
        .negotiate_version()
        .await
        .map_err(|e| unavailable("the Docker engine", e))?;
        let info = docker
            .info()
            .await
            .map_err(|e| unavailable("the Docker engine", e))?;
        if cfg.docker.runtime == crate::RuntimeKind::Runsc
            && !info
                .runtimes
                .as_ref()
                .is_some_and(|r| r.contains_key("runsc"))
        {
            return Err(RuntimeError::Unavailable {
                reason: "RUUM_RUNTIME_DRIVER=runsc, but the engine has no runtime runsc".to_owned(),
            });
        }
        let arch = oci::engine_architecture(info.architecture.as_deref().unwrap_or(""))?;
        let index = if cfg.base_index == NODE22_BASE_INDEX {
            if OciDigest::of(EMBEDDED_INDEX).as_str() != NODE22_BASE_INDEX {
                return Err(RuntimeError::Internal {
                    reason: "the embedded base index does not match its pin".to_owned(),
                });
            }
            Bytes::from_static(EMBEDDED_INDEX)
        } else {
            // Another pin: the index comes from the local registry, checked against the pin.
            let p = registry::RepositoryPush::new(
                &cfg.docker.registry_url,
                String::new(),
                cfg.base_repository.clone(),
                cfg.credential.clone(),
                cfg.token_realm.as_deref(),
            )?;
            p.base_manifest(&OciDigest::parse(&cfg.base_index)?)
                .await?
                .1
        };
        let d = oci::select_manifest(&index, arch)?;
        let base_manifest = OciDigest::parse(&d.digest)?;
        Ok(Self {
            docker,
            slots: Slots::new(cfg.max_parallel, cfg.per_tenant),
            cfg,
            usage,
            egress,
            base_manifest,
        })
    }

    /// The engine reference of the base image (the job runs from it).
    pub fn base_image(&self) -> String {
        format!(
            "{}/{}@{}",
            self.cfg.docker.registry, self.cfg.base_repository, self.base_manifest
        )
    }

    /// Pulls the base image into the engine unless it is there (only a local registry
    /// reference, as I-4's socket proxy requires).
    async fn ensure_base(&self) -> Result<(), RuntimeError> {
        use futures::StreamExt;
        let reference = self.base_image();
        if self.docker.inspect_image(&reference).await.is_ok() {
            return Ok(());
        }
        let opts = bollard::query_parameters::CreateImageOptionsBuilder::new()
            .from_image(&reference)
            .build();
        let mut pull = self.docker.create_image(Some(opts), None, None);
        while let Some(item) = pull.next().await {
            item.map_err(|e| RuntimeError::Unavailable {
                reason: format!("pulling the base image: {e}"),
            })?;
        }
        Ok(())
    }

    /// Removes the images of builds that the engine holds (the driver pulled them), when no
    /// container uses them: images labelled [`LABEL_BUILD`] and [`crate::LABEL_MANAGED`] =
    /// [`MANAGED_BY`], plus every extra label of the configuration, created more than `age`
    /// ago. The count of removed images.
    pub async fn prune_images(&self, age: Duration) -> Result<usize, RuntimeError> {
        let mut labels = vec![
            LABEL_BUILD.to_owned(),
            format!("{}={MANAGED_BY}", crate::LABEL_MANAGED),
        ];
        for (k, v) in &self.cfg.docker.extra_labels {
            labels.push(format!("{k}={v}"));
        }
        let filters = HashMap::from([("label".to_owned(), labels)]);
        let opts = bollard::query_parameters::ListImagesOptionsBuilder::new()
            .filters(&filters)
            .build();
        let images =
            self.docker
                .list_images(Some(opts))
                .await
                .map_err(|e| RuntimeError::Unavailable {
                    reason: format!("listing build images: {e}"),
                })?;
        let before = time::OffsetDateTime::now_utc().unix_timestamp() - age.as_secs() as i64;
        let mut removed = 0;
        for i in images {
            // `containers` is -1 unless the engine counts it (shared-size option): the delete
            // below is not forced, so the engine refuses an image a container uses.
            if i.created > before || i.containers > 0 {
                continue;
            }
            let opts = bollard::query_parameters::RemoveImageOptionsBuilder::new()
                .force(false)
                .build();
            if self
                .docker
                .remove_image(&i.id, Some(opts), None)
                .await
                .is_ok()
            {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// The build up to its manifest push; the caller times it out, commits the manifest and
    /// cleans up.
    async fn run(
        &self,
        input: &BuildInput,
        build: uuid::Uuid,
        trace: &Trace,
        log: &Mutex<job::LogTail>,
    ) -> Result<Ready, RuntimeError> {
        let limits = archive::Limits {
            max_total_bytes: self.cfg.output_max.min(input.limits.disk_bytes),
            max_entry_bytes: self.cfg.output_max.min(input.limits.disk_bytes),
            max_files: 100_000,
            max_ratio: 200,
        };
        // Unpacking and the source tar are CPU work over up to the archive limit: off the async
        // workers, so the build's timeout still ends the build.
        let archive_bytes = input.archive.clone();
        let bounds = SourceBounds {
            names: MAX_SOURCE_NAME_TOTAL_BYTES,
            entries: MAX_SOURCE_ENTRIES,
            bytes: self.cfg.output_max,
        };
        let (recipe, source) = blocking(move || {
            let files = archive::unpack(&archive_bytes, &limits).map_err(|r| match r {
                archive::Refusal::Hostile { entry, reason } => failed(format!(
                    "the archive entry {:?} is refused: {reason}",
                    entry.escape_debug().take(200).collect::<String>()
                )),
                archive::Refusal::Corrupt { reason } => {
                    failed(format!("the archive cannot be read: {reason}"))
                }
                archive::Refusal::Limit { what, limit } => {
                    failed(format!("the archive is over the limit of {what} ({limit})"))
                }
            })?;
            let recipe = read_manifest(&files).map_err(failed)?;
            let source = source_tar(&files, bounds).map_err(failed)?;
            Ok::<_, RuntimeError>((recipe, source))
        })
        .await??;
        self.ensure_base().await?;
        let network = job::create_network(
            &self.docker,
            &self.cfg,
            self.egress.as_ref(),
            input,
            build,
            &trace.network,
        )
        .await?;
        let end = job::run(
            &self.docker,
            &self.cfg,
            input,
            build,
            &self.base_image(),
            &network,
            source,
            recipe.build.as_deref(),
            log,
        )
        .await?;
        let output = match end {
            job::JobEnd::OutputOverCap => {
                return Err(failed(format!(
                    "the build's output is over the cap of {} bytes",
                    self.cfg.output_max
                )));
            }
            job::JobEnd::Exited { code, output } if code != 0 => {
                let _ = output;
                return Err(failed(format!(
                    "the build exited with code {code}: {}",
                    tail_text(log)
                )));
            }
            job::JobEnd::Exited { output, .. } => output,
        };
        // The job's work is done: free its sandbox before the assembly (the network's ID stays
        // in the trace when its unregistration failed: the final cleanup tries again).
        cleanup(&self.docker, &self.cfg, self.egress.as_ref(), build, trace).await;
        // Reading the output and making the layer are CPU work over up to the output cap: off
        // the async workers, so the build's timeout still ends the build.
        let cap = self.cfg.output_max;
        let out = blocking(move || output::read_output(output, cap))
            .await?
            .map_err(|e| failed(format!("the build's output is refused: {e}")))?;
        {
            let mut l = log.lock().unwrap_or_else(|p| p.into_inner());
            for (name, why) in out.dropped.iter().take(100) {
                l.note(&format!(
                    "ruum-build: dropped output entry \"{name}\": {why}"
                ));
            }
            if out.dropped_total > 100 {
                l.note(&format!(
                    "ruum-build: dropped {} more output entries",
                    out.dropped_total - 100
                ));
            }
        }
        let push = registry::RepositoryPush::new(
            &self.cfg.docker.registry_url,
            repository(&input.identity),
            self.cfg.base_repository.clone(),
            self.cfg.credential.clone(),
            self.cfg.token_realm.as_deref(),
        )?;
        let (_, base_manifest) = push.base_manifest(&self.base_manifest).await?;
        let base = oci::parse_manifest(&base_manifest)?;
        let base_config = push
            .base_blob(&OciDigest::parse(&base.config.digest)?)
            .await?;
        let layer = blocking(move || oci::build_layer(&out.entries, cap))
            .await?
            .map_err(|e| failed(format!("the build's output is refused: {e}")))?;
        let mut labels: BTreeMap<String, String> = self.cfg.docker.extra_labels.clone();
        labels.insert(
            crate::LABEL_APP.to_owned(),
            input.identity.app.0.to_string(),
        );
        labels.insert(
            crate::LABEL_TENANT.to_owned(),
            input.identity.tenant.0.to_string(),
        );
        labels.insert(LABEL_BUILD.to_owned(), build.to_string());
        labels.insert(crate::LABEL_MANAGED.to_owned(), MANAGED_BY.to_owned());
        let config = oci::image_config(&base_config, &layer, &recipe.start, &labels)?;
        let manifest = oci::image_manifest(&base.layers, &config, &layer);
        for d in &base.layers {
            push.bring_base_blob(&OciDigest::parse(&d.digest)?, d.size)
                .await?;
        }
        push.push_blob(&layer.digest, layer.gz.clone()).await?;
        push.push_blob(&OciDigest::of(&config), config.clone())
            .await?;
        Ok(Ready { push, manifest })
    }
}

/// Runs CPU work on the blocking pool.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, RuntimeError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| RuntimeError::Internal {
            reason: format!("the build's assembly: {e}"),
        })
}

fn tail_text(log: &Mutex<job::LogTail>) -> String {
    let lines = log.lock().unwrap_or_else(|p| p.into_inner()).lines();
    let mut out = String::new();
    for (_, l) in lines.iter().rev() {
        if out.len() + l.len() + 1 > MAX_BUILD_FAILED_REASON_BYTES - 200 {
            break;
        }
        out = if out.is_empty() {
            l.clone()
        } else {
            format!("{l}\n{out}")
        };
    }
    out
}

#[async_trait]
impl Builder for DockerBuilder {
    fn name(&self) -> &str {
        match self.cfg.docker.runtime {
            crate::RuntimeKind::Runc => "docker-node22",
            crate::RuntimeKind::Runsc => "docker-runsc-node22",
        }
    }

    async fn build(&self, input: BuildInput) -> Result<BuildOutput, RuntimeError> {
        input.validate()?;
        if input.runtime != "node22" {
            return Err(RuntimeError::UnsupportedRuntime {
                runtime: input.runtime,
            });
        }
        let slot = self.slots.acquire(input.identity.tenant).await;
        let build = uuid::Uuid::now_v7();
        let trace = Arc::new(Trace::default());
        // From here on, every outcome (also a drop of this future) cleans up and records.
        let mut finish = Finish {
            docker: self.docker.clone(),
            cfg: self.cfg.clone(),
            egress: self.egress.clone(),
            usage: self.usage.clone(),
            build,
            identity: input.identity,
            begun: Instant::now(),
            trace: trace.clone(),
            slot: Some(slot),
            done: false,
        };
        let log = Mutex::new(job::LogTail::new(
            MAX_BUILD_LOG_LINES,
            MAX_BUILD_LOG_LINE_BYTES,
        ));
        let ready = match tokio::time::timeout(input.timeout, self.run(&input, build, &trace, &log))
            .await
        {
            Ok(r) => r,
            Err(_) => Err(RuntimeError::BuildTimedOut),
        };
        // The commit point, outside the timeout: a manifest is pushed whole or not at all by
        // this build, never cut by the timeout after the registry stored it.
        let result = match ready {
            Ok(r) => r
                .push
                .push_manifest(registry::OCI_MANIFEST, r.manifest)
                .await
                .inspect(|digest| {
                    log.lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .note(&format!("ruum-build: image {digest}"));
                }),
            Err(e) => Err(e),
        };
        let took = finish.run().await;
        let digest = result?;
        let logs = log
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .lines()
            .into_iter()
            .map(|(at, text)| BuildLogLine { at, text })
            .collect();
        Ok(BuildOutput {
            image: ImageRef::Oci(digest),
            logs,
            took,
            routes: BUILD_ROUTES.to_vec(),
            limits: input.limits,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn one_tenant_cannot_hold_every_slot() {
        let slots = Arc::new(Slots::new(3, 2));
        let a = TenantId::new();
        let b = TenantId::new();
        let a1 = slots.acquire(a).await;
        let a2 = slots.acquire(a).await;
        // A third build of tenant A waits for A's share.
        let s = slots.clone();
        let mut a3 = tokio::spawn(async move { s.acquire(a).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!a3.is_finished(), "tenant A got a third slot");
        // Tenant B still gets the slot that is left.
        let b1 = tokio::time::timeout(Duration::from_secs(1), slots.acquire(b))
            .await
            .expect("tenant B gets a slot while A waits");
        drop(a1);
        let a3 = tokio::time::timeout(Duration::from_secs(1), &mut a3)
            .await
            .expect("A's third build starts when one of A's ends")
            .expect("task");
        drop((a2, a3, b1));
    }

    #[test]
    fn the_manifest_needs_node22_and_start() {
        let files = |yaml: &str| archive::Unpacked {
            entries: vec![archive::Entry {
                name: "app.yaml".to_owned(),
                data: Bytes::from(yaml.to_owned()),
            }],
        };
        assert_eq!(
            read_manifest(&files(
                "runtime: node22\nstart: node server.js\nbuild: npm run build\n"
            )),
            Ok(Recipe {
                start: "node server.js".to_owned(),
                build: Some("npm run build".to_owned())
            })
        );
        assert!(read_manifest(&files("runtime: node22\n")).is_err());
        assert!(read_manifest(&files("runtime: static\nstart: x\n")).is_err());
        assert!(read_manifest(&files("runtime: node22\nstart: [a]\n")).is_err());
        assert!(read_manifest(&archive::Unpacked::default()).is_err());
    }

    fn unpacked(names: impl IntoIterator<Item = String>) -> archive::Unpacked {
        archive::Unpacked {
            entries: names
                .into_iter()
                .map(|name| archive::Entry {
                    name,
                    data: Bytes::from_static(b"x"),
                })
                .collect(),
        }
    }

    /// The bounds of the build, with a name bound of `names` bytes.
    fn names_bound(names: usize) -> SourceBounds {
        SourceBounds {
            names,
            entries: MAX_SOURCE_ENTRIES,
            bytes: u64::MAX,
        }
    }

    /// The folder entries of a source tar, in order.
    fn tar_dirs(tar: &[u8]) -> Vec<String> {
        let out = output::read_output(Bytes::copy_from_slice(tar), u64::MAX).expect("a valid tar");
        out.entries
            .iter()
            .filter(|e| matches!(e.kind, output::Kind::Dir))
            .map(|e| String::from_utf8(e.path.clone()).expect("UTF-8"))
            .collect()
    }

    #[test]
    fn the_source_tar_adds_each_folder_once() {
        let files = unpacked(
            ["app.yaml", "src/a/b.js", "src/a/c.js", "src/d.js"]
                .into_iter()
                .map(str::to_owned),
        );
        let tar =
            source_tar(&files, names_bound(MAX_SOURCE_NAME_TOTAL_BYTES)).expect("small source");
        assert_eq!(tar_dirs(&tar), ["src", "src/a"]);
    }

    #[test]
    fn deep_source_paths_cannot_amplify_the_source_tar() {
        // A small cap: the name fits, its folders do not.
        let files = unpacked(["a/b/c/d/e/f".to_owned()]);
        assert!(source_tar(&files, names_bound(11)).is_err());
        assert!(source_tar(&files, names_bound(11 + 9 + 7 + 5 + 3 + 1)).is_ok());
        // Each name is under the archive's 1024-byte cap, but its 500 folders are distinct from
        // every other name's: about 250 KiB of folder names per file. 600 such files (600 KiB of
        // names in the archive) would make about 150 MiB of folder names in the source tar.
        let deep = |i: usize| format!("d{i:04}/{}f", "x/".repeat(500));
        let files = unpacked((0..600).map(deep));
        let names: usize = files.entries.iter().map(|e| e.name.len()).sum();
        assert!(names < 1024 * 1024);
        let err = source_tar(&files, names_bound(MAX_SOURCE_NAME_TOTAL_BYTES))
            .expect_err("folder names over the cap are refused");
        assert!(err.contains("over"), "{err}");
    }

    /// Many short paths, each with folders of its own: the names stay far under their bound,
    /// but the folders count against the entry bound (100,000 files with 20 folders each would
    /// make a source tar of about 1 GiB of headers from an archive of a few MiB).
    #[test]
    fn wide_source_paths_count_against_the_entry_bound() {
        let wide = |i: usize| format!("{i:05}/{}f", "a/".repeat(19));
        // 25,000 files with 20 folders each: 525,000 entries, about 13 MiB of names.
        let files = unpacked((0..25_000).map(wide));
        let err = source_tar(&files, names_bound(MAX_SOURCE_NAME_TOTAL_BYTES))
            .expect_err("over the entry bound");
        assert!(err.contains("entries with the folders"), "{err}");
        // The bound is exact: 20 such files are 420 entries.
        let files = unpacked((0..20).map(wide));
        let entries = |entries| SourceBounds {
            entries,
            ..names_bound(MAX_SOURCE_NAME_TOTAL_BYTES)
        };
        assert!(source_tar(&files, entries(420)).is_ok());
        assert!(source_tar(&files, entries(419)).is_err());
    }

    /// The source tar is held to its byte bound as it is written.
    #[test]
    fn the_source_tar_is_held_to_its_byte_bound() {
        // 2,000 files of one byte: 2,000 headers and data blocks, about 2 MiB.
        let files = unpacked((0..2_000).map(|i| format!("f{i:04}")));
        let bounds = |bytes| SourceBounds {
            bytes,
            ..names_bound(MAX_SOURCE_NAME_TOTAL_BYTES)
        };
        let tar = source_tar(&files, bounds(4 * 1024 * 1024)).expect("under the bound");
        assert_eq!(tar.len(), 2_000 * 1024 + 1024);
        let err = source_tar(&files, bounds(1024 * 1024)).expect_err("over the byte bound");
        assert!(err.contains("source tar of the archive is over"), "{err}");
    }

    #[test]
    fn a_reason_is_bounded_on_a_character_boundary() {
        let s = bounded("ä".repeat(MAX_BUILD_FAILED_REASON_BYTES));
        assert!(s.len() <= MAX_BUILD_FAILED_REASON_BYTES);
        assert!(s.chars().all(|c| c == 'ä'));
    }

    #[test]
    fn the_job_environment_holds_no_credential() {
        let mut d = DockerConfig::new("unix:///x", "127.0.0.1:5000", "/tmp".into());
        d.routes.insert(
            ruum_core::runtime::Route::EgressProxy,
            crate::SharedContainer {
                container: "egress".to_owned(),
                alias: "ruum-egress".to_owned(),
                port: 3128,
            },
        );
        let mut cfg = BuilderConfig::new(d);
        cfg.credential = Some(RegistryCredential {
            username: "canary-user".to_owned(),
            password: ruum_core::runtime::SecretValue::new("canary-pass".to_owned()),
        });
        let env = job::job_env(&cfg, 10, Some("npm run build"));
        assert!(env.iter().all(|e| !e.contains("canary")));
        assert!(env.contains(&"HTTPS_PROXY=http://ruum-egress:3128".to_owned()));
    }
}
