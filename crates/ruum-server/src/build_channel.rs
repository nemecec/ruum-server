//! The build channel (card I6-13; `ruum_runtime::builder::kube`): the build pods of the
//! Kubernetes builder fetch their source here once (`GET /v1/builds/<build>/source`) and send
//! their output tar once (`PUT /v1/builds/<build>/output`), with the build's own token as the
//! bearer. Its own port, only these two routes, the peer address of the connection (never a
//! header). The builder makes every check (`KubeBuilder::channel_source`, `channel_output`):
//! the token, the caller's address against the build pod's, once only, the output cap. This
//! module only maps the answer: `200` with the tar, `204`, or `403`, `404`, `409`, `413`, `503`
//! without a detail.
//!
//! TLS 1.3 with the supervisor listener's identity (derived from the record key, backlog
//! I620-2); the build pods pin its certificate. The callers run untrusted code, so the
//! connections have the supervisor listener's caps ([`crate::supervisor::serve`]) with a longer
//! lifetime for the transfer of an output up to the builder's cap.

use crate::supervisor::Caps;
use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use ruum_runtime::builder::kube::KubeBuilder;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// The connection caps of the build channel: fewer connections than the supervisor listener
/// (a build holds at most one at a time), and a lifetime long enough for an output of the cap.
pub fn caps() -> Caps {
    Caps {
        connections: 256,
        per_peer: 2,
        header_timeout: Duration::from_secs(5),
        lifetime: Duration::from_secs(600),
    }
}

pub fn router(builder: Arc<KubeBuilder>) -> Router {
    Router::new()
        .route("/v1/builds/{build}/source", get(source))
        .route("/v1/builds/{build}/output", put(output))
        // The builder reads the output under its own cap.
        .layer(DefaultBodyLimit::disable())
        .with_state(builder)
}

fn bearer(headers: &HeaderMap) -> String {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default()
        .trim()
        .to_owned()
}

fn refused(status: u16) -> Response {
    StatusCode::from_u16(status)
        .unwrap_or(StatusCode::FORBIDDEN)
        .into_response()
}

async fn source(
    State(builder): State<Arc<KubeBuilder>>,
    Path(build): Path<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    match builder
        .channel_source(&build, &bearer(&headers), peer.ip())
        .await
    {
        Ok(tar) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/x-tar"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            tar,
        )
            .into_response(),
        Err(r) => refused(r.status()),
    }
}

async fn output(
    State(builder): State<Arc<KubeBuilder>>,
    Path(build): Path<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    match builder
        .channel_output(
            &build,
            &bearer(&headers),
            peer.ip(),
            body.into_data_stream(),
        )
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(r) => refused(r.status()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruum_core::runtime::MemoryUsage;
    use ruum_runtime::NoEgress;
    use ruum_runtime::builder::kube::{ChannelTls, KubeBuilderConfig};
    use tower::ServiceExt as _;

    fn builder() -> Arc<KubeBuilder> {
        let cfg = KubeBuilderConfig::new(
            "https://127.0.0.1:1",
            "registry.ruum.internal:5000",
            "http://127.0.0.1:1",
            "10.0.0.2",
            8092,
            ChannelTls {
                cert_der: vec![1, 2, 3],
                pin: format!("sha256:{}", "0".repeat(64)),
            },
        );
        Arc::new(
            KubeBuilder::new(cfg, Arc::new(MemoryUsage::new()), Arc::new(NoEgress))
                .expect("builder"),
        )
    }

    async fn call(method: &str, path: &str, auth: Option<&str>) -> StatusCode {
        let mut req = axum::http::Request::builder().method(method).uri(path);
        if let Some(a) = auth {
            req = req.header(header::AUTHORIZATION, a);
        }
        let mut req = req.body(Body::empty()).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo::<SocketAddr>("10.0.0.9:4000".parse().unwrap()));
        router(builder()).oneshot(req).await.unwrap().status()
    }

    #[tokio::test]
    async fn a_build_that_does_not_run_is_not_found_and_other_routes_do_not_exist() {
        let id = uuid::Uuid::now_v7();
        for (m, p) in [
            ("GET", format!("/v1/builds/{id}/source")),
            ("PUT", format!("/v1/builds/{id}/output")),
            ("GET", "/v1/builds/not-a-uuid/source".to_owned()),
        ] {
            assert_eq!(
                call(m, &p, Some("Bearer x")).await,
                StatusCode::NOT_FOUND,
                "{m} {p}"
            );
            assert_eq!(call(m, &p, None).await, StatusCode::NOT_FOUND, "{m} {p}");
        }
        // Only the two routes and their methods.
        assert_eq!(
            call("POST", &format!("/v1/builds/{id}/source"), None).await,
            StatusCode::METHOD_NOT_ALLOWED
        );
        assert_eq!(call("GET", "/v1/launch", None).await, StatusCode::NOT_FOUND);
    }
}

