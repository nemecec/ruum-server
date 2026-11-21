//! Card O-2: the read operations of telemetry (logs, metrics, traces) over `TelemetryStore`,
//! and the class operations over `telemetry_fields` (`classes.rs`).
//!
//! [`TelemetryService`] is the telemetry part of the services (`Services::telemetry`): the
//! store, the field store with its class cache, and the tenant keys of the pipeline, so the API,
//! the processor and the suites read and write the same records and classes.
//!
//! # The rules of every read
//!
//! - The tenant comes from the caller's context, the app from the path; neither is ever a
//!   parameter of a store query. The store builds the selector from typed values and escapes
//!   every value (`lgtm.rs`, `quote`); `q` is only words for line filters (`|=`), never a regex
//!   or a pipeline stage.
//! - Access: `edit` on the app (an agent: its user's right within its scope). A caller who
//!   cannot see the app, an app of another tenant and an ID of no app get the same `404`.
//! - Records are returned as the pipeline stored them (decision 13): a `secret` value is the
//!   hidden-value marker, a `personal` value a pseudonym, `normal` readable. Log answers carry
//!   `content_trust: untrusted`.
//! - The read budget ([`ReadBudget`]) holds every read of the stores: per tenant and per
//!   principal, concurrent and per minute, and a global bound of concurrent reads.

use super::classes::{
    ClassCache, Confirmation, FieldRow, MemoryFieldStore, PgFieldStore, SeenFields,
    SeenFieldsIngest, StoreClasses, StoreSuggestions, TelemetryFieldStore, parse_class, parse_kind,
};
use super::*;
use crate::actor::Context;
use crate::apps::store::Environment as DeployEnvironment;
use crate::audit::NewAuditEntry;
use crate::authz::{self, Role};
use crate::error::{ApiError, Error, ErrorCode};
use crate::nodes::{Node, NodeKind};
use crate::paging::{Cursor, Page, PageRequest};
use crate::privacy::{Pseudonymizer, SqlValue};
use crate::runtime::SecretValue;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The longest window of a log search or a metrics read.
pub const MAX_WINDOW: time::Duration = time::Duration::days(7);
/// The window of a log search without `since`.
pub const DEFAULT_LOG_WINDOW: time::Duration = time::Duration::days(7);
/// The window of a metrics read without `since`.
pub const DEFAULT_METRICS_WINDOW: time::Duration = time::Duration::hours(1);
/// The most points of one metric series.
pub const MAX_POINTS: i64 = 500;
/// The longest `q`, in characters (the contract's `maxLength`).
pub const MAX_Q_CHARS: usize = 500;

// ---------------------------------------------------------------------------- the budget

/// The read budget of the telemetry stores (P-99): each read of Loki, Mimir or Tempo takes a
/// slot. Over a bound, the read is refused with the REST API's rate-limit answer (`429
/// rate_limited`, `Retry-After`).
///
/// Who is refused first across tenants (M-59): no tenant holds more than
/// [`BudgetLimits::tenant_concurrent`] of the [`BudgetLimits::global_concurrent`] slots, so
/// when the pool is full the newcomer is refused, and at least `global / tenant` tenants can
/// always read; within a tenant, no principal holds more than
/// [`BudgetLimits::principal_concurrent`], and the per-minute counts bound a tenant's and an
/// agent's steady load.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetLimits {
    pub global_concurrent: usize,
    pub tenant_concurrent: usize,
    pub principal_concurrent: usize,
    pub tenant_per_minute: usize,
    pub principal_per_minute: usize,
}

impl Default for BudgetLimits {
    fn default() -> Self {
        BudgetLimits {
            global_concurrent: 64,
            tenant_concurrent: 8,
            principal_concurrent: 4,
            tenant_per_minute: 600,
            principal_per_minute: 240,
        }
    }
}

#[derive(Default)]
struct Counter {
    running: usize,
    window: Option<Instant>,
    in_window: usize,
}

impl Counter {
    fn roll(&mut self, now: Instant) {
        if self
            .window
            .is_none_or(|w| now.duration_since(w) >= Duration::from_secs(60))
        {
            self.window = Some(now);
            self.in_window = 0;
        }
    }

    fn idle(&self, now: Instant) -> bool {
        self.running == 0
            && self
                .window
                .is_none_or(|w| now.duration_since(w) >= Duration::from_secs(60))
    }
}

#[derive(Default)]
struct BudgetState {
    running: usize,
    tenants: HashMap<TenantId, Counter>,
    principals: HashMap<(TenantId, Uuid), Counter>,
}

/// See [`BudgetLimits`].
pub struct ReadBudget {
    limits: BudgetLimits,
    state: Arc<Mutex<BudgetState>>,
}

/// A held read slot; dropping it frees the slot.
pub struct ReadPermit {
    state: Arc<Mutex<BudgetState>>,
    tenant: TenantId,
    principal: Uuid,
}

impl Drop for ReadPermit {
    fn drop(&mut self) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.running = st.running.saturating_sub(1);
        if let Some(c) = st.tenants.get_mut(&self.tenant) {
            c.running = c.running.saturating_sub(1);
        }
        if let Some(c) = st.principals.get_mut(&(self.tenant, self.principal)) {
            c.running = c.running.saturating_sub(1);
        }
    }
}

fn rate_limited(what: &str, retry: u64) -> Error {
    Error::Api(
        ApiError::new(ErrorCode::RateLimited, format!("too many {what}"))
            .with_hint("wait and try again")
            .with_retry_after(retry),
    )
}

impl ReadBudget {
    pub fn new(limits: BudgetLimits) -> Self {
        ReadBudget {
            limits,
            state: Arc::default(),
        }
    }

    /// Takes a slot for a read of `principal` in `tenant`, or refuses it.
    pub fn acquire(&self, tenant: TenantId, principal: Uuid) -> Result<ReadPermit> {
        let l = self.limits;
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if st.tenants.len() > 10_000 {
            st.tenants.retain(|_, c| !c.idle(now));
        }
        if st.principals.len() > 100_000 {
            st.principals.retain(|_, c| !c.idle(now));
        }
        if st.running >= l.global_concurrent {
            return Err(rate_limited("telemetry reads at once", 1));
        }
        {
            let t = st.tenants.entry(tenant).or_default();
            t.roll(now);
            if t.running >= l.tenant_concurrent {
                return Err(rate_limited("telemetry reads of this tenant at once", 1));
            }
            if t.in_window >= l.tenant_per_minute {
                return Err(rate_limited(
                    "telemetry reads of this tenant in a minute",
                    60,
                ));
            }
        }
        {
            let p = st.principals.entry((tenant, principal)).or_default();
            p.roll(now);
            if p.running >= l.principal_concurrent {
                return Err(rate_limited("telemetry reads of this caller at once", 1));
            }
            if p.in_window >= l.principal_per_minute {
                return Err(rate_limited(
                    "telemetry reads of this caller in a minute",
                    60,
                ));
            }
            p.running += 1;
            p.in_window += 1;
        }
        let t = st.tenants.get_mut(&tenant).expect("the tenant counter");
        t.running += 1;
        t.in_window += 1;
        st.running += 1;
        Ok(ReadPermit {
            state: self.state.clone(),
            tenant,
            principal,
        })
    }
}

