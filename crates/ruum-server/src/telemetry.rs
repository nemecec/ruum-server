//! The telemetry role of `ruum-server` (card O-1b; decision 0006 answer 4): the OTLP receiver
//! over gRPC (tonic) and HTTP/protobuf (axum), the driver log shipper, and the real store over
//! the Collector and Loki, Mimir and Tempo (`ruum_core::telemetry::lgtm`).
//!
//! `ruum-server telemetry` runs only this role. The serving process starts it too, but only
//! with `RUUM_TELEMETRY=on` (default `off`), so a deploy that does not set it does not change.
//!
//! # A request
//!
//! 1. **Authenticate** the source from facts the receiver saw itself: the peer address, the
//!    listener, an optional bearer token ([`SourceFacts`]). Nothing of the request's content,
//!    headers (other than the bearer token, which is refused until card R-3) or query decides
//!    the identity. An unknown source gets `401` / `UNAUTHENTICATED`; the body is not read,
//!    over HTTP and over gRPC (the gRPC services are written here on tonic's `Grpc` with a
//!    codec that hands over the raw message, so no message is decoded before this step).
//! 2. **Budget**: one request of the identity's rate ([`FairBudget`]); `429` /
//!    `RESOURCE_EXHAUSTED` when over. Then the in-flight bytes are held for the identity's
//!    tenant from the first byte: the declared `Content-Length` before reading (HTTP), and
//!    every frame as it arrives (HTTP and gRPC). A tenant over its share gets `429` /
//!    `RESOURCE_EXHAUSTED` while the body is still arriving.
//! 3. **Read the body**: capped at `Limits::max_batch_bytes` (`413`; gRPC `OUT_OF_RANGE`) and
//!    bounded in time (HTTP `408` for a client that trickles bytes; gRPC the request timeout).
//! 4. **Check before decoding** ([`otlp::check`]): the wire walk counts the records and
//!    estimates the decoded size, `413` / `INVALID_ARGUMENT` over a cap. Then the byte rate of
//!    the identity, and the decoded estimate is held in flight too (`429` /
//!    `RESOURCE_EXHAUSTED` over the tenant's share).
//! 5. **Decode and map** ([`otlp`]): malformed `400` / `INVALID_ARGUMENT`. The bytes of the
//!    mapped batch are held before the mapping allocates them (`429` / `RESOURCE_EXHAUSTED`
//!    over the tenant's share); then the decoded estimate is given back, and the body and the
//!    batch stay held.
//! 6. **Ingest** with the authenticated identity ([`TelemetryIngest`]). The held bytes are
//!    given back when the request ends.
//!
//! The answers carry counts and reason names only (OTLP partial success), never an attribute
//! name or value. The `tracing` events carry the identity, the listener, the peer address,
//! the reason and the counts.
//!
//! # Wiring that waits for other cards
//!
//! - The source lookup on the Docker driver (container address on its app network) and the
//!   app-network listeners: cards I-4 and I-6. Until then the role uses
//!   [`NetworkSourceAuthenticator::refuse_all`]: every request is refused.
//! - The secret lookup (`WorkloadSpec::redacted_values` of the running instances) and the
//!   instance list of the shipper: the Docker driver of I-4/I-6. Until then: none.
//! - The field classes: the in-memory class source with no rows, so every field is
//!   unclassified and so `personal` (the safe default), until card O-2 adds the Postgres one.

use anyhow::Context as _;
use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{self, HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use bytes::{Buf as _, BufMut as _, Bytes, BytesMut};
use futures::StreamExt as _;
use opentelemetry_proto::tonic::collector::logs::v1::{
    ExportLogsPartialSuccess, ExportLogsServiceResponse,
};
use opentelemetry_proto::tonic::collector::metrics::v1::{
    ExportMetricsPartialSuccess, ExportMetricsServiceResponse,
};
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTracePartialSuccess, ExportTraceServiceResponse,
};
use prost::Message as _;
use ruum_core::telemetry::otlp::{self, Refusal};
use ruum_core::telemetry::processor::{BudgetConfig, BudgetPermit, BudgetRefusal, FairBudget};
use ruum_core::telemetry::source::{
    BearerToken, ListenerId, SourceAuthenticator, SourceError, SourceFacts,
};
use ruum_core::telemetry::{IngestReport, Limits, Signal, SourceIdentity, TelemetryIngest};
use std::convert::Infallible;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ---------------------------------------------------------------------------- settings

