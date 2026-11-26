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
use ruum_runtime::kube::{LaunchRefusal, RecordKey};
use ruum_runtime::{EgressNetwork, EgressRegistry};
use ruum_runtime::{KubeConfig, KubeDriver, SharedService, repository};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const NS: &str = "ruum-apps";

/// Every registration and unregistration, in order; unregistrations fail while `refuse` is set.
/// With `hold` set, the next registration waits (after `held`) until `release`: a registration
/// in flight while another process acts.
#[derive(Default)]
struct Recording {
    events: Mutex<Vec<String>>,
    refuse: AtomicBool,
    hold: AtomicBool,
    held: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl Recording {
    /// The events about one pod.
    fn of(&self, pod: &str) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.split(' ').nth(1) == Some(pod))
            .cloned()
            .collect()
    }
}

#[async_trait]
impl EgressRegistry for Recording {
    async fn register(&self, n: &EgressNetwork) -> Result<(), RuntimeError> {
        if self.hold.swap(false, Ordering::SeqCst) {
            self.held.notify_one();
            self.release.notified().await;
        }
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
    /// The driver behind the test's supervisor listener: the newest one `driver` made (in
    /// production every server replica answers it).
    listener: Arc<Mutex<Option<Arc<KubeDriver>>>>,
    _dir: TempDir,
}

/// The supervisor listener of the server, as `crates/ruum-server` serves it, except that the
/// peer address comes from the fake kubelet's header `x-fake-peer` (the pods' addresses are
/// fake).
async fn start_listener(slot: Arc<Mutex<Option<Arc<KubeDriver>>>>) -> u16 {
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    async fn answer(
        slot: Arc<Mutex<Option<Arc<KubeDriver>>>>,
        headers: HeaderMap,
        launch: bool,
    ) -> axum::response::Response {
        let bearer = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or_default()
            .to_owned();
        let peer: std::net::IpAddr = headers
            .get("x-fake-peer")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or([127, 0, 0, 1].into());
        let Some(d) = slot.lock().unwrap().clone() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let r = if launch {
            d.supervisor_launch(&bearer, peer).await
        } else {
            d.supervisor_token(&bearer, peer).await
        };
        match r {
            Ok(body) => {
                (StatusCode::OK, [("content-type", "application/json")], body).into_response()
            }
            Err(LaunchRefusal::Forbidden) => StatusCode::FORBIDDEN.into_response(),
            Err(LaunchRefusal::NotReady) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
            Err(LaunchRefusal::TooMany) => StatusCode::TOO_MANY_REQUESTS.into_response(),
        }
    }
    let app = axum::Router::new()
        .route(
            "/v1/launch",
            axum::routing::post(|State(s), h| answer(s, h, true)),
        )
        .route(
            "/v1/token",
            axum::routing::post(|State(s), h| answer(s, h, false)),
        )
        .with_state(slot);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(l, app).await.unwrap();
    });
    port
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
    cfg.runtime_class_overhead = kube_fake::staging_runtime_class_overhead();
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
    // The sandbox supervisor (I6-20, S-89): the record key, the pinned image, the listener.
    cfg.record_key = Some(RecordKey::new(rand::random()));
    cfg.supervisor_image = format!(
        "registry.test:5000/ruum-supervisor@sha256:{}",
        "5".repeat(64)
    );
    cfg.network_encrypted = true;
    let listener = Arc::new(Mutex::new(None));
    cfg.supervisor_address = "127.0.0.1".into();
    cfg.supervisor_port = start_listener(listener.clone()).await;
    World {
        fake,
        cfg,
        egress: Arc::new(Recording::default()),
        listener,
        _dir: TempDir(dir),
    }
}

impl World {
    fn driver(&self, usage: Arc<MemoryUsage>) -> Arc<KubeDriver> {
        let d = Arc::new(KubeDriver::new(self.cfg.clone(), self.egress.clone(), usage).unwrap());
        *self.listener.lock().unwrap() = Some(d.clone());
        d
    }

    /// A driver whose own sweep never runs (the test calls `sweep`).
    fn quiet_driver(&self) -> Arc<KubeDriver> {
        let mut cfg = self.cfg.clone();
        cfg.sweep_every = Duration::from_secs(3600);
        let d = Arc::new(
            KubeDriver::new(cfg, self.egress.clone(), Arc::new(MemoryUsage::new())).unwrap(),
        );
        *self.listener.lock().unwrap() = Some(d.clone());
        d
    }

    fn service(&self, identity: WorkloadIdentity, idle: Option<Duration>) -> WorkloadSpec {
        let mut spec = self.spec(identity, &[Step::Serve]);
        spec.kind = WorkloadKind::Service(ServiceSpec {
            port: 8080,
            health: None,
            idle_timeout: idle,
        });
        spec
    }

    /// An image of the conformance program: its entrypoint `/ruum-program` (the fake kubelet
    /// runs the steps after it), pushed to the repository of the identity.
    fn image(&self, identity: &WorkloadIdentity) -> ImageRef {
        let config = serde_json::json!({
            "architecture": "amd64", "os": "linux",
            "config": { "Entrypoint": ["/ruum-program"], "Cmd": [] },
            "rootfs": { "type": "layers", "diff_ids": [] },
        })
        .to_string();
        let config_digest = OciDigest::of(config.as_bytes());
        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": { "mediaType": "application/vnd.oci.image.config.v1+json",
                "digest": config_digest.as_str(), "size": config.len() },
            "layers": [],
            "annotations": { "test": uuid::Uuid::now_v7().to_string() },
        })
        .to_string();
        let digest = self.fake.push_image(
            &repository(identity),
            manifest.as_bytes(),
            config.as_bytes(),
        );
        ImageRef::Oci(OciDigest::parse(&digest).unwrap())
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
    d: &KubeDriver,
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
    let driver: Arc<dyn RuntimeDriver> = world.driver(usage.clone());
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
        calls
            .iter()
            .all(|(ns, _, r)| ns == NS || (ns.is_empty() && r == "tokenreviews")),
        "a call outside {NS}"
    );
    assert!(
        !calls.iter().any(|(_, _, r)| r == "secrets"),
        "the driver touched a Secret"
    );
    assert!(f.world.fake.secret_names().is_empty(), "a Secret exists");
    // The pods took their launch data from the supervisor listener; a pod whose instance ended
    // before its launch (a stop right after the start) is refused.
    let launches = f.world.fake.launches.lock().unwrap().clone();
    assert!(
        launches.iter().filter(|(_, c)| *c == 200).count() > launches.len() / 2
            && launches.iter().all(|(_, c)| matches!(c, 200 | 403)),
        "{launches:?}"
    );
}

