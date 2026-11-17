//! The telemetry privacy suite: the same cases for every [`TelemetryIngest`] and
//! [`TelemetryStore`] pair. The in-memory store passes it here (`tests.rs`); card O-1b runs
//! [`run`] unchanged against the real store.
//!
//! The suite sees the pair only through the traits and a [`PrivacyFixture`]. Every case makes
//! its own tenants and apps, so the cases share one store.
//!
//! **No case passes on silence**: each case first checks that its records were stored (a case
//! whose ingest stored nothing fails), then checks what they hold.
//!
//! The secret scan ([`scan_for_secrets`]) has its own encoders, on purpose separate from the
//! secret matcher, so a matcher that misses an encoding fails the scan. Later cards reuse it.
//!
//! The fixture secrets look fake and are put together at run time, so no source line matches a
//! gitleaks rule.

use super::*;
use crate::privacy::Pseudonym;
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------- fixture

/// What the suite needs beyond the traits.
#[async_trait]
pub trait PrivacyFixture: Send + Sync {
    fn ingest(&self) -> Arc<dyn TelemetryIngest>;
    fn store(&self) -> Arc<dyn TelemetryStore>;
    /// Sets the class of one field of an app (a `telemetry_fields` row).
    async fn set_class(
        &self,
        tenant: TenantId,
        app: NodeId,
        kind: FieldKind,
        name: &str,
        entry: FieldClassEntry,
    );
    /// Sets the secret values of an app (current, previous, workload token).
    async fn set_secrets(&self, tenant: TenantId, app: NodeId, values: Vec<SecretValue>);
    /// Everything the store holds, serialised (any format that keeps the stored text, such as
    /// JSON).
    async fn dump(&self) -> std::result::Result<Vec<u8>, String>;
}

/// The result of one case.
#[derive(Clone, Debug)]
pub struct CaseResult {
    pub name: String,
    pub outcome: std::result::Result<(), String>,
}

/// The results of a run.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub cases: Vec<CaseResult>,
}

impl Report {
    fn add(&mut self, name: impl Into<String>, outcome: std::result::Result<(), String>) {
        self.cases.push(CaseResult {
            name: name.into(),
            outcome,
        });
    }

    pub fn failures(&self) -> Vec<&CaseResult> {
        self.cases.iter().filter(|c| c.outcome.is_err()).collect()
    }

    /// Panics with every failed case.
    pub fn assert_ok(&self) {
        let failed: Vec<String> = self
            .failures()
            .iter()
            .map(|c| format!("{}: {}", c.name, c.outcome.as_ref().err().unwrap()))
            .collect();
        assert!(
            failed.is_empty(),
            "telemetry privacy suite: {} of {} cases failed:\n{}",
            failed.len(),
            self.cases.len(),
            failed.join("\n")
        );
    }
}

type CaseResultT = std::result::Result<(), String>;

/// Runs every case.
pub async fn run(f: &dyn PrivacyFixture) -> Report {
    let mut r = Report::default();
    for s in [Signal::Logs, Signal::Metrics, Signal::Traces] {
        let n = signal_name(s);
        r.add(format!("no_raw_secret_{n}"), no_raw_secret(f, s).await);
        r.add(format!("deny_list_{n}"), deny_list(f, s).await);
        r.add(format!("suggestions_{n}"), suggestions(f, s).await);
        r.add(format!("pseudonyms_{n}"), pseudonyms(f, s).await);
        r.add(format!("unclassified_{n}"), unclassified(f, s).await);
        r.add(format!("forged_identity_{n}"), forged_identity(f, s).await);
        r.add(
            format!("tenant_isolation_{n}"),
            tenant_isolation(f, s).await,
        );
        r.add(format!("numeric_secret_{n}"), numeric_secret(f, s).await);
        r.add(
            format!("personal_data_in_keys_{n}"),
            personal_data_in_keys(f, s).await,
        );
        r.add(
            format!("free_text_attributes_confirmed_normal_{n}"),
            free_text_confirmed_normal(f, s).await,
        );
    }
    r.add("metric_labels_dropped", metric_labels(f).await);
    r.add("metric_names_checked", metric_names(f).await);
    r.add("free_text_patterns_logs", free_text_logs(f).await);
    r.add("free_text_patterns_traces", free_text_traces(f).await);
    r.add("free_text_time_bound", free_text_time_bound(f).await);
    r.add("whole_store_scan", whole_store_scan(f).await);
    r
}

fn signal_name(s: Signal) -> &'static str {
    match s {
        Signal::Logs => "logs",
        Signal::Metrics => "metrics",
        Signal::Traces => "traces",
    }
}

fn kind_of(s: Signal) -> FieldKind {
    match s {
        Signal::Logs => FieldKind::LogField,
        Signal::Metrics => FieldKind::MetricLabel,
        Signal::Traces => FieldKind::SpanAttribute,
    }
}

// ---------------------------------------------------------------------------- secrets

/// The fixture secrets: an app secret (with `/`, `+` and a space, so every encoding differs),
/// a previous value (with `"` and `\`, for JSON escaping) and a workload token.
pub fn fixture_secrets() -> Vec<String> {
    vec![
        ["Fx7pQ2/wire", "+9Lm3 Zt8v-k5Rb"].concat(),
        ["prev\"Qt\\Sl-4Hn8", "-Wc2m-Jy6d"].concat(),
        ["wltok.7Kd2Pq9X", "r4Vs1Mz8Bn3"].concat(),
    ]
}

/// A secret of digits only (a numeric PIN or client secret), which an app can log as a number.
pub fn fixture_numeric_secret() -> String {
    ["7305", "9182", "64"].concat()
}

/// A token of a known prefix that no matcher knows (the scrubber must catch it in free text).
pub fn fixture_live_key() -> String {
    ["sk_", "live_", "Ruum0Fixture0Only0Not0Real"].concat()
}

fn fixture_secret_values() -> Vec<SecretValue> {
    fixture_secrets()
        .into_iter()
        .map(SecretValue::new)
        .collect()
}

