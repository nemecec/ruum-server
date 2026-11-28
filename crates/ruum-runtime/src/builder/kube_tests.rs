//! The Kubernetes builder (card I6-13) against a small fake API server: the pod's shape, the
//! build channel's rules (token, address, once, cap), every way a build pod ends, and the
//! cleanup. A test "kubelet" plays the pod: it reads the token and the channel from the pod's
//! environment, as the pod's program does, and calls the channel. The image assembly and push
//! are the Docker builder's (tested in `tests/build.rs`); here the registry is unreachable, so a
//! build whose output arrived ends at the registry.

use super::*;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use ruum_core::ids::{NodeId, TenantId};
use ruum_core::runtime::{Environment, Limits, MemoryUsage};

const POD_IP: &str = "10.0.0.9";

#[derive(Default)]
struct Fake {
    pods: Mutex<HashMap<String, Value>>,
    deleted: Mutex<Vec<String>>,
    log: Mutex<String>,
}

type S = Arc<Fake>;

async fn create(State(f): State<S>, axum::Json(mut pod): axum::Json<Value>) -> impl IntoResponse {
    let name = pod["metadata"]["name"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    pod["status"] = json!({ "phase": "Pending", "podIP": POD_IP });
    pod["metadata"]["creationTimestamp"] = json!("2026-01-01T00:00:00Z");
    f.pods.lock().unwrap().insert(name, pod.clone());
    (StatusCode::CREATED, axum::Json(pod))
}

async fn list(State(f): State<S>) -> impl IntoResponse {
    let items: Vec<Value> = f.pods.lock().unwrap().values().cloned().collect();
    axum::Json(json!({ "items": items }))
}

async fn one(State(f): State<S>, Path(name): Path<String>) -> axum::response::Response {
    match f.pods.lock().unwrap().get(&name) {
        Some(p) => axum::Json(p.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn delete(State(f): State<S>, Path(name): Path<String>) -> impl IntoResponse {
    f.pods.lock().unwrap().remove(&name);
    f.deleted.lock().unwrap().push(name);
    StatusCode::OK
}

async fn log(State(f): State<S>) -> impl IntoResponse {
    f.log.lock().unwrap().clone()
}

async fn fake() -> (Arc<Fake>, String) {
    let f = Arc::new(Fake::default());
    let base = "/api/v1/namespaces/ruum-builds/pods";
    let app = Router::new()
        .route(base, post(create).get(list))
        .route(&format!("{base}/{{name}}"), get(one).delete(delete))
        .route(&format!("{base}/{{name}}/log"), get(log))
        .with_state(f.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (f, format!("http://{addr}"))
}

fn cfg(api: &str) -> KubeBuilderConfig {
    let mut c = KubeBuilderConfig::new(
        api,
        "registry.ruum.internal:5000",
        // Nothing listens here: a build whose output arrived ends at the registry.
        "http://127.0.0.1:9",
        "10.0.0.2",
        8092,
        ChannelTls {
            cert_der: vec![0x30, 0x03, 0x02, 0x01, 0x01],
            pin: format!("sha256:{}", "1".repeat(64)),
        },
    );
    c.egress_proxy = Some(SharedService {
        address: "10.0.0.3".into(),
        alias: "ruum-egress".into(),
        port: 3128,
    });
    c
}

fn input(archive: Bytes) -> BuildInput {
    BuildInput {
        identity: WorkloadIdentity {
            tenant: TenantId::new(),
            app: NodeId::new(),
            deployment: uuid::Uuid::now_v7(),
            environment: Environment::Preview,
        },
        runtime: "node22".into(),
        archive,
        limits: Limits {
            cpu_millis: 1000,
            memory_bytes: 512 * 1024 * 1024,
            disk_bytes: 64 * 1024 * 1024,
        },
        timeout: Duration::from_secs(20),
    }
}

/// A tar archive with `app.yaml` (node22) and one file.
fn archive(build: Option<&str>) -> Bytes {
    let mut w = oci::TarWriter::new();
    let mut yaml = "runtime: node22\nstart: node server.js\n".to_owned();
    if let Some(b) = build {
        yaml.push_str(&format!("build: {b}\n"));
    }
    w.file(b"app.yaml", 0o644, yaml.as_bytes());
    w.file(b"server.js", 0o644, b"console.log('hi')\n");
    Bytes::from(w.finish())
}

fn env_of(pod: &Value, name: &str) -> Option<String> {
    pod["spec"]["containers"][0]["env"]
        .as_array()?
        .iter()
        .find(|e| e["name"] == name)
        .and_then(|e| e["value"].as_str())
        .map(str::to_owned)
}

/// Waits for the build pod; its build ID and token.
async fn wait_pod(f: &Fake) -> (String, String, Value) {
    for _ in 0..200 {
        if let Some(p) = f.pods.lock().unwrap().values().next().cloned() {
            let build = p["metadata"]["labels"][LABEL_BUILD]
                .as_str()
                .unwrap()
                .to_owned();
            let token = env_of(&p, "RUUM_BUILD_TOKEN").unwrap();
            return (build, token, p);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no build pod");
}

fn set_status(f: &Fake, status: Value) {
    for p in f.pods.lock().unwrap().values_mut() {
        p["status"] = status.clone();
    }
}

fn ip() -> IpAddr {
    POD_IP.parse().unwrap()
}

fn builder(c: KubeBuilderConfig) -> Arc<KubeBuilder> {
    Arc::new(KubeBuilder::new(c, Arc::new(MemoryUsage::new()), Arc::new(crate::NoEgress)).unwrap())
}

fn chunks(data: Vec<u8>) -> futures::stream::Iter<std::vec::IntoIter<Result<Bytes, String>>> {
    futures::stream::iter(
        data.chunks(1000)
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect::<Vec<_>>(),
    )
}

#[tokio::test]
async fn the_build_pod_is_a_restricted_gvisor_pod_with_a_fixed_environment() {
    let (f, api) = fake().await;
    let b = builder(cfg(&api));
    let task = {
        let b = b.clone();
        tokio::spawn(async move { b.build(input(archive(Some("npm run build")))).await })
    };
    let (_, token, pod) = wait_pod(&f).await;
    let spec = &pod["spec"];
    assert_eq!(spec["runtimeClassName"], "gvisor");
    assert_eq!(spec["serviceAccountName"], SERVICE_ACCOUNT);
    assert_eq!(spec["automountServiceAccountToken"], false);
    assert_eq!(spec["enableServiceLinks"], false);
    assert_eq!(spec["hostNetwork"], false);
    assert_eq!(spec["restartPolicy"], "Never");
    assert_eq!(spec["activeDeadlineSeconds"], 20);
    assert_eq!(spec["dnsPolicy"], "None");
    assert_eq!(spec["hostAliases"][0]["hostnames"][0], "ruum-egress");
    assert_eq!(spec["hostAliases"][0]["ip"], "10.0.0.3");
    let c = &spec["containers"][0];
    assert_eq!(c["securityContext"]["readOnlyRootFilesystem"], true);
    assert_eq!(c["securityContext"]["allowPrivilegeEscalation"], false);
    assert_eq!(c["securityContext"]["runAsUser"], BUILD_UID);
    assert_eq!(c["securityContext"]["capabilities"]["drop"][0], "ALL");
    assert_eq!(c["resources"]["limits"], c["resources"]["requests"]);
    assert_eq!(c["resources"]["limits"]["cpu"], "1000m");
    assert_eq!(c["command"][0], "node");
    assert_eq!(c["command"][2], BUILD_SCRIPT);
    assert!(
        c["image"]
            .as_str()
            .unwrap()
            .starts_with("registry.ruum.internal:5000/ruum-base/node22@sha256:")
    );
    // Only emptyDir volumes, each with a size limit; no Secret, no projected token.
    for v in spec["volumes"].as_array().unwrap() {
        assert!(v["emptyDir"]["sizeLimit"].is_string(), "{v}");
        assert!(v.get("secret").is_none() && v.get("projected").is_none());
    }
    // The environment: the fixed names only; the build command as a value, never in the
    // program's text; no valueFrom.
    let names: Vec<String> = c["env"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            assert!(e.get("valueFrom").is_none());
            e["name"].as_str().unwrap().to_owned()
        })
        .collect();
    for n in &names {
        assert!(
            [
                "HOME",
                "npm_config_cache",
                "npm_config_registry",
                "npm_config_update_notifier",
                "NODE_OPTIONS",
                "RUUM_BUILD_CHANNEL",
                "RUUM_BUILD_TOKEN",
                "RUUM_BUILD_PIN",
                "RUUM_BUILD_CA",
                "RUUM_BUILD_COMMAND",
                "HTTPS_PROXY",
                "HTTP_PROXY",
                "https_proxy",
                "http_proxy",
                "NO_PROXY",
                "no_proxy",
            ]
            .contains(&n.as_str()),
            "{n}"
        );
    }
    assert_eq!(
        env_of(&pod, "RUUM_BUILD_COMMAND").as_deref(),
        Some("npm run build")
    );
    assert!(!BUILD_SCRIPT.contains("npm run build"));
    assert!(
        env_of(&pod, "RUUM_BUILD_CHANNEL")
            .unwrap()
            .starts_with("https://10.0.0.2:8092/v1/builds/")
    );
    assert!(
        env_of(&pod, "RUUM_BUILD_CA")
            .unwrap()
            .starts_with("-----BEGIN CERTIFICATE-----")
    );
    assert_eq!(token.len(), 64);
    assert!(env_of(&pod, "NO_PROXY").unwrap().contains("10.0.0.2"));
    task.abort();
}

#[tokio::test]
async fn the_channel_gives_the_source_once_to_the_pod_with_the_token_only() {
    let (f, api) = fake().await;
    let b = builder(cfg(&api));
    let task = {
        let b = b.clone();
        tokio::spawn(async move { b.build(input(archive(None))).await })
    };
    let (build, token, _) = wait_pod(&f).await;
    let other: IpAddr = "10.0.0.66".parse().unwrap();
    assert_eq!(
        b.channel_source(&build, "wrong", ip()).await.unwrap_err(),
        ChannelRefusal::Forbidden
    );
    assert_eq!(
        b.channel_source(&build, &token, other).await.unwrap_err(),
        ChannelRefusal::Forbidden,
        "another address with the right token"
    );
    assert_eq!(
        b.channel_source(&uuid::Uuid::now_v7().to_string(), &token, ip())
            .await
            .unwrap_err(),
        ChannelRefusal::NotFound
    );
    let src = b.channel_source(&build, &token, ip()).await.unwrap();
    let out = super::super::output::read_output(src, 1 << 20).unwrap();
    let names: Vec<String> = out
        .entries
        .iter()
        .map(|e| String::from_utf8_lossy(&e.path).into_owned())
        .collect();
    assert!(names.iter().any(|n| n.ends_with("server.js")), "{names:?}");
    assert_eq!(
        b.channel_source(&build, &token, ip()).await.unwrap_err(),
        ChannelRefusal::Conflict,
        "the source is given once"
    );
    // The pod fails: the build fails with the end of its log, the pod is deleted, and the
    // channel forgets the build.
    *f.log.lock().unwrap() = "ruum-build: npm ci\nnpm ERR! the canary failure\n".into();
    set_status(
        &f,
        json!({ "phase": "Failed", "podIP": POD_IP, "containerStatuses": [
            { "state": { "terminated": { "exitCode": 1 } } }
        ] }),
    );
    let err = task.await.unwrap().unwrap_err();
    let RuntimeError::BuildFailed { reason } = &err else {
        panic!("{err:?}");
    };
    assert!(
        reason.contains("code 1") && reason.contains("canary failure"),
        "{reason}"
    );
    assert!(f.pods.lock().unwrap().is_empty());
    assert_eq!(f.deleted.lock().unwrap().len(), 1);
    assert_eq!(
        b.channel_source(&build, &token, ip()).await.unwrap_err(),
        ChannelRefusal::NotFound
    );
}

#[tokio::test]
async fn an_output_over_the_cap_fails_the_build() {
    let (f, api) = fake().await;
    let mut c = cfg(&api);
    c.output_max = 4096;
    let b = builder(c);
    let task = {
        let b = b.clone();
        tokio::spawn(async move { b.build(input(archive(None))).await })
    };
    let (build, token, _) = wait_pod(&f).await;
    b.channel_source(&build, &token, ip()).await.unwrap();
    assert_eq!(
        b.channel_output(&build, &token, ip(), chunks(vec![0u8; 10_000]))
            .await
            .unwrap_err(),
        ChannelRefusal::TooLarge
    );
    let err = task.await.unwrap().unwrap_err();
    assert!(
        matches!(&err, RuntimeError::BuildFailed { reason } if reason.contains("over the cap")),
        "{err:?}"
    );
    assert!(f.pods.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_output_arrives_once_and_goes_to_the_assembly() {
    let (f, api) = fake().await;
    let b = builder(cfg(&api));
    let task = {
        let b = b.clone();
        tokio::spawn(async move { b.build(input(archive(None))).await })
    };
    let (build, token, _) = wait_pod(&f).await;
    let src = b.channel_source(&build, &token, ip()).await.unwrap();
    // The output: the source as the pod would send it back.
    assert_eq!(
        b.channel_output(&build, "wrong", ip(), chunks(src.to_vec()))
            .await
            .unwrap_err(),
        ChannelRefusal::Forbidden
    );
    b.channel_output(&build, &token, ip(), chunks(src.to_vec()))
        .await
        .unwrap();
    assert_eq!(
        b.channel_output(&build, &token, ip(), chunks(src.to_vec()))
            .await
            .unwrap_err(),
        ChannelRefusal::Conflict,
        "the output is taken once"
    );
    set_status(&f, json!({ "phase": "Succeeded", "podIP": POD_IP }));
    // The output was read and accepted; the build stops at the (unreachable) registry.
    let err = task.await.unwrap().unwrap_err();
    assert!(
        matches!(&err, RuntimeError::Unavailable { reason } if reason.starts_with("the registry")),
        "the build reached the registry step: {err:?}"
    );
    assert!(f.pods.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_pod_that_cannot_pull_or_runs_too_long_ends_the_build() {
    for (status, want_timeout) in [
        (
            json!({ "phase": "Pending", "podIP": POD_IP, "containerStatuses": [
                { "state": { "waiting": { "reason": "ImagePullBackOff" } } }
            ] }),
            false,
        ),
        (
            json!({ "phase": "Failed", "reason": "DeadlineExceeded", "podIP": POD_IP }),
            true,
        ),
    ] {
        let (f, api) = fake().await;
        let b = builder(cfg(&api));
        let task = {
            let b = b.clone();
            tokio::spawn(async move { b.build(input(archive(None))).await })
        };
        wait_pod(&f).await;
        set_status(&f, status);
        let err = task.await.unwrap().unwrap_err();
        if want_timeout {
            assert_eq!(err, RuntimeError::BuildTimedOut);
        } else {
            assert!(
                matches!(&err, RuntimeError::Unavailable { reason } if reason.contains("ImagePullBackOff")),
                "{err:?}"
            );
        }
        assert!(f.pods.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn a_dropped_build_deletes_its_pod_and_the_sweep_takes_old_leftovers() {
    let (f, api) = fake().await;
    let b = builder(cfg(&api));
    let task = {
        let b = b.clone();
        tokio::spawn(async move { b.build(input(archive(None))).await })
    };
    let (build, token, _) = wait_pod(&f).await;
    task.abort();
    for _ in 0..100 {
        if f.pods.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        f.pods.lock().unwrap().is_empty(),
        "the drop guard deletes the pod"
    );
    assert_eq!(
        b.channel_source(&build, &token, ip()).await.unwrap_err(),
        ChannelRefusal::NotFound
    );
    // A pod another process left: old enough, it is deleted; a young one stays.
    let mk = |name: &str, created: &str| {
        json!({ "metadata": { "name": name, "creationTimestamp": created,
            "labels": { (crate::LABEL_MANAGED): super::super::MANAGED_BY } } })
    };
    f.pods.lock().unwrap().insert(
        "ruum-b-old".into(),
        mk("ruum-b-old", "2020-01-01T00:00:00Z"),
    );
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    f.pods
        .lock()
        .unwrap()
        .insert("ruum-b-new".into(), mk("ruum-b-new", &now));
    assert_eq!(b.sweep(Duration::from_secs(3600)).await.unwrap(), 1);
    assert!(f.pods.lock().unwrap().contains_key("ruum-b-new"));
}

#[test]
fn the_settings_are_checked() {
    let ok = cfg("https://k");
    assert!(ok.check().is_ok());
    for bad in [
        KubeBuilderConfig {
            per_tenant: 5,
            ..cfg("https://k")
        },
        KubeBuilderConfig {
            node_registry: "http://r/x".into(),
            ..cfg("https://k")
        },
        KubeBuilderConfig {
            registry_url: "r:5000".into(),
            ..cfg("https://k")
        },
        KubeBuilderConfig {
            channel_port: 0,
            ..cfg("https://k")
        },
        KubeBuilderConfig {
            channel_tls: ChannelTls {
                cert_der: vec![],
                pin: "x".into(),
            },
            ..cfg("https://k")
        },
    ] {
        assert!(bad.check().is_err(), "{bad:?}");
    }
    assert!(
        KubeBuilder::new(
            KubeBuilderConfig {
                base_index: format!("sha256:{}", "2".repeat(64)),
                ..cfg("https://k")
            },
            Arc::new(MemoryUsage::new()),
            Arc::new(crate::NoEgress)
        )
        .is_err(),
        "another base pin is refused"
    );
}
