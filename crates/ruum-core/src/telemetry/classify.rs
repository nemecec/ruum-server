//! Applying the classes: the pipeline function [`process`] and the class precedence
//! [`effective_class`] (design 8.5 and 9.6, review R6).
//!
//! # The class of a field
//!
//! 1. A deny-listed name is `secret` ([`super::denylist`]), whatever the classes say.
//! 2. A field the source does not know is `personal`.
//! 3. A field that no person confirmed is at least `personal` (its listed class counts only
//!    when it is stricter).
//! 4. A suggestion stricter than the class from 2 or 3 applies at once (`secret` over `normal`
//!    or `personal`, `personal` over `normal`); a looser suggestion waits for a person.
//! 5. A free-text attribute ([`FREE_TEXT_ATTRIBUTES`]) is at least `personal`, also when a
//!    person confirmed it as `normal`: free text is `personal` and scrubbed (card O-1a, pipeline
//!    step 4), like the log text, the span name and the event name, whose class nobody can
//!    relax either.
//!
//! # What a class does to a value
//!
//! - `secret`: the hidden-value marker (`{"$ruum": "hidden", "class": "secret"}`).
//! - `personal`: a pseudonym with the per-app key; a free-text attribute
//!   ([`FREE_TEXT_ATTRIBUTES`]) is scrubbed with patterns instead.
//! - `normal`: kept.
//! - Metric labels may only be `normal`: a label of any other effective class (unclassified
//!   included) is dropped, never pseudonymised (pseudonyms would explode the series and still
//!   link people).
//!
//! The secret matcher runs on every value before all of this: a value that holds a secret is
//! the hidden-value marker whatever its class (a metric label is dropped).

use super::scrub;
use super::secrets::SecretMatcher;
use super::*;
use crate::privacy::{Pseudonymizer, SqlValue, canonical_text};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::time::{Duration, Instant};

/// Attributes whose value is free text (OTel semantic conventions): a `personal` value is
/// scrubbed with patterns instead of becoming one pseudonym. Their class is never looser than
/// `personal` ([`effective_class`]).
pub const FREE_TEXT_ATTRIBUTES: &[&str] = &["exception.message", "exception.stacktrace"];

/// Whether a key is a free-text attribute ([`FREE_TEXT_ATTRIBUTES`], any case).
pub fn is_free_text_attribute(key: &str) -> bool {
    FREE_TEXT_ATTRIBUTES
        .iter()
        .any(|n| n.eq_ignore_ascii_case(key))
}

/// The effective class of a field (the rules of the module docs).
pub fn effective_class(name: &str, kind: FieldKind, classes: &FieldClasses) -> FieldClass {
    if denylist::is_denied(name) {
        return FieldClass::Secret;
    }
    let Some(entry) = classes.get(kind, name) else {
        return FieldClass::Personal;
    };
    let base = if entry.confirmed {
        entry.class
    } else {
        entry.class.max(FieldClass::Personal)
    };
    let class = match entry.suggested {
        Some(s) if s > base => s,
        _ => base,
    };
    // Free text is at least personal (rule 5), whatever a person confirmed.
    if is_free_text_attribute(name) {
        class.max(FieldClass::Personal)
    } else {
        class
    }
}

/// Whether a name is valid for `telemetry_fields` (`^[A-Za-z_][A-Za-z0-9_.]{0,199}$`).
pub fn is_field_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 200
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b[1..]
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || c == b'_' || c == b'.')
}

/// Whether a name may be stored as an attribute key: a field name, also with `-` (HTTP header
/// attributes such as `http.request.header.x-api-key`). Other keys are dropped: a key is
/// attacker text too.
pub fn is_attribute_key(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 200
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b[1..]
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
}

/// Whether a metric name has the OTel instrument name syntax
/// (`^[A-Za-z][A-Za-z0-9_.\-/]{0,254}$`). Other names drop the record: a name is attacker text,
/// and the stores and query languages of card O-1b and O-2 must not have to escape it.
pub fn is_metric_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 255
        && b[0].is_ascii_alphabetic()
        && b[1..]
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-' | b'/'))
}

