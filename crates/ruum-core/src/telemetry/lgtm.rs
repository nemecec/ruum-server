//! The real [`TelemetryStore`] (card O-1b): writes go to the OpenTelemetry Collector of card I-5
//! (OTLP/HTTP, protobuf), reads go to Loki (logs), Mimir (metrics) and Tempo (traces).
//!
//! # Who builds the store queries
//!
//! The module doc of [`super`] says that card O-2 builds LogQL, PromQL and TraceQL. This card
//! builds the query builders of the store, because the privacy suite
//! ([`super::conformance::run`]) needs [`TelemetryStore::read`] on the real store. Card O-2
//! builds the read **operations** of the API over the trait. (A text fix of `docs/STEP3.md`
//! for the integrator; not a border change.)
//!
//! # Tenancy
//!
//! - The storage tenant is the header `X-Scope-OrgID`: on a write the identity's tenant, on a
//!   read the `tenant` argument, always in canonical UUID form ([`tenant_header`]). It is never
//!   a value of a record or of a filter.
//! - The selector (app, environment, deployment) is built by the server from typed values of the
//!   filter (UUIDs and the environment enum), and every value that goes into a query is escaped
//!   by [`quote`], which has its own unit tests. Field equalities are applied after the read,
//!   on the decoded records; the only part of them that goes into a query is a Loki line filter
//!   on the field's key, escaped, which only narrows the read.
//! - Every decoded record is checked again against the tenant and the selector; a record that
//!   does not match is dropped, so a store that mixed tenants would still not cross them here.
//!
//! # The record encoding
//!
//! The stores keep what the pipeline made, losslessly, so a read returns the same
//! [`ClassifiedBatch`]:
//!
//! - Logs: the log line is the JSON of the record (the private `WireLog`). Resource attributes
//!   `service.name` (the app), `ruum.environment` and `ruum.deployment` make the stream and its
//!   structured metadata.
//! - Metrics: one series name, [`METRIC_NAME`], with the labels `ruum_app`,
//!   `ruum_environment`, `ruum_deployment`, `ruum_kind`, `ruum_name` (the metric name) and
//!   `ruum_labels` (the JSON of the stored labels). Times have the store's millisecond
//!   resolution; two points of one series in the same millisecond are written one millisecond
//!   apart.
//! - Traces: native OTLP spans (IDs, times, name, kind, status, events, links); each attribute
//!   value is a string that holds the JSON of the stored value (`{"s": "text"}`,
//!   `{"p": {"$ruum": "pseudonym", ...}}`), so a pseudonym or a hidden-value marker reads back as
//!   itself and never as text that looks like one.
//!
//! # Limits (each has a backlog row in the impl notes)
//!
//! - A log record whose line is longer than [`MAX_LOG_LINE`] and a metric point whose labels
//!   are longer than [`MAX_LABEL_VALUE`] are not written (Loki's `max_line_size`, Mimir's
//!   `max_label_value_length`); the write logs the count (no content) and succeeds for the rest.
//! - Any non-2xx answer of the Collector is a refusal of that signal's write (S-62).
//! - A read returns at most [`MAX_READ`] records per signal; Tempo search is eventually
//!   consistent (a trace is searchable after the ingester cuts it).
//! - A read is bounded in memory (Kimi run 13 finding 2): a store answer is read as it arrives
//!   and refused over [`MAX_ANSWER_BYTES`] (a Tempo trace over [`MAX_TRACE_BYTES`], whose
//!   protobuf is walked before prost decodes it, as on the receiver: a trace over the bounds
//!   is skipped and counted); Loki paging stops once the read has its `limit`, and the read
//!   keeps at most `limit` records of a signal (the oldest) while it reads.

use super::*;
use crate::privacy::{HiddenValue, Pseudonym};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::logs::v1 as otlp_logs;
use opentelemetry_proto::tonic::metrics::v1 as otlp_metrics;
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1 as otlp_trace;
use prost::Message as _;
use serde::Deserialize;
use std::time::Duration;

/// The series name of every app metric in Mimir.
pub const METRIC_NAME: &str = "ruum_app_metric";
/// The longest log line written to Loki (its `max_line_size` is 256 KB).
pub const MAX_LOG_LINE: usize = 250 * 1024;
/// The longest label value written to Mimir (its default `max_label_value_length`).
pub const MAX_LABEL_VALUE: usize = 2048;
/// The most records of one signal that a read returns.
pub const MAX_READ: usize = 5000;
/// How far back a read without `from` looks (the stores keep 72 hours).
pub const DEFAULT_LOOKBACK: time::Duration = time::Duration::hours(71);
/// Loki's page size (`max_entries_limit_per_query`).
const LOKI_PAGE: usize = 5000;
/// The pages of one Loki read.
const LOKI_PAGES: usize = 20;
/// The traces one Tempo search returns.
const TEMPO_TRACES: usize = 1000;
/// The largest store answer read (Loki, Mimir and Tempo search JSON).
pub const MAX_ANSWER_BYTES: usize = 32 * 1024 * 1024;
/// The largest Tempo trace answer read (Tempo's own `max_bytes_per_trace` is 5 MB by default).
/// Its decoded size is bounded by [`otlp::check`] at [`otlp::DECODED_FACTOR`] times this.
pub const MAX_TRACE_BYTES: usize = 8 * 1024 * 1024;
/// The spans of one Tempo trace answer.
const MAX_TRACE_SPANS: usize = 100_000;
/// The spans per span set of a Tempo search (Tempo refuses more than 100).
const TEMPO_SPANS_PER_SET: usize = 100;
/// The resource attribute of the deployment and the environment (Loki: structured metadata
/// `ruum_deployment` and `ruum_environment`; Tempo: `resource.ruum.deployment`).
const RES_DEPLOYMENT: &str = "ruum.deployment";
const RES_ENVIRONMENT: &str = "ruum.environment";
const SERVICE_NAME: &str = "service.name";

/// The settings of [`LgtmStore`].
pub struct LgtmConfig {
    /// The Collector's OTLP/HTTP base URL (`http://127.0.0.1:4318`).
    pub collector_url: String,
    pub loki_url: String,
    pub mimir_url: String,
    pub tempo_url: String,
    /// The processor's Collector credential (the Collector's `bearertokenauth`). Never logged.
    pub collector_token: SecretValue,
    /// The timeout of one store request.
    pub timeout: Duration,
}

/// [`TelemetryStore`] over the Collector, Loki, Mimir and Tempo. See the module docs.
pub struct LgtmStore {
    cfg: LgtmConfig,
    http: reqwest::Client,
    /// [`MAX_ANSWER_BYTES`] and [`MAX_TRACE_BYTES`]; smaller in tests.
    answer_cap: usize,
    trace_cap: usize,
}

/// The storage tenant header value: the tenant's UUID in canonical (lowercase, hyphenated)
/// form. The only source of `X-Scope-OrgID`.
pub fn tenant_header(tenant: TenantId) -> String {
    tenant.as_uuid().hyphenated().to_string()
}

/// The header name of the storage tenant.
pub const TENANT_HEADER: &str = "X-Scope-OrgID";