// ---------------------------------------------------------------------------- pseudonym limit

/// The rate limit of `computeAppPseudonym` (decision 0006-8; the contract says 60 per hour per
/// person): per person and app at most [`PseudonymLimits::per_minute`] in a minute (the burst)
/// and [`PseudonymLimits::per_hour`] in an hour; per tenant at most
/// [`PseudonymLimits::tenant_per_hour`] in an hour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PseudonymLimits {
    pub per_minute: usize,
    pub per_hour: usize,
    pub tenant_per_hour: usize,
}

impl Default for PseudonymLimits {
    fn default() -> Self {
        PseudonymLimits {
            per_minute: 10,
            per_hour: 60,
            tenant_per_hour: 300,
        }
    }
}

#[derive(Default)]
struct LimiterState {
    people: HashMap<(TenantId, Uuid, NodeId), VecDeque<Instant>>,
    tenants: HashMap<TenantId, VecDeque<Instant>>,
}

/// See [`PseudonymLimits`].
pub struct PseudonymLimiter {
    limits: PseudonymLimits,
    state: Mutex<LimiterState>,
}

const HOUR: Duration = Duration::from_secs(3600);

impl PseudonymLimiter {
    pub fn new(limits: PseudonymLimits) -> Self {
        PseudonymLimiter {
            limits,
            state: Mutex::default(),
        }
    }

    /// Counts one lookup of `user` on `app`, or refuses it.
    pub fn take(&self, tenant: TenantId, user: Uuid, app: NodeId) -> Result<()> {
        let l = self.limits;
        let now = Instant::now();
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let prune = |q: &mut VecDeque<Instant>| {
            while q.front().is_some_and(|t| now.duration_since(*t) >= HOUR) {
                q.pop_front();
            }
        };
        if st.people.len() > 100_000 {
            st.people.retain(|_, q| {
                prune(q);
                !q.is_empty()
            });
        }
        if st.tenants.len() > 10_000 {
            st.tenants.retain(|_, q| {
                prune(q);
                !q.is_empty()
            });
        }
        let tq = st.tenants.entry(tenant).or_default();
        prune(tq);
        if tq.len() >= l.tenant_per_hour {
            return Err(rate_limited("pseudonym lookups in this tenant", 600));
        }
        let pq = st.people.entry((tenant, user, app)).or_default();
        prune(pq);
        let last_minute = pq
            .iter()
            .filter(|t| now.duration_since(**t) < Duration::from_secs(60))
            .count();
        if pq.len() >= l.per_hour {
            return Err(rate_limited("pseudonym lookups in an hour", 600));
        }
        if last_minute >= l.per_minute {
            return Err(rate_limited("pseudonym lookups in a minute", 60));
        }
        pq.push_back(now);
        st.tenants.entry(tenant).or_default().push_back(now);
        Ok(())
    }
}

// ---------------------------------------------------------------------------- views

/// One `TelemetryField` of the contract.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FieldView {
    pub field: String,
    pub kind: FieldKind,
    /// The effective class (the deny-list wins over the row).
    pub class: FieldClass,
    pub confirmed: bool,
    pub suggested_class: Option<FieldClass>,
    pub suggestion_reason: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub first_seen_at: Option<OffsetDateTime>,
}

impl From<&FieldRow> for FieldView {
    fn from(r: &FieldRow) -> Self {
        FieldView {
            field: r.field.clone(),
            kind: r.kind,
            class: r.effective_class(),
            confirmed: r.confirmed(),
            suggested_class: r.suggested_class,
            suggestion_reason: r.suggestion_reason.clone(),
            first_seen_at: r.first_seen_at,
        }
    }
}

/// One `AppLogRecord` of the contract.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LogView {
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub level: Severity,
    pub origin: LogOrigin,
    pub deployment_id: Option<Uuid>,
    pub template: Option<String>,
    pub message: String,
    pub fields: BTreeMap<String, StoredValue>,
    pub trace_id: Option<String>,
    pub route: Option<String>,
}

/// `AppLogPage`.
#[derive(Clone, Debug, Serialize)]
pub struct LogPage {
    pub items: Vec<LogView>,
    pub next_cursor: Option<Cursor>,
    pub content_trust: &'static str,
}

/// The text of a stored value in a message: the value, or a marker as text.
fn value_text(v: &StoredValue) -> String {
    match v {
        StoredValue::Text(s) => s.clone(),
        StoredValue::Int(i) => i.to_string(),
        StoredValue::Float(f) => f.to_string(),
        StoredValue::Bool(b) => b.to_string(),
        StoredValue::Hidden(_) => "<hidden>".into(),
        StoredValue::Pseudonym(p) => {
            let id = p.id().strip_prefix("p_").unwrap_or(p.id());
            format!("<person {}>", &id[..id.len().min(6)])
        }
    }
}

