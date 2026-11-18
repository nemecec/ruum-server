//! Tests of the telemetry border: the privacy suite on the in-memory store, and unit tests of
//! each stage (deny-list, class precedence, secret encodings, free-text patterns, the key
//! derivation, the caps).

use super::conformance::{self, PrivacyFixture};
use super::memory::{MemoryFieldClasses, MemorySecretSource, MemoryTelemetryStore};
use super::secrets::SecretMatcher;
use super::*;
use crate::privacy::keys::{LocalKekWrapper, MemoryTenantKeyStore};
use crate::privacy::{Pseudonymizer, SqlValue};
use crate::usage::MemoryUsageSink;

struct Fixture {
    store: Arc<MemoryTelemetryStore>,
    classes: Arc<MemoryFieldClasses>,
    secrets: Arc<MemorySecretSource>,
    usage: Arc<MemoryUsageSink>,
    ingest: Arc<PipelineIngest>,
}

fn tenant_keys() -> TenantKeys {
    TenantKeys::new(
        Arc::new(MemoryTenantKeyStore::new()),
        Arc::new(LocalKekWrapper::from_bytes(&[7u8; 32])),
    )
}

impl Fixture {
    fn new() -> Self {
        Self::with_limits(Limits::default())
    }

    fn with_limits(limits: Limits) -> Self {
        let store = Arc::new(MemoryTelemetryStore::new());
        let classes = Arc::new(MemoryFieldClasses::new());
        let secrets = Arc::new(MemorySecretSource::new());
        let usage = Arc::new(MemoryUsageSink::default());
        let ingest = Arc::new(
            PipelineIngest::new(
                store.clone(),
                classes.clone(),
                secrets.clone(),
                tenant_keys(),
                usage.clone(),
            )
            .with_limits(limits),
        );
        Fixture {
            store,
            classes,
            secrets,
            usage,
            ingest,
        }
    }
}

#[async_trait]
impl PrivacyFixture for Fixture {
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
        self.classes.set(tenant, app, kind, name, entry);
    }

    async fn set_secrets(&self, tenant: TenantId, app: NodeId, values: Vec<SecretValue>) {
        self.secrets.set(tenant, app, values);
    }

    async fn dump(&self) -> std::result::Result<Vec<u8>, String> {
        Ok(self.store.dump())
    }
}

// ---------------------------------------------------------------------------- the suite

#[tokio::test]
async fn privacy_suite_passes_in_memory() {
    let f = Fixture::new();
    let report = conformance::run(&f).await;
    report.assert_ok();
    assert!(report.cases.len() >= 25, "{} cases", report.cases.len());
}

#[test]
fn the_scan_finds_every_form() {
    let secrets = conformance::fixture_secrets();
    for s in &secrets {
        for (name, form) in conformance::secret_forms(s) {
            let hay = serde_json::to_vec(&serde_json::json!({ "v": format!("x{form}y") })).unwrap();
            assert_eq!(
                conformance::scan_for_secrets(&hay, std::slice::from_ref(s))
                    .len()
                    .min(1),
                1,
                "{name}"
            );
        }
    }
    let clean = serde_json::to_vec(&serde_json::json!({"v": "nothing here"})).unwrap();
    assert!(conformance::scan_for_secrets(&clean, &secrets).is_empty());
}

// ---------------------------------------------------------------------------- deny-list

#[test]
fn deny_list_normalisation_table() {
    let denied = [
        "password",
        "Password",
        "userPassword",
        "db.password",
        "X-Api-Key",
        "x_api_key",
        "apiKey",
        "api_key",
        "stripe_token",
        "access_token",
        "refreshToken",
        "client_secret",
        "private_key",
        "privateKey",
        "authorization",
        "Authorization",
        "authHeader",
        "cookie",
        "set-cookie",
        "credential",
        "credentials",
        "session",
        "sessionId",
        "passwd",
        "pwd",
        "jwt",
        "db.dsn",
        "AWSSecretAccessKey",
    ];
    for name in denied {
        assert!(denylist::is_denied(name), "{name} must be denied");
    }
    let allowed = [
        "order_id",
        "author",
        "route",
        "http.method",
        "user_id",
        "amount",
        "keyboard",
        "passenger",
        "status",
    ];
    for name in allowed {
        assert!(!denylist::is_denied(name), "{name} must not be denied");
    }
}

#[test]
fn deny_list_segments() {
    assert_eq!(denylist::segments("userPassword"), ["user", "password"]);
    assert_eq!(denylist::segments("XMLHttpToken"), ["xml", "http", "token"]);
    assert_eq!(denylist::segments("X-Api-Key"), ["x", "api", "key"]);
    assert_eq!(
        denylist::segments("db.password_v2"),
        ["db", "password", "v2"]
    );
}

// ---------------------------------------------------------------------------- classes