/// The forms of a value that the scan looks for: raw, base64 (standard and URL-safe, the
/// characters that only the value decides at each of the three alignments), hex (both cases),
/// percent-encoding (both cases, and `+` for a space; also the partial forms of JS
/// `encodeURI`, which keeps the reserved characters, and of Python `quote`, which keeps `/`)
/// and JSON-escaped. Forms shorter than 8 characters are left out. Its own encoders, separate
/// from the secret matcher.
pub fn secret_forms(value: &str) -> Vec<(String, String)> {
    let b = value.as_bytes();
    let mut forms: Vec<(String, String)> = vec![("raw".into(), value.to_owned())];
    for (name, engine) in [
        ("base64", &STANDARD_NO_PAD),
        ("base64url", &URL_SAFE_NO_PAD),
    ] {
        for shift in 0..3usize {
            let mut buf = vec![b'A'; shift];
            buf.extend_from_slice(b);
            let enc = engine.encode(&buf);
            let start = (shift * 8).div_ceil(6);
            let end = ((shift + b.len()) * 8) / 6;
            if end > start {
                forms.push((format!("{name} shift {shift}"), enc[start..end].to_owned()));
            }
        }
    }
    forms.push(("hex".into(), hex::encode(b)));
    forms.push(("HEX".into(), hex::encode_upper(b)));
    let mut pct_upper = String::new();
    let mut pct_lower = String::new();
    let mut pct_uri = String::new();
    let mut pct_path = String::new();
    for &c in b {
        let unreserved = c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b'_' | b'~');
        if unreserved {
            pct_upper.push(c as char);
            pct_lower.push(c as char);
        } else {
            pct_upper.push_str(&format!("%{c:02X}"));
            pct_lower.push_str(&format!("%{c:02x}"));
        }
        if unreserved || b";,/?:@&=+$#!'()*".contains(&c) {
            pct_uri.push(c as char);
        } else {
            pct_uri.push_str(&format!("%{c:02X}"));
        }
        if unreserved || c == b'/' {
            pct_path.push(c as char);
        } else {
            pct_path.push_str(&format!("%{c:02X}"));
        }
    }
    forms.push(("percent encodeURI".into(), pct_uri));
    forms.push(("percent keeps slash".into(), pct_path));
    forms.push(("percent form".into(), pct_upper.replace("%20", "+")));
    forms.push(("percent".into(), pct_upper));
    forms.push(("percent lower".into(), pct_lower));
    let json = serde_json::to_string(value).unwrap_or_default();
    forms.push(("json".into(), json[1..json.len() - 1].to_owned()));
    forms.retain(|(_, f)| f.len() >= 8);
    forms
}

/// Scans bytes (a serialised store) for the secrets in every form of [`secret_forms`], also as
/// the inside of a JSON string. Returns one line per finding, naming the secret by its index
/// and the form, never the value.
pub fn scan_for_secrets(haystack: &[u8], secrets: &[String]) -> Vec<String> {
    let text = String::from_utf8_lossy(haystack);
    let mut found = Vec::new();
    for (i, s) in secrets.iter().enumerate() {
        for (name, form) in secret_forms(s) {
            let json = serde_json::to_string(&form).unwrap_or_default();
            let json_inner = &json[1..json.len() - 1];
            if text.contains(form.as_str()) || text.contains(json_inner) {
                found.push(format!("secret #{i} found in form {name}"));
            }
        }
    }
    found
}

/// Full encodings of a secret, as an app could log them (each holds a form of
/// [`secret_forms`]).
fn encoded_values(value: &str) -> Vec<String> {
    let b = value.as_bytes();
    let mut out = Vec::new();
    for prefix in ["", "u:", "us:"] {
        let mut buf = prefix.as_bytes().to_vec();
        buf.extend_from_slice(b);
        out.push(STANDARD.encode(&buf));
        out.push(URL_SAFE.encode(&buf));
        out.push(STANDARD_NO_PAD.encode(&buf));
    }
    out.push(hex::encode(b));
    out.push(hex::encode_upper(b));
    for (_, form) in secret_forms(value) {
        if form.contains('%') || form.contains('\\') {
            out.push(format!("pre {form} post"));
        }
    }
    out
}

// ---------------------------------------------------------------------------- helpers

fn fresh(env: Environment) -> SourceIdentity {
    SourceIdentity {
        tenant: TenantId::new(),
        app: NodeId::new(),
        deployment: Uuid::now_v7(),
        environment: env,
    }
}

fn sibling(of: &SourceIdentity) -> SourceIdentity {
    SourceIdentity {
        app: NodeId::new(),
        deployment: Uuid::now_v7(),
        ..*of
    }
}

fn need(cond: bool, msg: impl FnOnce() -> String) -> CaseResultT {
    if cond { Ok(()) } else { Err(msg()) }
}

/// `len` hex characters (at most 64) that differ on each call.
fn random_hex(len: usize) -> String {
    hex::encode(Sha256::digest(Uuid::now_v7().as_bytes()))[..len].to_owned()
}

fn trace_id() -> String {
    random_hex(32)
}

fn span_id() -> String {
    random_hex(16)
}

fn log_with(fields: Vec<Attribute>, attributes: Vec<Attribute>) -> LogRecord {
    LogRecord {
        time: crate::now(),
        severity: Severity::Info,
        body: LogBody::Template {
            template: "event {a}".into(),
            fields,
        },
        attributes,
    }
}

fn log_text(text: String) -> LogRecord {
    LogRecord {
        time: crate::now(),
        severity: Severity::Warn,
        body: LogBody::Text(text),
        attributes: vec![],
    }
}

fn metric_with(name: &str, labels: Vec<Attribute>) -> MetricPoint {
    MetricPoint {
        time: crate::now(),
        name: name.into(),
        kind: MetricKind::Counter,
        value: 1.0,
        labels,
    }
}

fn span_with(attributes: Vec<Attribute>) -> Span {
    let start = crate::now();
    Span {
        trace_id: trace_id(),
        span_id: span_id(),
        parent_span_id: None,
        name: "GET /orders".into(),
        kind: SpanKind::Server,
        start,
        end: start + time::Duration::milliseconds(5),
        status: StatusCode::Ok,
        status_message: None,
        attributes,
        events: vec![],
        links: vec![],
    }
}

/// A batch of one record of the signal that carries `attrs` as template fields, labels or span
/// attributes.
fn batch_with(s: Signal, attrs: Vec<Attribute>) -> Batch {
    match s {
        Signal::Logs => Batch {
            logs: vec![log_with(attrs, vec![])],
            ..Default::default()
        },
        Signal::Metrics => Batch {
            metrics: vec![metric_with("http_requests_total", attrs)],
            ..Default::default()
        },
        Signal::Traces => Batch {
            spans: vec![span_with(attrs)],
            ..Default::default()
        },
    }
}

/// The attribute sets of every stored record of the signal (template fields with the log
/// attributes, labels, span attributes).
fn stored_sets(b: &ClassifiedBatch, s: Signal) -> Vec<Vec<StoredAttribute>> {
    match s {
        Signal::Logs => b
            .logs
            .iter()
            .map(|r| r.fields.iter().chain(&r.attributes).cloned().collect())
            .collect(),
        Signal::Metrics => b.metrics.iter().map(|r| r.labels.clone()).collect(),
        Signal::Traces => b.spans.iter().map(|r| r.attributes.clone()).collect(),
    }
}

