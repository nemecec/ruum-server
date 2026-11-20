//! Tests of card O-1b: the OTLP mapping, source authentication, the budget, the shipper, the
//! query escaping of the real store, and the privacy suite against the real stores (ignored
//! unless the stores are up; failing, not skipping, with `CI` or
//! `RUUM_TELEMETRY_REQUIRE_STORES=1`).

use super::*;
use crate::runtime::{
    EnvVar, ImageRef, InstanceId, JobSpec, Limits as RtLimits, MemoryDriver, MemoryRegistry,
    MemoryUsage, OciDigest, RuntimeDriver, WorkloadKind, WorkloadSpec,
};
use crate::telemetry::lgtm::{LgtmConfig, LgtmStore, quote, tenant_header};
use crate::telemetry::otlp::{self, Refusal};
use crate::telemetry::processor::{
    BudgetConfig, BudgetRefusal, FairBudget, LogShipper, MemoryPositions, PositionStore,
    ShipperConfig,
};
use crate::telemetry::source::{
    BearerToken, ListenerId, MemorySourceAuthenticator, NetworkSourceAuthenticator,
    SourceAuthenticator, SourceError, SourceFacts,
};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, InstrumentationScope, KeyValue, any_value};
use opentelemetry_proto::tonic::logs::v1 as ol;
use opentelemetry_proto::tonic::metrics::v1 as om;
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1 as ot;
use prost::Message as _;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------- helpers

