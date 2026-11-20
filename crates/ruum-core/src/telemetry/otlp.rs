//! The OTLP mapping of the processor (card O-1b): OTLP logs, metrics and traces (the
//! `opentelemetry-proto` messages, protobuf) onto the record model of [`super`].
//!
//! # Rules
//!
//! - **Bounded before decoding.** [`check`] (and so [`decode_logs`], [`decode_metrics`] and
//!   [`decode_traces`]) refuses a body larger than [`Limits::max_batch_bytes`], then walks the
//!   protobuf wire format (tags and lengths only, nothing allocated) before prost decodes it.
//!   The walk counts the records (log records, metric points, spans) and estimates the heap
//!   that prost will allocate: the size of the Rust struct of every message entry, plus the
//!   bytes of strings and repeated numbers. An empty protobuf entry costs 2 bytes on the wire
//!   but a whole struct in memory (a `LogRecord` is about 180 bytes), and an empty parent entry
//!   counts no record, so the wire size alone does not bound the decode. More records than
//!   [`Limits::max_records`] is [`Refusal::TooManyRecords`]; an estimate over
//!   [`max_decoded_bytes`] is [`Refusal::BodyTooLarge`]; both before prost runs. The caller
//!   can hold the estimate in its budget ([`Shape::decoded_bytes`]) before it calls
//!   [`Checked::decode`]. The mapping itself stops at the byte cap: resource and scope
//!   attributes are copied onto every record, so a small request could otherwise grow without
//!   limit. Records past the cap are counted as [`DropReason::BatchTooLarge`], never mapped.
//!   The walk refuses protobuf groups (wire types 3 and 4; OTLP has none) as malformed.
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
    /// The caller's budget did not hold the bytes of the mapped batch
    /// ([`Checked::decode_held`]).
    NotHeld,
}

impl Refusal {
    /// A short reason for answers and logs (no content).
    pub fn as_str(self) -> &'static str {
        match self {
            Refusal::BodyTooLarge => "body_too_large",
            Refusal::TooManyRecords => "too_many_records",
            Refusal::Malformed => "malformed",
            Refusal::NotHeld => "not_held",
        }
    }

    /// The drop reason of the report.
    pub fn drop_reason(self) -> Option<DropReason> {
        match self {
            Refusal::BodyTooLarge => Some(DropReason::BatchTooLarge),
            Refusal::TooManyRecords => Some(DropReason::TooManyRecords),
            Refusal::Malformed | Refusal::NotHeld => None,
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
    /// The mapping's count of the bytes the batch holds (at most [`Limits::max_batch_bytes`];
    /// strings, rendered JSON text and the fixed costs per value and record). The receiver
    /// holds it in its budget while the batch is in flight.
    pub bytes: u64,
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

/// The decoded message may hold at most this many times [`Limits::max_batch_bytes`] (the
/// estimate of [`check`]). Real OTLP batches decode to about 2 to 4 times their wire size;
/// short attributes (a one-letter key and a small number) to about 7 times, which only a batch
/// near the wire cap made of nothing else reaches.
pub const DECODED_FACTOR: usize = 6;

/// The cap of the decoded estimate: [`DECODED_FACTOR`] times [`Limits::max_batch_bytes`].
pub fn max_decoded_bytes(limits: &Limits) -> u64 {
    (limits.max_batch_bytes as u64).saturating_mul(DECODED_FACTOR as u64)
}

/// What the wire walk of [`check`] found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shape {
    /// The records of the request (log records, metric points or spans).
    pub records: u64,
    /// The estimate of the heap that decoding allocates.
    pub decoded_bytes: u64,
}

/// A body that passed [`check`]: only this can be decoded.
pub struct Checked<'a> {
    signal: Signal,
    body: &'a [u8],
    shape: Shape,
}