/// Whether a name (an attribute key or a metric name) holds a match of a free-text pattern
/// (an email, IBAN, card or phone number, a token): such a name is dropped, because names are
/// stored verbatim and become series names.
pub fn name_has_pattern(name: &str) -> bool {
    !scrub::find(name).is_empty()
}

/// Whether a key is in the reserved namespace `ruum.*` (any case).
pub fn is_reserved(key: &str) -> bool {
    key.len() >= RESERVED_PREFIX.len()
        && key.as_bytes()[..RESERVED_PREFIX.len()].eq_ignore_ascii_case(RESERVED_PREFIX.as_bytes())
}

/// The identity attributes that the pipeline writes.
pub fn identity_attributes(identity: &SourceIdentity) -> Vec<StoredAttribute> {
    vec![
        StoredAttribute::text(ATTR_TENANT, identity.tenant.to_string()),
        StoredAttribute::text(ATTR_APP, identity.app.to_string()),
        StoredAttribute::text(ATTR_DEPLOYMENT, identity.deployment.to_string()),
        StoredAttribute::text(ATTR_ENVIRONMENT, identity.environment.as_str()),
    ]
}

/// The pipeline: a plain function of the batch, the classes, the key (in the pseudonymizer)
/// and the secret matcher. Returns the records to store and the report.
pub fn process(
    identity: &SourceIdentity,
    batch: Batch,
    classes: &FieldClasses,
    pseudonymizer: &HmacPseudonymizer,
    secrets: &SecretMatcher,
    limits: &Limits,
) -> (ClassifiedBatch, IngestReport) {
    let mut p = Pipeline {
        identity,
        classes,
        pseudonymizer,
        secrets,
        limits,
        report: IngestReport::default(),
    };
    let mut out = ClassifiedBatch::default();
    let mut budget = Budget {
        records: 0,
        bytes: 0,
    };
    for rec in batch.logs {
        if p.admit(&mut budget, log_size(&rec), log_values(&rec))
            && let Some(s) = p.log(rec)
        {
            p.report.stored_logs += 1;
            out.logs.push(s);
        }
    }
    for rec in batch.metrics {
        if p.admit(&mut budget, metric_size(&rec), rec.labels.len())
            && let Some(s) = p.metric(rec)
        {
            p.report.stored_metrics += 1;
            out.metrics.push(s);
        }
    }
    for rec in batch.spans {
        if p.admit(&mut budget, span_size(&rec), span_values(&rec))
            && let Some(s) = p.span(rec)
        {
            p.report.stored_spans += 1;
            out.spans.push(s);
        }
    }
    (out, p.report)
}

struct Budget {
    records: usize,
    bytes: usize,
}

struct Pipeline<'a> {
    identity: &'a SourceIdentity,
    classes: &'a FieldClasses,
    pseudonymizer: &'a HmacPseudonymizer,
    secrets: &'a SecretMatcher,
    limits: &'a Limits,
    report: IngestReport,
}

/// The bytes the batch size cap counts for each value (attribute, field, label, event, link)
/// on top of its key and text: about the size of one stored pseudonym attribute in memory
/// and in JSON. So the batch byte cap also bounds the output; an empty value never costs
/// nothing (review K4: 256 empty fields per record made about 50 times the input).
pub const VALUE_OVERHEAD: usize = 64;

/// The bytes the batch size cap counts for each record on top of its values: the record frame
/// and the four identity attributes the pipeline adds.
pub const RECORD_OVERHEAD: usize = 256;

fn attrs_size(a: &[Attribute]) -> usize {
    a.iter()
        .map(|a| VALUE_OVERHEAD + a.key.len() + a.value.size())
        .sum()
}

fn attrs_max(a: &[Attribute]) -> usize {
    a.iter()
        .map(|a| a.key.len().max(a.value.size()))
        .max()
        .unwrap_or(0)
}