fn count(b: &ClassifiedBatch, s: Signal) -> usize {
    match s {
        Signal::Logs => b.logs.len(),
        Signal::Metrics => b.metrics.len(),
        Signal::Traces => b.spans.len(),
    }
}

async fn ingest(
    f: &dyn PrivacyFixture,
    id: &SourceIdentity,
    batch: Batch,
) -> std::result::Result<IngestReport, String> {
    f.ingest()
        .ingest(id, batch)
        .await
        .map_err(|e| format!("ingest failed: {e}"))
}

/// The records of the identity's app, after checking that at least `min` of the signal were
/// stored.
async fn read_stored(
    f: &dyn PrivacyFixture,
    id: &SourceIdentity,
    s: Signal,
    min: usize,
) -> std::result::Result<ClassifiedBatch, String> {
    let filter = TelemetryFilter {
        signal: Some(s),
        ..TelemetryFilter::app(id.app)
    };
    let b = f
        .store()
        .read(id.tenant, &filter)
        .await
        .map_err(|e| format!("read failed: {e}"))?;
    need(count(&b, s) >= min, || {
        format!(
            "expected at least {min} stored {} records, found {} (a case never passes on silence)",
            signal_name(s),
            count(&b, s)
        )
    })?;
    Ok(b)
}

fn value_of<'a>(sets: &'a [Vec<StoredAttribute>], key: &str) -> Option<&'a StoredValue> {
    sets.iter().find_map(|set| attr(set, key))
}

fn describe(v: Option<&StoredValue>) -> String {
    match v {
        None => "absent".into(),
        Some(StoredValue::Hidden(_)) => "hidden".into(),
        Some(StoredValue::Pseudonym(_)) => "a pseudonym".into(),
        Some(StoredValue::Text(_)) => "plain text".into(),
        Some(_) => "a plain value".into(),
    }
}

fn expect_hidden(sets: &[Vec<StoredAttribute>], key: &str, s: Signal) -> CaseResultT {
    let v = value_of(sets, key);
    if s == Signal::Metrics {
        need(v.is_none(), || {
            format!("label {key}: expected dropped, was {}", describe(v))
        })
    } else {
        need(v.is_some_and(StoredValue::is_hidden_secret), || {
            format!(
                "{key}: expected the hidden secret marker, was {}",
                describe(v)
            )
        })
    }
}

fn expect_pseudonym<'a>(
    sets: &'a [Vec<StoredAttribute>],
    key: &str,
) -> std::result::Result<&'a Pseudonym, String> {
    let v = value_of(sets, key);
    match v.and_then(StoredValue::as_pseudonym) {
        Some(p) if Pseudonym::parse(p.id()).is_some() => Ok(p),
        _ => Err(format!("{key}: expected a pseudonym, was {}", describe(v))),
    }
}

fn expect_text(sets: &[Vec<StoredAttribute>], key: &str, want: &str) -> CaseResultT {
    let v = value_of(sets, key);
    need(v.and_then(StoredValue::as_text) == Some(want), || {
        format!("{key}: expected the plain value, was {}", describe(v))
    })
}

/// A personal field: a pseudonym in logs and traces, dropped as a metric label.
fn expect_personal(sets: &[Vec<StoredAttribute>], key: &str, s: Signal) -> CaseResultT {
    if s == Signal::Metrics {
        let v = value_of(sets, key);
        need(v.is_none(), || {
            format!("label {key}: expected dropped, was {}", describe(v))
        })
    } else {
        expect_pseudonym(sets, key).map(|_| ())
    }
}

async fn scan_store(f: &dyn PrivacyFixture, secrets: &[String]) -> CaseResultT {
    let dump = f.dump().await.map_err(|e| format!("dump failed: {e}"))?;
    need(!dump.is_empty(), || "the store dump is empty".into())?;
    let found = scan_for_secrets(&dump, secrets);
    need(found.is_empty(), || {
        format!("secrets in the store: {}", found.join("; "))
    })
}

// ---------------------------------------------------------------------------- cases

/// No raw secret value is stored, in any place a record can carry one, in any encoding.
async fn no_raw_secret(f: &dyn PrivacyFixture, s: Signal) -> CaseResultT {
    let id = fresh(Environment::Live);
    let kind = kind_of(s);
    let secrets = fixture_secrets();
    let (cur, prev, tok) = (&secrets[0], &secrets[1], &secrets[2]);
    f.set_secrets(id.tenant, id.app, fixture_secret_values())
        .await;
    f.set_class(
        id.tenant,
        id.app,
        kind,
        "stash",
        FieldClassEntry::confirmed(FieldClass::Secret),
    )
    .await;
    f.set_class(
        id.tenant,
        id.app,
        kind,
        "note",
        FieldClassEntry::confirmed(FieldClass::Normal),
    )
    .await;
    f.set_class(
        id.tenant,
        id.app,
        kind,
        "who",
        FieldClassEntry::confirmed(FieldClass::Personal),
    )
    .await;
    f.set_class(
        id.tenant,
        id.app,
        kind,
        "route",
        FieldClassEntry::confirmed(FieldClass::Normal),
    )
    .await;
    let mut attrs = vec![
        Attribute::new("password", cur.as_str()),
        Attribute::new("stash", cur.as_str()),
        Attribute::new("mystery", prev.as_str()),
        Attribute::new("note", tok.as_str()),
        Attribute::new("who", tok.as_str()),
        Attribute::new("route", "/orders"),
    ];
    let mut n = 0;
    for secret in &secrets {
        for enc in encoded_values(secret) {
            let key = format!("enc{n}");
            f.set_class(
                id.tenant,
                id.app,
                kind,
                &key,
                FieldClassEntry::confirmed(FieldClass::Normal),
            )
            .await;
            attrs.push(Attribute::new(key, enc));
            n += 1;
        }
    }
    let dump_text = format!(
        "APP_SECRET={cur}\nPREVIOUS={prev}\nRUUM_TOKEN={tok}\nSTRIPE_KEY={}\nHOME=/home/app\n",
        fixture_live_key()
    );
    let mut batch = batch_with(s, attrs);
    match s {
        Signal::Logs => {
            batch
                .logs
                .push(log_text(format!("login failed for {cur} (retry)")));
            batch.logs.push(log_text(dump_text));
            batch.logs.push(LogRecord {
                body: LogBody::Template {
                    template: format!("boot with {prev}"),
                    fields: vec![],
                },
                ..log_with(vec![], vec![Attribute::new("note", cur.as_str())])
            });
        }
        Signal::Metrics => {
            batch
                .metrics
                .push(metric_with(&format!("leak_{tok}"), vec![]));
            batch.metrics.push(metric_with(
                "jobs_total",
                vec![Attribute::new("note", prev.as_str())],
            ));
        }
        Signal::Traces => {
            let span = &mut batch.spans[0];
            span.name = format!("GET /callback?code={cur}");
            span.status = StatusCode::Error;
            span.status_message = Some(dump_text);
            span.events.push(SpanEvent {
                time: crate::now(),
                name: format!("exception {prev}"),
                attributes: vec![
                    Attribute::new("exception.message", format!("bad key {tok}")),
                    Attribute::new("note", cur.as_str()),
                ],
            });
            span.links.push(SpanLink {
                trace_id: trace_id(),
                span_id: span_id(),
                attributes: vec![Attribute::new("note", prev.as_str())],
            });
        }
    }
    let report = ingest(f, &id, batch).await?;
    let stored = read_stored(f, &id, s, 1).await?;
    need(report.secrets_redacted > 0, || {
        "the report counts no redacted secret".into()
    })?;
    let sets = stored_sets(&stored, s);
    // The order: the secret matcher runs before the classes, so a personal field that holds a
    // secret is hidden, not a pseudonym of the secret.
    expect_hidden(&sets, "who", s)?;
    expect_hidden(&sets, "note", s)?;
    if s != Signal::Metrics {
        expect_hidden(&sets, "stash", s)?;
        expect_hidden(&sets, "password", s)?;
    }
    if s == Signal::Metrics {
        expect_text(&sets, "route", "/orders")?;
    }
    let mut all = secrets.clone();
    all.push(fixture_live_key());
    scan_store(f, &all).await
}