/// The pod is the sandbox of rule 1 in gVisor with the sandbox supervisor as its first process
/// (I6-20), and no secret value is in any Kubernetes object in the clear: not in the pod, not in
/// the record (sealed), and there is no Secret at all (S-89).
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
    let pod_n = ruum_runtime::kube::pod_name(s.id, 1);
    let pod = w.fake.pod(NS, &pod_n).unwrap();
    let p = |ptr: &str| pod.pointer(ptr).cloned().unwrap_or_default();
    assert_eq!(p("/spec/runtimeClassName"), "gvisor");
    assert_eq!(p("/spec/automountServiceAccountToken"), false);
    assert_eq!(p("/spec/serviceAccountName"), "ruum-sandbox");
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
    // The supervisor: installed by the init container from the pinned image into a read-only
    // volume, the app container's command, the projected token of the audience ruum-supervisor.
    assert_eq!(
        p("/spec/initContainers/0/image"),
        serde_json::json!(w.cfg.supervisor_image)
    );
    assert_eq!(p(&format!("{c}/command/0")), "/ruum/bin/ruum-supervisor");
    let mounts = p(&format!("{c}/volumeMounts")).to_string();
    assert!(
        mounts.contains(r#""mountPath":"/ruum/bin","name":"ruum-bin","readOnly":true"#),
        "{mounts}"
    );
    assert_eq!(
        p("/spec/volumes/3/projected/sources/0/serviceAccountToken/audience"),
        "ruum-supervisor"
    );
    assert_eq!(p(&format!("{c}/terminationMessagePolicy")), "File");
    let env = p(&format!("{c}/env"));
    let env_s = env.to_string();
    assert!(env_s.contains("\"HTTPS_PROXY\""), "{env_s}");
    assert!(env_s.contains("http://ruum-egress:3128"), "{env_s}");
    assert!(
        !env_s.contains("valueFrom") && !env_s.contains("API_KEY"),
        "{env_s}"
    );
    // The app got its value from the supervisor, and its output is redacted before the kubelet
    // writes it (the pod log holds the supervisor's output only).
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let logs = d.logs(id.tenant, s.id, None, 100).await.unwrap();
        if logs.iter().any(|l| l.text == "API_KEY=<hidden secret>") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the app did not get its value: {logs:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let raw = w.fake.raw_log(NS, &pod_n);
    assert!(raw.contains("API_KEY=<hidden secret>"), "{raw}");
    assert!(!raw.contains(&secret), "the node's log holds the secret");
    // No secret value in any pod or record, and no Secret.
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
    assert!(all.contains("\\\"sealed\\\""), "the record has no seal");
    assert!(w.fake.secret_names().is_empty(), "a Secret exists");
    d.stop(id.tenant, s.id).await.unwrap();
}

/// A pod that the API server stored with another setting (an admission step changed it) is
/// refused and removed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_changed_pod_is_refused() {
    let w = world(false).await;
    let d = w.driver(Arc::new(MemoryUsage::new()));
    for t in [
        Tamper::AddCapability,
        Tamper::DropRuntimeClass,
        Tamper::InjectSecretEnv,
        Tamper::ChangeHostAlias,
        Tamper::ServiceAccount,
        Tamper::HostPort,
        Tamper::WritableSupervisor,
        Tamper::TokenAudience,
    ] {
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

/// The runtime class's overhead (review of I6-20, required change 1): the RuntimeClass
/// admission plugin writes it into every pod, so the exhaustive read-back must know it. A driver
/// with the class's overhead starts (the conformance suite runs with it); one without the
/// setting refuses the stored pod; one with another value is refused at the create. Both fail
/// closed and leave nothing behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_runtime_class_overhead_is_checked() {
    let w = world(false).await;
    for overhead in [
        BTreeMap::new(),
        BTreeMap::from([
            ("cpu".to_owned(), "50m".to_owned()),
            ("memory".to_owned(), "128Mi".to_owned()),
        ]),
    ] {
        let mut cfg = w.cfg.clone();
        cfg.runtime_class_overhead = overhead.clone();
        let d =
            Arc::new(KubeDriver::new(cfg, w.egress.clone(), Arc::new(MemoryUsage::new())).unwrap());
        *w.listener.lock().unwrap() = Some(d.clone());
        let err = d
            .start(w.spec(identity(), &[Step::Serve]))
            .await
            .unwrap_err();
        assert_eq!(err.code(), "internal", "{overhead:?}: {err}");
        assert!(w.fake.pods().is_empty(), "{overhead:?}: the pod stayed");
        assert!(
            w.fake.config_maps().is_empty(),
            "{overhead:?}: the record stayed"
        );
    }
    let d = w.driver(Arc::new(MemoryUsage::new()));
    let id = identity();
    let s = d.start(w.spec(id, &[Step::Serve])).await.unwrap();
    let pods = w.fake.pods();
    assert_eq!(pods.len(), 1);
    assert_eq!(
        pods[0].pointer("/spec/overhead"),
        Some(&serde_json::json!({ "cpu": "50m", "memory": "64Mi" }))
    );
    d.stop(id.tenant, s.id).await.unwrap();
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

/// The last event of a pod at the egress proxy is its unregistration.
fn assert_unregistered(w: &World, pod: &str) {
    let events = w.egress.of(pod);
    assert!(
        events.iter().any(|e| e.starts_with("register ")),
        "{events:?}"
    );
    assert_eq!(
        events.last().map(String::as_str),
        Some(format!("unregister {pod}").as_str()),
        "a registration outlived the run: {events:?}"
    );
}

/// A stop by another process while a registration is in flight leaves no registration behind
/// (review of I6-1, change 1; Kimi finding 2): the record names the registration before it is
/// made, and the registering process reads the record again after it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_during_a_registration_leaves_no_registration() {
    let w = world(false).await;
    let a = w.quiet_driver();
    let b = w.quiet_driver();
    let id = identity();
    w.egress.hold.store(true, Ordering::SeqCst);
    let s = a.start(w.service(id, None)).await.unwrap();
    let pod = ruum_runtime::kube::pod_name(s.id, 1);
    let a2 = a.clone();
    let observer = tokio::spawn(async move {
        loop {
            let st = a2.status(id.tenant, s.id).await.unwrap().state;
            if st != InstanceState::Starting {
                return st;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    tokio::time::timeout(Duration::from_secs(10), w.egress.held.notified())
        .await
        .expect("no registration started");
    b.stop(id.tenant, s.id).await.unwrap();
    w.egress.release.notify_one();
    let st = tokio::time::timeout(Duration::from_secs(10), observer)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(st, InstanceState::Stopped);
    assert_unregistered(&w, &pod);
    assert!(w.fake.pod(NS, &pod).is_none());
}

/// A stop by another process while the sweep registers a running pod again (from its older
/// list) leaves no registration behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_during_a_reregistration_leaves_no_registration() {
    let w = world(false).await;
    let a = w.quiet_driver();
    let b = w.quiet_driver();
    let id = identity();
    let s = a.start(w.service(id, None)).await.unwrap();
    wait_state(a.as_ref(), id.tenant, s.id, |x| {
        *x == InstanceState::Running
    })
    .await;
    let pod = ruum_runtime::kube::pod_name(s.id, 1);
    // Past `reregister_every`, so the sweep registers again.
    tokio::time::sleep(Duration::from_millis(400)).await;
    w.egress.hold.store(true, Ordering::SeqCst);
    let a2 = a.clone();
    let sweep = tokio::spawn(async move { a2.sweep().await });
    tokio::time::timeout(Duration::from_secs(10), w.egress.held.notified())
        .await
        .expect("the sweep did not register again");
    b.stop(id.tenant, s.id).await.unwrap();
    w.egress.release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), sweep)
        .await
        .unwrap()
        .unwrap();
    assert_unregistered(&w, &pod);
}

/// After another process scaled an instance to zero and woke a new run, this process never
/// gives the gateway the earlier run's address (review of I6-1, change 3).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_gateway_never_gets_an_earlier_runs_address() {
    let w = world(false).await;
    let a = w.driver(Arc::new(MemoryUsage::new()));
    let b = w.quiet_driver();
    let id = identity();
    let s = a
        .start(w.service(id, Some(Duration::from_secs(1))))
        .await
        .unwrap();
    wait_state(b.as_ref(), id.tenant, s.id, |x| {
        *x == InstanceState::Running
    })
    .await;
    let first = b.upstream(s.id).expect("the first run's address");
    wait_state(&a, id.tenant, s.id, |x| *x == InstanceState::ScaledToZero).await;
    a.wake(id.tenant, s.id).await.unwrap();
    wait_state(&a, id.tenant, s.id, |x| *x == InstanceState::Running).await;
    let second = a.upstream(s.id).expect("the second run's address");
    assert_ne!(first, second, "the fake gives each pod its own address");
    let st = b.wake(id.tenant, s.id).await.unwrap();
    assert_eq!(st.state, InstanceState::Running);
    assert_eq!(b.upstream(s.id), Some(second));
}

/// A token rotation through a process that does not follow the log is refused, so a new token
/// never reaches a log without its redaction (review of I6-1, change 4).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_process_that_redacts_rotates_the_token() {
    let w = world(false).await;
    let a = w.driver(Arc::new(MemoryUsage::new()));
    let b = w.quiet_driver();
    let id = identity();
    let s = a.start(w.service(id, None)).await.unwrap();
    let err = b
        .rotate_token(id.tenant, s.id, SecretValue::new("tok-new-value-1234"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "runtime_unavailable", "{err}");
    a.rotate_token(id.tenant, s.id, SecretValue::new("tok-new-value-1234"))
        .await
        .unwrap();
}

/// The logs that a process keeps of an instance go after the retention also when another process
/// ended the instance and removed its record (review of I6-1, change 6).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_logs_go_when_another_process_ended_the_instance() {
    let mut w = world(false).await;
    w.cfg.ended_retention = Duration::from_millis(300);
    let a = w.quiet_driver();
    let b = w.quiet_driver();
    let id = identity();
    let s = a.start(w.service(id, None)).await.unwrap();
    wait_state(a.as_ref(), id.tenant, s.id, |x| {
        *x == InstanceState::Running
    })
    .await;
    assert_eq!(a.kept_instances(), 1);
    b.stop(id.tenant, s.id).await.unwrap();
    a.sweep().await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    b.sweep().await;
    assert!(w.fake.config_maps().is_empty(), "b removed the record");
    a.sweep().await;
    assert_eq!(
        a.kept_instances(),
        0,
        "a kept the logs of a removed instance"
    );
}