/// The default gRPC listener of the receiver: not the Collector's 4317.
pub const DEFAULT_GRPC_LISTEN: &str = "127.0.0.1:4417";
/// The default HTTP listener of the receiver: not the Collector's 4318.
pub const DEFAULT_HTTP_LISTEN: &str = "127.0.0.1:4418";
/// How long a client may take to send a request body.
pub const DEFAULT_BODY_TIMEOUT: Duration = Duration::from_secs(10);

/// The settings of the telemetry role (`crates/ruum-server/src/config.rs` reads them).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TelemetrySettings {
    /// `RUUM_TELEMETRY` (`on` or `off`, default `off`): the serving process starts the role.
    pub enabled: bool,
    /// `RUUM_TELEMETRY_GRPC_LISTEN` (default [`DEFAULT_GRPC_LISTEN`]).
    pub grpc_listen: String,
    /// `RUUM_TELEMETRY_HTTP_LISTEN` (default [`DEFAULT_HTTP_LISTEN`]).
    pub http_listen: String,
    /// `RUUM_TELEMETRY_COLLECTOR_URL`: the Collector's OTLP/HTTP URL (default
    /// `http://127.0.0.1:4318`, the laptop stack).
    pub collector_url: String,
    /// `RUUM_TELEMETRY_COLLECTOR_TOKEN_FILE`: the file with the processor's Collector token
    /// (`scripts/dev.sh` writes `deploy/dev/.state/otel/collector-token`). Without it every
    /// write is refused by the Collector.
    pub collector_token_file: Option<PathBuf>,
    /// `RUUM_LOKI_URL`, `RUUM_MIMIR_URL`, `RUUM_TEMPO_URL` (the laptop stack's defaults).
    pub loki_url: String,
    pub mimir_url: String,
    pub tempo_url: String,
    /// `RUUM_TELEMETRY_SHIPPER_STATE`: the shipper's position file. Without it the positions
    /// live in memory (a restart reads the driver's buffer again).
    pub shipper_state: Option<PathBuf>,
}

impl Default for TelemetrySettings {
    fn default() -> Self {
        TelemetrySettings {
            enabled: false,
            grpc_listen: DEFAULT_GRPC_LISTEN.into(),
            http_listen: DEFAULT_HTTP_LISTEN.into(),
            collector_url: "http://127.0.0.1:4318".into(),
            collector_token_file: None,
            loki_url: "http://127.0.0.1:3100".into(),
            mimir_url: "http://127.0.0.1:9009".into(),
            tempo_url: "http://127.0.0.1:3200".into(),
            shipper_state: None,
        }
    }
}

/// Whether the serving process starts the role next to the API: only with `RUUM_TELEMETRY=on`.
pub fn starts_with_server(settings: &TelemetrySettings) -> bool {
    settings.enabled
}

/// Reads `RUUM_TELEMETRY` (`on`/`off`, also `true`/`false`).
pub fn parse_switch(value: Option<&str>) -> anyhow::Result<bool> {
    match value {
        None => Ok(false),
        Some(v) => match v.to_ascii_lowercase().as_str() {
            "on" | "true" => Ok(true),
            "off" | "false" => Ok(false),
            _ => anyhow::bail!("RUUM_TELEMETRY must be on or off"),
        },
    }
}

// ---------------------------------------------------------------------------- receiver

/// Why the receiver refused a request. Reasons only, never content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    Unauthenticated(SourceError),
    Budget(BudgetRefusal),
    Request(Refusal),
    /// The body did not arrive within the body timeout.
    SlowBody,
    /// The body passed the size cap while it arrived (before any decoding).
    BodyTooLarge,
    /// The content type or encoding is not OTLP protobuf without compression.
    Unsupported,
    /// The pipeline or the store failed.
    Unavailable,
}