/// Deny-listed names are `secret` even when confirmed `normal`.
async fn deny_list(f: &dyn PrivacyFixture, s: Signal) -> CaseResultT {
    let id = fresh(Environment::Live);
    let kind = kind_of(s);
    let names = [
        "password",
        "userPassword",
        "db.password",
        "X-Api-Key",
        "stripe_token",
        "Authorization",
        "set-cookie",
        "client_secret",
        "session",
    ];
    let mut attrs = vec![Attribute::new("order_id", "A-1001")];
    f.set_class(
        id.tenant,
        id.app,
        kind,
        "order_id",
        FieldClassEntry::confirmed(FieldClass::Normal),
    )
    .await;
    for (i, name) in names.iter().enumerate() {
        f.set_class(
            id.tenant,
            id.app,
            kind,
            name,
            FieldClassEntry::confirmed(FieldClass::Normal),
        )
        .await;
        attrs.push(Attribute::new(*name, format!("v-{i}")));
    }
    ingest(f, &id, batch_with(s, attrs)).await?;
    let stored = read_stored(f, &id, s, 1).await?;
    let sets = stored_sets(&stored, s);
    expect_text(&sets, "order_id", "A-1001")?;
    for name in names {
        expect_hidden(&sets, name, s)?;
    }
    Ok(())
}

/// A stricter suggestion applies at once; a looser one waits for a person.
async fn suggestions(f: &dyn PrivacyFixture, s: Signal) -> CaseResultT {
    let id = fresh(Environment::Live);
    let kind = kind_of(s);
    use FieldClass::*;
    let rows = [
        (
            "f_a",
            FieldClassEntry::confirmed(Normal).with_suggestion(Secret),
        ),
        (
            "f_b",
            FieldClassEntry::confirmed(Normal).with_suggestion(Personal),
        ),
        (
            "f_c",
            FieldClassEntry::confirmed(Personal).with_suggestion(Normal),
        ),
        (
            "f_d",
            FieldClassEntry::confirmed(Secret).with_suggestion(Normal),
        ),
        (
            "f_e",
            FieldClassEntry::confirmed(Normal).with_suggestion(Normal),
        ),
        (
            "f_f",
            FieldClassEntry::unconfirmed(Normal).with_suggestion(Secret),
        ),
    ];
    let mut attrs = vec![];
    for (name, entry) in rows {
        f.set_class(id.tenant, id.app, kind, name, entry).await;
        attrs.push(Attribute::new(name, format!("value of {name}")));
    }
    ingest(f, &id, batch_with(s, attrs)).await?;
    let stored = read_stored(f, &id, s, 1).await?;
    let sets = stored_sets(&stored, s);
    expect_hidden(&sets, "f_a", s)?;
    expect_personal(&sets, "f_b", s)?;
    expect_personal(&sets, "f_c", s)?;
    expect_hidden(&sets, "f_d", s)?;
    expect_text(&sets, "f_e", "value of f_e")?;
    expect_hidden(&sets, "f_f", s)
}

/// Personal values are pseudonyms: equal within an app, different across apps and tenants.
/// For metrics: a personal label is dropped, never a pseudonym.
async fn pseudonyms(f: &dyn PrivacyFixture, s: Signal) -> CaseResultT {
    let a1 = fresh(Environment::Live);
    let a2 = sibling(&a1);
    let t2 = fresh(Environment::Live);
    let kind = kind_of(s);
    let value = "alice@example.org";
    for id in [&a1, &a2, &t2] {
        f.set_class(
            id.tenant,
            id.app,
            kind,
            "customer",
            FieldClassEntry::confirmed(FieldClass::Personal),
        )
        .await;
        f.set_class(
            id.tenant,
            id.app,
            kind,
            "seat",
            FieldClassEntry::confirmed(FieldClass::Personal),
        )
        .await;
        f.set_class(
            id.tenant,
            id.app,
            kind,
            "route",
            FieldClassEntry::confirmed(FieldClass::Normal),
        )
        .await;
    }
    let attrs = || {
        vec![
            Attribute::new("customer", value),
            Attribute::new("seat", 42i64),
            Attribute::new("route", "/pay"),
        ]
    };
    ingest(f, &a1, batch_with(s, attrs())).await?;
    ingest(f, &a1, batch_with(s, attrs())).await?;
    ingest(f, &a2, batch_with(s, attrs())).await?;
    ingest(f, &t2, batch_with(s, attrs())).await?;
    let r1 = read_stored(f, &a1, s, 2).await?;
    let r2 = read_stored(f, &a2, s, 1).await?;
    let r3 = read_stored(f, &t2, s, 1).await?;
    let (s1, s2, s3) = (
        stored_sets(&r1, s),
        stored_sets(&r2, s),
        stored_sets(&r3, s),
    );
    if s == Signal::Metrics {
        for sets in [&s1, &s2, &s3] {
            expect_personal(sets, "customer", s)?;
            expect_personal(sets, "seat", s)?;
            expect_text(sets, "route", "/pay")?;
        }
        return Ok(());
    }
    let p_a = expect_pseudonym(&s1[..1], "customer")?;
    let p_b = expect_pseudonym(&s1[1..], "customer")?;
    let p_app2 = expect_pseudonym(&s2, "customer")?;
    let p_t2 = expect_pseudonym(&s3, "customer")?;
    expect_pseudonym(&s1, "seat")?;
    need(p_a == p_b, || {
        "the same value in two records of one app gave two pseudonyms".into()
    })?;
    need(p_a != p_app2, || {
        "the same value in two apps gave the same pseudonym".into()
    })?;
    need(p_a != p_t2, || {
        "the same value in two tenants gave the same pseudonym".into()
    })?;
    need(!p_a.id().contains("alice"), || {
        "the pseudonym holds the value".into()
    })?;
    expect_text(&s1, "route", "/pay")
}

