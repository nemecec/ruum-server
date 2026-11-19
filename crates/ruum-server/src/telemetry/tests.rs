//! Tests of the telemetry role (card O-1b): the receiver over HTTP and gRPC with the hostile
//! cases of `docs/STEP3.md`, the bounds, the outgoing tenant header (a capturing fake
//! Collector), the switch, and the Collector config.
//!
//! The clients bind to their own loopback addresses (127.0.0.2, 127.0.0.3): the source
//! lookup maps an address to an identity, as the app-network lookup of cards I-4 and I-6 will.

use super::*;
use opentelemetry_proto::tonic::collector::logs::v1::logs_service_client::LogsServiceClient;
use opentelemetry_proto::tonic::collector::trace::v1::trace_service_client::TraceServiceClient;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::logs::v1 as ol;
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1 as ot;
use ruum_core::ids::{NodeId, TenantId};
use ruum_core::privacy::keys::{LocalKekWrapper, MemoryTenantKeyStore, TenantKeys};
use ruum_core::runtime::{Environment, SecretValue, WorkloadIdentity};
use ruum_core::telemetry::lgtm::{LgtmConfig, LgtmStore, TENANT_HEADER, tenant_header};
use ruum_core::telemetry::memory::{MemoryFieldClasses, MemorySecretSource, MemoryTelemetryStore};
use ruum_core::telemetry::source::{MemorySourceAuthenticator, NetworkSourceAuthenticator};
use ruum_core::telemetry::{
    ATTR_TENANT, PipelineIngest, StoredValue, TelemetryFilter, TelemetryStore, attr,
};
use ruum_core::usage::{Charged, MemoryUsageSink, Meter};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const A_IP: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 2);
const B_IP: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 3);
const STRANGER_IP: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 9);

fn identity(env: Environment) -> SourceIdentity {
    SourceIdentity {
        tenant: TenantId::new(),
        app: NodeId::new(),
        deployment: TenantId::new().as_uuid(),
        environment: env,
    }
}

fn workload(id: &SourceIdentity) -> WorkloadIdentity {
    WorkloadIdentity {
        tenant: id.tenant,
        app: id.app,
        deployment: id.deployment,
        environment: id.environment,
    }
}

fn keys() -> TenantKeys {
    TenantKeys::new(
        Arc::new(MemoryTenantKeyStore::new()),
        Arc::new(LocalKekWrapper::from_bytes(&[9u8; 32])),
    )
}

struct World {
    store: Arc<dyn TelemetryStore>,
    usage: Arc<MemoryUsageSink>,
    http: SocketAddr,
    grpc: SocketAddr,
    a: SourceIdentity,
    b: SourceIdentity,
    _stop: tokio::sync::oneshot::Sender<()>,
}

struct Opts {
    budget: BudgetConfig,
    limits: Limits,
    body_timeout: Duration,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            budget: BudgetConfig::default(),
            limits: Limits::default(),
            body_timeout: Duration::from_secs(5),
        }
    }
}

/// A receiver on both listeners: A (a preview) at 127.0.0.2 and B (live, another tenant) at
/// 127.0.0.3 on the listener `local`; nothing else is known.
async fn world(store: Arc<dyn TelemetryStore>, opts: Opts) -> World {
    let a = identity(Environment::Preview);
    let b = identity(Environment::Live);
    let sources = Arc::new(MemorySourceAuthenticator::new());
    let local = ListenerId::new(LOCAL_LISTENER);
    sources.insert(local.clone(), IpAddr::V4(A_IP), workload(&a));
    sources.insert(local.clone(), IpAddr::V4(B_IP), workload(&b));
    let auth = Arc::new(NetworkSourceAuthenticator::new(sources.lookup()));
    let usage = Arc::new(MemoryUsageSink::new());
    let ingest = Arc::new(
        PipelineIngest::new(
            store.clone(),
            Arc::new(MemoryFieldClasses::new()),
            Arc::new(MemorySecretSource::new()),
            keys(),
            usage.clone(),
        )
        .with_limits(opts.limits),
    );
    let receiver = Receiver::new(auth, ingest, FairBudget::new(opts.budget), opts.limits)
        .with_body_timeout(opts.body_timeout);
    let http_l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let grpc_l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (http, grpc) = (http_l.local_addr().unwrap(), grpc_l.local_addr().unwrap());
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let stop = async move {
        let _ = rx.await;
    };
    let (s1, s2) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        stop.await;
        let _ = s1.send(true);
    });
    let (mut w1, mut w2) = (s2.clone(), s2);
    tokio::spawn(serve_http(
        receiver.clone(),
        local.clone(),
        http_l,
        async move {
            let _ = w1.wait_for(|s| *s).await;
        },
    ));
    tokio::spawn(serve_grpc(receiver, local, grpc_l, async move {
        let _ = w2.wait_for(|s| *s).await;
    }));
    World {
        store,
        usage,
        http,
        grpc,
        a,
        b,
        _stop: tx,
    }
}