/// The message of a template with the stored values put in (`{name}`); a placeholder without
/// a field stays as it is.
pub fn render_message(template: &str, fields: &[StoredAttribute]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let name = &after[..close];
                match attr(fields, name) {
                    Some(v) => out.push_str(&value_text(v)),
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(&rest[open..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn log_view(r: &StoredLog) -> LogView {
    let mut fields = BTreeMap::new();
    for a in r.fields.iter().chain(
        r.attributes
            .iter()
            .filter(|a| !classify::is_reserved(&a.key)),
    ) {
        fields
            .entry(a.key.clone())
            .or_insert_with(|| a.value.clone());
    }
    let message = match (&r.template, &r.text) {
        (Some(t), _) => render_message(t, &r.fields),
        (None, Some(t)) => t.clone(),
        (None, None) => String::new(),
    };
    let route = ["http.route", "url.path"]
        .iter()
        .find_map(|k| attr(&r.attributes, k).and_then(StoredValue::as_text))
        .map(|s| s.split('?').next().unwrap_or_default().to_owned());
    LogView {
        at: r.time,
        level: r.severity,
        origin: log_origin(r),
        deployment_id: Some(r.source.deployment),
        template: r.template.clone(),
        message,
        fields,
        trace_id: log_trace_id(r).map(str::to_owned),
        route,
    }
}

/// One span of `AppTrace`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SpanView {
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub app_id: NodeId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    pub name: String,
    #[serde(with = "time::serde::rfc3339")]
    pub start: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub end: OffsetDateTime,
    pub status: StatusCode,
    pub attributes: BTreeMap<String, StoredValue>,
    pub opaque: bool,
}

/// `AppTrace`.
#[derive(Clone, Debug, Serialize)]
pub struct TraceView {
    pub trace_id: String,
    pub spans: Vec<SpanView>,
}

/// The name of the one opaque span of an app the caller cannot read.
pub const OPAQUE_SPAN_NAME: &str = "another app";

fn span_view(s: &StoredSpan, app_name: Option<String>) -> SpanView {
    let mut attributes = BTreeMap::new();
    for a in s
        .attributes
        .iter()
        .filter(|a| !classify::is_reserved(&a.key))
    {
        attributes
            .entry(a.key.clone())
            .or_insert_with(|| a.value.clone());
    }
    SpanView {
        span_id: s.span_id.clone(),
        parent_span_id: s.parent_span_id.clone(),
        app_id: s.source.app,
        app_name,
        name: s.name.clone(),
        start: s.start,
        end: s.end,
        status: s.status,
        attributes,
        opaque: false,
    }
}

/// The one opaque span of an app's spans: its app, its first span's IDs (so the tree still
/// joins) and the times; no name, attributes, events or status.
fn opaque_span(spans: &[&StoredSpan], app_name: Option<String>) -> Option<SpanView> {
    let first = spans.iter().min_by_key(|s| s.start)?;
    Some(SpanView {
        span_id: first.span_id.clone(),
        parent_span_id: first.parent_span_id.clone(),
        app_id: first.source.app,
        app_name,
        name: OPAQUE_SPAN_NAME.into(),
        start: spans.iter().map(|s| s.start).min()?,
        end: spans.iter().map(|s| s.end).max()?,
        status: StatusCode::Unset,
        attributes: BTreeMap::new(),
        opaque: true,
    })
}

// ---------------------------------------------------------------------------- metrics

/// How a series aggregates the points of one step.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Agg {
    Sum,
    Max,
    Avg,
    Percentile(f64),
    /// The sum of the errors metric over the sum of the requests metric.
    ErrorRatio,
}

/// A series of `AppMetrics`: its name, unit, the metric it reads and the card that brings that
/// metric (none of them is written before that card; until then the series is empty).
struct SeriesDef {
    name: &'static str,
    unit: &'static str,
    metric: &'static str,
    agg: Agg,
}

/// The metric of the request counter (gateway request metrics, a G-1b follow-up).
pub const METRIC_REQUESTS: &str = "http.server.requests";
/// The metric of the requests answered with `5xx` (gateway, G-1b follow-up).
pub const METRIC_ERRORS: &str = "http.server.errors";
/// The request duration in milliseconds (gateway, G-1b follow-up; or the app's OTel SDK).
pub const METRIC_DURATION: &str = "http.server.duration";
/// Instance CPU in cores (the driver, I-4/I-6).
pub const METRIC_CPU: &str = "ruum.instance.cpu";
/// Instance memory in MB (the driver, I-4/I-6).
pub const METRIC_MEMORY: &str = "ruum.instance.memory";
/// Running instances (the driver, I-4/I-6).
pub const METRIC_INSTANCES: &str = "ruum.instance.count";
/// Cold starts (the driver, I-4/I-6).
pub const METRIC_COLD_STARTS: &str = "ruum.instance.cold_starts";

const SERIES: &[SeriesDef] = &[
    SeriesDef {
        name: "requests",
        unit: "count",
        metric: METRIC_REQUESTS,
        agg: Agg::Sum,
    },
    SeriesDef {
        name: "errors",
        unit: "ratio",
        metric: METRIC_ERRORS,
        agg: Agg::ErrorRatio,
    },
    SeriesDef {
        name: "latency_p50",
        unit: "ms",
        metric: METRIC_DURATION,
        agg: Agg::Percentile(0.50),
    },
    SeriesDef {
        name: "latency_p95",
        unit: "ms",
        metric: METRIC_DURATION,
        agg: Agg::Percentile(0.95),
    },
    SeriesDef {
        name: "latency_p99",
        unit: "ms",
        metric: METRIC_DURATION,
        agg: Agg::Percentile(0.99),
    },
    SeriesDef {
        name: "cpu",
        unit: "cores",
        metric: METRIC_CPU,
        agg: Agg::Avg,
    },
    SeriesDef {
        name: "memory",
        unit: "mb",
        metric: METRIC_MEMORY,
        agg: Agg::Max,
    },
    SeriesDef {
        name: "instances",
        unit: "count",
        metric: METRIC_INSTANCES,
        agg: Agg::Max,
    },
    SeriesDef {
        name: "cold_starts",
        unit: "count",
        metric: METRIC_COLD_STARTS,
        agg: Agg::Sum,
    },
    // The database pooler (step 2 follow-up, D cards); no card writes it in step 3 yet.
    SeriesDef {
        name: "slow_queries",
        unit: "count",
        metric: "ruum.db.slow_queries",
        agg: Agg::Sum,
    },
    // Core Web Vitals from the browser script (G-2), the 75th percentile.
    SeriesDef {
        name: "lcp",
        unit: "ms",
        metric: "browser.web_vitals.lcp",
        agg: Agg::Percentile(0.75),
    },
    SeriesDef {
        name: "inp",
        unit: "ms",
        metric: "browser.web_vitals.inp",
        agg: Agg::Percentile(0.75),
    },
    SeriesDef {
        name: "cls",
        unit: "score",
        metric: "browser.web_vitals.cls",
        agg: Agg::Percentile(0.75),
    },
];

fn percentile(values: &mut Vec<f64>, p: f64) -> Option<f64> {
    values.retain(|v| v.is_finite());
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = (p * (values.len() - 1) as f64).round() as usize;
    values.get(rank.min(values.len() - 1)).copied()
}

fn sum(v: &[f64]) -> f64 {
    v.iter().filter(|x| x.is_finite()).sum()
}

fn aggregate(agg: Agg, values: &mut Vec<f64>, requests: &[f64]) -> Option<f64> {
    match agg {
        Agg::Sum => (!values.is_empty()).then(|| sum(values)),
        Agg::Max => values
            .iter()
            .copied()
            .filter(|v| v.is_finite())
            .reduce(f64::max),
        Agg::Avg => {
            let f: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
            (!f.is_empty()).then(|| f.iter().sum::<f64>() / f.len() as f64)
        }
        Agg::Percentile(p) => percentile(values, p),
        Agg::ErrorRatio => {
            let r = sum(requests);
            (r > 0.0).then(|| (sum(values) / r).clamp(0.0, 1.0))
        }
    }
}

/// One series of `AppMetrics`.
#[derive(Clone, Debug, Serialize)]
pub struct SeriesView {
    pub name: &'static str,
    pub unit: &'static str,
    pub points: Vec<(String, Option<f64>)>,
}

/// `AppMetrics`.
#[derive(Clone, Debug, Serialize)]
pub struct MetricsView {
    #[serde(with = "time::serde::rfc3339")]
    pub since: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub until: OffsetDateTime,
    pub step_seconds: i64,
    pub series: Vec<SeriesView>,
}

/// The step of a metrics read: `step` raised to a multiple of 60 seconds that gives at most
/// [`MAX_POINTS`] points over `range` seconds.
pub fn fit_step(range: i64, step: i64) -> i64 {
    let need = (range + MAX_POINTS - 1) / MAX_POINTS;
    if step >= need {
        step
    } else {
        (need + 59) / 60 * 60
    }
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// The series over `[since, until)` in steps of `step` seconds.
fn series_of(
    points: &[StoredMetric],
    since: OffsetDateTime,
    until: OffsetDateTime,
    step: i64,
) -> Vec<SeriesView> {
    let buckets = ((until - since).whole_seconds() + step - 1) / step;
    let buckets = buckets.clamp(0, MAX_POINTS) as usize;
    let index = |t: OffsetDateTime| -> Option<usize> {
        if t < since || t >= until {
            return None;
        }
        let i = ((t - since).whole_seconds() / step) as usize;
        (i < buckets).then_some(i)
    };
    let mut by_metric: HashMap<&str, Vec<Vec<f64>>> = HashMap::new();
    for p in points {
        if let Some(i) = index(p.time) {
            by_metric
                .entry(p.name.as_str())
                .or_insert_with(|| vec![Vec::new(); buckets])[i]
                .push(p.value);
        }
    }
    let requests = by_metric.get(METRIC_REQUESTS).cloned();
    SERIES
        .iter()
        .map(|d| {
            let points = match by_metric.get(d.metric) {
                None => Vec::new(),
                Some(per) => (0..buckets)
                    .map(|i| {
                        let mut v = per[i].clone();
                        let r = requests.as_ref().map(|r| r[i].as_slice()).unwrap_or(&[]);
                        let at = since + time::Duration::seconds(step * i as i64);
                        (rfc3339(at), aggregate(d.agg, &mut v, r))
                    })
                    .collect(),
            };
            SeriesView {
                name: d.name,
                unit: d.unit,
                points,
            }
        })
        .collect()
}

/// `AppMetricSummary`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SummaryView {
    pub requests: i64,
    pub error_rate: f64,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub cold_starts: i64,
    pub memory_mb_max: Option<f64>,
}

fn summary(points: &[StoredMetric]) -> SummaryView {
    let of = |m: &str| -> Vec<f64> {
        points
            .iter()
            .filter(|p| p.name == m && p.value.is_finite())
            .map(|p| p.value)
            .collect()
    };
    let requests = sum(&of(METRIC_REQUESTS));
    let errors = sum(&of(METRIC_ERRORS));
    let mut duration = of(METRIC_DURATION);
    SummaryView {
        requests: requests.max(0.0).min(i64::MAX as f64) as i64,
        error_rate: if requests > 0.0 {
            (errors / requests).clamp(0.0, 1.0)
        } else {
            0.0
        },
        p50_ms: percentile(&mut duration.clone(), 0.5),
        p95_ms: percentile(&mut duration, 0.95),
        cold_starts: sum(&of(METRIC_COLD_STARTS)).max(0.0).min(i64::MAX as f64) as i64,
        memory_mb_max: of(METRIC_MEMORY).into_iter().reduce(f64::max),
    }
}

/// `AppDeploymentComparison`.
#[derive(Clone, Debug, Serialize)]
pub struct ComparisonView {
    pub deployment_id: Uuid,
    pub previous_deployment_id: Option<Uuid>,
    pub window_minutes: i64,
    pub before: SummaryView,
    pub after: SummaryView,
    /// Empty until cards O-3 and X-1 (error groups and issues).
    pub new_issues: Vec<Value>,
}

// ---------------------------------------------------------------------------- cursors

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
}

fn bad_cursor() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::ValidationFailed,
            "the cursor is not of this app and filter",
        )
        .with_hint(
            "cursor: pass the next_cursor of the last call with the same filter, or no cursor",
        ),
    )
}