/// A double-quoted string literal of LogQL, PromQL and TraceQL (all three read Go string
/// literals): `\` and `"` are escaped, newline, carriage return and tab get their short
/// escapes, every other control character gets `\xNN` (or `\u{...}` style `\uNNNN` for C1
/// controls), and every other character stays as it is. Inside the quotes `|`, `}`, `` ` ``,
/// `{` and regex metacharacters are plain characters; the builders only use `=` matchers and
/// `|=` line filters, never a regex matcher, so a value is never a pattern.
pub fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c if ('\u{80}'..='\u{9f}').contains(&c) => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------- wire

/// A stored value in the stores: externally tagged, so a pseudonym or a hidden marker is never
/// confused with text. Floats as text, so NaN and infinities survive JSON.
#[derive(Serialize, Deserialize)]
enum WireValue {
    #[serde(rename = "s")]
    Text(String),
    #[serde(rename = "i")]
    Int(i64),
    #[serde(rename = "f")]
    Float(String),
    #[serde(rename = "b")]
    Bool(bool),
    #[serde(rename = "h")]
    Hidden(HiddenValue),
    #[serde(rename = "p")]
    Pseudonym(Pseudonym),
}

impl WireValue {
    fn from_stored(v: &StoredValue) -> Self {
        match v {
            StoredValue::Text(s) => WireValue::Text(s.clone()),
            StoredValue::Int(i) => WireValue::Int(*i),
            StoredValue::Float(f) => WireValue::Float(f.to_string()),
            StoredValue::Bool(b) => WireValue::Bool(*b),
            StoredValue::Hidden(h) => WireValue::Hidden(*h),
            StoredValue::Pseudonym(p) => WireValue::Pseudonym(p.clone()),
        }
    }

    fn into_stored(self) -> Option<StoredValue> {
        Some(match self {
            WireValue::Text(s) => StoredValue::Text(s),
            WireValue::Int(i) => StoredValue::Int(i),
            WireValue::Float(f) => StoredValue::Float(f.parse().ok()?),
            WireValue::Bool(b) => StoredValue::Bool(b),
            WireValue::Hidden(h) => StoredValue::Hidden(h),
            WireValue::Pseudonym(p) => StoredValue::Pseudonym(p),
        })
    }
}

#[derive(Serialize, Deserialize)]
struct WireAttr {
    k: String,
    v: WireValue,
}

fn wire_attrs(attrs: &[StoredAttribute]) -> Vec<WireAttr> {
    attrs
        .iter()
        .map(|a| WireAttr {
            k: a.key.clone(),
            v: WireValue::from_stored(&a.value),
        })
        .collect()
}

fn stored_attrs(attrs: Vec<WireAttr>) -> Option<Vec<StoredAttribute>> {
    attrs
        .into_iter()
        .map(|a| {
            Some(StoredAttribute {
                key: a.k,
                value: a.v.into_stored()?,
            })
        })
        .collect()
}

#[derive(Serialize, Deserialize)]
struct WireSource {
    tenant: Uuid,
    app: Uuid,
    deployment: Uuid,
    environment: String,
}

impl WireSource {
    fn from_source(s: &SourceIdentity) -> Self {
        WireSource {
            tenant: s.tenant.as_uuid(),
            app: s.app.as_uuid(),
            deployment: s.deployment,
            environment: s.environment.as_str().to_owned(),
        }
    }

    fn into_source(self) -> Option<SourceIdentity> {
        Some(SourceIdentity {
            tenant: TenantId::from(self.tenant),
            app: NodeId::from(self.app),
            deployment: self.deployment,
            environment: environment_of(&self.environment)?,
        })
    }
}

fn environment_of(s: &str) -> Option<Environment> {
    match s {
        "live" => Some(Environment::Live),
        "preview" => Some(Environment::Preview),
        _ => None,
    }
}

fn severity_name(s: Severity) -> &'static str {
    match s {
        Severity::Trace => "trace",
        Severity::Debug => "debug",
        Severity::Info => "info",
        Severity::Warn => "warn",
        Severity::Error => "error",
        Severity::Fatal => "fatal",
    }
}

fn severity_from(s: &str) -> Option<Severity> {
    Some(match s {
        "trace" => Severity::Trace,
        "debug" => Severity::Debug,
        "info" => Severity::Info,
        "warn" => Severity::Warn,
        "error" => Severity::Error,
        "fatal" => Severity::Fatal,
        _ => return None,
    })
}

fn severity_number(s: Severity) -> i32 {
    match s {
        Severity::Trace => 1,
        Severity::Debug => 5,
        Severity::Info => 9,
        Severity::Warn => 13,
        Severity::Error => 17,
        Severity::Fatal => 21,
    }
}

fn metric_kind_name(k: MetricKind) -> &'static str {
    match k {
        MetricKind::Counter => "counter",
        MetricKind::Gauge => "gauge",
        MetricKind::Histogram => "histogram",
    }
}

fn metric_kind_from(s: &str) -> Option<MetricKind> {
    Some(match s {
        "counter" => MetricKind::Counter,
        "gauge" => MetricKind::Gauge,
        "histogram" => MetricKind::Histogram,
        _ => return None,
    })
}

#[derive(Serialize, Deserialize)]
struct WireLog {
    src: WireSource,
    /// Nanoseconds since the epoch.
    t: i128,
    sev: String,
    tpl: Option<String>,
    txt: Option<String>,
    f: Vec<WireAttr>,
    a: Vec<WireAttr>,
}

/// The Loki line of a stored log (tests of the read path on a fake Loki).
#[cfg(test)]
pub(crate) fn wire_log_line(r: &StoredLog) -> String {
    serde_json::to_string(&WireLog::from_stored(r)).expect("a stored log serialises to JSON")
}

impl WireLog {
    fn from_stored(r: &StoredLog) -> Self {
        WireLog {
            src: WireSource::from_source(&r.source),
            t: r.time.unix_timestamp_nanos(),
            sev: severity_name(r.severity).to_owned(),
            tpl: r.template.clone(),
            txt: r.text.clone(),
            f: wire_attrs(&r.fields),
            a: wire_attrs(&r.attributes),
        }
    }

    fn into_stored(self) -> Option<StoredLog> {
        Some(StoredLog {
            source: self.src.into_source()?,
            time: OffsetDateTime::from_unix_timestamp_nanos(self.t).ok()?,
            severity: severity_from(&self.sev)?,
            template: self.tpl,
            text: self.txt,
            fields: stored_attrs(self.f)?,
            attributes: stored_attrs(self.a)?,
        })
    }
}

fn kv(key: &str, value: impl Into<String>) -> KeyValue {
    KeyValue {
        key: key.to_owned(),
        value: Some(AnyValue {
            value: Some(any_value::Value::StringValue(value.into())),
        }),
        ..Default::default()
    }
}

/// A stored attribute as an OTLP attribute whose string value is the JSON of the value.
fn encoded_kv(a: &StoredAttribute) -> KeyValue {
    let json = serde_json::to_string(&WireValue::from_stored(&a.value))
        .expect("a stored value serialises to JSON");
    kv(&a.key, json)
}

fn decoded_attrs(kvs: Vec<KeyValue>) -> Option<Vec<StoredAttribute>> {
    kvs.into_iter()
        .map(|kv| {
            let s = match kv.value?.value? {
                any_value::Value::StringValue(s) => s,
                _ => return None,
            };
            let v: WireValue = serde_json::from_str(&s).ok()?;
            Some(StoredAttribute {
                key: kv.key,
                value: v.into_stored()?,
            })
        })
        .collect()
}