impl Checked<'_> {
    pub fn shape(&self) -> Shape {
        self.shape
    }

    /// Decodes and maps the checked body.
    pub fn decode(self, limits: &Limits) -> std::result::Result<Mapped, Refusal> {
        self.decode_held(limits, &mut |_| true)
    }

    /// Decodes and maps the checked body; `hold` is asked to hold the bytes of the mapped
    /// batch before the mapping allocates them (in steps of [`HOLD_STEP`], at most
    /// [`Limits::max_batch_bytes`] in all). When it refuses, the request is refused
    /// ([`Refusal::NotHeld`]). Kimi run 13 finding 1: the receiver holds the decoded estimate,
    /// and with this also the mapped batch.
    pub fn decode_held(
        self,
        limits: &Limits,
        hold: &mut dyn FnMut(u64) -> bool,
    ) -> std::result::Result<Mapped, Refusal> {
        match self.signal {
            Signal::Logs => {
                let req =
                    ExportLogsServiceRequest::decode(self.body).map_err(|_| Refusal::Malformed)?;
                map_logs_held(req, limits, hold)
            }
            Signal::Metrics => {
                let req = ExportMetricsServiceRequest::decode(self.body)
                    .map_err(|_| Refusal::Malformed)?;
                map_metrics_held(req, limits, hold)
            }
            Signal::Traces => {
                let req =
                    ExportTraceServiceRequest::decode(self.body).map_err(|_| Refusal::Malformed)?;
                map_traces_held(req, limits, hold)
            }
        }
    }
}

/// The step in which [`Checked::decode_held`] asks for bytes to hold.
pub const HOLD_STEP: u64 = 64 * 1024;

/// The bounds before decoding (see the module docs): the body size, then the wire walk with
/// the record count and the decoded estimate.
pub fn check<'a>(
    signal: Signal,
    body: &'a [u8],
    limits: &Limits,
) -> std::result::Result<Checked<'a>, Refusal> {
    if body.len() > limits.max_batch_bytes {
        return Err(Refusal::BodyTooLarge);
    }
    let shape = prescan(signal, body, limits)?;
    Ok(Checked {
        signal,
        body,
        shape,
    })
}

/// Decodes and maps an OTLP logs request (protobuf). See the module docs for the bounds.
pub fn decode_logs(body: &[u8], limits: &Limits) -> std::result::Result<Mapped, Refusal> {
    check(Signal::Logs, body, limits)?.decode(limits)
}

/// Decodes and maps an OTLP metrics request (protobuf).
pub fn decode_metrics(body: &[u8], limits: &Limits) -> std::result::Result<Mapped, Refusal> {
    check(Signal::Metrics, body, limits)?.decode(limits)
}

/// Decodes and maps an OTLP traces request (protobuf).
pub fn decode_traces(body: &[u8], limits: &Limits) -> std::result::Result<Mapped, Refusal> {
    check(Signal::Traces, body, limits)?.decode(limits)
}

// ---------------------------------------------------------------------------- wire walk

/// The nesting the walk follows (prost's own recursion limit is 100).
const MAX_DEPTH: u32 = 100;

/// The OTLP messages of the three export requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum M {
    LogsReq,
    ResourceLogs,
    ScopeLogs,
    LogRecord,
    MetricsReq,
    ResourceMetrics,
    ScopeMetrics,
    Metric,
    Gauge,
    Sum,
    Histogram,
    ExpHistogram,
    Summary,
    NumberPoint,
    HistogramPoint,
    ExpHistogramPoint,
    Buckets,
    SummaryPoint,
    Quantile,
    Exemplar,
    TracesReq,
    ResourceSpans,
    ScopeSpans,
    Span,
    Event,
    Link,
    Status,
    Resource,
    Scope,
    EntityRef,
    KeyValue,
    AnyValue,
    ArrayValue,
    KvList,
}

/// What a field of a message is, for the walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum F {
    /// A message (one entry of a repeated field, or an optional one).
    Msg(M),
    /// A string or bytes field: its bytes.
    Bytes,
    /// A repeated string: its bytes and a `String` per element.
    RepBytes,
    /// A repeated `fixed64` or `double`: 8 bytes per element, packed or not.
    RepFixed,
    /// A repeated varint: 8 bytes per element, packed or not.
    RepVarint,
    /// A scalar held inside the struct, or a field prost does not keep.
    Inline,
}

