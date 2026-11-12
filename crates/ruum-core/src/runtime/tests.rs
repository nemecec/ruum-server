//! Tests of the runtime border: the conformance suites on the in-memory driver and builder,
//! proof that the suite notices a broken driver, and the unit tests of the parts.

use super::conformance::{self, BuildTiming, BuilderFixture, DriverFixture, Outcome, Timing};
use super::memory::{
    BuilderFaults, ENV_DUMP_ARCHIVE_MARKER, FAILING_ARCHIVE_MARKER, Faults, SLOW_ARCHIVE_MARKER,
};
use super::program::{self, Step};
use super::redact::{LineRedactor, MAX_LINE_BYTES, REDACTED, redact};
use super::*;
use bytes::Bytes;
use std::sync::Arc;

// ------------------------------------------------------------------------------ fixture

struct Memory {
    registry: Arc<MemoryRegistry>,
    usage: Arc<MemoryUsage>,
    driver: Arc<MemoryDriver>,
    builder: Arc<MemoryBuilder>,
    canaries: Vec<String>,
    skip: Option<&'static str>,
}

impl Memory {
    fn new() -> Self {
        Self::with_faults(Faults::default(), BuilderFaults::default())
    }

    fn with_faults(faults: Faults, builder_faults: BuilderFaults) -> Self {
        let registry = MemoryRegistry::new();
        let usage = Arc::new(MemoryUsage::new());
        let canaries = vec![
            format!("canary-registry-{}", Uuid::now_v7().simple()),
            format!("canary-host-{}", Uuid::now_v7().simple()),
        ];
        Self {
            driver: Arc::new(MemoryDriver::with_faults(
                registry.clone(),
                usage.clone(),
                faults,
            )),
            builder: Arc::new(
                MemoryBuilder::new(registry.clone(), usage.clone())
                    .with_own_secrets(canaries.iter().map(SecretValue::new).collect())
                    .with_faults(builder_faults),
            ),
            registry,
            usage,
            canaries,
            skip: None,
        }
    }
}

#[async_trait]
impl DriverFixture for Memory {
    fn driver(&self) -> Arc<dyn RuntimeDriver> {
        self.driver.clone()
    }

    fn usage(&self) -> Arc<MemoryUsage> {
        self.usage.clone()
    }

    async fn program_image(&self, identity: &WorkloadIdentity) -> Result<ImageRef, String> {
        // The in-memory driver runs the conformance program in any image of the app; the image
        // comes from the in-memory builder, so build and start meet here.
        let out = self
            .builder
            .build(BuildInput {
                identity: *identity,
                runtime: "node22".to_owned(),
                archive: Bytes::from_static(b"the conformance program"),
                limits: Limits {
                    cpu_millis: 1000,
                    memory_bytes: 1 << 30,
                    disk_bytes: 1 << 30,
                },
                timeout: Duration::from_secs(60),
            })
            .await
            .map_err(|e| e.to_string())?;
        Ok(out.image)
    }

    fn timing(&self) -> Timing {
        Timing {
            idle_timeout: Duration::from_millis(400),
            health_interval: Duration::from_millis(20),
            job_timeout: Duration::from_millis(100),
            wait: Duration::from_secs(10),
        }
    }

    fn skip(&self, case: &str) -> Option<String> {
        (self.skip == Some(case)).then(|| "the fixture skips it".to_owned())
    }
}

impl BuilderFixture for Memory {
    fn builder(&self) -> Arc<dyn Builder> {
        self.builder.clone()
    }

    fn usage(&self) -> Arc<MemoryUsage> {
        self.usage.clone()
    }

    fn runtime(&self) -> String {
        "node22".to_owned()
    }

    fn archive(&self) -> Bytes {
        Bytes::from(format!("an app archive {}", Uuid::now_v7()))
    }

    fn failing_archive(&self) -> Bytes {
        let mut a = b"an app archive whose build fails: ".to_vec();
        a.extend_from_slice(FAILING_ARCHIVE_MARKER);
        Bytes::from(a)
    }

