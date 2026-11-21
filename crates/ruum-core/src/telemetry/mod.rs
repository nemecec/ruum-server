//! Telemetry of apps: the record model, the privacy pipeline and the store border (design 8.5
//! and 9.6, `docs/STEP3.md` "Telemetry and privacy"; decision 0006 answer 4: the pipeline is
//! Rust code, not Collector processors).
//!
//! # The border
//!
//! - [`TelemetryIngest`]: takes a [`Batch`] of one source ([`SourceIdentity`]) and runs the
//!   pipeline. [`PipelineIngest`] is the one implementation; it works with any store.
//! - [`TelemetryStore`]: holds classified records, one storage tenant per Ruum tenant, and reads
//!   them back by a typed [`TelemetryFilter`]. [`memory::MemoryTelemetryStore`] here; card O-1b
//!   adds the real store (Loki, Mimir, Tempo).
//! - [`FieldClassSource`]: the classes of the fields of one app. [`memory::MemoryFieldClasses`]
//!   here; card O-2 adds the Postgres one over `telemetry_fields` (`classes.rs`).
//! - [`SecretSource`]: the app's current and previous secret values and the workload token.
//!   [`memory::MemorySecretSource`] here; card O-1b decides where the real values come from.
//!
//! # The identity of a record
//!
//! The tenant, app, deployment and environment of a record come only from the
//! [`SourceIdentity`] that the caller gives (the authenticated source). The pipeline drops every
//! attribute in the reserved namespace `ruum.*` that a record carries and writes
//! `ruum.tenant`, `ruum.app`, `ruum.deployment` and `ruum.environment` itself. The storage
//! tenant is `identity.tenant`, never a value of a record.
//!
//! # The pipeline
//!
//! [`classify::process`] is a plain function of the batch, the classes, the key and the secret
//! matcher, so every store gets the same treatment. The order (review R6):
//!
//! 1. secrets ([`secrets::SecretMatcher`]),
//! 2. the deny-list ([`denylist`]),
//! 3. the classes ([`classify::effective_class`]),
//! 4. the free-text patterns ([`scrub`]),
//! 5. pseudonyms with the per-app key ([`app_telemetry_key`]),
//! 6. no raw copy here (card O-3; the hook point is marked `RAW COPY HOOK` in `classify.rs`).
//!
//! No code of the pipeline keeps a raw value: the input types have no `Debug`, error texts name
//! no value, and the pipeline writes no `tracing` events.

pub mod classes;
pub mod classify;
pub mod conformance;
pub mod denylist;
pub mod erasure;
pub mod errors;
pub mod lgtm;
pub mod memory;
pub mod otlp;
pub mod processor;
pub mod query;
pub mod raw_copy;
pub mod scrub;
pub mod secrets;
pub mod source;

#[cfg(test)]
mod tests;

use crate::Result;
use crate::ids::{NodeId, TenantId};
use crate::privacy::keys::TenantKeys;
use crate::privacy::{HiddenValue, HmacPseudonymizer, Pseudonym, PseudonymKey};
use crate::runtime::{Environment, SecretValue, WorkloadIdentity};
use crate::usage::{Charged, Meter, UsageEvent, UsageSink};
use async_trait::async_trait;
use serde::{Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use time::OffsetDateTime;
use uuid::Uuid;

// ---------------------------------------------------------------------------- identity

/// Who sent a batch: the authenticated source (card O-1b authenticates it). Never read from a
/// record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct SourceIdentity {
    pub tenant: TenantId,
    /// The app node.
    pub app: NodeId,
    /// The deployment (`app_deployments.id`).
    pub deployment: Uuid,
    pub environment: Environment,
}

impl From<&WorkloadIdentity> for SourceIdentity {
    fn from(w: &WorkloadIdentity) -> Self {
        SourceIdentity {
            tenant: w.tenant,
            app: w.app,
            deployment: w.deployment,
            environment: w.environment,
        }
    }
}

/// The reserved attribute namespace. The pipeline drops these attributes from records and
/// writes the identity attributes itself.
pub const RESERVED_PREFIX: &str = "ruum.";

/// The identity attributes that the pipeline writes on every record.
pub const ATTR_TENANT: &str = "ruum.tenant";
pub const ATTR_APP: &str = "ruum.app";
pub const ATTR_DEPLOYMENT: &str = "ruum.deployment";
pub const ATTR_ENVIRONMENT: &str = "ruum.environment";

