//! The Kubernetes driver (backlog I6-1) against a fake API server with a fake kubelet
//! (`kube_fake`): the conformance suite of `ruum_core::runtime::conformance`, unchanged, and the
//! driver's own rules: the restricted gVisor pod, no secret value outside the Secret, only the
//! verbs of its Role, the read-back, adoption by another server process, the egress
//! registration and a pod that disappears. No cluster and no Docker needed: these run in every
//! `cargo test`.

#[path = "kube_fake/mod.rs"]
mod kube_fake;

use async_trait::async_trait;
use kube_fake::{Fake, TOKEN, Tamper};
use ruum_core::ids::{NodeId, TenantId};
use ruum_core::runtime::conformance::{self, DriverFixture, Timing};
use ruum_core::runtime::program::{self, Step};
use ruum_core::runtime::{
    EnvVar, Environment, ExitReason, ImageRef, InstanceState, Limits, MemoryUsage, OciDigest,
    Route, RuntimeDriver, RuntimeError, RuntimeUsage, SecretValue, ServiceSpec, WorkloadIdentity,
    WorkloadKind, WorkloadSpec,
};
use ruum_runtime::{EgressNetwork, EgressRegistry};
use ruum_runtime::{KubeConfig, KubeDriver, SharedService, repository};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const NS: &str = "ruum-apps";

/// Every registration and unregistration, in order; unregistrations fail while `refuse` is set.
#[derive(Default)]
struct Recording {
    events: Mutex<Vec<String>>,
    refuse: AtomicBool,
}

#[async_trait]
impl EgressRegistry for Recording {
    async fn register(&self, n: &EgressNetwork) -> Result<(), RuntimeError> {
        self.events
            .lock()
            .unwrap()
            .push(format!("register {} {}", n.network, n.subnet));
        Ok(())
    }
    async fn unregister(&self, network: &str) -> Result<(), RuntimeError> {
        if self.refuse.load(Ordering::SeqCst) {
            return Err(RuntimeError::Unavailable {
                reason: "the proxy is down".into(),
            });
        }
        self.events
            .lock()
            .unwrap()
            .push(format!("unregister {network}"));
        Ok(())
    }
}

struct World {
    fake: Arc<Fake>,
    cfg: KubeConfig,
    egress: Arc<Recording>,
    _dir: TempDir,
}