/// A field the source does not know, or that no person confirmed, is personal.
async fn unclassified(f: &dyn PrivacyFixture, s: Signal) -> CaseResultT {
    let id = fresh(Environment::Live);
    let kind = kind_of(s);
    f.set_class(
        id.tenant,
        id.app,
        kind,
        "listed_field",
        FieldClassEntry::unconfirmed(FieldClass::Normal),
    )
    .await;
    f.set_class(
        id.tenant,
        id.app,
        kind,
        "route",
        FieldClassEntry::confirmed(FieldClass::Normal),
    )
    .await;
    // The same name as another kind is another field: a confirmed normal class of another kind
    // does not count.
    let other = match kind {
        FieldKind::LogField => FieldKind::SpanAttribute,
        _ => FieldKind::LogField,
    };
    f.set_class(
        id.tenant,
        id.app,
        other,
        "other_kind",
        FieldClassEntry::confirmed(FieldClass::Normal),
    )
    .await;
    let attrs = vec![
        Attribute::new("mystery_field", "bob"),
        Attribute::new("listed_field", "carol"),
        Attribute::new("other_kind", "dave"),
        Attribute::new("route", "/x"),
    ];
    let report = ingest(f, &id, batch_with(s, attrs)).await?;
    let stored = read_stored(f, &id, s, 1).await?;
    let sets = stored_sets(&stored, s);
    expect_personal(&sets, "mystery_field", s)?;
    expect_personal(&sets, "listed_field", s)?;
    expect_personal(&sets, "other_kind", s)?;
    expect_text(&sets, "route", "/x")?;
    need(
        report
            .unknown_fields
            .contains(&(kind, "mystery_field".to_owned())),
        || "the report does not name the unknown field as seen".into(),
    )?;
    need(
        !report
            .unknown_fields
            .contains(&(kind, "listed_field".to_owned())),
        || "the report names a listed field as unknown".into(),
    )
}

/// Personal, unclassified, secret and deny-listed metric labels are dropped; a normal label
/// stays; no label is ever a pseudonym or a hidden marker.
async fn metric_labels(f: &dyn PrivacyFixture) -> CaseResultT {
    let id = fresh(Environment::Live);
    let k = FieldKind::MetricLabel;
    f.set_class(
        id.tenant,
        id.app,
        k,
        "user_email",
        FieldClassEntry::confirmed(FieldClass::Personal),
    )
    .await;
    f.set_class(
        id.tenant,
        id.app,
        k,
        "api_handle",
        FieldClassEntry::confirmed(FieldClass::Secret),
    )
    .await;
    f.set_class(
        id.tenant,
        id.app,
        k,
        "session",
        FieldClassEntry::confirmed(FieldClass::Normal),
    )
    .await;
    f.set_class(
        id.tenant,
        id.app,
        k,
        "route",
        FieldClassEntry::confirmed(FieldClass::Normal),
    )
    .await;
    f.set_class(
        id.tenant,
        id.app,
        k,
        "status",
        FieldClassEntry::confirmed(FieldClass::Normal),
    )
    .await;
    let labels = vec![
        Attribute::new("user_email", "erin@example.org"),
        Attribute::new("api_handle", "h-77"),
        Attribute::new("session", "s-1"),
        Attribute::new("unknown_label", "frank"),
        Attribute::new("route", "/a"),
        Attribute::new("status", 200i64),
    ];
    let report = ingest(f, &id, batch_with(Signal::Metrics, labels)).await?;
    let stored = read_stored(f, &id, Signal::Metrics, 1).await?;
    let sets = stored_sets(&stored, Signal::Metrics);
    for key in ["user_email", "api_handle", "session", "unknown_label"] {
        let v = value_of(&sets, key);
        need(v.is_none(), || {
            format!("label {key}: expected dropped, was {}", describe(v))
        })?;
    }
    expect_text(&sets, "route", "/a")?;
    need(
        matches!(value_of(&sets, "status"), Some(StoredValue::Int(200))),
        || "label status: the normal number was not kept".into(),
    )?;
    for set in &sets {
        for a in set {
            need(
                !matches!(a.value, StoredValue::Pseudonym(_) | StoredValue::Hidden(_)),
                || format!("label {} holds a marker; labels may only be normal", a.key),
            )?;
        }
    }
    need(report.labels_dropped >= 4, || {
        format!(
            "the report counts {} dropped labels, expected 4",
            report.labels_dropped
        )
    })
}

/// Metric names are attacker text: a name without the OTel instrument name syntax, or with an
/// email or a card number in it, drops the record; a normal name is stored.
async fn metric_names(f: &dyn PrivacyFixture) -> CaseResultT {
    let id = fresh(Environment::Live);
    let card = ["4111", "1111", "1111", "1111"].concat();
    let email = ["alice", "@example.org"].concat();
    let good = "http.server.request.duration";
    let batch = Batch {
        metrics: vec![
            metric_with(&format!("orders_{card}_total"), vec![]),
            metric_with(&format!("orders_{email}_total"), vec![]),
            metric_with("bad\"}\nname{x=\"1\"", vec![]),
            metric_with(&"a".repeat(256), vec![]),
            metric_with(good, vec![]),
        ],
        ..Default::default()
    };
    let report = ingest(f, &id, batch).await?;
    let stored = read_stored(f, &id, Signal::Metrics, 1).await?;
    let names: Vec<&str> = stored.metrics.iter().map(|m| m.name.as_str()).collect();
    need(names == [good], || {
        format!(
            "expected only the metric {good} stored, found {} metrics",
            names.len()
        )
    })?;
    need(
        report.dropped(DropReason::InvalidName) >= 3
            && report.dropped(DropReason::PatternInName) >= 1,
        || {
            format!(
                "the report counts {} invalid and {} pattern names, expected 3 and 1",
                report.dropped(DropReason::InvalidName),
                report.dropped(DropReason::PatternInName)
            )
        },
    )?;
    let dump = f.dump().await.map_err(|e| format!("dump failed: {e}"))?;
    let dump = String::from_utf8_lossy(&dump);
    need(
        !dump.contains(&format!("orders_{card}")) && !dump.contains(&email),
        || "a card number or an email of a metric name is in the store".into(),
    )
}