// ---------------------------------------------------------------------------- classes

/// The kind of a telemetry field (`telemetry_field_kind`). The same name with another kind is
/// another field with its own class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    /// A field of a log message template, or an attribute of a log record.
    LogField,
    /// A label of a metric point.
    MetricLabel,
    /// An attribute of a span, of a span event or of a span link.
    SpanAttribute,
}

impl FieldKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FieldKind::LogField => "log_field",
            FieldKind::MetricLabel => "metric_label",
            FieldKind::SpanAttribute => "span_attribute",
        }
    }

    fn index(self) -> usize {
        match self {
            FieldKind::LogField => 0,
            FieldKind::MetricLabel => 1,
            FieldKind::SpanAttribute => 2,
        }
    }
}

/// The class of a field (design 8.5, `data_class`). The order is the strictness:
/// `Normal < Personal < Secret`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldClass {
    Normal,
    Personal,
    Secret,
}

/// One row of `telemetry_fields`: the class, whether a person confirmed it, and the suggestion
/// of the privacy file or the review agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldClassEntry {
    pub class: FieldClass,
    /// `confirmed_at` is set.
    pub confirmed: bool,
    pub suggested: Option<FieldClass>,
}

impl FieldClassEntry {
    /// A class that a person confirmed, without a suggestion.
    pub fn confirmed(class: FieldClass) -> Self {
        FieldClassEntry {
            class,
            confirmed: true,
            suggested: None,
        }
    }

    /// A listed field that no person confirmed.
    pub fn unconfirmed(class: FieldClass) -> Self {
        FieldClassEntry {
            class,
            confirmed: false,
            suggested: None,
        }
    }

    pub fn with_suggestion(mut self, suggested: FieldClass) -> Self {
        self.suggested = Some(suggested);
        self
    }
}

/// The classes of the fields of one app.
#[derive(Clone, Debug, Default)]
pub struct FieldClasses {
    fields: HashMap<String, [Option<FieldClassEntry>; 3]>,
}

impl FieldClasses {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, kind: FieldKind, name: impl Into<String>, entry: FieldClassEntry) {
        self.fields.entry(name.into()).or_default()[kind.index()] = Some(entry);
    }

    pub fn get(&self, kind: FieldKind, name: &str) -> Option<FieldClassEntry> {
        self.fields.get(name).and_then(|k| k[kind.index()])
    }

    pub fn len(&self) -> usize {
        self.fields
            .values()
            .map(|k| k.iter().flatten().count())
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Where the pipeline reads the classes of an app's fields, once per batch. The Postgres
/// implementation over `telemetry_fields` is card O-2's (`classes.rs`); wrap it in
/// [`classify::CachedFieldClasses`] for a short cache.
#[async_trait]
pub trait FieldClassSource: Send + Sync {
    async fn classes(&self, tenant: TenantId, app: NodeId) -> Result<Arc<FieldClasses>>;
}

/// Where the pipeline reads the secret values to redact: the app's current and previous secret
/// values and the workload token of the source.
#[async_trait]
pub trait SecretSource: Send + Sync {
    async fn secrets(&self, identity: &SourceIdentity) -> Result<Vec<SecretValue>>;
}

// ---------------------------------------------------------------------------- input records

/// A value of a field, an attribute or a label as the source sent it. It has no `Debug`, so a
/// raw value never ends up in a log by accident.
#[derive(Clone, PartialEq)]
pub enum AttrValue {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl AttrValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            AttrValue::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The bytes that the batch size cap counts.
    fn size(&self) -> usize {
        match self {
            AttrValue::Str(s) => s.len(),
            _ => 8,
        }
    }
}

impl From<&str> for AttrValue {
    fn from(s: &str) -> Self {
        AttrValue::Str(s.to_owned())
    }
}

impl From<String> for AttrValue {
    fn from(s: String) -> Self {
        AttrValue::Str(s)
    }
}

impl From<i64> for AttrValue {
    fn from(v: i64) -> Self {
        AttrValue::Int(v)
    }
}

impl From<f64> for AttrValue {
    fn from(v: f64) -> Self {
        AttrValue::Float(v)
    }
}

impl From<bool> for AttrValue {
    fn from(v: bool) -> Self {
        AttrValue::Bool(v)
    }
}

/// A named value of a record.
#[derive(Clone, PartialEq)]
pub struct Attribute {
    pub key: String,
    pub value: AttrValue,
}

impl Attribute {
    pub fn new(key: impl Into<String>, value: impl Into<AttrValue>) -> Self {
        Attribute {
            key: key.into(),
            value: value.into(),
        }
    }
}

/// The severity of a log record (OTel severity ranges).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

/// The body of a log record.
#[derive(Clone, PartialEq)]
pub enum LogBody {
    /// A message template (`order {order_id} paid by {customer}`) and its named fields.
    Template {
        template: String,
        fields: Vec<Attribute>,
    },
    /// Free text without a template: `personal`, scrubbed with patterns.
    Text(String),
}

/// One log record.
#[derive(Clone, PartialEq)]
pub struct LogRecord {
    pub time: OffsetDateTime,
    pub severity: Severity,
    pub body: LogBody,
    pub attributes: Vec<Attribute>,
}

/// The kind of a metric point.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    Counter,
    Gauge,
    Histogram,
}