impl Refused {
    pub fn reason(self) -> &'static str {
        match self {
            Refused::Unauthenticated(e) => e.as_str(),
            Refused::Budget(b) => b.as_str(),
            Refused::Request(r) => r.as_str(),
            Refused::SlowBody => "slow_body",
            Refused::BodyTooLarge => Refusal::BodyTooLarge.as_str(),
            Refused::Unsupported => "unsupported_media_type",
            Refused::Unavailable => "unavailable",
        }
    }

    fn http_status(self) -> StatusCode {
        match self {
            Refused::Unauthenticated(_) => StatusCode::UNAUTHORIZED,
            Refused::Budget(_) => StatusCode::TOO_MANY_REQUESTS,
            Refused::Request(Refusal::Malformed) => StatusCode::BAD_REQUEST,
            Refused::Request(_) | Refused::BodyTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Refused::SlowBody => StatusCode::REQUEST_TIMEOUT,
            Refused::Unsupported => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Refused::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    fn grpc_status(self) -> tonic::Status {
        let msg = self.reason();
        match self {
            Refused::Unauthenticated(_) => tonic::Status::unauthenticated(msg),
            Refused::Budget(_) => tonic::Status::resource_exhausted(msg),
            Refused::Request(_) | Refused::Unsupported | Refused::SlowBody => {
                tonic::Status::invalid_argument(msg)
            }
            Refused::BodyTooLarge => tonic::Status::out_of_range(msg),
            Refused::Unavailable => tonic::Status::unavailable(msg),
        }
    }
}

/// What an accepted request did: counts for the OTLP partial success.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Accepted {
    pub records: u64,
    pub stored: u64,
    pub report: IngestReport,
}

impl Accepted {
    pub fn rejected(&self) -> i64 {
        i64::try_from(self.records.saturating_sub(self.stored)).unwrap_or(i64::MAX)
    }

    /// The partial-success message: drop reasons and counts, never a value or a name.
    pub fn message(&self) -> String {
        if self.rejected() == 0 {
            return String::new();
        }
        let parts: Vec<String> = self
            .report
            .dropped
            .iter()
            .map(|(r, n)| {
                let name = serde_json::to_value(r)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default();
                format!("{name}={n}")
            })
            .collect();
        format!("dropped records: {}", parts.join(", "))
    }
}

/// The receiver: shared by the gRPC and the HTTP listeners.
pub struct Receiver {
    auth: Arc<dyn SourceAuthenticator>,
    ingest: Arc<dyn TelemetryIngest>,
    budget: Arc<FairBudget>,
    limits: Limits,
    body_timeout: Duration,
}

impl Receiver {
    pub fn new(
        auth: Arc<dyn SourceAuthenticator>,
        ingest: Arc<dyn TelemetryIngest>,
        budget: Arc<FairBudget>,
        limits: Limits,
    ) -> Arc<Self> {
        Arc::new(Receiver {
            auth,
            ingest,
            budget,
            limits,
            body_timeout: DEFAULT_BODY_TIMEOUT,
        })
    }

    /// The same receiver with another body timeout (tests use a short one).
    #[cfg(test)]
    pub fn with_body_timeout(self: Arc<Self>, timeout: Duration) -> Arc<Self> {
        Arc::new(Receiver {
            auth: self.auth.clone(),
            ingest: self.ingest.clone(),
            budget: self.budget.clone(),
            limits: self.limits,
            body_timeout: timeout,
        })
    }

    /// Steps 1 and 2: the identity and one request of its rate.
    async fn admit(&self, facts: &SourceFacts) -> Result<SourceIdentity, Refused> {
        let identity = match self.auth.authenticate(facts).await {
            Ok(id) => id,
            Err(e) => {
                tracing::info!(
                    listener = %facts.listener,
                    peer = %facts.peer.ip(),
                    reason = e.as_str(),
                    "telemetry: request refused (no identity)"
                );
                return Err(Refused::Unauthenticated(e));
            }
        };
        if let Err(b) = self.budget.admit_request(&identity) {
            log_budget(&identity, b, 0, 0);
            return Err(Refused::Budget(b));
        }
        Ok(identity)
    }

    /// Holds `bytes` for the identity's tenant before the body is read (step 2).
    fn hold(&self, identity: &SourceIdentity, bytes: u64) -> Result<SharedPermit, Refused> {
        match self.budget.hold(identity, bytes) {
            Ok(p) => Ok(Arc::new(Mutex::new(p))),
            Err(b) => {
                log_budget(identity, b, bytes, 0);
                Err(Refused::Budget(b))
            }
        }
    }

