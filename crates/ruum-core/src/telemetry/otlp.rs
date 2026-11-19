//! The OTLP mapping of the processor (card O-1b): OTLP logs, metrics and traces (the
//! `opentelemetry-proto` messages, protobuf) onto the record model of [`super`].
//!
//! # Rules
//!
//! - **Bounded before decoding.** [`decode_logs`], [`decode_metrics`] and [`decode_traces`]
//!   refuse a body larger than [`Limits::max_batch_bytes`] before prost reads it, and a decoded
//!   message with more records than [`Limits::max_records`] before it is mapped. The mapping
//!   itself stops at the byte cap: resource and scope attributes are copied onto every record,
//!   so a small request could otherwise grow without limit. Records past the cap are counted
//!   as [`DropReason::BatchTooLarge`], never mapped.
//! - **One path for attributes.** Resource attributes, then scope attributes, then the record's
//!   own attributes become the record's attributes (template fields of a log have no OTLP form;
//!   see below). Every attribute goes into a list that the pipeline classifies; no value is
//!   copied into a field that the pipeline does not classify. Attributes in the reserved
//!   namespace `ruum.*` are passed on as they came: the pipeline drops them and writes the
//!   identity attributes itself ([`super::classify::process`]).
//! - **No identity from a record.** Nothing here reads a tenant, app, deployment or environment.
//!   The caller gives the [`super::SourceIdentity`] of the authenticated source.
//! - The scope name and version, the schema URLs, exemplars, trace state, flags and the
//!   `event_name` of a log are not mapped (exemplars can carry attributes of their own; they
//!   are dropped, not mapped around the pipeline).
//!
//! # Mapping
//!
//! - Logs: the body becomes [`LogBody::Text`] (free text: `personal`, scrubbed with patterns).
//!   A non-string body is rendered as compact JSON text. OTLP has no message template, so no
//!   OTLP record becomes [`LogBody::Template`] (card O-5 may add a convention). The time is
//!   `time_unix_nano`, else `observed_time_unix_nano`, else now. The severity follows the OTel
//!   ranges of `severity_number`; unspecified is `info`.
//! - Metrics: a gauge point is [`MetricKind::Gauge`]; a sum point is [`MetricKind::Counter`];
//!   histogram, exponential histogram and summary points are [`MetricKind::Histogram`] with
//!   their `sum` as the value. Point attributes become labels.
//! - Traces: IDs become lowercase hex of their bytes (the pipeline drops a span whose ID has
//!   another length: [`DropReason::InvalidId`]); an empty parent is `None`.
//! - Values: string, int, double and bool as they are; bytes as lowercase hex; arrays and key
//!   value lists as compact JSON text (classified by their key like any other value).

use super::*;
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, InstrumentationScope, KeyValue, any_value};
use opentelemetry_proto::tonic::metrics::v1::{metric, number_data_point};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{span, status};
use prost::Message as _;

/// The bytes the mapping counts per value and per record, on top of the text (the same order
/// of magnitude as the pipeline's own overheads, so the cap bounds the mapped batch).
const VALUE_COST: usize = 16;
const RECORD_COST: usize = 64;

/// Why a whole request was refused before mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The body is larger than [`Limits::max_batch_bytes`].
    BodyTooLarge,
    /// The decoded message has more records than [`Limits::max_records`].
    TooManyRecords,
    /// The body is not a valid OTLP protobuf message.
    Malformed,
}

impl Refusal {
    /// A short reason for answers and logs (no content).
    pub fn as_str(self) -> &'static str {
        match self {
            Refusal::BodyTooLarge => "body_too_large",
            Refusal::TooManyRecords => "too_many_records",
            Refusal::Malformed => "malformed",
        }
    }

    /// The drop reason of the report.
    pub fn drop_reason(self) -> Option<DropReason> {
        match self {
            Refusal::BodyTooLarge => Some(DropReason::BatchTooLarge),
            Refusal::TooManyRecords => Some(DropReason::TooManyRecords),
            Refusal::Malformed => None,
        }
    }
}