/// The position of a log page: bound to the tenant, the app and a hash of the filter.
#[derive(Serialize, Deserialize)]
struct LogCursor {
    v: u8,
    t: Uuid,
    a: Uuid,
    f: String,
    /// The newest time (Unix nanoseconds, inclusive) of the next page.
    u: i128,
    /// The records at exactly `u` already returned.
    s: usize,
}

impl LogCursor {
    /// The exclusive end of the next page's window and the records to skip at `u`. The cursor
    /// is not signed, so both numbers are checked (review O-2, 4): `u + 1` must be a time and
    /// `s` at most what one read returns; else `422`, never an overflow.
    fn position(&self) -> Result<(OffsetDateTime, usize)> {
        if self.s > lgtm::MAX_READ {
            return Err(bad_cursor());
        }
        let end = self.u.checked_add(1).ok_or_else(bad_cursor)?;
        let to = OffsetDateTime::from_unix_timestamp_nanos(end).map_err(|_| bad_cursor())?;
        Ok((to, self.s))
    }
}

fn encode_json<T: Serialize>(v: &T) -> Cursor {
    Cursor(b64().encode(serde_json::to_vec(v).expect("a cursor serialises")))
}

fn decode_json<T: for<'de> Deserialize<'de>>(c: &Cursor) -> Result<T> {
    let raw = b64().decode(c.0.as_bytes()).map_err(|_| bad_cursor())?;
    serde_json::from_slice(&raw).map_err(|_| bad_cursor())
}

/// The position of a field page.
#[derive(Serialize, Deserialize)]
struct FieldCursor {
    v: u8,
    t: Uuid,
    a: Uuid,
    u: bool,
    f: String,
    k: String,
}

// ---------------------------------------------------------------------------- requests

/// The filters of `searchAppLogs` after parsing.
#[derive(Clone, Debug, Default)]
pub struct LogQuery {
    pub q: Option<String>,
    pub level: Option<Severity>,
    pub origin: Option<LogOrigin>,
    pub deployment: Option<Uuid>,
    pub trace_id: Option<String>,
    pub since: Option<OffsetDateTime>,
    pub until: Option<OffsetDateTime>,
    pub page: PageRequest,
}

impl LogQuery {
    /// The hash of the filter that a cursor is bound to (everything but the page).
    fn hash(&self) -> String {
        let text = format!(
            "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
            self.q,
            self.level,
            self.origin,
            self.deployment,
            self.trace_id,
            self.since.map(|t| t.unix_timestamp_nanos()),
            self.until.map(|t| t.unix_timestamp_nanos()),
        );
        hex::encode(&Sha256::digest(text.as_bytes())[..12])
    }
}

/// The parameters of `getAppMetrics`.
#[derive(Clone, Copy, Debug, Default)]
pub struct MetricsQuery {
    pub since: Option<OffsetDateTime>,
    pub until: Option<OffsetDateTime>,
    pub step: Option<i64>,
    pub deployment: Option<Uuid>,
}

/// One entry of `confirmAppTelemetryFields`.
#[derive(Clone, Debug, Deserialize)]
pub struct ConfirmItem {
    pub field: String,
    pub kind: String,
    pub class: String,
}

fn invalid(message: &str, pointer: &str) -> Error {
    Error::Api(
        ApiError::new(ErrorCode::ValidationFailed, message)
            .with_hint(pointer.to_owned())
            .with_details(json!({ "pointer": pointer })),
    )
}

fn not_an_app() -> Error {
    Error::not_found("app")
}

// ---------------------------------------------------------------------------- the service

/// The telemetry of the services: the store, the field store and the pipeline that writes into
/// them, and the read operations of the API.
pub struct TelemetryService {
    store: Arc<dyn TelemetryStore>,
    fields: Arc<dyn TelemetryFieldStore>,
    cache: Arc<ClassCache>,
    seen: Arc<SeenFields>,
    keys: TenantKeys,
    usage: Arc<dyn UsageSink>,
    budget: ReadBudget,
    pseudonyms: PseudonymLimiter,
}