/// A wake after a failed unregistration of the previous run waits until it passes (Kimi finding
/// 3): a new run never forgets a registration.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wake_waits_for_the_previous_unregistration() {
    let w = world(false).await;
    let a = w.driver(Arc::new(MemoryUsage::new()));
    let id = identity();
    let s = a
        .start(w.service(id, Some(Duration::from_secs(1))))
        .await
        .unwrap();
    wait_state(&a, id.tenant, s.id, |x| *x == InstanceState::Running).await;
    let first = ruum_runtime::kube::pod_name(s.id, 1);
    w.egress.refuse.store(true, Ordering::SeqCst);
    wait_state(&a, id.tenant, s.id, |x| *x == InstanceState::ScaledToZero).await;
    let err = a.wake(id.tenant, s.id).await.unwrap_err();
    assert_eq!(err.code(), "runtime_unavailable", "{err}");
    assert!(w.fake.pod(NS, &first).is_some(), "the registered pod went");
    w.egress.refuse.store(false, Ordering::SeqCst);
    a.wake(id.tenant, s.id).await.unwrap();
    wait_state(&a, id.tenant, s.id, |x| *x == InstanceState::Running).await;
    let second = ruum_runtime::kube::pod_name(s.id, 2);
    let events = w.egress.events.lock().unwrap().clone();
    let un = events
        .iter()
        .position(|e| *e == format!("unregister {first}"));
    let re = events
        .iter()
        .position(|e| e.starts_with(&format!("register {second} ")));
    assert!(
        matches!((un, re), (Some(u), Some(r)) if u < r),
        "{events:?}"
    );
    a.sweep().await;
    assert!(w.fake.pod(NS, &first).is_none(), "the old pod stayed");
}

/// A start that the sweep's first list missed keeps its pod: the record is read again before an
/// orphan pod is deleted (Kimi finding 10, the race fix of `f497d42`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_start_that_the_sweeps_list_missed_keeps_its_pod() {
    let w = world(false).await;
    let d = w.quiet_driver();
    let id = identity();
    // No egress route: no registration names the pod, so only the record's state keeps it.
    let mut spec = w.service(id, None);
    spec.routes = Vec::new();
    let s = d.start(spec).await.unwrap();
    wait_state(d.as_ref(), id.tenant, s.id, |x| {
        *x == InstanceState::Running
    })
    .await;
    w.fake.hide_records_once.store(true, Ordering::SeqCst);
    d.sweep().await;
    assert!(
        w.fake
            .pod(NS, &ruum_runtime::kube::pod_name(s.id, 1))
            .is_some(),
        "the sweep deleted a running pod"
    );
}