/// One metric point.
#[derive(Clone, PartialEq)]
pub struct MetricPoint {
    pub time: OffsetDateTime,
    pub name: String,
    pub kind: MetricKind,
    pub value: f64,
    pub labels: Vec<Attribute>,
}

/// The kind of a span (OTel `SpanKind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanKind {
    Internal,
    Server,
    Client,
    Producer,
    Consumer,
}

/// The status code of a span.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusCode {
    Unset,
    Ok,
    Error,
}

/// An event of a span (an exception, a log point).
#[derive(Clone, PartialEq)]
pub struct SpanEvent {
    pub time: OffsetDateTime,
    /// Free text: scrubbed with patterns.
    pub name: String,
    pub attributes: Vec<Attribute>,
}

/// A link of a span to another span.
#[derive(Clone, PartialEq)]
pub struct SpanLink {
    /// 32 lowercase hex characters.
    pub trace_id: String,
    /// 16 lowercase hex characters.
    pub span_id: String,
    pub attributes: Vec<Attribute>,
}

/// One span.
#[derive(Clone, PartialEq)]
pub struct Span {
    /// 32 lowercase hex characters, not all zero.
    pub trace_id: String,
    /// 16 lowercase hex characters, not all zero.
    pub span_id: String,
    pub parent_span_id: Option<String>,
    /// Scrubbed with patterns (`GET /users/alice@example.com` happens).
    pub name: String,
    pub kind: SpanKind,
    pub start: OffsetDateTime,
    pub end: OffsetDateTime,
    pub status: StatusCode,
    /// Free text (an exception message): scrubbed with patterns.
    pub status_message: Option<String>,
    pub attributes: Vec<Attribute>,
    pub events: Vec<SpanEvent>,
    pub links: Vec<SpanLink>,
}

/// One batch of one source. OTLP-shaped, not OTLP-bound: card O-1b maps OTLP onto it.
#[derive(Clone, Default, PartialEq)]
pub struct Batch {
    pub logs: Vec<LogRecord>,
    pub metrics: Vec<MetricPoint>,
    pub spans: Vec<Span>,
}

impl Batch {
    pub fn records(&self) -> usize {
        self.logs.len() + self.metrics.len() + self.spans.len()
    }
}

// ---------------------------------------------------------------------------- stored records

/// A value after the pipeline (`TelemetryValue` of `contracts/openapi.yaml`): a `normal` value,
/// the hidden-value marker or a pseudonym marker.
#[derive(Clone, Debug, PartialEq)]
pub enum StoredValue {
    Text(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Hidden(HiddenValue),
    Pseudonym(Pseudonym),
}

impl StoredValue {
    /// The text that a field equality filter compares with: the text, the number, the pseudonym
    /// ID. The hidden-value marker never matches.
    pub fn match_text(&self) -> Option<String> {
        match self {
            StoredValue::Text(s) => Some(s.clone()),
            StoredValue::Int(i) => Some(i.to_string()),
            StoredValue::Float(f) => Some(f.to_string()),
            StoredValue::Bool(b) => Some(b.to_string()),
            StoredValue::Pseudonym(p) => Some(p.id().to_owned()),
            StoredValue::Hidden(_) => None,
        }
    }

    pub fn is_hidden_secret(&self) -> bool {
        matches!(self, StoredValue::Hidden(h) if *h == HiddenValue::SECRET)
    }