impl M {
    /// The size of the Rust struct prost allocates for one entry.
    fn size(self) -> usize {
        use opentelemetry_proto::tonic::{common::v1 as c, logs::v1 as l, metrics::v1 as m};
        use opentelemetry_proto::tonic::{resource::v1 as r, trace::v1 as t};
        use std::mem::size_of;
        match self {
            M::LogsReq => size_of::<ExportLogsServiceRequest>(),
            M::ResourceLogs => size_of::<l::ResourceLogs>(),
            M::ScopeLogs => size_of::<l::ScopeLogs>(),
            M::LogRecord => size_of::<l::LogRecord>(),
            M::MetricsReq => size_of::<ExportMetricsServiceRequest>(),
            M::ResourceMetrics => size_of::<m::ResourceMetrics>(),
            M::ScopeMetrics => size_of::<m::ScopeMetrics>(),
            M::Metric => size_of::<m::Metric>(),
            M::Gauge => size_of::<m::Gauge>(),
            M::Sum => size_of::<m::Sum>(),
            M::Histogram => size_of::<m::Histogram>(),
            M::ExpHistogram => size_of::<m::ExponentialHistogram>(),
            M::Summary => size_of::<m::Summary>(),
            M::NumberPoint => size_of::<m::NumberDataPoint>(),
            M::HistogramPoint => size_of::<m::HistogramDataPoint>(),
            M::ExpHistogramPoint => size_of::<m::ExponentialHistogramDataPoint>(),
            M::Buckets => size_of::<m::exponential_histogram_data_point::Buckets>(),
            M::SummaryPoint => size_of::<m::SummaryDataPoint>(),
            M::Quantile => size_of::<m::summary_data_point::ValueAtQuantile>(),
            M::Exemplar => size_of::<m::Exemplar>(),
            M::TracesReq => size_of::<ExportTraceServiceRequest>(),
            M::ResourceSpans => size_of::<t::ResourceSpans>(),
            M::ScopeSpans => size_of::<t::ScopeSpans>(),
            M::Span => size_of::<t::Span>(),
            M::Event => size_of::<t::span::Event>(),
            M::Link => size_of::<t::span::Link>(),
            M::Status => size_of::<t::Status>(),
            M::Resource => size_of::<r::Resource>(),
            M::Scope => size_of::<c::InstrumentationScope>(),
            M::EntityRef => size_of::<c::EntityRef>(),
            M::KeyValue => size_of::<c::KeyValue>(),
            M::AnyValue => size_of::<c::AnyValue>(),
            M::ArrayValue => size_of::<c::ArrayValue>(),
            M::KvList => size_of::<c::KeyValueList>(),
        }
    }

    /// Whether one entry is one record of [`Limits::max_records`].
    fn is_record(self) -> bool {
        matches!(
            self,
            M::LogRecord
                | M::NumberPoint
                | M::HistogramPoint
                | M::ExpHistogramPoint
                | M::SummaryPoint
                | M::Span
        )
    }