/// A short job's log is read also when the job ended between the log request and the pod look
/// (Kimi finding 10, the race fix of `f497d42`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_short_jobs_log_is_read_after_it_ended() {
    let w = world(false).await;
    let d = w.quiet_driver();
    let id = identity();
    let mut spec = w.spec(id, &[Step::Print("only line".into()), Step::Exit(0)]);
    spec.kind = WorkloadKind::Job(ruum_core::runtime::JobSpec {
        timeout: Duration::from_secs(10),
    });
    w.fake.log_not_ready_once.store(true, Ordering::SeqCst);
    let s = d.start(spec).await.unwrap();
    wait_state(d.as_ref(), id.tenant, s.id, |x| {
        matches!(x, InstanceState::Exited(_))
    })
    .await;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let logs = d.logs(id.tenant, s.id, None, 10).await.unwrap();
        if logs.iter().any(|l| l.text == "only line") {
            break;
        }
        assert!(Instant::now() < deadline, "the log was lost: {logs:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The fake's admission is the namespace's rule set (S-83, S-89): the supervisor's pod passes
/// (every test above); a Secret volume, an environment value from any object, `envFrom`,
/// another service account, a projected token of another audience or lifetime, another init
/// container, a writable supervisor volume and another termination message path are refused.
#[test]
fn the_admission_rules_admit_only_the_supervisor_shape() {
    let base = serde_json::json!({
        "metadata": { "labels": { "ruum.instance": uuid::Uuid::now_v7().to_string() } },
        "spec": {
            "runtimeClassName": "gvisor",
            "serviceAccountName": "ruum-sandbox",
            "automountServiceAccountToken": false,
            "initContainers": [{ "name": "ruum-supervisor", "volumeMounts": [{ "name": "ruum-bin", "mountPath": "/ruum/bin" }] }],
            "containers": [{ "name": "app", "env": [{ "name": "MODE", "value": "x" }],
                "terminationMessagePath": "/dev/termination-log", "terminationMessagePolicy": "File",
                "volumeMounts": [{ "name": "ruum-bin", "mountPath": "/ruum/bin", "readOnly": true }] }],
            "volumes": [
                { "name": "ruum-bin", "emptyDir": { "medium": "Memory" } },
                { "name": "ruum-id", "projected": { "sources": [{ "serviceAccountToken": {
                    "audience": "ruum-supervisor", "expirationSeconds": 600, "path": "token" } }] } },
            ],
        },
    });
    assert_eq!(kube_fake::admit(&base), Ok(()));
    let change = |f: &dyn Fn(&mut serde_json::Value)| {
        let mut p = base.clone();
        f(&mut p);
        p
    };
    let cases = [
        (
            "a Secret volume",
            change(&|p| {
                p["spec"]["volumes"][0] =
                    serde_json::json!({ "name": "t", "secret": { "secretName": "ruum-i-0" } })
            }),
        ),
        (
            "an environment value from a Secret",
            change(
                &|p| p["spec"]["containers"][0]["env"] = serde_json::json!([{ "name": "X", "valueFrom": { "secretKeyRef": { "name": "x", "key": "y" } } }]),
            ),
        ),
        (
            "an environment value from a ConfigMap",
            change(
                &|p| p["spec"]["containers"][0]["env"] = serde_json::json!([{ "name": "X", "valueFrom": { "configMapKeyRef": { "name": "x", "key": "y" } } }]),
            ),
        ),
        (
            "envFrom",
            change(&|p| {
                p["spec"]["containers"][0]["envFrom"] =
                    serde_json::json!([{ "secretRef": { "name": "x" } }])
            }),
        ),
        (
            "the default service account",
            change(&|p| p["spec"]["serviceAccountName"] = serde_json::json!("default")),
        ),
        (
            "the server's service account",
            change(&|p| p["spec"]["serviceAccountName"] = serde_json::json!("ruum-server")),
        ),
        (
            "an API audience",
            change(&|p| {
                p["spec"]["volumes"][1]["projected"]["sources"][0]["serviceAccountToken"]["audience"] =
                    serde_json::json!("https://kubernetes.default.svc")
            }),
        ),
        (
            "a long-lived token",
            change(&|p| {
                p["spec"]["volumes"][1]["projected"]["sources"][0]["serviceAccountToken"]["expirationSeconds"] =
                    serde_json::json!(86400)
            }),
        ),
        (
            "a projected Secret",
            change(&|p| {
                p["spec"]["volumes"][1]["projected"]["sources"] =
                    serde_json::json!([{ "secret": { "name": "x" } }])
            }),
        ),
        (
            "a second source",
            change(&|p| {
                p["spec"]["volumes"][1]["projected"]["sources"]
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::json!({ "configMap": { "name": "x" } }))
            }),
        ),
        (
            "another init container",
            change(&|p| p["spec"]["initContainers"][0]["name"] = serde_json::json!("other")),
        ),
        (
            "an init container with more mounts",
            change(&|p| {
                p["spec"]["initContainers"][0]["volumeMounts"]
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::json!({ "name": "ruum-id", "mountPath": "/x" }))
            }),
        ),
        (
            "a writable supervisor",
            change(&|p| {
                p["spec"]["containers"][0]["volumeMounts"][0]["readOnly"] = serde_json::json!(false)
            }),
        ),
        (
            "another termination path",
            change(&|p| {
                p["spec"]["containers"][0]["terminationMessagePath"] = serde_json::json!("/tmp/x")
            }),
        ),
        (
            "a fallback to logs",
            change(&|p| {
                p["spec"]["containers"][0]["terminationMessagePolicy"] =
                    serde_json::json!("FallbackToLogsOnError")
            }),
        ),
        (
            "no gVisor",
            change(&|p| p["spec"]["runtimeClassName"] = serde_json::json!("runc")),
        ),
        (
            "an automounted token",
            change(&|p| p["spec"]["automountServiceAccountToken"] = serde_json::json!(true)),
        ),
        (
            "an ephemeral container",
            change(&|p| p["spec"]["ephemeralContainers"] = serde_json::json!([{ "name": "x" }])),
        ),
        (
            "no init container",
            change(&|p| {
                p["spec"].as_object_mut().unwrap().remove("initContainers");
            }),
        ),
        (
            "no supervisor mount",
            change(&|p| p["spec"]["containers"][0]["volumeMounts"] = serde_json::json!([])),
        ),
        (
            "two projected tokens",
            change(&|p| {
                let mut v = p["spec"]["volumes"][1].clone();
                v["name"] = serde_json::json!("ruum-id2");
                p["spec"]["volumes"].as_array_mut().unwrap().push(v);
            }),
        ),
    ];
    for (what, pod) in cases {
        assert!(kube_fake::admit(&pod).is_err(), "{what} was admitted");
    }
}