/// A mapped request: the batch and the records the mapping dropped (counts only).
#[derive(Default)]
pub struct Mapped {
    pub batch: Batch,
    /// The records of the request, mapped or not.
    pub records: u64,
    pub dropped: BTreeMap<DropReason, u64>,
}

impl Mapped {
    fn drop(&mut self, reason: DropReason) {
        *self.dropped.entry(reason).or_default() += 1;
    }

    /// Adds the mapping's drops to a pipeline report.
    pub fn merge_into(&self, report: &mut IngestReport) {
        for (reason, n) in &self.dropped {
            *report.dropped.entry(*reason).or_default() += n;
        }
    }
}

fn check_body(body: &[u8], limits: &Limits) -> std::result::Result<(), Refusal> {
    if body.len() > limits.max_batch_bytes {
        Err(Refusal::BodyTooLarge)
    } else {
        Ok(())
    }
}

/// Decodes and maps an OTLP logs request (protobuf). See the module docs for the bounds.
pub fn decode_logs(body: &[u8], limits: &Limits) -> std::result::Result<Mapped, Refusal> {
    check_body(body, limits)?;
    let req = ExportLogsServiceRequest::decode(body).map_err(|_| Refusal::Malformed)?;
    map_logs(req, limits)
}

/// Decodes and maps an OTLP metrics request (protobuf).
pub fn decode_metrics(body: &[u8], limits: &Limits) -> std::result::Result<Mapped, Refusal> {
    check_body(body, limits)?;
    let req = ExportMetricsServiceRequest::decode(body).map_err(|_| Refusal::Malformed)?;
    map_metrics(req, limits)
}

/// Decodes and maps an OTLP traces request (protobuf).
pub fn decode_traces(body: &[u8], limits: &Limits) -> std::result::Result<Mapped, Refusal> {
    check_body(body, limits)?;
    let req = ExportTraceServiceRequest::decode(body).map_err(|_| Refusal::Malformed)?;
    map_traces(req, limits)
}

/// The number of records of a logs request.
pub fn count_logs(req: &ExportLogsServiceRequest) -> u64 {
    req.resource_logs
        .iter()
        .flat_map(|r| &r.scope_logs)
        .map(|s| s.log_records.len() as u64)
        .sum()
}

/// The number of points of a metrics request.
pub fn count_metrics(req: &ExportMetricsServiceRequest) -> u64 {
    req.resource_metrics
        .iter()
        .flat_map(|r| &r.scope_metrics)
        .flat_map(|s| &s.metrics)
        .map(|m| match &m.data {
            Some(metric::Data::Gauge(g)) => g.data_points.len() as u64,
            Some(metric::Data::Sum(s)) => s.data_points.len() as u64,
            Some(metric::Data::Histogram(h)) => h.data_points.len() as u64,
            Some(metric::Data::ExponentialHistogram(h)) => h.data_points.len() as u64,
            Some(metric::Data::Summary(s)) => s.data_points.len() as u64,
            None => 0,
        })
        .sum()
}

/// The number of spans of a traces request.
pub fn count_traces(req: &ExportTraceServiceRequest) -> u64 {
    req.resource_spans
        .iter()
        .flat_map(|r| &r.scope_spans)
        .map(|s| s.spans.len() as u64)
        .sum()
}

/// The byte budget of one mapping.
struct Budget {
    left: usize,
    exhausted: bool,
}

impl Budget {
    fn new(limits: &Limits) -> Self {
        Budget {
            left: limits.max_batch_bytes,
            exhausted: false,
        }
    }

    /// Takes `cost` bytes; once a record does not fit, every later record is refused too, so
    /// the drop is a clean tail.
    fn take(&mut self, cost: usize) -> bool {
        if self.exhausted || cost > self.left {
            self.exhausted = true;
            return false;
        }
        self.left -= cost;
        true
    }
}

fn kv_cost(kvs: &[KeyValue]) -> usize {
    kvs.iter()
        .map(|kv| kv.key.len() + kv.value.as_ref().map_or(0, value_cost) + VALUE_COST)
        .sum()
}