    /// The fields of the message, by tag (opentelemetry-proto 0.33). Every field that holds a
    /// message is listed, so the walk sees every entry prost allocates.
    fn field(self, tag: u64) -> F {
        use F::*;
        match (self, tag) {
            (M::LogsReq, 1) => Msg(M::ResourceLogs),
            (M::ResourceLogs, 1) => Msg(M::Resource),
            (M::ResourceLogs, 2) => Msg(M::ScopeLogs),
            (M::ScopeLogs, 1) => Msg(M::Scope),
            (M::ScopeLogs, 2) => Msg(M::LogRecord),
            (M::LogRecord, 5) => Msg(M::AnyValue),
            (M::LogRecord, 6) => Msg(M::KeyValue),
            (M::LogRecord, 3 | 9 | 10 | 12) => Bytes,

            (M::MetricsReq, 1) => Msg(M::ResourceMetrics),
            (M::ResourceMetrics, 1) => Msg(M::Resource),
            (M::ResourceMetrics, 2) => Msg(M::ScopeMetrics),
            (M::ScopeMetrics, 1) => Msg(M::Scope),
            (M::ScopeMetrics, 2) => Msg(M::Metric),
            (M::Metric, 1..=3) => Bytes,
            (M::Metric, 5) => Msg(M::Gauge),
            (M::Metric, 7) => Msg(M::Sum),
            (M::Metric, 9) => Msg(M::Histogram),
            (M::Metric, 10) => Msg(M::ExpHistogram),
            (M::Metric, 11) => Msg(M::Summary),
            (M::Metric, 12) => Msg(M::KeyValue),
            (M::Gauge | M::Sum, 1) => Msg(M::NumberPoint),
            (M::Histogram, 1) => Msg(M::HistogramPoint),
            (M::ExpHistogram, 1) => Msg(M::ExpHistogramPoint),
            (M::Summary, 1) => Msg(M::SummaryPoint),
            (M::NumberPoint, 7) => Msg(M::KeyValue),
            (M::NumberPoint, 5) => Msg(M::Exemplar),
            (M::HistogramPoint, 9) => Msg(M::KeyValue),
            (M::HistogramPoint, 6 | 7) => RepFixed,
            (M::HistogramPoint, 8) => Msg(M::Exemplar),
            (M::ExpHistogramPoint, 1) => Msg(M::KeyValue),
            (M::ExpHistogramPoint, 8 | 9) => Msg(M::Buckets),
            (M::ExpHistogramPoint, 11) => Msg(M::Exemplar),
            (M::Buckets, 2) => RepVarint,
            (M::SummaryPoint, 7) => Msg(M::KeyValue),
            (M::SummaryPoint, 6) => Msg(M::Quantile),
            (M::Exemplar, 7) => Msg(M::KeyValue),
            (M::Exemplar, 4 | 5) => Bytes,

            (M::TracesReq, 1) => Msg(M::ResourceSpans),
            (M::ResourceSpans, 1) => Msg(M::Resource),
            (M::ResourceSpans, 2) => Msg(M::ScopeSpans),
            (M::ScopeSpans, 1) => Msg(M::Scope),
            (M::ScopeSpans, 2) => Msg(M::Span),
            (M::Span, 1..=5) => Bytes,
            (M::Span, 9) => Msg(M::KeyValue),
            (M::Span, 11) => Msg(M::Event),
            (M::Span, 13) => Msg(M::Link),
            (M::Span, 15) => Msg(M::Status),
            (M::Event, 2) => Bytes,
            (M::Event, 3) => Msg(M::KeyValue),
            (M::Link, 1..=3) => Bytes,
            (M::Link, 4) => Msg(M::KeyValue),
            (M::Status, 2) => Bytes,

            (M::ResourceLogs | M::ScopeLogs, 3)
            | (M::ResourceMetrics | M::ScopeMetrics, 3)
            | (M::ResourceSpans | M::ScopeSpans, 3) => Bytes,
            (M::Resource, 1) => Msg(M::KeyValue),
            (M::Resource, 3) => Msg(M::EntityRef),
            (M::Scope, 1 | 2) => Bytes,
            (M::Scope, 3) => Msg(M::KeyValue),
            (M::EntityRef, 1 | 2) => Bytes,
            (M::EntityRef, 3 | 4) => RepBytes,
            (M::KeyValue, 1) => Bytes,
            (M::KeyValue, 2) => Msg(M::AnyValue),
            (M::AnyValue, 1 | 7) => Bytes,
            (M::AnyValue, 5) => Msg(M::ArrayValue),
            (M::AnyValue, 6) => Msg(M::KvList),
            (M::ArrayValue, 1) => Msg(M::AnyValue),
            (M::KvList, 1) => Msg(M::KeyValue),
            _ => Inline,
        }
    }
}

/// The running totals of one walk, with the caps.
struct Walk {
    shape: Shape,
    max_records: u64,
    max_bytes: u64,
}

impl Walk {
    fn add(&mut self, bytes: usize) -> std::result::Result<(), Refusal> {
        self.shape.decoded_bytes = self.shape.decoded_bytes.saturating_add(bytes as u64);
        if self.shape.decoded_bytes > self.max_bytes {
            return Err(Refusal::BodyTooLarge);
        }
        Ok(())
    }

    fn entry(&mut self, m: M) -> std::result::Result<(), Refusal> {
        if m.is_record() {
            self.shape.records += 1;
            if self.shape.records > self.max_records {
                return Err(Refusal::TooManyRecords);
            }
        }
        self.add(m.size())
    }