/// The launch decision trusts the pod's `status.podIPs` and its app container's `startedAt`
/// (S-89), so the runtime identity may never write a pod or its status (review of I6-20,
/// required change 3; `ops/rbac-check.sh` asserts the same against the live cluster).
#[test]
fn the_runtime_role_writes_no_pod_and_no_pod_status() {
    for (resource, verbs) in kube_fake::role_rules() {
        if resource.starts_with("pods") {
            for v in &verbs {
                assert!(
                    !matches!(v.as_str(), "update" | "patch" | "*"),
                    "the runtime Role may {v} {resource}"
                );
            }
        }
        assert!(
            !matches!(resource.as_str(), "pods/status" | "pods/*" | "*"),
            "the runtime Role names {resource}"
        );
    }
}

/// The runtime identity has no verb on Secrets at all (S-89: no Secret holds a value, so none is
/// needed): create, get, list, watch, patch, update and delete are refused by its Role, and the
/// namespace admits no Secret anyway.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_runtime_identity_has_no_verb_on_secrets() {
    let w = world(false).await;
    let d = w.driver(Arc::new(MemoryUsage::new()));
    let s = d.start(w.service(identity(), None)).await.unwrap();
    let name = ruum_runtime::kube::object_name(s.id);
    let base = format!("{}/api/v1/namespaces/{NS}/secrets", w.cfg.api_url);
    let one = format!("{base}/{name}");
    let http = reqwest::Client::new();
    let body =
        serde_json::json!({ "apiVersion": "v1", "kind": "Secret", "metadata": { "name": name } });
    let calls = [
        ("create", http.post(&base).json(&body)),
        ("get", http.get(&one)),
        ("list", http.get(&base)),
        ("watch", http.get(&base).query(&[("watch", "true")])),
        (
            "patch",
            http.patch(&one)
                .header("content-type", "application/merge-patch+json")
                .body("{}"),
        ),
        ("update", http.put(&one).json(&body)),
        ("delete", http.delete(&one)),
    ];
    for (what, req) in calls {
        let r = req.bearer_auth(TOKEN).send().await.unwrap();
        assert_eq!(r.status().as_u16(), 403, "{what} of a Secret is allowed");
    }
    assert!(
        kube_fake::admit_secret(&body).is_err(),
        "a Secret was admitted"
    );
    assert!(w.fake.secret_names().is_empty());
}

/// Waits until the fake kubelet recorded the launch of `pod` (its status code).
async fn wait_launched(w: &World, pod: &str) -> u16 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some((_, c)) = w
            .fake
            .launches
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(p, _)| p == pod)
        {
            return *c;
        }
        assert!(Instant::now() < deadline, "no launch of {pod}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

impl World {
    /// The record ConfigMap of an instance, as the fake stores it.
    fn record(&self, id: ruum_core::runtime::InstanceId) -> serde_json::Value {
        let name = ruum_runtime::kube::object_name(id);
        self.fake
            .config_maps()
            .into_iter()
            .find(|c| c["metadata"]["name"] == serde_json::json!(name))
            .expect("the record")
    }

    /// Replaces the record's data as a holder of the runtime identity's token can (`PUT` with
    /// the current resourceVersion).
    async fn put_record_data(&self, id: ruum_core::runtime::InstanceId, data: serde_json::Value) {
        let mut cm = self.record(id);
        cm["data"] = data;
        let url = format!(
            "{}/api/v1/namespaces/{NS}/configmaps/{}",
            self.cfg.api_url,
            ruum_runtime::kube::object_name(id)
        );
        let r = reqwest::Client::new()
            .put(url)
            .bearer_auth(TOKEN)
            .json(&cm)
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "{}", r.status());
    }

    /// Changes the record's JSON (`data.record`) with `f` and writes it back.
    async fn change_record(
        &self,
        id: ruum_core::runtime::InstanceId,
        f: impl Fn(&mut serde_json::Value),
    ) {
        let cm = self.record(id);
        let mut rec: serde_json::Value =
            serde_json::from_str(cm["data"]["record"].as_str().unwrap()).unwrap();
        f(&mut rec);
        self.put_record_data(id, serde_json::json!({ "record": rec.to_string() }))
            .await;
    }

    /// A projected token for `pod` (its name and uid now) and the pod's address, as the kubelet
    /// gives them to the pod.
    fn pod_token(&self, pod: &str) -> (String, std::net::IpAddr) {
        let p = self.fake.pod(NS, pod).expect("the pod");
        let tok = self.fake.issue(kube_fake::Issued {
            audience: "ruum-supervisor".into(),
            namespace: NS.into(),
            service_account: "ruum-sandbox".into(),
            pod: pod.into(),
            pod_uid: p["metadata"]["uid"].as_str().unwrap().into(),
            exp: time::OffsetDateTime::now_utc().unix_timestamp() + 600,
        });
        let ip = p["status"]["podIP"].as_str().unwrap().parse().unwrap();
        (tok, ip)
    }

    /// Makes a pod as a holder of the runtime identity's token can (`POST`).
    async fn create_pod(&self, pod: &serde_json::Value) -> u16 {
        let mut body = pod.clone();
        if let Some(m) = body["metadata"].as_object_mut() {
            for k in ["uid", "resourceVersion", "namespace"] {
                m.remove(k);
            }
        }
        body.as_object_mut().unwrap().remove("status");
        reqwest::Client::new()
            .post(format!("{}/api/v1/namespaces/{NS}/pods", self.cfg.api_url))
            .bearer_auth(TOKEN)
            .json(&body)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    }
}

/// S-89, the binding the pod's creator cannot forge: a holder of the runtime identity's token
/// deletes a running sandbox pod and makes it again, the same in every byte (it passes the
/// admission rules and the exhaustive read-back). The new pod has another uid, so the kubelet's
/// token names a pod that the seal does not: its launch is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pod_made_again_by_the_runtime_identity_gets_no_launch_data() {
    let w = world(false).await;
    let d = w.quiet_driver();
    let id = identity();
    let mut spec = w.spec(id, &[Step::Env("API_KEY".into()), Step::Serve]);
    let secret = format!("victim-{}", uuid::Uuid::now_v7().simple());
    spec.env = vec![EnvVar::secret("API_KEY", SecretValue::new(secret.clone()))];
    let s = d.start(spec).await.unwrap();
    let pod_n = ruum_runtime::kube::pod_name(s.id, 1);
    assert_eq!(wait_launched(&w, &pod_n).await, 200);
    let honest = w.fake.pod(NS, &pod_n).unwrap();
    let url = format!("{}/api/v1/namespaces/{NS}/pods/{pod_n}", w.cfg.api_url);
    let r = reqwest::Client::new()
        .delete(url)
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
    w.fake.launches.lock().unwrap().clear();
    assert_eq!(w.create_pod(&honest).await, 201, "the copy is admitted");
    assert_eq!(
        wait_launched(&w, &pod_n).await,
        403,
        "the copy got the launch data"
    );
    let raw = w.fake.raw_log(NS, &pod_n);
    assert!(!raw.contains(&secret), "{raw}");
    // A pod under another name with the victim's labels gets nothing either.
    let mut other = honest.clone();
    other["metadata"]["name"] = serde_json::json!(format!("{pod_n}9"));
    assert_eq!(w.create_pod(&other).await, 201);
    assert_eq!(wait_launched(&w, &format!("{pod_n}9")).await, 403);
}