    /// Steps 4 to 6 on a body that was read whole while `permit` held its bytes: the check
    /// before decoding, the byte rate, the decoded estimate in flight, the mapping and the
    /// pipeline.
    async fn process(
        &self,
        facts: &SourceFacts,
        identity: &SourceIdentity,
        permit: &SharedPermit,
        signal: Signal,
        body: &[u8],
    ) -> Result<Accepted, Refused> {
        let bytes = body.len() as u64;
        lock(permit).shrink_to(bytes);
        let checked = otlp::check(signal, body, &self.limits)
            .map_err(|e| Self::refuse_request(facts, identity, e))?;
        let shape = checked.shape();
        if let Err(b) = self.budget.take_bytes(identity, bytes) {
            log_budget(identity, b, bytes, shape.records);
            return Err(Refused::Budget(b));
        }
        if let Err(b) = lock(permit).grow(shape.decoded_bytes) {
            log_budget(identity, b, bytes + shape.decoded_bytes, shape.records);
            return Err(Refused::Budget(b));
        }
        // The mapped batch is held too, before the mapping allocates it (Kimi run 13 finding
        // 1). Once mapped, the decoded message is gone: the permit keeps the body and the batch.
        let mut not_held = None;
        let decoded =
            checked.decode_held(&self.limits, &mut |step| match lock(permit).grow(step) {
                Ok(()) => true,
                Err(b) => {
                    not_held = Some(b);
                    false
                }
            });
        let mapped = match (decoded, not_held) {
            (Err(Refusal::NotHeld), Some(b)) => {
                let held = lock(permit).bytes();
                log_budget(identity, b, held, shape.records);
                return Err(Refused::Budget(b));
            }
            (r, _) => r.map_err(|e| Self::refuse_request(facts, identity, e))?,
        };
        lock(permit).shrink_to(bytes + mapped.bytes);
        let records = mapped.records;
        let otlp::Mapped { batch, dropped, .. } = mapped;
        let mut report = match self.ingest.ingest(identity, batch).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    tenant = %identity.tenant,
                    app = %identity.app,
                    deployment = %identity.deployment,
                    environment = identity.environment.as_str(),
                    records,
                    error = %e,
                    "telemetry: the pipeline or the store failed"
                );
                return Err(Refused::Unavailable);
            }
        };
        for (reason, n) in dropped {
            *report.dropped.entry(reason).or_default() += n;
        }
        let stored = report.stored_logs + report.stored_metrics + report.stored_spans;
        Ok(Accepted {
            records,
            stored,
            report,
        })
    }

    fn refuse_request(facts: &SourceFacts, identity: &SourceIdentity, r: Refusal) -> Refused {
        tracing::info!(
            listener = %facts.listener,
            tenant = %identity.tenant,
            app = %identity.app,
            deployment = %identity.deployment,
            reason = r.as_str(),
            "telemetry: request refused"
        );
        Refused::Request(r)
    }
}

/// The in-flight bytes of one request, shared by its body reader and its handler.
type SharedPermit = Arc<Mutex<BudgetPermit>>;

fn lock(p: &SharedPermit) -> std::sync::MutexGuard<'_, BudgetPermit> {
    p.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn log_budget(identity: &SourceIdentity, b: BudgetRefusal, bytes: u64, records: u64) {
    tracing::warn!(
        tenant = %identity.tenant,
        app = %identity.app,
        deployment = %identity.deployment,
        environment = identity.environment.as_str(),
        reason = b.as_str(),
        bytes,
        records,
        "telemetry: request refused by the budget"
    );
}

// ---------------------------------------------------------------------------- HTTP

/// The HTTP/protobuf receiver: `POST /v1/logs`, `/v1/metrics`, `/v1/traces`.
#[derive(Clone)]
struct HttpState {
    receiver: Arc<Receiver>,
    listener: ListenerId,
}

/// The HTTP router of the receiver on one listener.
pub fn http_router(receiver: Arc<Receiver>, listener: ListenerId) -> Router {
    Router::new()
        .route("/v1/logs", post(http_logs))
        .route("/v1/metrics", post(http_metrics))
        .route("/v1/traces", post(http_traces))
        .with_state(HttpState { receiver, listener })
}

async fn http_logs(
    State(s): State<HttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    http_export(s, peer, headers, body, Signal::Logs).await
}

async fn http_metrics(
    State(s): State<HttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    http_export(s, peer, headers, body, Signal::Metrics).await
}

async fn http_traces(
    State(s): State<HttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    http_export(s, peer, headers, body, Signal::Traces).await
}

/// The bearer token of an `Authorization` header, if any.
fn bearer_of(headers: &HeaderMap) -> Option<BearerToken> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = v.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| BearerToken::new(token.trim()))
}

fn refused_response(r: Refused) -> Response {
    let mut res = (r.http_status(), r.reason()).into_response();
    if matches!(r, Refused::Budget(_)) {
        res.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    }
    res
}