    fn message(&mut self, m: M, buf: &[u8], depth: u32) -> std::result::Result<(), Refusal> {
        if depth > MAX_DEPTH {
            return Err(Refusal::Malformed);
        }
        let mut i = 0;
        while i < buf.len() {
            let key = varint(buf, &mut i)?;
            let tag = key >> 3;
            if tag == 0 {
                return Err(Refusal::Malformed);
            }
            let field = m.field(tag);
            match key & 7 {
                0 => {
                    varint(buf, &mut i)?;
                    if matches!(field, F::RepFixed | F::RepVarint) {
                        self.add(8)?;
                    }
                }
                1 => {
                    skip(buf, &mut i, 8)?;
                    if matches!(field, F::RepFixed | F::RepVarint) {
                        self.add(8)?;
                    }
                }
                5 => skip(buf, &mut i, 4)?,
                2 => {
                    let len =
                        usize::try_from(varint(buf, &mut i)?).map_err(|_| Refusal::Malformed)?;
                    let start = i;
                    skip(buf, &mut i, len)?;
                    let sub = &buf[start..i];
                    match field {
                        F::Msg(child) => {
                            self.entry(child)?;
                            self.message(child, sub, depth + 1)?;
                        }
                        F::Bytes | F::RepFixed => self.add(len)?,
                        F::RepBytes => self.add(len + std::mem::size_of::<String>())?,
                        // Packed varints: one element per byte without the high bit.
                        F::RepVarint => self.add(8 * sub.iter().filter(|b| **b < 0x80).count())?,
                        F::Inline => {}
                    }
                }
                // Groups (3, 4) and the unused wire types: OTLP has none.
                _ => return Err(Refusal::Malformed),
            }
        }
        Ok(())
    }
}

fn varint(buf: &[u8], i: &mut usize) -> std::result::Result<u64, Refusal> {
    let mut v: u64 = 0;
    for shift in (0..64).step_by(7) {
        let b = *buf.get(*i).ok_or(Refusal::Malformed)?;
        *i += 1;
        v |= u64::from(b & 0x7f) << shift;
        if b < 0x80 {
            return Ok(v);
        }
    }
    Err(Refusal::Malformed)
}

fn skip(buf: &[u8], i: &mut usize, n: usize) -> std::result::Result<(), Refusal> {
    match i.checked_add(n) {
        Some(end) if end <= buf.len() => {
            *i = end;
            Ok(())
        }
        _ => Err(Refusal::Malformed),
    }
}

/// The wire walk of [`check`]: the records and the decoded estimate of a body, without
/// allocating. Refuses as soon as a cap is passed, so its time is linear in the body.
pub fn prescan(
    signal: Signal,
    body: &[u8],
    limits: &Limits,
) -> std::result::Result<Shape, Refusal> {
    let root = match signal {
        Signal::Logs => M::LogsReq,
        Signal::Metrics => M::MetricsReq,
        Signal::Traces => M::TracesReq,
    };
    let mut walk = Walk {
        shape: Shape::default(),
        max_records: limits.max_records as u64,
        max_bytes: max_decoded_bytes(limits),
    };
    walk.add(root.size())?;
    walk.message(root, body, 0)?;
    Ok(walk.shape)
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

/// The byte budget of one mapping, and the bytes the caller holds for it.
struct Budget<'h> {
    cap: usize,
    left: usize,
    exhausted: bool,
    /// The bytes `hold` holds so far.
    held: u64,
    hold: &'h mut dyn FnMut(u64) -> bool,
    /// `hold` refused: the request is refused.
    not_held: bool,
}

impl<'h> Budget<'h> {
    fn new(limits: &Limits, hold: &'h mut dyn FnMut(u64) -> bool) -> Self {
        Budget {
            cap: limits.max_batch_bytes,
            left: limits.max_batch_bytes,
            exhausted: false,
            held: 0,
            hold,
            not_held: false,
        }
    }

    /// The bytes taken.
    fn used(&self) -> u64 {
        (self.cap - self.left) as u64
    }

    /// Takes `cost` bytes; once a record does not fit, every later record is refused too, so
    /// the drop is a clean tail. Before the bytes are taken, the caller holds them.
    fn take(&mut self, cost: usize) -> bool {
        if self.exhausted || self.not_held || cost > self.left {
            self.exhausted = true;
            return false;
        }
        let need = self.used() + cost as u64;
        if need > self.held {
            let step = (need - self.held)
                .max(HOLD_STEP)
                .min(self.cap as u64 - self.held);
            if !(self.hold)(step) {
                self.not_held = true;
                return false;
            }
            self.held += step;
        }
        self.left -= cost;
        true
    }

