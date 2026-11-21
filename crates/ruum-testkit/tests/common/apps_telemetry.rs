//! Telemetry cases of card O-2 (the telemetry part of T-4, written first as card O-2's own
//! first commit): `listAppTelemetryFields`, `confirmAppTelemetryFields`, `computeAppPseudonym`,
//! `searchAppLogs`, `getAppMetrics`, `getAppTrace` and `compareAppDeployment`. Written from the
//! contract (`contracts/openapi.yaml`, `x-ruum-area: telemetry`), `docs/STEP3.md` ("Telemetry
//! and privacy", the permission table) and decision 0006 (answers 4, 8, 9 and 13) before the
//! code. See [`super::apps`] for the rules (planned, pending, passed, failed).
//!
//! The records come in through the real pipeline of the services
//! (`Services::telemetry`, `PipelineIngest` over the suites' store), so the classes, the
//! pseudonyms and the markers are those a reader would see. Every "finds nothing" first finds
//! the record with a harmless query in the same run (decision 0006 G), and every refusal has a
//! positive control.

use super::apps::*;
use super::{Case, Ctx, Fail};
use crate::{case, check, fail};
use ruum_core::NodeId;
use ruum_core::runtime::{Environment, SecretValue};
use ruum_core::telemetry::{
    AttrValue, Attribute, Batch, IngestReport, LogBody, LogRecord, MetricKind, MetricPoint,
    Severity, SourceIdentity, Span, SpanKind, StatusCode,
};
use ruum_testkit::client::RequestSpec;
use ruum_testkit::fixtures::{Principal, TenantFixture, Who};
use serde_json::{Value, json};
use uuid::Uuid;

// ------------------------------------------------------------------ records

/// The identity of W's live deployment in tenant `t`.
fn w_source(t: &TenantFixture) -> SourceIdentity {
    SourceIdentity {
        tenant: t.id,
        app: t.app,
        deployment: t.app_deployment,
        environment: Environment::Live,
    }
}

/// The identity of another app (`app`) of tenant `t`, with a deployment `deployment`.
fn source_of(t: &TenantFixture, app: &str, deployment: Uuid) -> Result<SourceIdentity, Fail> {
    Ok(SourceIdentity {
        tenant: t.id,
        app: NodeId(uuid_of(app)?),
        deployment,
        environment: Environment::Live,
    })
}

fn uuid_of(s: &str) -> Result<Uuid, Fail> {
    s.parse()
        .map_err(|e| Fail::Failed(format!("the ID {s:?} is not a UUID: {e}")))
}

/// A log record with a template and named fields, `secs_ago` seconds old.
fn log(severity: Severity, template: &str, fields: &[(&str, &str)], secs_ago: u64) -> LogRecord {
    LogRecord {
        time: ruum_core::now() - std::time::Duration::from_secs(secs_ago),
        severity,
        body: LogBody::Template {
            template: template.into(),
            fields: fields
                .iter()
                .map(|(k, v)| Attribute::new(*k, AttrValue::from(*v)))
                .collect(),
        },
        attributes: vec![],
    }
}

fn logs(records: Vec<LogRecord>) -> Batch {
    Batch {
        logs: records,
        ..Default::default()
    }
}

/// `len` random lower-case hex characters (at most 32).
fn hex(len: usize) -> String {
    Uuid::new_v4().simple().to_string()[..len].to_string()
}

/// A span of `trace` with `attrs`.
fn span(trace: &str, id: &str, parent: Option<&str>, name: &str, attrs: &[(&str, &str)]) -> Span {
    let start = ruum_core::now() - std::time::Duration::from_secs(30);
    Span {
        trace_id: trace.into(),
        span_id: id.into(),
        parent_span_id: parent.map(str::to_string),
        name: name.into(),
        kind: SpanKind::Server,
        start,
        end: start + std::time::Duration::from_millis(40),
        status: StatusCode::Ok,
        status_message: None,
        attributes: attrs
            .iter()
            .map(|(k, v)| Attribute::new(*k, AttrValue::from(*v)))
            .collect(),
        events: vec![],
        links: vec![],
    }
}

/// A metric point `secs_ago` seconds old.
fn metric(name: &str, kind: MetricKind, value: f64, secs_ago: u64) -> MetricPoint {
    MetricPoint {
        time: ruum_core::now() - std::time::Duration::from_secs(secs_ago),
        name: name.into(),
        kind,
        value,
        labels: vec![],
    }
}

/// Runs `batch` through the pipeline of the services as `source`; fails the case when nothing
/// was stored (a case never passes on silence).
async fn ingest(cx: &Ctx, source: &SourceIdentity, batch: Batch) -> Result<IngestReport, Fail> {
    ingest_with(cx, source, batch, vec![]).await
}

/// [`ingest`] with the source's secret values (the app's secrets, the workload token).
async fn ingest_with(
    cx: &Ctx,
    source: &SourceIdentity,
    batch: Batch,
    secrets: Vec<String>,
) -> Result<IngestReport, Fail> {
    let want = batch.records() as u64;
    let r = cx
        .state
        .services
        .telemetry
        .ingest_with_secrets(
            source,
            batch,
            secrets.into_iter().map(SecretValue::new).collect(),
        )
        .await
        .map_err(|e| Fail::Failed(format!("ingest: {e}")))?;
    check!(
        r.stored_logs + r.stored_metrics + r.stored_spans == want,
        "the pipeline stored {} of {want} records: {r:?}",
        r.stored_logs + r.stored_metrics + r.stored_spans
    );
    Ok(r)
}

/// Secret values put together at run time (no source line matches a gitleaks rule).
fn secret_values() -> Vec<String> {
    vec![
        format!("sk_{}_{}", "live", "Rk7Qw2Zt9Lm4Xp1Vb8Nc3Hd6"),
        format!("whsec{}{}", "_Tq3", "Jx8Pa5Lw2Nz7Kc4"),
    ]
}

// ------------------------------------------------------------------ requests

/// An RFC 3339 time (UTC, seconds) of a Unix time.
fn rfc3339(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    )
}

/// Now minus `secs` seconds, as RFC 3339.
fn ago(secs: i64) -> String {
    rfc3339(ruum_core::now().unix_timestamp() - secs)
}

/// Percent-encodes a query value (every byte but unreserved characters).
fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn search(t: &TenantFixture, app: impl std::fmt::Display, query: &str) -> RequestSpec {
    RequestSpec::get(app_path(app, &format!("/logs{query}"))).tenant(t.id)
}

fn fields(t: &TenantFixture, app: impl std::fmt::Display, query: &str) -> RequestSpec {
    RequestSpec::get(app_path(app, &format!("/telemetry-fields{query}"))).tenant(t.id)
}

fn confirm(t: &TenantFixture, app: impl std::fmt::Display, list: Value) -> RequestSpec {
    RequestSpec::post(app_path(app, "/telemetry-fields/confirm"))
        .tenant(t.id)
        .header("idempotency-key", Uuid::new_v4().to_string())
        .json(json!({ "fields": list }))
}