/// A temporary folder for the token file, removed at the end.
struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn world(split: bool) -> World {
    let (fake, url) = Fake::start(NS).await;
    let dir = std::env::temp_dir().join(format!("ruum-kube-{}", uuid::Uuid::now_v7().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let token = dir.join("token");
    std::fs::write(&token, format!("{TOKEN}\n")).unwrap();
    let mut cfg = KubeConfig::new(url.clone(), NS, "gvisor");
    cfg.token_file = Some(token);
    cfg.registry = Some("registry.test:5000".into());
    cfg.registry_url = Some(url);
    cfg.routes = BTreeMap::from([
        (
            Route::InternalGateway,
            SharedService {
                address: "127.0.0.11".into(),
                alias: "ruum.internal".into(),
                port: 8080,
            },
        ),
        (
            Route::EgressProxy,
            SharedService {
                address: "127.0.0.12".into(),
                alias: "ruum-egress".into(),
                port: 3128,
            },
        ),
        (
            Route::Pooler,
            SharedService {
                address: "127.0.0.13".into(),
                alias: "ruum-pooler".into(),
                port: 6432,
            },
        ),
    ]);
    cfg.sweep_every = Duration::from_millis(100);
    cfg.start_period = Duration::from_millis(200);
    cfg.stop_timeout = Duration::from_secs(1);
    cfg.reregister_every = Duration::from_millis(300);
    cfg.split_log_streams = split;
    cfg.extra_labels = BTreeMap::from([("ruum.test".into(), "i61".into())]);
    World {
        fake,
        cfg,
        egress: Arc::new(Recording::default()),
        _dir: TempDir(dir),
    }
}

impl World {
    fn driver(&self, usage: Arc<MemoryUsage>) -> KubeDriver {
        KubeDriver::new(self.cfg.clone(), self.egress.clone(), usage).unwrap()
    }

    fn image(&self, identity: &WorkloadIdentity) -> ImageRef {
        let digest = OciDigest::of(uuid::Uuid::now_v7().as_bytes());
        self.fake.push(&repository(identity), digest.as_str());
        ImageRef::Oci(digest)
    }

    fn spec(&self, identity: WorkloadIdentity, steps: &[Step]) -> WorkloadSpec {
        WorkloadSpec {
            identity,
            kind: WorkloadKind::Service(ServiceSpec {
                port: 8080,
                health: None,
                idle_timeout: None,
            }),
            image: self.image(&identity),
            command: program::command(steps),
            env: Vec::new(),
            previous_secrets: Vec::new(),
            workload_token: SecretValue::new(format!("tok-{}", uuid::Uuid::now_v7().simple())),
            limits: Limits {
                cpu_millis: 250,
                memory_bytes: 64 << 20,
                disk_bytes: 16 << 20,
            },
            routes: vec![Route::EgressProxy],
        }
    }
}

fn identity() -> WorkloadIdentity {
    WorkloadIdentity {
        tenant: TenantId::new(),
        app: NodeId::new(),
        deployment: uuid::Uuid::now_v7(),
        environment: Environment::Live,
    }
}

async fn wait_state(
    d: &dyn RuntimeDriver,
    tenant: TenantId,
    id: ruum_core::runtime::InstanceId,
    pred: impl Fn(&InstanceState) -> bool,
) -> InstanceState {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let s = d.status(tenant, id).await.unwrap().state;
        if pred(&s) {
            return s;
        }
        assert!(Instant::now() < deadline, "the state stayed {}", s.name());
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

struct Fixture {
    world: World,
    driver: Arc<dyn RuntimeDriver>,
    usage: Arc<MemoryUsage>,
}

#[async_trait]
impl DriverFixture for Fixture {
    fn driver(&self) -> Arc<dyn RuntimeDriver> {
        self.driver.clone()
    }
    fn usage(&self) -> Arc<MemoryUsage> {
        self.usage.clone()
    }
    async fn program_image(&self, identity: &WorkloadIdentity) -> Result<ImageRef, String> {
        Ok(self.world.image(identity))
    }
    fn timing(&self) -> Timing {
        Timing {
            idle_timeout: Duration::from_secs(1),
            health_interval: Duration::from_millis(200),
            // activeDeadlineSeconds counts whole seconds.
            job_timeout: Duration::from_secs(1),
            wait: Duration::from_secs(15),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_kubernetes_driver_passes_the_conformance_suite() {
    let world = world(true).await;
    let usage = Arc::new(MemoryUsage::new());
    let driver: Arc<dyn RuntimeDriver> = Arc::new(world.driver(usage.clone()));
    let f = Fixture {
        world,
        driver,
        usage,
    };
    let report = conformance::run(&f).await;
    eprintln!(
        "conformance [kubernetes, fake API server]\n{}",
        report.summary()
    );
    report.assert_ok();
    // Every call stayed in the namespace and within the Role (the fake refuses the rest; a
    // refused call would have failed a case), and no Secret was ever read.
    let calls = f.world.fake.calls.lock().unwrap().clone();
    assert!(
        calls.iter().all(|(ns, _, _)| ns == NS),
        "a call outside {NS}"
    );
    assert!(
        !calls
            .iter()
            .any(|(_, v, r)| r == "secrets" && matches!(v.as_str(), "get" | "list" | "watch")),
        "the driver read a Secret"
    );
}

/// The pod is the sandbox of rule 1 in gVisor, and no secret value is outside the Secret: not in
/// the pod, not in the record.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_pod_is_a_restricted_gvisor_sandbox_without_secret_values() {
    let w = world(false).await;
    let d = w.driver(Arc::new(MemoryUsage::new()));
    let id = identity();
    let secret = format!("s3cr3t-{}", uuid::Uuid::now_v7().simple());
    let mut spec = w.spec(id, &[Step::Env("API_KEY".into()), Step::Serve]);
    spec.env = vec![
        EnvVar::secret("API_KEY", SecretValue::new(secret.clone())),
        EnvVar::plain("MODE", "visible"),
    ];
    spec.previous_secrets = vec![SecretValue::new("old-value-of-the-secret")];
    let token = spec.workload_token.expose().to_owned();
    let s = d.start(spec).await.unwrap();
    let pod = w
        .fake
        .pod(NS, &ruum_runtime::kube::pod_name(s.id, 1))
        .unwrap();
    let p = |ptr: &str| pod.pointer(ptr).cloned().unwrap_or_default();
    assert_eq!(p("/spec/runtimeClassName"), "gvisor");
    assert_eq!(p("/spec/automountServiceAccountToken"), false);
    assert_eq!(p("/spec/enableServiceLinks"), false);
    assert_eq!(p("/spec/hostNetwork"), false);
    assert_eq!(p("/spec/restartPolicy"), "Never");
    assert_eq!(p("/spec/dnsPolicy"), "None");
    assert_eq!(p("/spec/securityContext/runAsNonRoot"), true);
    assert_eq!(p("/spec/securityContext/runAsUser"), 10001);
    assert_eq!(
        p("/spec/securityContext/seccompProfile/type"),
        "RuntimeDefault"
    );
    let c = "/spec/containers/0";
    assert_eq!(
        p(&format!("{c}/securityContext/readOnlyRootFilesystem")),
        true
    );
    assert_eq!(
        p(&format!("{c}/securityContext/allowPrivilegeEscalation")),
        false
    );
    assert_eq!(
        p(&format!("{c}/securityContext/capabilities/drop")),
        serde_json::json!(["ALL"])
    );
    assert_eq!(p(&format!("{c}/resources/limits/cpu")), "250m");
    assert_eq!(p(&format!("{c}/resources/limits/memory")), "64Mi");
    assert_eq!(
        p("/spec/hostAliases/0/hostnames/0"),
        "ruum-egress",
        "the egress proxy by /etc/hosts"
    );
    let env = p(&format!("{c}/env"));
    let env_s = env.to_string();
    assert!(env_s.contains("\"HTTPS_PROXY\""), "{env_s}");
    assert!(env_s.contains("http://ruum-egress:3128"), "{env_s}");
    assert!(env_s.contains("secretKeyRef"), "{env_s}");
    // No secret value in any pod or record (the Secret holds them).
    let all: String = w
        .fake
        .pods()
        .into_iter()
        .chain(w.fake.config_maps())
        .map(|v| v.to_string())
        .collect();
    for (what, v) in [
        ("the secret", secret.as_str()),
        ("the token", token.as_str()),
        ("the previous secret", "old-value-of-the-secret"),
    ] {
        assert!(!all.contains(v), "{what} is in a pod or a record");
    }
    assert!(all.contains("visible"), "the plain value is in the pod");
    // The Secret exists while the instance lives and goes with it.
    let name = ruum_runtime::kube::object_name(s.id);
    assert!(w.fake.secret_names().contains(&name));
    d.stop(id.tenant, s.id).await.unwrap();
    assert!(!w.fake.secret_names().contains(&name), "the Secret stayed");
}

/// A pod that the API server stored with another setting (an admission step changed it) is
/// refused and removed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_changed_pod_is_refused() {
    let w = world(false).await;
    let d = w.driver(Arc::new(MemoryUsage::new()));
    for t in [Tamper::AddCapability, Tamper::DropRuntimeClass] {
        *w.fake.tamper.lock().unwrap() = t;
        let err = d
            .start(w.spec(identity(), &[Step::Serve]))
            .await
            .unwrap_err();
        assert_eq!(err.code(), "internal", "{t:?}: {err}");
        assert!(w.fake.pods().is_empty(), "{t:?}: the pod stayed");
        assert!(w.fake.config_maps().is_empty(), "{t:?}: the record stayed");
        assert!(w.fake.secret_names().is_empty(), "{t:?}: the Secret stayed");
    }
}

/// A second driver (another server process, or the same after a restart) answers for the
/// instances of the first: status, stop. It reads no log it cannot redact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_server_process_adopts_the_instances() {
    let w = world(false).await;
    let a = w.driver(Arc::new(MemoryUsage::new()));
    let b_usage = Arc::new(MemoryUsage::new());
    let b = w.driver(b_usage.clone());
    let id = identity();
    let s = a
        .start(w.spec(id, &[Step::Print("hi".into()), Step::Serve]))
        .await
        .unwrap();
    wait_state(&b, id.tenant, s.id, |x| *x == InstanceState::Running).await;
    assert_eq!(
        b.status(TenantId::new(), s.id).await.unwrap_err().code(),
        "instance_not_found"
    );
    assert_eq!(
        b.logs(id.tenant, s.id, None, 10).await.unwrap_err().code(),
        "runtime_unavailable",
        "a process that does not know the values reads no log"
    );
    b.stop(id.tenant, s.id).await.unwrap();
    assert_eq!(
        a.status(id.tenant, s.id).await.unwrap().state,
        InstanceState::Stopped
    );
    // The process that made the change charged the run, once.
    assert!(b_usage.records().iter().any(|r| matches!(r,
        RuntimeUsage::Instance { instance, .. } if *instance == s.id)));
    assert!(w.fake.pods().is_empty());
}

/// The pod's address is registered at the egress proxy before the instance runs, unregistered
/// before the pod goes; a failed unregistration keeps the pod until the sweep gets it through.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_egress_registration_follows_the_pod() {
    let w = world(false).await;
    let d = w.driver(Arc::new(MemoryUsage::new()));
    let id = identity();
    let s = d.start(w.spec(id, &[Step::Serve])).await.unwrap();
    wait_state(&d, id.tenant, s.id, |x| *x == InstanceState::Running).await;
    let pod = ruum_runtime::kube::pod_name(s.id, 1);
    let ip = w.fake.pod(NS, &pod).unwrap()["status"]["podIP"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        w.egress
            .events
            .lock()
            .unwrap()
            .contains(&format!("register {pod} {ip}/32")),
        "{:?}",
        w.egress.events.lock().unwrap()
    );
    assert_eq!(d.upstream(s.id), Some((ip, 8080)), "the gateway's address");
    w.egress.refuse.store(true, Ordering::SeqCst);
    d.stop(id.tenant, s.id).await.unwrap();
    assert!(
        w.fake.pod(NS, &pod).is_some(),
        "the pod went while its address is still registered"
    );
    w.egress.refuse.store(false, Ordering::SeqCst);
    d.sweep().await;
    assert!(
        w.fake.pod(NS, &pod).is_none(),
        "the sweep did not remove the pod"
    );
    assert!(
        w.egress
            .events
            .lock()
            .unwrap()
            .contains(&format!("unregister {pod}"))
    );
}

/// A pod that disappears (a node loss, an eviction) ends its instance as killed; the run is
/// charged once, however many processes look.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_vanished_pod_ends_the_instance() {
    let w = world(false).await;
    let usage = Arc::new(MemoryUsage::new());
    let a = w.driver(usage.clone());
    let b = w.driver(usage.clone());
    let id = identity();
    let s = a.start(w.spec(id, &[Step::Serve])).await.unwrap();
    wait_state(&a, id.tenant, s.id, |x| *x == InstanceState::Running).await;
    w.fake
        .remove_pod(NS, &ruum_runtime::kube::pod_name(s.id, 1));
    let (x, y) = tokio::join!(a.status(id.tenant, s.id), b.status(id.tenant, s.id));
    for st in [x.unwrap().state, y.unwrap().state] {
        match st {
            InstanceState::Exited(e) => assert_eq!(e.reason, ExitReason::Killed),
            other => panic!("the state is {}", other.name()),
        }
    }
    let runs = usage
        .records()
        .iter()
        .filter(|r| matches!(r, RuntimeUsage::Instance { instance, .. } if *instance == s.id))
        .count();
    assert_eq!(runs, 1, "the run was charged {runs} times");
}

/// Without split streams (the API server's feature gate off) the log is one stream, reported as
/// standard output, still redacted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_log_stream_without_split_streams() {
    let w = world(false).await;
    let d = w.driver(Arc::new(MemoryUsage::new()));
    let id = identity();
    let mut spec = w.spec(
        id,
        &[
            Step::Print("out".into()),
            Step::Stderr("err".into()),
            Step::Token,
            Step::Exit(0),
        ],
    );
    spec.kind = WorkloadKind::Job(ruum_core::runtime::JobSpec {
        timeout: Duration::from_secs(10),
    });
    let token = spec.workload_token.expose().to_owned();
    let s = d.start(spec).await.unwrap();
    wait_state(&d, id.tenant, s.id, |x| {
        matches!(x, InstanceState::Exited(_))
    })
    .await;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let logs = d.logs(id.tenant, s.id, None, 100).await.unwrap();
        let texts: Vec<&str> = logs.iter().map(|l| l.text.as_str()).collect();
        if texts.len() >= 3 {
            assert_eq!(texts[..2], ["out", "err"]);
            assert!(
                !texts.iter().any(|t| t.contains(&token)),
                "the token is in the log"
            );
            assert!(
                logs.iter()
                    .all(|l| l.stream == ruum_core::runtime::LogStream::Stdout)
            );
            break;
        }
        assert!(Instant::now() < deadline, "the log is {texts:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