    fn slow_archive(&self) -> Bytes {
        let mut a = b"an app archive whose build is slow: ".to_vec();
        a.extend_from_slice(SLOW_ARCHIVE_MARKER);
        Bytes::from(a)
    }

    fn build_timing(&self) -> BuildTiming {
        BuildTiming {
            timeout: Duration::from_millis(50),
            wait: Duration::from_secs(3),
        }
    }

    fn env_dump_archive(&self) -> Bytes {
        let mut a = b"an app archive that reports its environment: ".to_vec();
        a.extend_from_slice(ENV_DUMP_ARCHIVE_MARKER);
        Bytes::from(a)
    }

    fn canaries(&self) -> Vec<String> {
        self.canaries.clone()
    }
}

// ------------------------------------------------------------------------------ the suites

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_driver_passes_every_case() {
    let f = Memory::new();
    let report = conformance::run(&f).await;
    println!("{}", report.summary());
    report.assert_ok();
    // The in-memory driver passes every case, without a skip, whether CI is set or not.
    assert!(report.problems(true).is_empty(), "{}", report.summary());
    assert_eq!(report.cases.len(), conformance::DRIVER_CASES.len());
}

#[tokio::test]
async fn memory_builder_passes_every_case() {
    let f = Memory::new();
    let report = conformance::run_builder(&f).await;
    println!("{}", report.summary());
    report.assert_ok();
    assert!(report.problems(true).is_empty(), "{}", report.summary());
    assert_eq!(report.cases.len(), conformance::BUILDER_CASES.len());
}

#[tokio::test]
async fn a_skip_fails_when_ci_is_set() {
    let mut f = Memory::new();
    f.skip = Some("health_failing_marks_unhealthy");
    let report = conformance::run_cases(&f, &["health_failing_marks_unhealthy", "stop"]).await;
    let skipped = report
        .cases
        .iter()
        .find(|c| c.name == "health_failing_marks_unhealthy")
        .expect("the case ran");
    assert_eq!(
        skipped.outcome,
        Outcome::Skipped("the fixture skips it".to_owned())
    );
    assert!(report.problems(false).is_empty(), "{}", report.summary());
    let problems = report.problems(true);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("skipped while CI is set"),
        "{problems:?}"
    );
    assert_eq!(
        conformance::skips_fail(),
        std::env::var_os("CI").is_some(),
        "skips fail exactly when CI is set"
    );
}

