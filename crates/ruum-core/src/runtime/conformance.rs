//! The conformance suites of the runtime: the same cases for every [`RuntimeDriver`]
//! ([`run`]) and every [`Builder`] ([`run_builder`]). The in-memory driver and builder pass
//! them here; card R-2a runs [`run`] unchanged against the Docker driver, plain and with runsc,
//! and R-2b runs [`run_builder`] against the Railpack builder.
//!
//! The suite sees a driver only through the trait and a fixture. The "code" of the workloads is
//! the conformance program ([`super::program`]): the fixture gives an image that interprets it.
//!
//! A case that a driver cannot run returns `skipped` with a reason (the fixture's
//! [`DriverFixture::skip`], or the case itself). **A skip fails when the `CI` environment
//! variable is set** ([`Report::assert_ok`]; backlog T-26), so a CI run never passes a case
//! silently.

use super::program::{self, DISK_FULL, Step};
use super::redact::REDACTED;
use super::*;
use crate::ids::{NodeId, TenantId};
use bytes::Bytes;
use futures::FutureExt;
use futures::future::BoxFuture;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Instant;

// ------------------------------------------------------------------------------ fixtures

/// The times the cases use. A real driver needs longer ones than the in-memory driver.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// The idle timeout of the scale-to-zero case.
    pub idle_timeout: Duration,
    /// The health check interval.
    pub health_interval: Duration,
    /// The timeout of the job that times out.
    pub job_timeout: Duration,
    /// How long a case waits for a state at most (on top of the times above).
    pub wait: Duration,
}

/// What the driver suite needs beyond the trait.
#[async_trait]
pub trait DriverFixture: Send + Sync {
    fn driver(&self) -> Arc<dyn RuntimeDriver>;
    /// The sink the driver reports usage to.
    fn usage(&self) -> Arc<MemoryUsage>;
    /// An OCI image whose entry point runs the conformance program given as arguments.
    async fn program_image(&self) -> Result<ImageRef, String>;
    fn timing(&self) -> Timing;
    /// A reason to skip the named case, when the driver cannot run it.
    fn skip(&self, _case: &str) -> Option<String> {
        None
    }
}

/// What the builder suite needs beyond the trait.
pub trait BuilderFixture: Send + Sync {
    fn builder(&self) -> Arc<dyn Builder>;
    /// The sink the builder reports usage to.
    fn usage(&self) -> Arc<MemoryUsage>;
    /// A runtime the builder supports (`node22`).
    fn runtime(&self) -> String;
    /// An archive that builds.
    fn archive(&self) -> Bytes;
    /// An archive whose build runs and fails.
    fn failing_archive(&self) -> Bytes;
    fn skip(&self, _case: &str) -> Option<String> {
        None
    }
}

// ------------------------------------------------------------------------------ report

/// The result of one case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    Skipped(String),
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct CaseReport {
    pub name: &'static str,
    pub outcome: Outcome,
}

/// The results of a suite run.
#[derive(Clone, Debug)]
pub struct Report {
    pub suite: &'static str,
    /// The name of the driver or builder.
    pub subject: String,
    pub cases: Vec<CaseReport>,
}

/// True when skips fail: the `CI` environment variable is set.
pub fn skips_fail() -> bool {
    std::env::var_os("CI").is_some()
}

impl Report {
    /// The problems of the run: every failed case, and every skipped one when `skips_fail`.
    pub fn problems(&self, skips_fail: bool) -> Vec<String> {
        self.cases
            .iter()
            .filter_map(|c| match &c.outcome {
                Outcome::Passed => None,
                Outcome::Failed(m) => Some(format!("{}: failed: {m}", c.name)),
                Outcome::Skipped(r) if skips_fail => {
                    Some(format!("{}: skipped while CI is set: {r}", c.name))
                }
                Outcome::Skipped(_) => None,
            })
            .collect()
    }

    /// A table of the cases, for the test output.
    pub fn summary(&self) -> String {
        let mut s = format!("{} [{}]\n", self.suite, self.subject);
        for c in &self.cases {
            let (mark, note) = match &c.outcome {
                Outcome::Passed => ("pass", String::new()),
                Outcome::Skipped(r) => ("skip", format!(" ({r})")),
                Outcome::Failed(m) => ("FAIL", format!(" ({m})")),
            };
            s.push_str(&format!("  {mark}  {}{note}\n", c.name));
        }
        s
    }

    /// Panics with the summary when a case failed, or was skipped while `CI` is set.
    pub fn assert_ok(&self) {
        let problems = self.problems(skips_fail());
        assert!(
            problems.is_empty(),
            "{}\n{}",
            self.summary(),
            problems.join("\n")
        );
    }
}

// ------------------------------------------------------------------------------ helpers

enum CaseError {
    Skip(String),
    Fail(String),
}

type CaseResult = Result<(), CaseError>;

fn fail(msg: impl Into<String>) -> CaseError {
    CaseError::Fail(msg.into())
}

macro_rules! check {
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            return Err(fail(format!($($arg)+)));
        }
    };
}

/// The context of one driver case: the fixture, the program image, and the instances the case
/// started (stopped after the case).
struct Ctx<'a> {
    f: &'a dyn DriverFixture,
    driver: Arc<dyn RuntimeDriver>,
    image: ImageRef,
    timing: Timing,
    started: std::sync::Mutex<Vec<(TenantId, InstanceId)>>,
}