fn value_cost(v: &AnyValue) -> usize {
    match &v.value {
        Some(any_value::Value::StringValue(s)) => s.len(),
        Some(any_value::Value::BytesValue(b)) => b.len() * 2,
        Some(any_value::Value::ArrayValue(a)) => {
            a.values.iter().map(value_cost).sum::<usize>() + a.values.len() * 2 + 2
        }
        Some(any_value::Value::KvlistValue(l)) => kv_cost(&l.values) + 2,
        _ => 8,
    }
}

/// The attributes that every record of one resource and scope carries.
fn shared_attrs(
    resource: Option<&Resource>,
    scope: Option<&InstrumentationScope>,
) -> Vec<KeyValue> {
    let mut out = Vec::new();
    if let Some(r) = resource {
        out.extend(r.attributes.iter().cloned());
    }
    if let Some(s) = scope {
        out.extend(s.attributes.iter().cloned());
    }
    out
}

fn attributes(shared: &[KeyValue], own: Vec<KeyValue>) -> Vec<Attribute> {
    shared
        .iter()
        .cloned()
        .chain(own)
        .filter_map(|kv| {
            let value = attr_value(kv.value?)?;
            Some(Attribute { key: kv.key, value })
        })
        .collect()
}

/// An OTLP value as an attribute value. `None` for a value without content.
pub fn attr_value(v: AnyValue) -> Option<AttrValue> {
    Some(match v.value? {
        any_value::Value::StringValue(s) => AttrValue::Str(s),
        any_value::Value::BoolValue(b) => AttrValue::Bool(b),
        any_value::Value::IntValue(i) => AttrValue::Int(i),
        any_value::Value::DoubleValue(f) => AttrValue::Float(f),
        any_value::Value::BytesValue(b) => AttrValue::Str(hex::encode(b)),
        other @ (any_value::Value::ArrayValue(_) | any_value::Value::KvlistValue(_)) => {
            AttrValue::Str(render(&AnyValue { value: Some(other) }).to_string())
        }
        // A dictionary index of the profiles signal: no dictionary here, so no content.
        any_value::Value::StringValueStrindex(_) => return None,
    })
}

/// A value as JSON (for arrays and key value lists, and for a non-string log body).
fn render(v: &AnyValue) -> serde_json::Value {
    use serde_json::Value as J;
    match &v.value {
        Some(any_value::Value::StringValue(s)) => J::String(s.clone()),
        Some(any_value::Value::BoolValue(b)) => J::Bool(*b),
        Some(any_value::Value::IntValue(i)) => J::from(*i),
        Some(any_value::Value::DoubleValue(f)) => {
            serde_json::Number::from_f64(*f).map_or_else(|| J::String(f.to_string()), J::Number)
        }
        Some(any_value::Value::BytesValue(b)) => J::String(hex::encode(b)),
        Some(any_value::Value::ArrayValue(a)) => J::Array(a.values.iter().map(render).collect()),
        Some(any_value::Value::KvlistValue(l)) => J::Object(
            l.values
                .iter()
                .map(|kv| (kv.key.clone(), kv.value.as_ref().map_or(J::Null, render)))
                .collect(),
        ),
        Some(any_value::Value::StringValueStrindex(_)) | None => J::Null,
    }
}

/// A time in nanoseconds since the epoch; 0 is "not set".
pub fn time_of(nanos: u64) -> Option<OffsetDateTime> {
    if nanos == 0 {
        return None;
    }
    OffsetDateTime::from_unix_timestamp_nanos(nanos as i128).ok()
}

/// The severity of an OTel `severity_number`.
pub fn severity_of(number: i32) -> Severity {
    match number {
        1..=4 => Severity::Trace,
        5..=8 => Severity::Debug,
        13..=16 => Severity::Warn,
        17..=20 => Severity::Error,
        21..=24 => Severity::Fatal,
        _ => Severity::Info,
    }
}

/// Lowercase hex of an ID; `None` for an empty one.
pub fn hex_id(bytes: &[u8]) -> Option<String> {
    (!bytes.is_empty()).then(|| hex::encode(bytes))
}