/// A secret of digits only is hidden (a dropped label) also when it arrives as a number, in a
/// normal and in a personal field.
async fn numeric_secret(f: &dyn PrivacyFixture, s: Signal) -> CaseResultT {
    let id = fresh(Environment::Live);
    let kind = kind_of(s);
    let digits = fixture_numeric_secret();
    let n: i64 = digits
        .parse()
        .map_err(|_| "the numeric fixture is not a number")?;
    f.set_secrets(id.tenant, id.app, vec![SecretValue::new(digits.clone())])
        .await;
    for (key, class) in [
        ("note", FieldClass::Normal),
        ("who", FieldClass::Personal),
        ("ratio", FieldClass::Normal),
        ("count", FieldClass::Normal),
    ] {
        f.set_class(
            id.tenant,
            id.app,
            kind,
            key,
            FieldClassEntry::confirmed(class),
        )
        .await;
    }
    let attrs = vec![
        Attribute::new("note", n),
        Attribute::new("who", n),
        Attribute::new("ratio", n as f64),
        Attribute::new("count", 42i64),
    ];
    let report = ingest(f, &id, batch_with(s, attrs)).await?;
    let stored = read_stored(f, &id, s, 1).await?;
    let sets = stored_sets(&stored, s);
    for key in ["note", "who", "ratio"] {
        expect_hidden(&sets, key, s)?;
    }
    need(
        matches!(value_of(&sets, "count"), Some(StoredValue::Int(42))),
        || "count: the normal number was not kept".into(),
    )?;
    need(report.secrets_redacted >= 3, || {
        format!(
            "the report counts {} redacted secrets, expected 3",
            report.secrets_redacted
        )
    })?;
    scan_store(f, &[digits]).await
}

/// Attribute keys are stored verbatim (and become series names), so a key with a card number
/// or an IBAN in it is dropped, wherever a record carries attributes.
async fn personal_data_in_keys(f: &dyn PrivacyFixture, s: Signal) -> CaseResultT {
    let id = fresh(Environment::Live);
    let kind = kind_of(s);
    let card_key = ["card_4111", "1111", "1111", "1111"].concat();
    let iban_key = ["iban_GB82", "WEST", "12345698765432"].concat();
    f.set_class(
        id.tenant,
        id.app,
        kind,
        "route",
        FieldClassEntry::confirmed(FieldClass::Normal),
    )
    .await;
    let attrs = || {
        vec![
            Attribute::new(card_key.as_str(), "x"),
            Attribute::new(iban_key.as_str(), "y"),
            Attribute::new("route", "/k"),
        ]
    };
    let mut batch = batch_with(s, attrs());
    match s {
        Signal::Logs => batch.logs[0].attributes = attrs(),
        Signal::Metrics => {}
        Signal::Traces => {
            let span = &mut batch.spans[0];
            span.events.push(SpanEvent {
                time: crate::now(),
                name: "ev".into(),
                attributes: attrs(),
            });
            span.links.push(SpanLink {
                trace_id: trace_id(),
                span_id: span_id(),
                attributes: attrs(),
            });
        }
    }
    let report = ingest(f, &id, batch).await?;
    let stored = read_stored(f, &id, s, 1).await?;
    expect_text(&stored_sets(&stored, s), "route", "/k")?;
    let text = serde_json::to_string(&stored).map_err(|e| format!("serialise: {e}"))?;
    need(
        !text.contains(card_key.as_str()) && !text.contains(iban_key.as_str()),
        || "an attribute key with a card number or an IBAN was stored".into(),
    )?;
    need(report.invalid_names_dropped >= 2, || {
        format!(
            "the report counts {} dropped names, expected at least 2",
            report.invalid_names_dropped
        )
    })
}

/// Records that carry `ruum.*` attributes of another tenant or app, or `live` from a preview,
/// are stored only under the given identity, with those attributes replaced.
async fn forged_identity(f: &dyn PrivacyFixture, s: Signal) -> CaseResultT {
    let id = fresh(Environment::Preview);
    let victim = fresh(Environment::Live);
    let forged = vec![
        Attribute::new(ATTR_TENANT, victim.tenant.to_string()),
        Attribute::new(ATTR_APP, victim.app.to_string()),
        Attribute::new(ATTR_DEPLOYMENT, victim.deployment.to_string()),
        Attribute::new(ATTR_ENVIRONMENT, "live"),
        Attribute::new("RUUM.Tenant", victim.tenant.to_string()),
        Attribute::new("ruum.extra", "x"),
    ];
    let mut batch = match s {
        Signal::Logs => Batch {
            logs: vec![log_with(forged.clone(), forged.clone())],
            ..Default::default()
        },
        _ => batch_with(s, forged.clone()),
    };
    if s == Signal::Traces {
        batch.spans[0].events.push(SpanEvent {
            time: crate::now(),
            name: "ev".into(),
            attributes: forged.clone(),
        });
    }
    ingest(f, &id, batch).await?;
    let stored = read_stored(f, &id, s, 1).await?;
    let want = classify::identity_attributes(&id);
    let check = |attrs: &[StoredAttribute], what: &str, expect_identity: bool| -> CaseResultT {
        let reserved: Vec<&StoredAttribute> = attrs
            .iter()
            .filter(|a| classify::is_reserved(&a.key))
            .collect();
        if expect_identity {
            need(reserved.len() == want.len(), || {
                format!(
                    "{what}: {} ruum.* attributes, expected exactly the {} of the identity",
                    reserved.len(),
                    want.len()
                )
            })?;
            for w in &want {
                need(reserved.contains(&w), || {
                    format!("{what}: {} is not the identity's value", w.key)
                })?;
            }
            Ok(())
        } else {
            need(reserved.is_empty(), || {
                format!("{what}: kept a ruum.* attribute of the record")
            })
        }
    };
    match s {
        Signal::Logs => {
            for r in &stored.logs {
                need(r.source == id, || {
                    "the source is not the given identity".into()
                })?;
                check(&r.fields, "log fields", false)?;
                check(&r.attributes, "log attributes", true)?;
            }
        }
        Signal::Metrics => {
            for r in &stored.metrics {
                need(r.source == id, || {
                    "the source is not the given identity".into()
                })?;
                check(&r.labels, "labels", true)?;
            }
        }
        Signal::Traces => {
            for r in &stored.spans {
                need(r.source == id, || {
                    "the source is not the given identity".into()
                })?;
                check(&r.attributes, "span attributes", true)?;
                for e in &r.events {
                    check(&e.attributes, "event attributes", false)?;
                }
            }
        }
    }
    let victim_view = f
        .store()
        .read(victim.tenant, &TelemetryFilter::default())
        .await
        .map_err(|e| format!("read failed: {e}"))?;
    need(victim_view.is_empty(), || {
        "a record reached the storage tenant named by a forged attribute".into()
    })
}