impl TelemetryService {
    pub fn new(
        store: Arc<dyn TelemetryStore>,
        fields: Arc<dyn TelemetryFieldStore>,
        keys: TenantKeys,
        usage: Arc<dyn UsageSink>,
    ) -> Self {
        let cache = Arc::new(ClassCache::new(Arc::new(StoreClasses::new(fields.clone()))));
        TelemetryService {
            seen: Arc::new(SeenFields::new(fields.clone())),
            store,
            fields,
            cache,
            keys,
            usage,
            budget: ReadBudget::new(BudgetLimits::default()),
            pseudonyms: PseudonymLimiter::new(PseudonymLimits::default()),
        }
    }

    /// The in-memory store and field store (the journal for `app.telemetry_fields_confirmed`).
    pub fn memory(
        keys: TenantKeys,
        usage: Arc<dyn UsageSink>,
        journal: Option<Arc<crate::journal::memory::MemoryJournal>>,
    ) -> Self {
        Self::new(
            Arc::new(memory::MemoryTelemetryStore::new()),
            Arc::new(MemoryFieldStore::new(journal)),
            keys,
            usage,
        )
    }

    /// The store the reads go to.
    pub fn store(&self) -> &Arc<dyn TelemetryStore> {
        &self.store
    }

    /// The field store.
    pub fn fields(&self) -> &Arc<dyn TelemetryFieldStore> {
        &self.fields
    }

    /// Where deployments write the suggestions of their privacy file: the field store, then
    /// the class cache of this service.
    pub fn suggestions(&self) -> Arc<dyn classes::PrivacySuggestions> {
        Arc::new(StoreSuggestions {
            store: self.fields.clone(),
            cache: self.cache.clone(),
        })
    }

    /// The class source of the pipeline: the field store behind the bounded cache.
    pub fn class_source(&self) -> Arc<dyn FieldClassSource> {
        self.cache.clone()
    }

    /// The pipeline over `store` with this service's classes, keys and meter, and the writer of
    /// the fields it sees (the processor's ingest).
    pub fn pipeline(
        &self,
        store: Arc<dyn TelemetryStore>,
        secrets: Arc<dyn SecretSource>,
    ) -> Arc<dyn TelemetryIngest> {
        let inner = Arc::new(PipelineIngest::new(
            store,
            self.cache.clone(),
            secrets,
            self.keys.clone(),
            self.usage.clone(),
        ));
        Arc::new(SeenFieldsIngest::new(inner, self.seen.clone()))
    }

    /// Runs the pipeline on a batch of `identity` and writes it to the store (the processor's
    /// path; the suites use it to put records in).
    pub async fn ingest(&self, identity: &SourceIdentity, batch: Batch) -> Result<IngestReport> {
        self.ingest_with_secrets(identity, batch, Vec::new()).await
    }

    /// [`TelemetryService::ingest`] with the secret values of the source (the app's secrets and
    /// the workload token) that the pipeline redacts.
    pub async fn ingest_with_secrets(
        &self,
        identity: &SourceIdentity,
        batch: Batch,
        secrets: Vec<SecretValue>,
    ) -> Result<IngestReport> {
        let source = memory::MemorySecretSource::new();
        source.set(identity.tenant, identity.app, secrets);
        self.pipeline(self.store.clone(), Arc::new(source))
            .ingest(identity, batch)
            .await
    }

    // ------------------------------------------------------------------------ access

    /// The app node, when the caller has `role` on it. Share links get `user_required`; a node
    /// the caller cannot see, of another tenant, or that is not an app is `404 not_found`
    /// (one body for all).
    async fn app(
        &self,
        s: &crate::Services,
        ctx: &Context,
        app: NodeId,
        role: Role,
    ) -> Result<Node> {
        if ctx.actor.actor.kind == crate::actor::PrincipalKind::ShareLink {
            return Err(Error::user_required());
        }
        let node = match s.nodes.get(ctx, app).await {
            Ok(n) => n,
            Err(Error::Api(e)) if e.code == ErrorCode::NotFound => return Err(not_an_app()),
            Err(e) => return Err(e),
        };
        if node.kind != NodeKind::Typed
            || node.node_type.as_deref() != Some(crate::apps::NODE_TYPE)
            || node.tenant_id != ctx.tenant
        {
            return Err(not_an_app());
        }
        authz::require(&*s.authz, &*s.nodes, ctx, app, role).await?;
        Ok(node)
    }

    /// The deployment `id` of `app`, or `404` exactly like a missing one.
    async fn deployment_of(
        &self,
        s: &crate::Services,
        ctx: &Context,
        app: NodeId,
        id: Uuid,
    ) -> Result<crate::apps::store::DeploymentRow> {
        s.apps
            .store()
            .get_deployment(ctx.tenant, app, id)
            .await?
            .filter(|d| d.app_node_id == app && d.tenant_id == ctx.tenant)
            .ok_or_else(|| Error::not_found("deployment"))
    }

    fn permit(&self, ctx: &Context) -> Result<ReadPermit> {
        self.budget.acquire(ctx.tenant, ctx.actor.actor.id.0)
    }

    // ------------------------------------------------------------------------ fields

    /// `listAppTelemetryFields` (`edit`).
    pub async fn list_fields(
        &self,
        s: &crate::Services,
        ctx: &Context,
        app: NodeId,
        unconfirmed: bool,
        page: PageRequest,
    ) -> Result<Page<FieldView>> {
        self.app(s, ctx, app, Role::Edit).await?;
        let after = match &page.cursor {
            None => None,
            Some(c) => {
                let fc: FieldCursor = decode_json(c)?;
                if fc.v != 1 || fc.t != ctx.tenant.0 || fc.a != app.0 || fc.u != unconfirmed {
                    return Err(bad_cursor());
                }
                Some((fc.f, parse_kind(&fc.k).ok_or_else(bad_cursor)?))
            }
        };
        let limit = page.limit() as usize;
        let mut rows = self
            .fields
            .page(ctx.tenant, app, unconfirmed, after, limit + 1)
            .await?;
        let more = rows.len() > limit;
        rows.truncate(limit);
        let next_cursor = match rows.last() {
            Some(last) if more => Some(encode_json(&FieldCursor {
                v: 1,
                t: ctx.tenant.0,
                a: app.0,
                u: unconfirmed,
                f: last.field.clone(),
                k: last.kind.as_str().to_owned(),
            })),
            _ => None,
        };
        Ok(Page {
            items: rows.iter().map(FieldView::from).collect(),
            next_cursor,
        })
    }