/// S-89: the launch data is given once per pod, only to the pod's own address, only for a token
/// of the sandbox ServiceAccount with the audience ruum-supervisor; a token request comes only
/// after the launch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_launch_is_once_per_pod_from_its_address_and_audience() {
    let w = world(false).await;
    let d = w.quiet_driver();
    let id = identity();
    let s = d.start(w.service(id, None)).await.unwrap();
    let pod_n = ruum_runtime::kube::pod_name(s.id, 1);
    assert_eq!(wait_launched(&w, &pod_n).await, 200);
    let (tok, ip) = w.pod_token(&pod_n);
    assert_eq!(
        d.supervisor_launch(&tok, ip).await.unwrap_err(),
        LaunchRefusal::Forbidden,
        "a second launch"
    );
    assert!(
        d.supervisor_token(&tok, ip).await.is_ok(),
        "the token after the launch"
    );
    let elsewhere: std::net::IpAddr = "10.9.9.9".parse().unwrap();
    assert_eq!(
        d.supervisor_token(&tok, elsewhere).await.unwrap_err(),
        LaunchRefusal::Forbidden,
        "another address"
    );
    let p = w.fake.pod(NS, &pod_n).unwrap();
    let uid = p["metadata"]["uid"].as_str().unwrap().to_owned();
    for (what, aud, sa) in [
        ("another audience", "kubernetes", "ruum-sandbox"),
        ("another ServiceAccount", "ruum-supervisor", "default"),
    ] {
        let bad = w.fake.issue(kube_fake::Issued {
            audience: aud.into(),
            namespace: NS.into(),
            service_account: sa.into(),
            pod: pod_n.clone(),
            pod_uid: uid.clone(),
            exp: time::OffsetDateTime::now_utc().unix_timestamp() + 600,
        });
        assert_eq!(
            d.supervisor_token(&bad, ip).await.unwrap_err(),
            LaunchRefusal::Forbidden,
            "{what}"
        );
    }
    assert_eq!(
        d.supervisor_token("not-a-token", ip).await.unwrap_err(),
        LaunchRefusal::Forbidden
    );
}

/// S-89: a writer of ConfigMaps changes a fixed field of the record (the image), or moves the
/// seal of another instance into it: the seal no longer matches and every request is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_changed_record_or_a_moved_seal_is_refused() {
    let w = world(false).await;
    let d = w.quiet_driver();
    let (a, b) = (identity(), identity());
    let sa = d.start(w.service(a, None)).await.unwrap();
    let sb = d.start(w.service(b, None)).await.unwrap();
    let (pa, pb) = (
        ruum_runtime::kube::pod_name(sa.id, 1),
        ruum_runtime::kube::pod_name(sb.id, 1),
    );
    assert_eq!(wait_launched(&w, &pa).await, 200);
    assert_eq!(wait_launched(&w, &pb).await, 200);
    let (ta, ia) = w.pod_token(&pa);
    let (tb, ib) = w.pod_token(&pb);
    assert!(d.supervisor_token(&ta, ia).await.is_ok());
    w.change_record(sa.id, |r| {
        r["image"] = serde_json::json!("registry.test:5000/ruum/evil/evil@sha256:00")
    })
    .await;
    assert_eq!(
        d.supervisor_token(&ta, ia).await.unwrap_err(),
        LaunchRefusal::Forbidden,
        "a changed image"
    );
    let sealed_a: serde_json::Value =
        serde_json::from_str(w.record(sa.id)["data"]["record"].as_str().unwrap()).unwrap();
    w.change_record(sb.id, |r| r["sealed"] = sealed_a["sealed"].clone())
        .await;
    assert_eq!(
        d.supervisor_token(&tb, ib).await.unwrap_err(),
        LaunchRefusal::Forbidden,
        "a moved seal"
    );
}

/// After a wake the new pod's supervisor redacts the tokens that rotations replaced (they stay
/// valid until their expiry): the seal keeps them and the launch answer names them (Kimi
/// re-check of I6-20, finding 3).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_woken_pod_redacts_the_earlier_tokens() {
    let w = world(false).await;
    let d = w.driver(Arc::new(MemoryUsage::new()));
    let id = identity();
    // Idle after 3 s: the rotations end well before the scale to zero.
    let spec = w.service(id, Some(Duration::from_secs(3)));
    let first = spec.workload_token.expose().to_owned();
    let s = d.start(spec).await.unwrap();
    wait_state(&d, id.tenant, s.id, |x| *x == InstanceState::Running).await;
    let second = format!("tok-second-{}", uuid::Uuid::now_v7().simple());
    let third = format!("tok-third-{}", uuid::Uuid::now_v7().simple());
    for t in [&second, &third] {
        d.rotate_token(id.tenant, s.id, SecretValue::new(t.clone()))
            .await
            .unwrap();
    }
    let st = wait_state(&d, id.tenant, s.id, |x| {
        matches!(x, InstanceState::ScaledToZero | InstanceState::Exited(_))
    })
    .await;
    assert_eq!(
        st,
        InstanceState::ScaledToZero,
        "{st:?} {:?}",
        w.fake.launches.lock().unwrap()
    );
    w.fake.skip_launch_once.store(true, Ordering::SeqCst);
    d.wake(id.tenant, s.id).await.unwrap();
    let pod_n = ruum_runtime::kube::pod_name(s.id, 2);
    let deadline = Instant::now() + Duration::from_secs(10);
    while w
        .fake
        .pod(NS, &pod_n)
        .and_then(|p| p.pointer("/status/podIP").cloned())
        .is_none()
    {
        assert!(Instant::now() < deadline, "the woken pod did not run");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (tok, ip) = w.pod_token(&pod_n);
    let deadline = Instant::now() + Duration::from_secs(10);
    let answer = loop {
        match d.supervisor_launch(&tok, ip).await {
            Ok(a) => break a,
            Err(LaunchRefusal::NotReady) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("the woken pod's launch: {e:?}"),
        }
    };
    let answer: serde_json::Value = serde_json::from_slice(&answer).unwrap();
    assert_eq!(answer["token"], serde_json::json!(third));
    let redact: Vec<String> = serde_json::from_value(answer["redact"].clone()).unwrap();
    assert!(
        redact.contains(&first) && redact.contains(&second),
        "the earlier tokens are not redacted after the wake"
    );
    d.stop(id.tenant, s.id).await.unwrap();
}