async fn http_export(
    s: HttpState,
    peer: SocketAddr,
    headers: HeaderMap,
    body: Body,
    signal: Signal,
) -> Response {
    let facts = SourceFacts {
        peer,
        listener: s.listener.clone(),
        bearer: bearer_of(&headers),
    };
    let r = &s.receiver;
    let identity = match r.admit(&facts).await {
        Ok(id) => id,
        Err(e) => return refused_response(e),
    };
    // OTLP protobuf without compression only (JSON OTLP is optional and not built).
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let encoded = headers
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| !v.eq_ignore_ascii_case("identity"));
    if !content_type.starts_with("application/x-protobuf") || encoded {
        return refused_response(Refused::Unsupported);
    }
    let cap = r.limits.max_batch_bytes;
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|n| n > cap) {
        return refused_response(Receiver::refuse_request(
            &facts,
            &identity,
            Refusal::BodyTooLarge,
        ));
    }
    // The tenant holds the declared length before a byte is read, and every byte past it as
    // it arrives.
    let permit = match r.hold(&identity, declared.unwrap_or(0) as u64) {
        Ok(p) => p,
        Err(e) => return refused_response(e),
    };
    let read = read_body(body, cap, &permit, &identity);
    let bytes = match tokio::time::timeout(r.body_timeout, read).await {
        Err(_) => {
            tracing::info!(listener = %facts.listener, tenant = %identity.tenant, app = %identity.app, reason = "slow_body", "telemetry: request refused");
            return refused_response(Refused::SlowBody);
        }
        Ok(Err(e)) => {
            tracing::info!(listener = %facts.listener, tenant = %identity.tenant, app = %identity.app, reason = e.reason(), "telemetry: request refused");
            return refused_response(e);
        }
        Ok(Ok(b)) => b,
    };
    match r.process(&facts, &identity, &permit, signal, &bytes).await {
        Ok(a) => http_success(signal, &a),
        Err(e) => refused_response(e),
    }
}

/// Reads an HTTP body up to `cap` bytes, growing `permit` with every chunk.
async fn read_body(
    body: Body,
    cap: usize,
    permit: &SharedPermit,
    identity: &SourceIdentity,
) -> Result<Bytes, Refused> {
    let mut stream = body.into_data_stream();
    let mut buf = BytesMut::new();
    while let Some(chunk) = stream.next().await {
        // The client went away or sent a broken body: nothing to decode.
        let chunk = chunk.map_err(|_| Refused::Request(Refusal::Malformed))?;
        let total = buf.len() + chunk.len();
        if total > cap {
            return Err(Refused::BodyTooLarge);
        }
        if let Err(b) = lock(permit).grow_to(total as u64) {
            log_budget(identity, b, total as u64, 0);
            return Err(Refused::Budget(b));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf.freeze())
}

fn http_success(signal: Signal, a: &Accepted) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/x-protobuf")],
        encode_success(signal, a),
    )
        .into_response()
}

/// The OTLP export response of a signal, encoded: the partial success with counts and reason
/// names only.
fn encode_success(signal: Signal, a: &Accepted) -> Vec<u8> {
    let (rejected, message) = (a.rejected(), a.message());
    match signal {
        Signal::Logs => ExportLogsServiceResponse {
            partial_success: (rejected > 0).then_some(ExportLogsPartialSuccess {
                rejected_log_records: rejected,
                error_message: message,
            }),
        }
        .encode_to_vec(),
        Signal::Metrics => ExportMetricsServiceResponse {
            partial_success: (rejected > 0).then_some(ExportMetricsPartialSuccess {
                rejected_data_points: rejected,
                error_message: message,
            }),
        }
        .encode_to_vec(),
        Signal::Traces => ExportTraceServiceResponse {
            partial_success: (rejected > 0).then_some(ExportTracePartialSuccess {
                rejected_spans: rejected,
                error_message: message,
            }),
        }
        .encode_to_vec(),
    }
}

// ---------------------------------------------------------------------------- gRPC

/// One OTLP gRPC service: its name and its signal.
pub trait OtlpService: Send + Sync + 'static {
    const NAME: &'static str;
    const SIGNAL: Signal;
}

/// `opentelemetry.proto.collector.logs.v1.LogsService`.
pub enum LogsService {}
/// `opentelemetry.proto.collector.metrics.v1.MetricsService`.
pub enum MetricsService {}
/// `opentelemetry.proto.collector.trace.v1.TraceService`.
pub enum TraceService {}