    /// `confirmAppTelemetryFields` (`manage`, users only). The rules: a metric label is only
    /// `normal`; a deny-listed name is only `secret`; a field is named once.
    pub async fn confirm_fields(
        &self,
        s: &crate::Services,
        ctx: &Context,
        app: NodeId,
        items: Vec<ConfirmItem>,
    ) -> Result<Vec<FieldView>> {
        let by = crate::actor::require_user_actor(ctx)?;
        self.app(s, ctx, app, Role::Manage).await?;
        if items.is_empty() || items.len() > classes::MAX_NAMED_FIELDS {
            return Err(invalid("fields has 1 to 2000 entries", "/fields"));
        }
        let mut list = Vec::with_capacity(items.len());
        let mut seen = std::collections::HashSet::new();
        for (i, it) in items.iter().enumerate() {
            if !classify::is_field_name(&it.field) {
                return Err(invalid(
                    "a field name is 1 to 200 letters, digits, _ and ., starting with a letter or _",
                    &format!("/fields/{i}/field"),
                ));
            }
            let kind = parse_kind(&it.kind).ok_or_else(|| {
                invalid(
                    "kind is log_field, metric_label or span_attribute",
                    &format!("/fields/{i}/kind"),
                )
            })?;
            let class = parse_class(&it.class).ok_or_else(|| {
                invalid(
                    "class is normal, personal or secret",
                    &format!("/fields/{i}/class"),
                )
            })?;
            if kind == FieldKind::MetricLabel && class != FieldClass::Normal {
                return Err(invalid(
                    "a metric label can only be normal",
                    &format!("/fields/{i}/class"),
                ));
            }
            if denylist::is_denied(&it.field) && class != FieldClass::Secret {
                return Err(invalid(
                    "this name is on the platform deny-list and is always secret",
                    &format!("/fields/{i}/class"),
                ));
            }
            if !seen.insert((it.field.clone(), kind)) {
                return Err(invalid("a field is named twice", &format!("/fields/{i}")));
            }
            list.push(Confirmation {
                field: it.field.clone(),
                kind,
                class,
            });
        }
        if !s.apps.is_app(ctx.tenant, app).await? {
            return Err(Error::Api(
                ApiError::new(ErrorCode::Conflict, "the app is still being set up")
                    .with_hint("try again in a few seconds"),
            ));
        }
        let (rows, _entry) = self.fields.confirm(ctx, by, app, &list).await?;
        self.cache.invalidate(ctx.tenant, app);
        Ok(rows.iter().map(FieldView::from).collect())
    }

    // ------------------------------------------------------------------------ pseudonym

    /// `computeAppPseudonym` (`edit`, users only, rate limited): the pseudonym the pipeline
    /// stores for `value` in a `personal` field of `app`'s live records (the same key and
    /// function; a preview has a key of its own, `source_telemetry_key`), or the
    /// hidden-value marker for a `secret` field. The value is never stored, logged or echoed;
    /// the audit log records the lookup with the field name.
    pub async fn pseudonym(
        &self,
        s: &crate::Services,
        ctx: &Context,
        app: NodeId,
        field: &str,
        value: &str,
    ) -> Result<Value> {
        let user = crate::actor::require_user_actor(ctx)?;
        self.app(s, ctx, app, Role::Edit).await?;
        if field.chars().count() > 200 || field.is_empty() {
            return Err(invalid("field has 1 to 200 characters", "/field"));
        }
        if value.chars().count() > 1000 {
            return Err(invalid("value has at most 1000 characters", "/value"));
        }
        self.pseudonyms.take(ctx.tenant, user.0, app)?;
        let classes = self.cache.classes(ctx.tenant, app).await?;
        let secret = [FieldKind::LogField, FieldKind::SpanAttribute]
            .iter()
            .any(|k| classify::effective_class(field, *k, &classes) == FieldClass::Secret);
        let answer = if secret {
            serde_json::to_value(crate::privacy::HiddenValue::SECRET)
                .map_err(|e| Error::Internal(format!("hidden marker: {e}")))?
        } else {
            let key = self.keys.tenant_key(ctx.tenant).await?;
            let p = crate::privacy::HmacPseudonymizer::new(app_telemetry_key(&key, app))
                .pseudonym(&SqlValue::Text(value));
            serde_json::to_value(p).map_err(|e| Error::Internal(format!("pseudonym: {e}")))?
        };
        s.audit
            .record(
                ctx,
                NewAuditEntry::new("app.pseudonym_computed")
                    .target("node", app)
                    .data(json!({ "field": field })),
            )
            .await?;
        Ok(answer)
    }

    // ------------------------------------------------------------------------ logs

    /// `searchAppLogs` (`edit`): one page, newest first.
    pub async fn search_logs(
        &self,
        s: &crate::Services,
        ctx: &Context,
        app: NodeId,
        q: LogQuery,
    ) -> Result<LogPage> {
        self.app(s, ctx, app, Role::Edit).await?;
        let words: Vec<String> = match &q.q {
            Some(text) if text.chars().count() > MAX_Q_CHARS => {
                return Err(invalid("q has at most 500 characters", "q"));
            }
            Some(text) => text.split_whitespace().map(str::to_owned).collect(),
            None => Vec::new(),
        };
        if let Some(t) = &q.trace_id
            && !is_trace_id(t)
        {
            return Err(invalid(
                "trace_id is 32 lower-case hex characters",
                "trace_id",
            ));
        }
        if let Some(d) = q.deployment {
            self.deployment_of(s, ctx, app, d).await?;
        }
        let until = q
            .until
            .unwrap_or_else(|| crate::now() + time::Duration::minutes(1));
        let since = q.since.unwrap_or(until - DEFAULT_LOG_WINDOW);
        if until <= since {
            return Err(invalid("until must be after since", "until"));
        }
        if until - since > MAX_WINDOW {
            return Err(invalid("the window is at most 7 days", "since"));
        }
        let hash = q.hash();
        let (top, skip) = match &q.page.cursor {
            None => (None, 0),
            Some(c) => {
                let lc: LogCursor = decode_json(c)?;
                if lc.v != 1 || lc.t != ctx.tenant.0 || lc.a != app.0 || lc.f != hash {
                    return Err(bad_cursor());
                }
                let (to, skip) = lc.position()?;
                (Some((lc.u, to)), skip)
            }
        };
        let limit = q.page.limit() as usize;
        let to = match top {
            Some((_, t)) => t.min(until),
            None => until,
        };
        let top = top.map(|(u, _)| u);
        let filter = TelemetryFilter {
            signal: Some(Signal::Logs),
            app: Some(app),
            deployment: q.deployment,
            from: Some(since),
            to: Some(to),
            min_severity: q.level,
            limit: Some((limit + skip + 1).min(lgtm::MAX_READ)),
            words,
            trace_id: q.trace_id.clone(),
            origin: q.origin,
            newest: true,
            ..Default::default()
        };
        let read = {
            let _permit = self.permit(ctx)?;
            self.store.read(ctx.tenant, &filter).await?
        };
        // Newest first; the records at the cursor's time that the last page gave are skipped.
        let mut logs: Vec<StoredLog> = read
            .logs
            .into_iter()
            .filter(|r| r.source.tenant == ctx.tenant && r.source.app == app)
            .collect();
        logs.reverse();
        let mut skipped = 0;
        if let Some(u) = top {
            logs.retain(|r| {
                if r.time.unix_timestamp_nanos() == u && skipped < skip {
                    skipped += 1;
                    false
                } else {
                    true
                }
            });
        }
        let more = logs.len() > limit;
        logs.truncate(limit);
        let next_cursor = match logs.last() {
            Some(last) if more => {
                let u = last.time.unix_timestamp_nanos();
                let mut s = logs
                    .iter()
                    .filter(|r| r.time.unix_timestamp_nanos() == u)
                    .count();
                if top == Some(u) {
                    s += skip;
                }
                Some(encode_json(&LogCursor {
                    v: 1,
                    t: ctx.tenant.0,
                    a: app.0,
                    f: hash,
                    u,
                    s,
                }))
            }
            _ => None,
        };
        Ok(LogPage {
            items: logs.iter().map(log_view).collect(),
            next_cursor,
            content_trust: "untrusted",
        })
    }

