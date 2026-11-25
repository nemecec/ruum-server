//! The harness of the Docker driver's tests: the fixture image, a local `registry:2`, stand-in
//! shared containers, and clean-up. Everything it makes carries the label
//! `ruum.test=s3w3r2a` and is removed at the start and the end of a run (also when a test
//! fails: [`Harness`] cleans up on drop).
#![allow(dead_code)]

use ruum_core::runtime::{
    ImageRef, MemoryUsage, OciDigest, Route, RuntimeUsageSink, WorkloadIdentity,
};
use ruum_runtime::{
    DockerConfig, DockerDriver, EgressRegistry, HttpEgressRegistry, NoEgress, RuntimeKind,
    SharedContainer,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

pub const LABEL_KEY: &str = "ruum.test";
pub const LABEL_VALUE: &str = "s3w3r2a";
pub const LABEL: &str = "ruum.test=s3w3r2a";
/// `registry:2`, pinned by the digest of its multi-arch index (2026-11-15).
pub const REGISTRY_IMAGE: &str =
    "registry:2@sha256:a3d8aaa63ed8681a604f1dea0aa03f100d5895b6a58ace528858a7b332415373";

/// The static sandbox supervisor (backlog I6-20) for the driver: `RUUM_SUPERVISOR_BIN`, or built
/// here for `<arch>-unknown-linux-musl` (once per test binary, into `CARGO_TARGET_TMPDIR`). The
/// musl target must be installed (`rustup target add`); without it the build fails, and the test
/// fails when `CI` is set (the CI job installs the target), else it runs without a supervisor.
pub fn supervisor_bin() -> Option<PathBuf> {
    static BIN: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    BIN.get_or_init(|| {
        if let Ok(p) = std::env::var("RUUM_SUPERVISOR_BIN") {
            return Some(PathBuf::from(p));
        }
        let target = format!("{}-unknown-linux-musl", std::env::consts::ARCH);
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("supervisor");
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
        let built = Command::new(cargo)
            .args(["build", "--release", "-p", "ruum-supervisor", "--target", &target])
            .env("CARGO_TARGET_DIR", &dir)
            .current_dir(&root)
            .status();
        let bin = dir.join(&target).join("release/ruum-supervisor");
        match built {
            Ok(s) if s.success() && bin.is_file() => Some(bin),
            other => {
                assert!(
                    std::env::var_os("CI").is_none(),
                    "the sandbox supervisor did not build for {target} ({other:?}); install the target"
                );
                eprintln!("the sandbox supervisor did not build for {target}: the Docker tests run without it");
                None
            }
        }
    })
    .clone()
}

pub fn docker_host() -> String {
    std::env::var("RUUM_DOCKER_HOST").unwrap_or_else(|_| "unix:///var/run/docker.sock".to_owned())
}

pub fn runtime_kind() -> RuntimeKind {
    match std::env::var("RUUM_RUNTIME_DRIVER") {
        Ok(v) => RuntimeKind::parse(&v).expect("RUUM_RUNTIME_DRIVER"),
        Err(_) => RuntimeKind::Runc,
    }
}

/// Runs the docker CLI against the test engine; the trimmed standard output, or the error.
pub fn docker(args: &[&str]) -> Result<String, String> {
    let host = docker_host();
    let out = Command::new("docker")
        .args(args)
        .env("DOCKER_HOST", &host)
        .output()
        .map_err(|e| format!("docker {}: {e}", args.join(" ")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(format!(
            "docker {}: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// True when an engine answers. Without one a test fails when `CI` is set (backlog T-26) and
/// says why it did not run otherwise.
pub fn engine_or_skip(test: &str) -> bool {
    match docker(&["version", "--format", "{{.Server.Version}}"]) {
        Ok(_) => true,
        Err(e) => {
            assert!(
                std::env::var_os("CI").is_none(),
                "{test}: no Docker engine while CI is set: {e}"
            );
            eprintln!("{test}: not run, no Docker engine: {e}");
            false
        }
    }
}

/// Removes every container and network of the tests.
pub fn cleanup() {
    let filter = format!("label={LABEL}");
    if let Ok(ids) = docker(&["ps", "-aq", "--filter", &filter])
        && !ids.is_empty()
    {
        let mut args = vec!["rm", "-f", "-v"];
        args.extend(ids.lines());
        let _ = docker(&args);
    }
    // The driver's own containers carry the label too (extra_labels).
    if let Ok(ids) = docker(&["network", "ls", "-q", "--filter", &filter])
        && !ids.is_empty()
    {
        for id in ids.lines() {
            let _ = docker(&["network", "rm", id]);
        }
    }
}

pub struct Harness {
    pub fixture_image: String,
    pub registry: String,
    pub state_dir: PathBuf,
    pub shared: HashMap<Route, SharedContainer>,
    pub shared_network: String,
    pushed: Mutex<HashMap<(uuid::Uuid, uuid::Uuid, String), String>>,
    /// The real egress proxy (card R-2e), when [`Harness::start_egress`] ran.
    pub egress: Option<Egress>,
}

/// The egress proxy of R-2e (`deploy/images/ruum-egress`), built and started by the harness: on
/// an outside network, with its control socket on a host folder and its token in a file.
pub struct Egress {
    pub container: String,
    pub control: String,
    pub registry: Arc<HttpEgressRegistry>,
    pub deny: String,
    pub dir: PathBuf,
}

impl Drop for Harness {
    fn drop(&mut self) {
        cleanup();
        let _ = std::fs::remove_dir_all(&self.state_dir);
        if let Some(e) = &self.egress {
            let _ = std::fs::remove_dir_all(&e.dir);
        }
    }
}

fn run_id() -> String {
    uuid::Uuid::now_v7().simple().to_string()[20..].to_owned()
}

impl Harness {
    /// Builds the fixture image, starts the registry and the stand-in shared containers.
    pub fn start() -> Harness {
        cleanup();
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/fixture");
        let fixture_image = "ruum-r2a-fixture:test".to_owned();
        docker(&["build", "-q", "--label", LABEL, "-t", &fixture_image, dir])
            .expect("fixture image");
        let id = run_id();
        let reg = format!("r2a-registry-{id}");
        docker(&[
            "run",
            "-d",
            "--name",
            &reg,
            "--label",
            LABEL,
            "-p",
            "127.0.0.1::5000",
            REGISTRY_IMAGE,
        ])
        .expect("registry");
        let port = docker(&["port", &reg, "5000/tcp"]).expect("registry port");
        let port = port
            .lines()
            .next()
            .and_then(|l| l.rsplit(':').next())
            .expect("registry port")
            .to_owned();
        let registry = format!("127.0.0.1:{port}");
        // Wait until the registry answers.
        for _ in 0..100 {
            if std::net::TcpStream::connect(&registry).is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        // The shared containers stand on a network of their own; the driver attaches them to
        // app networks.
        let shared_network = format!("r2a-shared-{id}");
        docker(&[
            "network",
            "create",
            "--internal",
            "--label",
            LABEL,
            &shared_network,
        ])
        .expect("shared net");
        let mut shared = HashMap::new();
        for (route, alias, port) in [
            (Route::EgressProxy, "ruum-egress", 3128u16),
            (Route::InternalGateway, "ruum.internal", 80),
            (Route::Pooler, "ruum-pooler", 6432),
        ] {
            let name = format!("r2a-{}-{id}", route.as_str().replace('_', "-"));
            docker(&[
                "run",
                "-d",
                "--name",
                &name,
                "--label",
                LABEL,
                "--network",
                &shared_network,
                "--read-only",
                "--cap-drop",
                "ALL",
                "--user",
                "10001:10001",
                &fixture_image,
                "hold",
            ])
            .expect("shared stand-in");
            shared.insert(
                route,
                SharedContainer {
                    container: name,
                    alias: alias.to_owned(),
                    port,
                },
            );
        }
        let state_dir = std::env::temp_dir().join(format!("r2a-state-{id}"));
        Harness {
            fixture_image,
            registry,
            state_dir,
            shared,
            shared_network,
            pushed: Mutex::new(HashMap::new()),
            egress: None,
        }
    }

    /// Builds R-2e's image and starts the egress proxy in place of the stand-in. `deny` is its
    /// `EGRESS_DENY` (the host's public addresses).
    pub fn start_egress(&mut self, deny: &str) -> Result<(), String> {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        let dockerfile = format!("{root}/deploy/images/ruum-egress/Dockerfile");
        if !std::path::Path::new(&dockerfile).exists() {
            return Err(format!(
                "no {dockerfile}: R-2e's image recipe is not in this tree"
            ));
        }
        let image = "ruum-egress:s3w3r2a";
        docker(&[
            "build",
            "-q",
            "--label",
            LABEL,
            "-f",
            &dockerfile,
            "-t",
            image,
            root,
        ])?;
        let id = run_id();
        let out = format!("r2a-out-{id}");
        docker(&["network", "create", "--label", LABEL, &out])?;
        let dir = std::env::temp_dir().join(format!("r2a-egress-{id}"));
        let ctl = dir.join("ctl");
        std::fs::create_dir_all(&ctl).map_err(|e| e.to_string())?;
        let token = format!(
            "{:032x}{:032x}",
            rand::random::<u128>(),
            rand::random::<u128>()
        );
        let token_file = dir.join("token");
        std::fs::write(&token_file, &token).map_err(|e| e.to_string())?;
        let uid = Command::new("id")
            .arg("-u")
            .output()
            .map_err(|e| e.to_string())?;
        let gid = Command::new("id")
            .arg("-g")
            .output()
            .map_err(|e| e.to_string())?;
        let user = format!(
            "{}:{}",
            String::from_utf8_lossy(&uid.stdout).trim(),
            String::from_utf8_lossy(&gid.stdout).trim()
        );
        let name = format!("r2a-egress-{id}");
        let ctl_mount = format!("{}:/run/egress", ctl.display());
        let token_mount = format!("{}:/run/egress-token/token:ro", token_file.display());
        let deny_env = format!("EGRESS_DENY={deny}");
        // The proxy runs as the calling user, so the control socket on the bind mount is usable
        // by the test (the driver's process); production gives the volume to the server only.
        docker(&[
            "run",
            "-d",
            "--name",
            &name,
            "--label",
            LABEL,
            "--network",
            &out,
            "--read-only",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--user",
            &user,
            // A shared container is the only hop between app networks: it forwards nothing
            // (I-4 starts the production container with the same sysctls).
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
            image,
        ])?;
        let sock = ctl.join("control.sock");
        for _ in 0..100 {
            if sock.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if !sock.exists() {
            return Err(format!(
                "the egress proxy made no control socket: {}",
                docker(&["logs", &name]).unwrap_or_default()
            ));
        }
        let control = format!("unix:{}", sock.display());
        let registry = Arc::new(
            HttpEgressRegistry::new(&control, ruum_core::runtime::SecretValue::new(token))
                .map_err(|e| e.to_string())?,
        );
        if let Some(s) = self.shared.get_mut(&Route::EgressProxy) {
            let _ = docker(&["rm", "-f", &s.container]);
            s.container = name.clone();
        }
        self.egress = Some(Egress {
            container: name,
            control,
            registry,
            deny: deny.to_owned(),
            dir,
        });
        Ok(())
    }

    pub fn config(&self) -> DockerConfig {
        let mut c = DockerConfig::new(docker_host(), self.registry.clone(), self.state_dir.clone());
        c.runtime = runtime_kind();
        c.routes = self.shared.iter().map(|(r, s)| (*r, s.clone())).collect();
        c.extra_labels
            .insert(LABEL_KEY.to_owned(), LABEL_VALUE.to_owned());
        c.stop_timeout = std::time::Duration::from_secs(5);
        c.supervisor = supervisor_bin();
        c
    }

    pub async fn driver(&self, usage: Arc<dyn RuntimeUsageSink>) -> Arc<DockerDriver> {
        self.driver_with(self.config(), usage, Arc::new(NoEgress))
            .await
    }

    pub async fn driver_with(
        &self,
        cfg: DockerConfig,
        usage: Arc<dyn RuntimeUsageSink>,
        egress: Arc<dyn EgressRegistry>,
    ) -> Arc<DockerDriver> {
        Arc::new(
            DockerDriver::connect(cfg, usage, egress)
                .await
                .expect("the Docker driver connects"),
        )
    }

    /// Pushes the fixture image to the repository of the identity's tenant and app; the digest.
    pub fn push(&self, identity: &WorkloadIdentity) -> Result<ImageRef, String> {
        let image = self.fixture_image.clone();
        self.push_image(identity, &image)
    }

    /// Builds a variant of the fixture from `testdata/fixture/<dockerfile>` (same context);
    /// the local image name.
    pub fn build_variant(&self, dockerfile: &str) -> Result<String, String> {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/fixture");
        let tag = format!(
            "ruum-r2a-fixture-{}:test",
            dockerfile.replace('.', "-").to_lowercase()
        );
        let file = format!("{dir}/{dockerfile}");
        docker(&[
            "build", "-q", "--label", LABEL, "-f", &file, "-t", &tag, dir,
        ])?;
        Ok(tag)
    }

    /// Pushes a local image to the repository of the identity's tenant and app; the digest.
    pub fn push_image(&self, identity: &WorkloadIdentity, image: &str) -> Result<ImageRef, String> {
        let key = (identity.tenant.0, identity.app.0, image.to_owned());
        if let Some(d) = self.pushed.lock().unwrap().get(&key) {
            return Ok(ImageRef::Oci(
                OciDigest::parse(d).map_err(|e| e.to_string())?,
            ));
        }
        let target = format!(
            "{}/{}:fixture",
            self.registry,
            ruum_runtime::repository(identity)
        );
        docker(&["tag", image, &target])?;
        let out = docker(&["push", &target])?;
        let digest = out
            .split_whitespace()
            .find_map(|w| {
                w.strip_prefix("sha256:")
                    .map(|h| format!("sha256:{}", &h[..64.min(h.len())]))
            })
            .ok_or_else(|| format!("no digest in the push output: {out}"))?;
        let _ = docker(&["rmi", &target]);
        self.pushed.lock().unwrap().insert(key, digest.clone());
        Ok(ImageRef::Oci(
            OciDigest::parse(&digest).map_err(|e| e.to_string())?,
        ))
    }
}

/// Writes a report to the test's standard error directly, past the test harness's capture, so
/// the CI log shows it also when the test passes (card R-2a review, change 3).
pub fn show(text: &str) {
    use std::io::Write;
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(text.as_bytes());
    if !text.ends_with('\n') {
        let _ = err.write_all(b"\n");
    }
    let _ = err.flush();
}

pub fn new_usage() -> Arc<MemoryUsage> {
    Arc::new(MemoryUsage::new())
}
