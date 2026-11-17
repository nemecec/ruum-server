//! The node22 builder on a Docker engine (card R-2b). `RUUM_RUNTIME_DRIVER=docker` (default)
//! runs it with runc, `RUUM_RUNTIME_DRIVER=runsc` with gVisor.
//!
//! - `the_builder_passes_the_conformance_suite`: `run_builder` of `ruum-core` against
//!   [`DockerBuilder`] (acceptance 1), with the S-37 hex check and the canary check of the built
//!   image's config and layer.
//! - `the_builder_holds`: the image runs on the driver and lands in its own repository only
//!   (acceptance 2), the hostile build (3: with `RUUM_SANDBOX_FULL=1`, R-2e's egress proxy and the
//!   dev stack's targets; output cap, disk limit, fork bomb and hostile names always), the same
//!   hardening as the driver (4), usage (6), cleanup (7). Acceptance 5 is the recording
//!   stand-in registry of `src/builder/registry.rs`.
//!
//! Every run labels what it makes `ruum.test=r2b-<random>` and removes only that (backlog T-53),
//! so two runs on one engine do not touch each other. The base image is mirrored into the run's
//! own `registry:2` from Docker Hub (the platform image of the pinned index only; blobs are
//! cached in `CARGO_TARGET_TMPDIR`). `RUUM_BUILD_CASES` (comma-separated) runs some cases only,
//! for the mutation checks; with `CI` set it is refused (T-26).

mod common;