    pub fn as_pseudonym(&self) -> Option<&Pseudonym> {
        match self {
            StoredValue::Pseudonym(p) => Some(p),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            StoredValue::Text(s) => Some(s),
            _ => None,
        }
    }
}

impl Serialize for StoredValue {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            StoredValue::Text(v) => s.serialize_str(v),
            StoredValue::Int(v) => s.serialize_i64(*v),
            StoredValue::Float(v) => s.serialize_f64(*v),
            StoredValue::Bool(v) => s.serialize_bool(*v),
            StoredValue::Hidden(v) => v.serialize(s),
            StoredValue::Pseudonym(v) => v.serialize(s),
        }
    }
}

/// A named value after the pipeline.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StoredAttribute {
    pub key: String,
    pub value: StoredValue,
}

impl StoredAttribute {
    pub fn text(key: &str, value: impl Into<String>) -> Self {
        StoredAttribute {
            key: key.to_owned(),
            value: StoredValue::Text(value.into()),
        }
    }
}

/// The value of the first attribute named `key`.
pub fn attr<'a>(attrs: &'a [StoredAttribute], key: &str) -> Option<&'a StoredValue> {
    attrs.iter().find(|a| a.key == key).map(|a| &a.value)
}

/// A log record after the pipeline.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StoredLog {
    pub source: SourceIdentity,
    #[serde(with = "time::serde::rfc3339")]
    pub time: OffsetDateTime,
    pub severity: Severity,
    /// The template (scrubbed with patterns); `None` for free text.
    pub template: Option<String>,
    /// The free text after the scrubber; `None` for a template.
    pub text: Option<String>,
    pub fields: Vec<StoredAttribute>,
    /// The record's attributes, then the identity attributes.
    pub attributes: Vec<StoredAttribute>,
}

/// A metric point after the pipeline. Every label is `normal`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StoredMetric {
    pub source: SourceIdentity,
    #[serde(with = "time::serde::rfc3339")]
    pub time: OffsetDateTime,
    pub name: String,
    pub kind: MetricKind,
    pub value: f64,
    /// The record's `normal` labels, then the identity labels.
    pub labels: Vec<StoredAttribute>,
}

/// A span event after the pipeline.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StoredEvent {
    #[serde(with = "time::serde::rfc3339")]
    pub time: OffsetDateTime,
    pub name: String,
    pub attributes: Vec<StoredAttribute>,
}

/// A span link after the pipeline.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StoredLink {
    pub trace_id: String,
    pub span_id: String,
    pub attributes: Vec<StoredAttribute>,
}

/// A span after the pipeline.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StoredSpan {
    pub source: SourceIdentity,
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub kind: SpanKind,
    #[serde(with = "time::serde::rfc3339")]
    pub start: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub end: OffsetDateTime,
    pub status: StatusCode,
    pub status_message: Option<String>,
    /// The span's attributes, then the identity attributes.
    pub attributes: Vec<StoredAttribute>,
    pub events: Vec<StoredEvent>,
    pub links: Vec<StoredLink>,
}

/// Records after the pipeline: what a store writes and what a read returns.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ClassifiedBatch {
    pub logs: Vec<StoredLog>,
    pub metrics: Vec<StoredMetric>,
    pub spans: Vec<StoredSpan>,
}

impl ClassifiedBatch {
    pub fn records(&self) -> usize {
        self.logs.len() + self.metrics.len() + self.spans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records() == 0
    }

    /// The bytes that the meter counts: the JSON form of the records. Counted as it is
    /// written, so the JSON text is never held in memory (Kimi run 13 finding 1).
    pub fn byte_size(&self) -> usize {
        struct Count(usize);
        impl std::io::Write for Count {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0 += buf.len();
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut n = Count(0);
        serde_json::to_writer(&mut n, self).expect("stored telemetry records serialise to JSON");
        n.0
    }
}

// ---------------------------------------------------------------------------- reads

/// One signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Signal {
    Logs,
    Metrics,
    Traces,
}

/// A field equality: an attribute, template field or label `key` whose value has the text
/// `value` ([`StoredValue::match_text`]: a pseudonym compares by its ID).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldEquals {
    pub key: String,
    pub value: String,
}