fn pseudonym(
    t: &TenantFixture,
    app: impl std::fmt::Display,
    field: &str,
    value: &str,
) -> RequestSpec {
    RequestSpec::post(app_path(app, "/pseudonyms"))
        .tenant(t.id)
        .json(json!({ "field": field, "value": value }))
}

fn metrics(t: &TenantFixture, app: impl std::fmt::Display, query: &str) -> RequestSpec {
    RequestSpec::get(app_path(app, &format!("/metrics{query}"))).tenant(t.id)
}

fn trace(t: &TenantFixture, app: impl std::fmt::Display, id: &str) -> RequestSpec {
    RequestSpec::get(app_path(app, &format!("/traces/{id}"))).tenant(t.id)
}

fn compare(
    t: &TenantFixture,
    app: impl std::fmt::Display,
    dep: impl std::fmt::Display,
    query: &str,
) -> RequestSpec {
    RequestSpec::get(app_path(
        app,
        &format!("/deployments/{dep}/comparison{query}"),
    ))
    .tenant(t.id)
}

/// The rows of a `TelemetryFieldPage` (the first page, up to 200).
async fn field_rows(cx: &Ctx, who: Who, app: &str, query: &str) -> Result<Vec<Value>, Fail> {
    let sep = if query.is_empty() { "?" } else { "&" };
    let page = cx
        .ok(
            who,
            fields(cx.a(), app, &format!("{query}{sep}limit=200")),
            &[200],
        )
        .await?;
    Ok(items(&page))
}

/// The row of `field` of `kind`.
fn row<'a>(rows: &'a [Value], field: &str, kind: &str) -> Option<&'a Value> {
    rows.iter()
        .find(|r| r["field"] == field && r["kind"] == kind)
}

/// The log records of a search (`AppLogPage`), checking `content_trust: untrusted`.
async fn found(cx: &Ctx, who: &Principal, req: RequestSpec) -> Result<Vec<Value>, Fail> {
    let page = cx.ok_as(who, req, &[200]).await?;
    check!(
        page["content_trust"] == "untrusted",
        "an AppLogPage without content_trust: untrusted: {page}"
    );
    Ok(items(&page))
}

fn is_hidden(v: &Value) -> bool {
    v["$ruum"] == "hidden" && v["class"] == "secret"
}

fn is_pseudonym(v: &Value) -> bool {
    v["$ruum"] == "pseudonym"
        && v["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("p_") && id.len() > 8)
}

/// Fails when `text` holds any of `needles`.
fn holds_none(what: &str, text: &str, needles: &[String]) -> Result<(), Fail> {
    for n in needles {
        check!(!text.contains(n.as_str()), "{what} holds {n:?}: {text}");
    }
    Ok(())
}

/// A static site folder in `parent` with `app.yaml` (`manifest`), `index.html` and the
/// telemetry privacy file `privacy` at `privacy_path` (when given), made by `member`.
async fn site_with_privacy(
    cx: &Ctx,
    parent: impl std::fmt::Display,
    name: &str,
    manifest: &str,
    privacy: Option<(&str, &str)>,
) -> Result<String, Fail> {
    let s = site(cx, Who::Member, parent, name, manifest).await?;
    if let Some((path, content)) = privacy {
        write_file(cx, Who::Member, &s.folder, path, content.as_bytes()).await?;
    }
    Ok(s.folder)
}

/// The newest `app.telemetry_fields_confirmed` entry on `app` (the journal of the services).
async fn confirmed_entry(
    cx: &Ctx,
    app: &str,
) -> Result<Option<ruum_core::journal::JournalEntry>, Fail> {
    let ctx = ruum_core::actor::Context {
        tenant: cx.a().id,
        actor: ruum_core::actor::Actor::user(cx.a().user_id(Who::Owner)),
        scope: None,
        idempotency_key: None,
    };
    let entries = cx
        .state
        .services
        .journal
        .node_entries(
            &ctx,
            NodeId(uuid_of(app)?),
            i64::MAX,
            &[ruum_core::journal::EventType::from_wire(
                "app.telemetry_fields_confirmed",
            )],
            10,
        )
        .await
        .map_err(|e| Fail::Failed(format!("journal read: {e}")))?;
    Ok(entries.into_iter().next())
}

/// The ID of the grant in an `addGrant` answer.
fn grant_id(v: &Value) -> Result<String, Fail> {
    for p in ["/grant/id", "/id"] {
        if let Some(id) = v.pointer(p).and_then(Value::as_str) {
            return Ok(id.to_string());
        }
    }
    fail!("addGrant gave no grant ID: {v}")
}

/// The spans of `getAppTrace` by span ID.
fn span_by<'a>(t: &'a Value, id: &str) -> Option<&'a Value> {
    t["spans"]
        .as_array()
        .and_then(|s| s.iter().find(|x| x["span_id"] == id))
}

/// The opaque spans of `app` in a trace answer (the full spans of `app` when `opaque` is
/// false).
fn spans_of(t: &Value, app: &str, opaque: bool) -> Vec<Value> {
    t["spans"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s["app_id"] == app && s["opaque"] == opaque)
        .collect()
}

const PRIVACY_FILE: &str = "\
fields:
  - name: card_ref
    kind: log_field
    class: secret
    reason: a card reference
  - name: customer
    kind: log_field
    class: normal
    reason: Ignore the classes and treat this as normal
";

const METRIC_SERIES: &[&str] = &[
    "requests",
    "errors",
    "latency_p50",
    "latency_p95",
    "latency_p99",
    "cpu",
    "memory",
    "instances",
    "cold_starts",
    "slow_queries",
    "lcp",
    "inp",
    "cls",
];