fn skv(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_owned(),
        value: Some(AnyValue {
            value: Some(any_value::Value::StringValue(value.to_owned())),
        }),
        ..Default::default()
    }
}

/// Attributes that claim another tenant, app, deployment and `live`.
fn forged(other: &SourceIdentity) -> Vec<KeyValue> {
    vec![
        skv("ruum.tenant", &other.tenant.to_string()),
        skv("ruum.app", &other.app.to_string()),
        skv("ruum.deployment", &other.deployment.to_string()),
        skv("ruum.environment", "live"),
    ]
}

fn now_nanos() -> u64 {
    ruum_core::now().unix_timestamp_nanos() as u64
}

fn logs_req(claims: &SourceIdentity, text: &str, n: usize) -> ExportLogsServiceRequest {
    ExportLogsServiceRequest {
        resource_logs: vec![ol::ResourceLogs {
            resource: Some(Resource {
                attributes: forged(claims),
                ..Default::default()
            }),
            scope_logs: vec![ol::ScopeLogs {
                log_records: (0..n)
                    .map(|_| ol::LogRecord {
                        time_unix_nano: now_nanos(),
                        severity_number: 9,
                        body: Some(AnyValue {
                            value: Some(any_value::Value::StringValue(text.to_owned())),
                        }),
                        attributes: forged(claims),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
}

fn traces_req(claims: &SourceIdentity) -> ExportTraceServiceRequest {
    let start = now_nanos();
    let id = TenantId::new().as_uuid();
    ExportTraceServiceRequest {
        resource_spans: vec![ot::ResourceSpans {
            resource: Some(Resource {
                attributes: forged(claims),
                ..Default::default()
            }),
            scope_spans: vec![ot::ScopeSpans {
                spans: vec![ot::Span {
                    trace_id: id.as_bytes().to_vec(),
                    span_id: id.as_bytes()[8..].to_vec(),
                    name: "GET /x".into(),
                    kind: 2,
                    start_time_unix_nano: start,
                    end_time_unix_nano: start + 1000,
                    attributes: forged(claims),
                    events: vec![ot::span::Event {
                        time_unix_nano: start,
                        name: "e".into(),
                        attributes: forged(claims),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
}

fn client(from: Ipv4Addr) -> reqwest::Client {
    reqwest::Client::builder()
        .local_address(IpAddr::V4(from))
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap()
}

/// Posts an OTLP body over HTTP from `from`, with a forged tenant header and query.
async fn post(w: &World, from: Ipv4Addr, path: &str, body: Vec<u8>) -> u16 {
    client(from)
        .post(format!(
            "http://{}{path}?tenant=forged&X-Scope-OrgID=forged",
            w.http
        ))
        .header("Content-Type", "application/x-protobuf")
        .header(TENANT_HEADER, tenant_header(TenantId::new()))
        .header("ruum-tenant", TenantId::new().to_string())
        .body(body)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

async fn grpc_logs(
    w: &World,
    from: Ipv4Addr,
    req: ExportLogsServiceRequest,
    bearer: Option<&str>,
) -> Result<ExportLogsServiceResponse, tonic::Status> {
    let ch = tonic::transport::Endpoint::from_shared(format!("http://{}", w.grpc))
        .unwrap()
        .local_address(Some(IpAddr::V4(from)))
        .connect()
        .await
        .unwrap();
    let mut c = LogsServiceClient::new(ch);
    let mut r = tonic::Request::new(req);
    r.metadata_mut().insert(
        "x-scope-orgid",
        tenant_header(TenantId::new()).parse().unwrap(),
    );
    if let Some(b) = bearer {
        r.metadata_mut()
            .insert("authorization", format!("Bearer {b}").parse().unwrap());
    }
    c.export(r).await.map(|r| r.into_inner())
}

async fn grpc_traces(
    w: &World,
    from: Ipv4Addr,
    req: ExportTraceServiceRequest,
) -> Result<ExportTraceServiceResponse, tonic::Status> {
    let ch = tonic::transport::Endpoint::from_shared(format!("http://{}", w.grpc))
        .unwrap()
        .local_address(Some(IpAddr::V4(from)))
        .connect()
        .await
        .unwrap();
    TraceServiceClient::new(ch)
        .export(tonic::Request::new(req))
        .await
        .map(|r| r.into_inner())
}

/// The sender's identity attributes, and no other `ruum.*` attribute.
fn assert_own(attrs: &[ruum_core::telemetry::StoredAttribute], id: &SourceIdentity) {
    let reserved = attrs.iter().filter(|a| a.key.starts_with("ruum.")).count();
    assert_eq!(reserved, 4);
    assert_eq!(
        attr(attrs, ATTR_TENANT).and_then(StoredValue::as_text),
        Some(id.tenant.to_string().as_str())
    );
    assert_eq!(
        attr(attrs, "ruum.environment").and_then(StoredValue::as_text),
        Some(id.environment.as_str())
    );
}

async fn read_all(w: &World, tenant: TenantId) -> ruum_core::telemetry::ClassifiedBatch {
    w.store
        .read(tenant, &TelemetryFilter::default())
        .await
        .unwrap()
}

fn telemetry_bytes(w: &World, id: &SourceIdentity) -> i64 {
    w.usage
        .total(id.tenant, id.app, Charged::App, Meter::TelemetryBytes)
}

// ---------------------------------------------------------------------------- hostile cases

#[tokio::test]
async fn http_a_forged_identity_is_stored_only_as_the_sender() {
    let w = world(Arc::new(MemoryTelemetryStore::new()), Opts::default()).await;
    // A (a preview) claims to be B (live) in resource and record attributes, a tenant header,
    // a `ruum-tenant` header and the query.
    let code = post(
        &w,
        A_IP,
        "/v1/logs",
        logs_req(&w.b, "from a", 1).encode_to_vec(),
    )
    .await;
    assert_eq!(code, 200);
    let code = post(&w, A_IP, "/v1/traces", traces_req(&w.b).encode_to_vec()).await;
    assert_eq!(code, 200);
    let mine = read_all(&w, w.a.tenant).await;
    assert_eq!(
        (mine.logs.len(), mine.spans.len()),
        (1, 1),
        "positive control"
    );
    assert_own(&mine.logs[0].attributes, &w.a);
    assert_own(&mine.spans[0].attributes, &w.a);
    assert_eq!(mine.logs[0].source, w.a);
    assert!(
        read_all(&w, w.b.tenant).await.is_empty(),
        "B's store has nothing of A"
    );
    assert!(telemetry_bytes(&w, &w.a) > 0);
    assert_eq!(telemetry_bytes(&w, &w.b), 0);
}

#[tokio::test]
async fn grpc_a_forged_identity_is_stored_only_as_the_sender() {
    let w = world(Arc::new(MemoryTelemetryStore::new()), Opts::default()).await;
    let r = grpc_logs(&w, A_IP, logs_req(&w.b, "from a", 2), None)
        .await
        .unwrap();
    assert!(r.partial_success.is_none());
    grpc_traces(&w, A_IP, traces_req(&w.b)).await.unwrap();
    let mine = read_all(&w, w.a.tenant).await;
    assert_eq!(
        (mine.logs.len(), mine.spans.len()),
        (2, 1),
        "positive control"
    );
    assert_own(&mine.logs[0].attributes, &w.a);
    assert_own(&mine.spans[0].attributes, &w.a);
    assert!(read_all(&w, w.b.tenant).await.is_empty());
}

#[tokio::test]
async fn no_identity_no_write_over_http() {
    let w = world(Arc::new(MemoryTelemetryStore::new()), Opts::default()).await;
    let body = logs_req(&w.a, "x", 1).encode_to_vec();
    // Positive control: the same request from A is stored.
    assert_eq!(post(&w, A_IP, "/v1/logs", body.clone()).await, 200);
    let before = (
        read_all(&w, w.a.tenant).await.records(),
        telemetry_bytes(&w, &w.a),
    );
    assert_eq!(before.0, 1);
    // An unknown peer.
    assert_eq!(post(&w, STRANGER_IP, "/v1/logs", body.clone()).await, 401);
    // A bearer token only (until card R-3), from an unknown and from a known address.
    for ip in [STRANGER_IP, A_IP] {
        let code = client(ip)
            .post(format!("http://{}/v1/logs", w.http))
            .header("Content-Type", "application/x-protobuf")
            .header("Authorization", "Bearer some-workload-token")
            .body(body.clone())
            .send()
            .await
            .unwrap()
            .status()
            .as_u16();
        assert_eq!(code, 401);
    }
    // A malformed body from A.
    assert_eq!(post(&w, A_IP, "/v1/logs", vec![0xff; 64]).await, 400);
    // JSON OTLP is not accepted.
    let code = client(A_IP)
        .post(format!("http://{}/v1/logs", w.http))
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(code, 415);
    let after = (
        read_all(&w, w.a.tenant).await.records(),
        telemetry_bytes(&w, &w.a),
    );
    assert_eq!(after, before, "nothing stored and nothing counted");
}

#[tokio::test]
async fn no_identity_no_write_over_grpc() {
    let w = world(Arc::new(MemoryTelemetryStore::new()), Opts::default()).await;
    grpc_logs(&w, A_IP, logs_req(&w.a, "x", 1), None)
        .await
        .unwrap();
    let before = (
        read_all(&w, w.a.tenant).await.records(),
        telemetry_bytes(&w, &w.a),
    );
    assert_eq!(before.0, 1, "positive control");
    let e = grpc_logs(&w, STRANGER_IP, logs_req(&w.a, "x", 1), None)
        .await
        .unwrap_err();
    assert_eq!(e.code(), tonic::Code::Unauthenticated);
    assert_eq!(e.message(), "unknown_source");
    let e = grpc_logs(&w, STRANGER_IP, logs_req(&w.a, "x", 1), Some("tok"))
        .await
        .unwrap_err();
    assert_eq!(e.code(), tonic::Code::Unauthenticated);
    let e = grpc_logs(&w, A_IP, logs_req(&w.a, "x", 1), Some("tok"))
        .await
        .unwrap_err();
    assert_eq!(e.code(), tonic::Code::Unauthenticated);
    let after = (
        read_all(&w, w.a.tenant).await.records(),
        telemetry_bytes(&w, &w.a),
    );
    assert_eq!(after, before);
}

// ---------------------------------------------------------------------------- bounds

#[tokio::test]
async fn bodies_past_the_caps_are_refused() {
    let limits = Limits {
        max_batch_bytes: 8192,
        max_records: 5,
        ..Limits::default()
    };
    let w = world(
        Arc::new(MemoryTelemetryStore::new()),
        Opts {
            limits,
            ..Opts::default()
        },
    )
    .await;
    // Positive control.
    assert_eq!(
        post(
            &w,
            A_IP,
            "/v1/logs",
            logs_req(&w.a, "ok", 5).encode_to_vec()
        )
        .await,
        200
    );
    // Too many records; a body past the cap; the same over gRPC.
    assert_eq!(
        post(&w, A_IP, "/v1/logs", logs_req(&w.a, "n", 6).encode_to_vec()).await,
        413
    );
    let big = logs_req(&w.a, &"x".repeat(9000), 1).encode_to_vec();
    assert_eq!(post(&w, A_IP, "/v1/logs", big).await, 413);
    let e = grpc_logs(&w, A_IP, logs_req(&w.a, "n", 6), None)
        .await
        .unwrap_err();
    assert_eq!(e.code(), tonic::Code::InvalidArgument);
    let e = grpc_logs(&w, A_IP, logs_req(&w.a, &"x".repeat(9000), 1), None)
        .await
        .unwrap_err();
    assert!(
        matches!(
            e.code(),
            tonic::Code::InvalidArgument | tonic::Code::OutOfRange | tonic::Code::ResourceExhausted
        ),
        "{e:?}"
    );
    assert_eq!(read_all(&w, w.a.tenant).await.logs.len(), 5);
}

#[tokio::test]
async fn a_slow_client_is_cut_off_and_others_are_served() {
    let w = world(
        Arc::new(MemoryTelemetryStore::new()),
        Opts {
            body_timeout: Duration::from_millis(500),
            ..Opts::default()
        },
    )
    .await;
    // A request that trickles its body from A.
    let sock = tokio::net::TcpSocket::new_v4().unwrap();
    sock.bind(SocketAddr::new(IpAddr::V4(A_IP), 0)).unwrap();
    let mut s = sock.connect(w.http).await.unwrap();
    s.write_all(
        b"POST /v1/logs HTTP/1.1\r\nHost: x\r\nContent-Type: application/x-protobuf\r\nContent-Length: 1000\r\n\r\n",
    )
    .await
    .unwrap();
    let started = std::time::Instant::now();
    let trickle = tokio::spawn(async move {
        for _ in 0..20 {
            if s.write_all(b"\x0a").await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let mut buf = vec![0u8; 512];
        let n = s.read(&mut buf).await.unwrap_or(0);
        String::from_utf8_lossy(&buf[..n]).to_string()
    });
    // Meanwhile B's batch is taken.
    assert_eq!(
        post(&w, B_IP, "/v1/logs", logs_req(&w.b, "b", 1).encode_to_vec()).await,
        200
    );
    let answer = trickle.await.unwrap();
    assert!(answer.starts_with("HTTP/1.1 408"), "{answer}");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(read_all(&w, w.a.tenant).await.is_empty());
    assert_eq!(read_all(&w, w.b.tenant).await.logs.len(), 1);
}

#[tokio::test]
async fn a_flood_of_one_tenant_leaves_room_for_another() {
    let w = world(
        Arc::new(MemoryTelemetryStore::new()),
        Opts {
            budget: BudgetConfig {
                requests_per_second: 2,
                burst_seconds: 2,
                ..BudgetConfig::default()
            },
            ..Opts::default()
        },
    )
    .await;
    let body = logs_req(&w.a, "flood", 1).encode_to_vec();
    let mut codes = Vec::new();
    for _ in 0..8 {
        codes.push(post(&w, A_IP, "/v1/logs", body.clone()).await);
    }
    assert!(codes.contains(&200), "positive control: {codes:?}");
    assert!(codes.contains(&429), "the flood is refused: {codes:?}");
    // Over gRPC too.
    let e = grpc_logs(&w, A_IP, logs_req(&w.a, "flood", 1), None)
        .await
        .unwrap_err();
    assert_eq!(e.code(), tonic::Code::ResourceExhausted);
    // Another tenant in the same moment.
    assert_eq!(
        post(&w, B_IP, "/v1/logs", logs_req(&w.b, "b", 1).encode_to_vec()).await,
        200
    );
}

// ---------------------------------------------------------------------------- the store tenant

#[derive(Default)]
struct Captured {
    writes: Mutex<Vec<(String, Option<String>, bool)>>,
    status: Mutex<u16>,
}

/// A fake Collector that records the path, the tenant header and whether the write token was
/// sent, and answers with the set status.
async fn fake_collector(token: &'static str) -> (SocketAddr, Arc<Captured>) {
    let cap = Arc::new(Captured::default());
    *cap.status.lock().unwrap() = 200;
    let c = cap.clone();
    let app = Router::new().fallback(move |req: axum::extract::Request| {
        let c = c.clone();
        async move {
            let tenant = req
                .headers()
                .get(TENANT_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let authed = req
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                == Some(format!("Bearer {token}").as_str());
            c.writes
                .lock()
                .unwrap()
                .push((req.uri().path().to_owned(), tenant, authed));
            let status = *c.status.lock().unwrap();
            StatusCode::from_u16(status).unwrap()
        }
    });
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await });
    (addr, cap)
}

fn lgtm(collector: SocketAddr, token: &str) -> Arc<dyn TelemetryStore> {
    Arc::new(
        LgtmStore::new(LgtmConfig {
            collector_url: format!("http://{collector}"),
            loki_url: "http://127.0.0.1:9".into(),
            mimir_url: "http://127.0.0.1:9".into(),
            tempo_url: "http://127.0.0.1:9".into(),
            collector_token: SecretValue::new(token),
            timeout: Duration::from_secs(5),
        })
        .unwrap(),
    )
}

#[tokio::test]
async fn the_collector_gets_the_identity_tenant_only() {
    let (addr, cap) = fake_collector("proc-token-1").await;
    let w = world(lgtm(addr, "proc-token-1"), Opts::default()).await;
    // A sends B's claims, a forged X-Scope-OrgID header and query, over HTTP and gRPC.
    assert_eq!(
        post(&w, A_IP, "/v1/logs", logs_req(&w.b, "a", 1).encode_to_vec()).await,
        200
    );
    assert_eq!(
        post(&w, A_IP, "/v1/traces", traces_req(&w.b).encode_to_vec()).await,
        200
    );
    grpc_logs(&w, A_IP, logs_req(&w.b, "a", 1), None)
        .await
        .unwrap();
    let writes = cap.writes.lock().unwrap().clone();
    assert_eq!(writes.len(), 3, "positive control: {writes:?}");
    let want = tenant_header(w.a.tenant);
    for (path, tenant, authed) in &writes {
        assert_eq!(tenant.as_deref(), Some(want.as_str()), "{path}");
        assert!(authed, "{path}: the processor's Collector token");
    }
    assert_eq!(writes[0].0, "/v1/logs");
    assert_eq!(writes[1].0, "/v1/traces");
    // Any non-2xx of the Collector is a refusal (S-62): the sender gets 503.
    for status in [400, 401, 500, 422] {
        *cap.status.lock().unwrap() = status;
        assert_eq!(
            post(&w, A_IP, "/v1/logs", logs_req(&w.a, "a", 1).encode_to_vec()).await,
            503,
            "Collector {status}"
        );
    }
}

// ---------------------------------------------------------------------------- the switch

#[test]
fn the_serving_process_starts_nothing_without_the_switch() {
    let d = TelemetrySettings::default();
    assert!(!starts_with_server(&d));
    assert!(!parse_switch(None).unwrap());
    assert!(!parse_switch(Some("off")).unwrap());
    assert!(parse_switch(Some("on")).unwrap());
    assert!(parse_switch(Some("yes please")).is_err());
    // The receiver never takes the Collector's ports.
    assert!(!d.grpc_listen.ends_with(":4317") && !d.http_listen.ends_with(":4318"));
    assert!(d.grpc_listen.starts_with("127.0.0.1:") && d.http_listen.starts_with("127.0.0.1:"));
}

// ---------------------------------------------------------------------------- the Collector config

/// The lines of the block that starts at the line `head` (by its indentation).
fn block<'a>(lines: &[&'a str], head: &str) -> Vec<&'a str> {
    let Some(start) = lines.iter().position(|l| l.trim_end() == head) else {
        return Vec::new();
    };
    let indent = head.len() - head.trim_start().len();
    lines[start + 1..]
        .iter()
        .take_while(|l| {
            l.trim().is_empty() || l.starts_with('#') || l.len() - l.trim_start().len() > indent
        })
        .copied()
        .collect()
}

#[test]
fn the_collector_takes_writes_only_with_the_processor_token() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../deploy/dev/otel/collector.yaml"
    );
    let text = std::fs::read_to_string(path).unwrap();
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    let ext = block(&lines, "  bearertokenauth/processor:");
    assert!(
        ext.iter()
            .any(|l| l.trim() == "filename: /etc/otelcol/collector-token"),
        "the token comes from the mounted file: {ext:?}"
    );
    for proto in ["      grpc:", "      http:"] {
        let b = block(&lines, proto);
        assert!(
            b.iter()
                .any(|l| l.trim() == "authenticator: bearertokenauth/processor"),
            "{proto} must require the processor's token: {b:?}"
        );
    }
    let service = block(&lines, "service:");
    assert!(
        service
            .iter()
            .any(|l| l.trim().starts_with("extensions:") && l.contains("bearertokenauth/processor")),
        "the extension is started"
    );
    // The tenant header copy of I-5 stays.
    assert!(text.contains("from_context: X-Scope-OrgID"));
    let compose = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../deploy/dev/otel/compose.yaml"
    ))
    .unwrap();
    assert!(compose.contains("/etc/otelcol/collector-token:ro"));
}