    /// The refusal of the request when `hold` refused.
    fn check(&self) -> std::result::Result<(), Refusal> {
        if self.not_held {
            Err(Refusal::NotHeld)
        } else {
            Ok(())
        }
    }
}

fn kv_cost(kvs: &[KeyValue]) -> usize {
    kvs.iter()
        .map(|kv| kv.key.len() + kv.value.as_ref().map_or(0, value_cost) + VALUE_COST)
        .sum()
}

/// The bytes of a value as the mapping keeps it: a string as it is, bytes as hex, an array or
/// a key value list as its JSON text ([`json_cost`]), another scalar as a fixed cost.
fn value_cost(v: &AnyValue) -> usize {
    match &v.value {
        Some(any_value::Value::StringValue(s)) => s.len(),
        Some(any_value::Value::BytesValue(b)) => b.len() * 2,
        Some(any_value::Value::ArrayValue(_) | any_value::Value::KvlistValue(_)) => json_cost(v),
        _ => 8,
    }
}

/// The bytes of a log body: a string body as it is, any other body as its JSON text.
fn body_cost(v: &AnyValue) -> usize {
    match &v.value {
        Some(any_value::Value::StringValue(s)) => s.len(),
        _ => json_cost(v),
    }
}

/// The longest JSON text of an `i64` (`-9223372036854775808`) and of an `f64` (ryu's longest
/// form, such as `-2.2250738585072014e-308`; NaN and the infinities are the strings `"NaN"`,
/// `"inf"` and `"-inf"`).
const JSON_INT: usize = 20;
const JSON_FLOAT: usize = 24;

/// An upper bound of the bytes of a JSON string literal of `s`: the quotes, and every byte
/// that JSON escapes counted at its longest escape (`\u001f`). Kimi run 13 finding 1: the
/// raw length undercounts a text of control characters six times.
fn json_str_cost(s: &str) -> usize {
    2 + s
        .bytes()
        .map(|b| match b {
            b'"' | b'\\' => 2,
            0x00..=0x1f => 6,
            _ => 1,
        })
        .sum::<usize>()
}

/// An upper bound of the bytes of the compact JSON text of `v` ([`render_json`]).
pub(crate) fn json_cost(v: &AnyValue) -> usize {
    match &v.value {
        Some(any_value::Value::StringValue(s)) => json_str_cost(s),
        Some(any_value::Value::BoolValue(_)) => 5,
        Some(any_value::Value::IntValue(_)) => JSON_INT,
        Some(any_value::Value::DoubleValue(_)) => JSON_FLOAT,
        Some(any_value::Value::BytesValue(b)) => 2 + b.len() * 2,
        Some(any_value::Value::ArrayValue(a)) => {
            2 + a.values.len() + a.values.iter().map(json_cost).sum::<usize>()
        }
        Some(any_value::Value::KvlistValue(l)) => {
            2 + l
                .values
                .iter()
                .map(|kv| json_str_cost(&kv.key) + 2 + kv.value.as_ref().map_or(4, json_cost))
                .sum::<usize>()
        }
        Some(any_value::Value::StringValueStrindex(_)) | None => 4,
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
            AttrValue::Str(render_json(&AnyValue { value: Some(other) }))
        }
        // A dictionary index of the profiles signal: no dictionary here, so no content.
        any_value::Value::StringValueStrindex(_) => return None,
    })
}

/// A value as compact JSON text (for arrays and key value lists, and for a non-string log
/// body), written straight into the text: no JSON tree is built in between (Kimi run 13
/// finding 1), and the text is at most [`json_cost`] bytes. Key value lists are objects with
/// their keys sorted, and a repeated key keeps its last value; a non-finite double is the
/// string of its Rust form (`"NaN"`, `"inf"`, `"-inf"`); bytes are lowercase hex strings.
pub(crate) fn render_json(v: &AnyValue) -> String {
    let mut out = Vec::with_capacity(json_cost(v).min(64 * 1024));
    serde_json::to_writer(&mut out, &Json(Some(v))).expect("an OTLP value serialises to JSON");
    // serde_json writes UTF-8 only.
    String::from_utf8(out).expect("JSON text is UTF-8")
}

/// [`serde::Serialize`] of an OTLP value, borrowed. `None` is `null`.
struct Json<'a>(Option<&'a AnyValue>);