/// Runs the named cases against a driver with a fault and returns the failed ones.
async fn failed_with(faults: Faults, cases: &[&str]) -> Vec<&'static str> {
    let f = Memory::with_faults(faults, BuilderFaults::default());
    let report = conformance::run_cases(&f, cases).await;
    report
        .cases
        .iter()
        .filter(|c| matches!(c.outcome, Outcome::Failed(_)))
        .map(|c| c.name)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_suite_notices_a_driver_without_redaction() {
    let cases = [
        "secrets_redacted",
        "secret_split_across_writes_redacted",
        "rotated_token_redacted",
    ];
    let failed = failed_with(
        Faults {
            no_redaction: true,
            ..Faults::default()
        },
        &cases,
    )
    .await;
    assert_eq!(failed, cases);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_suite_notices_a_shared_network() {
    let cases = [
        "apps_never_share_a_network",
        "preview_and_live_are_separate",
    ];
    let failed = failed_with(
        Faults {
            shared_network: true,
            ..Faults::default()
        },
        &cases,
    )
    .await;
    assert_eq!(failed, cases);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_suite_notices_an_added_route() {
    let failed = failed_with(
        Faults {
            extra_route: true,
            ..Faults::default()
        },
        &["network_routes_exact"],
    )
    .await;
    assert_eq!(failed, ["network_routes_exact"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_suite_notices_a_driver_that_never_scales_to_zero() {
    let failed = failed_with(
        Faults {
            no_idle: true,
            ..Faults::default()
        },
        &["scale_to_zero_and_wake"],
    )
    .await;
    assert_eq!(failed, ["scale_to_zero_and_wake"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_suite_notices_missing_limits() {
    let failed = failed_with(
        Faults {
            no_job_timeout: true,
            no_memory_limit: true,
            ..Faults::default()
        },
        &["job_times_out", "memory_limit_enforced"],
    )
    .await;
    assert_eq!(failed, ["job_times_out", "memory_limit_enforced"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_suite_notices_tenant_blind_calls() {
    let failed = failed_with(
        Faults {
            tenant_blind_calls: true,
            ..Faults::default()
        },
        &["other_tenant_cannot_act"],
    )
    .await;
    assert_eq!(failed, ["other_tenant_cannot_act"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_suite_notices_a_global_registry() {
    let failed = failed_with(
        Faults {
            global_registry: true,
            ..Faults::default()
        },
        &["image_of_another_app_refused"],
    )
    .await;
    assert_eq!(failed, ["image_of_another_app_refused"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_suite_notices_a_cold_start_that_adds_a_route() {
    let failed = failed_with(
        Faults {
            wake_adds_route: true,
            ..Faults::default()
        },
        &["scale_to_zero_and_wake"],
    )
    .await;
    assert_eq!(failed, ["scale_to_zero_and_wake"]);
}

/// Runs the named builder cases against a builder with a fault and returns the failed ones.
async fn builder_failed_with(faults: BuilderFaults, cases: &[&str]) -> Vec<&'static str> {
    let f = Memory::with_faults(Faults::default(), faults);
    let report = conformance::run_builder_cases(&f, cases).await;
    report
        .cases
        .iter()
        .filter(|c| matches!(c.outcome, Outcome::Failed(_)))
        .map(|c| c.name)
        .collect()
}

#[tokio::test]
async fn the_suite_notices_a_builder_that_leaks_its_secrets() {
    let failed = builder_failed_with(
        BuilderFaults {
            leak_host_secrets: true,
            ..BuilderFaults::default()
        },
        &["build_sees_no_platform_secrets"],
    )
    .await;
    assert_eq!(failed, ["build_sees_no_platform_secrets"]);
}

#[tokio::test]
async fn the_suite_notices_a_builder_without_a_timeout() {
    let failed = builder_failed_with(
        BuilderFaults {
            no_timeout: true,
            ..BuilderFaults::default()
        },
        &["build_times_out"],
    )
    .await;
    assert_eq!(failed, ["build_times_out"]);
}

// ------------------------------------------------------------------------------ the trait

#[test]
fn a_build_input_holds_no_secret_and_no_binding() {
    // Rule 7: a build gets no secrets and no bindings. This destructures the input field by
    // field: a new field breaks the test, so its author must check the rule here.
    let input = BuildInput {
        identity: WorkloadIdentity {
            tenant: TenantId::new(),
            app: NodeId::new(),
            deployment: Uuid::now_v7(),
            environment: Environment::Preview,
        },
        runtime: "node22".to_owned(),
        archive: Bytes::from_static(b"x"),
        limits: Limits {
            cpu_millis: 1,
            memory_bytes: 1,
            disk_bytes: 1,
        },
        timeout: Duration::from_secs(1),
    };
    let BuildInput {
        identity: _,
        runtime: _,
        archive: _,
        limits: _,
        timeout: _,
    } = input;
    assert_eq!(BUILD_ROUTES, &[Route::EgressProxy]);
}

#[test]
fn secret_values_never_show_in_debug() {
    let value = "s3cr3t-value-123";
    let spec_env = EnvVar::secret("K", SecretValue::new(value));
    let text = format!("{spec_env:?} {:?}", SecretValue::new(value));
    assert!(!text.contains(value), "{text}");
    assert!(text.contains(REDACTED), "{text}");
}

#[test]
fn redacted_values_list_secrets_previous_values_and_the_token() {
    let spec = WorkloadSpec {
        identity: WorkloadIdentity {
            tenant: TenantId::new(),
            app: NodeId::new(),
            deployment: Uuid::now_v7(),
            environment: Environment::Live,
        },
        kind: WorkloadKind::Job(JobSpec {
            timeout: Duration::from_secs(1),
        }),
        image: ImageRef::Oci(OciDigest::of(b"x")),
        command: Vec::new(),
        env: vec![
            EnvVar::secret("A", SecretValue::new("aaa")),
            EnvVar::plain("B", "plain"),
            EnvVar::secret("C", SecretValue::new("")),
        ],
        previous_secrets: vec![SecretValue::new("old"), SecretValue::new("aaa")],
        workload_token: SecretValue::new("tok"),
        limits: Limits {
            cpu_millis: 1,
            memory_bytes: 1,
            disk_bytes: 1,
        },
        routes: Vec::new(),
    };
    assert_eq!(spec.redacted_values(), vec!["aaa", "old", "tok"]);
}

#[test]
fn errors_have_snake_case_codes_and_details() {
    let errors = [
        RuntimeError::NotFound {
            instance: InstanceId::new(),
        },
        RuntimeError::invalid("env", "bad"),
        RuntimeError::UnsupportedKind {
            kind: "session".into(),
        },
        RuntimeError::UnsupportedImage { reason: "r".into() },
        RuntimeError::ImageNotFound { image: "i".into() },
        RuntimeError::state("wake", &InstanceState::Stopped),
        RuntimeError::UnsupportedRuntime {
            runtime: "x".into(),
        },
        RuntimeError::BuildFailed { reason: "r".into() },
        RuntimeError::BuildTimedOut,
        RuntimeError::Unavailable { reason: "r".into() },
        RuntimeError::Internal { reason: "r".into() },
    ];
    let mut codes = std::collections::HashSet::new();
    for e in &errors {
        let code = e.code();
        assert!(
            code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
            "{code}"
        );
        assert!(codes.insert(code), "{code} twice");
        assert!(e.details().is_object(), "{code}");
    }
    assert_eq!(
        RuntimeError::state("wake", &InstanceState::Stopped).details(),
        serde_json::json!({ "operation": "wake", "state": "stopped" })
    );
}

#[test]
fn oci_digests_are_checked() {
    let d = OciDigest::of(b"hello");
    assert_eq!(OciDigest::parse(d.as_str()).unwrap(), d);
    assert!(d.as_str().starts_with("sha256:"));
    for bad in [
        "",
        "sha256:",
        "sha512:00",
        &format!("sha256:{}", "A".repeat(64)),
        &format!("sha256:{}", "0".repeat(63)),
    ] {
        assert!(OciDigest::parse(bad).is_err(), "{bad}");
    }
    assert!(serde_json::from_str::<OciDigest>("\"sha256:zz\"").is_err());
}

#[test]
fn static_images_are_checked() {
    let file = |p: &str| StaticFile {
        path: p.to_owned(),
        content: crate::blobs::Hash::of(p.as_bytes()),
        size: p.len() as u64,
    };
    let img = StaticImage::new(vec![file("b.html"), file("a/index.html")]);
    assert_eq!(img.files[0].path, "a/index.html");
    img.verify().unwrap();
    // The tree hash depends on the files.
    let other = StaticImage::new(vec![file("b.html")]);
    assert_ne!(img.tree, other.tree);
    for bad in ["/abs", "a/../b", "a//b", "./a", ""] {
        let img = StaticImage::new(vec![file(bad)]);
        assert!(img.verify().is_err(), "{bad:?}");
    }
    let twice = StaticImage::new(vec![file("a"), file("a")]);
    assert!(twice.verify().is_err());
}

#[test]
fn program_steps_read_back() {
    let steps = vec![
        Step::Print("a: b".into()),
        Step::Write("x".into()),
        Step::Stderr("e".into()),
        Step::StderrWrite("e2".into()),
        Step::Env("NAME".into()),
        Step::Token,
        Step::TokenSha,
        Step::Sleep(Duration::from_millis(15)),
        Step::Exit(-1),
        Step::Serve,
        Step::Unhealthy,
        Step::Alloc(10),
        Step::Disk(20),
    ];
    let cmd = program::command(&steps);
    let back: Vec<Step> = cmd.iter().map(|a| Step::parse(a).unwrap()).collect();
    assert_eq!(back, steps);
    for bad in [
        "",
        "nope",
        "sleep:x",
        "exit",
        "env:",
        "serve:1",
        "token-sha:1",
    ] {
        assert!(Step::parse(bad).is_none(), "{bad}");
    }
    let sha = program::token_sha("a-token");
    assert!(sha.starts_with("token-sha:") && sha.len() == "token-sha:".len() + 16);
    assert_ne!(sha, program::token_sha("another-token"));
}

#[tokio::test]
async fn the_memory_driver_refuses_an_unknown_step() {
    let f = Memory::new();
    let identity = WorkloadIdentity {
        tenant: TenantId::new(),
        app: NodeId::new(),
        deployment: Uuid::now_v7(),
        environment: Environment::Live,
    };
    let image = f.program_image(&identity).await.unwrap();
    let spec = WorkloadSpec {
        identity,
        kind: WorkloadKind::Job(JobSpec {
            timeout: Duration::from_secs(1),
        }),
        image,
        command: vec!["rm -rf /".to_owned()],
        env: Vec::new(),
        previous_secrets: Vec::new(),
        workload_token: SecretValue::new("t"),
        limits: Limits {
            cpu_millis: 1,
            memory_bytes: 1,
            disk_bytes: 1,
        },
        routes: Vec::new(),
    };
    let err = f.driver.start(spec).await.unwrap_err();
    assert_eq!(err.code(), "invalid_spec");
    let digest = OciDigest::of(b"the conformance program");
    assert!(f.registry.contains(&identity, &digest));
    // The repository is the app's: another app of the tenant does not have the image.
    let other_app = WorkloadIdentity {
        app: NodeId::new(),
        ..identity
    };
    assert!(!f.registry.contains(&other_app, &digest));
}

// ------------------------------------------------------------------------------ redaction

fn at() -> time::OffsetDateTime {
    crate::now()
}

fn texts(lines: Vec<(time::OffsetDateTime, String)>) -> Vec<String> {
    lines.into_iter().map(|(_, t)| t).collect()
}

#[test]
fn redact_replaces_every_exact_match() {
    assert_eq!(
        redact("a SECRET b SECRET", &["SECRET"]),
        format!("a {REDACTED} b {REDACTED}")
    );
    // The longest value wins where two start at one place.
    assert_eq!(
        redact("xSECRETx", &["SEC", "SECRET"]),
        format!("x{REDACTED}x")
    );
    assert_eq!(redact("nothing here", &["", "absent"]), "nothing here");
    assert_eq!(redact("päss wörd", &["wörd"]), format!("päss {REDACTED}"));
}

#[test]
fn a_value_split_at_every_byte_is_redacted() {
    let secret = "value-to-hide-in-the-log";
    let text = format!("before {secret} after\nnext line\n");
    for split in 1..text.len() {
        let mut r = LineRedactor::new([secret]);
        let mut lines = texts(r.push(&text[..split], at()));
        lines.extend(texts(r.push(&text[split..], at())));
        lines.extend(texts(r.finish(at())));
        assert_eq!(
            lines,
            vec![format!("before {REDACTED} after"), "next line".to_owned()],
            "split at {split}"
        );
    }
}

#[test]
fn a_value_written_one_byte_at_a_time_is_redacted() {
    let secret = "s3cr3t";
    let text = format!("x{secret}y\n{secret}\n");
    let mut r = LineRedactor::new([secret]);
    let mut lines = Vec::new();
    for b in text.chars() {
        lines.extend(texts(r.push(&b.to_string(), at())));
    }
    lines.extend(texts(r.finish(at())));
    assert_eq!(lines, vec![format!("x{REDACTED}y"), REDACTED.to_owned()]);
}

#[test]
fn a_finished_line_is_not_held_back() {
    let mut r = LineRedactor::new(["SECRET-VALUE"]);
    assert_eq!(texts(r.push("boot\n", at())), vec!["boot"]);
    // An end that could start a value waits for the next piece.
    assert!(r.push("S", at()).is_empty());
    assert_eq!(
        texts(r.push("ECRET-VALUE!\n", at())),
        vec![format!("{REDACTED}!")]
    );
}

#[test]
fn a_value_with_a_line_end_is_redacted_as_one() {
    let key = "-----BEGIN KEY-----\nabc\n-----END KEY-----";
    let mut r = LineRedactor::new([key]);
    let mut lines = texts(r.push("key: -----BEGIN KEY-----\nab", at()));
    lines.extend(texts(r.push("c\n-----END KEY----- done\n", at())));
    lines.extend(texts(r.finish(at())));
    assert_eq!(lines, vec![format!("key: {REDACTED} done")]);
}

#[test]
fn an_added_value_is_redacted_from_then_on() {
    let mut r = LineRedactor::new(["old-token"]);
    r.add("new-token");
    let lines = texts(r.push("old-token new-token\n", at()));
    assert_eq!(lines, vec![format!("{REDACTED} {REDACTED}")]);
}

#[test]
fn a_long_line_is_let_out_in_redacted_pieces() {
    let secret = "LONGSECRET";
    let mut r = LineRedactor::new([secret]);
    let mut out = Vec::new();
    let chunk = format!("{}{secret}", "x".repeat(1000));
    for _ in 0..(MAX_LINE_BYTES / chunk.len() + 10) {
        out.extend(texts(r.push(&chunk, at())));
    }
    out.extend(texts(r.finish(at())));
    assert!(out.len() > 1, "a long line was never let out");
    assert!(out.iter().all(|l| !l.contains(secret)));
}

#[test]
fn an_added_value_split_at_every_byte_is_redacted() {
    let new = "renewed-token-0123456789";
    let text = format!("a {new} b\n");
    for split in 1..text.len() {
        let mut r = LineRedactor::new(["first-token"]);
        let mut lines = texts(r.push(&text[..split], at()));
        r.add(new);
        lines.extend(texts(r.push(&text[split..], at())));
        lines.extend(texts(r.finish(at())));
        assert_eq!(lines, vec![format!("a {REDACTED} b")], "split at {split}");
    }
}

#[test]
fn a_multibyte_value_split_at_every_byte_is_redacted() {
    let secret = "pässwörd-€-日本";
    let text = format!("x {secret} y\n");
    let bytes = text.as_bytes();
    for split in 1..bytes.len() {
        let mut r = LineRedactor::new([secret]);
        let mut lines = texts(r.push_bytes(&bytes[..split], at()));
        lines.extend(texts(r.push_bytes(&bytes[split..], at())));
        lines.extend(texts(r.finish(at())));
        assert_eq!(lines, vec![format!("x {REDACTED} y")], "split at {split}");
    }
    // Bytes that are not UTF-8 become U+FFFD and do not stop the output.
    let mut r = LineRedactor::new([secret]);
    let lines = texts(r.push_bytes(b"a\xffb\n", at()));
    assert_eq!(lines, vec!["a\u{FFFD}b".to_owned()]);
    // An unfinished character at the end comes out at finish.
    assert!(r.push_bytes(&"é".as_bytes()[..1], at()).is_empty());
    assert_eq!(texts(r.finish(at())), vec!["\u{FFFD}".to_owned()]);
}

#[test]
fn a_value_the_marker_rebuilds_hides_the_line() {
    // "secret>xx" becomes "<hidden secret>x", which holds "secret>x" again.
    let value = "secret>x";
    assert_eq!(redact("a secret>x b", &[value]), format!("a {REDACTED} b"));
    assert_eq!(redact("a secret>xx b", &[value]), REDACTED);
    let mut r = LineRedactor::new([value]);
    let mut lines = texts(r.push("ok\nsecret>xx\n", at()));
    lines.extend(texts(r.finish(at())));
    assert_eq!(lines, vec!["ok".to_owned(), REDACTED.to_owned()]);
}

#[test]
fn one_byte_writes_without_a_line_end_cost_linear_time() {
    // A hostile app writes 256 KiB one byte at a time, with no line end, while 50 values are
    // redacted. The redactor must stay linear (each push scanned the whole held-back line
    // again before review R-1 item 3, which was quadratic).
    let values: Vec<String> = (0..50)
        .map(|i| format!("v{i:02}-{}", Uuid::now_v7().simple()))
        .collect();
    let refs: Vec<&str> = values.iter().map(String::as_str).collect();
    let mut text = String::new();
    let mut i = 0;
    while text.len() < 256 * 1024 {
        text.push_str(&"x".repeat(997));
        text.push_str(&values[i % values.len()]);
        i += 1;
    }
    let mut r = LineRedactor::new(refs.iter().copied());
    let begun = std::time::Instant::now();
    let mut out = Vec::new();
    for b in text.as_bytes() {
        out.extend(texts(r.push(std::str::from_utf8(&[*b]).unwrap(), at())));
    }
    out.extend(texts(r.finish(at())));
    let took = begun.elapsed();
    assert!(
        took < Duration::from_secs(2),
        "256 KiB in one-byte writes took {took:?}"
    );
    assert!(out.len() > 1, "the long line was never let out");
    assert_eq!(out.concat(), redact(&text, &refs));
    assert!(
        values
            .iter()
            .all(|v| out.iter().all(|l| !l.contains(v.as_str())))
    );
}

#[test]
fn streaming_redaction_matches_whole_text_redaction() {
    // Random texts and values over a small alphabet (with line ends, so values can hold them),
    // cut into random pieces, with a value added by `add` now and then: the lines must be
    // those of redacting the whole text at once. The marker holds no `a` or `b`, so it never
    // rebuilds a value here.
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    let alphabet = ['a', 'b', 'a', 'b', '\n'];
    for round in 0..3000 {
        let mut word = |len: u64| -> String {
            (0..len)
                .map(|_| alphabet[next(alphabet.len() as u64) as usize])
                .collect()
        };
        let values: Vec<String> = (0..1 + round % 4)
            .map(|_| word(1 + (round as u64 * 7 + 3) % 6))
            .filter(|v| v != "\n")
            .collect();
        let added = word(2 + round as u64 % 5);
        let text = word(5 + round as u64 % 60);
        let add_at = if round % 3 == 0 { Some(0) } else { None };
        let mut refs: Vec<&str> = values.iter().map(String::as_str).collect();
        if add_at.is_some() {
            refs.push(added.as_str());
        }
        let whole = redact(&text, &refs);
        let whole = whole.strip_suffix('\n').unwrap_or(&whole);
        let want: Vec<String> = if text.is_empty() {
            Vec::new()
        } else {
            whole.split('\n').map(str::to_owned).collect()
        };
        let mut r = LineRedactor::new(values.iter().map(String::as_str));
        if add_at.is_some() {
            r.add(&added);
        }
        let mut lines = Vec::new();
        let mut at_byte = 0;
        while at_byte < text.len() {
            let n = 1 + next(5) as usize;
            let end = (at_byte + n).min(text.len());
            lines.extend(texts(r.push(&text[at_byte..end], at())));
            at_byte = end;
        }
        lines.extend(texts(r.finish(at())));
        assert_eq!(lines, want, "round {round}: text {text:?}, values {refs:?}");
    }
}