fn log_size(r: &LogRecord) -> usize {
    RECORD_OVERHEAD
        + attrs_size(&r.attributes)
        + match &r.body {
            LogBody::Template { template, fields } => template.len() + attrs_size(fields),
            LogBody::Text(t) => t.len(),
        }
}

fn log_values(r: &LogRecord) -> usize {
    r.attributes.len()
        + match &r.body {
            LogBody::Template { fields, .. } => fields.len(),
            LogBody::Text(_) => 0,
        }
}

fn log_longest(r: &LogRecord) -> usize {
    attrs_max(&r.attributes).max(match &r.body {
        LogBody::Template { template, fields } => template.len().max(attrs_max(fields)),
        LogBody::Text(t) => t.len(),
    })
}

fn metric_size(r: &MetricPoint) -> usize {
    RECORD_OVERHEAD + r.name.len() + attrs_size(&r.labels)
}

fn span_size(r: &Span) -> usize {
    RECORD_OVERHEAD
        + r.trace_id.len()
        + r.span_id.len()
        + r.name.len()
        + r.status_message.as_ref().map_or(0, String::len)
        + attrs_size(&r.attributes)
        + r.events
            .iter()
            .map(|e| VALUE_OVERHEAD + e.name.len() + attrs_size(&e.attributes))
            .sum::<usize>()
        + r.links
            .iter()
            .map(|l| {
                VALUE_OVERHEAD + l.trace_id.len() + l.span_id.len() + attrs_size(&l.attributes)
            })
            .sum::<usize>()
}

fn span_values(r: &Span) -> usize {
    r.attributes.len()
        + r.events
            .iter()
            .map(|e| 1 + e.attributes.len())
            .sum::<usize>()
        + r.links
            .iter()
            .map(|l| 1 + l.attributes.len())
            .sum::<usize>()
}

fn span_longest(r: &Span) -> usize {
    let mut m = r
        .name
        .len()
        .max(r.status_message.as_ref().map_or(0, String::len))
        .max(attrs_max(&r.attributes))
        .max(r.parent_span_id.as_ref().map_or(0, String::len));
    for e in &r.events {
        m = m.max(e.name.len()).max(attrs_max(&e.attributes));
    }
    for l in &r.links {
        m = m.max(attrs_max(&l.attributes));
    }
    m
}

/// `len` lowercase hex characters, not all zero (OTel trace and span IDs).
fn valid_id(id: &str, len: usize) -> bool {
    id.len() == len
        && id
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        && id.bytes().any(|c| c != b'0')
}