impl serde::Serialize for Json<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeSeq};
        match self.0.and_then(|v| v.value.as_ref()) {
            Some(any_value::Value::StringValue(x)) => s.serialize_str(x),
            Some(any_value::Value::BoolValue(b)) => s.serialize_bool(*b),
            Some(any_value::Value::IntValue(i)) => s.serialize_i64(*i),
            Some(any_value::Value::DoubleValue(f)) if f.is_finite() => s.serialize_f64(*f),
            Some(any_value::Value::DoubleValue(f)) => s.serialize_str(&f.to_string()),
            Some(any_value::Value::BytesValue(b)) => s.serialize_str(&hex::encode(b)),
            Some(any_value::Value::ArrayValue(a)) => {
                let mut seq = s.serialize_seq(Some(a.values.len()))?;
                for x in &a.values {
                    seq.serialize_element(&Json(Some(x)))?;
                }
                seq.end()
            }
            Some(any_value::Value::KvlistValue(l)) => {
                let sorted: BTreeMap<&str, Option<&AnyValue>> = l
                    .values
                    .iter()
                    .map(|kv| (kv.key.as_str(), kv.value.as_ref()))
                    .collect();
                let mut map = s.serialize_map(Some(sorted.len()))?;
                for (k, x) in sorted {
                    map.serialize_entry(k, &Json(x))?;
                }
                map.end()
            }
            Some(any_value::Value::StringValueStrindex(_)) | None => s.serialize_unit(),
        }
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
    map_logs_held(req, limits, &mut |_| true)
}

fn map_logs_held(
    req: ExportLogsServiceRequest,
    limits: &Limits,
    hold: &mut dyn FnMut(u64) -> bool,
) -> std::result::Result<Mapped, Refusal> {
    let records = count_logs(&req);
    if records > limits.max_records as u64 {
        return Err(Refusal::TooManyRecords);
    }
    let mut out = Mapped {
        records,
        ..Default::default()
    };
    let mut budget = Budget::new(limits, hold);
    let now = crate::now();
    for rl in req.resource_logs {
        for sl in rl.scope_logs {
            let shared = shared_attrs(rl.resource.as_ref(), sl.scope.as_ref());
            let shared_cost = kv_cost(&shared);
            for rec in sl.log_records {
                let body_cost = rec.body.as_ref().map_or(0, body_cost);
                let cost = RECORD_COST + shared_cost + kv_cost(&rec.attributes) + body_cost;
                if !budget.take(cost) {
                    budget.check()?;
                    out.drop(DropReason::BatchTooLarge);
                    continue;
                }
                let text = match rec.body {
                    Some(AnyValue {
                        value: Some(any_value::Value::StringValue(s)),
                    }) => s,
                    Some(AnyValue {
                        value: None | Some(any_value::Value::StringValueStrindex(_)),
                    }) => String::new(),
                    Some(v) => render_json(&v),
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
    out.bytes = budget.used();
    Ok(out)
}

/// Maps a metrics request.
pub fn map_metrics(
    req: ExportMetricsServiceRequest,
    limits: &Limits,
) -> std::result::Result<Mapped, Refusal> {
    map_metrics_held(req, limits, &mut |_| true)
}

fn map_metrics_held(
    req: ExportMetricsServiceRequest,
    limits: &Limits,
    hold: &mut dyn FnMut(u64) -> bool,
) -> std::result::Result<Mapped, Refusal> {
    let records = count_metrics(&req);
    if records > limits.max_records as u64 {
        return Err(Refusal::TooManyRecords);
    }
    let mut out = Mapped {
        records,
        ..Default::default()
    };
    let mut budget = Budget::new(limits, hold);
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
                        budget.check()?;
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
    out.bytes = budget.used();
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
    map_traces_held(req, limits, &mut |_| true)
}

fn map_traces_held(
    req: ExportTraceServiceRequest,
    limits: &Limits,
    hold: &mut dyn FnMut(u64) -> bool,
) -> std::result::Result<Mapped, Refusal> {
    let records = count_traces(&req);
    if records > limits.max_records as u64 {
        return Err(Refusal::TooManyRecords);
    }
    let mut out = Mapped {
        records,
        ..Default::default()
    };
    let mut budget = Budget::new(limits, hold);
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
                    budget.check()?;
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
    out.bytes = budget.used();
    Ok(out)
}