fn string_attr<'a>(kvs: &'a [KeyValue], key: &str) -> Option<&'a str> {
    kvs.iter()
        .find(|kv| kv.key == key)
        .and_then(|kv| match kv.value.as_ref()?.value.as_ref()? {
            any_value::Value::StringValue(s) => Some(s.as_str()),
            _ => None,
        })
}

fn nanos(t: OffsetDateTime) -> u64 {
    u64::try_from(t.unix_timestamp_nanos()).unwrap_or(0)
}

fn resource_of(identity: &SourceIdentity) -> Resource {
    Resource {
        attributes: vec![
            kv(
                SERVICE_NAME,
                identity.app.as_uuid().hyphenated().to_string(),
            ),
            kv(RES_DEPLOYMENT, identity.deployment.hyphenated().to_string()),
            kv(RES_ENVIRONMENT, identity.environment.as_str()),
        ],
        ..Default::default()
    }
}

fn hex_bytes(s: &str) -> Vec<u8> {
    hex::decode(s).unwrap_or_default()
}

// ---------------------------------------------------------------------------- store

fn store_error(what: &str) -> crate::Error {
    crate::Error::Internal(format!("telemetry store: {what}"))
}

impl LgtmStore {
    pub fn new(cfg: LgtmConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(cfg.timeout)
            .build()
            .map_err(|_| store_error("cannot build the HTTP client"))?;
        Ok(LgtmStore {
            cfg,
            http,
            answer_cap: MAX_ANSWER_BYTES,
            trace_cap: MAX_TRACE_BYTES,
        })
    }

    /// Other caps of a store answer and of a Tempo trace answer (tests).
    pub fn with_answer_caps(mut self, answer: usize, trace: usize) -> Self {
        self.answer_cap = answer;
        self.trace_cap = trace;
        self
    }

    /// Reads an answer body as it arrives; with more than `cap` bytes it stops reading and
    /// answers `None`.
    async fn read_capped(mut res: reqwest::Response, cap: usize) -> Result<Option<Vec<u8>>> {
        if res.content_length().is_some_and(|n| n > cap as u64) {
            return Ok(None);
        }
        let mut out = Vec::new();
        while let Some(chunk) = res
            .chunk()
            .await
            .map_err(|_| store_error("a store answer was cut off"))?
        {
            if out.len() + chunk.len() > cap {
                return Ok(None);
            }
            out.extend_from_slice(&chunk);
        }
        Ok(Some(out))
    }

    fn base(url: &str) -> &str {
        url.trim_end_matches('/')
    }