fn identity(env: Environment) -> SourceIdentity {
    SourceIdentity {
        tenant: TenantId::new(),
        app: NodeId::new(),
        deployment: Uuid::now_v7(),
        environment: env,
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

/// Resource and record attributes that claim another tenant, app, deployment and `live`.
fn forged(other: &SourceIdentity) -> Vec<KeyValue> {
    vec![
        skv(ATTR_TENANT, &other.tenant.to_string()),
        skv(ATTR_APP, &other.app.to_string()),
        skv(ATTR_DEPLOYMENT, &other.deployment.to_string()),
        skv(ATTR_ENVIRONMENT, "live"),
        skv("RUUM.TENANT", &other.tenant.to_string()),
    ]
}

fn now_nanos() -> u64 {
    crate::now().unix_timestamp_nanos() as u64
}

pub(crate) fn logs_request(resource: Vec<KeyValue>, records: Vec<ol::LogRecord>) -> Vec<u8> {
    ExportLogsServiceRequest {
        resource_logs: vec![ol::ResourceLogs {
            resource: Some(Resource {
                attributes: resource,
                ..Default::default()
            }),
            scope_logs: vec![ol::ScopeLogs {
                scope: Some(InstrumentationScope {
                    name: "test".into(),
                    attributes: vec![skv("scope.attr", "s")],
                    ..Default::default()
                }),
                log_records: records,
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec()
}

fn log_record(text: &str, attrs: Vec<KeyValue>) -> ol::LogRecord {
    ol::LogRecord {
        time_unix_nano: now_nanos(),
        severity_number: 13,
        body: Some(AnyValue {
            value: Some(any_value::Value::StringValue(text.to_owned())),
        }),
        attributes: attrs,
        ..Default::default()
    }
}

fn metrics_request(resource: Vec<KeyValue>, points: Vec<om::NumberDataPoint>) -> Vec<u8> {
    ExportMetricsServiceRequest {
        resource_metrics: vec![om::ResourceMetrics {
            resource: Some(Resource {
                attributes: resource,
                ..Default::default()
            }),
            scope_metrics: vec![om::ScopeMetrics {
                metrics: vec![om::Metric {
                    name: "jobs_total".into(),
                    data: Some(om::metric::Data::Sum(om::Sum {
                        data_points: points,
                        is_monotonic: true,
                        aggregation_temporality: 2,
                    })),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec()
}

fn point(attrs: Vec<KeyValue>) -> om::NumberDataPoint {
    om::NumberDataPoint {
        attributes: attrs,
        time_unix_nano: now_nanos(),
        value: Some(om::number_data_point::Value::AsInt(3)),
        ..Default::default()
    }
}

fn random_bytes(n: usize) -> Vec<u8> {
    let mut v = Vec::new();
    while v.len() < n {
        v.extend_from_slice(Uuid::now_v7().as_bytes());
    }
    v.truncate(n);
    v[0] |= 1;
    v
}

fn span(attrs: Vec<KeyValue>, events: usize, links: usize) -> ot::Span {
    let start = now_nanos();
    ot::Span {
        trace_id: random_bytes(16),
        span_id: random_bytes(8),
        name: "GET /orders".into(),
        kind: 2,
        start_time_unix_nano: start,
        end_time_unix_nano: start + 5_000_000,
        attributes: attrs,
        events: (0..events)
            .map(|i| ot::span::Event {
                time_unix_nano: start + 1,
                name: format!("event {i}"),
                attributes: vec![skv("ev", "x")],
                ..Default::default()
            })
            .collect(),
        links: (0..links)
            .map(|_| ot::span::Link {
                trace_id: random_bytes(16),
                span_id: random_bytes(8),
                attributes: vec![skv("ln", "y")],
                ..Default::default()
            })
            .collect(),
        status: Some(ot::Status {
            message: "ok".into(),
            code: 1,
        }),
        ..Default::default()
    }
}

fn traces_request(resource: Vec<KeyValue>, spans: Vec<ot::Span>) -> Vec<u8> {
    ExportTraceServiceRequest {
        resource_spans: vec![ot::ResourceSpans {
            resource: Some(Resource {
                attributes: resource,
                ..Default::default()
            }),
            scope_spans: vec![ot::ScopeSpans {
                spans,
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec()
}

/// The identity attributes of a stored record are the sender's, and no other `ruum.*`
/// attribute is left.
fn assert_own_identity(attrs: &[StoredAttribute], id: &SourceIdentity) {
    let want = classify::identity_attributes(id);
    let reserved: Vec<&StoredAttribute> = attrs
        .iter()
        .filter(|a| classify::is_reserved(&a.key))
        .collect();
    assert_eq!(reserved.len(), 4, "only the pipeline's identity attributes");
    for w in &want {
        assert_eq!(attr(attrs, &w.key), Some(&w.value), "{}", w.key);
    }
}

// ---------------------------------------------------------------------------- mapping

#[tokio::test]
async fn forged_identity_in_otlp_is_stored_only_as_the_sender() {
    let f = Fixture::new();
    // A preview that claims live, another tenant, app and deployment, in resource and record.
    let me = identity(Environment::Preview);
    let other = identity(Environment::Live);
    let lim = Limits::default();
    let logs = otlp::decode_logs(
        &logs_request(forged(&other), vec![log_record("hello", forged(&other))]),
        &lim,
    )
    .unwrap();
    let metrics = otlp::decode_metrics(
        &metrics_request(forged(&other), vec![point(forged(&other))]),
        &lim,
    )
    .unwrap();
    let traces = otlp::decode_traces(
        &traces_request(forged(&other), vec![span(forged(&other), 1, 1)]),
        &lim,
    )
    .unwrap();
    for m in [logs, metrics, traces] {
        let r = f.ingest.ingest(&me, m.batch).await.unwrap();
        assert!(r.reserved_dropped > 0, "the forged attributes were dropped");
    }
    // Positive control: everything is stored, as the sender.
    let mine = f
        .store
        .read(me.tenant, &TelemetryFilter::app(me.app))
        .await
        .unwrap();
    assert_eq!(
        (mine.logs.len(), mine.metrics.len(), mine.spans.len()),
        (1, 1, 1)
    );
    assert_own_identity(&mine.logs[0].attributes, &me);
    assert_own_identity(&mine.metrics[0].labels, &me);
    assert_own_identity(&mine.spans[0].attributes, &me);
    assert_eq!(mine.logs[0].source, me);
    assert_eq!(mine.spans[0].source.environment, Environment::Preview);
    // Nothing reached the claimed tenant.
    let theirs = f
        .store
        .read(other.tenant, &TelemetryFilter::default())
        .await
        .unwrap();
    assert!(theirs.is_empty());
    assert_eq!(f.store.tenants(), vec![me.tenant]);
}

#[test]
fn resource_and_scope_attributes_become_record_attributes() {
    let lim = Limits::default();
    let m = otlp::decode_logs(
        &logs_request(
            vec![skv("service.name", "shop")],
            vec![log_record("x", vec![skv("k", "v")])],
        ),
        &lim,
    )
    .unwrap();
    let keys: Vec<&str> = m.batch.logs[0]
        .attributes
        .iter()
        .map(|a| a.key.as_str())
        .collect();
    assert_eq!(keys, ["service.name", "scope.attr", "k"]);
    assert!(matches!(&m.batch.logs[0].body, LogBody::Text(t) if t == "x"));
    assert_eq!(m.batch.logs[0].severity, Severity::Warn);
    let t = otlp::decode_traces(&traces_request(vec![], vec![span(vec![], 2, 1)]), &lim).unwrap();
    let s = &t.batch.spans[0];
    assert_eq!(
        (
            s.trace_id.len(),
            s.span_id.len(),
            s.parent_span_id.is_none()
        ),
        (32, 16, true)
    );
    assert_eq!(
        (s.events.len(), s.links.len(), s.kind),
        (2, 1, SpanKind::Server)
    );
}

#[test]
fn a_body_over_the_cap_is_refused_before_decoding() {
    let lim = Limits {
        max_batch_bytes: 1024,
        ..Limits::default()
    };
    // Garbage: if the size were not checked first, the answer would be `Malformed`.
    let body = vec![0xffu8; 1025];
    assert_eq!(
        otlp::decode_logs(&body, &lim).err(),
        Some(Refusal::BodyTooLarge)
    );
    assert_eq!(
        otlp::decode_metrics(&body, &lim).err(),
        Some(Refusal::BodyTooLarge)
    );
    assert_eq!(
        otlp::decode_traces(&body, &lim).err(),
        Some(Refusal::BodyTooLarge)
    );
    assert_eq!(
        otlp::decode_logs(&body[..1024], &lim).err(),
        Some(Refusal::Malformed)
    );
    assert_eq!(
        Refusal::BodyTooLarge.drop_reason(),
        Some(DropReason::BatchTooLarge)
    );
}

#[test]
fn too_many_records_are_refused_before_mapping() {
    let lim = Limits {
        max_records: 3,
        ..Limits::default()
    };
    let four = (0..4).map(|i| log_record(&i.to_string(), vec![])).collect();
    assert_eq!(
        otlp::decode_logs(&logs_request(vec![], four), &lim).err(),
        Some(Refusal::TooManyRecords)
    );
    let four = (0..4).map(|_| point(vec![])).collect();
    assert_eq!(
        otlp::decode_metrics(&metrics_request(vec![], four), &lim).err(),
        Some(Refusal::TooManyRecords)
    );
    let four = (0..4).map(|_| span(vec![], 0, 0)).collect();
    assert_eq!(
        otlp::decode_traces(&traces_request(vec![], four), &lim).err(),
        Some(Refusal::TooManyRecords)
    );
    // Positive control: three pass.
    let three = (0..3).map(|i| log_record(&i.to_string(), vec![])).collect();
    assert_eq!(
        otlp::decode_logs(&logs_request(vec![], three), &lim)
            .unwrap()
            .batch
            .logs
            .len(),
        3
    );
}

/// Wraps `inner` as the length-delimited field `field`.
fn wrap(field: u8, inner: &[u8]) -> Vec<u8> {
    let mut out = vec![(field << 3) | 2];
    prost::encoding::encode_varint(inner.len() as u64, &mut out);
    out.extend_from_slice(inner);
    out
}

/// S-61 and T-56, the review of O-1b: the wire size alone does not bound the decode. Two
/// hostile bodies of the full 8 MiB cap per signal, measured with prost at 426 MiB and 751 MiB
/// of heap: empty top-level entries (no record at all) and empty records (or empty metrics,
/// which count no point) in one scope. Both are refused by the wire walk, before prost runs.
/// A mutation that skips the walk (`telemetry-processor-decode-unbounded`) makes the first
/// body decode and pass.
#[test]
fn bodies_that_decode_far_larger_are_refused_before_decoding() {
    let lim = Limits::default();
    let cap = lim.max_batch_bytes;
    let entries = [0x0a, 0x00].repeat(cap / 2);
    let records = wrap(1, &wrap(2, &[0x12, 0x00].repeat(cap / 2 - 16)));
    assert!(entries.len() <= cap && records.len() <= cap);
    for signal in [Signal::Logs, Signal::Metrics, Signal::Traces] {
        let started = Instant::now();
        assert_eq!(
            otlp::check(signal, &entries, &lim).err(),
            Some(Refusal::BodyTooLarge),
            "{signal:?}: empty top-level entries"
        );
        let want = if signal == Signal::Metrics {
            // An empty `Metric` has no point: its struct size passes the decoded cap.
            Refusal::BodyTooLarge
        } else {
            Refusal::TooManyRecords
        };
        assert_eq!(
            otlp::check(signal, &records, &lim).err(),
            Some(want),
            "{signal:?}: empty records"
        );
        // The walk stops at the first cap: far below a decode of hundreds of MiB.
        assert!(started.elapsed() < Duration::from_secs(2), "{signal:?}");
    }
    assert_eq!(
        otlp::decode_logs(&entries, &lim).err(),
        Some(Refusal::BodyTooLarge)
    );
    assert_eq!(
        otlp::decode_metrics(&records, &lim).err(),
        Some(Refusal::BodyTooLarge)
    );
    assert_eq!(
        otlp::decode_traces(&records, &lim).err(),
        Some(Refusal::TooManyRecords)
    );
}

/// The walk's estimate grows with the entries and stays within the decoded cap for real
/// batches: a full batch of ordinary records passes, and the estimate is at least the struct
/// size of every record (positive controls of the walk).
#[test]
fn the_wire_walk_counts_records_and_estimates_the_decode() {
    let lim = Limits::default();
    let attrs = vec![skv("http.method", "GET"), skv("http.route", "/orders/{id}")];
    let recs: Vec<ol::LogRecord> = (0..lim.max_records)
        .map(|i| {
            log_record(
                &format!("order {i} shipped to the warehouse"),
                attrs.clone(),
            )
        })
        .collect();
    let body = logs_request(attrs.clone(), recs);
    let shape = otlp::check(Signal::Logs, &body, &lim).unwrap().shape();
    assert_eq!(shape.records, lim.max_records as u64);
    let structs = (lim.max_records * std::mem::size_of::<ol::LogRecord>()) as u64;
    assert!(shape.decoded_bytes > structs, "{shape:?}");
    assert!(shape.decoded_bytes <= otlp::max_decoded_bytes(&lim));
    assert_eq!(
        otlp::decode_logs(&body, &lim).unwrap().batch.logs.len(),
        lim.max_records
    );
    let points = (0..100).map(|_| point(attrs.clone())).collect();
    let shape = otlp::check(Signal::Metrics, &metrics_request(vec![], points), &lim)
        .unwrap()
        .shape();
    assert_eq!(shape.records, 100);
    let spans = (0..10).map(|_| span(attrs.clone(), 2, 1)).collect();
    let shape = otlp::check(Signal::Traces, &traces_request(vec![], spans), &lim)
        .unwrap()
        .shape();
    assert_eq!(shape.records, 10);
    // Truncated or grouped wire data is malformed before prost sees it.
    assert_eq!(
        otlp::check(Signal::Logs, &body[..body.len() - 1], &lim).err(),
        Some(Refusal::Malformed)
    );
    assert_eq!(
        otlp::check(Signal::Logs, &[0x0b, 0x0c], &lim).err(),
        Some(Refusal::Malformed)
    );
    // Nesting past prost's own limit.
    let mut deep = vec![];
    for _ in 0..200 {
        deep = wrap(5, &wrap(1, &deep));
    }
    let nested = wrap(1, &wrap(2, &wrap(2, &wrap(6, &wrap(2, &deep)))));
    assert_eq!(
        otlp::check(Signal::Logs, &nested, &lim).err(),
        Some(Refusal::Malformed)
    );
}

/// The bytes a mapped batch holds (strings and values), for the bounded-output checks.
fn mapped_bytes(b: &Batch) -> usize {
    fn attrs(a: &[Attribute]) -> usize {
        a.iter().map(|x| x.key.len() + x.value.size() + 16).sum()
    }
    b.logs
        .iter()
        .map(|r| {
            64 + attrs(&r.attributes)
                + match &r.body {
                    LogBody::Text(t) => t.len(),
                    LogBody::Template { template, fields } => template.len() + attrs(fields),
                }
        })
        .sum::<usize>()
        + b.metrics
            .iter()
            .map(|r| 64 + r.name.len() + attrs(&r.labels))
            .sum::<usize>()
        + b.spans
            .iter()
            .map(|s| {
                64 + s.name.len()
                    + attrs(&s.attributes)
                    + s.events
                        .iter()
                        .map(|e| 16 + e.name.len() + attrs(&e.attributes))
                        .sum::<usize>()
                    + s.links
                        .iter()
                        .map(|l| 64 + attrs(&l.attributes))
                        .sum::<usize>()
            })
            .sum::<usize>()
}

/// T-56: each signal on its own budget. A large resource attribute is copied onto every record;
/// the mapping stops at the cap, counts the rest, and its output stays within a small multiple
/// of the cap (checked by its ratio to a small cap, not by one absolute size).
#[test]
fn the_mapping_is_bounded_per_signal() {
    let cap = 16 * 1024;
    let lim = Limits {
        max_batch_bytes: cap,
        ..Limits::default()
    };
    let big = vec![skv("big", &"r".repeat(4000))];
    let cases: Vec<(&str, std::result::Result<otlp::Mapped, Refusal>)> = vec![
        (
            "logs",
            otlp::map_logs(
                ExportLogsServiceRequest::decode(
                    logs_request(
                        big.clone(),
                        (0..200)
                            .map(|i| log_record(&i.to_string(), vec![]))
                            .collect(),
                    )
                    .as_slice(),
                )
                .unwrap(),
                &lim,
            ),
        ),
        (
            "metrics",
            otlp::map_metrics(
                ExportMetricsServiceRequest::decode(
                    metrics_request(big.clone(), (0..200).map(|_| point(vec![])).collect())
                        .as_slice(),
                )
                .unwrap(),
                &lim,
            ),
        ),
        (
            "spans with events and links",
            otlp::map_traces(
                ExportTraceServiceRequest::decode(
                    traces_request(big.clone(), (0..200).map(|_| span(vec![], 3, 2)).collect())
                        .as_slice(),
                )
                .unwrap(),
                &lim,
            ),
        ),
    ];
    for (name, m) in cases {
        let m = m.unwrap();
        let mapped = m.batch.records();
        let dropped = m
            .dropped
            .get(&DropReason::BatchTooLarge)
            .copied()
            .unwrap_or(0);
        assert!(mapped > 0, "{name}: some records are mapped");
        assert!(dropped > 0, "{name}: the rest is dropped");
        assert_eq!(
            mapped as u64 + dropped,
            200,
            "{name}: every record is counted"
        );
        let ratio = mapped_bytes(&m.batch) as f64 / cap as f64;
        assert!(ratio <= 1.5, "{name}: mapped {ratio:.2} times the cap");
    }
}

fn av(v: any_value::Value) -> AnyValue {
    AnyValue { value: Some(v) }
}

fn control_array(n: usize, len: usize) -> AnyValue {
    av(any_value::Value::ArrayValue(
        opentelemetry_proto::tonic::common::v1::ArrayValue {
            values: (0..n)
                .map(|_| av(any_value::Value::StringValue("\u{1}".repeat(len))))
                .collect(),
        },
    ))
}

/// Kimi run 13 finding 1: arrays and key value lists become JSON text, in which a control
/// character takes six bytes (`\u0001`). The mapping counts the JSON text, so the mapped
/// batch stays within the cap for a log body and for attributes of each signal.
#[test]
fn json_text_of_control_characters_stays_within_the_mapping_cap() {
    let cap = 16 * 1024;
    let lim = Limits {
        max_batch_bytes: cap,
        ..Limits::default()
    };
    let arr = control_array(10, 100);
    let attr = KeyValue {
        key: "arr".into(),
        value: Some(arr.clone()),
        ..Default::default()
    };
    let mut body_records: Vec<ol::LogRecord> = (0..50).map(|_| log_record("x", vec![])).collect();
    for r in &mut body_records {
        r.body = Some(arr.clone());
    }
    let cases: Vec<(&str, otlp::Mapped)> = vec![
        (
            "log body",
            otlp::map_logs(
                ExportLogsServiceRequest::decode(logs_request(vec![], body_records).as_slice())
                    .unwrap(),
                &lim,
            )
            .unwrap(),
        ),
        (
            "log attribute",
            otlp::map_logs(
                ExportLogsServiceRequest::decode(
                    logs_request(
                        vec![],
                        (0..50)
                            .map(|_| log_record("x", vec![attr.clone()]))
                            .collect(),
                    )
                    .as_slice(),
                )
                .unwrap(),
                &lim,
            )
            .unwrap(),
        ),
        (
            "metric label",
            otlp::map_metrics(
                ExportMetricsServiceRequest::decode(
                    metrics_request(vec![], (0..50).map(|_| point(vec![attr.clone()])).collect())
                        .as_slice(),
                )
                .unwrap(),
                &lim,
            )
            .unwrap(),
        ),
        (
            "span attribute",
            otlp::map_traces(
                ExportTraceServiceRequest::decode(
                    traces_request(
                        vec![],
                        (0..50).map(|_| span(vec![attr.clone()], 0, 0)).collect(),
                    )
                    .as_slice(),
                )
                .unwrap(),
                &lim,
            )
            .unwrap(),
        ),
    ];
    for (name, m) in cases {
        assert!(m.batch.records() > 0, "{name}: some records are mapped");
        assert!(
            m.dropped
                .get(&DropReason::BatchTooLarge)
                .copied()
                .unwrap_or(0)
                > 0,
            "{name}: the rest is dropped"
        );
        let got = mapped_bytes(&m.batch);
        assert!(
            got <= m.bytes as usize && m.bytes as usize <= cap,
            "{name}: mapped {got} bytes, counted {}, cap {cap}",
            m.bytes
        );
    }
}

/// The JSON text of OTLP values is written without a tree in between, in the same form as
/// before (sorted keys, the last of a repeated key, non-finite doubles as strings, bytes as
/// hex), and never longer than its count.
#[test]
fn otlp_values_render_as_compact_json_within_their_count() {
    use opentelemetry_proto::tonic::common::v1::{ArrayValue, KeyValueList};
    let kv = |k: &str, v: Option<AnyValue>| KeyValue {
        key: k.into(),
        value: v,
        ..Default::default()
    };
    let list = av(any_value::Value::KvlistValue(KeyValueList {
        values: vec![
            kv("b", Some(av(any_value::Value::DoubleValue(f64::NAN)))),
            kv("a", Some(av(any_value::Value::IntValue(1)))),
            kv("a", Some(av(any_value::Value::IntValue(2)))),
            kv("c", None),
            kv(
                "d",
                Some(av(any_value::Value::BytesValue(vec![0xab, 0x01]))),
            ),
            kv(
                "e",
                Some(av(any_value::Value::ArrayValue(ArrayValue {
                    values: vec![
                        av(any_value::Value::BoolValue(true)),
                        av(any_value::Value::DoubleValue(1.5)),
                        av(any_value::Value::StringValue("q\"\\\n\u{1}é".into())),
                        AnyValue { value: None },
                    ],
                }))),
            ),
        ],
    }));
    assert_eq!(
        otlp::render_json(&list),
        r#"{"a":2,"b":"NaN","c":null,"d":"ab01","e":[true,1.5,"q\"\\\n\u0001é",null]}"#
    );
    let extremes = av(any_value::Value::ArrayValue(ArrayValue {
        values: vec![
            av(any_value::Value::IntValue(i64::MIN)),
            av(any_value::Value::DoubleValue(-f64::MAX)),
            av(any_value::Value::DoubleValue(-f64::MIN_POSITIVE)),
            av(any_value::Value::DoubleValue(5e-324)),
            av(any_value::Value::DoubleValue(f64::NEG_INFINITY)),
            av(any_value::Value::StringValue("\u{1f}\u{7f}\"\\".repeat(50))),
        ],
    }));
    for v in [list, extremes, control_array(20, 30)] {
        let text = otlp::render_json(&v);
        assert!(
            text.len() <= otlp::json_cost(&v),
            "{} > {}: {text}",
            text.len(),
            otlp::json_cost(&v)
        );
    }
}

/// The caller holds the mapped batch before it is mapped: in steps, never more than the cap,
/// at least what the mapping counted; a refusal refuses the request.
#[test]
fn the_mapped_batch_is_held_before_it_is_mapped() {
    let lim = Limits {
        max_batch_bytes: 1024 * 1024,
        ..Limits::default()
    };
    let body = logs_request(
        vec![skv("big", &"r".repeat(4000))],
        (0..100)
            .map(|i| log_record(&i.to_string(), vec![]))
            .collect(),
    );
    let mut held = 0u64;
    let m = otlp::check(Signal::Logs, &body, &lim)
        .unwrap()
        .decode_held(&lim, &mut |n| {
            held += n;
            true
        })
        .unwrap();
    assert_eq!(m.batch.records(), 100);
    assert!(
        held >= m.bytes && held <= lim.max_batch_bytes as u64,
        "{held} {}",
        m.bytes
    );
    let refused = otlp::check(Signal::Logs, &body, &lim)
        .unwrap()
        .decode_held(&lim, &mut |_| false);
    assert_eq!(refused.err(), Some(Refusal::NotHeld));
}

// ---------------------------------------------------------------------------- sources

fn facts(ip: [u8; 4], listener: &str, bearer: Option<&str>) -> SourceFacts {
    SourceFacts {
        peer: SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), 40000),
        listener: ListenerId::new(listener),
        bearer: bearer.map(BearerToken::new),
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

#[tokio::test]
async fn sources_are_authenticated_by_address_and_listener_only() {
    let map = Arc::new(MemorySourceAuthenticator::new());
    let me = identity(Environment::Preview);
    let ip = IpAddr::V4(Ipv4Addr::new(10, 1, 0, 5));
    map.insert(ListenerId::new("net-a"), ip, workload(&me));
    let auth = NetworkSourceAuthenticator::new(map.lookup());
    for a in [&auth as &dyn SourceAuthenticator, map.as_ref()] {
        // Positive control first.
        assert_eq!(
            a.authenticate(&facts([10, 1, 0, 5], "net-a", None)).await,
            Ok(me)
        );
        // Another address, another listener: refused, never a default identity.
        assert_eq!(
            a.authenticate(&facts([10, 1, 0, 6], "net-a", None)).await,
            Err(SourceError::UnknownSource)
        );
        assert_eq!(
            a.authenticate(&facts([10, 1, 0, 5], "net-b", None)).await,
            Err(SourceError::UnknownSource)
        );
        // A bearer token alone, or next to a known address: refused until card R-3.
        assert_eq!(
            a.authenticate(&facts([10, 9, 9, 9], "net-a", Some("t")))
                .await,
            Err(SourceError::BearerNotSupported)
        );
        assert_eq!(
            a.authenticate(&facts([10, 1, 0, 5], "net-a", Some("t")))
                .await,
            Err(SourceError::BearerNotSupported)
        );
    }
    // A stopped instance's old address is refused.
    map.remove(&ListenerId::new("net-a"), ip);
    assert_eq!(
        auth.authenticate(&facts([10, 1, 0, 5], "net-a", None))
            .await,
        Err(SourceError::UnknownSource)
    );
    assert_eq!(
        NetworkSourceAuthenticator::refuse_all()
            .authenticate(&facts([127, 0, 0, 1], "local", None))
            .await,
        Err(SourceError::UnknownSource)
    );
    assert!(!format!("{:?}", BearerToken::new("visible-token")).contains("visible"));
}

// ---------------------------------------------------------------------------- budget

#[test]
fn a_flood_of_one_identity_leaves_room_for_another_tenant() {
    let budget = FairBudget::new(BudgetConfig {
        requests_per_second: 2,
        bytes_per_second: 1000,
        burst_seconds: 2,
        max_in_flight_bytes: 4000,
        tenant_share: 4,
        max_identities: 10,
    });
    let a = identity(Environment::Live);
    let b = identity(Environment::Live);
    // The rate: four requests (2/s, 2 s burst), then refused.
    for _ in 0..4 {
        budget.admit_request(&a).unwrap();
    }
    assert_eq!(budget.admit_request(&a), Err(BudgetRefusal::IdentityRate));
    assert_eq!(
        budget.admit_request(&b),
        Ok(()),
        "another tenant is still taken"
    );
    // The share: a tenant holds at most 4000 / 4 bytes in flight.
    let a2 = SourceIdentity {
        deployment: Uuid::now_v7(),
        ..a
    };
    let held = budget.admit_bytes(&a2, 1000).unwrap();
    assert_eq!(
        budget.admit_bytes(&a2, 1).err(),
        Some(BudgetRefusal::TenantShare)
    );
    let a3 = SourceIdentity {
        deployment: Uuid::now_v7(),
        ..a
    };
    assert_eq!(
        budget.admit_bytes(&a3, 1).err(),
        Some(BudgetRefusal::TenantShare)
    );
    let b_permit = budget.admit_bytes(&b, 1000);
    assert!(b_permit.is_ok(), "another tenant's batch is still taken");
    drop(held);
    assert!(budget.admit_bytes(&a3, 1).is_ok(), "the share comes back");
}

/// A permit grows while a body arrives and is refused at the tenant's share, keeping what it
/// held; shrinking gives bytes back.
#[test]
fn a_held_permit_grows_within_the_tenant_share() {
    let budget = FairBudget::new(BudgetConfig {
        max_in_flight_bytes: 4000,
        tenant_share: 4,
        ..BudgetConfig::default()
    });
    let a = identity(Environment::Live);
    let b = identity(Environment::Live);
    let mut p = budget.hold(&a, 0).unwrap();
    p.grow_to(600).unwrap();
    p.grow(300).unwrap();
    assert_eq!(p.bytes(), 900);
    assert_eq!(p.grow(200), Err(BudgetRefusal::TenantShare));
    assert_eq!(p.bytes(), 900, "a refused growth keeps what was held");
    assert_eq!(
        budget.hold(&a, 200).err(),
        Some(BudgetRefusal::TenantShare),
        "another request of A does not fit"
    );
    assert!(budget.hold(&b, 1000).is_ok(), "B is taken");
    p.shrink_to(100);
    assert!(budget.hold(&a, 800).is_ok(), "the share comes back");
    // The byte rate is separate from the hold.
    assert!(budget.take_bytes(&a, 10).is_ok());
}

#[test]
fn the_budget_tracks_a_bounded_number_of_identities() {
    let budget = FairBudget::new(BudgetConfig {
        max_identities: 3,
        ..BudgetConfig::default()
    });
    let ids: Vec<SourceIdentity> = (0..3).map(|_| identity(Environment::Live)).collect();
    for id in &ids {
        budget.admit_request(id).unwrap();
    }
    // The buckets are not full again yet (requests were taken a moment ago): refused.
    assert_eq!(
        budget.admit_request(&identity(Environment::Live)),
        Err(BudgetRefusal::Global)
    );
}

// ---------------------------------------------------------------------------- shipper

async fn start_job(
    driver: &MemoryDriver,
    registry: &MemoryRegistry,
    id: &WorkloadIdentity,
    secret: &str,
    lines: &[String],
) -> InstanceId {
    let digest = OciDigest::of(b"o1b shipper");
    registry.push(id, &digest);
    let mut steps: Vec<String> = lines.iter().map(|l| format!("print:{l}")).collect();
    steps.push("env:APP_SECRET".into());
    steps.push("sleep:60000".into());
    let st = driver
        .start(WorkloadSpec {
            identity: *id,
            kind: WorkloadKind::Job(JobSpec {
                timeout: Duration::from_secs(120),
            }),
            image: ImageRef::Oci(digest),
            command: steps,
            env: vec![EnvVar::secret("APP_SECRET", SecretValue::new(secret))],
            previous_secrets: Vec::new(),
            workload_token: SecretValue::new(["wl", "tok-", "o1b-shipper-0001"].concat()),
            limits: RtLimits {
                cpu_millis: 100,
                memory_bytes: 1 << 26,
                disk_bytes: 1 << 20,
            },
            routes: Vec::new(),
        })
        .await
        .unwrap();
    st.id
}

async fn wait_lines(driver: &MemoryDriver, tenant: TenantId, i: InstanceId, n: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let got = driver.logs(tenant, i, None, 1000).await.unwrap().len();
        if got >= n || Instant::now() > deadline {
            assert!(got >= n, "the driver has {got} of {n} lines");
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn the_shipper_stores_driver_lines_as_the_driver_identity() {
    let f = Fixture::new();
    let registry = MemoryRegistry::new();
    let driver = Arc::new(MemoryDriver::new(
        registry.clone(),
        Arc::new(MemoryUsage::new()),
    ));
    let a = identity(Environment::Preview);
    let b = identity(Environment::Live);
    let secret = ["sh1pper", "-Secret-", "9f3K"].concat();
    // An app that prints another app's ruum.* claims in its text, and its environment.
    let lines: Vec<String> = vec![
        format!("ruum.tenant={} ruum.environment=live", b.tenant),
        "user alice@example.com logged in".into(),
    ];
    let ia = start_job(&driver, &registry, &workload(&a), &secret, &lines).await;
    let ib = start_job(
        &driver,
        &registry,
        &workload(&b),
        &secret,
        &["b line".into()],
    )
    .await;
    wait_lines(&driver, a.tenant, ia, 3).await;
    wait_lines(&driver, b.tenant, ib, 2).await;
    let list = vec![(a.tenant, ia), (b.tenant, ib)];
    let positions = Arc::new(MemoryPositions::new());
    let shipper = LogShipper::new(
        driver.clone(),
        Arc::new(move || list.clone()),
        f.ingest.clone(),
        positions.clone(),
        ShipperConfig::default(),
    )
    .await
    .unwrap();
    let r = shipper.tick().await;
    assert_eq!(r.lines, 5);
    assert_eq!(r.stored, 5);
    let got = f
        .store
        .read(a.tenant, &TelemetryFilter::app(a.app))
        .await
        .unwrap();
    assert_eq!(got.logs.len(), 3, "positive control: the lines are stored");
    for l in &got.logs {
        assert_eq!(l.source, a);
        assert_own_identity(&l.attributes, &a);
        // Unclassified, so `personal`: a pseudonym until the app classifies it (card O-2).
        assert!(attr(&l.attributes, processor::ATTR_IOSTREAM).is_some());
    }
    // The free-text scrubber ran (the email became a pseudonym), and B's claimed tenant got
    // nothing from A.
    assert!(
        got.logs
            .iter()
            .all(|l| !l.text.as_deref().unwrap_or("").contains("alice@"))
    );
    let b_got = f
        .store
        .read(b.tenant, &TelemetryFilter::default())
        .await
        .unwrap();
    assert_eq!(b_got.logs.len(), 2);
    assert!(b_got.logs.iter().all(|l| l.source == b));
    let found = conformance::scan_for_secrets(&f.store.dump(), std::slice::from_ref(&secret));
    assert!(found.is_empty(), "{found:?}");
    // Usage: charged to each app.
    assert!(
        f.usage
            .total(a.tenant, a.app, Charged::App, Meter::TelemetryBytes)
            > 0
    );
}

#[tokio::test]
async fn a_shipper_restart_neither_loses_nor_repeats() {
    let f = Fixture::new();
    let registry = MemoryRegistry::new();
    let driver = Arc::new(MemoryDriver::new(
        registry.clone(),
        Arc::new(MemoryUsage::new()),
    ));
    let a = identity(Environment::Live);
    let lines: Vec<String> = (0..7).map(|i| format!("line {i}")).collect();
    let ia = start_job(&driver, &registry, &workload(&a), "unused-secret-1", &lines).await;
    wait_lines(&driver, a.tenant, ia, 8).await;
    let positions = Arc::new(MemoryPositions::new());
    let cfg = ShipperConfig {
        lines_per_instance: 3,
        ..ShipperConfig::default()
    };
    let list = vec![(a.tenant, ia)];
    let l1 = list.clone();
    let first = LogShipper::new(
        driver.clone(),
        Arc::new(move || l1.clone()),
        f.ingest.clone(),
        positions.clone(),
        cfg,
    )
    .await
    .unwrap();
    assert_eq!(first.tick().await.stored, 3);
    let saved = first.position(ia).await;
    drop(first);
    // A new shipper (a restart) starts at the saved position.
    let second = LogShipper::new(
        driver.clone(),
        Arc::new(move || list.clone()),
        f.ingest.clone(),
        positions,
        cfg,
    )
    .await
    .unwrap();
    assert_eq!(second.position(ia).await, saved);
    while second.tick().await.lines > 0 {}
    let got = f
        .store
        .read(a.tenant, &TelemetryFilter::app(a.app))
        .await
        .unwrap();
    let texts: Vec<&str> = got.logs.iter().filter_map(|l| l.text.as_deref()).collect();
    assert_eq!(texts.len(), 8, "every line once: {texts:?}");
    let mut uniq = texts.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), 8, "no line twice");
}

#[tokio::test]
async fn the_shipper_shares_a_tick_between_tenants() {
    let f = Fixture::new();
    let registry = MemoryRegistry::new();
    let driver = Arc::new(MemoryDriver::new(
        registry.clone(),
        Arc::new(MemoryUsage::new()),
    ));
    let a = identity(Environment::Live);
    let b = identity(Environment::Live);
    let many: Vec<String> = (0..40).map(|i| format!("a {i}")).collect();
    let mut list = Vec::new();
    for _ in 0..3 {
        let i = start_job(&driver, &registry, &workload(&a), "unused-secret-2", &many).await;
        wait_lines(&driver, a.tenant, i, 41).await;
        list.push((a.tenant, i));
    }
    let ib = start_job(
        &driver,
        &registry,
        &workload(&b),
        "unused-secret-3",
        &["b".into()],
    )
    .await;
    wait_lines(&driver, b.tenant, ib, 2).await;
    list.push((b.tenant, ib));
    let shipper = LogShipper::new(
        driver.clone(),
        Arc::new(move || list.clone()),
        f.ingest.clone(),
        Arc::new(MemoryPositions::new()),
        ShipperConfig {
            lines_per_instance: 100,
            lines_per_tick: 20,
            ..ShipperConfig::default()
        },
    )
    .await
    .unwrap();
    let r = shipper.tick().await;
    // Tenant A gets its half (10 lines) however many instances it has; B gets its lines.
    assert!(r.lines <= 20, "{} lines in one tick", r.lines);
    let b_got = f
        .store
        .read(b.tenant, &TelemetryFilter::default())
        .await
        .unwrap();
    assert_eq!(b_got.logs.len(), 2, "tenant B is shipped in the same tick");
}

/// Review of O-1b: within one tenant the instances take turns. An instance that always has
/// more lines than the tenant's share of a tick does not starve the instance listed after it.
#[tokio::test]
async fn a_busy_instance_does_not_starve_the_next_one() {
    let f = Fixture::new();
    let registry = MemoryRegistry::new();
    let driver = Arc::new(MemoryDriver::new(
        registry.clone(),
        Arc::new(MemoryUsage::new()),
    ));
    let a = identity(Environment::Live);
    let flood: Vec<String> = (0..100).map(|i| format!("busy {i}")).collect();
    let busy = start_job(&driver, &registry, &workload(&a), "unused-secret-4", &flood).await;
    wait_lines(&driver, a.tenant, busy, 101).await;
    let quiet_id = SourceIdentity {
        deployment: Uuid::now_v7(),
        ..a
    };
    let quiet = start_job(
        &driver,
        &registry,
        &workload(&quiet_id),
        "unused-secret-5",
        &["quiet line".into()],
    )
    .await;
    wait_lines(&driver, a.tenant, quiet, 2).await;
    // The busy instance first in the list, every tick.
    let list = vec![(a.tenant, busy), (a.tenant, quiet)];
    let shipper = LogShipper::new(
        driver.clone(),
        Arc::new(move || list.clone()),
        f.ingest.clone(),
        Arc::new(MemoryPositions::new()),
        ShipperConfig {
            lines_per_instance: 100,
            lines_per_tick: 10,
            ..ShipperConfig::default()
        },
    )
    .await
    .unwrap();
    let quiet_lines = || async {
        f.store
            .read(
                a.tenant,
                &TelemetryFilter {
                    deployment: Some(quiet_id.deployment),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .logs
            .len()
    };
    let first = shipper.tick().await;
    assert_eq!(
        first.lines, 10,
        "positive control: the busy instance took the tick"
    );
    assert_eq!(quiet_lines().await, 0);
    shipper.tick().await;
    assert_eq!(
        quiet_lines().await,
        2,
        "the quiet instance is shipped on the next tick"
    );
    // And the busy one continues after it.
    let busy_got = f
        .store
        .read(
            a.tenant,
            &TelemetryFilter {
                deployment: Some(a.deployment),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .logs
        .len();
    assert!(busy_got >= 18, "{busy_got}");
}

/// A [`PositionStore`] whose saves fail while `failing` is set (the disk is full, the volume
/// is read-only).
struct FlakyPositions {
    inner: MemoryPositions,
    failing: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl PositionStore for FlakyPositions {
    async fn load(&self) -> Result<HashMap<InstanceId, u64>> {
        self.inner.load().await
    }

    async fn save(&self, positions: &HashMap<InstanceId, u64>) -> Result<()> {
        if self.failing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(crate::Error::Internal("test: the save fails".into()));
        }
        self.inner.save(positions).await
    }
}

/// Kimi run 13 finding 3: when saving the positions fails, the shipper ships nothing more
/// until a save works, so a restart repeats at most one batch, not every batch since the last
/// save.
#[tokio::test]
async fn a_failed_position_save_stops_shipping_until_a_save_works() {
    let f = Fixture::new();
    let registry = MemoryRegistry::new();
    let driver = Arc::new(MemoryDriver::new(
        registry.clone(),
        Arc::new(MemoryUsage::new()),
    ));
    let a = identity(Environment::Live);
    let lines: Vec<String> = (0..11).map(|i| format!("line {i}")).collect();
    let ia = start_job(&driver, &registry, &workload(&a), "unused-secret-6", &lines).await;
    wait_lines(&driver, a.tenant, ia, 12).await;
    let positions = Arc::new(FlakyPositions {
        inner: MemoryPositions::new(),
        failing: std::sync::atomic::AtomicBool::new(false),
    });
    let cfg = ShipperConfig {
        lines_per_instance: 3,
        ..ShipperConfig::default()
    };
    let list = vec![(a.tenant, ia)];
    let l1 = list.clone();
    let first = LogShipper::new(
        driver.clone(),
        Arc::new(move || l1.clone()),
        f.ingest.clone(),
        positions.clone(),
        cfg,
    )
    .await
    .unwrap();
    let r = first.tick().await;
    assert_eq!((r.lines, r.positions_unsaved), (3, false));
    positions
        .failing
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let r = first.tick().await;
    assert_eq!(r.lines, 3, "the batch of the failed save was shipped");
    assert!(r.positions_unsaved);
    for _ in 0..3 {
        let r = first.tick().await;
        assert_eq!(r.lines, 0, "nothing is shipped while the save fails");
        assert!(r.positions_unsaved);
    }
    // A crash now, then a restart with a working disk: one batch (3 lines) is repeated.
    drop(first);
    positions
        .failing
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let second = LogShipper::new(
        driver.clone(),
        Arc::new(move || list.clone()),
        f.ingest.clone(),
        positions.clone(),
        cfg,
    )
    .await
    .unwrap();
    while second.tick().await.lines > 0 {}
    let got = f
        .store
        .read(a.tenant, &TelemetryFilter::app(a.app))
        .await
        .unwrap();
    let mut texts: Vec<&str> = got.logs.iter().filter_map(|l| l.text.as_deref()).collect();
    texts.sort();
    let all = texts.len();
    texts.dedup();
    assert_eq!(texts.len(), 12, "every line is stored");
    assert!(
        all - texts.len() <= cfg.lines_per_instance,
        "a restart repeats at most one batch: {} repeated",
        all - texts.len()
    );
}

/// Positive control of the test above: once the save works again, the same shipper goes on
/// without repeating a line.
#[tokio::test]
async fn the_shipper_goes_on_once_the_save_works_again() {
    let f = Fixture::new();
    let registry = MemoryRegistry::new();
    let driver = Arc::new(MemoryDriver::new(
        registry.clone(),
        Arc::new(MemoryUsage::new()),
    ));
    let a = identity(Environment::Live);
    let lines: Vec<String> = (0..7).map(|i| format!("line {i}")).collect();
    let ia = start_job(&driver, &registry, &workload(&a), "unused-secret-7", &lines).await;
    wait_lines(&driver, a.tenant, ia, 8).await;
    let positions = Arc::new(FlakyPositions {
        inner: MemoryPositions::new(),
        failing: std::sync::atomic::AtomicBool::new(true),
    });
    let shipper = LogShipper::new(
        driver.clone(),
        Arc::new(move || vec![(a.tenant, ia)]),
        f.ingest.clone(),
        positions.clone(),
        ShipperConfig {
            lines_per_instance: 3,
            ..ShipperConfig::default()
        },
    )
    .await
    .unwrap();
    assert!(shipper.tick().await.positions_unsaved);
    assert_eq!(shipper.tick().await.lines, 0);
    positions
        .failing
        .store(false, std::sync::atomic::Ordering::SeqCst);
    while shipper.tick().await.lines > 0 {}
    assert_eq!(
        positions.inner.load().await.unwrap().get(&ia).copied(),
        shipper.position(ia).await,
        "the position is saved"
    );
    let got = f
        .store
        .read(a.tenant, &TelemetryFilter::app(a.app))
        .await
        .unwrap();
    let mut texts: Vec<&str> = got.logs.iter().filter_map(|l| l.text.as_deref()).collect();
    texts.sort();
    let all = texts.len();
    texts.dedup();
    assert_eq!((all, texts.len()), (8, 8), "every line once");
}

/// Kimi run 13 finding 4: `lines_per_tick` is a hard cap also when there are more tenants
/// than lines; the tenants take turns over the ticks, and a cap of 0 ships nothing.
#[tokio::test]
async fn lines_per_tick_holds_with_more_tenants_than_lines() {
    let f = Fixture::new();
    let registry = MemoryRegistry::new();
    let driver = Arc::new(MemoryDriver::new(
        registry.clone(),
        Arc::new(MemoryUsage::new()),
    ));
    let mut list = Vec::new();
    let mut ids = Vec::new();
    for k in 0..3 {
        let id = identity(Environment::Live);
        let i = start_job(
            &driver,
            &registry,
            &workload(&id),
            "unused-secret-8",
            &[format!("tenant {k}")],
        )
        .await;
        wait_lines(&driver, id.tenant, i, 2).await;
        list.push((id.tenant, i));
        ids.push(id);
    }
    let shipper = |cap: usize| {
        let list = list.clone();
        let driver = driver.clone();
        let ingest = f.ingest.clone();
        async move {
            LogShipper::new(
                driver,
                Arc::new(move || list.clone()),
                ingest,
                Arc::new(MemoryPositions::new()),
                ShipperConfig {
                    lines_per_instance: 100,
                    lines_per_tick: cap,
                    ..ShipperConfig::default()
                },
            )
            .await
            .unwrap()
        }
    };
    let zero = shipper(0).await;
    assert_eq!(zero.tick().await.lines, 0, "a cap of 0 ships nothing");
    let two = shipper(2).await;
    let mut ticks = Vec::new();
    for _ in 0..3 {
        ticks.push(two.tick().await.lines);
    }
    assert!(
        ticks.iter().all(|n| *n <= 2),
        "at most 2 lines per tick: {ticks:?}"
    );
    // Every tenant was reached within three ticks (each has 2 lines).
    for id in &ids {
        let got = f
            .store
            .read(id.tenant, &TelemetryFilter::default())
            .await
            .unwrap();
        assert!(!got.logs.is_empty(), "every tenant gets a turn");
    }
}

/// Kimi run 13 finding 5: `max_positions` is a hard cap, also when the listed instances alone
/// are more, and on saved positions loaded at start.
#[tokio::test]
async fn max_positions_is_a_hard_cap() {
    let f = Fixture::new();
    let registry = MemoryRegistry::new();
    let driver = Arc::new(MemoryDriver::new(
        registry.clone(),
        Arc::new(MemoryUsage::new()),
    ));
    let a = identity(Environment::Live);
    let mut list = Vec::new();
    for k in 0..3 {
        let id = SourceIdentity {
            deployment: Uuid::now_v7(),
            ..a
        };
        let i = start_job(
            &driver,
            &registry,
            &workload(&id),
            "unused-secret-9",
            &[format!("instance {k}")],
        )
        .await;
        wait_lines(&driver, a.tenant, i, 2).await;
        list.push((a.tenant, i));
    }
    let cfg = ShipperConfig {
        max_positions: 2,
        ..ShipperConfig::default()
    };
    let l1 = list.clone();
    let shipper = LogShipper::new(
        driver.clone(),
        Arc::new(move || l1.clone()),
        f.ingest.clone(),
        Arc::new(MemoryPositions::new()),
        cfg,
    )
    .await
    .unwrap();
    for _ in 0..3 {
        let r = shipper.tick().await;
        assert!(shipper.positions_held().await <= 2, "{r:?}");
    }
    assert_eq!(shipper.tick().await.skipped_instances, 1);
    // Saved positions over the cap (the cap was lowered) are cut on load.
    let saved = Arc::new(MemoryPositions::new());
    let over: HashMap<InstanceId, u64> = list.iter().map(|(_, i)| (*i, 1)).collect();
    saved.save(&over).await.unwrap();
    let restarted = LogShipper::new(
        driver.clone(),
        Arc::new(move || list.clone()),
        f.ingest.clone(),
        saved,
        cfg,
    )
    .await
    .unwrap();
    assert_eq!(restarted.positions_held().await, 2);
}

// ---------------------------------------------------------------------------- secrets

/// S-59 and T-44: the secret matcher on a hostile text of the value cap (many `%`, `+`, `\`)
/// finishes well within a second.
#[test]
fn the_secret_matcher_cost_is_bounded_on_hostile_text() {
    let secrets = conformance::fixture_secrets();
    let m = secrets::SecretMatcher::new(secrets.iter().map(String::as_str));
    let unit = "%2B+\\%5C%25+";
    let text: String = unit.repeat(scrub::MAX_TEXT_BYTES / unit.len());
    assert!(text.len() <= scrub::MAX_TEXT_BYTES);
    let t = Instant::now();
    for _ in 0..3 {
        assert!(!m.contains(&text));
        let _ = m.redact(&text);
    }
    let took = t.elapsed();
    assert!(took < Duration::from_secs(3), "three passes took {took:?}");
}

/// T-49: the forms of real library encoders (Python `urllib.parse.quote`, `quote_plus`,
/// `quote(safe="")`, `json.dumps`, JS `encodeURI` and `escape`), taken from the libraries'
/// output for the fixture secrets, never reach the store.
#[tokio::test]
async fn library_encodings_of_secrets_never_reach_the_store() {
    let f = Fixture::new();
    let id = identity(Environment::Live);
    let secrets = conformance::fixture_secrets();
    f.secrets.set(
        id.tenant,
        id.app,
        secrets.iter().cloned().map(SecretValue::new).collect(),
    );
    let forms: Vec<String> = vec![
        ["Fx7pQ2/wire", "%2B9Lm3%20Zt8v-k5Rb"].concat(),
        ["Fx7pQ2%2Fwire", "%2B9Lm3+Zt8v-k5Rb"].concat(),
        ["Fx7pQ2%2Fwire", "%2B9Lm3%20Zt8v-k5Rb"].concat(),
        ["Fx7pQ2/wire", "+9Lm3%20Zt8v-k5Rb"].concat(),
        ["prev%22Qt%5CSl", "-4Hn8-Wc2m-Jy6d"].concat(),
        ["\"prev\\\"Qt\\\\Sl", "-4Hn8-Wc2m-Jy6d\""].concat(),
    ];
    for i in 0..forms.len() {
        f.classes.set(
            id.tenant,
            id.app,
            FieldKind::LogField,
            &format!("lib{i}"),
            FieldClassEntry::confirmed(FieldClass::Normal),
        );
    }
    let attrs: Vec<KeyValue> = forms
        .iter()
        .enumerate()
        .map(|(i, v)| skv(&format!("lib{i}"), v))
        .collect();
    let text = forms.join(" ");
    let m = otlp::decode_logs(
        &logs_request(vec![], vec![log_record(&text, attrs)]),
        &Limits::default(),
    )
    .unwrap();
    let r = f.ingest.ingest(&id, m.batch).await.unwrap();
    assert_eq!(r.stored_logs, 1, "positive control: stored");
    assert!(r.secrets_redacted > 0);
    let found = conformance::scan_for_secrets(&f.store.dump(), &secrets);
    assert!(found.is_empty(), "{found:?}");
    for form in &forms {
        assert!(
            !String::from_utf8_lossy(&f.store.dump()).contains(form.as_str()),
            "a library form reached the store"
        );
    }
}

// ---------------------------------------------------------------------------- escaping

/// The reverse of [`quote`]: a Go string literal reader for the escapes `quote` writes.
fn unquote(q: &str) -> Option<String> {
    let inner = q.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::new();
    let mut it = inner.chars();
    while let Some(c) = it.next() {
        match c {
            '"' => return None, // an unescaped quote would end the literal early
            '\\' => match it.next()? {
                '\\' => out.push('\\'),
                '"' => out.push('"'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'x' => {
                    let h: String = it.by_ref().take(2).collect();
                    out.push(char::from_u32(u32::from_str_radix(&h, 16).ok()?)?);
                }
                'u' => {
                    let h: String = it.by_ref().take(4).collect();
                    out.push(char::from_u32(u32::from_str_radix(&h, 16).ok()?)?);
                }
                _ => return None,
            },
            c if (c as u32) < 0x20 => return None,
            c => out.push(c),
        }
    }
    Some(out)
}

#[test]
fn quote_escapes_every_breakout() {
    let hostile = [
        "\"} or {tenant=\"b\"",
        "a\"b",
        "back\\slash\\",
        "new\nline",
        "cr\rtab\t",
        "pipe | and } and ` and {",
        ".*+?()[]{}^$|\\d regex",
        "unicode ü ✓ 𝄞 \u{2028}",
        "nul\u{0} bell\u{7} del\u{7f} c1\u{85}",
        "",
    ];
    for h in hostile {
        let q = quote(h);
        assert_eq!(unquote(&q).as_deref(), Some(h), "{q}");
        assert!(!q[1..q.len() - 1].contains('\n'));
    }
    assert_eq!(quote("a\"b"), r#""a\"b""#);
    assert_eq!(quote("a\\b"), r#""a\\b""#);
    assert_eq!(quote("a\nb"), r#""a\nb""#);
    assert_eq!(quote("x|y}`"), "\"x|y}`\"");
}

proptest::proptest! {
    #[test]
    fn quote_round_trips(s in "\\PC*|[\\x00-\\x7f]{0,40}") {
        let q = quote(&s);
        proptest::prop_assert_eq!(unquote(&q), Some(s));
    }
}

#[test]
fn the_selector_is_built_by_the_server() {
    let app = NodeId::new();
    let dep = Uuid::now_v7();
    let f = TelemetryFilter {
        app: Some(app),
        environment: Some(Environment::Preview),
        deployment: Some(dep),
        fields: vec![FieldEquals {
            key: "\"} or {service_name=~\".+\"} |= \"".into(),
            value: "\"} or {tenant=\"b\"}\n|`".into(),
        }],
        ..Default::default()
    };
    let q = LgtmStore::logql(&f);
    let head = format!(
        "{{service_name=\"{app}\"}} | ruum_environment=\"preview\" | ruum_deployment=\"{dep}\" |= "
    );
    assert!(q.starts_with(&head), "{q}");
    // The rest is one quoted literal that reads back to the line filter text.
    let lit = &q[head.len()..];
    let back = unquote(lit).expect("one literal");
    assert!(back.starts_with("\"k\":"));
    // Field values never go into a query; the tenant never does.
    assert!(!q.contains("tenant=\\\"b"), "{q}");
    let p = LgtmStore::promql(&f, 1000);
    assert_eq!(
        p,
        format!(
            "ruum_app_metric{{ruum_app=\"{app}\",ruum_environment=\"preview\",ruum_deployment=\"{dep}\"}}[1000ms]"
        )
    );
    let t = LgtmStore::traceql(&f);
    assert_eq!(
        t,
        format!(
            "{{ resource.service.name = \"{app}\" && resource.ruum.environment = \"preview\" && resource.ruum.deployment = \"{dep}\" }}"
        )
    );
    let tenant = TenantId::new();
    assert_eq!(
        tenant_header(tenant),
        tenant.as_uuid().hyphenated().to_string()
    );
}

// ---------------------------------------------------------------------------- the real stores

/// The settings of the real stores, or `None` (then the test skips, unless `CI` or
/// `RUUM_TELEMETRY_REQUIRE_STORES=1` is set: then it fails).
fn real_store() -> Option<Arc<LgtmStore>> {
    let get = |n: &str| std::env::var(n).ok().filter(|v| !v.is_empty());
    let required =
        get("CI").is_some() || get("RUUM_TELEMETRY_REQUIRE_STORES").as_deref() == Some("1");
    let collector = get("RUUM_TELEMETRY_COLLECTOR_URL").or_else(|| get("RUUM_OTLP_ENDPOINT"));
    let parts = (
        collector,
        get("RUUM_LOKI_URL"),
        get("RUUM_MIMIR_URL"),
        get("RUUM_TEMPO_URL"),
        get("RUUM_TELEMETRY_COLLECTOR_TOKEN_FILE"),
    );
    let (Some(c), Some(l), Some(m), Some(t), Some(tf)) = parts else {
        assert!(
            !required,
            "the telemetry stores are required (CI or RUUM_TELEMETRY_REQUIRE_STORES=1) but their settings are missing"
        );
        eprintln!(
            "skipped: the telemetry stores are not configured (scripts/dev.sh up --apps; eval \"$(scripts/dev.sh env)\")"
        );
        return None;
    };
    let token = std::fs::read_to_string(&tf).expect("read the Collector token file");
    Some(Arc::new(
        LgtmStore::new(LgtmConfig {
            collector_url: c,
            loki_url: l,
            mimir_url: m,
            tempo_url: t,
            collector_token: SecretValue::new(token.trim()),
            timeout: Duration::from_secs(60),
        })
        .unwrap(),
    ))
}

// ---------------------------------------------------------------------------- bounded reads

/// How a fake store answers.
enum FakeBody {
    /// The whole body with its length.
    Full(Vec<u8>),
    /// Declares this length, sends a little and then nothing more.
    Declared(u64),
    /// Chunked, without end.
    Endless,
}

/// A fake Loki, Mimir and Tempo on one port: `answer` maps a request path (with query) to a
/// status and a body. Counts the requests.
async fn fake_store(
    answer: impl Fn(&str) -> (u16, FakeBody) + Send + Sync + 'static,
) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let answer = Arc::new(answer);
    let n = count.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let answer = answer.clone();
            let n = n.clone();
            tokio::spawn(async move {
                loop {
                    let mut head = Vec::new();
                    let mut b = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        match sock.read(&mut b).await {
                            Ok(1) => head.push(b[0]),
                            _ => return,
                        }
                    }
                    n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let head = String::from_utf8_lossy(&head).to_string();
                    let path = head.split(' ').nth(1).unwrap_or("/").to_owned();
                    let (status, body) = answer(&path);
                    match body {
                        FakeBody::Full(body) => {
                            let h = format!(
                                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\n\r\n",
                                body.len()
                            );
                            if sock.write_all(h.as_bytes()).await.is_err()
                                || sock.write_all(&body).await.is_err()
                            {
                                return;
                            }
                        }
                        FakeBody::Declared(len) => {
                            let h = format!("HTTP/1.1 {status} X\r\nContent-Length: {len}\r\n\r\n");
                            let _ = sock.write_all(h.as_bytes()).await;
                            let _ = sock.write_all(&[0u8; 1024]).await;
                            tokio::time::sleep(Duration::from_secs(3600)).await;
                            return;
                        }
                        FakeBody::Endless => {
                            let h = format!(
                                "HTTP/1.1 {status} X\r\nTransfer-Encoding: chunked\r\n\r\n"
                            );
                            if sock.write_all(h.as_bytes()).await.is_err() {
                                return;
                            }
                            let chunk =
                                [b"10000\r\n".as_slice(), &[0x0a; 0x10000], b"\r\n"].concat();
                            while sock.write_all(&chunk).await.is_ok() {}
                            return;
                        }
                    }
                }
            });
        }
    });
    (url, count)
}

fn fake_lgtm(url: &str, timeout: Duration) -> LgtmStore {
    LgtmStore::new(LgtmConfig {
        collector_url: url.to_owned(),
        loki_url: url.to_owned(),
        mimir_url: url.to_owned(),
        tempo_url: url.to_owned(),
        collector_token: SecretValue::new("unused-collector-token"),
        timeout,
    })
    .unwrap()
}

/// Kimi run 13 finding 2: a store answer over the read cap is refused, whether it declares
/// its length or not; a valid answer under the cap is read.
#[tokio::test]
async fn store_answers_over_the_read_cap_are_refused() {
    let pad = "x".repeat(4096);
    let body = format!(
        r#"{{"status":"success","data":{{"resultType":"matrix","result":[]}},"pad":"{pad}"}}"#
    );
    let (url, _) = fake_store(move |_| (200, FakeBody::Full(body.clone().into_bytes()))).await;
    let f = TelemetryFilter {
        signal: Some(Signal::Metrics),
        ..Default::default()
    };
    let tenant = TenantId::new();
    let store = fake_lgtm(&url, Duration::from_secs(10)).with_answer_caps(1024, 1024);
    assert!(
        store.read(tenant, &f).await.is_err(),
        "a 4 kB answer over a 1 kB cap is refused"
    );
    // Positive control: the same answer under a larger cap is read.
    let roomy = fake_lgtm(&url, Duration::from_secs(10)).with_answer_caps(64 * 1024, 1024);
    assert!(roomy.read(tenant, &f).await.unwrap().is_empty());
}

/// Kimi run 13 finding 2: a Tempo trace answer over the trace cap is skipped without being
/// read whole: one that declares a huge length, and one that never ends. Without the cap the
/// read would wait for the body until the timeout and fail.
#[tokio::test]
async fn tempo_traces_over_the_cap_are_skipped_without_reading_them() {
    let (url, _) = fake_store(|path| {
        if path.starts_with("/api/search") {
            let ids = r#"{"traces":[{"traceID":"0af7651916cd43dd8448eb211c80319c"},{"traceID":"1af7651916cd43dd8448eb211c80319c"}]}"#;
            (200, FakeBody::Full(ids.as_bytes().to_vec()))
        } else if path.contains("0af76519") {
            (200, FakeBody::Declared(1 << 30))
        } else {
            (200, FakeBody::Endless)
        }
    })
    .await;
    let store = fake_lgtm(&url, Duration::from_secs(5)).with_answer_caps(1024 * 1024, 256 * 1024);
    let started = Instant::now();
    let got = store
        .read(
            TenantId::new(),
            &TelemetryFilter {
                signal: Some(Signal::Traces),
                ..Default::default()
            },
        )
        .await
        .expect("the large traces are skipped, not an error");
    assert!(got.spans.is_empty());
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "the bodies were not read to the end"
    );
}

/// Kimi run 13 finding 2: Loki paging stops once the read has its `limit`; without it, the
/// read takes all 20 pages of 5000 lines.
#[tokio::test]
async fn loki_paging_stops_at_the_limit() {
    let f = Fixture::new();
    let a = identity(Environment::Live);
    f.ingest
        .ingest(
            &a,
            Batch {
                logs: vec![LogRecord {
                    time: crate::now(),
                    severity: Severity::Info,
                    body: LogBody::Text("a line".into()),
                    attributes: Vec::new(),
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let stored = f
        .store
        .read(a.tenant, &TelemetryFilter::app(a.app))
        .await
        .unwrap()
        .logs
        .remove(0);
    let line = crate::telemetry::lgtm::wire_log_line(&stored);
    let base = stored.time.unix_timestamp_nanos();
    let page = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let p = page.clone();
    let (url, count) = fake_store(move |_| {
        let k = p.fetch_add(1, std::sync::atomic::Ordering::SeqCst) as i128;
        let values: Vec<serde_json::Value> = (0..5000i128)
            .map(|i| serde_json::json!([(base + k * 5000 + i).to_string(), line]))
            .collect();
        let body = serde_json::json!({
            "status": "success",
            "data": {"resultType": "streams", "result": [{"stream": {}, "values": values}]}
        });
        (200, FakeBody::Full(serde_json::to_vec(&body).unwrap()))
    })
    .await;
    let store = fake_lgtm(&url, Duration::from_secs(30));
    let got = store
        .read(
            a.tenant,
            &TelemetryFilter {
                signal: Some(Signal::Logs),
                limit: Some(10),
                ..TelemetryFilter::app(a.app)
            },
        )
        .await
        .unwrap();
    assert_eq!(got.logs.len(), 10);
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one page holds the limit"
    );
}

/// Ingest that waits until the stores show what it stored (the stores are eventually
/// consistent; Tempo search sees a trace after the ingester cuts it).
struct WaitingIngest {
    inner: Arc<PipelineIngest>,
    store: Arc<LgtmStore>,
    seen: std::sync::Mutex<HashMap<SourceIdentity, (u64, u64, u64)>>,
}

#[async_trait]
impl TelemetryIngest for WaitingIngest {
    async fn ingest(&self, identity: &SourceIdentity, batch: Batch) -> Result<IngestReport> {
        let r = self.inner.ingest(identity, batch).await?;
        let want = {
            let mut seen = self.seen.lock().unwrap();
            let e = seen.entry(*identity).or_default();
            e.0 += r.stored_logs;
            e.1 += r.stored_metrics;
            e.2 += r.stored_spans;
            *e
        };
        wait_visible(&self.store, identity, want).await;
        Ok(r)
    }
}

async fn wait_visible(store: &LgtmStore, id: &SourceIdentity, want: (u64, u64, u64)) {
    let filter = TelemetryFilter {
        app: Some(id.app),
        deployment: Some(id.deployment),
        environment: Some(id.environment),
        ..Default::default()
    };
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if let Ok(b) = store.read(id.tenant, &filter).await {
            let got = (
                b.logs.len() as u64,
                b.metrics.len() as u64,
                b.spans.len() as u64,
            );
            if got.0 >= want.0 && got.1 >= want.1 && got.2 >= want.2 {
                return;
            }
        }
        assert!(
            Instant::now() <= deadline,
            "the stores did not show {want:?} records of {id:?} within 90 s"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

struct RealFixture {
    store: Arc<LgtmStore>,
    classes: Arc<MemoryFieldClasses>,
    secrets: Arc<MemorySecretSource>,
    ingest: Arc<WaitingIngest>,
    tenants: std::sync::Mutex<BTreeSet<TenantId>>,
}

impl RealFixture {
    fn new(store: Arc<LgtmStore>) -> Self {
        let classes = Arc::new(MemoryFieldClasses::new());
        let secrets = Arc::new(MemorySecretSource::new());
        let inner = Arc::new(PipelineIngest::new(
            store.clone(),
            classes.clone(),
            secrets.clone(),
            tenant_keys(),
            Arc::new(MemoryUsageSink::default()),
        ));
        RealFixture {
            ingest: Arc::new(WaitingIngest {
                inner,
                store: store.clone(),
                seen: Default::default(),
            }),
            store,
            classes,
            secrets,
            tenants: Default::default(),
        }
    }
}

#[async_trait]
impl PrivacyFixture for RealFixture {
    fn ingest(&self) -> Arc<dyn TelemetryIngest> {
        self.ingest.clone()
    }

    fn store(&self) -> Arc<dyn TelemetryStore> {
        self.store.clone()
    }

    async fn set_class(
        &self,
        tenant: TenantId,
        app: NodeId,
        kind: FieldKind,
        name: &str,
        entry: FieldClassEntry,
    ) {
        self.tenants.lock().unwrap().insert(tenant);
        self.classes.set(tenant, app, kind, name, entry);
    }

    async fn set_secrets(&self, tenant: TenantId, app: NodeId, values: Vec<SecretValue>) {
        self.tenants.lock().unwrap().insert(tenant);
        self.secrets.set(tenant, app, values);
    }

    async fn dump(&self) -> std::result::Result<Vec<u8>, String> {
        let seen: Vec<TenantId> = {
            let mut t = self.tenants.lock().unwrap().clone();
            t.extend(self.ingest.seen.lock().unwrap().keys().map(|i| i.tenant));
            t.into_iter().collect()
        };
        let mut out = Vec::new();
        for t in seen {
            out.extend(self.store.dump_tenant(t).await.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }
}

#[tokio::test]
#[ignore = "needs the telemetry stores: scripts/dev.sh up --apps (build server: RUUM_TELEMETRY_REQUIRE_STORES=1)"]
async fn privacy_suite_passes_on_the_real_stores() {
    let Some(store) = real_store() else { return };
    let f = RealFixture::new(store);
    let report = conformance::run(&f).await;
    report.assert_ok();
    assert!(report.cases.len() >= 25, "{} cases", report.cases.len());
}

#[tokio::test]
#[ignore = "needs the telemetry stores: scripts/dev.sh up --apps (build server: RUUM_TELEMETRY_REQUIRE_STORES=1)"]
async fn the_real_stores_keep_tenants_apart() {
    let Some(store) = real_store() else { return };
    let f = RealFixture::new(store.clone());
    let a = identity(Environment::Preview);
    // B shares A's app and deployment IDs: only the tenant header separates them.
    let b = SourceIdentity {
        tenant: TenantId::new(),
        environment: Environment::Live,
        ..a
    };
    // A sends B's claims (and `live`) in resource and record attributes, for every signal.
    let lim = Limits::default();
    let batch = |claims: &SourceIdentity, text: &str| Batch {
        logs: otlp::decode_logs(
            &logs_request(forged(claims), vec![log_record(text, forged(claims))]),
            &lim,
        )
        .unwrap()
        .batch
        .logs,
        metrics: otlp::decode_metrics(
            &metrics_request(forged(claims), vec![point(forged(claims))]),
            &lim,
        )
        .unwrap()
        .batch
        .metrics,
        spans: otlp::decode_traces(
            &traces_request(forged(claims), vec![span(forged(claims), 1, 1)]),
            &lim,
        )
        .unwrap()
        .batch
        .spans,
    };
    let ra = f.ingest.ingest(&a, batch(&b, "from a")).await.unwrap();
    let rb = f.ingest.ingest(&b, batch(&b, "from b")).await.unwrap();
    assert_eq!(ra.stored_logs + ra.stored_metrics + ra.stored_spans, 3);
    assert_eq!(rb.stored_logs + rb.stored_metrics + rb.stored_spans, 3);
    // Positive control: each tenant reads its own three records.
    let all = TelemetryFilter::app(a.app);
    let got_a = store.read(a.tenant, &all).await.unwrap();
    let got_b = store.read(b.tenant, &all).await.unwrap();
    assert_eq!(
        (got_a.logs.len(), got_a.metrics.len(), got_a.spans.len()),
        (1, 1, 1)
    );
    assert_eq!(
        (got_b.logs.len(), got_b.metrics.len(), got_b.spans.len()),
        (1, 1, 1)
    );
    assert_eq!(got_a.logs[0].text.as_deref(), Some("from a"));
    assert_eq!(got_b.logs[0].text.as_deref(), Some("from b"));
    assert_own_identity(&got_a.logs[0].attributes, &a);
    assert_own_identity(&got_a.metrics[0].labels, &a);
    assert_own_identity(&got_a.spans[0].attributes, &a);
    assert_eq!(got_a.spans[0].source.environment, Environment::Preview);
    // Hostile filter values: only A's records or nothing, never B's.
    for key in [
        "\"} or {service_name=~\".+\"}",
        "|",
        "\n",
        "\\",
        "ruum.tenant",
    ] {
        for value in [
            b.tenant.to_string(),
            "\"} or {tenant=\"x\"}".into(),
            "\\\n|".into(),
        ] {
            let hostile = TelemetryFilter {
                fields: vec![FieldEquals {
                    key: key.into(),
                    value: value.clone(),
                }],
                ..TelemetryFilter::app(a.app)
            };
            let r = store.read(a.tenant, &hostile).await;
            if let Ok(r) = r {
                assert!(r.logs.iter().all(|l| l.source.tenant == a.tenant));
                assert!(r.metrics.iter().all(|l| l.source.tenant == a.tenant));
                assert!(r.spans.iter().all(|l| l.source.tenant == a.tenant));
                assert!(r.logs.iter().all(|l| l.text.as_deref() != Some("from b")));
            }
        }
    }
    // The secret scan of what both tenants hold.
    let dump = f.dump().await.unwrap();
    assert!(!dump.is_empty());
    assert!(conformance::scan_for_secrets(&dump, &conformance::fixture_secrets()).is_empty());
}

#[tokio::test]
#[ignore = "needs the telemetry stores: scripts/dev.sh up --apps (build server: RUUM_TELEMETRY_REQUIRE_STORES=1)"]
async fn the_shipper_path_keeps_secrets_out_of_the_real_stores() {
    let Some(store) = real_store() else { return };
    let f = RealFixture::new(store.clone());
    let registry = MemoryRegistry::new();
    let driver = Arc::new(MemoryDriver::new(
        registry.clone(),
        Arc::new(MemoryUsage::new()),
    ));
    let a = identity(Environment::Live);
    let secrets = conformance::fixture_secrets();
    f.set_secrets(
        a.tenant,
        a.app,
        secrets.iter().cloned().map(SecretValue::new).collect(),
    )
    .await;
    // A `console.log(process.env)`-style line: the driver redacts the value it knows; the
    // pipeline's matcher catches the encoded forms of all three.
    let lines: Vec<String> = vec![
        format!(
            "{{ APP_SECRET: '{}', PREV: '{}', HOME: '/home/app' }}",
            secrets[0],
            secrets[1].replace('"', "\\\"")
        ),
        format!("token={}", hex::encode(secrets[2].as_bytes())),
    ];
    let ia = start_job(&driver, &registry, &workload(&a), &secrets[0], &lines).await;
    wait_lines(&driver, a.tenant, ia, 3).await;
    let list = vec![(a.tenant, ia)];
    let shipper = LogShipper::new(
        driver,
        Arc::new(move || list.clone()),
        f.ingest.clone(),
        Arc::new(MemoryPositions::new()),
        ShipperConfig::default(),
    )
    .await
    .unwrap();
    let r = shipper.tick().await;
    assert_eq!(r.stored, 3, "positive control: the lines are stored");
    let got = store
        .read(a.tenant, &TelemetryFilter::app(a.app))
        .await
        .unwrap();
    assert_eq!(got.logs.len(), 3);
    let dump = f.dump().await.unwrap();
    let found = conformance::scan_for_secrets(&dump, &secrets);
    assert!(found.is_empty(), "{found:?}");
}