/// A typed filter of a read. There is no query string of any store language: card O-2 builds
/// LogQL, PromQL and TraceQL from this filter, escaped. The tenant is not part of the filter:
/// it is the separate argument of [`TelemetryStore::read`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TelemetryFilter {
    pub signal: Option<Signal>,
    pub app: Option<NodeId>,
    pub environment: Option<Environment>,
    pub deployment: Option<Uuid>,
    /// Inclusive.
    pub from: Option<OffsetDateTime>,
    /// Exclusive.
    pub to: Option<OffsetDateTime>,
    /// Logs only: at least this severity.
    pub min_severity: Option<Severity>,
    /// All must match.
    pub fields: Vec<FieldEquals>,
    pub limit: Option<usize>,
    /// Logs only: words that must all be in the record ([`log_has_words`]). Card O-2.
    pub words: Vec<String>,
    /// Logs: the record's [`LOG_TRACE_ID`] attribute; spans: the span's trace ID. Only 32
    /// lower-case hex characters match anything ([`is_trace_id`]). Card O-2.
    pub trace_id: Option<String>,
    /// Logs only: the origin of the record ([`log_origin`]). Card O-2.
    pub origin: Option<LogOrigin>,
    /// Keep the newest `limit` matches instead of the oldest (the answer is still oldest
    /// first). Card O-2 (`searchAppLogs` is newest first).
    pub newest: bool,
}

/// Where a log record comes from (`AppLogRecord.origin`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogOrigin {
    /// Standard output and the OTel SDK of the app (through the runtime and the processor).
    Backend,
    /// The browser script (card G-2; no such record exists before it).
    Frontend,
    /// Build and deploy events of the platform (no such record exists yet).
    Platform,
}

impl LogOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            LogOrigin::Backend => "backend",
            LogOrigin::Frontend => "frontend",
            LogOrigin::Platform => "platform",
        }
    }
}

/// The origin of a stored log record. Every record that the processor stores today comes from
/// an app's backend: the browser script (G-2) and platform events will mark theirs when they
/// come.
pub fn log_origin(_r: &StoredLog) -> LogOrigin {
    LogOrigin::Backend
}

/// The attribute of a log record that holds its trace ID (the OTLP log record's own trace ID
/// is not kept by the mapping yet; the Node SDK writes this attribute).
pub const LOG_TRACE_ID: &str = "trace_id";

/// Whether `s` is an OTel trace ID in the form of the contract (`^[0-9a-f]{32}$`).
pub fn is_trace_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The trace ID of a stored log record: its [`LOG_TRACE_ID`] attribute when it is readable
/// text of the trace ID form.
pub fn log_trace_id(r: &StoredLog) -> Option<&str> {
    attr(&r.attributes, LOG_TRACE_ID)
        .and_then(StoredValue::as_text)
        .filter(|t| is_trace_id(t))
}

/// Whether every word of `words` is in the record (case-sensitive): in its template or free
/// text, in a readable (`normal`) value of a field or attribute, or equal to the ID of a
/// pseudonym the record holds (`p_...`). A hidden-value marker never matches. The identity
/// attributes (`ruum.*`) are not searched.
pub fn log_has_words(r: &StoredLog, words: &[String]) -> bool {
    words.iter().all(|w| {
        r.template
            .as_deref()
            .is_some_and(|t| t.contains(w.as_str()))
            || r.text.as_deref().is_some_and(|t| t.contains(w.as_str()))
            || r.fields
                .iter()
                .chain(
                    r.attributes
                        .iter()
                        .filter(|a| !classify::is_reserved(&a.key)),
                )
                .any(|a| match &a.value {
                    StoredValue::Text(s) => s.contains(w.as_str()),
                    StoredValue::Int(i) => i.to_string() == *w,
                    StoredValue::Float(f) => f.to_string() == *w,
                    StoredValue::Bool(b) => b.to_string() == *w,
                    StoredValue::Pseudonym(p) => p.id() == w,
                    StoredValue::Hidden(_) => false,
                })
    })
}

/// The parts of a log filter that the stores apply alike on the decoded records: the words,
/// the trace ID and the origin.
pub fn log_extra_matches(r: &StoredLog, f: &TelemetryFilter) -> bool {
    log_has_words(r, &f.words)
        && f.trace_id
            .as_deref()
            .is_none_or(|t| is_trace_id(t) && log_trace_id(r) == Some(t))
        && f.origin.is_none_or(|o| log_origin(r) == o)
}