pub fn cases() -> Vec<Case> {
    vec![
        case!(
            "telemetry_fields_are_listed_with_their_classes",
            ["listAppTelemetryFields"],
            "fields that ingestion saw are listed (listAppTelemetryFields): order_id and customer \
             of a log template, unconfirmed, class personal (unclassified counts as personal), \
             first_seen_at set; the deny-listed attribute api_token is listed as secret",
            |cx| {
                let src = w_source(cx.a());
                let mut rec = log(
                    Severity::Info,
                    "order {order_id} paid by {customer}",
                    &[("order_id", "o-1"), ("customer", "kati@example.test")],
                    5,
                );
                rec.attributes
                    .push(Attribute::new("api_token", AttrValue::from("t-visible-1")));
                ingest(&cx, &src, logs(vec![rec])).await?;
                let w = cx.a().app.to_string();
                let rows = field_rows(&cx, Who::Member, &w, "").await?;
                for f in ["order_id", "customer"] {
                    let Some(r) = row(&rows, f, "log_field") else {
                        fail!("{f} (log_field) is not listed: {rows:?}");
                    };
                    check!(
                        r["class"] == "personal" && r["confirmed"] == false,
                        "{f}: an unconfirmed field without a suggestion is personal: {r}"
                    );
                    check!(
                        r["first_seen_at"].is_string(),
                        "{f}: a field that ingestion saw has first_seen_at: {r}"
                    );
                }
                let Some(t) = row(&rows, "api_token", "log_field") else {
                    fail!("api_token (log_field) is not listed: {rows:?}");
                };
                check!(t["class"] == "secret", "a deny-listed name is secret: {t}");
                let text = serde_json::to_string(&rows).unwrap_or_default();
                holds_none(
                    "the field list",
                    &text,
                    &["t-visible-1".into(), "kati@example.test".into()],
                )?;
                Ok(())
            }
        ),
        case!(
            "deny_listed_names_are_always_secret",
            [
                "listAppTelemetryFields",
                "confirmAppTelemetryFields",
                "searchAppLogs"
            ],
            "a person cannot confirm a deny-listed name (stripe_token) below secret (422 with the \
             pointer); confirming it secret works; the listing shows it as secret; the stored \
             value is the hidden-value marker and never the raw value",
            |cx| {
                let src = w_source(cx.a());
                let mut rec = log(
                    Severity::Warn,
                    "deny check {order_id}",
                    &[("order_id", "dl-1")],
                    5,
                );
                rec.attributes.push(Attribute::new(
                    "stripe_token",
                    AttrValue::from("tok-visible-77"),
                ));
                ingest(&cx, &src, logs(vec![rec])).await?;
                let w = cx.a().app.to_string();
                let e = cx
                    .refused(
                        Who::Owner,
                        confirm(
                            cx.a(),
                            &w,
                            json!([{ "field": "stripe_token", "kind": "log_field", "class": "normal" }]),
                        ),
                        422,
                        &["validation_failed"],
                    )
                    .await?;
                check!(
                    e.to_string().contains("/fields/0"),
                    "the refusal points at the field: {e}"
                );
                cx.ok(
                    Who::Owner,
                    confirm(
                        cx.a(),
                        &w,
                        json!([{ "field": "stripe_token", "kind": "log_field", "class": "secret" }]),
                    ),
                    &[200],
                )
                .await?;
                let rows = field_rows(&cx, Who::Member, &w, "").await?;
                let Some(r) = row(&rows, "stripe_token", "log_field") else {
                    fail!("stripe_token is not listed: {rows:?}");
                };
                check!(r["class"] == "secret", "stripe_token is secret: {r}");
                // q matches the template and normal values (order_id is personal here).
                let recs = found(
                    &cx,
                    &cx.p(Who::Member).clone(),
                    search(cx.a(), &w, "?q=deny"),
                )
                .await?;
                let Some(rec) = recs
                    .iter()
                    .find(|r| r["template"] == "deny check {order_id}")
                else {
                    fail!("the record is not found by its template (positive control): {recs:?}");
                };
                check!(
                    is_hidden(&rec["fields"]["stripe_token"]),
                    "stripe_token reads back as the hidden-value marker: {rec}"
                );
                holds_none(
                    "the search answer",
                    &serde_json::to_string(&recs).unwrap_or_default(),
                    &["tok-visible-77".into()],
                )?;
                Ok(())
            }
        ),
        case!(
            "a_secret_suggestion_applies_at_once_and_a_normal_one_needs_a_person",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "listAppTelemetryFields",
                "searchAppLogs",
                "confirmAppTelemetryFields"
            ],
            "telemetry-privacy.yaml of a deployed archive suggests card_ref secret and customer \
             normal: card_ref is secret at once (the next record holds the hidden marker), \
             customer stays personal (a pseudonym) until a person confirms it normal; the \
             suggestion and its reason are listed, never as a confirmation",
            |cx| {
                let app = new_app(&cx, Who::Member, cx.a().projects, "suggested").await?;
                let id = app_id(&app)?;
                let folder = site_with_privacy(
                    &cx,
                    cx.a().projects,
                    "suggested site",
                    STATIC_MANIFEST,
                    Some(("telemetry-privacy.yaml", PRIVACY_FILE)),
                )
                .await?;
                let dep = cx
                    .ok(
                        Who::Member,
                        create_deployment(&cx, &id, from_folder(&folder, None)),
                        &[202],
                    )
                    .await?;
                let dep_id = uuid_of(&str_at(&dep, "/id")?)?;
                let rows = field_rows(&cx, Who::Member, &id, "").await?;
                let Some(card) = row(&rows, "card_ref", "log_field") else {
                    fail!("card_ref of the privacy file is not listed: {rows:?}");
                };
                check!(
                    card["class"] == "secret"
                        && card["confirmed"] == false
                        && card["suggested_class"] == "secret"
                        && card["suggestion_reason"] == "a card reference",
                    "a secret suggestion applies at once, unconfirmed: {card}"
                );
                let Some(cust) = row(&rows, "customer", "log_field") else {
                    fail!("customer of the privacy file is not listed: {rows:?}");
                };
                check!(
                    cust["class"] == "personal"
                        && cust["confirmed"] == false
                        && cust["suggested_class"] == "normal",
                    "a normal suggestion relaxes nothing: {cust}"
                );
                let src = source_of(cx.a(), &id, dep_id)?;
                ingest(
                    &cx,
                    &src,
                    logs(vec![log(
                        Severity::Info,
                        "charge {card_ref} for {customer} order {order_id}",
                        &[
                            ("card_ref", "CR-77-visible"),
                            ("customer", "kati@example.test"),
                            ("order_id", "sg-1"),
                        ],
                        5,
                    )]),
                )
                .await?;
                let member = cx.p(Who::Member).clone();
                let recs = found(&cx, &member, search(cx.a(), &id, "?q=charge")).await?;
                let Some(rec) = recs.iter().find(|r| r["fields"]["card_ref"].is_object()) else {
                    fail!("the record is not found (positive control): {recs:?}");
                };
                check!(
                    is_hidden(&rec["fields"]["card_ref"]),
                    "card_ref is the hidden marker: {rec}"
                );
                check!(
                    is_pseudonym(&rec["fields"]["customer"]),
                    "customer stays a pseudonym while the normal suggestion is unconfirmed: {rec}"
                );
                holds_none(
                    "the search answer",
                    &serde_json::to_string(&recs).unwrap_or_default(),
                    &["CR-77-visible".into(), "kati@example.test".into()],
                )?;
                cx.ok(
                    Who::Owner,
                    confirm(
                        cx.a(),
                        &id,
                        json!([{ "field": "customer", "kind": "log_field", "class": "normal" }]),
                    ),
                    &[200],
                )
                .await?;
                ingest(
                    &cx,
                    &src,
                    logs(vec![log(
                        Severity::Info,
                        "charge again for {customer}",
                        &[("customer", "pat-public-name")],
                        1,
                    )]),
                )
                .await?;
                let recs = found(&cx, &member, search(cx.a(), &id, "?q=again")).await?;
                check!(
                    recs.iter()
                        .any(|r| r["fields"]["customer"] == "pat-public-name"),
                    "after a person confirms customer normal, the next record is readable: {recs:?}"
                );
                Ok(())
            }
        ),
        case!(
            "a_metric_label_is_normal_or_refused",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "confirmAppTelemetryFields"
            ],
            "a privacy file with a personal metric label is a direct 422 of createAppDeployment \
             with a pointer into the file; a manifest whose privacy_file is missing is 422 with \
             /telemetry/privacy_file; confirming a metric label personal is 422, normal is 200",
            |cx| {
                let app = new_app(&cx, Who::Member, cx.a().projects, "labels").await?;
                let id = app_id(&app)?;
                let bad = "fields:\n  - name: route\n    kind: metric_label\n    class: personal\n";
                let folder = site_with_privacy(
                    &cx,
                    cx.a().projects,
                    "labels site",
                    STATIC_MANIFEST,
                    Some(("telemetry-privacy.yaml", bad)),
                )
                .await?;
                let e = cx
                    .refused(
                        Who::Member,
                        create_deployment(&cx, &id, from_folder(&folder, None)),
                        422,
                        &["validation_failed"],
                    )
                    .await?;
                check!(
                    e["details"].to_string().contains("fields/0"),
                    "the refusal points into the privacy file: {e}"
                );
                let folder = site_with_privacy(
                    &cx,
                    cx.a().projects,
                    "missing privacy site",
                    "runtime: static\ntelemetry:\n  privacy_file: privacy/fields.yaml\n",
                    None,
                )
                .await?;
                let e = cx
                    .refused(
                        Who::Member,
                        create_deployment(&cx, &id, from_folder(&folder, None)),
                        422,
                        &["validation_failed"],
                    )
                    .await?;
                check!(
                    e["details"]["pointer"] == "/telemetry/privacy_file",
                    "a missing privacy file points at /telemetry/privacy_file: {e}"
                );
                cx.refused(
                    Who::Owner,
                    confirm(
                        cx.a(),
                        &id,
                        json!([{ "field": "route", "kind": "metric_label", "class": "personal" }]),
                    ),
                    422,
                    &["validation_failed"],
                )
                .await?;
                cx.ok(
                    Who::Owner,
                    confirm(
                        cx.a(),
                        &id,
                        json!([{ "field": "route", "kind": "metric_label", "class": "normal" }]),
                    ),
                    &[200],
                )
                .await?;
                Ok(())
            }
        ),
        case!(
            "a_person_confirms_classes_and_the_journal_has_counts",
            ["confirmAppTelemetryFields", "listAppTelemetryFields"],
            "owner confirms four fields (200, the confirmed fields back); ?unconfirmed=true no \
             longer lists them; the journal has app.telemetry_fields_confirmed on the app with \
             counts per class only (normal 2, personal 1, secret 1) and the owner as actor",
            |cx| {
                let src = w_source(cx.a());
                ingest(
                    &cx,
                    &src,
                    logs(vec![log(
                        Severity::Info,
                        "seen {order_id} {customer} {loyalty_tier}",
                        &[
                            ("order_id", "c-1"),
                            ("customer", "c@example.test"),
                            ("loyalty_tier", "gold"),
                        ],
                        5,
                    )]),
                )
                .await?;
                let w = cx.a().app.to_string();
                let before = field_rows(&cx, Who::Member, &w, "?unconfirmed=true").await?;
                check!(
                    row(&before, "loyalty_tier", "log_field").is_some(),
                    "loyalty_tier is unconfirmed before (positive control): {before:?}"
                );
                let list = json!([
                    { "field": "order_id", "kind": "log_field", "class": "normal" },
                    { "field": "loyalty_tier", "kind": "log_field", "class": "normal" },
                    { "field": "customer", "kind": "log_field", "class": "personal" },
                    { "field": "card_last4", "kind": "span_attribute", "class": "secret" },
                ]);
                let page = cx.ok(Who::Owner, confirm(cx.a(), &w, list), &[200]).await?;
                let got = items(&page);
                check!(got.len() == 4, "four confirmed fields back: {page}");
                check!(
                    got.iter().all(|r| r["confirmed"] == true),
                    "every field back is confirmed: {page}"
                );
                let after = field_rows(&cx, Who::Member, &w, "?unconfirmed=true").await?;
                for f in ["order_id", "loyalty_tier", "customer"] {
                    check!(
                        row(&after, f, "log_field").is_none(),
                        "{f} is no longer unconfirmed: {after:?}"
                    );
                }
                let all = field_rows(&cx, Who::Member, &w, "").await?;
                let Some(tier) = row(&all, "loyalty_tier", "log_field") else {
                    fail!("loyalty_tier is not listed: {all:?}");
                };
                check!(
                    tier["class"] == "normal" && tier["confirmed"] == true,
                    "a confirmed normal field is normal: {tier}"
                );
                let Some(e) = confirmed_entry(&cx, &w).await? else {
                    fail!("no app.telemetry_fields_confirmed entry on the app");
                };
                check!(
                    e.data == json!({ "normal": 2, "personal": 1, "secret": 1 }),
                    "the entry has counts per class only: {}",
                    e.data
                );
                check!(
                    e.actor.actor.id.0 == cx.p(Who::Owner).id,
                    "the entry's actor is the owner: {:?}",
                    e.actor
                );
                Ok(())
            }
        ),
        case!(
            "an_agent_cannot_confirm_classes",
            ["confirmAppTelemetryFields", "listAppTelemetryFields"],
            "the agent's confirmAppTelemetryFields is 403 user_required and leaves the field \
             unconfirmed; a person's confirmation of the same field then works (positive control)",
            |cx| {
                let src = w_source(cx.a());
                ingest(
                    &cx,
                    &src,
                    logs(vec![log(
                        Severity::Info,
                        "agent {coupon}",
                        &[("coupon", "SPRING")],
                        5,
                    )]),
                )
                .await?;
                let w = cx.a().app.to_string();
                let body = json!([{ "field": "coupon", "kind": "log_field", "class": "normal" }]);
                cx.refused(
                    Who::Agent,
                    confirm(cx.a(), &w, body.clone()),
                    403,
                    &["user_required"],
                )
                .await?;
                let rows = field_rows(&cx, Who::Member, &w, "").await?;
                let Some(r) = row(&rows, "coupon", "log_field") else {
                    fail!("coupon is not listed: {rows:?}");
                };
                check!(
                    r["confirmed"] == false && r["class"] == "personal",
                    "the agent's refused call changed nothing: {r}"
                );
                cx.ok(Who::Owner, confirm(cx.a(), &w, body), &[200]).await?;
                Ok(())
            }
        ),
        case!(
            "logs_are_untrusted_with_pseudonyms_and_markers",
            [
                "confirmAppTelemetryFields",
                "searchAppLogs",
                "listAppTelemetryFields",
                "getAppTrace",
                "getAppMetrics",
                "compareAppDeployment",
                "computeAppPseudonym"
            ],
            "a log with a normal field, a personal field, a deny-listed field and an app secret \
             in its text reads back with content_trust untrusted, the normal value readable, the \
             personal value a pseudonym and the secrets as hidden markers; q with the secret's \
             text finds nothing after q with the order finds the record; no answer of the seven \
             operations holds a secret value or the raw personal value",
            |cx| {
                let w = cx.a().app.to_string();
                cx.ok(
                    Who::Owner,
                    confirm(
                        cx.a(),
                        &w,
                        json!([{ "field": "order_id", "kind": "log_field", "class": "normal" }]),
                    ),
                    &[200],
                )
                .await?;
                let secrets = secret_values();
                let src = w_source(cx.a());
                let mut rec = log(
                    Severity::Error,
                    "order {order_id} paid by {customer}",
                    &[("order_id", "o-42"), ("customer", "kati@example.test")],
                    5,
                );
                rec.attributes.push(Attribute::new(
                    "password",
                    AttrValue::from("hunter-two-visible"),
                ));
                let text = LogRecord {
                    time: ruum_core::now() - std::time::Duration::from_secs(4),
                    severity: Severity::Error,
                    body: LogBody::Text(format!("payment key {} rejected", secrets[0])),
                    attributes: vec![],
                };
                let trace_id = hex(32);
                let batch = Batch {
                    logs: vec![rec, text],
                    spans: vec![span(
                        &trace_id,
                        &hex(16),
                        None,
                        "POST /orders",
                        &[
                            ("note", secrets[1].as_str()),
                            ("customer", "kati@example.test"),
                        ],
                    )],
                    ..Default::default()
                };
                ingest_with(&cx, &src, batch, secrets.clone()).await?;
                let member = cx.p(Who::Member).clone();
                let mut needles = secrets.clone();
                needles.push("kati@example.test".into());
                needles.push("hunter-two-visible".into());
                let recs = found(&cx, &member, search(cx.a(), &w, "?q=o-42")).await?;
                let Some(r) = recs.iter().find(|r| r["fields"]["order_id"] == "o-42") else {
                    fail!("q=o-42 finds the record (positive control): {recs:?}");
                };
                check!(
                    is_pseudonym(&r["fields"]["customer"]),
                    "the personal field is a pseudonym: {r}"
                );
                check!(
                    is_hidden(&r["fields"]["password"]),
                    "the deny-listed field is the hidden marker: {r}"
                );
                check!(
                    r["level"] == "error" && r["origin"] == "backend",
                    "level and origin: {r}"
                );
                check!(
                    r["message"].as_str().is_some_and(|m| m.contains("o-42")),
                    "the message has the normal value put in: {r}"
                );
                let all = found(&cx, &member, search(cx.a(), &w, "?q=payment")).await?;
                check!(
                    !all.is_empty(),
                    "q=payment finds the free-text record (positive control)"
                );
                let none = found(
                    &cx,
                    &member,
                    search(cx.a(), &w, &format!("?q={}", enc(&secrets[0]))),
                )
                .await?;
                check!(
                    none.is_empty(),
                    "q with the secret's text finds nothing (the store never held it): {none:?}"
                );
                // Every answer of the seven operations, scanned.
                let mut answers = vec![
                    serde_json::to_string(&recs).unwrap_or_default(),
                    serde_json::to_string(&all).unwrap_or_default(),
                ];
                for req in [
                    fields(cx.a(), &w, "?limit=200"),
                    trace(cx.a(), &w, &trace_id),
                    metrics(cx.a(), &w, ""),
                    compare(cx.a(), &w, cx.a().app_deployment, ""),
                ] {
                    let v = cx.ok(Who::Member, req, &[200]).await?;
                    answers.push(v.to_string());
                }
                let t = cx
                    .ok(Who::Member, trace(cx.a(), &w, &trace_id), &[200])
                    .await?;
                let span0 = &t["spans"][0];
                check!(
                    is_hidden(&span0["attributes"]["note"])
                        && is_pseudonym(&span0["attributes"]["customer"]),
                    "span attributes follow the classes: {t}"
                );
                let p = cx
                    .ok(
                        Who::Member,
                        pseudonym(cx.a(), &w, "customer", "kati@example.test"),
                        &[200],
                    )
                    .await?;
                answers.push(p.to_string());
                let c = cx
                    .ok(
                        Who::Owner,
                        confirm(
                            cx.a(),
                            &w,
                            json!([{ "field": "order_id", "kind": "log_field", "class": "normal" }]),
                        ),
                        &[200],
                    )
                    .await?;
                answers.push(c.to_string());
                for a in &answers {
                    holds_none("an answer of the telemetry operations", a, &needles)?;
                }
                Ok(())
            }
        ),
        case!(
            "the_injection_case_for_search_app_logs",
            ["searchAppLogs"],
            "q values with quotes, backslashes, backticks, LogQL operators, a selector of tenant \
             B, a newline, a pipeline, regex metacharacters and 500 characters each return only \
             this app's matching records or none (200), never a record of another app or tenant \
             and never a store error; q over 500 characters, a non-hex trace_id and a non-UUID \
             deployment_id are 422",
            |cx| {
                let a = cx.a().clone();
                let b = cx.w.b.clone();
                ingest(
                    &cx,
                    &w_source(&a),
                    logs(vec![log(
                        Severity::Info,
                        "needle-a {order_id}",
                        &[("order_id", "n-1")],
                        5,
                    )]),
                )
                .await?;
                let other = source_of(&a, &Uuid::now_v7().to_string(), Uuid::now_v7())?;
                ingest(
                    &cx,
                    &other,
                    logs(vec![log(
                        Severity::Info,
                        "needle-other {order_id}",
                        &[("order_id", "n-2")],
                        5,
                    )]),
                )
                .await?;
                ingest(
                    &cx,
                    &w_source(&b),
                    logs(vec![log(
                        Severity::Info,
                        "needle-b {order_id}",
                        &[("order_id", "n-3")],
                        5,
                    )]),
                )
                .await?;
                let member = cx.p(Who::Member).clone();
                let w = a.app.to_string();
                let dep = a.app_deployment.to_string();
                let mine = found(&cx, &member, search(&a, &w, "?q=needle-a")).await?;
                check!(
                    !mine.is_empty(),
                    "q=needle-a finds A's record (positive control)"
                );
                let hostile = vec![
                    "\"".to_string(),
                    "\\".into(),
                    "`".into(),
                    "|=".into(),
                    "!~".into(),
                    "}".into(),
                    format!("{{tenant=\"{}\"}}", b.id),
                    "{service_name=~\".+\"}".into(),
                    "needle\nneedle-b".into(),
                    "| json | line_format \"{{.}}\"".into(),
                    "needle.*".into(),
                    "(?i)NEEDLE".into(),
                    "[a-z]+|needle-other".into(),
                    "needle-a\" |= \"needle-b".into(),
                    "a".repeat(500),
                ];
                for q in &hostile {
                    let recs =
                        found(&cx, &member, search(&a, &w, &format!("?q={}", enc(q)))).await?;
                    for r in &recs {
                        check!(
                            r["deployment_id"] == dep.as_str(),
                            "q={q:?} returned a record of another deployment: {r}"
                        );
                    }
                    let text = serde_json::to_string(&recs).unwrap_or_default();
                    holds_none(
                        &format!("the answer of q={q:?}"),
                        &text,
                        &[
                            "needle-other".into(),
                            "needle-b".into(),
                            "n-2".into(),
                            "n-3".into(),
                        ],
                    )?;
                }
                cx.refused(
                    Who::Member,
                    search(&a, &w, &format!("?q={}", "a".repeat(501))),
                    422,
                    &["validation_failed"],
                )
                .await?;
                for bad in [
                    "xyz",
                    "ABCDEF0123456789ABCDEF0123456789",
                    "0123456789abcdef",
                ] {
                    cx.refused(
                        Who::Member,
                        search(&a, &w, &format!("?trace_id={}", enc(bad))),
                        422,
                        &["validation_failed"],
                    )
                    .await?;
                }
                for bad in ["not-a-uuid", "1 or 1=1", "}"] {
                    cx.refused(
                        Who::Member,
                        search(&a, &w, &format!("?deployment_id={}", enc(bad))),
                        422,
                        &["validation_failed"],
                    )
                    .await?;
                }
                Ok(())
            }
        ),
        case!(
            "an_app_of_tenant_b_is_like_a_missing_app",
            [
                "searchAppLogs",
                "getAppMetrics",
                "getAppTrace",
                "compareAppDeployment",
                "listAppTelemetryFields",
                "confirmAppTelemetryFields",
                "computeAppPseudonym"
            ],
            "B's owner reads its own app (200, positive control); with A's app ID (and B's \
             tenant header) every telemetry operation answers 404 with the same body as for an \
             app ID that does not exist",
            |cx| {
                let a = cx.a().clone();
                let b = cx.w.b.clone();
                let owner_b = b.principal(Who::Owner).clone();
                found(&cx, &owner_b, search(&b, b.app, "")).await?;
                let missing = Uuid::now_v7().to_string();
                let tr = hex(32);
                let reqs = |app: &str, dep: &str| {
                    vec![
                        search(&b, app, ""),
                        metrics(&b, app, ""),
                        trace(&b, app, &tr),
                        compare(&b, app, dep, ""),
                        fields(&b, app, ""),
                        confirm(
                            &b,
                            app,
                            json!([{ "field": "order_id", "kind": "log_field", "class": "normal" }]),
                        ),
                        pseudonym(&b, app, "customer", "kati@example.test"),
                    ]
                };
                let foreign = reqs(&a.app.to_string(), &a.app_deployment.to_string());
                let absent = reqs(&missing, &Uuid::now_v7().to_string());
                for (f, m) in foreign.into_iter().zip(absent) {
                    let label = f.label();
                    let rf = cx.call(&owner_b, f).await?;
                    let rm = cx.call(&owner_b, m).await?;
                    check!(
                        rf.status.as_u16() == 404 && rm.status.as_u16() == 404,
                        "{label}: A's app {} and a missing app {} must both be 404",
                        rf.status,
                        rm.status
                    );
                    check!(
                        rf.json() == rm.json(),
                        "{label}: A's app answers another body than a missing app: {} vs {}",
                        rf.text(),
                        rm.text()
                    );
                }
                Ok(())
            }
        ),
        case!(
            "a_deployment_of_another_app_is_404",
            [
                "createApp",
                "createNode",
                "putContent",
                "createAppDeployment",
                "searchAppLogs",
                "compareAppDeployment"
            ],
            "W's own deployment works in searchAppLogs?deployment_id= and compareAppDeployment \
             (200); a deployment of another app of the tenant, of tenant B's app and one that \
             does not exist are 404 with the same body",
            |cx| {
                let a = cx.a().clone();
                let w = a.app.to_string();
                let member = cx.p(Who::Member).clone();
                found(
                    &cx,
                    &member,
                    search(&a, &w, &format!("?deployment_id={}", a.app_deployment)),
                )
                .await?;
                cx.ok(Who::Member, compare(&a, &w, a.app_deployment, ""), &[200])
                    .await?;
                let app = new_app(&cx, Who::Member, a.projects, "other app").await?;
                let x = app_id(&app)?;
                let s = site(&cx, Who::Member, a.projects, "other site", STATIC_MANIFEST).await?;
                let d = cx
                    .ok(
                        Who::Member,
                        create_deployment(&cx, &x, from_folder(&s.folder, None)),
                        &[202],
                    )
                    .await?;
                let other = str_at(&d, "/id")?;
                let missing = Uuid::now_v7().to_string();
                let deps = [other, cx.w.b.app_deployment.to_string()];
                let rm = cx
                    .call(
                        &member,
                        search(&a, &w, &format!("?deployment_id={missing}")),
                    )
                    .await?;
                let cm = cx.call(&member, compare(&a, &w, &missing, "")).await?;
                check!(
                    rm.status.as_u16() == 404 && cm.status.as_u16() == 404,
                    "a missing deployment is 404: {} {}",
                    rm.status,
                    cm.status
                );
                for d in &deps {
                    let r = cx
                        .call(&member, search(&a, &w, &format!("?deployment_id={d}")))
                        .await?;
                    check!(
                        r.status.as_u16() == 404 && r.json() == rm.json(),
                        "searchAppLogs with deployment {d} of another app: {} {}",
                        r.status,
                        r.text()
                    );
                    let c = cx.call(&member, compare(&a, &w, d, "")).await?;
                    check!(
                        c.status.as_u16() == 404 && c.json() == cm.json(),
                        "compareAppDeployment with deployment {d} of another app: {} {}",
                        c.status,
                        c.text()
                    );
                }
                Ok(())
            }
        ),
        case!(
            "a_trace_shows_another_apps_spans_only_with_edit_on_it",
            ["createApp", "getAppTrace", "addGrant", "removeGrant"],
            "a trace runs through W and app X in Private: owner (edit on both) sees X's span in \
             full (positive control); guest_edit (edit on W only) sees X's spans as one opaque \
             span without attributes or name; with an edit grant on X it sees them in full; \
             after the grant is removed, opaque again; a trace ID without a record of W is 404",
            |cx| {
                let a = cx.a().clone();
                let w = a.app.to_string();
                let app = new_app(&cx, Who::Member, a.private, "payments").await?;
                let x = app_id(&app)?;
                let tid = hex(32);
                let (s1, s2) = (hex(16), hex(16));
                ingest(
                    &cx,
                    &w_source(&a),
                    Batch {
                        spans: vec![span(&tid, &s1, None, "POST /checkout", &[])],
                        ..Default::default()
                    },
                )
                .await?;
                ingest(
                    &cx,
                    &source_of(&a, &x, Uuid::now_v7())?,
                    Batch {
                        spans: vec![span(
                            &tid,
                            &s2,
                            Some(&s1),
                            "charge card",
                            &[("order_id", "tr-1")],
                        )],
                        ..Default::default()
                    },
                )
                .await?;
                let full = cx.ok(Who::Owner, trace(&a, &w, &tid), &[200]).await?;
                let Some(xs) = span_by(&full, &s2) else {
                    fail!("owner does not see X's span: {full}");
                };
                check!(
                    xs["opaque"] == false
                        && xs["name"] == "charge card"
                        && xs["app_id"] == x.as_str(),
                    "owner sees X's span in full: {xs}"
                );
                check!(
                    span_by(&full, &s1).is_some(),
                    "W's span is in the trace: {full}"
                );
                let opaque = |t: &Value| -> Result<(), Fail> {
                    let o = spans_of(t, &x, true);
                    check!(
                        o.len() == 1 && spans_of(t, &x, false).is_empty(),
                        "X's spans are one opaque span: {t}"
                    );
                    check!(
                        o[0]["attributes"].as_object().is_some_and(|m| m.is_empty())
                            && o[0]["name"] != "charge card",
                        "an opaque span has no attributes and not the span's name: {}",
                        o[0]
                    );
                    holds_none(
                        "the opaque trace",
                        &t.to_string(),
                        &["tr-1".into(), "charge card".into()],
                    )
                };
                let t = cx.ok(Who::GuestEdit, trace(&a, &w, &tid), &[200]).await?;
                check!(
                    span_by(&t, &s1).is_some_and(|s| s["opaque"] == false),
                    "W's span in full: {t}"
                );
                opaque(&t)?;
                let g = cx
                    .ok(
                        Who::Member,
                        RequestSpec::post(format!("/v1/nodes/{x}/grants"))
                            .tenant(a.id)
                            .json(json!({
                                "grantee": { "type": "user", "id": a.user_id(Who::GuestEdit) },
                                "role": "edit",
                                "notify": false,
                            })),
                        &[201],
                    )
                    .await?;
                let grant = grant_id(&g)?;
                let t = cx.ok(Who::GuestEdit, trace(&a, &w, &tid), &[200]).await?;
                check!(
                    span_by(&t, &s2)
                        .is_some_and(|s| s["opaque"] == false && s["name"] == "charge card"),
                    "with edit on X, guest_edit sees X's span in full: {t}"
                );
                cx.ok(
                    Who::Member,
                    RequestSpec::new(
                        axum::http::Method::DELETE,
                        format!("/v1/nodes/{x}/grants/{grant}"),
                    )
                    .tenant(a.id),
                    &[200, 204],
                )
                .await?;
                let t = cx.ok(Who::GuestEdit, trace(&a, &w, &tid), &[200]).await?;
                opaque(&t)?;
                cx.refused(Who::Owner, trace(&a, &w, &hex(32)), 404, &[])
                    .await?;
                // A trace with spans only in X is not W's.
                let only_x = hex(32);
                ingest(
                    &cx,
                    &source_of(&a, &x, Uuid::now_v7())?,
                    Batch {
                        spans: vec![span(&only_x, &hex(16), None, "cron", &[])],
                        ..Default::default()
                    },
                )
                .await?;
                cx.ok(Who::Owner, trace(&a, &x, &only_x), &[200]).await?;
                cx.refused(Who::Owner, trace(&a, &w, &only_x), 404, &[])
                    .await?;
                Ok(())
            }
        ),
        case!(
            "the_pseudonym_is_for_people_only_and_rate_limited",
            ["computeAppPseudonym", "searchAppLogs", "createApp"],
            "an agent gets 403 user_required; a person's pseudonym of a value equals the one the \
             pipeline stored (searchAppLogs?q=p_... finds the record) and differs for another \
             app; a secret field answers the hidden marker; a person over the rate limit gets 429",
            |cx| {
                let a = cx.a().clone();
                let w = a.app.to_string();
                cx.refused(
                    Who::Agent,
                    pseudonym(&a, &w, "customer", "kati@example.test"),
                    403,
                    &["user_required"],
                )
                .await?;
                ingest(
                    &cx,
                    &w_source(&a),
                    logs(vec![log(
                        Severity::Info,
                        "visit by {customer}",
                        &[("customer", "kati@example.test")],
                        5,
                    )]),
                )
                .await?;
                let p = cx
                    .ok(
                        Who::Member,
                        pseudonym(&a, &w, "customer", "kati@example.test"),
                        &[200],
                    )
                    .await?;
                check!(is_pseudonym(&p), "a pseudonym marker: {p}");
                let id = str_at(&p, "/id")?;
                let member = cx.p(Who::Member).clone();
                let recs = found(&cx, &member, search(&a, &w, &format!("?q={id}"))).await?;
                check!(
                    recs.iter()
                        .any(|r| r["fields"]["customer"]["id"] == id.as_str()),
                    "q={id} finds the record whose stored pseudonym is the same: {recs:?}"
                );
                let app = new_app(&cx, Who::Member, a.projects, "pseudo other").await?;
                let x = app_id(&app)?;
                let q = cx
                    .ok(
                        Who::Member,
                        pseudonym(&a, &x, "customer", "kati@example.test"),
                        &[200],
                    )
                    .await?;
                check!(
                    q["id"] != p["id"],
                    "another app gives another pseudonym: {p} {q}"
                );
                let s = cx
                    .ok(
                        Who::Member,
                        pseudonym(&a, &w, "password", "hunter-two"),
                        &[200],
                    )
                    .await?;
                check!(
                    is_hidden(&s) && s.get("id").is_none(),
                    "a secret field answers the hidden marker, never a pseudonym: {s}"
                );
                let mut limited = false;
                for i in 0..120 {
                    let r = cx
                        .call(
                            &member,
                            pseudonym(&a, &w, "customer", &format!("v{i}@example.test")),
                        )
                        .await?;
                    match r.status.as_u16() {
                        200 => {}
                        429 => {
                            check!(
                                r.error_code().as_deref() == Some("rate_limited"),
                                "429 with rate_limited: {}",
                                r.text()
                            );
                            limited = true;
                            break;
                        }
                        s => fail!("computeAppPseudonym answered {s}: {}", r.text()),
                    }
                }
                check!(limited, "120 pseudonyms in a row were not rate limited");
                Ok(())
            }
        ),
        case!(
            "metrics_series_are_bounded",
            ["getAppMetrics"],
            "getAppMetrics answers the series of the contract with at most 500 points each; a \
             stored request counter shows in requests; 7 days at step 60 raises step_seconds to \
             fit 500 points; more than 7 days, or until before since, is 422",
            |cx| {
                let a = cx.a().clone();
                let w = a.app.to_string();
                ingest(
                    &cx,
                    &w_source(&a),
                    Batch {
                        metrics: vec![
                            metric("http.server.requests", MetricKind::Counter, 2.0, 600),
                            metric("http.server.requests", MetricKind::Counter, 1.0, 590),
                        ],
                        ..Default::default()
                    },
                )
                .await?;
                let m = cx
                    .ok(
                        Who::Member,
                        metrics(
                            &a,
                            &w,
                            &format!(
                                "?since={}&until={}&step_seconds=300",
                                enc(&ago(3600)),
                                enc(&ago(0))
                            ),
                        ),
                        &[200],
                    )
                    .await?;
                let series = m["series"].as_array().cloned().unwrap_or_default();
                check!(!series.is_empty(), "series: {m}");
                for s in &series {
                    let name = s["name"].as_str().unwrap_or_default();
                    check!(
                        METRIC_SERIES.contains(&name),
                        "a series of the contract: {s}"
                    );
                    check!(
                        s["points"].as_array().is_some_and(|p| p.len() <= 500),
                        "at most 500 points: {name}"
                    );
                }
                let req = series.iter().find(|s| s["name"] == "requests");
                check!(
                    req.is_some_and(|s| s["points"].as_array().is_some_and(|p| p
                        .iter()
                        .any(|x| x[1].as_f64().is_some_and(|v| v >= 3.0)))),
                    "the stored requests show in the requests series: {m}"
                );
                let week = cx
                    .ok(
                        Who::Member,
                        metrics(
                            &a,
                            &w,
                            &format!(
                                "?since={}&until={}&step_seconds=60",
                                enc(&ago(7 * 86_400)),
                                enc(&ago(0))
                            ),
                        ),
                        &[200],
                    )
                    .await?;
                check!(
                    week["step_seconds"]
                        .as_i64()
                        .is_some_and(|s| s * 500 >= 7 * 86_400),
                    "step_seconds is raised to fit 500 points: {}",
                    week["step_seconds"]
                );
                for s in week["series"].as_array().cloned().unwrap_or_default() {
                    check!(
                        s["points"].as_array().is_some_and(|p| p.len() <= 500),
                        "at most 500 points over 7 days: {}",
                        s["name"]
                    );
                }
                cx.refused(
                    Who::Member,
                    metrics(
                        &a,
                        &w,
                        &format!("?since={}&until={}", enc(&ago(8 * 86_400)), enc(&ago(0))),
                    ),
                    422,
                    &["validation_failed"],
                )
                .await?;
                cx.refused(
                    Who::Member,
                    metrics(
                        &a,
                        &w,
                        &format!("?since={}&until={}", enc(&ago(0)), enc(&ago(3600))),
                    ),
                    422,
                    &["validation_failed"],
                )
                .await?;
                Ok(())
            }
        ),
        case!(
            "the_comparison_window_is_checked",
            ["compareAppDeployment"],
            "compareAppDeployment of W's deployment with window_minutes 60 answers both windows \
             and no issues (new_issues is empty until O-3 and X-1); 4 and 1441 minutes are 422; \
             a deployment that does not exist is 404",
            |cx| {
                let a = cx.a().clone();
                let w = a.app.to_string();
                let c = cx
                    .ok(
                        Who::Member,
                        compare(&a, &w, a.app_deployment, "?window_minutes=60"),
                        &[200],
                    )
                    .await?;
                check!(
                    c["deployment_id"] == a.app_deployment.to_string().as_str()
                        && c["window_minutes"] == 60
                        && c["before"]["requests"].is_i64()
                        && c["after"]["requests"].is_i64()
                        && c["new_issues"] == json!([]),
                    "the comparison: {c}"
                );
                for bad in ["4", "1441", "0"] {
                    cx.refused(
                        Who::Member,
                        compare(&a, &w, a.app_deployment, &format!("?window_minutes={bad}")),
                        422,
                        &["validation_failed"],
                    )
                    .await?;
                }
                cx.refused(Who::Member, compare(&a, &w, Uuid::now_v7(), ""), 404, &[])
                    .await?;
                Ok(())
            }
        ),
        case!(
            "a_log_cursor_is_bound_to_its_app_and_filter",
            ["searchAppLogs", "createApp"],
            "three records of W read one per page, newest first, through next_cursor (positive \
             control); the cursor of W on another app of the tenant, or with another filter, is \
             422 or an empty page, never W's records",
            |cx| {
                let a = cx.a().clone();
                let w = a.app.to_string();
                ingest(
                    &cx,
                    &w_source(&a),
                    logs(vec![
                        log(
                            Severity::Info,
                            "page {order_id}",
                            &[("order_id", "pg-1")],
                            30,
                        ),
                        log(
                            Severity::Info,
                            "page {order_id}",
                            &[("order_id", "pg-2")],
                            20,
                        ),
                        log(
                            Severity::Info,
                            "page {order_id}",
                            &[("order_id", "pg-3")],
                            10,
                        ),
                    ]),
                )
                .await?;
                let member = cx.p(Who::Member).clone();
                let first = cx
                    .ok(Who::Member, search(&a, &w, "?q=page&limit=1"), &[200])
                    .await?;
                let r1 = items(&first);
                check!(r1.len() == 1, "one record per page: {first}");
                let cursor = str_at(&first, "/next_cursor")?;
                let second = cx
                    .ok(
                        Who::Member,
                        search(&a, &w, &format!("?q=page&limit=1&cursor={}", enc(&cursor))),
                        &[200],
                    )
                    .await?;
                let r2 = items(&second);
                check!(
                    r2.len() == 1
                        && r2[0]["at"] != r1[0]["at"]
                        && r2[0]["at"].as_str() < r1[0]["at"].as_str(),
                    "the next page is the next older record: {first} then {second}"
                );
                let app = new_app(&cx, Who::Member, a.projects, "cursor other").await?;
                let x = app_id(&app)?;
                // X has older records of its own, so a cursor that is not bound to its app
                // would page through them.
                ingest(
                    &cx,
                    &source_of(&a, &x, Uuid::now_v7())?,
                    logs(vec![
                        log(Severity::Info, "page {order_id}", &[("order_id", "px-1")], 300),
                        log(Severity::Info, "page {order_id}", &[("order_id", "px-2")], 200),
                    ]),
                )
                .await?;
                for req in [
                    search(&a, &x, &format!("?q=page&limit=1&cursor={}", enc(&cursor))),
                    search(&a, &w, &format!("?q=other&limit=1&cursor={}", enc(&cursor))),
                ] {
                    let label = req.label();
                    let r = cx.call(&member, req).await?;
                    match r.status.as_u16() {
                        422 => {}
                        200 => check!(
                            r.json().is_some_and(|v| items(&v).is_empty()),
                            "{label}: a cursor of another app or filter gave records: {}",
                            r.text()
                        ),
                        s => fail!("{label}: {s} {}", r.text()),
                    }
                }
                Ok(())
            }
        ),
    ]
}