/// Maps a logs request.
pub fn map_logs(
    req: ExportLogsServiceRequest,
    limits: &Limits,
) -> std::result::Result<Mapped, Refusal> {
    let records = count_logs(&req);
    if records > limits.max_records as u64 {
        return Err(Refusal::TooManyRecords);
    }
    let mut out = Mapped {
        records,
        ..Default::default()
    };
    let mut budget = Budget::new(limits);
    let now = crate::now();
    for rl in req.resource_logs {
        for sl in rl.scope_logs {
            let shared = shared_attrs(rl.resource.as_ref(), sl.scope.as_ref());
            let shared_cost = kv_cost(&shared);
            for rec in sl.log_records {
                let body_cost = rec.body.as_ref().map_or(0, value_cost);
                let cost = RECORD_COST + shared_cost + kv_cost(&rec.attributes) + body_cost;
                if !budget.take(cost) {
                    out.drop(DropReason::BatchTooLarge);
                    continue;
                }
                let text = match rec.body {
                    Some(AnyValue {
                        value: Some(any_value::Value::StringValue(s)),
                    }) => s,
                    Some(v) => match render(&v) {
                        serde_json::Value::Null => String::new(),
                        j => j.to_string(),
                    },
                    None => String::new(),
                };
                out.batch.logs.push(LogRecord {
                    time: time_of(rec.time_unix_nano)
                        .or_else(|| time_of(rec.observed_time_unix_nano))
                        .unwrap_or(now),
                    severity: severity_of(rec.severity_number),
                    body: LogBody::Text(text),
                    attributes: attributes(&shared, rec.attributes),
                });
            }
        }
    }
    Ok(out)
}

/// Maps a metrics request.
pub fn map_metrics(
    req: ExportMetricsServiceRequest,
    limits: &Limits,
) -> std::result::Result<Mapped, Refusal> {
    let records = count_metrics(&req);
    if records > limits.max_records as u64 {
        return Err(Refusal::TooManyRecords);
    }
    let mut out = Mapped {
        records,
        ..Default::default()
    };
    let mut budget = Budget::new(limits);
    let now = crate::now();
    for rm in req.resource_metrics {
        for sm in rm.scope_metrics {
            let shared = shared_attrs(rm.resource.as_ref(), sm.scope.as_ref());
            let shared_cost = kv_cost(&shared);
            for m in sm.metrics {
                // (attributes, time, kind, value) per point.
                let points: Vec<(Vec<KeyValue>, u64, MetricKind, f64)> = match m.data {
                    Some(metric::Data::Gauge(g)) => g
                        .data_points
                        .into_iter()
                        .map(|p| {
                            let v = number(p.value);
                            (p.attributes, p.time_unix_nano, MetricKind::Gauge, v)
                        })
                        .collect(),
                    Some(metric::Data::Sum(s)) => s
                        .data_points
                        .into_iter()
                        .map(|p| {
                            let v = number(p.value);
                            (p.attributes, p.time_unix_nano, MetricKind::Counter, v)
                        })
                        .collect(),
                    Some(metric::Data::Histogram(h)) => h
                        .data_points
                        .into_iter()
                        .map(|p| {
                            let v = p.sum.unwrap_or(0.0);
                            (p.attributes, p.time_unix_nano, MetricKind::Histogram, v)
                        })
                        .collect(),
                    Some(metric::Data::ExponentialHistogram(h)) => h
                        .data_points
                        .into_iter()
                        .map(|p| {
                            let v = p.sum.unwrap_or(0.0);
                            (p.attributes, p.time_unix_nano, MetricKind::Histogram, v)
                        })
                        .collect(),
                    Some(metric::Data::Summary(s)) => s
                        .data_points
                        .into_iter()
                        .map(|p| (p.attributes, p.time_unix_nano, MetricKind::Histogram, p.sum))
                        .collect(),
                    None => Vec::new(),
                };
                for (attrs, t, kind, value) in points {
                    let cost = RECORD_COST + m.name.len() + shared_cost + kv_cost(&attrs);
                    if !budget.take(cost) {
                        out.drop(DropReason::BatchTooLarge);
                        continue;
                    }
                    out.batch.metrics.push(MetricPoint {
                        time: time_of(t).unwrap_or(now),
                        name: m.name.clone(),
                        kind,
                        value,
                        labels: attributes(&shared, attrs),
                    });
                }
            }
        }
    }
    Ok(out)
}