/// The part of a span filter that the stores apply alike: the trace ID.
pub fn span_extra_matches(r: &StoredSpan, f: &TelemetryFilter) -> bool {
    f.trace_id
        .as_deref()
        .is_none_or(|t| is_trace_id(t) && r.trace_id == t)
}

impl TelemetryFilter {
    pub fn app(app: NodeId) -> Self {
        TelemetryFilter {
            app: Some(app),
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------- report

/// Why a record was dropped. A dropped record is never stored in part.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    /// The batch had more records than [`Limits::max_records`].
    TooManyRecords,
    /// The batch was larger than [`Limits::max_batch_bytes`].
    BatchTooLarge,
    /// The record had more values than [`Limits::max_fields`].
    TooManyFields,
    /// A value or a name was longer than [`Limits::max_value_bytes`].
    ValueTooLong,
    /// A trace or span ID is not of the OTel form.
    InvalidId,
    /// A trace or span ID, or a metric name, holds a secret value.
    SecretInIdentifier,
    /// A metric name does not have the OTel instrument name syntax
    /// ([`classify::is_metric_name`]).
    InvalidName,
    /// A metric name holds a match of a free-text pattern (an email, a card number, a token).
    PatternInName,
    /// A metric point value holds a secret value (a secret of digits only).
    SecretInValue,
}

/// What one ingest did. Counts and names only; never a value.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct IngestReport {
    pub stored_logs: u64,
    pub stored_metrics: u64,
    pub stored_spans: u64,
    pub dropped: BTreeMap<DropReason, u64>,
    /// Values and texts in which the secret matcher found a secret.
    pub secrets_redacted: u64,
    /// Values that became a pseudonym (fields, attributes, matches of the free-text patterns).
    pub pseudonymized: u64,
    /// Values that became the hidden-value marker by class or deny-list.
    pub hidden: u64,
    /// Metric labels dropped because their class is not `normal`.
    pub labels_dropped: u64,
    /// `ruum.*` attributes of records that were dropped (the pipeline writes its own).
    pub reserved_dropped: u64,
    /// Attributes whose name is not a valid attribute key, or holds a match of a free-text
    /// pattern (a card number, a token).
    pub invalid_names_dropped: u64,
    /// Fields that the class source does not know ("seen"), so card O-2 can write
    /// `first_seen_at`. Only names of the `telemetry_fields` pattern.
    pub unknown_fields: BTreeSet<(FieldKind, String)>,
    /// The bytes metered ([`ClassifiedBatch::byte_size`]).
    pub bytes: u64,
}

impl IngestReport {
    pub fn dropped(&self, reason: DropReason) -> u64 {
        self.dropped.get(&reason).copied().unwrap_or(0)
    }

    pub(crate) fn drop(&mut self, reason: DropReason) {
        *self.dropped.entry(reason).or_default() += 1;
    }
}

// ---------------------------------------------------------------------------- limits

/// The caps of a batch and a record. Over a cap, the record is dropped with a counted reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The records of one batch.
    pub max_records: usize,
    /// The bytes of one batch: strings by length, numbers as 8 bytes, plus a fixed overhead
    /// per value and per record (`classify::VALUE_OVERHEAD`, `classify::RECORD_OVERHEAD`), so
    /// the cap also bounds what the pipeline makes of the batch.
    pub max_batch_bytes: usize,
    /// The values of one record (fields, attributes, labels, events, links and their
    /// attributes).
    pub max_fields: usize,
    /// The bytes of one value, name, message or text. Also the cap of the free-text scrubber.
    pub max_value_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_records: 10_000,
            max_batch_bytes: 8 * 1024 * 1024,
            max_fields: 256,
            max_value_bytes: scrub::MAX_TEXT_BYTES,
        }
    }
}

// ---------------------------------------------------------------------------- traits

/// Runs the pipeline on a batch of one source and writes the result to a store.
#[async_trait]
pub trait TelemetryIngest: Send + Sync {
    async fn ingest(&self, identity: &SourceIdentity, batch: Batch) -> Result<IngestReport>;
}

/// The telemetry store: one storage tenant per Ruum tenant.
#[async_trait]
pub trait TelemetryStore: Send + Sync {
    /// Writes classified records into the storage tenant of `identity.tenant`. The storage
    /// tenant comes only from the identity.
    async fn write(&self, identity: &SourceIdentity, batch: ClassifiedBatch) -> Result<()>;