#[test]
fn class_precedence_table() {
    use FieldClass::*;
    let k = FieldKind::LogField;
    let cases: Vec<(Option<FieldClassEntry>, FieldClass)> = vec![
        (None, Personal),
        (Some(FieldClassEntry::confirmed(Normal)), Normal),
        (Some(FieldClassEntry::confirmed(Personal)), Personal),
        (Some(FieldClassEntry::confirmed(Secret)), Secret),
        (Some(FieldClassEntry::unconfirmed(Normal)), Personal),
        (Some(FieldClassEntry::unconfirmed(Secret)), Secret),
        (
            Some(FieldClassEntry::confirmed(Normal).with_suggestion(Secret)),
            Secret,
        ),
        (
            Some(FieldClassEntry::confirmed(Normal).with_suggestion(Personal)),
            Personal,
        ),
        (
            Some(FieldClassEntry::confirmed(Personal).with_suggestion(Secret)),
            Secret,
        ),
        (
            Some(FieldClassEntry::confirmed(Personal).with_suggestion(Normal)),
            Personal,
        ),
        (
            Some(FieldClassEntry::confirmed(Secret).with_suggestion(Normal)),
            Secret,
        ),
        (
            Some(FieldClassEntry::unconfirmed(Normal).with_suggestion(Normal)),
            Personal,
        ),
    ];
    for (i, (entry, want)) in cases.into_iter().enumerate() {
        let mut c = FieldClasses::new();
        if let Some(e) = entry {
            c.set(k, "field", e);
        }
        assert_eq!(classify::effective_class("field", k, &c), want, "case {i}");
    }
    // The deny-list wins over a confirmed normal class.
    let mut c = FieldClasses::new();
    c.set(k, "password", FieldClassEntry::confirmed(Normal));
    assert_eq!(classify::effective_class("password", k, &c), Secret);
    // The kind is part of the key.
    let mut c = FieldClasses::new();
    c.set(
        FieldKind::SpanAttribute,
        "route",
        FieldClassEntry::confirmed(Normal),
    );
    assert_eq!(classify::effective_class("route", k, &c), Personal);
    assert_eq!(
        classify::effective_class("route", FieldKind::SpanAttribute, &c),
        Normal
    );
    // A free-text attribute is at least personal, also confirmed normal (review K3); a
    // stricter class or suggestion still wins.
    for kind in [
        FieldKind::LogField,
        FieldKind::SpanAttribute,
        FieldKind::MetricLabel,
    ] {
        for name in [
            "exception.message",
            "exception.stacktrace",
            "Exception.Message",
        ] {
            let mut c = FieldClasses::new();
            c.set(kind, name, FieldClassEntry::confirmed(Normal));
            assert_eq!(
                classify::effective_class(name, kind, &c),
                Personal,
                "{name}"
            );
            c.set(
                kind,
                name,
                FieldClassEntry::confirmed(Normal).with_suggestion(Secret),
            );
            assert_eq!(classify::effective_class(name, kind, &c), Secret, "{name}");
        }
    }
}

#[test]
fn names_and_reserved_keys() {
    assert!(classify::is_field_name("order_id"));
    assert!(classify::is_field_name("http.route"));
    assert!(!classify::is_field_name("x-api-key"));
    assert!(!classify::is_field_name("1abc"));
    assert!(!classify::is_field_name(&"a".repeat(201)));
    assert!(classify::is_attribute_key("http.request.header.x-api-key"));
    assert!(!classify::is_attribute_key("alice@example.org"));
    assert!(!classify::is_attribute_key(""));
    assert!(classify::is_metric_name("http.server.request.duration"));
    assert!(classify::is_metric_name("jvm/memory-used_bytes"));
    assert!(!classify::is_metric_name("_x"));
    assert!(!classify::is_metric_name("orders_alice@example.org_total"));
    assert!(!classify::is_metric_name("bad\"}\nname"));
    assert!(!classify::is_metric_name(&"a".repeat(256)));
    assert!(classify::is_metric_name(&"a".repeat(255)));
    assert!(classify::name_has_pattern("card_4111111111111111"));
    assert!(classify::name_has_pattern("orders_4111111111111111_total"));
    assert!(!classify::name_has_pattern("http.request.header.x-api-key"));
    assert!(!classify::name_has_pattern("http_requests_total"));
    assert!(!classify::name_has_pattern("user_37"));
    assert!(classify::is_reserved("ruum.tenant"));
    assert!(classify::is_reserved("RUUM.Tenant"));
    assert!(!classify::is_reserved("ruumx.tenant"));
}

#[tokio::test]
async fn the_class_cache_reads_once_per_ttl() {
    struct Counting(std::sync::atomic::AtomicUsize);
    #[async_trait]
    impl FieldClassSource for Counting {
        async fn classes(&self, _: TenantId, _: NodeId) -> Result<Arc<FieldClasses>> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Arc::new(FieldClasses::new()))
        }
    }
    let inner = Arc::new(Counting(Default::default()));
    let cached =
        classify::CachedFieldClasses::new(inner.clone(), std::time::Duration::from_secs(60));
    let (t, a) = (TenantId::new(), NodeId::new());
    cached.classes(t, a).await.unwrap();
    cached.classes(t, a).await.unwrap();
    assert_eq!(inner.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    cached.classes(t, NodeId::new()).await.unwrap();
    assert_eq!(inner.0.load(std::sync::atomic::Ordering::SeqCst), 2);
    let none = classify::CachedFieldClasses::new(inner.clone(), std::time::Duration::ZERO);
    none.classes(t, a).await.unwrap();
    none.classes(t, a).await.unwrap();
    assert_eq!(inner.0.load(std::sync::atomic::Ordering::SeqCst), 4);
}