/// A token rotation seals the new token into the record; the supervisor gets it at its next
/// token request (the conformance suite checks that the app reads it, redacted). An older seal
/// put back by a writer of the record is refused (S-89 rollback).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rotation_reseals_and_an_older_seal_is_refused() {
    let w = world(false).await;
    let d = w.quiet_driver();
    let id = identity();
    let s = d.start(w.service(id, None)).await.unwrap();
    let pod_n = ruum_runtime::kube::pod_name(s.id, 1);
    assert_eq!(wait_launched(&w, &pod_n).await, 200);
    let before = w.record(s.id);
    let new = format!("tok-rotated-{}", uuid::Uuid::now_v7().simple());
    d.rotate_token(id.tenant, s.id, SecretValue::new(new.clone()))
        .await
        .unwrap();
    let after = w.record(s.id);
    assert_ne!(before["data"], after["data"], "the seal did not change");
    assert!(
        !after.to_string().contains(&new),
        "the new token is in the record in the clear"
    );
    let (tok, ip) = w.pod_token(&pod_n);
    let answer = d
        .supervisor_token(&tok, ip)
        .await
        .expect("the current token");
    let answer: serde_json::Value = serde_json::from_slice(&answer).unwrap();
    assert_eq!(answer["token"], serde_json::json!(new));
    // The seal of before the rotation, put back (a writer of ConfigMaps): refused.
    w.put_record_data(s.id, before["data"].clone()).await;
    assert_eq!(
        d.supervisor_token(&tok, ip).await.unwrap_err(),
        LaunchRefusal::Forbidden
    );
}

/// S-91: the running instances' log rings of one process stay within the live budget and each
/// tenant's share as lines arrive; the sweep brings the total under the budget by taking from
/// the tenant with the most bytes, so a quiet tenant keeps its lines.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_logs_stay_within_the_budget() {
    let mut w = world(false).await;
    w.cfg.max_live_log_bytes = 96 * 1024;
    w.cfg.max_tenant_live_log_bytes = 48 * 1024;
    let d = w.quiet_driver();
    let quiet = identity();
    let q = d
        .start(w.spec(quiet, &[Step::Print("quiet line".into()), Step::Serve]))
        .await
        .unwrap();
    let line = "n".repeat(1000);
    let mut steps: Vec<Step> = (0..60).map(|_| Step::Print(line.clone())).collect();
    steps.push(Step::Serve);
    let (a, b) = (identity(), identity());
    let mut ids = Vec::new();
    for t in [a, b] {
        for _ in 0..4 {
            ids.push((t, d.start(w.spec(t, &steps)).await.unwrap().id));
        }
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    // Every program printed its 60 lines (the pod logs hold them; the driver's rings keep what
    // the budget allows).
    for (_, i) in &ids {
        let pod = ruum_runtime::kube::pod_name(*i, 1);
        while w.fake.raw_log(NS, &pod).lines().count() < 60 {
            assert!(Instant::now() < deadline, "the lines did not arrive");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    for t in [a, b] {
        assert!(
            d.live_log_bytes(Some(t.tenant)) <= 48 * 1024,
            "{}",
            d.live_log_bytes(Some(t.tenant))
        );
    }
    assert!(d.live_log_bytes(None) <= 96 * 1024 + 48 * 1024);
    d.sweep().await;
    assert!(
        d.live_log_bytes(None) <= 96 * 1024,
        "{}",
        d.live_log_bytes(None)
    );
    assert_eq!(
        d.logs(quiet.tenant, q.id, None, 10).await.unwrap().len(),
        1,
        "the quiet tenant lost its line"
    );
}

/// One tenant cannot take every instance slot of a process with instances scaled to zero (Kimi
/// run 15, I6-1 finding 5; backlog R2A-9): past its share its start is `unavailable`, and another
/// tenant still starts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_tenant_cannot_take_every_slot() {
    let mut w = world(false).await;
    w.cfg.max_instances = 4;
    w.cfg.max_tenant_instances = 2;
    let d = w.driver(Arc::new(MemoryUsage::new()));
    let a = identity();
    let mut ids = Vec::new();
    for _ in 0..2 {
        let s = d
            .start(w.service(a, Some(Duration::from_millis(200))))
            .await
            .unwrap();
        ids.push(s.id);
    }
    for i in &ids {
        wait_state(&d, a.tenant, *i, |x| *x == InstanceState::ScaledToZero).await;
    }
    for _ in 0..2 {
        let err = d
            .start(w.service(a, Some(Duration::from_millis(200))))
            .await
            .unwrap_err();
        assert_eq!(err.code(), "runtime_unavailable", "{err}");
    }
    let b = identity();
    for _ in 0..2 {
        d.start(w.service(b, None)).await.unwrap();
    }
}

/// Starts at the same time cannot pass a tenant's share together: the slot is taken under the
/// lock of the check (Kimi review of the fix of finding 5).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_starts_keep_the_tenant_share() {
    let mut w = world(false).await;
    w.cfg.max_instances = 8;
    w.cfg.max_tenant_instances = 2;
    let d = Arc::new(w.driver(Arc::new(MemoryUsage::new())));
    let a = identity();
    let specs: Vec<_> = (0..6).map(|_| w.service(a, None)).collect();
    let tasks: Vec<_> = specs
        .into_iter()
        .map(|spec| {
            let d = d.clone();
            tokio::spawn(async move { d.start(spec).await })
        })
        .collect();
    let mut ok = 0;
    for t in tasks {
        match t.await.unwrap() {
            Ok(_) => ok += 1,
            Err(e) => assert_eq!(e.code(), "runtime_unavailable", "{e}"),
        }
    }
    assert_eq!(ok, 2, "{ok} starts passed a share of 2");
}