impl Pipeline<'_> {
    /// The batch caps: the record count and the byte size, then the field count.
    fn admit(&mut self, budget: &mut Budget, size: usize, values: usize) -> bool {
        if budget.records >= self.limits.max_records {
            self.report.drop(DropReason::TooManyRecords);
            return false;
        }
        if budget.bytes + size > self.limits.max_batch_bytes {
            self.report.drop(DropReason::BatchTooLarge);
            return false;
        }
        budget.records += 1;
        budget.bytes += size;
        if values > self.limits.max_fields {
            self.report.drop(DropReason::TooManyFields);
            return false;
        }
        true
    }

    fn too_long(&mut self, longest: usize) -> bool {
        if longest > self.limits.max_value_bytes {
            self.report.drop(DropReason::ValueTooLong);
            return true;
        }
        false
    }

    /// One value of a field, attribute or label. `None` drops it.
    fn value(&mut self, kind: FieldKind, key: &str, value: &AttrValue) -> Option<StoredValue> {
        // 1. Secrets first: before the classes, the pseudonym and the raw copy. Numbers and
        // bools are matched by their text (a secret of digits only can arrive as an `Int`).
        if self.holds_secret(value) {
            self.report.secrets_redacted += 1;
            if kind == FieldKind::MetricLabel {
                self.report.labels_dropped += 1;
                return None;
            }
            return Some(StoredValue::Hidden(HiddenValue::SECRET));
        }
        // 2 and 3. The deny-list and the classes.
        let class = effective_class(key, kind, self.classes);
        self.apply_class(kind, key, class, value)
    }

    /// Whether the secret matcher finds a secret in the text of a value: a string as it is,
    /// an `Int` in decimal, a `Float` in its canonical text and in Rust's shortest form, a
    /// `Bool` as `true` or `false`.
    fn holds_secret(&self, value: &AttrValue) -> bool {
        match value {
            AttrValue::Str(s) => self.secrets.contains(s),
            AttrValue::Int(i) => self.secrets.contains(&i.to_string()),
            AttrValue::Float(f) => {
                self.secrets
                    .contains(&canonical_text(&SqlValue::Float8(*f)))
                    || self.secrets.contains(&f.to_string())
            }
            AttrValue::Bool(b) => self.secrets.contains(if *b { "true" } else { "false" }),
        }
    }

    fn apply_class(
        &mut self,
        kind: FieldKind,
        key: &str,
        class: FieldClass,
        value: &AttrValue,
    ) -> Option<StoredValue> {
        if kind == FieldKind::MetricLabel && class != FieldClass::Normal {
            self.report.labels_dropped += 1;
            return None;
        }
        match class {
            FieldClass::Secret => {
                self.report.hidden += 1;
                Some(StoredValue::Hidden(HiddenValue::SECRET))
            }
            FieldClass::Personal => {
                // RAW COPY HOOK (card O-3): the encrypted raw copy of a `personal` value, per
                // end user, goes here, after the secret matcher. Nothing keeps the raw value
                // in card O-1a.
                if let AttrValue::Str(s) = value
                    && is_free_text_attribute(key)
                {
                    return Some(StoredValue::Text(self.free_text(s)));
                }
                Some(self.pseudonym(value))
            }
            FieldClass::Normal => Some(match value {
                AttrValue::Str(s) => StoredValue::Text(s.clone()),
                AttrValue::Int(i) => StoredValue::Int(*i),
                AttrValue::Float(f) => StoredValue::Float(*f),
                AttrValue::Bool(b) => StoredValue::Bool(*b),
            }),
        }
    }

    /// The pseudonym of a value; the canonical text follows `privacy::canonical_text`.
    fn pseudonym(&mut self, value: &AttrValue) -> StoredValue {
        self.report.pseudonymized += 1;
        let p = match value {
            AttrValue::Str(s) => self.pseudonymizer.pseudonym(&SqlValue::Text(s)),
            AttrValue::Int(i) => self.pseudonymizer.pseudonym(&SqlValue::Int(*i)),
            AttrValue::Float(f) => self.pseudonymizer.pseudonym(&SqlValue::Float8(*f)),
            AttrValue::Bool(b) => self.pseudonymizer.pseudonym(&SqlValue::Bool(*b)),
        };
        StoredValue::Pseudonym(p)
    }

    /// Free text: secrets first, then the patterns, then the secrets once more. A replacement
    /// of the scrubber (`<hidden secret>`, `<person ...>`) can build a secret again, or be one
    /// (Kimi review of run 12, finding 2); the second pass catches that, and its result holds
    /// no secret ([`SecretMatcher::redact`]).
    fn free_text(&mut self, text: &str) -> String {
        let text = match self.secrets.redact(text) {
            Some(r) => {
                self.report.secrets_redacted += 1;
                r
            }
            None => text.to_owned(),
        };
        let (out, counts) = scrub::scrub(&text, self.pseudonymizer);
        self.report.hidden += counts.hidden;
        self.report.pseudonymized += counts.pseudonymized;
        match self.secrets.redact(&out) {
            Some(r) => {
                self.report.secrets_redacted += 1;
                r
            }
            None => out,
        }
    }

    /// The attributes of a record: `ruum.*` and invalid keys dropped, keys with a secret or a
    /// free-text pattern (a card number, an email) dropped, each value through
    /// [`Self::value`].
    fn attributes(&mut self, kind: FieldKind, attrs: Vec<Attribute>) -> Vec<StoredAttribute> {
        let mut out = Vec::with_capacity(attrs.len());
        for a in attrs {
            if is_reserved(&a.key) {
                self.report.reserved_dropped += 1;
                continue;
            }
            if !is_attribute_key(&a.key) {
                self.report.invalid_names_dropped += 1;
                continue;
            }
            if self.secrets.contains(&a.key) {
                self.report.secrets_redacted += 1;
                continue;
            }
            if name_has_pattern(&a.key) {
                self.report.invalid_names_dropped += 1;
                continue;
            }
            if self.classes.get(kind, &a.key).is_none() && is_field_name(&a.key) {
                self.report.unknown_fields.insert((kind, a.key.clone()));
            }
            if let Some(v) = self.value(kind, &a.key, &a.value) {
                out.push(StoredAttribute {
                    key: a.key,
                    value: v,
                });
            }
        }
        out
    }

    fn log(&mut self, rec: LogRecord) -> Option<StoredLog> {
        if self.too_long(log_longest(&rec)) {
            return None;
        }
        let (template, text, fields) = match rec.body {
            LogBody::Template { template, fields } => (
                Some(self.free_text(&template)),
                None,
                self.attributes(FieldKind::LogField, fields),
            ),
            LogBody::Text(t) => (None, Some(self.free_text(&t)), Vec::new()),
        };
        let mut attributes = self.attributes(FieldKind::LogField, rec.attributes);
        attributes.extend(identity_attributes(self.identity));
        Some(StoredLog {
            source: *self.identity,
            time: rec.time,
            severity: rec.severity,
            template,
            text,
            fields,
            attributes,
        })
    }

    fn metric(&mut self, rec: MetricPoint) -> Option<StoredMetric> {
        if self.too_long(rec.name.len().max(attrs_max(&rec.labels))) {
            return None;
        }
        if self.secrets.contains(&rec.name) {
            self.report.secrets_redacted += 1;
            self.report.drop(DropReason::SecretInIdentifier);
            return None;
        }
        if !is_metric_name(&rec.name) {
            self.report.drop(DropReason::InvalidName);
            return None;
        }
        if name_has_pattern(&rec.name) {
            self.report.drop(DropReason::PatternInName);
            return None;
        }
        // The point value is a number an app chose: a secret of digits only can arrive here
        // too. A number cannot hold the marker, so the point is dropped.
        if self.holds_secret(&AttrValue::Float(rec.value)) {
            self.report.secrets_redacted += 1;
            self.report.drop(DropReason::SecretInValue);
            return None;
        }
        let mut labels = self.attributes(FieldKind::MetricLabel, rec.labels);
        labels.extend(identity_attributes(self.identity));
        Some(StoredMetric {
            source: *self.identity,
            time: rec.time,
            name: rec.name,
            kind: rec.kind,
            value: rec.value,
            labels,
        })
    }

    fn span(&mut self, rec: Span) -> Option<StoredSpan> {
        if self.too_long(span_longest(&rec)) {
            return None;
        }
        let ids_ok = valid_id(&rec.trace_id, 32)
            && valid_id(&rec.span_id, 16)
            && rec.parent_span_id.as_ref().is_none_or(|p| valid_id(p, 16))
            && rec
                .links
                .iter()
                .all(|l| valid_id(&l.trace_id, 32) && valid_id(&l.span_id, 16));
        if !ids_ok {
            self.report.drop(DropReason::InvalidId);
            return None;
        }
        let ids_secret = self.secrets.contains(&rec.trace_id)
            || self.secrets.contains(&rec.span_id)
            || rec
                .parent_span_id
                .as_ref()
                .is_some_and(|p| self.secrets.contains(p))
            || rec
                .links
                .iter()
                .any(|l| self.secrets.contains(&l.trace_id) || self.secrets.contains(&l.span_id));
        if ids_secret {
            self.report.secrets_redacted += 1;
            self.report.drop(DropReason::SecretInIdentifier);
            return None;
        }
        let name = self.free_text(&rec.name);
        let status_message = rec.status_message.map(|m| self.free_text(&m));
        let mut attributes = self.attributes(FieldKind::SpanAttribute, rec.attributes);
        attributes.extend(identity_attributes(self.identity));
        let events = rec
            .events
            .into_iter()
            .map(|e| StoredEvent {
                time: e.time,
                name: self.free_text(&e.name),
                attributes: self.attributes(FieldKind::SpanAttribute, e.attributes),
            })
            .collect();
        let links = rec
            .links
            .into_iter()
            .map(|l| StoredLink {
                trace_id: l.trace_id,
                span_id: l.span_id,
                attributes: self.attributes(FieldKind::SpanAttribute, l.attributes),
            })
            .collect();
        Some(StoredSpan {
            source: *self.identity,
            trace_id: rec.trace_id,
            span_id: rec.span_id,
            parent_span_id: rec.parent_span_id,
            name,
            kind: rec.kind,
            start: rec.start,
            end: rec.end,
            status: rec.status,
            status_message,
            attributes,
            events,
            links,
        })
    }
}