    /// Reads records of the storage tenant `tenant` that match `filter`, oldest first. Never a
    /// record of another tenant, whatever the filter says.
    async fn read(&self, tenant: TenantId, filter: &TelemetryFilter) -> Result<ClassifiedBatch>;
}

// ---------------------------------------------------------------------------- keys

/// The domain of the per-app telemetry key.
pub const APP_KEY_DOMAIN: &str = "ruum/telemetry/v1/";

/// The pseudonym key of one app's telemetry (provisional decision of card O-1a, for decision
/// 0006):
///
/// ```text
/// app_key = HMAC-SHA-256(tenant_key, "ruum/telemetry/v1/" || app_node_id)
/// ```
///
/// with the node ID in its text form (lowercase, with hyphens). So pseudonyms are pairwise per
/// app (design 9.6) and differ from the database pseudonyms of step 2 (`'db:' || node`, or the
/// tenant key itself). A pseudonym is then `privacy::HmacPseudonymizer` with this key:
/// `'p_' || hex(HMAC-SHA-256(app_key, canonical_text(v)))[..12]`.
pub fn app_telemetry_key(tenant_key: &PseudonymKey, app: NodeId) -> PseudonymKey {
    let msg = format!("{APP_KEY_DOMAIN}{}", app.as_uuid().hyphenated());
    PseudonymKey::from_bytes(hmac_sha256(tenant_key.as_bytes(), msg.as_bytes()))
}

/// HMAC-SHA-256 (RFC 2104) on `sha2`, for a key of at most one block (64 bytes). The same
/// construction as the private one of `privacy` (which this card may not change); the tests
/// check it against RFC 4231 and against `PseudonymKey::derive_for_database`.
pub(crate) fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut block_key = [0u8; BLOCK];
    if key.len() > BLOCK {
        block_key[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block_key[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= block_key[i];
        opad[i] ^= block_key[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    outer.finalize().into()
}

// ---------------------------------------------------------------------------- ingest

/// [`TelemetryIngest`] for any store: reads the classes, the tenant key and the secrets, runs
/// [`classify::process`], writes the result and meters the bytes.
pub struct PipelineIngest {
    store: Arc<dyn TelemetryStore>,
    classes: Arc<dyn FieldClassSource>,
    secrets: Arc<dyn SecretSource>,
    keys: TenantKeys,
    usage: Arc<dyn UsageSink>,
    limits: Limits,
}

impl PipelineIngest {
    pub fn new(
        store: Arc<dyn TelemetryStore>,
        classes: Arc<dyn FieldClassSource>,
        secrets: Arc<dyn SecretSource>,
        keys: TenantKeys,
        usage: Arc<dyn UsageSink>,
    ) -> Self {
        PipelineIngest {
            store,
            classes,
            secrets,
            keys,
            usage,
            limits: Limits::default(),
        }
    }

    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }
}

#[async_trait]
impl TelemetryIngest for PipelineIngest {
    async fn ingest(&self, identity: &SourceIdentity, batch: Batch) -> Result<IngestReport> {
        // The storage tenant comes only from the identity, never from a record.
        let target = *identity;
        let classes = self.classes.classes(identity.tenant, identity.app).await?;
        let tenant_key = self.keys.tenant_key(identity.tenant).await?;
        let pseudonymizer = HmacPseudonymizer::new(app_telemetry_key(&tenant_key, identity.app));
        let secret_values = self.secrets.secrets(identity).await?;
        let matcher = secrets::SecretMatcher::new(secret_values.iter().map(|s| s.expose()));
        let (out, mut report) = classify::process(
            identity,
            batch,
            &classes,
            &pseudonymizer,
            &matcher,
            &self.limits,
        );
        if !out.is_empty() {
            let bytes = out.byte_size();
            report.bytes = bytes as u64;
            // Meter first: when metering fails, nothing is stored (no stored telemetry without
            // usage). A store failure after this charges bytes that were not stored; that is
            // the safe side for budgets and hard caps (design 21.5).
            self.usage
                .record(UsageEvent {
                    tenant: identity.tenant,
                    app: identity.app,
                    deployment: None,
                    charged: Charged::App,
                    meter: Meter::TelemetryBytes,
                    quantity: i64::try_from(bytes).unwrap_or(i64::MAX),
                    at: crate::now(),
                })
                .await?;
            self.store.write(&target, out).await?;
        }
        Ok(report)
    }
}