    /// Posts one OTLP request to the Collector. Any non-2xx is a refusal (S-62).
    async fn post(&self, path: &str, tenant: TenantId, body: Vec<u8>) -> Result<()> {
        let url = format!("{}{path}", Self::base(&self.cfg.collector_url));
        let res = self
            .http
            .post(&url)
            .header(TENANT_HEADER, tenant_header(tenant))
            .header(
                "Authorization",
                format!("Bearer {}", self.cfg.collector_token.expose()),
            )
            .header("Content-Type", "application/x-protobuf")
            .body(body)
            .send()
            .await
            .map_err(|_| store_error(&format!("the Collector cannot be reached ({path})")))?;
        let status = res.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(store_error(&format!(
                "the Collector refused the write to {path} with status {}",
                status.as_u16()
            )))
        }
    }

    async fn get(&self, url: &str, tenant: TenantId, query: &[(&str, String)]) -> Result<Vec<u8>> {
        let res = self
            .http
            .get(url)
            .header(TENANT_HEADER, tenant_header(tenant))
            .header("Accept", "application/json")
            .query(query)
            .send()
            .await
            .map_err(|_| store_error("a store cannot be reached"))?;
        let status = res.status();
        if !status.is_success() {
            return Err(store_error(&format!(
                "a store refused the read with status {}",
                status.as_u16()
            )));
        }
        Self::read_capped(res, self.answer_cap)
            .await?
            .ok_or_else(|| store_error("a store answer is larger than the read cap"))
    }

    fn logs_request(identity: &SourceIdentity, logs: &[StoredLog]) -> (Vec<u8>, usize) {
        let mut skipped = 0;
        let records = logs
            .iter()
            .filter_map(|r| {
                let line = serde_json::to_string(&WireLog::from_stored(r))
                    .expect("a stored log serialises to JSON");
                if line.len() > MAX_LOG_LINE {
                    skipped += 1;
                    return None;
                }
                Some(otlp_logs::LogRecord {
                    time_unix_nano: nanos(r.time),
                    observed_time_unix_nano: nanos(r.time),
                    severity_number: severity_number(r.severity),
                    severity_text: severity_name(r.severity).to_owned(),
                    body: Some(AnyValue {
                        value: Some(any_value::Value::StringValue(line)),
                    }),
                    ..Default::default()
                })
            })
            .collect();
        let req = ExportLogsServiceRequest {
            resource_logs: vec![otlp_logs::ResourceLogs {
                resource: Some(resource_of(identity)),
                scope_logs: vec![otlp_logs::ScopeLogs {
                    log_records: records,
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };
        (req.encode_to_vec(), skipped)
    }

    fn metrics_request(identity: &SourceIdentity, metrics: &[StoredMetric]) -> (Vec<u8>, usize) {
        let mut skipped = 0;
        // (series key, time ms, point) so the points of one series go in time order.
        let mut points: Vec<(String, i64, otlp_metrics::NumberDataPoint)> = Vec::new();
        for m in metrics {
            let labels = serde_json::to_string(&wire_attrs(&m.labels))
                .expect("stored labels serialise to JSON");
            if labels.len() > MAX_LABEL_VALUE || m.name.len() > MAX_LABEL_VALUE {
                skipped += 1;
                continue;
            }
            let attrs = vec![
                kv("ruum_app", m.source.app.as_uuid().hyphenated().to_string()),
                kv(
                    "ruum_deployment",
                    m.source.deployment.hyphenated().to_string(),
                ),
                kv("ruum_environment", m.source.environment.as_str()),
                kv("ruum_kind", metric_kind_name(m.kind)),
                kv("ruum_name", m.name.clone()),
                kv("ruum_labels", labels),
            ];
            let key = attrs
                .iter()
                .filter_map(|a| string_attr(std::slice::from_ref(a), &a.key))
                .collect::<Vec<_>>()
                .join("\u{0}");
            let ms = (m.time.unix_timestamp_nanos() / 1_000_000) as i64;
            points.push((
                key,
                ms,
                otlp_metrics::NumberDataPoint {
                    attributes: attrs,
                    value: Some(otlp_metrics::number_data_point::Value::AsDouble(m.value)),
                    ..Default::default()
                },
            ));
        }
        points.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        // Two points of one series in the same millisecond: the later one moves 1 ms on.
        let mut last: Option<(String, i64)> = None;
        let mut data_points = Vec::with_capacity(points.len());
        for (key, mut ms, mut p) in points {
            if let Some((k, t)) = &last
                && *k == key
                && ms <= *t
            {
                ms = t + 1;
            }
            p.time_unix_nano = u64::try_from(ms).unwrap_or(0) * 1_000_000;
            last = Some((key, ms));
            data_points.push(p);
        }
        let _ = identity;
        let req = ExportMetricsServiceRequest {
            resource_metrics: vec![otlp_metrics::ResourceMetrics {
                resource: Some(Resource::default()),
                scope_metrics: vec![otlp_metrics::ScopeMetrics {
                    metrics: vec![otlp_metrics::Metric {
                        name: METRIC_NAME.to_owned(),
                        data: Some(otlp_metrics::metric::Data::Gauge(otlp_metrics::Gauge {
                            data_points,
                        })),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };
        (req.encode_to_vec(), skipped)
    }

    fn traces_request(identity: &SourceIdentity, spans: &[StoredSpan]) -> Vec<u8> {
        let spans = spans
            .iter()
            .map(|s| otlp_trace::Span {
                trace_id: hex_bytes(&s.trace_id),
                span_id: hex_bytes(&s.span_id),
                parent_span_id: s
                    .parent_span_id
                    .as_deref()
                    .map(hex_bytes)
                    .unwrap_or_default(),
                name: s.name.clone(),
                kind: match s.kind {
                    SpanKind::Internal => 1,
                    SpanKind::Server => 2,
                    SpanKind::Client => 3,
                    SpanKind::Producer => 4,
                    SpanKind::Consumer => 5,
                },
                start_time_unix_nano: nanos(s.start),
                end_time_unix_nano: nanos(s.end),
                attributes: s.attributes.iter().map(encoded_kv).collect(),
                events: s
                    .events
                    .iter()
                    .map(|e| otlp_trace::span::Event {
                        time_unix_nano: nanos(e.time),
                        name: e.name.clone(),
                        attributes: e.attributes.iter().map(encoded_kv).collect(),
                        ..Default::default()
                    })
                    .collect(),
                links: s
                    .links
                    .iter()
                    .map(|l| otlp_trace::span::Link {
                        trace_id: hex_bytes(&l.trace_id),
                        span_id: hex_bytes(&l.span_id),
                        attributes: l.attributes.iter().map(encoded_kv).collect(),
                        ..Default::default()
                    })
                    .collect(),
                status: Some(otlp_trace::Status {
                    message: s.status_message.clone().unwrap_or_default(),
                    code: match s.status {
                        StatusCode::Unset => 0,
                        StatusCode::Ok => 1,
                        StatusCode::Error => 2,
                    },
                }),
                ..Default::default()
            })
            .collect();
        ExportTraceServiceRequest {
            resource_spans: vec![otlp_trace::ResourceSpans {
                resource: Some(resource_of(identity)),
                scope_spans: vec![otlp_trace::ScopeSpans {
                    spans,
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
        .encode_to_vec()
    }

    /// The time range of a read: `from` (default: [`DEFAULT_LOOKBACK`] ago) to `to` (default:
    /// five minutes from now).
    fn range(f: &TelemetryFilter) -> (OffsetDateTime, OffsetDateTime) {
        let now = crate::now();
        let to = f.to.unwrap_or(now + time::Duration::minutes(5));
        let from = f.from.unwrap_or(now - DEFAULT_LOOKBACK);
        (from, to)
    }

    /// The LogQL query of a read (public for its unit tests).
    pub fn logql(f: &TelemetryFilter) -> String {
        let mut q = match f.app {
            Some(app) => format!(
                "{{service_name={}}}",
                quote(&app.as_uuid().hyphenated().to_string())
            ),
            None => "{service_name=~\".+\"}".to_owned(),
        };
        if let Some(env) = f.environment {
            q.push_str(&format!(" | ruum_environment={}", quote(env.as_str())));
        }
        if let Some(d) = f.deployment {
            q.push_str(&format!(
                " | ruum_deployment={}",
                quote(&d.hyphenated().to_string())
            ));
        }
        for eq in &f.fields {
            // The key as it is in the line: "k":"<JSON-escaped key>". Only narrows the read;
            // the equality itself is checked on the decoded record.
            let key_json = serde_json::to_string(&eq.key).expect("a string serialises");
            q.push_str(&format!(" |= {}", quote(&format!("\"k\":{key_json}"))));
        }
        // Card O-2: the words and the trace ID as line filters (`|=`, a plain substring, never a
        // regex or a pipeline stage), each value through `quote`. They only narrow the read; the
        // match itself is checked on the decoded record (`log_extra_matches`). A word that the
        // JSON line would hold escaped (a quote, a backslash, a control character) gets no line
        // filter, so the filter never misses a record that the check would keep.
        for w in f.words.iter().filter(|w| line_safe(w)) {
            q.push_str(&format!(" |= {}", quote(w)));
        }
        if let Some(t) = f.trace_id.as_deref().filter(|t| is_trace_id(t)) {
            q.push_str(&format!(" |= {}", quote(t)));
        }
        q
    }

    /// The PromQL query of a read (public for its unit tests).
    pub fn promql(f: &TelemetryFilter, range_ms: i64) -> String {
        let mut matchers = Vec::new();
        if let Some(app) = f.app {
            matchers.push(format!(
                "ruum_app={}",
                quote(&app.as_uuid().hyphenated().to_string())
            ));
        }
        if let Some(env) = f.environment {
            matchers.push(format!("ruum_environment={}", quote(env.as_str())));
        }
        if let Some(d) = f.deployment {
            matchers.push(format!(
                "ruum_deployment={}",
                quote(&d.hyphenated().to_string())
            ));
        }
        format!(
            "{METRIC_NAME}{{{}}}[{}ms]",
            matchers.join(","),
            range_ms.max(1)
        )
    }

    /// The TraceQL query of a read (public for its unit tests).
    pub fn traceql(f: &TelemetryFilter) -> String {
        let mut conds = Vec::new();
        if let Some(app) = f.app {
            conds.push(format!(
                "resource.service.name = {}",
                quote(&app.as_uuid().hyphenated().to_string())
            ));
        }
        if let Some(env) = f.environment {
            conds.push(format!(
                "resource.ruum.environment = {}",
                quote(env.as_str())
            ));
        }
        if let Some(d) = f.deployment {
            conds.push(format!(
                "resource.ruum.deployment = {}",
                quote(&d.hyphenated().to_string())
            ));
        }
        if conds.is_empty() {
            "{ }".to_owned()
        } else {
            format!("{{ {} }}", conds.join(" && "))
        }
    }

    /// The `limit` oldest logs of the filter. Loki answers forward, oldest first, so the paging
    /// stops once `limit` records are found.
    async fn read_logs(
        &self,
        tenant: TenantId,
        f: &TelemetryFilter,
        limit: usize,
    ) -> Result<Vec<StoredLog>> {
        if f.newest {
            return self.read_logs_newest(tenant, f, limit).await;
        }
        let (from, to) = Self::range(f);
        let url = format!("{}/loki/api/v1/query_range", Self::base(&self.cfg.loki_url));
        let query = Self::logql(f);
        let mut start = from.unix_timestamp_nanos();
        let end = to.unix_timestamp_nanos();
        let mut out = Vec::new();
        let mut dropped = 0u64;
        for _ in 0..LOKI_PAGES {
            if start >= end {
                break;
            }
            let body = self
                .get(
                    &url,
                    tenant,
                    &[
                        ("query", query.clone()),
                        ("start", start.to_string()),
                        ("end", end.to_string()),
                        ("limit", LOKI_PAGE.to_string()),
                        ("direction", "forward".to_owned()),
                    ],
                )
                .await?;
            let page: LokiAnswer =
                serde_json::from_slice(&body).map_err(|_| store_error("a Loki answer"))?;
            let mut n = 0usize;
            let mut last = start;
            for stream in page.data.result {
                for entry in stream.values {
                    n += 1;
                    let ts: i128 = entry.0.parse().unwrap_or(start);
                    last = last.max(ts);
                    match serde_json::from_str::<WireLog>(&entry.1)
                        .ok()
                        .and_then(WireLog::into_stored)
                    {
                        Some(r) if log_matches(&r, tenant, f) => out.push(r),
                        Some(r) if r.source.tenant != tenant => dropped += 1,
                        Some(_) => {}
                        None => dropped += 1,
                    }
                }
            }
            if out.len() > limit {
                out.sort_by_key(|r| r.time);
                out.truncate(limit);
            }
            if n < LOKI_PAGE || out.len() >= limit {
                break;
            }
            start = last + 1;
        }
        if dropped > 0 {
            tracing::warn!(tenant = %tenant, dropped, "telemetry store: Loki returned records that are not of this tenant or not of the record encoding; they were dropped");
        }
        Ok(out)
    }

    /// The `limit` newest logs of the filter (card O-2): Loki answers backward, newest first,
    /// so the paging moves the end back and stops once `limit` records are found. A record at
    /// the exact end of the previous page is read once.
    async fn read_logs_newest(
        &self,
        tenant: TenantId,
        f: &TelemetryFilter,
        limit: usize,
    ) -> Result<Vec<StoredLog>> {
        let (from, to) = Self::range(f);
        let url = format!("{}/loki/api/v1/query_range", Self::base(&self.cfg.loki_url));
        let query = Self::logql(f);
        let start = from.unix_timestamp_nanos();
        let mut end = to.unix_timestamp_nanos();
        let mut seen_before: Option<i128> = None;
        let mut out: Vec<StoredLog> = Vec::new();
        let mut dropped = 0u64;
        for _ in 0..LOKI_PAGES {
            if start >= end {
                break;
            }
            let body = self
                .get(
                    &url,
                    tenant,
                    &[
                        ("query", query.clone()),
                        ("start", start.to_string()),
                        ("end", end.to_string()),
                        ("limit", LOKI_PAGE.to_string()),
                        ("direction", "backward".to_owned()),
                    ],
                )
                .await?;
            let page: LokiAnswer =
                serde_json::from_slice(&body).map_err(|_| store_error("a Loki answer"))?;
            let mut n = 0usize;
            let mut first = end;
            for stream in page.data.result {
                for entry in stream.values {
                    n += 1;
                    let ts: i128 = entry.0.parse().unwrap_or(end);
                    first = first.min(ts);
                    if seen_before.is_some_and(|b| ts >= b) {
                        continue;
                    }
                    match serde_json::from_str::<WireLog>(&entry.1)
                        .ok()
                        .and_then(WireLog::into_stored)
                    {
                        Some(r) if log_matches(&r, tenant, f) => out.push(r),
                        Some(r) if r.source.tenant != tenant => dropped += 1,
                        Some(_) => {}
                        None => dropped += 1,
                    }
                }
            }
            if out.len() > limit {
                out.sort_by_key(|r| std::cmp::Reverse(r.time));
                out.truncate(limit);
            }
            if n < LOKI_PAGE || out.len() >= limit {
                break;
            }
            seen_before = Some(first);
            end = first;
        }
        if dropped > 0 {
            tracing::warn!(tenant = %tenant, dropped, "telemetry store: Loki returned records that are not of this tenant or not of the record encoding; they were dropped");
        }
        Ok(out)
    }

    /// The `limit` oldest points of the filter. Every part of a match but the time is the
    /// series', so a series is checked once; points are kept as (time, value, series) and the
    /// name and labels are cloned only for the `limit` points returned (Kimi run 13 re-check:
    /// the answer is capped, but cloning the labels for every point was not).
    async fn read_metrics(
        &self,
        tenant: TenantId,
        f: &TelemetryFilter,
        limit: usize,
    ) -> Result<Vec<StoredMetric>> {
        let (from, to) = Self::range(f);
        let range_ms = ((to - from).whole_milliseconds() as i64).max(1);
        let url = format!(
            "{}/prometheus/api/v1/query",
            Self::base(&self.cfg.mimir_url)
        );
        // The range selector is open at its start: one more millisecond keeps `from`, and the
        // evaluation time is just before `to` (exclusive).
        let at = (to.unix_timestamp_nanos() - 1_000_000) as f64 / 1e9;
        let body = self
            .get(
                &url,
                tenant,
                &[
                    ("query", Self::promql(f, range_ms)),
                    ("time", format!("{at:.3}")),
                ],
            )
            .await?;
        let ans: PromAnswer =
            serde_json::from_slice(&body).map_err(|_| store_error("a Mimir answer"))?;
        let (out, dropped, _) = select_metrics(ans, tenant, f, limit);
        if dropped > 0 {
            tracing::warn!(tenant = %tenant, dropped, "telemetry store: Mimir returned series that are not of the record encoding; they were dropped");
        }
        Ok(out)
    }

    /// The `limit` oldest spans of the filter (by start). Tempo search does not order its
    /// traces by age, so every trace found is read, one at a time, and only the `limit`
    /// oldest spans are kept between them.
    async fn read_spans(
        &self,
        tenant: TenantId,
        f: &TelemetryFilter,
        limit: usize,
    ) -> Result<Vec<StoredSpan>> {
        let (from, to) = Self::range(f);
        // Card O-2: one trace is read by its ID, without a search; a trace ID that is not of
        // the OTel form matches nothing.
        let ids = match f.trace_id.as_deref() {
            Some(t) if is_trace_id(t) => vec![t.to_owned()],
            Some(_) => return Ok(Vec::new()),
            None => self.trace_ids(tenant, &Self::traceql(f), from, to).await?,
        };
        let mut out = Vec::new();
        let mut dropped = 0u64;
        let mut too_large = 0u64;
        for id in ids {
            if out.len() > limit {
                out.sort_by_key(|r: &StoredSpan| r.start);
                out.truncate(limit);
            }
            let trace = match self.trace(tenant, &id).await? {
                Trace::Found(t) => t,
                Trace::Missing => continue,
                Trace::TooLarge => {
                    too_large += 1;
                    continue;
                }
            };
            for rs in trace.resource_spans {
                let attrs = rs.resource.map(|r| r.attributes).unwrap_or_default();
                let Some(source) = resource_source(&attrs, tenant) else {
                    dropped += 1;
                    continue;
                };
                for ss in rs.scope_spans {
                    for s in ss.spans {
                        match decode_span(s, source) {
                            Some(r) if span_matches(&r, tenant, f) => out.push(r),
                            Some(_) => {}
                            None => dropped += 1,
                        }
                    }
                }
            }
        }
        if dropped > 0 {
            tracing::warn!(tenant = %tenant, dropped, "telemetry store: Tempo returned spans that are not of the record encoding; they were dropped");
        }
        if too_large > 0 {
            tracing::warn!(tenant = %tenant, traces = too_large, "telemetry store: Tempo traces over the read cap were skipped");
        }
        Ok(out)
    }

    /// The trace IDs of a TraceQL search, padded to 32 hex characters (Tempo drops leading
    /// zeros).
    async fn trace_ids(
        &self,
        tenant: TenantId,
        q: &str,
        from: OffsetDateTime,
        to: OffsetDateTime,
    ) -> Result<Vec<String>> {
        let url = format!("{}/api/search", Self::base(&self.cfg.tempo_url));
        let start = from.unix_timestamp();
        let end = to.unix_timestamp().max(start + 1);
        let body = self
            .get(
                &url,
                tenant,
                &[
                    ("q", q.to_owned()),
                    ("start", start.to_string()),
                    ("end", end.to_string()),
                    ("limit", TEMPO_TRACES.to_string()),
                    ("spss", TEMPO_SPANS_PER_SET.to_string()),
                ],
            )
            .await?;
        let ans: TempoSearch =
            serde_json::from_slice(&body).map_err(|_| store_error("a Tempo search answer"))?;
        Ok(ans
            .traces
            .into_iter()
            .filter(|t| !t.trace_id.is_empty() && t.trace_id.len() <= 32)
            .filter(|t| t.trace_id.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(|t| format!("{:0>32}", t.trace_id.to_ascii_lowercase()))
            .collect())
    }

    /// One trace as OTLP (Tempo's protobuf answer: `tempopb.Trace`, whose field 1 is the
    /// resource spans, as in `TracesData` and in the export request). The answer is read up to
    /// the trace cap and walked like a received request ([`otlp::check`]) before prost decodes
    /// it, so its decoded size is bounded too.
    async fn trace(&self, tenant: TenantId, id: &str) -> Result<Trace> {
        let raw = match self.trace_raw(tenant, id, self.trace_cap).await? {
            Some(Some(raw)) => raw,
            Some(None) => return Ok(Trace::TooLarge),
            None => return Ok(Trace::Missing),
        };
        let limits = Limits {
            max_records: MAX_TRACE_SPANS,
            max_batch_bytes: self.trace_cap,
            ..Limits::default()
        };
        match otlp::check(Signal::Traces, &raw, &limits) {
            Ok(_) => {}
            Err(otlp::Refusal::BodyTooLarge | otlp::Refusal::TooManyRecords) => {
                return Ok(Trace::TooLarge);
            }
            Err(_) => return Err(store_error("a Tempo trace answer")),
        }
        otlp_trace::TracesData::decode(raw.as_slice())
            .map(Trace::Found)
            .map_err(|_| store_error("a Tempo trace answer"))
    }

    /// A trace answer: `None` when Tempo does not know it, `Some(None)` when it is larger than
    /// `cap`.
    async fn trace_raw(
        &self,
        tenant: TenantId,
        id: &str,
        cap: usize,
    ) -> Result<Option<Option<Vec<u8>>>> {
        let url = format!("{}/api/traces/{id}", Self::base(&self.cfg.tempo_url));
        let res = self
            .http
            .get(&url)
            .header(TENANT_HEADER, tenant_header(tenant))
            .header("Accept", "application/protobuf")
            .send()
            .await
            .map_err(|_| store_error("Tempo cannot be reached"))?;
        match res.status().as_u16() {
            404 => Ok(None),
            s if (200..300).contains(&s) => Self::read_capped(res, cap).await.map(Some),
            s => Err(store_error(&format!(
                "Tempo refused the read with status {s}"
            ))),
        }
    }

    /// Everything the stores hold for one tenant, as the stores answer (raw Loki and Mimir JSON,
    /// raw Tempo protobuf), for the privacy suite's secret scan.
    pub async fn dump_tenant(&self, tenant: TenantId) -> Result<Vec<u8>> {
        let now = crate::now();
        let from = now - DEFAULT_LOOKBACK;
        let to = now + time::Duration::minutes(5);
        let mut out = Vec::new();
        let loki = self
            .get(
                &format!("{}/loki/api/v1/query_range", Self::base(&self.cfg.loki_url)),
                tenant,
                &[
                    ("query", "{service_name=~\".+\"}".to_owned()),
                    ("start", from.unix_timestamp_nanos().to_string()),
                    ("end", to.unix_timestamp_nanos().to_string()),
                    ("limit", LOKI_PAGE.to_string()),
                    ("direction", "forward".to_owned()),
                ],
            )
            .await?;
        out.extend_from_slice(&loki);
        out.push(b'\n');
        let range_ms = (to - from).whole_milliseconds();
        let mimir = self
            .get(
                &format!(
                    "{}/prometheus/api/v1/query",
                    Self::base(&self.cfg.mimir_url)
                ),
                tenant,
                &[
                    ("query", format!("{{__name__=~\".+\"}}[{range_ms}ms]")),
                    (
                        "time",
                        format!("{:.3}", to.unix_timestamp_nanos() as f64 / 1e9),
                    ),
                ],
            )
            .await?;
        out.extend_from_slice(&mimir);
        out.push(b'\n');
        for id in self.trace_ids(tenant, "{ }", from, to).await? {
            // The secret scan must see every trace: one over the cap fails the dump.
            match self.trace_raw(tenant, &id, self.answer_cap).await? {
                Some(Some(raw)) => {
                    out.extend_from_slice(&raw);
                    out.push(b'\n');
                }
                Some(None) => {
                    return Err(store_error("a store answer is larger than the read cap"));
                }
                None => {}
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl TelemetryStore for LgtmStore {
    async fn write(&self, identity: &SourceIdentity, batch: ClassifiedBatch) -> Result<()> {
        // The storage tenant comes only from the identity.
        let tenant = identity.tenant;
        let mut skipped = 0usize;
        if !batch.logs.is_empty() {
            let (body, s) = Self::logs_request(identity, &batch.logs);
            skipped += s;
            self.post("/v1/logs", tenant, body).await?;
        }
        if !batch.metrics.is_empty() {
            let (body, s) = Self::metrics_request(identity, &batch.metrics);
            skipped += s;
            self.post("/v1/metrics", tenant, body).await?;
        }
        if !batch.spans.is_empty() {
            let body = Self::traces_request(identity, &batch.spans);
            self.post("/v1/traces", tenant, body).await?;
        }
        if skipped > 0 {
            tracing::warn!(
                tenant = %identity.tenant,
                app = %identity.app,
                deployment = %identity.deployment,
                skipped,
                "telemetry store: records over the stores' size limits were not written"
            );
        }
        Ok(())
    }

    async fn read(&self, tenant: TenantId, f: &TelemetryFilter) -> Result<ClassifiedBatch> {
        let limit = f.limit.unwrap_or(MAX_READ).min(MAX_READ);
        let want = |s: Signal| f.signal.is_none_or(|w| w == s);
        let mut out = ClassifiedBatch::default();
        if limit == 0 {
            return Ok(out);
        }
        // Card O-2: `newest` keeps the newest `limit` records. Logs page backward in Loki;
        // metrics and spans are read up to MAX_READ and the newest kept.
        let read_limit = if f.newest { MAX_READ } else { limit };
        if want(Signal::Logs) && f.origin.is_none_or(|o| o == LogOrigin::Backend) {
            let mut logs = self.read_logs(tenant, f, limit).await?;
            logs.sort_by_key(|r| r.time);
            keep(&mut logs, limit, f.newest);
            out.logs = logs;
        }
        // The filters of logs only (severity, words, origin) leave out the other signals.
        let logs_only = f.min_severity.is_some() || !f.words.is_empty() || f.origin.is_some();
        if want(Signal::Metrics) && !logs_only && f.trace_id.is_none() {
            let mut metrics = self.read_metrics(tenant, f, read_limit).await?;
            metrics.sort_by_key(|r| r.time);
            keep(&mut metrics, limit, f.newest);
            out.metrics = metrics;
        }
        if want(Signal::Traces) && !logs_only {
            let mut spans = self.read_spans(tenant, f, read_limit).await?;
            spans.sort_by_key(|r| r.start);
            keep(&mut spans, limit, f.newest);
            out.spans = spans;
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------- answers

/// A Tempo trace answer.
enum Trace {
    Found(otlp_trace::TracesData),
    /// Tempo does not know the trace.
    Missing,
    /// Over the trace cap or the span cap: skipped and counted.
    TooLarge,
}

#[derive(Deserialize)]
struct LokiAnswer {
    data: LokiData,
}

#[derive(Deserialize)]
struct LokiData {
    #[serde(default)]
    result: Vec<LokiStream>,
}

#[derive(Deserialize)]
struct LokiStream {
    #[serde(default)]
    values: Vec<LokiEntry>,
}

/// `[ts, line]` or `[ts, line, {structured metadata}]`.
struct LokiEntry(String, String);

impl<'de> Deserialize<'de> for LokiEntry {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let v: Vec<serde_json::Value> = Vec::deserialize(d)?;
        let ts = v.first().and_then(|x| x.as_str()).unwrap_or_default();
        let line = v.get(1).and_then(|x| x.as_str()).unwrap_or_default();
        Ok(LokiEntry(ts.to_owned(), line.to_owned()))
    }
}

#[derive(Deserialize)]
struct PromAnswer {
    data: PromData,
}

#[derive(Deserialize)]
struct PromData {
    #[serde(default)]
    result: Vec<PromSeries>,
}

#[derive(Deserialize)]
struct PromSeries {
    metric: HashMap<String, String>,
    #[serde(default)]
    values: Vec<(f64, String)>,
}

#[derive(Deserialize)]
struct TempoSearch {
    #[serde(default)]
    traces: Vec<TempoTrace>,
}

#[derive(Deserialize)]
struct TempoTrace {
    #[serde(rename = "traceID", default)]
    trace_id: String,
}

/// The decoded labels of one Mimir series (the tenant comes from the read).
struct MetricHead {
    source: SourceIdentity,
    name: String,
    kind: MetricKind,
    labels: Vec<StoredAttribute>,
}

/// The `limit` oldest points of a Mimir answer, the series dropped as not of the record
/// encoding, and the most points held at once (at most `max(2 * limit, 1024)`). See
/// [`LgtmStore::read_metrics`].
fn select_metrics(
    ans: PromAnswer,
    tenant: TenantId,
    f: &TelemetryFilter,
    limit: usize,
) -> (Vec<StoredMetric>, u64, usize) {
    let mut heads: Vec<MetricHead> = Vec::new();
    // (time, value, index in `heads`); cut to the `limit` oldest at `max(2 * limit, 1024)`.
    let mut points: Vec<(OffsetDateTime, f64, usize)> = Vec::new();
    let mut dropped = 0u64;
    let mut peak = 0usize;
    for series in ans.data.result {
        let Some(head) = metric_head(&series.metric) else {
            dropped += 1;
            continue;
        };
        let head = MetricHead {
            source: SourceIdentity {
                tenant,
                ..head.source
            },
            ..head
        };
        let series_ok = source_ok(&head.source, tenant, f)
            && identity_agrees(&head.labels, &head.source)
            && fields_ok(f, &[&head.labels]);
        if !series_ok {
            continue;
        }
        let at = heads.len();
        for (t, v) in series.values {
            let ms = (t * 1000.0).round() as i128;
            let Ok(time) = OffsetDateTime::from_unix_timestamp_nanos(ms * 1_000_000) else {
                continue;
            };
            let Ok(value) = v.parse::<f64>() else {
                continue;
            };
            if time_ok(time, f) {
                points.push((time, value, at));
            }
            peak = peak.max(points.len());
            if points.len() >= limit.saturating_mul(2).max(1024) {
                points.sort_by_key(|p| p.0);
                points.truncate(limit);
            }
        }
        heads.push(head);
    }
    points.sort_by_key(|p| p.0);
    points.truncate(limit);
    let out: Vec<StoredMetric> = points
        .into_iter()
        .map(|(time, value, i)| {
            let h = &heads[i];
            StoredMetric {
                source: h.source,
                time,
                name: h.name.clone(),
                kind: h.kind,
                value,
                labels: h.labels.clone(),
            }
        })
        // The whole check again on what is returned (the series check above is the same
        // without the time).
        .filter(|r| metric_matches(r, tenant, f))
        .collect();
    (out, dropped, peak)
}

/// [`select_metrics`] on a raw Mimir answer (tests).
#[cfg(test)]
pub(crate) fn select_mimir_answer(
    body: &[u8],
    tenant: TenantId,
    f: &TelemetryFilter,
    limit: usize,
) -> (Vec<StoredMetric>, usize) {
    let ans: PromAnswer = serde_json::from_slice(body).expect("a Mimir answer");
    let (out, _, peak) = select_metrics(ans, tenant, f, limit);
    (out, peak)
}

/// The Mimir series labels of a stored metric, as [`LgtmStore`] writes them (tests).
#[cfg(test)]
pub(crate) fn mimir_series_labels(m: &StoredMetric) -> HashMap<String, String> {
    HashMap::from([
        (
            "ruum_app".to_owned(),
            m.source.app.as_uuid().hyphenated().to_string(),
        ),
        (
            "ruum_deployment".to_owned(),
            m.source.deployment.hyphenated().to_string(),
        ),
        (
            "ruum_environment".to_owned(),
            m.source.environment.as_str().to_owned(),
        ),
        ("ruum_kind".to_owned(), metric_kind_name(m.kind).to_owned()),
        ("ruum_name".to_owned(), m.name.clone()),
        (
            "ruum_labels".to_owned(),
            serde_json::to_string(&wire_attrs(&m.labels)).expect("labels serialise"),
        ),
    ])
}

fn metric_head(m: &HashMap<String, String>) -> Option<MetricHead> {
    let labels: Vec<WireAttr> = serde_json::from_str(m.get("ruum_labels")?).ok()?;
    Some(MetricHead {
        source: SourceIdentity {
            // Replaced by the read's tenant.
            tenant: TenantId::from(Uuid::nil()),
            app: m.get("ruum_app")?.parse().ok()?,
            deployment: m.get("ruum_deployment")?.parse().ok()?,
            environment: environment_of(m.get("ruum_environment")?)?,
        },
        name: m.get("ruum_name")?.clone(),
        kind: metric_kind_from(m.get("ruum_kind")?)?,
        labels: stored_attrs(labels)?,
    })
}

fn resource_source(attrs: &[KeyValue], tenant: TenantId) -> Option<SourceIdentity> {
    Some(SourceIdentity {
        tenant,
        app: string_attr(attrs, SERVICE_NAME)?.parse().ok()?,
        deployment: string_attr(attrs, RES_DEPLOYMENT)?.parse().ok()?,
        environment: environment_of(string_attr(attrs, RES_ENVIRONMENT)?)?,
    })
}

fn decode_span(s: otlp_trace::Span, source: SourceIdentity) -> Option<StoredSpan> {
    let start = super::otlp::time_of(s.start_time_unix_nano)?;
    let end = super::otlp::time_of(s.end_time_unix_nano).unwrap_or(start);
    let (status, status_message) = match s.status {
        Some(st) => (
            match st.code {
                1 => StatusCode::Ok,
                2 => StatusCode::Error,
                _ => StatusCode::Unset,
            },
            (!st.message.is_empty()).then_some(st.message),
        ),
        None => (StatusCode::Unset, None),
    };
    Some(StoredSpan {
        source,
        trace_id: hex::encode(&s.trace_id),
        span_id: hex::encode(&s.span_id),
        parent_span_id: super::otlp::hex_id(&s.parent_span_id),
        name: s.name,
        kind: match s.kind {
            2 => SpanKind::Server,
            3 => SpanKind::Client,
            4 => SpanKind::Producer,
            5 => SpanKind::Consumer,
            _ => SpanKind::Internal,
        },
        start,
        end,
        status,
        status_message,
        attributes: decoded_attrs(s.attributes)?,
        events: s
            .events
            .into_iter()
            .map(|e| {
                Some(StoredEvent {
                    time: super::otlp::time_of(e.time_unix_nano).unwrap_or(start),
                    name: e.name,
                    attributes: decoded_attrs(e.attributes)?,
                })
            })
            .collect::<Option<Vec<_>>>()?,
        links: s
            .links
            .into_iter()
            .map(|l| {
                Some(StoredLink {
                    trace_id: hex::encode(&l.trace_id),
                    span_id: hex::encode(&l.span_id),
                    attributes: decoded_attrs(l.attributes)?,
                })
            })
            .collect::<Option<Vec<_>>>()?,
    })
}

// ---------------------------------------------------------------------------- matching

/// The identity attribute `key` of a record, as text.
fn identity_attr<'a>(attrs: &'a [StoredAttribute], key: &str) -> Option<&'a str> {
    attr(attrs, key).and_then(StoredValue::as_text)
}

/// The record's identity attributes agree with its source (the pipeline wrote both).
fn identity_agrees(attrs: &[StoredAttribute], s: &SourceIdentity) -> bool {
    identity_attr(attrs, ATTR_TENANT) == Some(tenant_header(s.tenant).as_str())
        && identity_attr(attrs, ATTR_APP) == Some(s.app.as_uuid().hyphenated().to_string().as_str())
        && identity_attr(attrs, ATTR_DEPLOYMENT)
            == Some(s.deployment.hyphenated().to_string().as_str())
        && identity_attr(attrs, ATTR_ENVIRONMENT) == Some(s.environment.as_str())
}

fn source_ok(s: &SourceIdentity, tenant: TenantId, f: &TelemetryFilter) -> bool {
    s.tenant == tenant
        && f.app.is_none_or(|a| a == s.app)
        && f.environment.is_none_or(|e| e == s.environment)
        && f.deployment.is_none_or(|d| d == s.deployment)
}

fn time_ok(t: OffsetDateTime, f: &TelemetryFilter) -> bool {
    f.from.is_none_or(|from| t >= from) && f.to.is_none_or(|to| t < to)
}

fn fields_ok(f: &TelemetryFilter, sets: &[&[StoredAttribute]]) -> bool {
    f.fields.iter().all(|eq| {
        sets.iter().any(|set| {
            set.iter()
                .any(|a| a.key == eq.key && a.value.match_text().as_deref() == Some(&eq.value))
        })
    })
}

fn log_matches(r: &StoredLog, tenant: TenantId, f: &TelemetryFilter) -> bool {
    source_ok(&r.source, tenant, f)
        && identity_agrees(&r.attributes, &r.source)
        && time_ok(r.time, f)
        && f.min_severity.is_none_or(|s| r.severity >= s)
        && fields_ok(f, &[&r.fields, &r.attributes])
        && log_extra_matches(r, f)
}

/// Whether a word reads the same in the JSON log line (no character that JSON escapes), so a
/// line filter on it never misses a record (card O-2).
pub fn line_safe(w: &str) -> bool {
    !w.is_empty() && !w.chars().any(|c| c == '"' || c == '\\' || c.is_control())
}

/// Keeps the `limit` oldest (or newest) of records sorted oldest first.
fn keep<T>(v: &mut Vec<T>, limit: usize, newest: bool) {
    if newest && v.len() > limit {
        v.drain(..v.len() - limit);
    } else {
        v.truncate(limit);
    }
}

fn metric_matches(r: &StoredMetric, tenant: TenantId, f: &TelemetryFilter) -> bool {
    source_ok(&r.source, tenant, f)
        && identity_agrees(&r.labels, &r.source)
        && time_ok(r.time, f)
        && fields_ok(f, &[&r.labels])
}

fn span_matches(r: &StoredSpan, tenant: TenantId, f: &TelemetryFilter) -> bool {
    source_ok(&r.source, tenant, f)
        && identity_agrees(&r.attributes, &r.source)
        && time_ok(r.start, f)
        && fields_ok(f, &[&r.attributes])
        && span_extra_matches(r, f)
}

#[cfg(test)]
mod o2_read_tests {
    //! Card O-2: the words and the trace ID of a log read become `|=` line filters whose
    //! values never leave their string literal.
    use super::*;

    /// The parts after the selector: each must be ` |= "<literal>"` (a Go string literal with
    /// `\` escapes); returns the literals' raw texts, or `None` when the tail is anything else.
    fn line_filters(tail: &str) -> Option<Vec<String>> {
        let mut out = Vec::new();
        let mut rest = tail;
        while !rest.is_empty() {
            rest = rest.strip_prefix(" |= \"")?;
            let mut lit = String::new();
            let mut chars = rest.char_indices();
            let end = loop {
                let (i, c) = chars.next()?;
                match c {
                    '\\' => {
                        let (_, n) = chars.next()?;
                        lit.push('\\');
                        lit.push(n);
                    }
                    '"' => break i,
                    c if c.is_control() => return None,
                    c => lit.push(c),
                }
            };
            out.push(lit);
            rest = &rest[end + 1..];
        }
        Some(out)
    }

    #[test]
    fn words_and_trace_ids_stay_inside_their_literals() {
        let app = NodeId::new();
        let head = format!("{{service_name=\"{app}\"}}");
        let hostile = [
            "\"",
            "\\",
            "`",
            "|=",
            "!~",
            "}",
            "x\" or \"1\"=\"1",
            "{tenant=\"b\"}",
            "{service_name=~\".+\"}",
            "a\nb",
            "| json | line_format \"{{.}}\"",
            ".*",
            "(?i)needle",
            "needle",
        ];
        for w in hostile {
            let f = TelemetryFilter {
                app: Some(app),
                words: vec![w.to_owned()],
                trace_id: Some("0af7651916cd43dd8448eb211c80319c".into()),
                ..Default::default()
            };
            let q = LgtmStore::logql(&f);
            let tail = q.strip_prefix(&head).unwrap_or_else(|| panic!("{q}"));
            let lits = line_filters(tail).unwrap_or_else(|| panic!("word {w:?}: {q}"));
            // A word that the JSON line holds as it is gets its literal; the trace ID too.
            let want = if line_safe(w) { 2 } else { 1 };
            assert_eq!(lits.len(), want, "word {w:?}: {q}");
            if line_safe(w) {
                assert_eq!(lits[0], w, "{q}");
            }
        }
        // A trace ID not of the OTel form never reaches the query.
        let f = TelemetryFilter {
            app: Some(app),
            trace_id: Some("\" |= \"x".into()),
            ..Default::default()
        };
        assert_eq!(LgtmStore::logql(&f), head);
    }
}