// ---------------------------------------------------------------------------- cache

/// A short cache in front of a [`FieldClassSource`]: the classes of an app are read at most
/// once per `ttl`. A stricter class reaches the pipeline within `ttl` of the first read that
/// starts after the change. An entry's age counts from the start of its read. Each read takes
/// a number from one counter when it starts, and a read never replaces an entry of a read
/// with a higher number, so a slow read of the old classes that ends after a newer read cannot
/// keep them (Kimi review of run 12, finding 4). The order uses the counter, not the time: two
/// reads can start at the same `Instant` (Kimi's second review of the fix).
pub struct CachedFieldClasses {
    inner: Arc<dyn FieldClassSource>,
    ttl: Duration,
    cache: Mutex<HashMap<(TenantId, NodeId), CachedRead>>,
    next_read: AtomicU64,
    clock: fn() -> Instant,
}

/// When the read of the classes started, its number, and the classes.
type CachedRead = (Instant, u64, Arc<FieldClasses>);

impl CachedFieldClasses {
    /// The default time a read stays valid.
    pub const DEFAULT_TTL: Duration = Duration::from_secs(5);

    pub fn new(inner: Arc<dyn FieldClassSource>, ttl: Duration) -> Self {
        Self::with_clock(inner, ttl, Instant::now)
    }