use async_trait::async_trait;
use bytes::Bytes;
use common::{REGISTRY_IMAGE, docker, docker_host, engine_or_skip, runtime_kind, show};
use futures::future::BoxFuture;
use ruum_core::ids::{NodeId, TenantId};
use ruum_core::runtime::conformance::{self, BuildTiming, BuilderFixture};
use ruum_core::runtime::{
    BuildInput, Builder, EnvVar, Environment, HealthCheck, ImageRef, InstanceState, Limits,
    MemoryUsage, OciDigest, Route, RuntimeDriver, RuntimeError, RuntimeUsage, SecretValue,
    ServiceSpec, WorkloadIdentity, WorkloadKind, WorkloadSpec,
};
use ruum_runtime::builder::{MANAGED_BY, oci};
use ruum_runtime::{
    BuilderConfig, DockerBuilder, DockerConfig, DockerDriver, EgressNetwork, EgressRegistry,
    HttpEgressRegistry, NoEgress, RegistryCredential, RuntimeKind, SharedContainer,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Read;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const LABEL_KEY: &str = "ruum.test";
const INDEX: &[u8] = include_bytes!("../src/builder/node22-index.json");
const BASE_REPO: &str = "ruum-base/node22";

fn full() -> bool {
    std::env::var("RUUM_SANDBOX_FULL").as_deref() == Ok("1")
}

fn wanted(case: &str) -> bool {
    match std::env::var("RUUM_BUILD_CASES") {
        Ok(v) if !v.is_empty() => v.split(',').any(|c| c == case),
        _ => true,
    }
}

fn refuse_filter_under_ci() {
    if std::env::var_os("CI").is_some() {
        assert!(
            std::env::var("RUUM_BUILD_CASES")
                .unwrap_or_default()
                .is_empty(),
            "RUUM_BUILD_CASES skips cases; it must not be set when CI is set (T-26)"
        );
    }
}

fn rand_id() -> String {
    format!("{:012x}", rand::random::<u64>() & 0xffff_ffff_ffff)
}

// ------------------------------------------------------------------------------ the run

/// The egress registry of the tests: it passes each call on and records which networks are
/// registered, so a test sees a registration that a build left.
struct Recording {
    inner: Arc<dyn EgressRegistry>,
    active: Mutex<HashSet<String>>,
    seen: Mutex<Vec<EgressNetwork>>,
}

#[async_trait]
impl EgressRegistry for Recording {
    async fn register(&self, n: &EgressNetwork) -> Result<(), RuntimeError> {
        self.inner.register(n).await?;
        self.active.lock().unwrap().insert(n.network.clone());
        self.seen.lock().unwrap().push(n.clone());
        Ok(())
    }
    async fn unregister(&self, network: &str) -> Result<(), RuntimeError> {
        self.inner.unregister(network).await?;
        self.active.lock().unwrap().remove(network);
        Ok(())
    }
}

struct Run {
    /// `r2b-<random>`: the value of [`LABEL_KEY`] on everything of this run.
    label: String,
    registry: String,
    registry_container: String,
    base: OciDigest,
    shared_net: String,
    egress_container: String,
    egress: Arc<Recording>,
    canaries: Vec<String>,
    credential: RegistryCredential,
    dir: PathBuf,
    real_egress: bool,
}

impl Drop for Run {
    fn drop(&mut self) {
        self.cleanup();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()
        .expect("http client")
}

impl Run {
    fn label_arg(&self) -> String {
        format!("{LABEL_KEY}={}", self.label)
    }

    /// Removes the containers, networks and images of this run only.
    fn cleanup(&self) {
        let filter = format!("label={}", self.label_arg());
        if let Ok(ids) = docker(&["ps", "-aq", "--filter", &filter])
            && !ids.is_empty()
        {
            let mut args = vec!["rm", "-f", "-v"];
            args.extend(ids.lines());
            let _ = docker(&args);
        }
        if let Ok(ids) = docker(&["network", "ls", "-q", "--filter", &filter]) {
            for id in ids.lines().filter(|l| !l.is_empty()) {
                let _ = docker(&["network", "rm", id]);
            }
        }
        if let Ok(ids) = docker(&["images", "-q", "--filter", &filter]) {
            for id in ids.lines().filter(|l| !l.is_empty()) {
                let _ = docker(&["rmi", "-f", id]);
            }
        }
        let _ = docker(&["rmi", &self.base_ref()]);
    }

    fn base_ref(&self) -> String {
        format!("{}/{BASE_REPO}@{}", self.registry, self.base)
    }

    fn registry_url(&self) -> String {
        format!("http://{}", self.registry)
    }

    async fn start(real_egress: bool) -> Result<Run, String> {
        let label = format!("r2b-{}", rand_id());
        let label_arg = format!("{LABEL_KEY}={label}");
        let dir = std::env::temp_dir().join(&label);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let reg = format!("{label}-registry");
        docker(&[
            "run",
            "-d",
            "--name",
            &reg,
            "--label",
            &label_arg,
            "-p",
            "127.0.0.1::5000",
            REGISTRY_IMAGE,
        ])?;
        let port = docker(&["port", &reg, "5000/tcp"])?;
        let port = port
            .lines()
            .next()
            .and_then(|l| l.rsplit(':').next())
            .ok_or("registry port")?
            .to_owned();
        let registry = format!("127.0.0.1:{port}");
        for _ in 0..100 {
            if TcpStream::connect(&registry).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let password = format!("canary-push-{}", uuid::Uuid::now_v7().simple());
        let credential = RegistryCredential {
            username: format!("canary-user-{}", uuid::Uuid::now_v7().simple()),
            password: SecretValue::new(password.clone()),
        };
        let mut run = Run {
            label: label.clone(),
            registry,
            registry_container: reg,
            base: OciDigest::of(b"not yet"),
            shared_net: format!("{label}-shared"),
            egress_container: format!("{label}-egress"),
            egress: Arc::new(Recording {
                inner: Arc::new(NoEgress),
                active: Mutex::new(HashSet::new()),
                seen: Mutex::new(Vec::new()),
            }),
            canaries: vec![password, credential.username.clone()],
            credential,
            dir,
            real_egress: false,
        };
        run.base = mirror_base(&run.registry).await?;
        docker(&[
            "network",
            "create",
            "--internal",
            "--label",
            &label_arg,
            &run.shared_net,
        ])?;
        if real_egress {
            let token = run.start_real_egress()?;
            run.canaries.push(token);
            run.real_egress = true;
        } else {
            // A stand-in: nothing listens on 3128; the hello builds need no network.
            docker(&[
                "run",
                "-d",
                "--name",
                &run.egress_container,
                "--label",
                &label_arg,
                "--network",
                &run.shared_net,
                "--read-only",
                "--cap-drop",
                "ALL",
                "--user",
                "10001:10001",
                "--entrypoint",
                "node",
                &run.base_ref(),
                "-e",
                "setInterval(() => {}, 1 << 30)",
            ])?;
        }
        Ok(run)
    }

    /// R-2e's egress proxy (`deploy/images/ruum-egress`), as the sandbox suite starts it; the
    /// control token (a canary: the builder's own credential).
    fn start_real_egress(&mut self) -> Result<String, String> {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        let dockerfile = format!("{root}/deploy/images/ruum-egress/Dockerfile");
        let image = format!("ruum-egress:{}", self.label);
        let label_arg = self.label_arg();
        docker(&[
            "build",
            "-q",
            "--label",
            &label_arg,
            "-f",
            &dockerfile,
            "-t",
            &image,
            root,
        ])?;
        let out = format!("{}-out", self.label);
        docker(&["network", "create", "--label", &label_arg, &out])?;
        let ctl = self.dir.join("ctl");
        std::fs::create_dir_all(&ctl).map_err(|e| e.to_string())?;
        let token = format!(
            "canary-egress-{:032x}{:032x}",
            rand::random::<u128>(),
            rand::random::<u128>()
        );
        let token_file = self.dir.join("token");
        std::fs::write(&token_file, &token).map_err(|e| e.to_string())?;
        let id = |flag: &str| {
            std::process::Command::new("id")
                .arg(flag)
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .unwrap_or_default()
        };
        let user = format!("{}:{}", id("-u"), id("-g"));
        let ctl_mount = format!("{}:/run/egress", ctl.display());
        let token_mount = format!("{}:/run/egress-token/token:ro", token_file.display());
        let deny_env = format!("EGRESS_DENY={}", egress_deny());
        docker(&[
            "run",
            "-d",
            "--name",
            &self.egress_container,
            "--label",
            &label_arg,
            "--network",
            &out,
            "--read-only",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--user",
            &user,
            "--sysctl",
            "net.ipv4.ip_forward=0",
            "--sysctl",
            "net.ipv6.conf.all.forwarding=0",
            "-v",
            &ctl_mount,
            "-v",
            &token_mount,
            "-e",
            "RUUM_EGRESS_CONTROL=unix:/run/egress/control.sock",
            "-e",
            "RUUM_EGRESS_CONTROL_TOKEN_FILE=/run/egress-token/token",
            "-e",
            &deny_env,
            &image,
        ])?;
        let sock = ctl.join("control.sock");
        for _ in 0..100 {
            if sock.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if !sock.exists() {
            return Err(format!(
                "the egress proxy made no control socket: {}",
                docker(&["logs", &self.egress_container]).unwrap_or_default()
            ));
        }
        let registry = HttpEgressRegistry::new(
            &format!("unix:{}", sock.display()),
            SecretValue::new(token.clone()),
        )
        .map_err(|e| e.to_string())?;
        self.egress = Arc::new(Recording {
            inner: Arc::new(registry),
            active: Mutex::new(HashSet::new()),
            seen: Mutex::new(Vec::new()),
        });
        Ok(token)
    }

    fn docker_config(&self) -> DockerConfig {
        let mut c = DockerConfig::new(docker_host(), self.registry.clone(), self.dir.join("state"));
        c.runtime = runtime_kind();
        c.extra_labels
            .insert(LABEL_KEY.to_owned(), self.label.clone());
        c.stop_timeout = Duration::from_secs(5);
        c
    }

    fn builder_config(&self) -> BuilderConfig {
        let mut d = self.docker_config();
        d.routes.insert(
            Route::EgressProxy,
            SharedContainer {
                container: self.egress_container.clone(),
                alias: "ruum-egress".to_owned(),
                port: 3128,
            },
        );
        let mut b = BuilderConfig::new(d);
        b.credential = Some(self.credential.clone());
        b
    }

    async fn builder(
        &self,
        usage: Arc<MemoryUsage>,
        tweak: impl FnOnce(&mut BuilderConfig),
    ) -> Arc<DockerBuilder> {
        let mut cfg = self.builder_config();
        tweak(&mut cfg);
        Arc::new(
            DockerBuilder::connect(cfg, usage, self.egress.clone())
                .await
                .expect("the builder connects"),
        )
    }

    async fn driver(&self, usage: Arc<MemoryUsage>) -> Arc<DockerDriver> {
        Arc::new(
            DockerDriver::connect(self.docker_config(), usage, Arc::new(NoEgress))
                .await
                .expect("the driver connects"),
        )
    }

    /// The containers and networks of this run's builds that are left.
    fn leftovers(&self) -> (Vec<String>, Vec<String>) {
        let f1 = format!("label={}", self.label_arg());
        // The builder's own objects (an app container inherits the image's labels, `ruum.build`
        // among them, but the driver sets `ruum.managed=ruum-runtime`).
        let f2 = format!("label=ruum.managed={MANAGED_BY}");
        let list = |args: &[&str]| -> Vec<String> {
            docker(args)
                .unwrap_or_default()
                .lines()
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect()
        };
        (
            list(&["ps", "-aq", "--filter", &f1, "--filter", &f2]),
            list(&["network", "ls", "-q", "--filter", &f1, "--filter", &f2]),
        )
    }

    async fn catalog(&self) -> BTreeSet<String> {
        let v: Value = http()
            .get(format!("{}/v2/_catalog", self.registry_url()))
            .send()
            .await
            .expect("catalog")
            .json()
            .await
            .expect("catalog json");
        v["repositories"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|r| r.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Copies the base image of the engine's platform (the pinned index's entry) from Docker Hub
/// into the run's registry, byte for byte. Blobs are cached between runs.
async fn mirror_base(registry: &str) -> Result<OciDigest, String> {
    let arch = docker(&["info", "--format", "{{.Architecture}}"])?;
    let arch = oci::engine_architecture(&arch).map_err(|e| e.to_string())?;
    let d = oci::select_manifest(INDEX, arch).map_err(|e| e.to_string())?;
    let digest = OciDigest::parse(&d.digest).map_err(|e| e.to_string())?;
    let c = http();
    #[derive(serde::Deserialize)]
    struct Tok {
        token: String,
    }
    let tok: Tok = c
        .get("https://auth.docker.io/token?service=registry.docker.io&scope=repository:library/node:pull")
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let hub = "https://registry-1.docker.io/v2/library/node";
    let manifest = c
        .get(format!("{hub}/manifests/{digest}"))
        .bearer_auth(&tok.token)
        .header("Accept", &d.media_type)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;
    if OciDigest::of(&manifest) != digest {
        return Err("the base manifest from Docker Hub does not match the pin".to_owned());
    }
    let m = oci::parse_manifest(&manifest).map_err(|e| e.to_string())?;
    let cache = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("r2b-blobs");
    std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    let local = format!("http://{registry}/v2/{BASE_REPO}");
    for b in std::iter::once(&m.config).chain(m.layers.iter()) {
        let file = cache.join(b.digest.replace(':', "-"));
        let data = match std::fs::read(&file) {
            Ok(d) if OciDigest::of(&d).as_str() == b.digest => Bytes::from(d),
            _ => {
                let d = c
                    .get(format!("{hub}/blobs/{}", b.digest))
                    .bearer_auth(&tok.token)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?
                    .bytes()
                    .await
                    .map_err(|e| e.to_string())?;
                if OciDigest::of(&d).as_str() != b.digest {
                    return Err(format!("blob {} from Docker Hub does not match", b.digest));
                }
                let _ = std::fs::write(&file, &d);
                d
            }
        };
        let r = c
            .post(format!("{local}/blobs/uploads/"))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let loc = r
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .ok_or("no upload location")?
            .to_owned();
        let loc = if loc.starts_with('/') {
            format!("http://{registry}{loc}")
        } else {
            loc
        };
        let sep = if loc.contains('?') { '&' } else { '?' };
        let r = c
            .put(format!("{loc}{sep}digest={}", b.digest))
            .body(data)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !r.status().is_success() {
            return Err(format!("mirroring blob {}: {}", b.digest, r.status()));
        }
    }
    let r = c
        .put(format!("{local}/manifests/{digest}"))
        .header("Content-Type", &d.media_type)
        .body(manifest)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("mirroring the base manifest: {}", r.status()));
    }
    Ok(digest)
}

// ------------------------------------------------------------------------------ fixtures

/// Packs `testdata/build/<name>` (and `extra` files) into a tar.
fn archive(name: &str, extra: &[(&str, &[u8])]) -> Bytes {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata/build")
        .join(name);
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for e in std::fs::read_dir(&dir).expect("fixture folder") {
        let e = e.expect("entry");
        if e.file_type().expect("type").is_file() {
            files.insert(
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).expect("fixture file"),
            );
        }
    }
    for (n, d) in extra {
        files.insert((*n).to_owned(), d.to_vec());
    }
    let mut w = oci::TarWriter::new();
    for (n, d) in &files {
        w.file(n.as_bytes(), 0o644, d);
    }
    Bytes::from(w.finish())
}

fn new_identity() -> WorkloadIdentity {
    WorkloadIdentity {
        tenant: TenantId::new(),
        app: NodeId::new(),
        deployment: uuid::Uuid::now_v7(),
        environment: Environment::Preview,
    }
}

fn limits() -> Limits {
    Limits {
        cpu_millis: 1000,
        memory_bytes: 512 * 1024 * 1024,
        disk_bytes: 64 * 1024 * 1024,
    }
}

fn input(identity: WorkloadIdentity, archive: Bytes, timeout: Duration) -> BuildInput {
    BuildInput {
        identity,
        runtime: "node22".to_owned(),
        archive,
        limits: limits(),
        timeout,
    }
}

fn usage_of(usage: &MemoryUsage, id: &WorkloadIdentity) -> usize {
    usage
        .records()
        .iter()
        .filter(|r| matches!(r, RuntimeUsage::Build { identity, took } if identity == id && !took.is_zero()))
        .count()
}

/// The config and the build's own (last) layer of a built image, uncompressed.
async fn image_content(
    registry_url: &str,
    identity: &WorkloadIdentity,
    image: &ImageRef,
) -> Result<(Value, Vec<u8>, Bytes), String> {
    let ImageRef::Oci(digest) = image else {
        return Err("not an OCI image".to_owned());
    };
    let repo = ruum_runtime::repository(identity);
    let c = http();
    let m: Value = c
        .get(format!("{registry_url}/v2/{repo}/manifests/{digest}"))
        .header("Accept", "application/vnd.oci.image.manifest.v1+json")
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let blob = |d: &str| {
        let url = format!("{registry_url}/v2/{repo}/blobs/{d}");
        let c = c.clone();
        async move {
            c.get(url)
                .send()
                .await
                .map_err(|e| e.to_string())?
                .bytes()
                .await
                .map_err(|e| e.to_string())
        }
    };
    let config = blob(m["config"]["digest"].as_str().ok_or("no config")?).await?;
    let last = m["layers"]
        .as_array()
        .and_then(|l| l.last())
        .and_then(|l| l["digest"].as_str())
        .ok_or("no layer")?
        .to_owned();
    let gz = blob(&last).await?;
    let mut tar = Vec::new();
    flate2::read::GzDecoder::new(&gz[..])
        .read_to_end(&mut tar)
        .map_err(|e| e.to_string())?;
    let cfg: Value = serde_json::from_slice(&config).map_err(|e| e.to_string())?;
    Ok((cfg, tar, config))
}

// ------------------------------------------------------------------------------ conformance

struct Fx {
    run: Run,
    builder: Arc<DockerBuilder>,
    usage: Arc<MemoryUsage>,
}

impl BuilderFixture for Fx {
    fn builder(&self) -> Arc<dyn Builder> {
        self.builder.clone()
    }
    fn usage(&self) -> Arc<MemoryUsage> {
        self.usage.clone()
    }
    fn runtime(&self) -> String {
        "node22".to_owned()
    }
    fn archive(&self) -> Bytes {
        archive("hello", &[])
    }
    fn failing_archive(&self) -> Bytes {
        archive("failing", &[])
    }
    fn slow_archive(&self) -> Bytes {
        archive("slow", &[])
    }
    fn build_timing(&self) -> BuildTiming {
        BuildTiming {
            timeout: Duration::from_secs(25),
            wait: Duration::from_secs(60),
        }
    }
    fn env_dump_archive(&self) -> Bytes {
        archive("envdump", &[])
    }
    fn canaries(&self) -> Vec<String> {
        self.run.canaries.clone()
    }
    fn image_content<'a>(
        &'a self,
        identity: &'a WorkloadIdentity,
        image: &'a ImageRef,
    ) -> BoxFuture<'a, Result<Option<Vec<Bytes>>, String>> {
        Box::pin(async move {
            let (_, tar, config) = image_content(&self.run.registry_url(), identity, image).await?;
            Ok(Some(vec![config, Bytes::from(tar)]))
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_builder_passes_the_conformance_suite() {
    if !engine_or_skip("build conformance") {
        return;
    }
    let run = Run::start(false).await.expect("the test run starts");
    let usage = Arc::new(MemoryUsage::new());
    let builder = run.builder(usage.clone(), |_| {}).await;
    let f = Fx {
        run,
        builder,
        usage,
    };
    let report = conformance::run_builder(&f).await;
    show(&format!(
        "builder conformance [{}]\n{}",
        f.builder.name(),
        report.summary()
    ));
    let (c, n) = f.run.leftovers();
    assert!(c.is_empty() && n.is_empty(), "builds left {c:?} and {n:?}");
    assert!(
        f.run.egress.active.lock().unwrap().is_empty(),
        "builds left egress registrations"
    );
    report.assert_ok();
}

// ------------------------------------------------------------------------------ the cases

#[derive(Default)]
struct Report {
    rows: Vec<(String, bool, String)>,
}

impl Report {
    fn check(&mut self, name: impl Into<String>, ok: bool, detail: impl Into<String>) {
        self.rows.push((name.into(), ok, detail.into()));
    }
    fn note(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.rows
            .push((format!("note: {}", name.into()), true, detail.into()));
    }
    fn print(&self, title: &str) {
        let mut out = format!("== {title}\n");
        for (n, ok, d) in &self.rows {
            out.push_str(&format!(
                "  {}  {n}{}\n",
                if *ok { "pass" } else { "FAIL" },
                if d.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", d.chars().take(600).collect::<String>())
                }
            ));
        }
        show(&out);
    }
}

struct Ctx {
    run: Run,
    usage: Arc<MemoryUsage>,
    builder: Arc<DockerBuilder>,
    driver: Arc<DockerDriver>,
}

impl Ctx {
    /// Checks the build's usage record and that nothing of it is left.
    fn after(&self, r: &mut Report, what: &str, id: &WorkloadIdentity) {
        r.check(
            format!("{what}: one usage record with the build's identity"),
            usage_of(&self.usage, id) == 1,
            format!("{}", usage_of(&self.usage, id)),
        );
        let (c, n) = self.run.leftovers();
        r.check(
            format!("{what}: no container or network of the build is left"),
            c.is_empty() && n.is_empty(),
            format!("containers {c:?}, networks {n:?}"),
        );
        let active = self.run.egress.active.lock().unwrap().clone();
        r.check(
            format!("{what}: no egress registration of the build is left"),
            active.is_empty(),
            format!("{active:?}"),
        );
    }

    fn is_failed(r: &Result<ruum_core::runtime::BuildOutput, RuntimeError>) -> String {
        match r {
            Ok(_) => "built".to_owned(),
            Err(e) => format!("{}: {e}", e.code()),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_builder_holds() {
    if !engine_or_skip("build") {
        return;
    }
    refuse_filter_under_ci();
    let run = Run::start(full()).await.expect("the test run starts");
    let usage = Arc::new(MemoryUsage::new());
    let builder = run.builder(usage.clone(), |_| {}).await;
    let driver = run.driver(Arc::new(MemoryUsage::new())).await;
    let c = Ctx {
        run,
        usage,
        builder,
        driver,
    };
    let mut r = Report::default();
    r.note(
        "runtime",
        format!(
            "{:?}, egress {}",
            runtime_kind(),
            if c.run.real_egress {
                "R-2e's proxy (RUUM_SANDBOX_FULL=1)"
            } else {
                "a stand-in"
            }
        ),
    );
    // The hello app runs and serves (acceptance 2); its instance is a target and a comparison
    // for the cases below.
    let app = if wanted("image") || wanted("hardening") || wanted("network") {
        image(&c, &mut r).await
    } else {
        None
    };
    if wanted("hardening") {
        hardening(&c, &mut r, app.as_ref()).await;
    }
    if wanted("failure") {
        failure(&c, &mut r).await;
    }
    if wanted("names") {
        names(&c, &mut r).await;
    }
    if wanted("limits") {
        output_limits(&c, &mut r).await;
    }
    if wanted("forkbomb") {
        forkbomb(&c, &mut r).await;
    }
    if wanted("network") {
        if c.run.real_egress {
            network(&c, &mut r, app.as_ref()).await;
        } else {
            r.note(
                "network",
                "the hostile network case runs with RUUM_SANDBOX_FULL=1 (R-2e's proxy, the dev stack)",
            );
        }
    }
    if let Some(a) = &app {
        let _ = c.driver.stop(a.identity.tenant, a.instance).await;
        // The image the driver pulled is a build image of this run: the prune removes it.
        let pruned = c.builder.prune_images(Duration::ZERO).await;
        r.check(
            "prune: the build images in the engine are removed by label",
            matches!(pruned, Ok(n) if n >= 1),
            format!("{pruned:?}"),
        );
    }
    r.print(&format!("node22 builder [{}]", c.builder.name()));
    let failed: Vec<&String> = r.rows.iter().filter(|x| !x.1).map(|x| &x.0).collect();
    assert!(
        failed.is_empty(),
        "{} checks failed: {failed:?}",
        failed.len()
    );
}

struct App {
    identity: WorkloadIdentity,
    instance: ruum_core::runtime::InstanceId,
    network: String,
    ip: String,
}

/// Runs a short node script in a container (the base image) on `network`: a control probe.
fn node_on(run: &Run, network: &str, script: &str, env: &[(&str, &str)]) -> String {
    let label = run.label_arg();
    let base = run.base_ref();
    let mut args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--label".into(),
        label,
        "--network".into(),
        network.into(),
        "--user".into(),
        "10001:10001".into(),
        "--read-only".into(),
        "--cap-drop".into(),
        "ALL".into(),
        "--entrypoint".into(),
        "node".into(),
    ];
    for (k, v) in env {
        args.push("-e".into());
        args.push(format!("{k}={v}"));
    }
    args.push(base);
    args.push("-e".into());
    args.push(script.into());
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    docker(&a).unwrap_or_else(|e| e)
}

const TCP_PROBE: &str = "const s=require('net').connect({host:process.env.H,port:+process.env.P,timeout:3000});\
s.on('connect',()=>{console.log('connected');process.exit(0)});\
s.on('timeout',()=>{console.log('timeout');process.exit(0)});\
s.on('error',e=>{console.log(e.code);process.exit(0)});";

/// Acceptance 2: the hello archive becomes an image in its own repository only; the driver
/// runs it as a service; the health check passes; an HTTP request from the app network gets the
/// hello answer. The image has no volume, user 10001, and no canary.
async fn image(c: &Ctx, r: &mut Report) -> Option<App> {
    let before = c.run.catalog().await;
    let id = new_identity();
    let t0 = Instant::now();
    let built = c
        .builder
        .build(input(id, archive("hello", &[]), Duration::from_secs(300)))
        .await;
    r.check(
        "image: the hello archive builds",
        built.is_ok(),
        format!("{} in {:?}", Ctx::is_failed(&built), t0.elapsed()),
    );
    c.after(r, "image", &id);
    let Ok(out) = built else { return None };
    let after = c.run.catalog().await;
    let changed: BTreeSet<String> = after.difference(&before).cloned().collect();
    r.check(
        "image: only the repository of its identity was added to the registry",
        changed == BTreeSet::from([ruum_runtime::repository(&id)]),
        format!("{changed:?}"),
    );
    match image_content(&c.run.registry_url(), &id, &out.image).await {
        Ok((cfg, tar, config)) => {
            let conf = &cfg["config"];
            r.check(
                "image: User is 10001",
                conf["User"] == "10001",
                conf["User"].to_string(),
            );
            r.check(
                "image: no VOLUME, no Healthcheck",
                conf.get("Volumes").is_none_or(Value::is_null)
                    && conf.get("Healthcheck").is_none_or(Value::is_null),
                conf.to_string(),
            );
            r.check(
                "image: the command is the manifest's start",
                conf["Cmd"] == json!(["/bin/sh", "-c", "node server.js"]),
                conf["Cmd"].to_string(),
            );
            let env = conf["Env"].as_array().cloned().unwrap_or_default();
            r.check(
                "image: Env is the base image's only (PATH, NODE_VERSION, YARN_VERSION)",
                env.iter().all(|e| {
                    e.as_str().is_some_and(|s| {
                        s.starts_with("PATH=")
                            || s.starts_with("NODE_VERSION=")
                            || s.starts_with("YARN_VERSION=")
                    })
                }),
                format!("{env:?}"),
            );
            for (i, canary) in c.run.canaries.iter().enumerate() {
                let hex = hex::encode(canary);
                let holds = |b: &[u8], n: &[u8]| b.windows(n.len()).any(|w| w == n);
                r.check(
                    format!("image: config and layer hold no canary {i} (plain or hex)"),
                    !holds(&tar, canary.as_bytes())
                        && !holds(&tar, hex.as_bytes())
                        && !holds(&config, canary.as_bytes())
                        && !holds(&config, hex.as_bytes()),
                    "",
                );
            }
            let names = layer_names(&tar);
            r.check(
                "image: the layer holds the source and the lock under /app, owned by 10001",
                names.contains("app/server.js") && names.contains("app/package-lock.json"),
                format!("{:?}", names.iter().take(20).collect::<Vec<_>>()),
            );
        }
        Err(e) => r.check("image: the built image reads back", false, e),
    }
    // The driver starts it as a service.
    let spec = WorkloadSpec {
        identity: WorkloadIdentity {
            environment: Environment::Live,
            ..id
        },
        kind: WorkloadKind::Service(ServiceSpec {
            port: 8080,
            health: Some(HealthCheck {
                path: "/health".to_owned(),
                interval: Duration::from_secs(1),
                failure_threshold: 3,
            }),
            idle_timeout: None,
        }),
        image: out.image.clone(),
        command: Vec::new(),
        env: vec![EnvVar::plain("PORT", "8080")],
        previous_secrets: Vec::new(),
        workload_token: SecretValue::new(format!("wt_{}", uuid::Uuid::now_v7().simple())),
        limits: Limits {
            cpu_millis: 500,
            memory_bytes: 256 * 1024 * 1024,
            disk_bytes: 16 * 1024 * 1024,
        },
        routes: Vec::new(),
    };
    let live = spec.identity;
    let started = match c.driver.start(spec).await {
        Ok(s) => s,
        Err(e) => {
            r.check(
                "image: the driver starts the built image",
                false,
                e.to_string(),
            );
            return None;
        }
    };
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut healthy = false;
    while Instant::now() < deadline {
        if let Ok(s) = c.driver.status(live.tenant, started.id).await {
            if s.state == InstanceState::Running && s.health == ruum_core::runtime::Health::Healthy
            {
                healthy = true;
                break;
            }
            if matches!(s.state, InstanceState::Exited(_)) {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    r.check(
        "image: the driver runs the built image and its health check passes",
        healthy,
        "",
    );
    let network = ruum_runtime::network_name(&live);
    let container = c.driver.container_of(started.id).unwrap_or_default();
    let ip = docker(&[
        "inspect",
        "-f",
        &format!("{{{{(index .NetworkSettings.Networks \"{network}\").IPAddress}}}}"),
        &container,
    ])
    .unwrap_or_default();
    let answer = node_on(
        &c.run,
        &network,
        "fetch(`http://${process.env.H}:8080/`).then(r=>r.text()).then(t=>console.log(t.trim())).catch(e=>console.log('error',e.cause?.code||e.message))",
        &[("H", &ip), ("NODE_USE_ENV_PROXY", "0")],
    );
    r.check(
        "image: an HTTP request from the app network gets the hello answer",
        answer.contains("hello from ruum"),
        answer,
    );
    Some(App {
        identity: live,
        instance: started.id,
        network,
        ip,
    })
}

fn layer_names(tar: &[u8]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut at = 0;
    let mut long: Option<String> = None;
    while at + 512 <= tar.len() {
        let h = &tar[at..at + 512];
        if h.iter().all(|b| *b == 0) {
            break;
        }
        let size = usize::from_str_radix(
            std::str::from_utf8(&h[124..135])
                .unwrap_or("0")
                .trim_matches(char::from(0))
                .trim(),
            8,
        )
        .unwrap_or(0);
        let cstr = |f: &[u8]| {
            String::from_utf8_lossy(&f[..f.iter().position(|b| *b == 0).unwrap_or(f.len())])
                .into_owned()
        };
        let data = &tar[at + 512..(at + 512 + size).min(tar.len())];
        if h[156] == b'x' {
            let s = String::from_utf8_lossy(data);
            long = s
                .lines()
                .find_map(|l| l.split_once(" path=").map(|(_, p)| p.to_owned()));
        } else {
            let name = long.take().unwrap_or_else(|| {
                let prefix = cstr(&h[345..500]);
                let n = cstr(&h[0..100]);
                if prefix.is_empty() {
                    n
                } else {
                    format!("{prefix}/{n}")
                }
            });
            let kind = match h[156] {
                b'2' => format!(" -> {}", cstr(&h[157..257])),
                _ => String::new(),
            };
            out.insert(format!("{}{kind}", name.trim_end_matches('/')));
        }
        at += 512 + size.div_ceil(512) * 512;
    }
    out
}

/// Acceptance 4: one app instance of the driver and one build job of the builder read back
/// from the engine with the same security fields; both networks internal with inhibit_ipv4.
/// The build is the slow archive, so it also ends with a timeout (cleanup after a timeout).
async fn hardening(c: &Ctx, r: &mut Report, app: Option<&App>) {
    let Some(app) = app else {
        r.check(
            "hardening: an app instance to compare with",
            false,
            "the image case failed",
        );
        return;
    };
    let id = new_identity();
    let b = c.builder.clone();
    let job = tokio::spawn(async move {
        b.build(input(id, archive("slow", &[]), Duration::from_secs(40)))
            .await
    });
    let f1 = format!("label={}", c.run.label_arg());
    // The builder's own objects (an app container inherits the image's labels, `ruum.build`
    // among them, but the driver sets `ruum.managed=ruum-runtime`).
    let f2 = format!("label=ruum.managed={MANAGED_BY}");
    let mut container = String::new();
    for _ in 0..240 {
        container = docker(&["ps", "-q", "--filter", &f1, "--filter", &f2]).unwrap_or_default();
        if !container.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let inspect = |name: &str| -> Value {
        docker(&["inspect", name])
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| v.get(0).cloned())
            .unwrap_or(Value::Null)
    };
    let driver_c = inspect(&c.driver.container_of(app.instance).unwrap_or_default());
    let build_c = inspect(container.lines().next().unwrap_or(""));
    r.check(
        "hardening: the build job runs and reads back",
        !build_c.is_null() && !driver_c.is_null(),
        container.clone(),
    );
    for field in [
        "/HostConfig/ReadonlyRootfs",
        "/HostConfig/CapDrop",
        "/HostConfig/CapAdd",
        "/HostConfig/SecurityOpt",
        "/HostConfig/Privileged",
        "/Config/User",
        "/HostConfig/PidsLimit",
        "/HostConfig/Runtime",
        "/HostConfig/PublishAllPorts",
        "/HostConfig/PortBindings",
        "/HostConfig/IpcMode",
        "/HostConfig/Init",
        "/HostConfig/PidMode",
        "/HostConfig/UsernsMode",
        "/HostConfig/Devices",
        "/AppArmorProfile",
    ] {
        let a = driver_c.pointer(field).cloned().unwrap_or(Value::Null);
        let b = build_c.pointer(field).cloned().unwrap_or(Value::Null);
        // An empty list and no list are the same setting.
        let norm = |v: Value| match v {
            Value::Array(a) if a.is_empty() => Value::Null,
            Value::Object(o) if o.is_empty() => Value::Null,
            v => v,
        };
        let (a, b) = (norm(a), norm(b));
        r.check(
            format!("hardening: {field} is the same for the build job and the app"),
            a == b && !(field == "/HostConfig/ReadonlyRootfs" && a != json!(true)),
            format!("app {a}, build {b}"),
        );
    }
    let binds = build_c
        .pointer("/HostConfig/Binds")
        .cloned()
        .unwrap_or(Value::Null);
    let mounts = build_c.pointer("/Mounts").cloned().unwrap_or(Value::Null);
    r.check(
        "hardening: the build job has no bind mount and no volume",
        (binds.is_null() || binds == json!([])) && (mounts.is_null() || mounts == json!([])),
        format!("binds {binds}, mounts {mounts}"),
    );
    let net_of = |c: &Value| -> String {
        c.pointer("/HostConfig/NetworkMode")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    for (who, net) in [("app", net_of(&driver_c)), ("build", net_of(&build_c))] {
        let n = docker(&["network", "inspect", &net])
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| v.get(0).cloned())
            .unwrap_or(Value::Null);
        r.check(
            format!("hardening: the {who} network is internal with inhibit_ipv4"),
            n["Internal"] == json!(true)
                && n.pointer("/Options/com.docker.network.bridge.inhibit_ipv4")
                    == Some(&json!("true")),
            format!(
                "{net}: internal {}, options {}",
                n["Internal"], n["Options"]
            ),
        );
        if who == "build" {
            let others: Vec<String> = n["Containers"]
                .as_object()
                .map(|m| {
                    m.values()
                        .filter_map(|v| v["Name"].as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            r.check(
                "hardening: on the build network only the job and the egress proxy",
                others.len() == 2 && others.contains(&c.run.egress_container),
                format!("{others:?}"),
            );
        }
    }
    let seen = c.run.egress.seen.lock().unwrap().clone();
    let reg = seen.iter().find(|n| n.identity == id);
    r.check(
        "hardening: the build network is registered at the egress proxy with the build's identity and the npm hosts only",
        reg.is_some_and(|n| n.allow.as_deref() == Some(&["registry.npmjs.org".to_owned()][..])),
        format!("{reg:?}"),
    );
    let res = job.await.expect("task");
    r.check(
        "timeout: the slow build is build_timed_out",
        matches!(res, Err(RuntimeError::BuildTimedOut)),
        Ctx::is_failed(&res),
    );
    c.after(r, "timeout", &id);
}

/// A failing build is `build_failed` with the end of its log; nothing is pushed.
async fn failure(c: &Ctx, r: &mut Report) {
    let before = c.run.catalog().await;
    let id = new_identity();
    let res = c
        .builder
        .build(input(id, archive("failing", &[]), Duration::from_secs(300)))
        .await;
    r.check(
        "failure: a failing postinstall is build_failed with its log",
        matches!(&res, Err(RuntimeError::BuildFailed { reason }) if reason.contains("failing on purpose")),
        Ctx::is_failed(&res),
    );
    r.check(
        "failure: a failed build pushes nothing",
        c.run.catalog().await == before,
        "",
    );
    c.after(r, "failure", &id);
    // A manifest without start, and an archive with a link, fail before anything runs.
    let id = new_identity();
    let res = c
        .builder
        .build(input(
            id,
            archive("hello", &[("app.yaml", b"runtime: node22\n")]),
            Duration::from_secs(60),
        ))
        .await;
    r.check(
        "failure: a manifest without start is build_failed",
        matches!(&res, Err(RuntimeError::BuildFailed { reason }) if reason.contains("start")),
        Ctx::is_failed(&res),
    );
    let mut w = oci::TarWriter::new();
    w.file(
        b"app.yaml",
        0o644,
        b"runtime: node22\nstart: node server.js\n",
    );
    w.symlink(b"server.js", b"/etc/passwd");
    let id = new_identity();
    let res = c
        .builder
        .build(input(id, Bytes::from(w.finish()), Duration::from_secs(60)))
        .await;
    r.check(
        "failure: an archive with a link out of the tree is refused (build_failed)",
        matches!(&res, Err(RuntimeError::BuildFailed { reason }) if reason.contains("refused")),
        Ctx::is_failed(&res),
    );
}

/// Hostile names stay data; links out of the tree, a hard link and a FIFO are dropped; nothing
/// outside the output is read (backlog S-42).
async fn names(c: &Ctx, r: &mut Report) {
    let id = new_identity();
    let res = c
        .builder
        .build(input(id, archive("names", &[]), Duration::from_secs(300)))
        .await;
    r.check(
        "names: the build with hostile names builds",
        res.is_ok(),
        Ctx::is_failed(&res),
    );
    c.after(r, "names", &id);
    let Ok(out) = res else { return };
    let Ok((_, tar, _)) = image_content(&c.run.registry_url(), &id, &out.image).await else {
        r.check("names: the image reads back", false, "");
        return;
    };
    let names = layer_names(&tar);
    for n in [
        "app/-rf",
        "app/--checkpoint-action=exec=sh",
        "app/a b",
        "app/$(touch pwned)",
        "app/new\nline",
        "app/back\\slash",
        "app/*",
        "app/inside -> server.js",
    ] {
        r.check(
            format!("names: {n:?} is kept as data"),
            names.contains(n),
            "",
        );
    }
    for n in ["escape", "up", "deep/up", "fifo"] {
        let gone = !names
            .iter()
            .any(|x| x.starts_with(&format!("app/{n} ")) || *x == format!("app/{n}"));
        r.check(format!("names: {n:?} is dropped"), gone, "");
    }
    let hl = ["app/hl-a", "app/hl-b"]
        .iter()
        .filter(|n| names.contains(**n))
        .count();
    r.check(
        "names: of a hard link pair, the link entry is dropped",
        hl == 1,
        format!("{hl}"),
    );
    r.check(
        "names: no file of the shell ran ($(touch pwned) made nothing)",
        !names.contains("app/pwned"),
        "",
    );
    let holds = |n: &[u8]| tar.windows(n.len()).any(|w| w == n);
    r.check(
        "names: nothing outside the output was read (/etc/passwd is not in the layer)",
        !holds(b"root:x:0:0"),
        "",
    );
    let logs: Vec<&str> = out.logs.iter().map(|l| l.text.as_str()).collect();
    r.check(
        "names: the build log names the dropped entries",
        logs.iter()
            .any(|l| l.contains("dropped output entry \"escape\"")),
        "",
    );
}

/// The disk limit (ENOSPC) and the output byte cap fail the build; neither fills the host.
async fn output_limits(c: &Ctx, r: &mut Report) {
    let id = new_identity();
    let res = c
        .builder
        .build(input(
            id,
            archive("huge", &[("size.txt", b"96")]),
            Duration::from_secs(300),
        ))
        .await;
    r.check(
        "limits: a postinstall that writes past the disk limit fails with ENOSPC",
        matches!(&res, Err(RuntimeError::BuildFailed { reason }) if reason.contains("ENOSPC")),
        Ctx::is_failed(&res),
    );
    c.after(r, "limits (disk)", &id);
    let usage = Arc::new(MemoryUsage::new());
    let small = c
        .run
        .builder(usage.clone(), |b| b.output_max = 8 * 1024 * 1024)
        .await;
    let id = new_identity();
    let res = small
        .build(input(
            id,
            archive("huge", &[("size.txt", b"24")]),
            Duration::from_secs(300),
        ))
        .await;
    r.check(
        "limits: an output over the byte cap fails the build",
        matches!(&res, Err(RuntimeError::BuildFailed { reason }) if reason.contains("over the cap")),
        Ctx::is_failed(&res),
    );
    r.check(
        "limits (cap): one usage record",
        usage_of(&usage, &id) == 1,
        "",
    );
    let (cs, ns) = c.run.leftovers();
    r.check(
        "limits (cap): nothing of the build is left",
        cs.is_empty() && ns.is_empty(),
        format!("{cs:?} {ns:?}"),
    );
}

/// A fork bomb stops at the pids limit.
async fn forkbomb(c: &Ctx, r: &mut Report) {
    let id = new_identity();
    let res = c
        .builder
        .build(input(
            id,
            archive("forkbomb", &[]),
            Duration::from_secs(300),
        ))
        .await;
    let reason = match &res {
        Err(RuntimeError::BuildFailed { reason }) => reason.clone(),
        _ => String::new(),
    };
    let line = reason
        .lines()
        .find_map(|l| {
            l.split_once("ruum-fixture: forkbomb ")
                .map(|(_, j)| j.to_owned())
        })
        .unwrap_or_default();
    let v: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
    let spawned = v["spawned"].as_u64().unwrap_or(2000);
    let errs = v["errs"].as_object().map(|m| m.len()).unwrap_or(0);
    let stopped = spawned < 2000 || errs > 0;
    let failed = matches!(res, Err(RuntimeError::BuildFailed { .. }));
    if runtime_kind() == RuntimeKind::Runsc && line.is_empty() {
        // Under runsc the pids limit bounds the sandbox's host threads (gVisor's own too, see
        // the README): the bomb takes the whole sandbox down before the fixture can report,
        // and the build fails with the job's exit.
        r.check(
            "forkbomb: the build fails at the pids limit (runsc: the sandbox ends)",
            failed && reason.contains("node bomb.js"),
            Ctx::is_failed(&res).chars().take(300).collect::<String>(),
        );
    } else {
        r.check(
            "forkbomb: the build fails and the pids limit stopped the processes",
            failed && !line.is_empty() && stopped,
            format!(
                "{} {line}",
                Ctx::is_failed(&res).chars().take(200).collect::<String>()
            ),
        );
    }
    c.after(r, "forkbomb", &id);
}

// ------------------------------------------------------------------------------ the network

fn listen_tcp(port: u16) -> Option<u16> {
    let l = TcpListener::bind(("::", port)).ok()?;
    let p = l.local_addr().ok()?.port();
    std::thread::spawn(move || {
        for c in l.incoming() {
            drop(c);
        }
    });
    Some(p)
}

fn host_tcp(addr: &str, port: u16) -> String {
    let target = if addr.contains(':') {
        format!("[{addr}]:{port}")
    } else {
        format!("{addr}:{port}")
    };
    match target.parse::<SocketAddr>() {
        Ok(s) => match TcpStream::connect_timeout(&s, Duration::from_secs(3)) {
            Ok(_) => "connected".to_owned(),
            Err(e) => e.kind().to_string(),
        },
        Err(_) => format!("bad address {target}"),
    }
}

fn host_addr(v6: bool) -> Option<IpAddr> {
    let (bind, to) = if v6 {
        ("[::]:0", "[2606:4700:4700::1111]:53")
    } else {
        ("0.0.0.0:0", "1.1.1.1:53")
    };
    let s = UdpSocket::bind(bind).ok()?;
    s.connect(to).ok()?;
    let ip = s.local_addr().ok()?.ip();
    match ip {
        IpAddr::V6(a) if (a.segments()[0] & 0xe000) != 0x2000 => None,
        _ => Some(ip),
    }
}

fn egress_deny() -> String {
    let mut deny = Vec::new();
    if let Some(v4) = host_addr(false) {
        deny.push(v4.to_string());
    }
    if let Some(IpAddr::V6(v6)) = host_addr(true) {
        let s = v6.segments();
        deny.push(format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3]));
    }
    deny.join(",")
}

fn container_ip(name: &str, network: Option<&str>) -> Option<String> {
    let fmt = match network {
        Some(n) => format!("{{{{(index .NetworkSettings.Networks \"{n}\").IPAddress}}}}"),
        None => "{{range .NetworkSettings.Networks}}{{.IPAddress}} {{end}}".to_owned(),
    };
    let ip = docker(&["inspect", "-f", &fmt, name]).ok()?;
    let ip = ip.split_whitespace().next()?.to_owned();
    ip.parse::<IpAddr>().ok().map(|_| ip)
}

fn container_publishing(port: &str) -> Option<String> {
    let list = docker(&["ps", "--format", "{{.Names}}\t{{.Ports}}"]).ok()?;
    list.lines().find_map(|l| {
        let (name, ports) = l.split_once('\t')?;
        ports
            .split(", ")
            .any(|p| p.contains(&format!(":{port}->")))
            .then(|| name.to_owned())
    })
}

fn url_port(var: &str) -> Option<String> {
    let v = std::env::var(var).ok()?;
    let after = v.rsplit('@').next().unwrap_or(&v);
    let after = after
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let hostport = after.split(['/', '?']).next()?;
    hostport.rsplit(':').next().map(str::to_owned)
}

fn host_http_get(addr: &str, port: u16) -> bool {
    use std::io::Write;
    let Ok(sock) = format!("{addr}:{port}").parse::<SocketAddr>() else {
        return false;
    };
    let Ok(mut s) = TcpStream::connect_timeout(&sock, Duration::from_secs(3)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
    if s.write_all(b"GET / HTTP/1.0\r\nHost: 169.254.169.254\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut buf = [0u8; 16];
    matches!(s.read(&mut buf), Ok(n) if n >= 5 && &buf[..5] == b"HTTP/")
}

enum Control {
    Host(String),
    /// A control probe in a container on this network (to the same host and port).
    Net(String),
    /// A control probe on a plain internal network to its gateway (the host behind a bridge).
    PlainGateway,
}

struct Target {
    name: String,
    host: String,
    port: u16,
    control: Control,
}

/// Acceptance 3 (the hostile build): the postinstall tries every forbidden target and reaches
/// none, each with a positive control that reaches it in the same run; refused egress shows
/// the proxy's deny answer; `ms` comes from the npm registry through the proxy.
async fn network(c: &Ctx, r: &mut Report, app: Option<&App>) {
    let tcp_port = listen_tcp(0).expect("a host TCP listener");
    let mut targets: Vec<Target> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut add = |name: &str, host: Option<String>, port: u16, control: Control| match host {
        Some(h) if !h.is_empty() => targets.push(Target {
            name: name.to_owned(),
            host: h,
            port,
            control,
        }),
        _ => missing.push(name.to_owned()),
    };
    let pg = url_port("DATABASE_URL")
        .and_then(|p| container_publishing(&p))
        .and_then(|n| container_ip(&n, None));
    add(
        "platform Postgres (DATABASE_URL)",
        pg.clone(),
        5432,
        Control::Host(pg.unwrap_or_default()),
    );
    let data = url_port("RUUM_DBHOST_URL")
        .and_then(|p| container_publishing(&p))
        .and_then(|n| container_ip(&n, None));
    add(
        "data cluster Postgres (RUUM_DBHOST_URL)",
        data.clone(),
        5432,
        Control::Host(data.unwrap_or_default()),
    );
    let spicedb = url_port("SPICEDB_ENDPOINT")
        .and_then(|p| container_publishing(&p))
        .and_then(|n| container_ip(&n, None));
    add(
        "SpiceDB (SPICEDB_ENDPOINT)",
        spicedb.clone(),
        50051,
        Control::Host(spicedb.unwrap_or_default()),
    );
    for (name, cont, port) in [
        ("OpenBao (ruum-openbao-1)", "ruum-openbao-1", 8200u16),
        ("NATS (ruum-nats-1)", "ruum-nats-1", 4222),
        ("Garage S3 (ruum-garage-1)", "ruum-garage-1", 3900),
        ("stack Postgres (ruum-postgres-1)", "ruum-postgres-1", 5432),
    ] {
        let ip = container_ip(cont, Some("ruum_default"));
        add(
            name,
            ip.clone(),
            port,
            Control::Host(ip.unwrap_or_default()),
        );
    }
    let reg_ip = container_ip(&c.run.registry_container, None);
    add(
        "the registry",
        reg_ip.clone(),
        5000,
        Control::Host(reg_ip.clone().unwrap_or_default()),
    );
    add(
        "metadata address TCP 80",
        Some("169.254.169.254".into()),
        80,
        Control::Host("169.254.169.254".into()),
    );
    add(
        "host through the build bridge's would-be gateway",
        Some("GATEWAY".into()),
        tcp_port,
        Control::PlainGateway,
    );
    let docker0 = docker(&[
        "network",
        "inspect",
        "bridge",
        "-f",
        "{{range .IPAM.Config}}{{.Gateway}}{{end}}",
    ])
    .ok()
    .filter(|g| g.parse::<IpAddr>().is_ok());
    add(
        "host through docker0",
        docker0.clone(),
        tcp_port,
        Control::Host(docker0.unwrap_or_default()),
    );
    let v4 = host_addr(false).map(|a| a.to_string());
    add(
        "host public IPv4",
        v4.clone(),
        tcp_port,
        Control::Host(v4.clone().unwrap_or_default()),
    );
    if let Some(a) = app {
        add(
            "another app's network (the hello app)",
            Some(a.ip.clone()),
            8080,
            Control::Net(a.network.clone()),
        );
    } else {
        add(
            "another app's network (the hello app)",
            None,
            8080,
            Control::Host(String::new()),
        );
    }
    add(
        "internet direct 1.1.1.1:443",
        Some("1.1.1.1".into()),
        443,
        Control::Host("1.1.1.1".into()),
    );
    let dns = ["registry.npmjs.org", "example.com"];
    // Through the proxy: refused with its own deny answer.
    let mut refused: Vec<String> = vec![
        "169.254.169.254:80".into(),
        "169.254.169.254:443".into(),
        "[fd00:ec2::254]:443".into(),
        "[fd00:ec2::254]:80".into(),
        "10.0.0.1:443".into(),
        "example.com:443".into(),
    ];
    if let Some(ip) = &reg_ip {
        refused.push(format!("{ip}:5000"));
    }
    if let Some(v4) = &v4 {
        refused.push(format!("{v4}:22"));
    }
    if let Some(a) = app {
        refused.push(format!("{}:8080", a.ip));
    }
    let mut proxy = vec!["registry.npmjs.org:443".to_owned()];
    proxy.extend(refused.iter().cloned());
    let tcp: Vec<Value> = targets
        .iter()
        .map(|t| json!({"name": t.name, "host": t.host, "port": t.port}))
        .collect();
    let cfg = json!({"tcp": tcp, "metadata": true, "dns": dns, "proxy": proxy});
    let cfg = serde_json::to_vec(&cfg).expect("json");
    let id = new_identity();
    let res = c
        .builder
        .build(input(
            id,
            archive("hostile", &[("targets.json", &cfg)]),
            Duration::from_secs(300),
        ))
        .await;
    r.check(
        "network: the hostile build ran and built",
        res.is_ok(),
        Ctx::is_failed(&res),
    );
    c.after(r, "network", &id);
    let logs: Vec<String> = match &res {
        Ok(o) => o.logs.iter().map(|l| l.text.clone()).collect(),
        Err(RuntimeError::BuildFailed { reason }) => reason.lines().map(str::to_owned).collect(),
        Err(_) => Vec::new(),
    };
    let results: BTreeMap<String, Value> = logs
        .iter()
        .filter_map(|l| l.strip_prefix("probe "))
        .filter_map(|j| serde_json::from_str::<Value>(j).ok())
        .filter_map(|v| Some((v.get("name")?.as_str()?.to_owned(), v)))
        .collect();
    let reached =
        |v: Option<&Value>| v.and_then(|v| v.get("reached")).and_then(Value::as_bool) == Some(true);
    let detail = |v: Option<&Value>| {
        v.and_then(|v| v.get("detail"))
            .and_then(Value::as_str)
            .unwrap_or("no result")
            .to_owned()
    };
    for m in &missing {
        r.check(
            format!("network: the target {m} has an address"),
            false,
            "none found",
        );
    }
    // The controls, in the same run.
    let plain = format!("{}-plain", c.run.label);
    let _ = docker(&[
        "network",
        "create",
        "--internal",
        "--label",
        &c.run.label_arg(),
        &plain,
    ]);
    let gw = docker(&[
        "network",
        "inspect",
        "-f",
        "{{range .IPAM.Config}}{{.Gateway}}{{end}}",
        &plain,
    ])
    .unwrap_or_default();
    for t in &targets {
        let ctl = match &t.control {
            Control::Host(a) => host_tcp(a, t.port),
            Control::Net(n) => node_on(
                &c.run,
                n,
                TCP_PROBE,
                &[("H", &t.host), ("P", &t.port.to_string())],
            ),
            Control::PlainGateway => node_on(
                &c.run,
                &plain,
                TCP_PROBE,
                &[("H", &gw), ("P", &t.port.to_string())],
            ),
        };
        let v = results.get(&t.name);
        if ctl.trim() == "connected" {
            r.check(
                format!("network: {} ({}:{}) is not reached", t.name, t.host, t.port),
                v.is_some() && !reached(v),
                format!("{}; control connected", detail(v)),
            );
        } else {
            r.check(
                format!("network: {}: the positive control does not reach it, so the probe proves nothing", t.name),
                false,
                format!("{}; control {}", detail(v), ctl.trim()),
            );
        }
    }
    let _ = docker(&["network", "rm", &plain]);
    let meta = results.get("metadata HTTP GET");
    r.check(
        "network: the metadata HTTP GET is not answered",
        host_http_get("169.254.169.254", 80) && meta.is_some() && !reached(meta),
        detail(meta),
    );
    for d in dns {
        let v = results.get(&format!("DNS {d}"));
        let ctl = std::net::ToSocketAddrs::to_socket_addrs(&format!("{d}:443"))
            .is_ok_and(|mut a| a.next().is_some());
        r.check(
            format!("network: DNS {d} does not resolve inside the build"),
            ctl && v.is_some() && !reached(v),
            detail(v),
        );
    }
    let npm = results.get("proxy registry.npmjs.org:443");
    r.check(
        "network: the npm registry is reached through the egress proxy (200)",
        npm.and_then(|v| v.get("status")).and_then(Value::as_i64) == Some(200),
        detail(npm),
    );
    for t in &refused {
        let v = results.get(&format!("proxy {t}"));
        let ok = v.and_then(|v| v.get("status")).and_then(Value::as_i64) == Some(403)
            && v.and_then(|v| v.get("deny"))
                .and_then(Value::as_str)
                .is_some();
        r.check(
            format!(
                "network: {t} through the proxy gets its deny answer (403, X-Ruum-Egress-Deny)"
            ),
            ok,
            detail(v),
        );
    }
    let ms = results.get("egress control: ms installed");
    r.check(
        "network: npm ci installed ms from the npm registry through the proxy",
        reached(ms),
        detail(ms),
    );
    let logs = docker(&["logs", &c.run.egress_container]).unwrap_or_default()
        + &std::process::Command::new("docker")
            .args(["logs", &c.run.egress_container])
            .env("DOCKER_HOST", docker_host())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stderr).into_owned())
            .unwrap_or_default();
    r.check(
        "network: the egress proxy logged the build's tunnel to registry.npmjs.org",
        logs.contains("registry.npmjs.org"),
        "",
    );
    r.check(
        "network: the probe answered for every target",
        targets.iter().all(|t| results.contains_key(&t.name)),
        format!("{} results", results.len()),
    );
    r.note(
        "targets run",
        targets
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>()
            .join("; "),
    );
}