/// Kimi review of run 12, finding 4: a slow read of the old classes that ends after a newer
/// read must not replace the newer classes in the cache.
#[tokio::test]
async fn a_slow_old_class_read_does_not_replace_a_newer_one() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Racy {
        calls: AtomicUsize,
        started: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }
    #[async_trait]
    impl FieldClassSource for Racy {
        async fn classes(&self, _: TenantId, _: NodeId) -> Result<Arc<FieldClasses>> {
            let mut c = FieldClasses::new();
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                // The first read sees the old class and is slow.
                self.started.notify_one();
                self.release.notified().await;
                c.set(
                    FieldKind::LogField,
                    "email",
                    FieldClassEntry::confirmed(FieldClass::Normal),
                );
            } else {
                c.set(
                    FieldKind::LogField,
                    "email",
                    FieldClassEntry::confirmed(FieldClass::Personal),
                );
            }
            Ok(Arc::new(c))
        }
    }
    let inner = Arc::new(Racy {
        calls: AtomicUsize::new(0),
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let cached = Arc::new(classify::CachedFieldClasses::new(
        inner.clone(),
        std::time::Duration::from_secs(600),
    ));
    let (t, a) = (TenantId::new(), NodeId::new());
    let class = |c: &FieldClasses| c.get(FieldKind::LogField, "email").map(|e| e.class);
    let slow = tokio::spawn({
        let cached = cached.clone();
        async move { cached.classes(t, a).await.unwrap() }
    });
    inner.started.notified().await;
    // A newer read starts after the slow one and ends first.
    let newer = cached.classes(t, a).await.unwrap();
    assert_eq!(class(&newer), Some(FieldClass::Personal));
    inner.release.notify_one();
    let late = slow.await.unwrap();
    // The slow caller gets the newer classes too.
    assert_eq!(class(&late), Some(FieldClass::Personal));
    let after = cached.classes(t, a).await.unwrap();
    assert_eq!(
        class(&after),
        Some(FieldClass::Personal),
        "the slow read of the old class replaced the newer one"
    );
    assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
}

// ---------------------------------------------------------------------------- secrets

#[test]
fn the_matcher_finds_each_encoding() {
    use base64::Engine as _;
    use base64::engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD};
    let v = ["Fx7pQ2/wire", "+9Lm3 Zt8v-k5Rb\"q\\"].concat();
    let m = SecretMatcher::new([v.as_str()]);
    let mut samples = vec![v.clone(), hex::encode(&v), hex::encode_upper(&v)];
    for prefix in ["", "a", "ab", "user:"] {
        let data = format!("{prefix}{v}");
        samples.push(STANDARD.encode(&data));
        samples.push(URL_SAFE.encode(&data));
        samples.push(URL_SAFE_NO_PAD.encode(&data));
    }
    samples.push(serde_json::to_string(&v).unwrap());
    samples.push(serde_json::to_string(&serde_json::to_string(&v).unwrap()).unwrap());
    samples.push("q=Fx7pQ2%2Fwire%2B9Lm3%20Zt8v-k5Rb%22q%5C".into());
    samples.push("q=Fx7pQ2%2fwire%2b9Lm3%20Zt8v-k5Rb%22q%5c".into());
    samples.push("q=Fx7pQ2%2Fwire%2B9Lm3+Zt8v-k5Rb%22q%5C".into());
    // Partial percent-encodings (JS `encodeURI`, Python `quote`), double encoding, percent-
    // encoded base64 and JSON `\u` escapes: found through the decoded forms.
    samples.push("https://x/cb?k=Fx7pQ2/wire+9Lm3%20Zt8v-k5Rb%22q%5C".into());
    samples.push("https://x/cb?k=Fx7pQ2/wire%2B9Lm3%20Zt8v-k5Rb%22q%5C".into());
    samples.push("q=Fx7pQ2%252Fwire%252B9Lm3%2520Zt8v-k5Rb%2522q%255C".into());
    let b64 = STANDARD.encode(&v);
    samples.push(format!("t={}", b64.replace('+', "%2B").replace('/', "%2F")));
    samples.push(r#"{"k":"Fx7pQ2\u002fwire\u002B9Lm3 Zt8v-k5Rb\u0022q\u005c"}"#.into());
    for (i, s) in samples.iter().enumerate() {
        let text = format!("before {s} after");
        assert!(m.contains(&text), "sample {i}");
        let red = m.redact(&text).unwrap();
        assert!(!m.contains(&red), "sample {i} after redaction");
        assert!(red.contains(crate::runtime::redact::REDACTED), "sample {i}");
    }
    assert!(m.redact("nothing secret").is_none());
    // A decoded match that no single word holds: the whole text becomes the marker.
    let split = "k=Fx7pQ2/wire%2B9Lm3 Zt8v-k5Rb%22q%5C";
    assert!(m.contains(split));
    assert_eq!(
        m.redact(split).as_deref(),
        Some(crate::runtime::redact::REDACTED)
    );
    // Only the word that holds a decoded match is replaced.
    let red = m
        .redact("keep this https://x/?k=Fx7pQ2/wire+9Lm3%20Zt8v-k5Rb%22q%5C and that")
        .unwrap();
    assert_eq!(
        red,
        format!("keep this {} and that", crate::runtime::redact::REDACTED)
    );
    // Decoding does not make ordinary text match.
    assert!(!m.contains("100% sure+certain \\u0041 %zz %4"));
    // Numbers: a secret of digits matches the decimal text.
    let digits = SecretMatcher::new(["98765432"]);
    assert!(digits.contains(&98765432i64.to_string()));
    assert!(!digits.contains(&9876543i64.to_string()));
    // Mixed-case hex (review Kimi K1): found and redacted.
    let mixed: String = hex::encode(&v)
        .chars()
        .enumerate()
        .map(|(i, c)| {
            if i % 3 == 0 {
                c.to_ascii_uppercase()
            } else {
                c
            }
        })
        .collect();
    assert!(mixed.chars().any(|c| c.is_ascii_uppercase()));
    assert!(mixed.chars().any(|c| ('a'..='f').contains(&c)));
    let text = format!("k={mixed} end");
    assert!(m.contains(&text));
    let red = m.redact(&text).unwrap();
    assert!(!m.contains(&red) && !red.contains(&mixed), "{red}");
    assert!(!m.contains("DEADbeef CAFE 0123456789ABCdef"));
    // Short secrets (review Kimi K2): a whole value, word or run of letters and digits, raw or
    // in an encoding of at least 4 characters; not inside a longer run.
    let short = SecretMatcher::new(["q7Z"]);
    assert!(!short.is_empty());
    for hit in [
        "q7Z",
        " q7Z ",
        "pin q7Z ok",
        "pin=q7Z;",
        "\"q7Z\"",
        "cTda",
        "cTda ok",
        "71375a",
        "x 71375A",
    ] {
        assert!(short.contains(hit), "{hit:?}");
        let red = short.redact(hit).unwrap();
        assert!(!short.contains(&red), "{hit:?} -> {red:?}");
    }
    for miss in ["xq7Zx", "q7Zq7Z", "Q7Z", "nothing", "cTdax"] {
        assert!(!short.contains(miss), "{miss:?}");
    }
    // A 4-byte secret: its base64 cores are short, the padded form is a needle (`azlXZA==`),
    // and the cores match as whole words.
    let four = SecretMatcher::new(["k9Wd"]);
    for hit in ["t=azlXZA==", "Basic azlXZA== x", "azlXZA", "azlXZ"] {
        assert!(four.contains(hit), "{hit:?}");
    }
    assert!(!four.contains("xazlXZx"));
    // An empty secret matches nothing.
    assert!(SecretMatcher::new([""]).is_empty());
    assert!(format!("{m:?}").starts_with("SecretMatcher("));
    assert!(!format!("{m:?}").contains("wire"));
}

/// Kimi review of run 12, finding 1: when a secret is part of `<hidden secret>`, the marker
/// of a fallback redaction (a match that only a decoded form shows) must not show it.
#[test]
fn the_marker_never_shows_a_secret() {
    let cases: &[(&str, &str)] = &[
        ("secret", "sec%72et"),
        ("secret", "sec%72et and more"),
        ("secret", "token=sec%72et"),
        ("hidden", "hid%64en"),
        ("<hidden secret>", "%3Chidden%20secret%3E"),
        ("<hidden secret>", "x <hidden secret> y"),
        ("hidden secret", "hidden%20secret"),
        ("secret", "a secret b"),
    ];
    for (secret, text) in cases {
        let m = SecretMatcher::new([*secret]);
        let out = m
            .redact(text)
            .unwrap_or_else(|| panic!("{text:?} holds {secret:?}"));
        assert!(
            !m.contains(&out),
            "secret {secret:?}, text {text:?}: the redaction {out:?} still holds it"
        );
        assert!(!out.contains(secret), "{out:?} shows {secret:?}");
    }
    // A secret that is not part of the marker keeps the usual marker.
    let m = SecretMatcher::new(["Fx7pQ2wire"]);
    assert_eq!(m.marker(), crate::runtime::redact::REDACTED);
    assert_eq!(SecretMatcher::new(["secret"]).marker(), "<hidden>");
    assert_eq!(SecretMatcher::new(["hidden"]).marker(), "***");
    assert_eq!(SecretMatcher::new(["hidden", "***"]).marker(), "");
}

#[test]
fn base64_cores_hold_only_the_value() {
    use base64::engine::general_purpose::STANDARD_NO_PAD;
    let v = b"0123456789";
    for shift in 0..3 {
        let a = secrets::base64_core(&STANDARD_NO_PAD, v, shift);
        assert!(!a.is_empty());
        for prefix in [[0u8, 0], [255, 255], [65, 66]] {
            use base64::Engine as _;
            let mut buf = prefix[..shift.min(2)].to_vec();
            buf.extend_from_slice(&[0x5a; 3][..shift.saturating_sub(2)]);
            buf.extend_from_slice(v);
            buf.extend_from_slice(b"tail");
            assert!(STANDARD_NO_PAD.encode(&buf).contains(&a), "shift {shift}");
        }
    }
}

// ---------------------------------------------------------------------------- scrubber

fn pz() -> HmacPseudonymizer {
    HmacPseudonymizer::new(PseudonymKey::from_bytes([3u8; 32]))
}

#[test]
fn scrub_pattern_table() {
    use scrub::Pattern::*;
    let jwt = [
        "eyJ",
        "hbGciOiJIUzI1NiJ9",
        ".",
        "eyJzdWIiOiIxIn0",
        ".",
        "c2lnLW5vdC1yZWFs",
    ]
    .concat();
    let positive: Vec<(String, scrub::Pattern)> = vec![
        ("mail bob.smith@example.com now".into(), Email),
        ("x a_b-c%d+e@sub.example-mail.org.".into(), Email),
        ("call +372 5123 4567".into(), Phone),
        ("call +14155552671".into(), Phone),
        ("call (020) 7946 0958".into(), Phone),
        ("call 0612345678".into(), Phone),
        ("call 555-123-4567".into(), Phone),
        ("call 4155552671 now".into(), Phone),
        ("call 415.555.2671.".into(), Phone),
        ("call 14155552671".into(), Phone),
        ("call 14155552671 now".into(), Phone),
        ("call 1.415.555.2671 now".into(), Phone),
        ("tel:2125550123".into(), Phone),
        ("iban DE89 3704 0044 0532 0130 00".into(), Iban),
        ("iban GB82WEST12345698765432 ok".into(), Iban),
        ("card 4111 1111 1111 1111".into(), Card),
        ("card 4111-1111-1111-1111".into(), Card),
        ("card 378282246310005".into(), Card),
        (format!("t {jwt}"), Jwt),
        ("Authorization: Bearer abcDEF123456ghiJKL".into(), Bearer),
        ("auth bearer   x.y-z_9~a+b/c=".into(), Bearer),
        (["k ", "sk_", "live_", "abcdefgh1234"].concat(), TokenPrefix),
        (
            ["k ", "ghp_", "abcdefghijklmnopqrstuvwxyz0123456789"].concat(),
            TokenPrefix,
        ),
        (["k ", "AKIA", "ABCDEFGHIJKLMNOP"].concat(), TokenPrefix),
        (
            ["k ", "ruum_", "appkey_", "abcdefghijklmnopqrstuv"].concat(),
            TokenPrefix,
        ),
        (["k ", "xox", "b-1234567890-abc"].concat(), TokenPrefix),
        (
            ["h ", "0123456789abcdef", "0123456789abcdef"].concat(),
            LongHex,
        ),
        (
            ["h ke", "y-0123456789abcdef", "0123456789ABCDEF"].concat(),
            LongHex,
        ),
        (
            "b QmFzZTY0IGZpeHR1cmUgb25seSwgbm90IHJlYWwgOkQ1==".into(),
            LongBase64,
        ),
    ];
    for (text, want) in &positive {
        let found = scrub::find(text);
        assert!(
            found.iter().any(|m| m.pattern == *want),
            "{text:?}: expected {want:?}, found {found:?}"
        );
    }
    let negative = [
        "order 12345 shipped",
        "card 4111 1111 1111 1112",
        "user@localhost",
        "a@b",
        "date 2026-11-15",
        "version 1.2.3",
        "pi 3.14159265",
        "snake_case_identifier_that_is_quite_long_indeed",
        "iban DE89 3704 0044 0532 0130 01x",
        "count 123456789",
        "at 1700000000 seconds",
        "id 4155552671x",
        "ratio 4155552671.5",
        "v 1.4155552671",
        "v 415.555.2671.9",
        "n 4151552671",
        "n 14151552671",
        "n 24155552671",
        "n 11155552671",
        "id 14155552671x",
        "ratio 14155552671.5",
        "v 2.1.415.555.2671",
        "ip 192.168.100.200",
        "id 0198c2e4-6a2b-7c3d-8e4f-123456789abc",
        "short deadbeef",
        "AKIASHORT",
        "bearer abc",
        "sk_live_",
    ];
    for text in negative {
        let found = scrub::find(text);
        assert!(found.is_empty(), "{text:?}: found {found:?}");
    }
}

#[test]
fn scrub_replaces_with_markers() {
    let p = pz();
    let (out, counts) = scrub::scrub("mail bob@example.com card 4111 1111 1111 1111.", &p);
    let pseudo = p.pseudonym(&SqlValue::Text("bob@example.com"));
    let hex = pseudo.id().trim_start_matches("p_");
    assert_eq!(
        out,
        format!(
            "mail <person {hex}> card {}.",
            crate::runtime::redact::REDACTED
        )
    );
    assert_eq!(counts.hidden, 1);
    assert_eq!(counts.pseudonymized, 1);
    let long = "x".repeat(scrub::MAX_TEXT_BYTES + 1);
    assert_eq!(scrub::scrub(&long, &p).0, crate::runtime::redact::REDACTED);
    // Multi-byte text around a match keeps its characters.
    let (out, _) = scrub::scrub("päev ☕ bob@example.com ✓", &p);
    assert!(out.starts_with("päev ☕ <person ") && out.ends_with("> ✓"));
}

#[test]
fn luhn_and_mod97() {
    assert!(scrub::luhn_valid(b"4111111111111111"));
    assert!(!scrub::luhn_valid(b"4111111111111112"));
    assert!(scrub::iban_valid(b"DE89370400440532013000"));
    assert!(!scrub::iban_valid(b"DE89370400440532013001"));
    assert!(!scrub::iban_valid(b"DE89"));
}

#[test]
fn scrub_is_linear_on_adversarial_input() {
    let p = pz();
    let units = [
        "a@",
        "a.",
        "+1 ",
        "4111 ",
        "eyJ.",
        "DE00 ",
        "bearer ",
        "(0",
        "AKIA",
        "sk_live_",
        "0-",
        "ab@cd.",
        "@a.bc ",
        "1 ",
        "aB3",
        "-",
        "415.555.",
        "2125550123.",
    ];
    for u in units {
        let small = u.repeat(1024 / u.len());
        let big = u.repeat(32 * 1024 / u.len());
        let t0 = std::time::Instant::now();
        for _ in 0..8 {
            scrub::scrub(&small, &p);
        }
        let t_small = t0.elapsed() / 8;
        let t1 = std::time::Instant::now();
        scrub::scrub(&big, &p);
        let t_big = t1.elapsed();
        // 32 times the input: a linear scanner takes about 32 times as long; allow noise.
        assert!(
            t_big < t_small * 200 + std::time::Duration::from_millis(50),
            "{u:?}: {t_small:?} for 1 KiB, {t_big:?} for 32 KiB"
        );
    }
}

// ---------------------------------------------------------------------------- keys

#[test]
fn hmac_matches_rfc_4231_and_privacy() {
    // RFC 4231 test case 2.
    let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
    assert_eq!(
        hex::encode(mac),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
    let tenant_key = PseudonymKey::from_bytes([9u8; 32]);
    let node = NodeId::new();
    let msg = format!("db:{}", node.as_uuid().hyphenated());
    assert_eq!(
        &hmac_sha256(tenant_key.as_bytes(), msg.as_bytes()),
        tenant_key.derive_for_database(node).as_bytes()
    );
}

#[test]
fn the_app_key_is_domain_separated() {
    let tenant_key = PseudonymKey::from_bytes([9u8; 32]);
    let (a, b) = (NodeId::new(), NodeId::new());
    let ka = app_telemetry_key(&tenant_key, a);
    let kb = app_telemetry_key(&tenant_key, b);
    assert_ne!(ka.as_bytes(), kb.as_bytes(), "pairwise per app");
    assert_ne!(ka.as_bytes(), tenant_key.as_bytes(), "not the tenant key");
    assert_ne!(
        ka.as_bytes(),
        tenant_key.derive_for_database(a).as_bytes(),
        "not the database key of the same node"
    );
    let msg = format!("ruum/telemetry/v1/{}", a.as_uuid().hyphenated());
    assert_eq!(
        ka.as_bytes(),
        &hmac_sha256(tenant_key.as_bytes(), msg.as_bytes())
    );
    // Another tenant key gives another app key.
    let other = PseudonymKey::from_bytes([8u8; 32]);
    assert_ne!(app_telemetry_key(&other, a).as_bytes(), ka.as_bytes());
}

// ---------------------------------------------------------------------------- pipeline

fn identity() -> SourceIdentity {
    SourceIdentity {
        tenant: TenantId::new(),
        app: NodeId::new(),
        deployment: uuid::Uuid::now_v7(),
        environment: Environment::Live,
    }
}

fn text_log(text: &str) -> LogRecord {
    LogRecord {
        time: crate::now(),
        severity: Severity::Info,
        body: LogBody::Text(text.into()),
        attributes: vec![],
    }
}

#[test]
fn caps_drop_whole_records_with_a_reason() {
    let id = identity();
    let limits = Limits {
        max_records: 3,
        max_batch_bytes: 10_000,
        max_fields: 2,
        max_value_bytes: 100,
    };
    let fields = |n: usize| -> Vec<Attribute> {
        (0..n)
            .map(|i| Attribute::new(format!("f{i}"), "v"))
            .collect()
    };
    let batch = Batch {
        logs: vec![
            text_log("fine"),
            text_log(&"x".repeat(101)),
            LogRecord {
                body: LogBody::Template {
                    template: "t".into(),
                    fields: fields(3),
                },
                ..text_log("")
            },
            text_log("over the record cap"),
        ],
        ..Default::default()
    };
    let (out, report) = classify::process(
        &id,
        batch,
        &FieldClasses::new(),
        &pz(),
        &SecretMatcher::default(),
        &limits,
    );
    assert_eq!(out.logs.len(), 1);
    assert_eq!(report.stored_logs, 1);
    assert_eq!(report.dropped(DropReason::ValueTooLong), 1);
    assert_eq!(report.dropped(DropReason::TooManyFields), 1);
    assert_eq!(report.dropped(DropReason::TooManyRecords), 1);

    // Each text log counts the record overhead and its 10 bytes: one fits, two do not.
    let small = Limits {
        max_batch_bytes: classify::RECORD_OVERHEAD + 20,
        ..limits
    };
    let batch = Batch {
        logs: vec![text_log("0123456789"), text_log("0123456789")],
        ..Default::default()
    };
    let (out, report) = classify::process(
        &id,
        batch,
        &FieldClasses::new(),
        &pz(),
        &SecretMatcher::default(),
        &small,
    );
    assert_eq!(out.logs.len(), 1);
    assert_eq!(report.dropped(DropReason::BatchTooLarge), 1);
}

/// The worst case of review K4: records full of empty, unclassified fields. Each becomes a
/// pseudonym attribute, so without a per-value cost a small batch makes a large output. The
/// output must stay within twice the batch byte cap, with the default caps and a small cap.
#[test]
fn empty_fields_do_not_amplify_the_output() {
    let id = identity();
    let run = |limits: Limits, records: usize| {
        let empty =
            |n: usize| -> Vec<Attribute> { (0..n).map(|_| Attribute::new("a", "")).collect() };
        let t = crate::now();
        let batch = Batch {
            logs: (0..records)
                .map(|_| LogRecord {
                    time: t,
                    severity: Severity::Info,
                    body: LogBody::Template {
                        template: "t".into(),
                        fields: empty(limits.max_fields),
                    },
                    attributes: vec![],
                })
                .collect(),
            spans: (0..records)
                .map(|i| Span {
                    trace_id: format!("{:032x}", i + 1),
                    span_id: format!("{:016x}", i + 1),
                    parent_span_id: None,
                    name: "n".into(),
                    kind: SpanKind::Internal,
                    start: t,
                    end: t,
                    status: StatusCode::Unset,
                    status_message: None,
                    attributes: empty(limits.max_fields),
                    events: vec![],
                    links: vec![],
                })
                .collect(),
            ..Default::default()
        };
        let (out, report) = classify::process(
            &id,
            batch,
            &FieldClasses::new(),
            &pz(),
            &SecretMatcher::default(),
            &limits,
        );
        // Not silent: records were stored, and the cap dropped the rest.
        assert!(report.stored_logs > 0, "no log was stored");
        assert!(
            report.dropped(DropReason::BatchTooLarge) > 0,
            "the batch cap dropped nothing"
        );
        let values: usize = out
            .logs
            .iter()
            .map(|r| r.fields.len() + r.attributes.len())
            .chain(out.spans.iter().map(|r| r.attributes.len()))
            .sum();
        assert!(
            values * classify::VALUE_OVERHEAD <= limits.max_batch_bytes + 4 * 1024,
            "{values} stored values for a cap of {} bytes",
            limits.max_batch_bytes
        );
        let bytes = out.byte_size();
        assert!(
            bytes <= 2 * limits.max_batch_bytes,
            "{bytes} output bytes for a cap of {} bytes",
            limits.max_batch_bytes
        );
    };
    run(Limits::default(), 600);
    run(
        Limits {
            max_batch_bytes: 64 * 1024,
            ..Limits::default()
        },
        300,
    );
}

#[test]
fn invalid_span_ids_drop_the_span() {
    let id = identity();
    let t = crate::now();
    let span = |trace: &str, span: &str| Span {
        trace_id: trace.into(),
        span_id: span.into(),
        parent_span_id: None,
        name: "n".into(),
        kind: SpanKind::Internal,
        start: t,
        end: t,
        status: StatusCode::Unset,
        status_message: None,
        attributes: vec![],
        events: vec![],
        links: vec![],
    };
    let good_trace = "0af7651916cd43dd8448eb211c80319c";
    let batch = Batch {
        spans: vec![
            span(good_trace, "b7ad6b7169203331"),
            span(&"0".repeat(32), "b7ad6b7169203331"),
            span(good_trace, "B7AD6B7169203331"),
            span("alice@example.org", "b7ad6b7169203331"),
        ],
        ..Default::default()
    };
    let (out, report) = classify::process(
        &id,
        batch,
        &FieldClasses::new(),
        &pz(),
        &SecretMatcher::default(),
        &Limits::default(),
    );
    assert_eq!(out.spans.len(), 1);
    assert_eq!(report.dropped(DropReason::InvalidId), 3);
}

#[test]
fn pseudonyms_use_canonical_text() {
    let id = identity();
    let p = pz();
    let batch = Batch {
        logs: vec![LogRecord {
            body: LogBody::Template {
                template: "t".into(),
                fields: vec![
                    Attribute::new("n", 42i64),
                    Attribute::new("f", 1.5f64),
                    Attribute::new("b", true),
                    Attribute::new("s", "x"),
                ],
            },
            ..text_log("")
        }],
        ..Default::default()
    };
    let (out, _) = classify::process(
        &id,
        batch,
        &FieldClasses::new(),
        &p,
        &SecretMatcher::default(),
        &Limits::default(),
    );
    let fields = &out.logs[0].fields;
    let want = [
        ("n", p.pseudonym(&SqlValue::Int(42))),
        ("f", p.pseudonym(&SqlValue::Float8(1.5))),
        ("b", p.pseudonym(&SqlValue::Bool(true))),
        ("s", p.pseudonym(&SqlValue::Text("x"))),
    ];
    for (k, w) in want {
        assert_eq!(
            attr(fields, k).and_then(StoredValue::as_pseudonym),
            Some(&w),
            "{k}"
        );
    }
}

#[test]
fn stored_values_serialize_as_contract_markers() {
    let v = serde_json::to_value(StoredValue::Hidden(HiddenValue::SECRET)).unwrap();
    assert_eq!(v, serde_json::json!({"$ruum": "hidden", "class": "secret"}));
    let p = pz().pseudonym(&SqlValue::Text("x"));
    let v = serde_json::to_value(StoredValue::Pseudonym(p.clone())).unwrap();
    assert_eq!(v, serde_json::json!({"$ruum": "pseudonym", "id": p.id()}));
    assert_eq!(
        serde_json::to_value(StoredValue::Text("a".into())).unwrap(),
        serde_json::json!("a")
    );
}

#[tokio::test]
async fn telemetry_bytes_are_metered_to_the_app() {
    let f = Fixture::new();
    let id = identity();
    let report = f
        .ingest
        .ingest(
            &id,
            Batch {
                logs: vec![text_log("hello")],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(report.bytes > 0);
    assert_eq!(
        f.usage
            .total(id.tenant, id.app, Charged::App, Meter::TelemetryBytes),
        report.bytes as i64
    );
    // An empty result meters nothing.
    let id2 = identity();
    let report = f.ingest.ingest(&id2, Batch::default()).await.unwrap();
    assert_eq!(report.bytes, 0);
    assert_eq!(
        f.usage
            .total(id2.tenant, id2.app, Charged::App, Meter::TelemetryBytes),
        0
    );
}

/// Metering comes before the store write: when the usage sink fails, the ingest fails and
/// nothing is stored (review Kimi run 12, finding 4).
#[tokio::test]
async fn a_metering_failure_stores_nothing() {
    struct FailingSink;
    #[async_trait]
    impl crate::usage::UsageSink for FailingSink {
        async fn record(&self, _: crate::usage::UsageEvent) -> Result<()> {
            Err(crate::error::Error::Internal("metering is down".into()))
        }
    }
    let store = Arc::new(MemoryTelemetryStore::new());
    let ingest = PipelineIngest::new(
        store.clone(),
        Arc::new(MemoryFieldClasses::new()),
        Arc::new(MemorySecretSource::new()),
        tenant_keys(),
        Arc::new(FailingSink),
    );
    let id = identity();
    let result = ingest
        .ingest(
            &id,
            Batch {
                logs: vec![text_log("hello")],
                ..Default::default()
            },
        )
        .await;
    assert!(result.is_err());
    let stored = store
        .read(id.tenant, &TelemetryFilter::app(id.app))
        .await
        .unwrap();
    assert!(stored.is_empty(), "telemetry was stored without usage");
    assert!(store.tenants().is_empty(), "the store has a storage tenant");
}

#[tokio::test]
async fn reads_filter_by_time_severity_and_fields() {
    let f = Fixture::new();
    let id = identity();
    f.classes.set(
        id.tenant,
        id.app,
        FieldKind::LogField,
        "order_id",
        FieldClassEntry::confirmed(FieldClass::Normal),
    );
    let t0 = crate::now();
    let rec = |sev: Severity, order: &str, dt: i64| LogRecord {
        time: t0 + time::Duration::seconds(dt),
        severity: sev,
        body: LogBody::Template {
            template: "o {order_id}".into(),
            fields: vec![Attribute::new("order_id", order)],
        },
        attributes: vec![],
    };
    f.ingest
        .ingest(
            &id,
            Batch {
                logs: vec![
                    rec(Severity::Info, "a", 0),
                    rec(Severity::Error, "b", 10),
                    rec(Severity::Warn, "c", 20),
                ],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let read = |filter: TelemetryFilter| {
        let store = f.store.clone();
        let tenant = id.tenant;
        async move { store.read(tenant, &filter).await.unwrap().logs.len() }
    };
    assert_eq!(read(TelemetryFilter::app(id.app)).await, 3);
    assert_eq!(
        read(TelemetryFilter {
            min_severity: Some(Severity::Warn),
            ..Default::default()
        })
        .await,
        2
    );
    assert_eq!(
        read(TelemetryFilter {
            from: Some(t0 + time::Duration::seconds(5)),
            to: Some(t0 + time::Duration::seconds(15)),
            ..Default::default()
        })
        .await,
        1
    );
    assert_eq!(
        read(TelemetryFilter {
            fields: vec![FieldEquals {
                key: "order_id".into(),
                value: "c".into()
            }],
            ..Default::default()
        })
        .await,
        1
    );
    assert_eq!(
        read(TelemetryFilter {
            environment: Some(Environment::Preview),
            ..Default::default()
        })
        .await,
        0
    );
    assert_eq!(
        read(TelemetryFilter {
            limit: Some(2),
            ..Default::default()
        })
        .await,
        2
    );
    assert_eq!(
        read(TelemetryFilter {
            signal: Some(Signal::Metrics),
            ..Default::default()
        })
        .await,
        0
    );
}

/// Kimi review of run 12, finding 5: reads return the records oldest first by record time,
/// and the limit keeps the oldest, whatever the order of writing.
#[tokio::test]
async fn reads_are_oldest_first_by_record_time() {
    let f = Fixture::new();
    let id = identity();
    let t0 = crate::now();
    let at = |dt: i64| t0 + time::Duration::seconds(dt);
    let log = |name: &str, dt: i64| LogRecord {
        time: at(dt),
        ..text_log(name)
    };
    let metric = |v: f64, dt: i64| MetricPoint {
        time: at(dt),
        name: "m".into(),
        kind: MetricKind::Gauge,
        value: v,
        labels: vec![],
    };
    let span = |name: &str, dt: i64, sid: &str| Span {
        trace_id: "0af7651916cd43dd8448eb211c80319c".into(),
        span_id: sid.into(),
        parent_span_id: None,
        name: name.into(),
        kind: SpanKind::Internal,
        start: at(dt),
        end: at(dt),
        status: StatusCode::Unset,
        status_message: None,
        attributes: vec![],
        events: vec![],
        links: vec![],
    };
    // Two batches, each out of order, the second older than the first.
    f.ingest
        .ingest(
            &id,
            Batch {
                logs: vec![log("third", 20), log("fourth", 30)],
                metrics: vec![metric(3.0, 20), metric(4.0, 30)],
                spans: vec![
                    span("third", 20, "b7ad6b7169203333"),
                    span("fourth", 30, "b7ad6b7169203334"),
                ],
            },
        )
        .await
        .unwrap();
    f.ingest
        .ingest(
            &id,
            Batch {
                logs: vec![log("second", 10), log("first", 0)],
                metrics: vec![metric(2.0, 10), metric(1.0, 0)],
                spans: vec![
                    span("second", 10, "b7ad6b7169203332"),
                    span("first", 0, "b7ad6b7169203331"),
                ],
            },
        )
        .await
        .unwrap();
    for limit in [None, Some(2)] {
        let filter = TelemetryFilter {
            limit,
            ..TelemetryFilter::app(id.app)
        };
        let read = f.store.read(id.tenant, &filter).await.unwrap();
        let want: &[&str] = match limit {
            None => &["first", "second", "third", "fourth"],
            Some(_) => &["first", "second"],
        };
        let logs: Vec<&str> = read.logs.iter().filter_map(|r| r.text.as_deref()).collect();
        assert_eq!(logs, want, "logs, limit {limit:?}");
        let spans: Vec<&str> = read.spans.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(spans, want, "spans, limit {limit:?}");
        let metrics: Vec<f64> = read.metrics.iter().map(|r| r.value).collect();
        let want_m: &[f64] = match limit {
            None => &[1.0, 2.0, 3.0, 4.0],
            Some(_) => &[1.0, 2.0],
        };
        assert_eq!(metrics, want_m, "metrics, limit {limit:?}");
    }
}