/// Two tenants with the same field names: a read of tenant A with any filter returns only A's
/// records.
async fn tenant_isolation(f: &dyn PrivacyFixture, s: Signal) -> CaseResultT {
    let a = fresh(Environment::Live);
    let b = fresh(Environment::Live);
    let kind = kind_of(s);
    for id in [&a, &b] {
        f.set_class(
            id.tenant,
            id.app,
            kind,
            "order_id",
            FieldClassEntry::confirmed(FieldClass::Normal),
        )
        .await;
        ingest(
            f,
            id,
            batch_with(s, vec![Attribute::new("order_id", "same-1")]),
        )
        .await?;
    }
    read_stored(f, &a, s, 1).await?;
    read_stored(f, &b, s, 1).await?;
    let eq = |k: &str, v: String| FieldEquals {
        key: k.into(),
        value: v,
    };
    let filters = [
        TelemetryFilter::default(),
        TelemetryFilter::app(b.app),
        TelemetryFilter {
            deployment: Some(b.deployment),
            ..Default::default()
        },
        TelemetryFilter {
            fields: vec![eq(ATTR_TENANT, b.tenant.to_string())],
            ..Default::default()
        },
        TelemetryFilter {
            fields: vec![eq("order_id", "same-1".into())],
            ..Default::default()
        },
        TelemetryFilter {
            signal: Some(s),
            app: Some(b.app),
            fields: vec![eq(ATTR_APP, b.app.to_string())],
            ..Default::default()
        },
    ];
    for (i, filter) in filters.iter().enumerate() {
        let got = f
            .store()
            .read(a.tenant, filter)
            .await
            .map_err(|e| format!("read failed: {e}"))?;
        let sources: Vec<SourceIdentity> = got
            .logs
            .iter()
            .map(|r| r.source)
            .chain(got.metrics.iter().map(|r| r.source))
            .chain(got.spans.iter().map(|r| r.source))
            .collect();
        need(sources.iter().all(|src| src.tenant == a.tenant), || {
            format!("filter #{i}: a read of tenant A returned a record of another tenant")
        })?;
        for set in stored_sets(&got, s) {
            need(
                attr(&set, ATTR_TENANT).and_then(StoredValue::as_text)
                    == Some(&a.tenant.to_string()),
                || format!("filter #{i}: a returned record names another tenant"),
            )?;
        }
    }
    Ok(())
}

/// The free-text samples: (text, expected marker or `None` for a negative example).
fn free_text_samples() -> Vec<(String, Option<scrub::Found>)> {
    use scrub::Found::{Hidden, Pseudonym};
    let jwt = [
        "eyJ",
        "hbGciOiJIUzI1NiJ9",
        ".",
        "eyJzdWIiOiIxIn0",
        ".",
        "c2lnLW5vdC1yZWFs",
    ]
    .concat();
    vec![
        ("alice.smith+tag@example.co.uk".into(), Some(Pseudonym)),
        ("+372 5123 4567".into(), Some(Pseudonym)),
        ("(020) 7946 0958".into(), Some(Pseudonym)),
        ("0612345678".into(), Some(Pseudonym)),
        ("DE89 3704 0044 0532 0130 00".into(), Some(Pseudonym)),
        ("GB82WEST12345698765432".into(), Some(Pseudonym)),
        ("4111 1111 1111 1111".into(), Some(Hidden)),
        ("5500-0000-0000-0004".into(), Some(Hidden)),
        (jwt, Some(Hidden)),
        (["abcDEF12", "3456ghiJKL"].concat(), Some(Hidden)), // after "Bearer "
        (fixture_live_key(), Some(Hidden)),
        (
            ["ghp_", "R0um0Fixture0Only0Not0Real0abcdef123"].concat(),
            Some(Hidden),
        ),
        (["AKIA", "R0UMFIXTURE0NOT0"].concat(), Some(Hidden)),
        (
            ["ruum_", "appkey_", "R0umFixtureOnlyNotReal0"].concat(),
            Some(Hidden),
        ),
        (
            ["0123456789abcdef", "0123456789abcdef01"].concat(),
            Some(Hidden),
        ),
        (
            ["QmFzZTY0IGZpeHR1cmUg", "b25seSwgbm90IHJlYWwgOkQ1"].concat(),
            Some(Hidden),
        ),
        ("order 12345 shipped".into(), None),
        ("4111 1111 1111 1112".into(), None),
        ("user@localhost".into(), None),
        ("2026-11-15".into(), None),
        ("version 1.2.3".into(), None),
        (
            "snake_case_identifier_that_is_quite_long_indeed".into(),
            None,
        ),
    ]
}

fn check_free_text(sample: &str, want: Option<scrub::Found>, stored: &str) -> CaseResultT {
    match want {
        None => need(stored.contains(sample), || {
            format!("negative example #{} was changed", sample.len())
        }),
        Some(found) => {
            need(!stored.contains(sample), || {
                format!("a sample of {} bytes stayed in the free text", sample.len())
            })?;
            let marker = match found {
                scrub::Found::Hidden => crate::runtime::redact::REDACTED,
                scrub::Found::Pseudonym => "<person ",
            };
            need(stored.contains(marker), || {
                format!("a sample of {} bytes did not become {marker}", sample.len())
            })
        }
    }
}