/// The logs of an instance scaled to zero count to the byte cap (Kimi run 15, I6-1 finding 5):
/// past it they go, and the instance keeps its values (a token rotation still works).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scaled_to_zero_logs_count_to_the_byte_cap() {
    let mut w = world(false).await;
    w.cfg.max_ended_log_bytes = 1;
    let d = w.quiet_driver();
    let id = identity();
    let mut spec = w.spec(id, &[Step::Print("hello from the run".into()), Step::Serve]);
    spec.kind = WorkloadKind::Service(ServiceSpec {
        port: 8080,
        health: None,
        idle_timeout: Some(Duration::from_millis(200)),
    });
    let s = d.start(spec).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while d.logs(id.tenant, s.id, None, 10).await.unwrap().is_empty() {
        assert!(Instant::now() < deadline, "no log line");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    loop {
        d.sweep().await;
        if d.status(id.tenant, s.id).await.unwrap().state == InstanceState::ScaledToZero {
            break;
        }
        assert!(Instant::now() < deadline, "it did not scale to zero");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    d.sweep().await;
    assert!(
        d.logs(id.tenant, s.id, None, 10).await.unwrap().is_empty(),
        "the logs of an instance scaled to zero stayed past the byte cap"
    );
    assert_eq!(d.kept_instances(), 1, "the instance was forgotten");
    d.rotate_token(id.tenant, s.id, SecretValue::new("tok-after-the-cap-1234"))
        .await
        .unwrap();
}

/// The launch comes only within two minutes of the app container's start (Kimi review of I6-20,
/// finding 2): later it can only be the app asking again, which a seal put back on another
/// replica could otherwise hide. The kubelet writes the start; the pod's creator cannot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_launch_is_refused_long_after_the_start() {
    let w = world(false).await;
    let d = w.quiet_driver();
    w.fake.skip_launch_once.store(true, Ordering::SeqCst);
    let s = d.start(w.service(identity(), None)).await.unwrap();
    let pod_n = ruum_runtime::kube::pod_name(s.id, 1);
    let deadline = Instant::now() + Duration::from_secs(10);
    while w
        .fake
        .pod(NS, &pod_n)
        .and_then(|p| p.pointer("/status/podIP").cloned())
        .is_none()
    {
        assert!(Instant::now() < deadline, "the pod did not run");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (tok, ip) = w.pod_token(&pod_n);
    // The kubelet writes the status on its own schedule (Kimi re-check of I6-20, finding 2): no
    // start or no address in it yet means "ask again", never a final refusal.
    let saved = w.fake.pod(NS, &pod_n).unwrap()["status"].clone();
    w.fake.edit_pod_status(NS, &pod_n, |st| {
        st["containerStatuses"][0]["state"] = serde_json::json!({ "waiting": {} });
    });
    assert_eq!(
        d.supervisor_launch(&tok, ip).await.unwrap_err(),
        LaunchRefusal::NotReady,
        "no start in the status yet"
    );
    w.fake.edit_pod_status(NS, &pod_n, |st| {
        st.as_object_mut().unwrap().remove("containerStatuses");
    });
    assert_eq!(
        d.supervisor_launch(&tok, ip).await.unwrap_err(),
        LaunchRefusal::NotReady,
        "no container status yet"
    );
    let running = saved.clone();
    w.fake.edit_pod_status(NS, &pod_n, |st| {
        *st = running;
        st.as_object_mut().unwrap().remove("podIP");
        st["podIPs"] = serde_json::json!([]);
    });
    assert_eq!(
        d.supervisor_launch(&tok, ip).await.unwrap_err(),
        LaunchRefusal::NotReady,
        "no address in the status yet"
    );
    let ended = saved.clone();
    w.fake.edit_pod_status(NS, &pod_n, |st| {
        *st = ended;
        st["containerStatuses"][0]["state"] =
            serde_json::json!({ "terminated": { "exitCode": 0, "reason": "Completed" } });
    });
    assert_eq!(
        d.supervisor_launch(&tok, ip).await.unwrap_err(),
        LaunchRefusal::Forbidden,
        "an app container that ended"
    );
    w.fake.edit_pod_status(NS, &pod_n, |st| *st = saved);
    w.fake.age_pod(NS, &pod_n, 600);
    assert_eq!(
        d.supervisor_launch(&tok, ip).await.unwrap_err(),
        LaunchRefusal::Forbidden,
        "a launch ten minutes after the start"
    );
    w.fake.age_pod(NS, &pod_n, 1);
    assert!(
        d.supervisor_launch(&tok, ip).await.is_ok(),
        "the launch right after the start"
    );
}

/// The driver's settings on staging (`charts/ruum` `templates/server.yaml` and
/// `crates/ruum-server` `wiring.rs`): the namespace, the runtime class and its overhead, the
/// egress proxy, the supervisor listener. The image digest is a placeholder (a dry run pulls
/// nothing).
fn staging_config() -> KubeConfig {
    let mut c = KubeConfig::new("https://kubernetes.default.svc", "ruum-apps", "gvisor");
    c.runtime_class_overhead = kube_fake::staging_runtime_class_overhead();
    c.supervisor_image = format!("ghcr.io/nemecec/ruum-supervisor@sha256:{}", "0".repeat(64));
    c.supervisor_address = "ruum-supervisor.ruum.svc.cluster.local".into();
    c.routes = BTreeMap::from([(
        Route::EgressProxy,
        SharedService {
            address: "ruum-egress.ruum.svc.cluster.local".into(),
            alias: "ruum-egress".into(),
            port: 3128,
        },
    )]);
    c
}

const SAMPLE_POD: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../deploy/staging/k8s/sandbox-sample-pod.json"
);
const SAMPLE_POD_STORED: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../deploy/staging/k8s/sandbox-sample-pod.stored.json"
);

/// The sample pod that `ops/gvisor-check.sh` sends to the API server (a server-side dry run) is
/// the one this driver makes on staging (review of I6-20, required change 2). After a change of
/// the pod, the message holds the new file.
#[test]
fn the_staging_sample_pod_is_the_drivers() {
    let want = format!(
        "{}\n",
        serde_json::to_string_pretty(&ruum_runtime::kube::sample_pod(&staging_config())).unwrap()
    );
    let have = std::fs::read_to_string(SAMPLE_POD).unwrap_or_default();
    assert!(
        have == want,
        "deploy/staging/k8s/sandbox-sample-pod.json is not the driver's pod; the new file:\n--- sample pod ---\n{want}--- end ---"
    );
}

/// What the real API server stored for the sample pod (`ops/gvisor-check.sh` writes it; the
/// orchestrator commits it before the driver is turned on) passes the driver's read-back: every
/// default the API server and its admission plugins add is one the read-back knows. Without the
/// committed answer the gate is the live check (deploy/staging/README.md).
#[test]
fn the_api_servers_answer_to_the_sample_pod_passes_the_read_back() {
    let Ok(text) = std::fs::read_to_string(SAMPLE_POD_STORED) else {
        return;
    };
    let stored: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        ruum_runtime::kube::check_stored_sample(&staging_config(), &stored),
        Ok(())
    );
}