/// The build pod's program for real (card I6-13): `RUUM_BUILD_SCRIPT_E2E=1` on a host with
/// Docker and the runtime `runsc` (the build server). A fake API server plays the cluster: it
/// runs each build pod as a gVisor container of the pinned `node:22-alpine` platform image,
/// with the pod's command, environment, user and size-limited writable folders. The channel is
/// this module's router on the supervisor listener's TLS identity, so the program's pin, the
/// token, the address check, `npm ci`, the build command and the output upload all run for
/// real. The registry is not there, so a build whose output arrived and was read ends at the
/// registry (`Unavailable`), and a build whose command fails ends with its log.
#[cfg(test)]
mod e2e {
    use super::*;
    use axum::routing::post;
    use ruum_core::ids::{NodeId, TenantId};
    use ruum_core::runtime::{
        BuildInput, Builder, Environment, Limits, MemoryUsage, RuntimeError, WorkloadIdentity,
    };
    use ruum_runtime::NoEgress;
    use ruum_runtime::builder::kube::{ChannelTls, KubeBuilderConfig, LABEL_BUILD};
    use serde_json::{Value, json};
    use std::collections::HashMap;
    use std::process::Command;
    use std::sync::Mutex;

    const IMAGE_REPO: &str = "library/node";

    fn docker(args: &[&str]) -> Result<String, String> {
        let o = Command::new("docker")
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        if o.status.success() {
            Ok(String::from_utf8_lossy(&o.stdout).trim().to_owned())
        } else {
            Err(String::from_utf8_lossy(&o.stderr).trim().to_owned())
        }
    }

    #[derive(Default)]
    struct Kubelet {
        /// pod name -> container name
        pods: Mutex<HashMap<String, String>>,
        label: String,
    }

    type K = Arc<Kubelet>;