impl Ctx<'_> {
    async fn start(&self, spec: WorkloadSpec) -> Result<InstanceStatus, CaseError> {
        let tenant = spec.identity.tenant;
        let s = self.driver.start(spec).await.map_err(|e| match e {
            // A driver that does not run a kind cannot run the case (a skip fails in CI).
            RuntimeError::UnsupportedKind { kind } => {
                CaseError::Skip(format!("the driver does not run {kind} workloads"))
            }
            e => fail(format!("start failed: {e} ({})", e.code())),
        })?;
        self.started.lock().expect("ctx lock").push((tenant, s.id));
        Ok(s)
    }

    async fn status(&self, s: &InstanceStatus) -> Result<InstanceStatus, CaseError> {
        self.driver
            .status(s.identity.tenant, s.id)
            .await
            .map_err(|e| fail(format!("status failed: {e}")))
    }

    /// Waits until the status meets `pred`, at most `extra` plus [`Timing::wait`].
    async fn wait_for(
        &self,
        s: &InstanceStatus,
        what: &str,
        extra: Duration,
        pred: impl Fn(&InstanceStatus) -> bool,
    ) -> Result<InstanceStatus, CaseError> {
        let deadline = Instant::now() + extra + self.timing.wait;
        loop {
            let now = self.status(s).await?;
            if pred(&now) {
                return Ok(now);
            }
            if Instant::now() > deadline {
                return Err(fail(format!(
                    "the instance never became {what}: state {}, health {:?}",
                    now.state.name(),
                    now.health
                )));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn wait_exit(
        &self,
        s: &InstanceStatus,
        extra: Duration,
    ) -> Result<ExitStatus, CaseError> {
        let st = self
            .wait_for(s, "exited", extra, |x| {
                matches!(x.state, InstanceState::Exited(_))
            })
            .await?;
        match st.state {
            InstanceState::Exited(e) => Ok(e),
            _ => unreachable!(),
        }
    }

    async fn logs(&self, s: &InstanceStatus) -> Result<Vec<LogLine>, CaseError> {
        let mut out: Vec<LogLine> = Vec::new();
        loop {
            let page = self
                .driver
                .logs(s.identity.tenant, s.id, out.last().map(|l| l.seq), 100)
                .await
                .map_err(|e| fail(format!("logs failed: {e}")))?;
            if page.is_empty() {
                return Ok(out);
            }
            out.extend(page);
        }
    }

    /// Waits until the log has a line equal to `text`.
    async fn wait_line(&self, s: &InstanceStatus, text: &str) -> Result<Vec<LogLine>, CaseError> {
        let deadline = Instant::now() + self.timing.wait;
        loop {
            let logs = self.logs(s).await?;
            if logs.iter().any(|l| l.text == text) {
                return Ok(logs);
            }
            if Instant::now() > deadline {
                return Err(fail(format!(
                    "the log never had the line {text:?}: {:?}",
                    texts(&logs)
                )));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn cleanup(&self) {
        let started = std::mem::take(&mut *self.started.lock().expect("ctx lock"));
        for (tenant, id) in started {
            let _ = self.driver.stop(tenant, id).await;
        }
    }
}

fn texts(logs: &[LogLine]) -> Vec<&str> {
    logs.iter().map(|l| l.text.as_str()).collect()
}

fn identity(tenant: TenantId, app: NodeId, environment: Environment) -> WorkloadIdentity {
    WorkloadIdentity {
        tenant,
        app,
        deployment: Uuid::now_v7(),
        environment,
    }
}

fn new_identity(environment: Environment) -> WorkloadIdentity {
    identity(TenantId::new(), NodeId::new(), environment)
}

fn limits() -> Limits {
    Limits {
        cpu_millis: 250,
        memory_bytes: 128 * 1024 * 1024,
        disk_bytes: 64 * 1024 * 1024,
    }
}

fn token() -> SecretValue {
    SecretValue::new(format!("wt_{}", Uuid::now_v7().simple()))
}

fn secret_text(label: &str) -> String {
    format!(
        "{label}{}{}",
        Uuid::now_v7().simple(),
        Uuid::now_v7().simple()
    )
}

impl Ctx<'_> {
    fn service(&self, identity: WorkloadIdentity, steps: &[Step]) -> WorkloadSpec {
        WorkloadSpec {
            identity,
            kind: WorkloadKind::Service(ServiceSpec {
                port: 8080,
                health: Some(HealthCheck {
                    path: "/health".to_owned(),
                    interval: self.timing.health_interval,
                    failure_threshold: 2,
                }),
                idle_timeout: None,
            }),
            image: self.image.clone(),
            command: program::command(steps),
            env: Vec::new(),
            previous_secrets: Vec::new(),
            workload_token: token(),
            limits: limits(),
            routes: vec![Route::InternalGateway, Route::EgressProxy],
        }
    }

    fn job(&self, identity: WorkloadIdentity, steps: &[Step], timeout: Duration) -> WorkloadSpec {
        WorkloadSpec {
            kind: WorkloadKind::Job(JobSpec { timeout }),
            ..self.service(identity, steps)
        }
    }

    /// A job with a timeout long enough for any short program.
    fn quick_job(&self, identity: WorkloadIdentity, steps: &[Step]) -> WorkloadSpec {
        self.job(identity, steps, self.timing.wait)
    }
}

fn expect_err<T>(r: Result<T, RuntimeError>, code: &str, what: &str) -> CaseResult {
    match r {
        Ok(_) => Err(fail(format!("{what}: expected {code}, the call succeeded"))),
        Err(e) if e.code() == code => Ok(()),
        Err(e) => Err(fail(format!(
            "{what}: expected {code}, got {} ({e})",
            e.code()
        ))),
    }
}

fn sorted(routes: &[Route]) -> Vec<Route> {
    let mut r = routes.to_vec();
    r.sort();
    r
}

// ------------------------------------------------------------------------------ driver cases

type DriverCase = for<'a, 'b> fn(&'a Ctx<'b>) -> BoxFuture<'a, CaseResult>;

macro_rules! cases {
    ($($name:ident),* $(,)?) => {
        &[$((stringify!($name), |c| Box::pin($name(c)))),*]
    };
}

/// The names of the driver cases, in run order.
pub const DRIVER_CASES: &[&str] = &[
    "start_service",
    "start_static",
    "invalid_spec_refused",
    "unknown_image_refused",
    "stop",
    "scale_to_zero_and_wake",
    "touch_keeps_service_awake",
    "job_completes",
    "job_times_out",
    "logs_in_order",
    "secrets_redacted",
    "secret_split_across_writes_redacted",
    "rotated_token_redacted",
    "health_failing_marks_unhealthy",
    "limits_recorded",
    "memory_limit_enforced",
    "disk_limit_enforced",
    "network_routes_exact",
    "apps_never_share_a_network",
    "preview_and_live_are_separate",
    "usage_reported",
];

const DRIVER_CASE_FNS: &[(&str, DriverCase)] = cases![
    start_service,
    start_static,
    invalid_spec_refused,
    unknown_image_refused,
    stop,
    scale_to_zero_and_wake,
    touch_keeps_service_awake,
    job_completes,
    job_times_out,
    logs_in_order,
    secrets_redacted,
    secret_split_across_writes_redacted,
    rotated_token_redacted,
    health_failing_marks_unhealthy,
    limits_recorded,
    memory_limit_enforced,
    disk_limit_enforced,
    network_routes_exact,
    apps_never_share_a_network,
    preview_and_live_are_separate,
    usage_reported,
];

/// Runs every driver case, one after another, and returns the report. Call
/// [`Report::assert_ok`] on it.
pub async fn run(f: &dyn DriverFixture) -> Report {
    run_cases(f, DRIVER_CASES).await
}

/// Runs the named driver cases only (for debugging a driver; CI runs [`run`]).
pub async fn run_cases(f: &dyn DriverFixture, names: &[&str]) -> Report {
    let driver = f.driver();
    let mut report = Report {
        suite: "runtime driver conformance",
        subject: driver.name().to_owned(),
        cases: Vec::new(),
    };
    let image = match f.program_image().await {
        Ok(i) => Some(i),
        Err(e) => {
            report.cases.push(CaseReport {
                name: "program_image",
                outcome: Outcome::Failed(format!("the fixture has no program image: {e}")),
            });
            None
        }
    };
    for (name, case) in DRIVER_CASE_FNS.iter().filter(|(n, _)| names.contains(n)) {
        let outcome = if let Some(reason) = f.skip(name) {
            Outcome::Skipped(reason)
        } else if let Some(image) = &image {
            let ctx = Ctx {
                f,
                driver: driver.clone(),
                image: image.clone(),
                timing: f.timing(),
                started: std::sync::Mutex::new(Vec::new()),
            };
            let r = AssertUnwindSafe(case(&ctx)).catch_unwind().await;
            ctx.cleanup().await;
            match r {
                Ok(Ok(())) => Outcome::Passed,
                Ok(Err(CaseError::Skip(r))) => Outcome::Skipped(r),
                Ok(Err(CaseError::Fail(m))) => Outcome::Failed(m),
                Err(_) => Outcome::Failed("the case panicked".to_owned()),
            }
        } else {
            Outcome::Failed("no program image".to_owned())
        };
        report.cases.push(CaseReport { name, outcome });
    }
    report
}

/// A service starts, runs, answers its health check, and carries the identity the platform
/// gave. Another tenant cannot see it.
async fn start_service(c: &Ctx<'_>) -> CaseResult {
    let id = new_identity(Environment::Live);
    let s = c.start(c.service(id, &[Step::Serve])).await?;
    check!(
        s.identity == id,
        "the status has another identity: {:?}",
        s.identity
    );
    check!(s.kind == "service", "the kind is {}", s.kind);
    let s = c
        .wait_for(&s, "running and healthy", Duration::ZERO, |x| {
            x.state == InstanceState::Running && x.health == Health::Healthy
        })
        .await?;
    check!(
        s.identity == id,
        "the status has another identity: {:?}",
        s.identity
    );
    expect_err(
        c.driver.status(TenantId::new(), s.id).await,
        "instance_not_found",
        "status for another tenant",
    )?;
    expect_err(
        c.driver.stop(TenantId::new(), s.id).await,
        "instance_not_found",
        "stop for another tenant",
    )?;
    expect_err(
        c.driver.logs(TenantId::new(), s.id, None, 10).await,
        "instance_not_found",
        "logs for another tenant",
    )?;
    let still = c.status(&s).await?;
    check!(
        still.state == InstanceState::Running,
        "a stop for another tenant changed the instance: {}",
        still.state.name()
    );
    Ok(())
}

fn static_image() -> StaticImage {
    StaticImage::new(vec![
        StaticFile {
            path: "index.html".to_owned(),
            content: crate::blobs::Hash::of(b"<h1>hello</h1>"),
            size: 14,
        },
        StaticFile {
            path: "assets/app.js".to_owned(),
            content: crate::blobs::Hash::of(b"console.log(1)"),
            size: 14,
        },
    ])
}

/// A static image runs no code: running, healthy, no network, no routes, no logs. It cannot be
/// a job, and it gets no routes.
async fn start_static(c: &Ctx<'_>) -> CaseResult {
    let mut spec = c.service(new_identity(Environment::Live), &[]);
    spec.image = ImageRef::Static(static_image());
    spec.routes = Vec::new();
    let s = c.start(spec.clone()).await?;
    let s = c
        .wait_for(&s, "running", Duration::ZERO, |x| {
            x.state == InstanceState::Running
        })
        .await?;
    check!(
        s.health == Health::Healthy,
        "a static instance is {:?}",
        s.health
    );
    check!(
        s.applied.network.is_none(),
        "a static instance has a network"
    );
    check!(s.applied.routes.is_empty(), "a static instance has routes");
    check!(c.logs(&s).await?.is_empty(), "a static instance has logs");

    let mut job = c.quick_job(new_identity(Environment::Live), &[]);
    job.image = spec.image.clone();
    job.routes = Vec::new();
    expect_err(
        c.driver.start(job).await,
        "unsupported_image",
        "a static job",
    )?;

    let mut routed = spec.clone();
    routed.routes = vec![Route::EgressProxy];
    expect_err(
        c.driver.start(routed).await,
        "invalid_spec",
        "a static image with a route",
    )?;

    let mut forged = spec;
    if let ImageRef::Static(img) = &mut forged.image {
        img.tree = crate::blobs::Hash::of(b"another tree");
    }
    expect_err(
        c.driver.start(forged).await,
        "invalid_spec",
        "a static image with a wrong tree hash",
    )
}

/// Specs that break the shared rules are refused before anything starts.
async fn invalid_spec_refused(c: &Ctx<'_>) -> CaseResult {
    let base = c.service(new_identity(Environment::Live), &[Step::Serve]);
    let mut bad_name = base.clone();
    bad_name.env = vec![EnvVar::plain("1BAD", "x")];
    expect_err(
        c.driver.start(bad_name).await,
        "invalid_spec",
        "a bad variable name",
    )?;
    let mut twice = base.clone();
    twice.env = vec![EnvVar::plain("A", "x"), EnvVar::plain("A", "y")];
    expect_err(
        c.driver.start(twice).await,
        "invalid_spec",
        "a variable set twice",
    )?;
    let mut no_memory = base.clone();
    no_memory.limits.memory_bytes = 0;
    expect_err(
        c.driver.start(no_memory).await,
        "invalid_spec",
        "a zero memory limit",
    )?;
    let mut routes = base.clone();
    routes.routes = vec![Route::Pooler, Route::Pooler];
    expect_err(
        c.driver.start(routes).await,
        "invalid_spec",
        "a route listed twice",
    )?;
    let mut no_token = base;
    no_token.workload_token = SecretValue::new("");
    expect_err(
        c.driver.start(no_token).await,
        "invalid_spec",
        "an empty workload token",
    )
}

/// An OCI image that is not in the registry is refused.
async fn unknown_image_refused(c: &Ctx<'_>) -> CaseResult {
    let mut spec = c.service(new_identity(Environment::Live), &[Step::Serve]);
    spec.image = ImageRef::Oci(OciDigest::of(Uuid::now_v7().as_bytes()));
    expect_err(
        c.driver.start(spec).await,
        "image_not_found",
        "an unknown image",
    )
}

/// A stopped instance stops; a second stop does nothing.
async fn stop(c: &Ctx<'_>) -> CaseResult {
    let s = c
        .start(c.service(new_identity(Environment::Live), &[Step::Serve]))
        .await?;
    c.wait_for(&s, "running", Duration::ZERO, |x| {
        x.state == InstanceState::Running
    })
    .await?;
    c.driver
        .stop(s.identity.tenant, s.id)
        .await
        .map_err(|e| fail(format!("stop failed: {e}")))?;
    match c.driver.status(s.identity.tenant, s.id).await {
        Ok(x) => check!(
            x.state == InstanceState::Stopped,
            "after stop the state is {}",
            x.state.name()
        ),
        Err(e) => check!(e.code() == "instance_not_found", "status after stop: {e}"),
    }
    match c.driver.stop(s.identity.tenant, s.id).await {
        Ok(()) => {}
        Err(e) => check!(e.code() == "instance_not_found", "a second stop: {e}"),
    }
    match c.driver.wake(s.identity.tenant, s.id).await {
        Ok(_) => Err(fail("a stopped instance woke up")),
        Err(e) => {
            check!(
                matches!(e.code(), "invalid_state" | "instance_not_found"),
                "wake after stop: {e}"
            );
            Ok(())
        }
    }
}

/// A service with an idle timeout scales to zero after it, and a wake starts it again (a cold
/// start: the program runs again from the start).
async fn scale_to_zero_and_wake(c: &Ctx<'_>) -> CaseResult {
    let mut spec = c.service(
        new_identity(Environment::Live),
        &[Step::Print("boot".into()), Step::Serve],
    );
    if let WorkloadKind::Service(s) = &mut spec.kind {
        s.idle_timeout = Some(c.timing.idle_timeout);
    }
    let s = c.start(spec).await?;
    c.wait_line(&s, "boot").await?;
    c.wait_for(&s, "scaled to zero", c.timing.idle_timeout, |x| {
        x.state == InstanceState::ScaledToZero
    })
    .await?;
    let woke = c
        .driver
        .wake(s.identity.tenant, s.id)
        .await
        .map_err(|e| fail(format!("wake failed: {e}")))?;
    check!(
        matches!(woke.state, InstanceState::Running | InstanceState::Starting),
        "after wake the state is {}",
        woke.state.name()
    );
    c.wait_for(&s, "running and healthy after wake", Duration::ZERO, |x| {
        x.state == InstanceState::Running && x.health == Health::Healthy
    })
    .await?;
    let deadline = Instant::now() + c.timing.wait;
    loop {
        let logs = c.logs(&s).await?;
        if logs.iter().filter(|l| l.text == "boot").count() >= 2 {
            break;
        }
        check!(
            Instant::now() < deadline,
            "the program did not start again after wake: {:?}",
            texts(&logs)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // It scales to zero again after the next idle time.
    c.wait_for(&s, "scaled to zero again", c.timing.idle_timeout, |x| {
        x.state == InstanceState::ScaledToZero
    })
    .await?;
    Ok(())
}

/// Requests (touch) keep a service awake past its idle timeout.
async fn touch_keeps_service_awake(c: &Ctx<'_>) -> CaseResult {
    let mut spec = c.service(new_identity(Environment::Live), &[Step::Serve]);
    if let WorkloadKind::Service(s) = &mut spec.kind {
        s.idle_timeout = Some(c.timing.idle_timeout);
    }
    let s = c.start(spec).await?;
    c.wait_for(&s, "running", Duration::ZERO, |x| {
        x.state == InstanceState::Running
    })
    .await?;
    let until = Instant::now() + c.timing.idle_timeout * 3;
    while Instant::now() < until {
        c.driver
            .touch(s.identity.tenant, s.id)
            .await
            .map_err(|e| fail(format!("touch failed: {e}")))?;
        tokio::time::sleep(c.timing.idle_timeout / 4).await;
        let now = c.status(&s).await?;
        check!(
            now.state == InstanceState::Running,
            "a touched service became {}",
            now.state.name()
        );
    }
    Ok(())
}

/// A job runs to completion with its exit code; a finished job cannot be woken.
async fn job_completes(c: &Ctx<'_>) -> CaseResult {
    let ok = c
        .start(c.quick_job(
            new_identity(Environment::Live),
            &[Step::Print("working".into()), Step::Exit(0)],
        ))
        .await?;
    check!(ok.kind == "job", "the kind is {}", ok.kind);
    let exit = c.wait_exit(&ok, Duration::ZERO).await?;
    check!(exit.success(), "the job ended with {exit:?}");
    let logs = c.logs(&ok).await?;
    check!(
        logs.iter().any(|l| l.text == "working"),
        "the job's output is missing: {:?}",
        texts(&logs)
    );
    expect_err(
        c.driver.wake(ok.identity.tenant, ok.id).await,
        "invalid_state",
        "wake of a finished job",
    )?;

    let failed = c
        .start(c.quick_job(new_identity(Environment::Live), &[Step::Exit(3)]))
        .await?;
    let exit = c.wait_exit(&failed, Duration::ZERO).await?;
    check!(
        exit == ExitStatus::completed(3),
        "a job that exits with 3 ended with {exit:?}"
    );

    let fell_off = c
        .start(c.quick_job(
            new_identity(Environment::Live),
            &[Step::Print("done".into())],
        ))
        .await?;
    let exit = c.wait_exit(&fell_off, Duration::ZERO).await?;
    check!(
        exit.success(),
        "a job that ends its program ended with {exit:?}"
    );
    Ok(())
}

/// A job that runs longer than its timeout ends as timed out.
async fn job_times_out(c: &Ctx<'_>) -> CaseResult {
    let t = c.timing.job_timeout;
    let s = c
        .start(c.job(
            new_identity(Environment::Live),
            &[
                Step::Print("start".into()),
                Step::Sleep(t * 20),
                Step::Exit(0),
            ],
            t,
        ))
        .await?;
    let exit = c.wait_exit(&s, t).await?;
    check!(
        exit.reason == ExitReason::TimedOut,
        "a job past its timeout ended with {exit:?}"
    );
    Ok(())
}

/// Log lines come in order, with growing positions and timestamps, the stream they came from,
/// and the identity of the instance. The cursor pages.
async fn logs_in_order(c: &Ctx<'_>) -> CaseResult {
    let mut steps: Vec<Step> = (0..50)
        .map(|i| Step::Print(format!("line-{i:02}")))
        .collect();
    steps.push(Step::Stderr("on-stderr".into()));
    steps.push(Step::Exit(0));
    let id = new_identity(Environment::Preview);
    let s = c.start(c.quick_job(id, &steps)).await?;
    c.wait_exit(&s, Duration::ZERO).await?;
    let logs = c.logs(&s).await?;
    let out: Vec<&str> = logs
        .iter()
        .filter(|l| l.stream == LogStream::Stdout)
        .map(|l| l.text.as_str())
        .collect();
    let want: Vec<String> = (0..50).map(|i| format!("line-{i:02}")).collect();
    check!(out == want, "standard output is not in order: {out:?}");
    let err: Vec<&LogLine> = logs
        .iter()
        .filter(|l| l.stream == LogStream::Stderr)
        .collect();
    check!(
        err.len() == 1 && err[0].text == "on-stderr",
        "standard error is wrong: {err:?}"
    );
    for w in logs.windows(2) {
        check!(
            w[0].seq < w[1].seq,
            "positions do not grow: {} then {}",
            w[0].seq,
            w[1].seq
        );
    }
    let stdout: Vec<&LogLine> = logs
        .iter()
        .filter(|l| l.stream == LogStream::Stdout)
        .collect();
    for w in stdout.windows(2) {
        check!(
            w[0].at <= w[1].at,
            "timestamps go back: {} then {}",
            w[0].at,
            w[1].at
        );
    }
    for l in &logs {
        check!(l.instance == s.id, "a line names instance {}", l.instance);
        check!(l.identity == id, "a line has identity {:?}", l.identity);
    }
    let tenth = logs[9].seq;
    let page = c
        .driver
        .logs(id.tenant, s.id, Some(tenth), 5)
        .await
        .map_err(|e| fail(format!("logs failed: {e}")))?;
    check!(
        page.len() == 5
            && page
                .iter()
                .map(|l| &l.seq)
                .eq(logs[10..15].iter().map(|l| &l.seq)),
        "the cursor does not page: {:?}",
        texts(&page)
    );
    Ok(())
}

/// A secret value reaches the workload at start, and neither it, a previous value nor the
/// workload token appears in the log: each is the marker.
async fn secrets_redacted(c: &Ctx<'_>) -> CaseResult {
    let current = secret_text("cur");
    let previous = secret_text("prev");
    let mut spec = c.quick_job(
        new_identity(Environment::Live),
        &[
            Step::Env("API_KEY".into()),
            Step::Env("MODE".into()),
            Step::Print(format!("old={previous};")),
            Step::Token,
            Step::Stderr(format!("err {current} err")),
            Step::Exit(0),
        ],
    );
    let tok = spec.workload_token.expose().to_owned();
    spec.env = vec![
        EnvVar::secret("API_KEY", SecretValue::new(current.clone())),
        EnvVar::plain("MODE", "visible"),
    ];
    spec.previous_secrets = vec![SecretValue::new(previous.clone())];
    let s = c.start(spec).await?;
    c.wait_exit(&s, Duration::ZERO).await?;
    let logs = c.logs(&s).await?;
    for l in &logs {
        for (what, v) in [
            ("a secret", &current),
            ("a previous secret", &previous),
            ("the token", &tok),
        ] {
            check!(
                !l.text.contains(v.as_str()),
                "the log holds {what}: line {}",
                l.seq
            );
        }
    }
    let t = texts(&logs);
    let want_key = format!("API_KEY={REDACTED}");
    let want_old = format!("old={REDACTED};");
    let want_err = format!("err {REDACTED} err");
    for want in [
        want_key.as_str(),
        "MODE=visible",
        want_old.as_str(),
        REDACTED,
        want_err.as_str(),
    ] {
        check!(t.contains(&want), "the log has no line {want:?}: {t:?}");
    }
    Ok(())
}

/// A secret value written in two pieces is still redacted.
async fn secret_split_across_writes_redacted(c: &Ctx<'_>) -> CaseResult {
    let value = secret_text("split");
    let (a, b) = value.split_at(value.len() / 2);
    let mut spec = c.quick_job(
        new_identity(Environment::Live),
        &[
            Step::Write(format!("abc{a}")),
            Step::Sleep(Duration::from_millis(30)),
            Step::Print(format!("{b}xyz")),
            Step::Write(format!("tail{a}")),
            Step::Exit(0),
        ],
    );
    spec.env = vec![EnvVar::secret("SPLIT", SecretValue::new(value.clone()))];
    let s = c.start(spec).await?;
    c.wait_exit(&s, Duration::ZERO).await?;
    let logs = c.logs(&s).await?;
    let t = texts(&logs);
    let want = format!("abc{REDACTED}xyz");
    check!(
        t.contains(&want.as_str()),
        "the split value is not redacted: {t:?}"
    );
    check!(
        !logs.iter().any(|l| l.text.contains(&value)),
        "the log holds the value"
    );
    let tail = format!("tail{a}");
    check!(
        t.contains(&tail.as_str()),
        "a half value at the end of the output is lost: {t:?}"
    );
    Ok(())
}

/// After a token renewal, neither the new nor the old token appears in the log.
async fn rotated_token_redacted(c: &Ctx<'_>) -> CaseResult {
    let pause = Duration::from_millis(300).max(c.timing.health_interval * 3);
    let spec = c.service(
        new_identity(Environment::Live),
        &[
            Step::Token,
            Step::Sleep(pause),
            Step::Token,
            Step::Print("after".into()),
            Step::Serve,
        ],
    );
    let old = spec.workload_token.expose().to_owned();
    let s = c.start(spec).await?;
    let new = token();
    let new_text = new.expose().to_owned();
    c.driver
        .rotate_token(s.identity.tenant, s.id, new)
        .await
        .map_err(|e| fail(format!("rotate_token failed: {e}")))?;
    let logs = c.wait_line(&s, "after").await?;
    for l in &logs {
        check!(!l.text.contains(&old), "the log holds the old token");
        check!(!l.text.contains(&new_text), "the log holds the new token");
    }
    let marks = logs.iter().filter(|l| l.text == REDACTED).count();
    check!(
        marks == 2,
        "expected two redacted tokens, got {:?}",
        texts(&logs)
    );
    Ok(())
}

/// A failing health check marks the instance unhealthy.
async fn health_failing_marks_unhealthy(c: &Ctx<'_>) -> CaseResult {
    let s = c
        .start(c.service(new_identity(Environment::Live), &[Step::Unhealthy]))
        .await?;
    c.wait_for(&s, "unhealthy", c.timing.health_interval * 4, |x| {
        x.health == Health::Unhealthy
    })
    .await?;
    Ok(())
}

/// The applied limits are exactly the requested ones, for services and jobs.
async fn limits_recorded(c: &Ctx<'_>) -> CaseResult {
    let mut spec = c.service(new_identity(Environment::Live), &[Step::Serve]);
    spec.limits = Limits {
        cpu_millis: 500,
        memory_bytes: 96 * 1024 * 1024,
        disk_bytes: 32 * 1024 * 1024,
    };
    let want = spec.limits;
    let s = c.start(spec).await?;
    check!(
        s.applied.limits == want,
        "applied {:?}, asked {want:?}",
        s.applied.limits
    );
    let s = c.status(&s).await?;
    check!(
        s.applied.limits == want,
        "applied {:?}, asked {want:?}",
        s.applied.limits
    );
    let j = c
        .start(c.quick_job(new_identity(Environment::Live), &[Step::Exit(0)]))
        .await?;
    check!(
        j.applied.limits == limits(),
        "a job applied {:?}",
        j.applied.limits
    );
    Ok(())
}

/// A program that goes over its memory limit is ended as out of memory.
async fn memory_limit_enforced(c: &Ctx<'_>) -> CaseResult {
    let mut spec = c.quick_job(
        new_identity(Environment::Live),
        &[
            Step::Alloc(48 * 1024 * 1024),
            Step::Alloc(48 * 1024 * 1024),
            Step::Sleep(Duration::from_millis(200)),
            Step::Exit(0),
        ],
    );
    spec.limits.memory_bytes = 64 * 1024 * 1024;
    let s = c.start(spec).await?;
    let exit = c.wait_exit(&s, Duration::ZERO).await?;
    check!(
        exit.reason == ExitReason::OutOfMemory,
        "a job over its memory limit ended with {exit:?}"
    );
    let mut within = c.quick_job(
        new_identity(Environment::Live),
        &[Step::Alloc(8 * 1024 * 1024), Step::Exit(0)],
    );
    within.limits.memory_bytes = 64 * 1024 * 1024;
    let s = c.start(within).await?;
    let exit = c.wait_exit(&s, Duration::ZERO).await?;
    check!(
        exit.success(),
        "a job within its memory limit ended with {exit:?}"
    );
    Ok(())
}

/// A program that writes more than its disk limit gets a full disk.
async fn disk_limit_enforced(c: &Ctx<'_>) -> CaseResult {
    let mut spec = c.quick_job(
        new_identity(Environment::Live),
        &[
            Step::Disk(12 * 1024 * 1024),
            Step::Disk(12 * 1024 * 1024),
            Step::Print("wrote".into()),
            Step::Exit(0),
        ],
    );
    spec.limits.disk_bytes = 16 * 1024 * 1024;
    let s = c.start(spec).await?;
    let exit = c.wait_exit(&s, Duration::ZERO).await?;
    let logs = c.logs(&s).await?;
    let t = texts(&logs);
    check!(t.contains(&DISK_FULL), "no full disk over the limit: {t:?}");
    check!(
        !t.contains(&"wrote"),
        "the program wrote past its disk limit: {t:?}"
    );
    check!(
        exit == ExitStatus::completed(1),
        "the program ended with {exit:?}"
    );
    Ok(())
}

/// The applied routes are exactly the spec's: none for none, never one more.
async fn network_routes_exact(c: &Ctx<'_>) -> CaseResult {
    for routes in [
        vec![],
        vec![Route::EgressProxy],
        vec![Route::InternalGateway],
        vec![Route::Pooler, Route::InternalGateway, Route::EgressProxy],
    ] {
        let mut spec = c.service(new_identity(Environment::Live), &[Step::Serve]);
        spec.routes = routes.clone();
        let s = c.start(spec).await?;
        let s = c
            .wait_for(&s, "running", Duration::ZERO, |x| {
                x.state == InstanceState::Running
            })
            .await?;
        check!(
            sorted(&s.applied.routes) == sorted(&routes),
            "asked routes {routes:?}, applied {:?}",
            s.applied.routes
        );
        check!(
            s.applied.network.is_some(),
            "an instance that runs code has no network"
        );
    }
    Ok(())
}

/// Two apps, of one tenant or of two, never share a network.
async fn apps_never_share_a_network(c: &Ctx<'_>) -> CaseResult {
    let tenant = TenantId::new();
    let a = c
        .start(c.service(
            identity(tenant, NodeId::new(), Environment::Live),
            &[Step::Serve],
        ))
        .await?;
    let b = c
        .start(c.service(
            identity(tenant, NodeId::new(), Environment::Live),
            &[Step::Serve],
        ))
        .await?;
    let other = c
        .start(c.service(new_identity(Environment::Live), &[Step::Serve]))
        .await?;
    let nets: Vec<Option<NetworkId>> = vec![
        c.status(&a).await?.applied.network,
        c.status(&b).await?.applied.network,
        c.status(&other).await?.applied.network,
    ];
    check!(
        nets.iter().all(Option::is_some),
        "an instance has no network: {nets:?}"
    );
    check!(
        nets[0] != nets[1] && nets[0] != nets[2] && nets[1] != nets[2],
        "two apps share a network: {nets:?}"
    );
    Ok(())
}

/// The live and the preview instances of one app are separate: two instances, two networks,
/// their own identity on every line, and stopping one leaves the other.
async fn preview_and_live_are_separate(c: &Ctx<'_>) -> CaseResult {
    let tenant = TenantId::new();
    let app = NodeId::new();
    let live_id = identity(tenant, app, Environment::Live);
    let preview_id = identity(tenant, app, Environment::Preview);
    let live = c
        .start(c.service(live_id, &[Step::Print("hello".into()), Step::Serve]))
        .await?;
    let preview = c
        .start(c.service(preview_id, &[Step::Print("hello".into()), Step::Serve]))
        .await?;
    check!(live.id != preview.id, "one instance for live and preview");
    let live = c
        .wait_for(&live, "running", Duration::ZERO, |x| {
            x.state == InstanceState::Running
        })
        .await?;
    let preview = c
        .wait_for(&preview, "running", Duration::ZERO, |x| {
            x.state == InstanceState::Running
        })
        .await?;
    check!(
        live.applied.network.is_some() && live.applied.network != preview.applied.network,
        "live and preview share a network: {:?}",
        live.applied.network
    );
    check!(
        live.identity.environment == Environment::Live,
        "the live instance says {:?}",
        live.identity
    );
    check!(
        preview.identity.environment == Environment::Preview,
        "the preview instance says {:?}",
        preview.identity
    );
    for (s, want) in [(&live, live_id), (&preview, preview_id)] {
        let logs = c.wait_line(s, "hello").await?;
        for l in &logs {
            check!(
                l.identity == want && l.instance == s.id,
                "a line of {} names {:?}",
                s.id,
                l.identity
            );
        }
    }
    c.driver
        .stop(tenant, preview.id)
        .await
        .map_err(|e| fail(format!("stop failed: {e}")))?;
    let live = c.status(&live).await?;
    check!(
        live.state == InstanceState::Running,
        "stopping the preview changed the live instance: {}",
        live.state.name()
    );
    Ok(())
}

/// Each run reports its instance time and memory, with the identity of the instance.
async fn usage_reported(c: &Ctx<'_>) -> CaseResult {
    let id = new_identity(Environment::Preview);
    let s = c.start(c.service(id, &[Step::Serve])).await?;
    c.wait_for(&s, "running", Duration::ZERO, |x| {
        x.state == InstanceState::Running
    })
    .await?;
    tokio::time::sleep(Duration::from_millis(50)).await;
    c.driver
        .stop(id.tenant, s.id)
        .await
        .map_err(|e| fail(format!("stop failed: {e}")))?;
    let job = c
        .start(c.quick_job(
            new_identity(Environment::Live),
            &[Step::Sleep(Duration::from_millis(30)), Step::Exit(0)],
        ))
        .await?;
    c.wait_exit(&job, Duration::ZERO).await?;
    let usage = c.f.usage();
    let deadline = Instant::now() + c.timing.wait;
    loop {
        let records = usage.records();
        let of = |inst: InstanceId| {
            records.iter().find_map(|r| match r {
                RuntimeUsage::Instance {
                    identity,
                    instance,
                    ran,
                    memory_bytes,
                } if *instance == inst => Some((*identity, *ran, *memory_bytes)),
                _ => None,
            })
        };
        if let (Some(service), Some(job_usage)) = (of(s.id), of(job.id)) {
            check!(service.0 == id, "the usage names {:?}", service.0);
            check!(!service.1.is_zero(), "the service ran zero time");
            check!(
                service.2 == limits().memory_bytes,
                "the usage has memory {}",
                service.2
            );
            check!(
                job_usage.0 == job.identity,
                "the job usage names {:?}",
                job_usage.0
            );
            return Ok(());
        }
        check!(
            Instant::now() < deadline,
            "no usage for the service or the job: {records:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ------------------------------------------------------------------------------ builder cases

type BuilderCase = for<'a> fn(&'a dyn BuilderFixture) -> BoxFuture<'a, CaseResult>;

/// The names of the builder cases, in run order.
pub const BUILDER_CASES: &[&str] = &[
    "build_makes_oci_image",
    "build_failure_is_typed",
    "build_unknown_runtime_refused",
    "build_invalid_input_refused",
    "build_gets_only_egress_and_its_limits",
];

const BUILDER_CASE_FNS: &[(&str, BuilderCase)] = &[
    ("build_makes_oci_image", |f| {
        Box::pin(build_makes_oci_image(f))
    }),
    ("build_failure_is_typed", |f| {
        Box::pin(build_failure_is_typed(f))
    }),
    ("build_unknown_runtime_refused", |f| {
        Box::pin(build_unknown_runtime_refused(f))
    }),
    ("build_invalid_input_refused", |f| {
        Box::pin(build_invalid_input_refused(f))
    }),
    ("build_gets_only_egress_and_its_limits", |f| {
        Box::pin(build_gets_only_egress_and_its_limits(f))
    }),
];

/// Runs every builder case and returns the report.
pub async fn run_builder(f: &dyn BuilderFixture) -> Report {
    let mut report = Report {
        suite: "runtime builder conformance",
        subject: f.builder().name().to_owned(),
        cases: Vec::new(),
    };
    for (name, case) in BUILDER_CASE_FNS {
        let outcome = if let Some(reason) = f.skip(name) {
            Outcome::Skipped(reason)
        } else {
            match AssertUnwindSafe(case(f)).catch_unwind().await {
                Ok(Ok(())) => Outcome::Passed,
                Ok(Err(CaseError::Skip(r))) => Outcome::Skipped(r),
                Ok(Err(CaseError::Fail(m))) => Outcome::Failed(m),
                Err(_) => Outcome::Failed("the case panicked".to_owned()),
            }
        };
        report.cases.push(CaseReport { name, outcome });
    }
    report
}

fn build_input(f: &dyn BuilderFixture, archive: Bytes) -> BuildInput {
    BuildInput {
        identity: new_identity(Environment::Preview),
        runtime: f.runtime(),
        archive,
        limits: limits(),
        timeout: Duration::from_secs(600),
    }
}

/// An archive becomes an OCI image; the build time is reported with the build's identity.
async fn build_makes_oci_image(f: &dyn BuilderFixture) -> CaseResult {
    let input = build_input(f, f.archive());
    let id = input.identity;
    let out = f
        .builder()
        .build(input)
        .await
        .map_err(|e| fail(format!("the build failed: {e}")))?;
    check!(
        matches!(out.image, ImageRef::Oci(_)),
        "the build made a {} image",
        out.image.kind()
    );
    check!(!out.took.is_zero(), "the build took no time");
    let records = f.usage().records();
    check!(
        records.iter().any(|r| matches!(r, RuntimeUsage::Build { identity, took } if *identity == id && *took == out.took)),
        "no build usage for the build: {records:?}"
    );
    Ok(())
}

/// A build that fails is `build_failed`, and its time is reported too.
async fn build_failure_is_typed(f: &dyn BuilderFixture) -> CaseResult {
    let input = build_input(f, f.failing_archive());
    let id = input.identity;
    expect_err(
        f.builder().build(input).await,
        "build_failed",
        "a failing build",
    )?;
    let records = f.usage().records();
    check!(
        records
            .iter()
            .any(|r| matches!(r, RuntimeUsage::Build { identity, .. } if *identity == id)),
        "no build usage for the failed build"
    );
    Ok(())
}

/// An unknown runtime is `unsupported_runtime`.
async fn build_unknown_runtime_refused(f: &dyn BuilderFixture) -> CaseResult {
    let mut input = build_input(f, f.archive());
    input.runtime = "cobol85".to_owned();
    expect_err(
        f.builder().build(input).await,
        "unsupported_runtime",
        "an unknown runtime",
    )
}

/// An empty archive and zero limits are refused.
async fn build_invalid_input_refused(f: &dyn BuilderFixture) -> CaseResult {
    let empty = build_input(f, Bytes::new());
    expect_err(
        f.builder().build(empty).await,
        "invalid_spec",
        "an empty archive",
    )?;
    let mut zero = build_input(f, f.archive());
    zero.limits.cpu_millis = 0;
    expect_err(
        f.builder().build(zero).await,
        "invalid_spec",
        "a zero CPU limit",
    )
}

/// A build has only the egress proxy as a route, and runs with exactly its limits.
async fn build_gets_only_egress_and_its_limits(f: &dyn BuilderFixture) -> CaseResult {
    let mut input = build_input(f, f.archive());
    input.limits.memory_bytes = 512 * 1024 * 1024;
    let want = input.limits;
    let out = f
        .builder()
        .build(input)
        .await
        .map_err(|e| fail(format!("the build failed: {e}")))?;
    check!(
        out.routes.iter().all(|r| BUILD_ROUTES.contains(r)),
        "the build had routes {:?}",
        out.routes
    );
    check!(
        out.limits == want,
        "the build ran with {:?}, asked {want:?}",
        out.limits
    );
    Ok(())
}