impl OtlpService for LogsService {
    const NAME: &'static str = "opentelemetry.proto.collector.logs.v1.LogsService";
    const SIGNAL: Signal = Signal::Logs;
}
impl OtlpService for MetricsService {
    const NAME: &'static str = "opentelemetry.proto.collector.metrics.v1.MetricsService";
    const SIGNAL: Signal = Signal::Metrics;
}
impl OtlpService for TraceService {
    const NAME: &'static str = "opentelemetry.proto.collector.trace.v1.TraceService";
    const SIGNAL: Signal = Signal::Traces;
}

/// The gRPC receiver of one OTLP service on one listener. It is not the generated tonic
/// server: that one decodes the message before the handler runs. This one authenticates and
/// admits the request from its headers and its peer address first, then lets tonic read the
/// message (as raw bytes, holding them in the budget frame by frame), then checks and decodes
/// it like the HTTP receiver.
pub struct GrpcReceiver<S> {
    receiver: Arc<Receiver>,
    listener: ListenerId,
    _service: PhantomData<fn() -> S>,
}

impl<S> Clone for GrpcReceiver<S> {
    fn clone(&self) -> Self {
        GrpcReceiver {
            receiver: self.receiver.clone(),
            listener: self.listener.clone(),
            _service: PhantomData,
        }
    }
}

impl<S: OtlpService> tonic::server::NamedService for GrpcReceiver<S> {
    const NAME: &'static str = S::NAME;
}

type GrpcResponse = http::Response<tonic::body::Body>;

impl<S: OtlpService> GrpcReceiver<S> {
    pub fn new(receiver: Arc<Receiver>, listener: ListenerId) -> Self {
        GrpcReceiver {
            receiver,
            listener,
            _service: PhantomData,
        }
    }

    /// The facts of the source: the peer address that tonic's transport saw, the listener, and
    /// a bearer token from the headers.
    fn facts<B>(&self, req: &http::Request<B>) -> Option<SourceFacts> {
        let peer = req
            .extensions()
            .get::<tonic::transport::server::TcpConnectInfo>()?
            .remote_addr()?;
        Some(SourceFacts {
            peer,
            listener: self.listener.clone(),
            bearer: bearer_of(req.headers()),
        })
    }

    async fn handle(self, req: http::Request<tonic::body::Body>) -> GrpcResponse {
        if req
            .uri()
            .path()
            .strip_prefix('/')
            .and_then(|p| p.strip_suffix("/Export"))
            != Some(S::NAME)
        {
            return tonic::Status::unimplemented("").into_http();
        }
        // Steps 1 and 2, from the headers and the peer only: the body is not polled before.
        // Without a peer address there is no identity.
        let Some(facts) = self.facts(&req) else {
            return Refused::Unauthenticated(SourceError::UnknownSource)
                .grpc_status()
                .into_http();
        };
        let r = self.receiver.clone();
        let identity = match r.admit(&facts).await {
            Ok(id) => id,
            Err(e) => return e.grpc_status().into_http(),
        };
        let permit = match r.hold(&identity, 0) {
            Ok(p) => p,
            Err(e) => return e.grpc_status().into_http(),
        };
        let max = r.limits.max_batch_bytes;
        let (parts, body) = req.into_parts();
        // The message and its 5-byte gRPC frame header.
        let body = metered_body(body, permit.clone(), identity, max + 5);
        let req = http::Request::from_parts(parts, body);
        let method = GrpcExport {
            receiver: r,
            facts,
            identity,
            permit,
            signal: S::SIGNAL,
        };
        tonic::server::Grpc::new(RawCodec)
            .apply_max_message_size_config(Some(max), None)
            .unary(method, req)
            .await
    }
}

impl<S: OtlpService> tonic::codegen::Service<http::Request<tonic::body::Body>> for GrpcReceiver<S> {
    type Response = GrpcResponse;
    type Error = Infallible;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<GrpcResponse, Infallible>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Infallible>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<tonic::body::Body>) -> Self::Future {
        let this = self.clone();
        Box::pin(async move { Ok(this.handle(req).await) })
    }
}