    async fn create(
        axum::extract::State(k): axum::extract::State<K>,
        axum::Json(pod): axum::Json<Value>,
    ) -> Response {
        let name = pod["metadata"]["name"].as_str().unwrap().to_owned();
        let c = &pod["spec"]["containers"][0];
        let run = k.label.split('=').nth(1).unwrap_or("i613");
        let container = format!("{run}-{name}");
        let mut args: Vec<String> = vec![
            "run".into(),
            "-d".into(),
            "--name".into(),
            container.clone(),
            "--label".into(),
            k.label.clone(),
            "--runtime".into(),
            "runsc".into(),
            "--read-only".into(),
            "--cap-drop".into(),
            "ALL".into(),
            "--security-opt".into(),
            "no-new-privileges".into(),
            "--user".into(),
            "10001:10001".into(),
            "--memory".into(),
            "1g".into(),
            "--tmpfs".into(),
            "/work:rw,exec,size=512m,mode=1777".into(),
            "--tmpfs".into(),
            "/tmp:rw,exec,size=512m,mode=1777".into(),
            "-w".into(),
            "/work".into(),
        ];
        for e in c["env"].as_array().unwrap() {
            args.push("-e".into());
            args.push(format!(
                "{}={}",
                e["name"].as_str().unwrap(),
                e["value"].as_str().unwrap()
            ));
        }
        // A pod's `command` replaces the image's entrypoint.
        let command = c["command"].as_array().unwrap();
        args.push("--entrypoint".into());
        args.push(command[0].as_str().unwrap().to_owned());
        args.push(c["image"].as_str().unwrap().to_owned());
        for a in &command[1..] {
            args.push(a.as_str().unwrap().to_owned());
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        if let Err(e) = docker(&refs) {
            eprintln!("docker run: {e}");
            return StatusCode::UNPROCESSABLE_ENTITY.into_response();
        }
        k.pods.lock().unwrap().insert(name, container);
        (StatusCode::CREATED, axum::Json(pod)).into_response()
    }

    async fn one(
        axum::extract::State(k): axum::extract::State<K>,
        Path(name): Path<String>,
    ) -> Response {
        let Some(c) = k.pods.lock().unwrap().get(&name).cloned() else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let Ok(out) = docker(&[
            "inspect",
            "-f",
            "{{.State.Status}} {{.State.ExitCode}} {{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
            &c,
        ])
        .inspect_err(|e| eprintln!("docker inspect: {e}")) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let mut parts = out.split(' ');
        let (state, code, ip) = (
            parts.next().unwrap_or(""),
            parts.next().unwrap_or("0").parse::<i64>().unwrap_or(0),
            parts.next().unwrap_or(""),
        );
        let status = match state {
            "running" | "created" => json!({ "phase": "Running", "podIP": ip }),
            _ if code == 0 => json!({ "phase": "Succeeded", "podIP": ip }),
            _ => json!({ "phase": "Failed", "podIP": ip, "containerStatuses": [
                { "state": { "terminated": { "exitCode": code } } }
            ] }),
        };
        axum::Json(json!({ "metadata": { "name": name }, "status": status })).into_response()
    }

    async fn delete(
        axum::extract::State(k): axum::extract::State<K>,
        Path(name): Path<String>,
    ) -> StatusCode {
        if let Some(c) = k.pods.lock().unwrap().remove(&name) {
            let _ = docker(&["rm", "-f", &c]);
        }
        StatusCode::OK
    }

    async fn log(
        axum::extract::State(k): axum::extract::State<K>,
        Path(name): Path<String>,
    ) -> Response {
        let Some(c) = k.pods.lock().unwrap().get(&name).cloned() else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let o = Command::new("docker").args(["logs", &c]).output().unwrap();
        let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&o.stderr));
        text.into_response()
    }

    fn tar(files: &[(&str, &str)]) -> bytes::Bytes {
        let mut w = ruum_runtime::builder::oci::TarWriter::new();
        for (n, d) in files {
            w.file(n.as_bytes(), 0o644, d.as_bytes());
        }
        bytes::Bytes::from(w.finish())
    }

    #[tokio::test]
    async fn the_build_program_runs_under_gvisor_against_the_pinned_channel() {
        if std::env::var("RUUM_BUILD_SCRIPT_E2E").as_deref() != Ok("1") {
            eprintln!("skipped: set RUUM_BUILD_SCRIPT_E2E=1 on a host with Docker and runsc");
            return;
        }
        let label = format!("ruum.test=i613-{}", uuid::Uuid::now_v7().simple());
        let kubelet = Arc::new(Kubelet {
            pods: Mutex::default(),
            label: label.clone(),
        });
        let base = "/api/v1/namespaces/ruum-builds/pods";
        let api = Router::new()
            .route(base, post(create))
            .route(&format!("{base}/{{name}}"), get(one).delete(delete))
            .route(&format!("{base}/{{name}}/log"), get(log))
            .with_state(kubelet.clone());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api_url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, api).await.unwrap() });
        // The channel on the TLS identity of a fresh record key, reachable from the bridge.
        let hex_key = format!(
            "{}{}",
            uuid::Uuid::now_v7().simple(),
            uuid::Uuid::now_v7().simple()
        );
        let key = ruum_runtime::kube::RecordKey::from_hex(&hex_key).unwrap();
        let tls = ruum_runtime::kube::tls::SupervisorTls::derive(&key).unwrap();
        let gateway = docker(&[
            "network",
            "inspect",
            "bridge",
            "-f",
            "{{(index .IPAM.Config 0).Gateway}}",
        ])
        .expect("the bridge network");
        let listener = tokio::net::TcpListener::bind((gateway.as_str(), 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut cfg = KubeBuilderConfig::new(
            &api_url,
            "docker.io",
            "http://127.0.0.1:9",
            &gateway,
            port,
            ChannelTls {
                cert_der: tls.cert_der.clone(),
                pin: tls.pin.clone(),
            },
        );
        cfg.base_repository = IMAGE_REPO.to_owned();
        // The platform of this engine (the build server is arm64; staging's nodes are amd64).
        cfg.node_arch = docker(&["info", "-f", "{{.Architecture}}"]).expect("docker info");
        let builder = Arc::new(
            KubeBuilder::new(cfg, Arc::new(MemoryUsage::new()), Arc::new(NoEgress)).unwrap(),
        );
        let (_stop, stopped) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(crate::supervisor::serve(
            listener,
            router(builder.clone()),
            caps(),
            Some(crate::supervisor::tls_acceptor(&tls).unwrap()),
            async move {
                let _ = stopped.await;
            },
        ));
        let input = |archive| BuildInput {
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
                memory_bytes: 1024 * 1024 * 1024,
                disk_bytes: 256 * 1024 * 1024,
            },
            timeout: Duration::from_secs(300),
        };
        let pkg = r#"{"name":"hello","version":"1.0.0","scripts":{"postinstall":"echo postinstall ran > post.txt"}}"#;
        let lock = r#"{"name":"hello","version":"1.0.0","lockfileVersion":3,"requires":true,"packages":{"":{"name":"hello","version":"1.0.0","hasInstallScript":true}}}"#;
        // A build whose output arrives: it ends at the missing registry, after the output was read.
        let ok = tar(&[
            (
                "app.yaml",
                "runtime: node22\nstart: node server.js\nbuild: test -f post.txt && echo built > dist.txt\n",
            ),
            ("package.json", pkg),
            ("package-lock.json", lock),
            (
                "server.js",
                "require('http').createServer((q,s)=>s.end('hi')).listen(process.env.PORT)\n",
            ),
        ]);
        let err = builder.build(input(ok)).await.unwrap_err();
        let cleanup = || {
            if let Ok(ids) = docker(&["ps", "-aq", "--filter", &format!("label={label}")]) {
                for id in ids.lines().filter(|l| !l.is_empty()) {
                    let _ = docker(&["rm", "-f", id]);
                }
            }
        };
        let at_registry = matches!(&err, RuntimeError::Unavailable { reason } if reason.starts_with("the registry"));
        if !at_registry {
            if let Ok(ids) = docker(&["ps", "-aq", "--filter", &format!("label={label}")]) {
                for id in ids.lines() {
                    let o = Command::new("docker").args(["logs", id]).output().unwrap();
                    eprintln!(
                        "logs: {}{}",
                        String::from_utf8_lossy(&o.stdout),
                        String::from_utf8_lossy(&o.stderr)
                    );
                }
            }
            cleanup();
            panic!("the build did not reach the registry step: {err:?}");
        }
        // A build command that fails: the build fails with the end of its log.
        let bad = tar(&[
            (
                "app.yaml",
                "runtime: node22\nstart: node server.js\nbuild: echo the-canary-line; exit 3\n",
            ),
            ("package.json", pkg),
            ("package-lock.json", lock),
        ]);
        let err = builder.build(input(bad)).await.unwrap_err();
        cleanup();
        let RuntimeError::BuildFailed { reason } = &err else {
            panic!("{err:?}");
        };
        assert!(
            reason.contains("the-canary-line") && reason.contains("code 3"),
            "{reason}"
        );
        assert!(kubelet.pods.lock().unwrap().is_empty(), "every pod deleted");
        let _ = LABEL_BUILD;
    }
}