/// Free text in logs: each pattern, positive and negative.
async fn free_text_logs(f: &dyn PrivacyFixture) -> CaseResultT {
    let id = fresh(Environment::Live);
    let samples = free_text_samples();
    let logs = samples
        .iter()
        .enumerate()
        .map(|(i, (text, _))| {
            let prefix = if i == 9 { "auth: Bearer " } else { "seen " };
            log_text(format!("{i}: {prefix}{text} end"))
        })
        .collect();
    ingest(
        f,
        &id,
        Batch {
            logs,
            ..Default::default()
        },
    )
    .await?;
    let stored = read_stored(f, &id, Signal::Logs, samples.len()).await?;
    for (i, (sample, want)) in samples.iter().enumerate() {
        let text = stored
            .logs
            .iter()
            .filter_map(|r| r.text.as_deref())
            .find(|t| t.starts_with(&format!("{i}: ")))
            .ok_or_else(|| format!("sample #{i} was not stored"))?;
        check_free_text(sample, *want, text).map_err(|e| format!("sample #{i}: {e}"))?;
    }
    Ok(())
}

/// Free text in traces: the span name, the status message and the event name.
async fn free_text_traces(f: &dyn PrivacyFixture) -> CaseResultT {
    let id = fresh(Environment::Live);
    let mut span = span_with(vec![]);
    span.name = "GET /users/alice.smith@example.org".into();
    span.status = StatusCode::Error;
    span.status_message = Some("card 4111 1111 1111 1111 declined".into());
    span.events.push(SpanEvent {
        time: crate::now(),
        name: "call +372 5123 4567".into(),
        attributes: vec![Attribute::new(
            "exception.message",
            "no user bob@example.org",
        )],
    });
    ingest(
        f,
        &id,
        Batch {
            spans: vec![span],
            ..Default::default()
        },
    )
    .await?;
    let stored = read_stored(f, &id, Signal::Traces, 1).await?;
    let s = &stored.spans[0];
    check_free_text(
        "alice.smith@example.org",
        Some(scrub::Found::Pseudonym),
        &s.name,
    )?;
    check_free_text(
        "4111 1111 1111 1111",
        Some(scrub::Found::Hidden),
        s.status_message.as_deref().unwrap_or_default(),
    )?;
    let ev = s.events.first().ok_or("the event was not stored")?;
    check_free_text("+372 5123 4567", Some(scrub::Found::Pseudonym), &ev.name)?;
    let msg = attr(&ev.attributes, "exception.message").and_then(StoredValue::as_text);
    check_free_text(
        "bob@example.org",
        Some(scrub::Found::Pseudonym),
        msg.unwrap_or_default(),
    )
    .map_err(|e| format!("exception.message: {e}"))
}

/// A free-text attribute (`exception.message`, `exception.stacktrace`) that a person confirmed
/// as `normal` is still scrubbed: free text is `personal` (review K3). As a metric label it is
/// dropped. In traces the span event attributes are checked too.
async fn free_text_confirmed_normal(f: &dyn PrivacyFixture, s: Signal) -> CaseResultT {
    let id = fresh(Environment::Live);
    let kind = kind_of(s);
    let email = "bob@example.org";
    let card = "4111 1111 1111 1111";
    let message = format!("no user {email} for card {card} declined");
    let stack = format!("at pay (user {email} card {card}) in pay.rs");
    for name in ["order_id", "exception.message", "exception.stacktrace"] {
        f.set_class(
            id.tenant,
            id.app,
            kind,
            name,
            FieldClassEntry::confirmed(FieldClass::Normal),
        )
        .await;
    }
    let attrs = || {
        vec![
            Attribute::new("order_id", "A-1001"),
            Attribute::new("exception.message", message.clone()),
            Attribute::new("exception.stacktrace", stack.clone()),
        ]
    };
    let mut batch = batch_with(s, attrs());
    if let Some(span) = batch.spans.first_mut() {
        span.events.push(SpanEvent {
            time: crate::now(),
            name: "exception".into(),
            attributes: attrs(),
        });
    }
    ingest(f, &id, batch).await?;
    let stored = read_stored(f, &id, s, 1).await?;
    let mut sets = stored_sets(&stored, s);
    expect_text(&sets, "order_id", "A-1001")?;
    if s == Signal::Metrics {
        for name in ["exception.message", "exception.stacktrace"] {
            let v = value_of(&sets, name);
            need(v.is_none(), || {
                format!("label {name}: expected dropped, was {}", describe(v))
            })?;
        }
        return Ok(());
    }
    if s == Signal::Traces {
        let ev = stored.spans[0]
            .events
            .first()
            .ok_or("the event was not stored")?;
        sets.push(ev.attributes.clone());
    }
    for set in &sets {
        for name in ["exception.message", "exception.stacktrace"] {
            let text = attr(set, name)
                .and_then(StoredValue::as_text)
                .ok_or_else(|| {
                    format!(
                        "{name}: expected scrubbed text, was {}",
                        describe(attr(set, name))
                    )
                })?;
            check_free_text(email, Some(scrub::Found::Pseudonym), text)
                .map_err(|e| format!("{name}: {e}"))?;
            check_free_text(card, Some(scrub::Found::Hidden), text)
                .map_err(|e| format!("{name}: {e}"))?;
        }
    }
    let dump = f.dump().await?;
    let leaked: Vec<&str> = [email, card]
        .into_iter()
        .filter(|v| dump.windows(v.len()).any(|w| w == v.as_bytes()))
        .collect();
    need(leaked.is_empty(), || {
        format!("{} sample(s) of free text are in the store", leaked.len())
    })
}

/// A long adversarial free text is scrubbed within a time bound (the scanner is linear).
async fn free_text_time_bound(f: &dyn PrivacyFixture) -> CaseResultT {
    let id = fresh(Environment::Live);
    let unit_texts = [
        "a@", "a.", "+1 ", "4111 ", "eyJ.", "DE00 ", "bearer ", "(0", "AKIA", "sk_live_", "0-",
        "ab@cd.", "@a.bc ",
    ];
    let target = 30 * 1024;
    let logs: Vec<LogRecord> = unit_texts
        .iter()
        .map(|u| log_text(u.repeat(target / u.len())))
        .collect();
    let n = logs.len();
    let started = Instant::now();
    ingest(
        f,
        &id,
        Batch {
            logs,
            ..Default::default()
        },
    )
    .await?;
    let took = started.elapsed();
    read_stored(f, &id, Signal::Logs, n).await?;
    need(took < Duration::from_secs(10), || {
        format!("scrubbing {n} adversarial texts of 30 KiB took {took:?}")
    })
}

/// After all cases: no fixture secret in any form anywhere in the store.
async fn whole_store_scan(f: &dyn PrivacyFixture) -> CaseResultT {
    let mut all = fixture_secrets();
    all.push(fixture_live_key());
    scan_store(f, &all).await
}