/// A gRPC request body that holds its bytes in `permit` as they arrive and stops past `cap`.
fn metered_body(
    body: tonic::body::Body,
    permit: SharedPermit,
    identity: SourceIdentity,
    cap: usize,
) -> impl axum::body::HttpBody<Data = Bytes, Error = tonic::Status> + Send + 'static {
    let mut total = 0usize;
    let frames = http_body_util::BodyStream::new(body).map(move |frame| {
        let frame = frame?;
        if let Some(data) = frame.data_ref() {
            total += data.len();
            if total > cap {
                return Err(Refused::BodyTooLarge.grpc_status());
            }
            if let Err(b) = lock(&permit).grow_to(total as u64) {
                log_budget(&identity, b, total as u64, 0);
                return Err(Refused::Budget(b).grpc_status());
            }
        }
        Ok(frame)
    });
    http_body_util::StreamBody::new(frames)
}

/// The export call of one admitted gRPC request.
struct GrpcExport {
    receiver: Arc<Receiver>,
    facts: SourceFacts,
    identity: SourceIdentity,
    permit: SharedPermit,
    signal: Signal,
}

impl tonic::server::UnaryService<Bytes> for GrpcExport {
    type Response = Vec<u8>;
    type Future = std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<tonic::Response<Vec<u8>>, tonic::Status>>
                + Send,
        >,
    >;

    fn call(&mut self, request: tonic::Request<Bytes>) -> Self::Future {
        let (receiver, facts, permit) = (
            self.receiver.clone(),
            self.facts.clone(),
            self.permit.clone(),
        );
        let (identity, signal) = (self.identity, self.signal);
        Box::pin(async move {
            let body = request.into_inner();
            let a = receiver
                .process(&facts, &identity, &permit, signal, &body)
                .await
                .map_err(Refused::grpc_status)?;
            Ok(tonic::Response::new(encode_success(signal, &a)))
        })
    }
}

/// A gRPC codec that hands over the raw request message (decoded by [`otlp`] after the
/// checks) and sends an already encoded response.
struct RawCodec;

impl tonic::codec::Codec for RawCodec {
    type Encode = Vec<u8>;
    type Decode = Bytes;
    type Encoder = RawCodec;
    type Decoder = RawCodec;

    fn encoder(&mut self) -> RawCodec {
        RawCodec
    }

    fn decoder(&mut self) -> RawCodec {
        RawCodec
    }
}

impl tonic::codec::Encoder for RawCodec {
    type Item = Vec<u8>;
    type Error = tonic::Status;

    fn encode(
        &mut self,
        item: Vec<u8>,
        dst: &mut tonic::codec::EncodeBuf<'_>,
    ) -> Result<(), tonic::Status> {
        dst.put_slice(&item);
        Ok(())
    }
}

impl tonic::codec::Decoder for RawCodec {
    type Item = Bytes;
    type Error = tonic::Status;

    fn decode(
        &mut self,
        src: &mut tonic::codec::DecodeBuf<'_>,
    ) -> Result<Option<Bytes>, tonic::Status> {
        Ok(Some(src.copy_to_bytes(src.remaining())))
    }
}

/// The gRPC request timeout (a client that trickles a message is cut off).
pub const GRPC_TIMEOUT: Duration = Duration::from_secs(15);

/// Serves the gRPC receiver on `listener` until `stop` resolves.
pub async fn serve_grpc(
    receiver: Arc<Receiver>,
    name: ListenerId,
    listener: tokio::net::TcpListener,
    stop: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    tonic::transport::Server::builder()
        .timeout(GRPC_TIMEOUT)
        .concurrency_limit_per_connection(32)
        .add_service(GrpcReceiver::<LogsService>::new(
            receiver.clone(),
            name.clone(),
        ))
        .add_service(GrpcReceiver::<MetricsService>::new(
            receiver.clone(),
            name.clone(),
        ))
        .add_service(GrpcReceiver::<TraceService>::new(receiver, name))
        .serve_with_incoming_shutdown(
            tokio_stream::wrappers::TcpListenerStream::new(listener),
            stop,
        )
        .await
        .context("the telemetry gRPC receiver stopped")
}

/// Serves the HTTP receiver on `listener` until `stop` resolves.
pub async fn serve_http(
    receiver: Arc<Receiver>,
    name: ListenerId,
    listener: tokio::net::TcpListener,
    stop: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let router = http_router(receiver, name);
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(stop)
    .await
    .context("the telemetry HTTP receiver stopped")
}

// ---------------------------------------------------------------------------- the role

/// The local listener's name. App-network listeners (cards I-4, I-6) get their own names.
pub const LOCAL_LISTENER: &str = "local";