fn number(v: Option<number_data_point::Value>) -> f64 {
    match v {
        Some(number_data_point::Value::AsDouble(f)) => f,
        Some(number_data_point::Value::AsInt(i)) => i as f64,
        None => 0.0,
    }
}

fn span_kind(kind: i32) -> SpanKind {
    match span::SpanKind::try_from(kind) {
        Ok(span::SpanKind::Server) => SpanKind::Server,
        Ok(span::SpanKind::Client) => SpanKind::Client,
        Ok(span::SpanKind::Producer) => SpanKind::Producer,
        Ok(span::SpanKind::Consumer) => SpanKind::Consumer,
        _ => SpanKind::Internal,
    }
}

fn status_code(code: i32) -> StatusCode {
    match status::StatusCode::try_from(code) {
        Ok(status::StatusCode::Ok) => StatusCode::Ok,
        Ok(status::StatusCode::Error) => StatusCode::Error,
        _ => StatusCode::Unset,
    }
}

/// Maps a traces request.
pub fn map_traces(
    req: ExportTraceServiceRequest,
    limits: &Limits,
) -> std::result::Result<Mapped, Refusal> {
    let records = count_traces(&req);
    if records > limits.max_records as u64 {
        return Err(Refusal::TooManyRecords);
    }
    let mut out = Mapped {
        records,
        ..Default::default()
    };
    let mut budget = Budget::new(limits);
    let now = crate::now();
    for rs in req.resource_spans {
        for ss in rs.scope_spans {
            let shared = shared_attrs(rs.resource.as_ref(), ss.scope.as_ref());
            let shared_cost = kv_cost(&shared);
            for s in ss.spans {
                let events_cost: usize = s
                    .events
                    .iter()
                    .map(|e| VALUE_COST + e.name.len() + kv_cost(&e.attributes))
                    .sum();
                let links_cost: usize = s
                    .links
                    .iter()
                    .map(|l| VALUE_COST + 48 + kv_cost(&l.attributes))
                    .sum();
                let status_cost = s.status.as_ref().map_or(0, |st| st.message.len());
                let cost = RECORD_COST
                    + shared_cost
                    + s.name.len()
                    + kv_cost(&s.attributes)
                    + events_cost
                    + links_cost
                    + status_cost;
                if !budget.take(cost) {
                    out.drop(DropReason::BatchTooLarge);
                    continue;
                }
                let start = time_of(s.start_time_unix_nano).unwrap_or(now);
                let end = time_of(s.end_time_unix_nano).unwrap_or(start);
                let (status, status_message) = match s.status {
                    Some(st) => (
                        status_code(st.code),
                        (!st.message.is_empty()).then_some(st.message),
                    ),
                    None => (StatusCode::Unset, None),
                };
                out.batch.spans.push(Span {
                    trace_id: hex::encode(&s.trace_id),
                    span_id: hex::encode(&s.span_id),
                    parent_span_id: hex_id(&s.parent_span_id),
                    name: s.name,
                    kind: span_kind(s.kind),
                    start,
                    end,
                    status,
                    status_message,
                    attributes: attributes(&shared, s.attributes),
                    events: s
                        .events
                        .into_iter()
                        .map(|e| SpanEvent {
                            time: time_of(e.time_unix_nano).unwrap_or(start),
                            name: e.name,
                            attributes: attributes(&[], e.attributes),
                        })
                        .collect(),
                    links: s
                        .links
                        .into_iter()
                        .map(|l| SpanLink {
                            trace_id: hex::encode(&l.trace_id),
                            span_id: hex::encode(&l.span_id),
                            attributes: attributes(&[], l.attributes),
                        })
                        .collect(),
                });
            }
        }
    }
    Ok(out)
}