    /// With another clock (tests: a clock that gives the same `Instant` to every read).
    pub(crate) fn with_clock(
        inner: Arc<dyn FieldClassSource>,
        ttl: Duration,
        clock: fn() -> Instant,
    ) -> Self {
        CachedFieldClasses {
            inner,
            ttl,
            cache: Mutex::new(HashMap::new()),
            next_read: AtomicU64::new(0),
            clock,
        }
    }

    fn fresh(&self, at: Instant) -> bool {
        (self.clock)().saturating_duration_since(at) < self.ttl
    }
}

#[async_trait]
impl FieldClassSource for CachedFieldClasses {
    async fn classes(&self, tenant: TenantId, app: NodeId) -> Result<Arc<FieldClasses>> {
        {
            let cache = self.cache.lock().expect("class cache lock");
            if let Some((at, _, c)) = cache.get(&(tenant, app))
                && self.fresh(*at)
            {
                return Ok(c.clone());
            }
        }
        let started = (self.clock)();
        let number = self.next_read.fetch_add(1, AtomicOrdering::SeqCst);
        let fresh = self.inner.classes(tenant, app).await?;
        let mut cache = self.cache.lock().expect("class cache lock");
        cache.retain(|_, (at, _, _)| self.fresh(*at));
        match cache.get(&(tenant, app)) {
            // A read that started later already stored its classes: they are newer.
            Some((_, n, newer)) if *n > number => Ok(newer.clone()),
            _ => {
                cache.insert((tenant, app), (started, number, fresh.clone()));
                Ok(fresh)
            }
        }
    }
}