/// Builds and runs the role: the receiver on both listeners and the shipper, with the real
/// store, until `stop` resolves.
pub async fn run(
    settings: &TelemetrySettings,
    services: &ruum_core::Services,
    stop: crate::shutdown::Stop,
) -> anyhow::Result<()> {
    let grpc = tokio::net::TcpListener::bind(&settings.grpc_listen)
        .await
        .with_context(|| format!("bind {} (RUUM_TELEMETRY_GRPC_LISTEN)", settings.grpc_listen))?;
    let http = tokio::net::TcpListener::bind(&settings.http_listen)
        .await
        .with_context(|| format!("bind {} (RUUM_TELEMETRY_HTTP_LISTEN)", settings.http_listen))?;
    run_on(settings, services, stop, grpc, http).await
}

/// [`run`] on listeners that are already bound (the tests bind port 0).
pub async fn run_on(
    settings: &TelemetrySettings,
    services: &ruum_core::Services,
    stop: crate::shutdown::Stop,
    grpc: tokio::net::TcpListener,
    http: tokio::net::TcpListener,
) -> anyhow::Result<()> {
    use ruum_core::telemetry::lgtm::{LgtmConfig, LgtmStore};
    use ruum_core::telemetry::memory::MemoryFieldClasses;
    use ruum_core::telemetry::processor::{
        FilePositions, LogShipper, MemoryPositions, PositionStore, ShipperConfig,
        WorkloadSecretSource,
    };
    use ruum_core::telemetry::source::NetworkSourceAuthenticator;
    use ruum_core::telemetry::{PipelineIngest, TelemetryStore};

    let token = match &settings.collector_token_file {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| {
                format!(
                    "read RUUM_TELEMETRY_COLLECTOR_TOKEN_FILE ({})",
                    path.display()
                )
            })?
            .trim()
            .to_owned(),
        None => {
            tracing::warn!(
                "RUUM_TELEMETRY_COLLECTOR_TOKEN_FILE is not set: the Collector refuses every write"
            );
            String::new()
        }
    };
    let store: Arc<dyn TelemetryStore> = Arc::new(LgtmStore::new(LgtmConfig {
        collector_url: settings.collector_url.clone(),
        loki_url: settings.loki_url.clone(),
        mimir_url: settings.mimir_url.clone(),
        tempo_url: settings.tempo_url.clone(),
        collector_token: ruum_core::runtime::SecretValue::new(token),
        timeout: Duration::from_secs(30),
    })?);
    // No Postgres class source before card O-2: every field is unclassified, so `personal`.
    let classes = Arc::new(MemoryFieldClasses::new());
    // No driver lookup before cards I-4 and I-6: no secret values to match (the driver
    // redacts its own log lines; the deny-list, classes and scrubber still run).
    let secrets = Arc::new(WorkloadSecretSource::empty());
    let ingest: Arc<dyn TelemetryIngest> = Arc::new(PipelineIngest::new(
        store,
        classes,
        secrets,
        (*services.tenant_keys).clone(),
        services.apps.usage().clone(),
    ));
    // No source lookup before cards I-4 and I-6: every request is refused.
    let auth = Arc::new(NetworkSourceAuthenticator::refuse_all());
    let receiver = Receiver::new(
        auth,
        ingest.clone(),
        FairBudget::new(BudgetConfig::default()),
        Limits::default(),
    );
    let positions: Arc<dyn PositionStore> = match &settings.shipper_state {
        Some(path) => Arc::new(FilePositions::new(path.clone())),
        None => Arc::new(MemoryPositions::new()),
    };
    // The instance list of the Docker driver comes with cards I-4 and I-6.
    let shipper = Arc::new(
        LogShipper::new(
            services.apps.deps().runtime.clone(),
            Arc::new(Vec::new),
            ingest,
            positions,
            ShipperConfig::default(),
        )
        .await?,
    );

    tracing::info!(
        grpc = %grpc.local_addr().map_or_else(|_| settings.grpc_listen.clone(), |a| a.to_string()),
        http = %http.local_addr().map_or_else(|_| settings.http_listen.clone(), |a| a.to_string()),
        "telemetry receiver listening"
    );
    let name = ListenerId::new(LOCAL_LISTENER);
    let shipper_task = tokio::spawn(shipper.run(stop.clone().stopped()));
    let r = tokio::try_join!(
        serve_grpc(receiver.clone(), name.clone(), grpc, stop.clone().stopped()),
        serve_http(receiver, name, http, stop.stopped()),
    );
    shipper_task.abort();
    r.map(|_| ())
}

#[cfg(test)]
mod tests;