    // ------------------------------------------------------------------------ metrics

    async fn metric_points(
        &self,
        ctx: &Context,
        app: NodeId,
        deployment: Option<Uuid>,
        from: OffsetDateTime,
        to: OffsetDateTime,
    ) -> Result<Vec<StoredMetric>> {
        let filter = TelemetryFilter {
            signal: Some(Signal::Metrics),
            app: Some(app),
            deployment,
            from: Some(from),
            to: Some(to),
            limit: Some(lgtm::MAX_READ),
            ..Default::default()
        };
        let _permit = self.permit(ctx)?;
        Ok(self
            .store
            .read(ctx.tenant, &filter)
            .await?
            .metrics
            .into_iter()
            .filter(|m| m.source.tenant == ctx.tenant && m.source.app == app)
            .collect())
    }

    /// `getAppMetrics` (`edit`). At most 7 days; `step_seconds` is raised (to a multiple of 60)
    /// so a series has at most 500 points.
    pub async fn metrics(
        &self,
        s: &crate::Services,
        ctx: &Context,
        app: NodeId,
        q: MetricsQuery,
    ) -> Result<MetricsView> {
        let MetricsQuery {
            since,
            until,
            step,
            deployment,
        } = q;
        self.app(s, ctx, app, Role::Edit).await?;
        if let Some(d) = deployment {
            self.deployment_of(s, ctx, app, d).await?;
        }
        let step = step.unwrap_or(300);
        if !(60..=86_400).contains(&step) {
            return Err(invalid("step_seconds is 60 to 86400", "step_seconds"));
        }
        let until = until.unwrap_or_else(crate::now);
        let since = since.unwrap_or(until - DEFAULT_METRICS_WINDOW);
        if until <= since {
            return Err(invalid("until must be after since", "until"));
        }
        if until - since > MAX_WINDOW {
            return Err(invalid("the window is at most 7 days", "since"));
        }
        let step = fit_step((until - since).whole_seconds(), step);
        let points = self
            .metric_points(ctx, app, deployment, since, until)
            .await?;
        Ok(MetricsView {
            since,
            until,
            step_seconds: step,
            series: series_of(&points, since, until, step),
        })
    }

    /// `compareAppDeployment` (`edit`): the window after the deployment against the same window
    /// before it, from the metric series of `getAppMetrics`.
    pub async fn compare(
        &self,
        s: &crate::Services,
        ctx: &Context,
        app: NodeId,
        deployment: Uuid,
        window_minutes: Option<i64>,
    ) -> Result<ComparisonView> {
        self.app(s, ctx, app, Role::Edit).await?;
        let w = window_minutes.unwrap_or(60);
        if !(5..=1440).contains(&w) {
            return Err(invalid("window_minutes is 5 to 1440", "window_minutes"));
        }
        let dep = self.deployment_of(s, ctx, app, deployment).await?;
        let previous = s
            .apps
            .store()
            .list_deployments(
                ctx.tenant,
                app,
                Some(dep.environment),
                PageRequest {
                    cursor: None,
                    limit: Some(crate::paging::MAX_PAGE_SIZE),
                },
            )
            .await?
            .items
            .into_iter()
            .filter(|d| d.id != dep.id && d.created_at < dep.created_at)
            .max_by_key(|d| d.created_at)
            .map(|d| d.id);
        let t0 = match dep.environment {
            DeployEnvironment::Live => dep.published_at.unwrap_or(dep.created_at),
            _ => dep.created_at,
        };
        let window = time::Duration::minutes(w);
        let before = self.metric_points(ctx, app, None, t0 - window, t0).await?;
        let after = self.metric_points(ctx, app, None, t0, t0 + window).await?;
        Ok(ComparisonView {
            deployment_id: dep.id,
            previous_deployment_id: previous,
            window_minutes: w,
            before: summary(&before),
            after: summary(&after),
            new_issues: Vec::new(),
        })
    }

    // ------------------------------------------------------------------------ traces

    /// `getAppTrace` (`edit` on the app): the spans of the trace in this app and in other apps
    /// of the tenant. Another app's spans are in full only when the caller has `edit` on that
    /// app too, else one opaque span. A trace without a span of this app is `404`.
    pub async fn trace(
        &self,
        s: &crate::Services,
        ctx: &Context,
        app: NodeId,
        trace_id: &str,
    ) -> Result<TraceView> {
        let node = self.app(s, ctx, app, Role::Edit).await?;
        if !is_trace_id(trace_id) {
            return Err(invalid(
                "trace_id is 32 lower-case hex characters",
                "trace_id",
            ));
        }
        // Two reads: this app's spans first, under their own limit, then the other apps'. A
        // called app that writes many spans into the trace can then crowd out only the other
        // apps' spans, never this app's (review O-2, Kimi 4).
        let own = TelemetryFilter {
            signal: Some(Signal::Traces),
            app: Some(app),
            trace_id: Some(trace_id.to_owned()),
            limit: Some(lgtm::MAX_READ),
            ..Default::default()
        };
        let all = TelemetryFilter {
            app: None,
            ..own.clone()
        };
        let (mine, rest) = {
            let _permit = self.permit(ctx)?;
            let mine = self.store.read(ctx.tenant, &own).await?;
            let rest = self.store.read(ctx.tenant, &all).await?;
            (mine, rest)
        };
        let spans: Vec<StoredSpan> = mine
            .spans
            .into_iter()
            .filter(|sp| sp.source.app == app)
            .chain(rest.spans.into_iter().filter(|sp| sp.source.app != app))
            .filter(|sp| sp.source.tenant == ctx.tenant && sp.trace_id == trace_id)
            .collect();
        if !spans.iter().any(|sp| sp.source.app == app) {
            return Err(Error::not_found("trace"));
        }
        let mut by_app: BTreeMap<NodeId, Vec<&StoredSpan>> = BTreeMap::new();
        for sp in &spans {
            by_app.entry(sp.source.app).or_default().push(sp);
        }
        let mut out = Vec::new();
        for (other, list) in by_app {
            if other == app {
                out.extend(list.iter().map(|sp| span_view(sp, Some(node.name.clone()))));
                continue;
            }
            // The name only when the caller can see the node; the spans only with `edit`.
            let seen = match s.nodes.get(ctx, other).await {
                Ok(n) if n.tenant_id == ctx.tenant => Some(n),
                _ => None,
            };
            let name = seen.as_ref().map(|n| n.name.clone());
            let full = seen.is_some()
                && authz::require(&*s.authz, &*s.nodes, ctx, other, Role::Edit)
                    .await
                    .is_ok();
            if full {
                out.extend(list.iter().map(|sp| span_view(sp, name.clone())));
            } else if let Some(o) = opaque_span(&list, name) {
                out.push(o);
            }
        }
        out.sort_by(|a, b| {
            a.start
                .cmp(&b.start)
                .then_with(|| a.span_id.cmp(&b.span_id))
        });
        Ok(TraceView {
            trace_id: trace_id.to_owned(),
            spans: out,
        })
    }
}

/// Installs the telemetry of the services over `fields` and `store`: the service, and the
/// suggestions of deployments into the apps area. Call it after the apps area is installed.
pub fn install(
    services: &mut crate::Services,
    store: Arc<dyn TelemetryStore>,
    fields: Arc<dyn TelemetryFieldStore>,
) {
    let t = Arc::new(TelemetryService::new(
        store,
        fields,
        (*services.tenant_keys).clone(),
        services.apps.usage().clone(),
    ));
    services.apps.set_privacy_suggestions(t.suggestions());
    services.telemetry = t;
}

/// [`install`] with the Postgres field store over `pool`, keeping the current store.
pub fn install_postgres(services: &mut crate::Services, pool: sqlx::PgPool) {
    let store = services.telemetry.store().clone();
    install(services, store, Arc::new(PgFieldStore::new(pool)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_step_fits_500_points() {
        assert_eq!(fit_step(3600, 300), 300);
        let week = 7 * 86_400;
        let s = fit_step(week, 60);
        assert!(s * MAX_POINTS >= week && s % 60 == 0, "{s}");
        assert_eq!(fit_step(week, 86_400), 86_400);
    }

    #[test]
    fn messages_put_in_the_stored_values() {
        let p = crate::privacy::HmacPseudonymizer::new(PseudonymKey::from_bytes([7; 32]))
            .pseudonym(&SqlValue::Text("kati@example.test"));
        let fields = vec![
            StoredAttribute::text("order_id", "o-1"),
            StoredAttribute {
                key: "customer".into(),
                value: StoredValue::Pseudonym(p.clone()),
            },
            StoredAttribute {
                key: "pw".into(),
                value: StoredValue::Hidden(crate::privacy::HiddenValue::SECRET),
            },
        ];
        let m = render_message(
            "order {order_id} by {customer} {pw} {missing} {open",
            &fields,
        );
        let id = p.id().strip_prefix("p_").unwrap();
        assert_eq!(
            m,
            format!(
                "order o-1 by <person {}> <hidden> {{missing}} {{open",
                &id[..6]
            )
        );
    }

    #[test]
    fn the_budget_refuses_over_its_bounds() {
        let b = ReadBudget::new(BudgetLimits {
            global_concurrent: 3,
            tenant_concurrent: 2,
            principal_concurrent: 1,
            tenant_per_minute: 100,
            principal_per_minute: 3,
        });
        let (t1, t2, t3) = (TenantId::new(), TenantId::new(), TenantId::new());
        let (p1, p2) = (Uuid::now_v7(), Uuid::now_v7());
        let a = b.acquire(t1, p1).unwrap();
        assert!(
            b.acquire(t1, p1).is_err(),
            "one principal, one read at once"
        );
        let _c = b.acquire(t1, p2).unwrap();
        assert!(
            b.acquire(t1, Uuid::now_v7()).is_err(),
            "the tenant's share is full"
        );
        let _d = b.acquire(t2, p1).unwrap();
        let e = b
            .acquire(t3, p1)
            .err()
            .expect("the pool is full: the newcomer is refused");
        assert!(
            matches!(e, Error::Api(ref x) if x.code == ErrorCode::RateLimited && x.retry_after.is_some())
        );
        drop(a);
        let _f = b.acquire(t1, p1).unwrap();
        drop(_f);
        drop(b.acquire(t1, p1).unwrap());
        assert!(
            b.acquire(t1, p1).is_err(),
            "three reads a minute for one principal"
        );
    }

    #[test]
    fn the_pseudonym_limit_has_a_burst_and_an_hourly_cap() {
        let l = PseudonymLimiter::new(PseudonymLimits {
            per_minute: 3,
            per_hour: 5,
            tenant_per_hour: 7,
        });
        let (t, u, a) = (TenantId::new(), Uuid::now_v7(), NodeId::new());
        for _ in 0..3 {
            l.take(t, u, a).unwrap();
        }
        assert!(l.take(t, u, a).is_err(), "the burst");
        for _ in 0..3 {
            l.take(t, Uuid::now_v7(), a).unwrap();
        }
        let other = NodeId::new();
        l.take(t, u, other).unwrap();
        assert!(
            l.take(t, Uuid::now_v7(), other).is_err(),
            "the tenant's cap"
        );
    }

    #[test]
    fn a_log_cursor_round_trips_and_binds() {
        let c = encode_json(&LogCursor {
            v: 1,
            t: Uuid::nil(),
            a: Uuid::nil(),
            f: "x".into(),
            u: 5,
            s: 1,
        });
        let back: LogCursor = decode_json(&c).unwrap();
        assert_eq!((back.u, back.s), (5, 1));
        assert_eq!(back.position().unwrap().1, 1);
        // Review O-2, 4: forged numbers are `422`, never an overflow.
        for (u, s) in [
            (i128::MAX, 0),
            (i128::MIN, 0),
            (5, usize::MAX),
            (5, lgtm::MAX_READ + 1),
        ] {
            let forged = LogCursor {
                v: 1,
                t: Uuid::nil(),
                a: Uuid::nil(),
                f: "x".into(),
                u,
                s,
            };
            let back: LogCursor = decode_json(&encode_json(&forged)).unwrap();
            assert!(back.position().is_err(), "u {u} s {s}");
        }
        assert!(decode_json::<LogCursor>(&Cursor("not base64!".into())).is_err());
        let q1 = LogQuery {
            q: Some("a".into()),
            ..Default::default()
        };
        let q2 = LogQuery {
            q: Some("b".into()),
            ..Default::default()
        };
        assert_ne!(q1.hash(), q2.hash());
    }

    #[test]
    fn series_have_at_most_500_points_and_sources() {
        let since = crate::now() - time::Duration::days(7);
        let until = crate::now();
        let step = fit_step((until - since).whole_seconds(), 60);
        let id = SourceIdentity {
            tenant: TenantId::new(),
            app: NodeId::new(),
            deployment: Uuid::now_v7(),
            environment: Environment::Live,
        };
        let p = |name: &str, v: f64| StoredMetric {
            source: id,
            time: until - time::Duration::minutes(5),
            name: name.into(),
            kind: MetricKind::Counter,
            value: v,
            labels: vec![],
        };
        let s = series_of(
            &[
                p(METRIC_REQUESTS, 4.0),
                p(METRIC_ERRORS, 1.0),
                p(METRIC_DURATION, 10.0),
            ],
            since,
            until,
            step,
        );
        assert_eq!(s.len(), SERIES.len());
        for x in &s {
            assert!(x.points.len() <= 500, "{}", x.name);
        }
        let errors = s.iter().find(|x| x.name == "errors").unwrap();
        assert!(errors.points.iter().any(|p| p.1 == Some(0.25)));
        let cpu = s.iter().find(|x| x.name == "cpu").unwrap();
        assert!(cpu.points.is_empty(), "a series without a source is empty");
    }
}
