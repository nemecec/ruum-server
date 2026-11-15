//! The hostile sandbox suite of the Docker driver (card R-2a; decision 0006 G; STEP3 "How an app
//! runs" rules 1, 2 and 7). Workloads start through [`DockerDriver`], so the sandbox under test is
//! the driver's own; probes run inside it (`testdata/fixture/main.js`) and print their results
//! to the log.
//!
//! The rules of S-2 hold: **no probe passes on silence.** Every "not reached" needs a positive
//! control that reaches the same target at the same time (from the host, from a container on
//! the target's network, or from a plain internal network for the host behind the bridge). A
//! target without an address, or a control that fails, fails the run, and the probe must answer
//! for exactly the expected set of targets.
//!
//! Target sets: `RUUM_SANDBOX_FULL=1` (the build server) adds the dev stack (SpiceDB, OpenBao,
//! NATS, Garage, the platform and data cluster Postgres of the agent) and the host's public
//! IPv6 address, which are required then. Without it (CI) the targets that exist there run and
//! are required: the Postgres of `DATABASE_URL`, the data cluster when it runs, the metadata
//! address, the host (its addresses and through the app bridge), another app, another tenant,
//! the Docker socket (file and TCP), UDP, IPv6 link-local and DNS. The test prints which ran.
//! `RUUM_SANDBOX_CASES` (comma-separated case names) runs some cases only, for the mutation
//! checks; a full run sets nothing.

mod common;

use common::{Harness, LABEL, docker, engine_or_skip, new_usage};
use ruum_core::ids::{NodeId, TenantId};
use ruum_core::runtime::redact::REDACTED;
use ruum_core::runtime::{
    EnvVar, Environment, ExitReason, ExitStatus, HealthCheck, ImageRef, InstanceId, InstanceState,
    JobSpec, Limits, LogLine, Route, RuntimeDriver, SecretValue, ServiceSpec, WorkloadIdentity,
    WorkloadKind, WorkloadSpec,
};
use ruum_runtime::{DockerDriver, RuntimeKind};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

// ------------------------------------------------------------------------------ report

#[derive(Default)]
struct Report {
    rows: Vec<(String, bool, String)>,
}

impl Report {
    fn check(&mut self, name: impl Into<String>, ok: bool, detail: impl Into<String>) {
        self.rows.push((name.into(), ok, detail.into()));
    }
    fn note(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.rows
            .push((format!("note: {}", name.into()), true, detail.into()));
    }
    fn failed(&self) -> Vec<&(String, bool, String)> {
        self.rows.iter().filter(|r| !r.1).collect()
    }
    fn print(&self, title: &str) {
        println!("== {title}");
        for (n, ok, d) in &self.rows {
            println!(
                "  {}  {n}{}",
                if *ok { "pass" } else { "FAIL" },
                if d.is_empty() {
                    String::new()
                } else {
                    format!(" ({d})")
                }
            );
        }
    }
}

fn full() -> bool {
    std::env::var("RUUM_SANDBOX_FULL").as_deref() == Ok("1")
}

fn wanted(case: &str) -> bool {
    match std::env::var("RUUM_SANDBOX_CASES") {
        Ok(v) if !v.is_empty() => v.split(',').any(|c| c == case),
        _ => true,
    }
}

// ------------------------------------------------------------------------------ helpers

struct Ctx {
    h: Harness,
    driver: Arc<DockerDriver>,
    runtime: RuntimeKind,
}

fn identity(tenant: TenantId, app: NodeId) -> WorkloadIdentity {
    WorkloadIdentity {
        tenant,
        app,
        deployment: uuid::Uuid::now_v7(),
        environment: Environment::Live,
    }
}

fn limits() -> Limits {
    Limits {
        cpu_millis: 1000,
        memory_bytes: 256 * 1024 * 1024,
        disk_bytes: 16 * 1024 * 1024,
    }
}

fn token() -> SecretValue {
    SecretValue::new(format!("wt_{}", uuid::Uuid::now_v7().simple()))
}

impl Ctx {
    fn image(&self, id: &WorkloadIdentity) -> ImageRef {
        self.h.push(id).expect("fixture image")
    }

    fn job(&self, id: WorkloadIdentity, command: Vec<String>, routes: Vec<Route>) -> WorkloadSpec {
        WorkloadSpec {
            identity: id,
            kind: WorkloadKind::Job(JobSpec {
                timeout: Duration::from_secs(90),
            }),
            image: self.image(&id),
            command,
            env: Vec::new(),
            previous_secrets: Vec::new(),
            workload_token: token(),
            limits: limits(),
            routes,
        }
    }

    fn service(
        &self,
        id: WorkloadIdentity,
        command: Vec<String>,
        routes: Vec<Route>,
    ) -> WorkloadSpec {
        WorkloadSpec {
            kind: WorkloadKind::Service(ServiceSpec {
                port: 8080,
                health: Some(HealthCheck {
                    path: "/health".to_owned(),
                    interval: Duration::from_secs(1),
                    failure_threshold: 3,
                }),
                idle_timeout: None,
            }),
            ..self.job(id, command, routes)
        }
    }

    async fn wait_state(
        &self,
        tenant: TenantId,
        id: InstanceId,
        pred: impl Fn(&InstanceState) -> bool,
        secs: u64,
    ) -> InstanceState {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let s = self.driver.status(tenant, id).await.expect("status").state;
            if pred(&s) || Instant::now() > deadline {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn logs(&self, tenant: TenantId, id: InstanceId) -> Vec<LogLine> {
        let mut out: Vec<LogLine> = Vec::new();
        loop {
            let page = self
                .driver
                .logs(tenant, id, out.last().map(|l| l.seq), 500)
                .await
                .expect("logs");
            if page.is_empty() {
                return out;
            }
            out.extend(page);
        }
    }

    /// Runs a probe job to its end; its exit and its results by name.
    async fn probe(
        &self,
        id: WorkloadIdentity,
        cfg: Value,
        routes: Vec<Route>,
    ) -> (InstanceState, BTreeMap<String, Value>, Vec<LogLine>) {
        let spec = self.job(id, vec![format!("probe:{cfg}")], routes);
        let s = self.driver.start(spec).await.expect("probe job starts");
        let st = self
            .wait_state(
                id.tenant,
                s.id,
                |s| matches!(s, InstanceState::Exited(_)),
                120,
            )
            .await;
        let logs = self.logs(id.tenant, s.id).await;
        (st, results(&logs), logs)
    }

    async fn start_running(&self, spec: WorkloadSpec) -> InstanceId {
        let tenant = spec.identity.tenant;
        let s = self.driver.start(spec).await.expect("start");
        let st = self
            .wait_state(tenant, s.id, |s| *s == InstanceState::Running, 60)
            .await;
        assert_eq!(st, InstanceState::Running, "the service runs");
        s.id
    }

    fn ip_of(&self, id: InstanceId) -> String {
        let c = self.driver.container_of(id).expect("container");
        docker(&[
            "inspect",
            "-f",
            "{{range .NetworkSettings.Networks}}{{.IPAddress}} {{end}}",
            &c,
        ])
        .unwrap_or_default()
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_owned()
    }
}

fn results(logs: &[LogLine]) -> BTreeMap<String, Value> {
    logs.iter()
        .filter_map(|l| l.text.strip_prefix("probe "))
        .filter_map(|j| serde_json::from_str::<Value>(j).ok())
        .filter_map(|v| Some((v.get("name")?.as_str()?.to_owned(), v)))
        .collect()
}

fn reached(v: Option<&Value>) -> bool {
    v.and_then(|v| v.get("reached")).and_then(Value::as_bool) == Some(true)
}

fn detail(v: Option<&Value>) -> String {
    v.and_then(|v| v.get("detail"))
        .and_then(Value::as_str)
        .unwrap_or("no result")
        .to_owned()
}

fn host_tcp(addr: &str, port: u16) -> String {
    let target = if addr.contains(':') {
        format!("[{addr}]:{port}")
    } else {
        format!("{addr}:{port}")
    };
    let sock: SocketAddr = match target.parse() {
        Ok(s) => s,
        Err(_) => {
            // A scoped link-local address (fe80::1%br-x) does not parse as SocketAddr; ask ip(8)-free
            // resolution through the standard resolver.
            match std::net::ToSocketAddrs::to_socket_addrs(&target)
                .ok()
                .and_then(|mut a| a.next())
            {
                Some(s) => s,
                None => return format!("bad address {target}"),
            }
        }
    };
    match TcpStream::connect_timeout(&sock, Duration::from_secs(3)) {
        Ok(_) => "connected".to_owned(),
        Err(e) => e.kind().to_string(),
    }
}

fn host_udp(addr: &str, port: u16) -> String {
    let ip: IpAddr = match addr.parse() {
        Ok(i) => i,
        Err(_) => return format!("bad address {addr}"),
    };
    let bind = if ip.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
    let Ok(s) = UdpSocket::bind(bind) else {
        return "no socket".to_owned();
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let msg = format!("ctl-{}", uuid::Uuid::now_v7());
    if s.send_to(msg.as_bytes(), SocketAddr::new(ip, port))
        .is_err()
    {
        return "send failed".to_owned();
    }
    let mut buf = [0u8; 256];
    match s.recv_from(&mut buf) {
        Ok((n, _)) if &buf[..n] == msg.as_bytes() => "echo".to_owned(),
        Ok(_) => "other".to_owned(),
        Err(e) => e.kind().to_string(),
    }
}

/// A TCP listener on every address of the host (dual stack), accepting and closing.
fn listen_tcp(port: u16) -> Option<u16> {
    let l = TcpListener::bind(("::", port)).ok()?;
    let p = l.local_addr().ok()?.port();
    std::thread::spawn(move || {
        for c in l.incoming() {
            drop(c);
        }
    });
    Some(p)
}

fn listen_udp_echo() -> u16 {
    let s = UdpSocket::bind("[::]:0").expect("udp echo");
    let p = s.local_addr().expect("udp port").port();
    std::thread::spawn(move || {
        let mut buf = [0u8; 512];
        while let Ok((n, from)) = s.recv_from(&mut buf) {
            let _ = s.send_to(&buf[..n], from);
        }
    });
    p
}

/// The host's source address towards the internet (no packet is sent).
fn host_addr(v6: bool) -> Option<IpAddr> {
    let (bind, to) = if v6 {
        ("[::]:0", "[2606:4700:4700::1111]:53")
    } else {
        ("0.0.0.0:0", "1.1.1.1:53")
    };
    let s = UdpSocket::bind(bind).ok()?;
    s.connect(to).ok()?;
    let ip = s.local_addr().ok()?.ip();
    match ip {
        IpAddr::V6(a) if (a.segments()[0] & 0xe000) != 0x2000 => None,
        _ => Some(ip),
    }
}

fn container_ip(name: &str, network: Option<&str>) -> Option<String> {
    let fmt = match network {
        Some(n) => format!("{{{{(index .NetworkSettings.Networks \"{n}\").IPAddress}}}}"),
        None => "{{range .NetworkSettings.Networks}}{{.IPAddress}} {{end}}".to_owned(),
    };
    let ip = docker(&["inspect", "-f", &fmt, name]).ok()?;
    let ip = ip.split_whitespace().next()?.to_owned();
    ip.parse::<IpAddr>().ok().map(|_| ip)
}

/// The container that publishes a host port (the Postgres of DATABASE_URL and the like).
fn container_publishing(port: &str) -> Option<String> {
    let list = docker(&["ps", "--format", "{{.Names}}\t{{.Ports}}"]).ok()?;
    list.lines().find_map(|l| {
        let (name, ports) = l.split_once('\t')?;
        ports
            .split(", ")
            .any(|p| p.contains(&format!(":{port}->")))
            .then(|| name.to_owned())
    })
}

fn url_port(var: &str) -> Option<String> {
    let v = std::env::var(var).ok()?;
    let after = v.rsplit('@').next().unwrap_or(&v);
    let after = after
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let hostport = after.split(['/', '?']).next()?;
    hostport.rsplit(':').next().map(str::to_owned)
}

#[derive(Clone)]
enum Control {
    /// The host connects to this address.
    Host(String),
    /// The result of this name in the plain-network probe.
    Plain(String),
    /// The result of this name in a control probe on the target's own network.
    Net(String),
}

#[derive(Clone)]
struct Target {
    name: String,
    host: String,
    port: u16,
    udp: bool,
    control: Control,
}

// ------------------------------------------------------------------------------ the test

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_sandbox_holds() {
    if !engine_or_skip("sandbox") {
        return;
    }
    let mut h = Harness::start();
    let mut r = Report::default();
    // The full run uses R-2e's egress proxy (its image from this tree) in place of the stand-in.
    let egress_ready = if full() {
        match h.start_egress(&egress_deny()) {
            Ok(()) => true,
            Err(e) => {
                r.check("egress: R-2e's proxy builds and starts", false, e);
                false
            }
        }
    } else {
        false
    };
    let driver = match &h.egress {
        Some(e) => {
            let registry: Arc<dyn ruum_runtime::EgressRegistry> = e.registry.clone();
            h.driver_with(h.config(), new_usage(), registry).await
        }
        None => h.driver(new_usage()).await,
    };
    let runtime = common::runtime_kind();
    let c = Ctx { h, driver, runtime };
    r.note(
        "runtime",
        format!(
            "{:?}, target set {}",
            c.runtime,
            if full() {
                "full (RUUM_SANDBOX_FULL=1)"
            } else {
                "CI"
            }
        ),
    );
    if wanted("privilege") {
        privilege(&c, &mut r).await;
    }
    if wanted("forkbomb") {
        forkbomb(&c, &mut r).await;
    }
    if wanted("memory") {
        memory(&c, &mut r).await;
    }
    if wanted("secrets") {
        secrets(&c, &mut r).await;
    }
    if wanted("network") {
        network(&c, &mut r).await;
    }
    if egress_ready && wanted("egress") {
        egress(&c, &mut r).await;
    } else if !full() {
        r.note(
            "egress",
            "the egress proxy cases run with RUUM_SANDBOX_FULL=1 (R-2e's image)",
        );
    }
    r.print(&format!("hostile sandbox suite [{}]", c.driver.name()));
    let failed = r.failed();
    assert!(
        failed.is_empty(),
        "{} checks failed: {:?}",
        failed.len(),
        failed.iter().map(|f| &f.0).collect::<Vec<_>>()
    );
}

/// No privilege: user 10001, no capabilities, no new privileges, read-only root, the
/// size-limited /tmp, no Docker socket, `mount` and `unshare -U` fail.
async fn privilege(c: &Ctx, r: &mut Report) {
    let id = identity(TenantId::new(), NodeId::new());
    let cfg = json!({ "self": { "tmp_limit": 64 * 1024 * 1024 } });
    let (st, res, _) = c.probe(id, cfg, Vec::new()).await;
    let s = res.get("self").cloned().unwrap_or(Value::Null);
    r.check(
        "privilege: the probe ran",
        st == InstanceState::Exited(ExitStatus::completed(0)) && s.is_object(),
        format!("{st:?}"),
    );
    let g = |k: &str| s.get(k).cloned().unwrap_or(Value::Null);
    r.check(
        "privilege: uid and gid are 10001",
        g("uid") == json!(10001) && g("gid") == json!(10001),
        format!("{} {}", g("uid"), g("gid")),
    );
    r.check(
        "privilege: CapEff and CapBnd are 0",
        g("cap_eff") == json!("0000000000000000") && g("cap_bnd") == json!("0000000000000000"),
        format!("{} {}", g("cap_eff"), g("cap_bnd")),
    );
    r.check(
        "privilege: NoNewPrivs is 1",
        g("no_new_privs") == json!("1"),
        g("no_new_privs").to_string(),
    );
    r.check(
        "privilege: the root is mounted read-only",
        g("root_mount")
            .as_str()
            .is_some_and(|m| m.split(',').any(|o| o == "ro")),
        g("root_mount").to_string(),
    );
    r.check(
        "privilege: a write to a folder the user owns on the root fails with EROFS",
        g("root_write") == json!("EROFS"),
        g("root_write").to_string(),
    );
    r.check(
        "privilege: a write to /etc fails",
        g("etc_write") != json!("written"),
        g("etc_write").to_string(),
    );
    let tmp = g("tmp");
    let written = tmp
        .get("written")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    r.check(
        "privilege: /tmp is the size-limited tmpfs (a write past the cap gets ENOSPC)",
        tmp.get("error") == Some(&json!("ENOSPC")) && written <= limits().disk_bytes,
        tmp.to_string(),
    );
    r.check(
        "privilege: no Docker socket in the sandbox",
        g("docker_socket") == json!([]),
        g("docker_socket").to_string(),
    );
    let mount = g("mount");
    r.check(
        "privilege: mount fails",
        mount
            .get("status")
            .and_then(Value::as_i64)
            .is_some_and(|x| x != 0)
            || mount.get("error").is_some_and(|e| !e.is_null()),
        mount.to_string(),
    );
    let unshare = g("unshare");
    let unshare_failed = unshare
        .get("status")
        .and_then(Value::as_i64)
        .is_some_and(|x| x != 0);
    match c.runtime {
        RuntimeKind::Runc => {
            r.check(
                "privilege: seccomp filter on (mode 2)",
                g("seccomp") == json!("2"),
                g("seccomp").to_string(),
            );
            r.check(
                "privilege: unshare -U fails",
                unshare_failed,
                unshare.to_string(),
            );
        }
        RuntimeKind::Runsc => {
            // gVisor runs the app on its own kernel (the Sentry); without runsc's --oci-seccomp
            // flag Docker's seccomp profile is not applied inside, so unshare(CLONE_NEWUSER)
            // succeeds there, inside gVisor, with no host user namespace. Recorded, not hidden:
            // the check requires gVisor itself, and the finding is in the impl notes.
            r.check(
                "privilege: runsc runs the app on gVisor's kernel",
                g("gvisor") == json!(true),
                g("gvisor").to_string(),
            );
            if unshare_failed {
                r.check("privilege: unshare -U fails", true, unshare.to_string());
            } else {
                r.note("FINDING runsc: unshare -U succeeds inside gVisor (no --oci-seccomp; Seccomp field)", g("seccomp").to_string());
            }
        }
    }
    let keys: Vec<String> = g("env_keys")
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    r.check(
        "privilege: the environment has the token file variable and no platform setting",
        keys.iter().any(|k| k == "RUUM_TOKEN_FILE")
            && !keys
                .iter()
                .any(|k| k == "DATABASE_URL" || (k.starts_with("RUUM_") && k != "RUUM_TOKEN_FILE")),
        keys.join(" "),
    );
}

/// A fork bomb stops at the pids limit: the engine never counts more processes than the limit,
/// and the bomb does not get all the processes it asks for.
async fn forkbomb(c: &Ctx, r: &mut Report) {
    let limit = c.h.config().pids_limit;
    let id = identity(TenantId::new(), NodeId::new());
    let mut spec = c.job(
        id,
        vec![format!(
            "probe:{}",
            json!({"forkbomb": {"n": limit * 2, "ms": 4000}})
        )],
        Vec::new(),
    );
    spec.kind = WorkloadKind::Job(JobSpec {
        timeout: Duration::from_secs(30),
    });
    let s = c.driver.start(spec).await.expect("fork bomb starts");
    let container = c.driver.container_of(s.id).unwrap_or_default();
    let mut max_pids = 0i64;
    let deadline = Instant::now() + Duration::from_secs(40);
    while Instant::now() < deadline {
        if let Ok(p) = docker(&["stats", "--no-stream", "--format", "{{.PIDs}}", &container])
            && let Ok(n) = p.trim().parse::<i64>()
        {
            max_pids = max_pids.max(n);
        }
        let st = c
            .driver
            .status(id.tenant, s.id)
            .await
            .expect("status")
            .state;
        if matches!(st, InstanceState::Exited(_)) {
            break;
        }
    }
    let st = c
        .wait_state(
            id.tenant,
            s.id,
            |s| matches!(s, InstanceState::Exited(_)),
            40,
        )
        .await;
    let logs = c.logs(id.tenant, s.id).await;
    let res = results(&logs);
    let fb = res.get("forkbomb").cloned().unwrap_or(Value::Null);
    let alive = fb.get("alive").and_then(Value::as_i64);
    let stopped = match alive {
        Some(a) => a < limit * 2,
        // A bomb that never reported ended by its timeout: it did not get its processes either.
        None => st == InstanceState::Exited(ExitStatus::of(ExitReason::TimedOut)),
    };
    r.check(
        "forkbomb: the bomb does not get the processes it asks for",
        stopped,
        format!("{fb} {st:?}"),
    );
    r.check(
        "forkbomb: the engine never counted more processes than the limit",
        max_pids > 0 && max_pids <= limit,
        format!("max {max_pids} of {limit}"),
    );
}

/// A memory hog is killed, and the exit says out of memory.
async fn memory(c: &Ctx, r: &mut Report) {
    let id = identity(TenantId::new(), NodeId::new());
    let mut spec = c.job(
        id,
        vec![
            "alloc:67108864".into(),
            "alloc:67108864".into(),
            "alloc:67108864".into(),
            "sleep:500".into(),
        ],
        Vec::new(),
    );
    spec.limits.memory_bytes = 96 * 1024 * 1024;
    let s = c.driver.start(spec).await.expect("memory hog starts");
    let st = c
        .wait_state(
            id.tenant,
            s.id,
            |s| matches!(s, InstanceState::Exited(_)),
            60,
        )
        .await;
    r.check(
        "memory: a memory hog ends as out of memory",
        st == InstanceState::Exited(ExitStatus::of(ExitReason::OutOfMemory)),
        format!("{st:?}"),
    );
}

/// Secrets in logs: an app that prints its whole environment and its token file shows the
/// marker for each secret, the previous value and the token, also split across two writes and
/// after a rotation (the old token kept by the app). The hex and base64 forms are NOT redacted:
/// exact match only, the known limit of rule 7 (the telemetry pipeline, O-1a, looks for them).
async fn secrets(c: &Ctx, r: &mut Report) {
    let id = identity(TenantId::new(), NodeId::new());
    let secret = format!(
        "sec{}{}",
        uuid::Uuid::now_v7().simple(),
        uuid::Uuid::now_v7().simple()
    );
    let previous = format!("prev{}", uuid::Uuid::now_v7().simple());
    let (a, b) = secret.split_at(secret.len() / 2);
    let mut spec = c.service(
        id,
        vec![
            "dump-env".into(),
            "token".into(),
            "token-keep".into(),
            format!("write:split={a}"),
            "sleep:200".into(),
            format!("print:{b};"),
            format!("print:old={previous}"),
            "encoded:API_SECRET".into(),
            "print:before-rotation".into(),
            "sleep:3000".into(),
            "token".into(),
            "token-kept".into(),
            "print:after-rotation".into(),
            "serve".into(),
        ],
        Vec::new(),
    );
    spec.env = vec![
        EnvVar::secret("API_SECRET", SecretValue::new(secret.clone())),
        EnvVar::plain("MODE", "visible"),
    ];
    spec.previous_secrets = vec![SecretValue::new(previous.clone())];
    let old = spec.workload_token.expose().to_owned();
    let s = c.driver.start(spec).await.expect("secrets app starts");
    let deadline = Instant::now() + Duration::from_secs(60);
    while !c
        .logs(id.tenant, s.id)
        .await
        .iter()
        .any(|l| l.text == "before-rotation")
        && Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let new = token();
    let new_text = new.expose().to_owned();
    c.driver
        .rotate_token(id.tenant, s.id, new)
        .await
        .expect("rotate");
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut logs = Vec::new();
    while Instant::now() < deadline {
        logs = c.logs(id.tenant, s.id).await;
        if logs.iter().any(|l| l.text == "after-rotation") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let _ = c.driver.stop(id.tenant, s.id).await;
    let texts: Vec<&str> = logs.iter().map(|l| l.text.as_str()).collect();
    r.check(
        "secrets: the app ran past the rotation",
        texts.contains(&"after-rotation"),
        format!("{} lines", texts.len()),
    );
    for (what, v) in [
        ("the secret", &secret),
        ("the previous value", &previous),
        ("the old token", &old),
        ("the new token", &new_text),
    ] {
        r.check(
            format!("secrets: no line holds {what}"),
            !texts.iter().any(|t| t.contains(v.as_str())),
            "",
        );
    }
    let marker_key = format!("API_SECRET={REDACTED}");
    r.check(
        "secrets: the environment dump shows the marker for the secret",
        texts.contains(&marker_key.as_str()),
        "",
    );
    r.check(
        "secrets: a plain value stays visible",
        texts.contains(&"MODE=visible"),
        "",
    );
    let split = format!("split={REDACTED};");
    r.check(
        "secrets: a value split across two writes is redacted",
        texts.contains(&split.as_str()),
        "",
    );
    let old_line = format!("old={REDACTED}");
    r.check(
        "secrets: a previous value is redacted",
        texts.contains(&old_line.as_str()),
        "",
    );
    let kept = format!("kept={REDACTED}");
    r.check(
        "secrets: the old token printed after the rotation is redacted",
        texts.contains(&kept.as_str()),
        "",
    );
    r.check(
        "secrets: both tokens print as the marker",
        texts.iter().filter(|t| **t == REDACTED).count() >= 2,
        "",
    );
    let hex = format!("hex:{}", hex::encode(secret.as_bytes()));
    use base64::Engine as _;
    let b64 = format!(
        "b64:{}",
        base64::engine::general_purpose::STANDARD.encode(secret.as_bytes())
    );
    r.check(
        "secrets: the hex and base64 forms are NOT claimed as redacted (exact match only, rule 7's known limit)",
        texts.contains(&hex.as_str()) && texts.contains(&b64.as_str()),
        "",
    );
}

/// The network targets (rule 2): nothing outside the app network is reached, while a positive
/// control reaches each target at the same time.
async fn network(c: &Ctx, r: &mut Report) {
    // Host services on every address: a TCP port, the Docker API ports 2375 and 2376 (bound by the
    // test when free; when taken, whatever listens there is the target), and a UDP echo.
    let tcp_port = listen_tcp(0).expect("a host TCP listener");
    let docker_ports: Vec<u16> = [2375u16, 2376]
        .into_iter()
        .filter(|p| listen_tcp(*p).is_some() || host_tcp("127.0.0.1", *p) == "connected")
        .collect();
    let udp_port = listen_udp_echo();

    let tenant = TenantId::new();
    let app_a = NodeId::new();
    let routes: Vec<Route> = vec![Route::EgressProxy, Route::InternalGateway, Route::Pooler];
    // App A's network, held by a running instance while the probe runs.
    let holder = c
        .start_running(c.service(
            identity(tenant, app_a),
            vec!["serve".into()],
            routes.clone(),
        ))
        .await;
    let a_net = ruum_runtime::network_name(&identity(tenant, app_a));
    // Another app of the tenant and an app of another tenant, each with a TCP and a UDP echo.
    let app_b = NodeId::new();
    let b = c
        .start_running(c.service(
            identity(tenant, app_b),
            vec!["udp-echo:9999".into(), "serve".into()],
            Vec::new(),
        ))
        .await;
    let other_tenant = TenantId::new();
    let app_c = NodeId::new();
    let cc = c
        .start_running(c.service(
            identity(other_tenant, app_c),
            vec!["udp-echo:9999".into(), "serve".into()],
            Vec::new(),
        ))
        .await;
    let b_ip = c.ip_of(b);
    let c_ip = c.ip_of(cc);

    let mut targets: Vec<Target> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut add =
        |name: &str, host: Option<String>, port: u16, udp: bool, control: Control| match host {
            Some(h) if !h.is_empty() => targets.push(Target {
                name: name.to_owned(),
                host: h,
                port,
                udp,
                control,
            }),
            _ => missing.push(name.to_owned()),
        };
    let host = |a: &Option<String>| Control::Host(a.clone().unwrap_or_default());

    // Platform databases and the dev stack.
    let pg = url_port("DATABASE_URL")
        .and_then(|p| container_publishing(&p))
        .and_then(|n| container_ip(&n, None));
    add(
        "platform Postgres (DATABASE_URL)",
        pg.clone(),
        5432,
        false,
        host(&pg),
    );
    let data = url_port("RUUM_DBHOST_URL")
        .and_then(|p| container_publishing(&p))
        .and_then(|n| container_ip(&n, None));
    if data.is_some() || full() {
        add(
            "data cluster Postgres (RUUM_DBHOST_URL)",
            data.clone(),
            5432,
            false,
            host(&data),
        );
    }
    if full() {
        let spicedb = url_port("SPICEDB_ENDPOINT")
            .and_then(|p| container_publishing(&p))
            .and_then(|n| container_ip(&n, None));
        add(
            "SpiceDB gRPC (SPICEDB_ENDPOINT)",
            spicedb.clone(),
            50051,
            false,
            host(&spicedb),
        );
        for (name, cont, port) in [
            (
                "stack Postgres (ruum-postgres-1)",
                "ruum-postgres-1",
                5432u16,
            ),
            (
                "SpiceDB of the stack (ruum-spicedb-1)",
                "ruum-spicedb-1",
                50051,
            ),
            ("OpenBao (ruum-openbao-1)", "ruum-openbao-1", 8200),
            ("NATS (ruum-nats-1)", "ruum-nats-1", 4222),
            ("Garage S3 (ruum-garage-1)", "ruum-garage-1", 3900),
        ] {
            let ip = container_ip(cont, Some("ruum_default"));
            add(name, ip.clone(), port, false, host(&ip));
        }
    }
    // The metadata address.
    add(
        "metadata address TCP 80",
        Some("169.254.169.254".into()),
        80,
        false,
        Control::Host("169.254.169.254".into()),
    );
    // The host through the app network's bridge: with inhibit_ipv4 the host holds no address
    // there; the would-be gateway is the subnet's first address. Its control is a plain internal
    // network, where the host does hold the gateway and the same port answers.
    let subnet = docker(&[
        "network",
        "inspect",
        "-f",
        "{{range .IPAM.Config}}{{.Subnet}}{{end}}",
        &a_net,
    ])
    .unwrap_or_default();
    let would_be = first_host(&subnet);
    for p in std::iter::once(tcp_port).chain(docker_ports.iter().copied()) {
        add(
            &format!("host through the app bridge's would-be gateway, TCP {p}"),
            would_be.clone(),
            p,
            false,
            Control::Plain(format!("tcp {p}")),
        );
    }
    add(
        &format!("host through the app bridge's would-be gateway, UDP {udp_port}"),
        would_be.clone(),
        udp_port,
        true,
        Control::Plain(format!("udp {udp_port}")),
    );
    // The host's own addresses.
    let docker0 = docker(&[
        "network",
        "inspect",
        "bridge",
        "-f",
        "{{range .IPAM.Config}}{{.Gateway}}{{end}}",
    ])
    .ok()
    .filter(|g| g.parse::<IpAddr>().is_ok());
    add(
        "host through docker0, TCP",
        docker0.clone(),
        tcp_port,
        false,
        host(&docker0),
    );
    add(
        "host through docker0, UDP",
        docker0.clone(),
        udp_port,
        true,
        host(&docker0),
    );
    let v4 = host_addr(false).map(|a| a.to_string());
    add(
        "host public IPv4, TCP",
        v4.clone(),
        tcp_port,
        false,
        host(&v4),
    );
    add(
        "host public IPv4, UDP",
        v4.clone(),
        udp_port,
        true,
        host(&v4),
    );
    let v6 = host_addr(true).map(|a| a.to_string());
    if v6.is_some() || full() {
        add(
            "host public IPv6, TCP",
            v6.clone(),
            tcp_port,
            false,
            host(&v6),
        );
        add(
            "host public IPv6, UDP",
            v6.clone(),
            udp_port,
            true,
            host(&v6),
        );
    } else {
        r.note(
            "host public IPv6",
            "this host has no global IPv6 address (required only with RUUM_SANDBOX_FULL=1)",
        );
    }
    // The bridge's IPv6 link-local address, if it has one.
    let net_id = docker(&["network", "inspect", "-f", "{{.Id}}", &a_net]).unwrap_or_default();
    let bridge = format!("br-{}", &net_id.get(..12).unwrap_or(""));
    let ll = std::process::Command::new("ip")
        .args(["-6", "-o", "addr", "show", "dev", &bridge, "scope", "link"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| {
            s.split_whitespace()
                .nth(3)
                .map(|a| a.split('/').next().unwrap_or("").to_owned())
        })
        .filter(|a| !a.is_empty());
    match &ll {
        Some(a) => add(
            "host through the app bridge's IPv6 link-local address, TCP",
            Some(format!("{a}%eth0")),
            tcp_port,
            false,
            Control::Host(format!("{a}%{bridge}")),
        ),
        None => r.check(
            format!("the app bridge {bridge} has no IPv6 link-local address"),
            !net_id.is_empty(),
            "",
        ),
    }
    // Other apps.
    add(
        "another app of the tenant, TCP 8080",
        Some(b_ip.clone()),
        8080,
        false,
        Control::Net("b tcp".into()),
    );
    add(
        "another app of the tenant, UDP 9999",
        Some(b_ip.clone()),
        9999,
        true,
        Control::Net("b udp".into()),
    );
    add(
        "an app of another tenant, TCP 8080",
        Some(c_ip.clone()),
        8080,
        false,
        Control::Net("c tcp".into()),
    );
    add(
        "an app of another tenant, UDP 9999",
        Some(c_ip.clone()),
        9999,
        true,
        Control::Net("c udp".into()),
    );
    // The internet, directly.
    add(
        "internet direct 1.1.1.1:443",
        Some("1.1.1.1".into()),
        443,
        false,
        Control::Host("1.1.1.1".into()),
    );
    add(
        "internet direct 8.8.8.8:53",
        Some("8.8.8.8".into()),
        53,
        false,
        Control::Host("8.8.8.8".into()),
    );
    for m in &missing {
        r.check(
            format!("network: the target {m} has an address"),
            false,
            "none found",
        );
    }
    let dns = ["registry.npmjs.org", "example.com"];

    let tcp_list: Vec<Value> = targets
        .iter()
        .filter(|t| !t.udp)
        .map(|t| json!({"name": t.name, "host": t.host, "port": t.port}))
        .collect();
    let udp_list: Vec<Value> = targets
        .iter()
        .filter(|t| t.udp)
        .map(|t| json!({"name": t.name, "host": t.host, "port": t.port}))
        .collect();
    let cfg = json!({ "tcp": tcp_list, "udp": udp_list, "metadata": true, "dns": dns, "self": {"tmp_limit": 1024} });
    let (st, res, _) = c.probe(identity(tenant, app_a), cfg, routes.clone()).await;
    r.check(
        "network: the probe ran",
        matches!(st, InstanceState::Exited(e) if e.success()),
        format!("{st:?}"),
    );

    // The controls, at the same time.
    let mut controls: BTreeMap<String, String> = BTreeMap::new();
    // The plain internal network (no inhibit_ipv4): the host holds its gateway.
    let plain = format!(
        "r2a-plain-{}",
        &uuid::Uuid::now_v7().simple().to_string()[24..]
    );
    let _ = docker(&["network", "create", "--internal", "--label", LABEL, &plain]);
    let gw = docker(&[
        "network",
        "inspect",
        "-f",
        "{{range .IPAM.Config}}{{.Gateway}}{{end}}",
        &plain,
    ])
    .unwrap_or_default();
    let mut ptcp: Vec<Value> = std::iter::once(tcp_port)
        .chain(docker_ports.iter().copied())
        .map(|p| json!({"name": format!("tcp {p}"), "host": gw, "port": p}))
        .collect();
    ptcp.dedup();
    let pcfg = json!({"tcp": ptcp, "udp": [{"name": format!("udp {udp_port}"), "host": gw, "port": udp_port}]});
    let mut runtime_args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--label".into(),
        LABEL.into(),
        "--network".into(),
        plain.clone(),
        "--user".into(),
        "10001:10001".into(),
        "--cap-drop".into(),
        "ALL".into(),
        "--read-only".into(),
    ];
    if c.runtime == RuntimeKind::Runsc {
        runtime_args.extend(["--runtime".into(), "runsc".into()]);
    }
    runtime_args.push(c.h.fixture_image.clone());
    runtime_args.push(format!("probe:{pcfg}"));
    let args: Vec<&str> = runtime_args.iter().map(String::as_str).collect();
    let plain_out = docker(&args).unwrap_or_default();
    let plain_res: BTreeMap<String, Value> = plain_out
        .lines()
        .filter_map(|l| l.strip_prefix("probe "))
        .filter_map(|j| serde_json::from_str::<Value>(j).ok())
        .filter_map(|v| Some((v.get("name")?.as_str()?.to_owned(), v)))
        .collect();
    // Control probes on the other apps' own networks (an instance of the same app joins it).
    let (_, bres, _) = c.probe(identity(tenant, app_b), json!({"tcp": [{"name": "b tcp", "host": b_ip, "port": 8080}], "udp": [{"name": "b udp", "host": b_ip, "port": 9999}]}), Vec::new()).await;
    let (_, cres, _) = c.probe(identity(other_tenant, app_c), json!({"tcp": [{"name": "c tcp", "host": c_ip, "port": 8080}], "udp": [{"name": "c udp", "host": c_ip, "port": 9999}]}), Vec::new()).await;
    for t in &targets {
        let ctl = match &t.control {
            Control::Host(a) if t.udp => host_udp(a, t.port),
            Control::Host(a) => host_tcp(a, t.port),
            Control::Plain(k) => {
                if reached(plain_res.get(k)) {
                    "connected".into()
                } else {
                    format!("plain probe: {}", detail(plain_res.get(k)))
                }
            }
            Control::Net(k) => {
                let v = bres.get(k).or_else(|| cres.get(k));
                if reached(v) {
                    "connected".into()
                } else {
                    format!("own-network probe: {}", detail(v))
                }
            }
        };
        controls.insert(t.name.clone(), ctl);
    }
    let meta_ctl = host_http_get("169.254.169.254", 80);
    let dns_ctl: Vec<(String, bool)> = dns
        .iter()
        .map(|d| {
            (
                d.to_string(),
                std::net::ToSocketAddrs::to_socket_addrs(&format!("{d}:443"))
                    .is_ok_and(|mut a| a.next().is_some()),
            )
        })
        .collect();
    let _ = docker(&["network", "rm", &plain]);

    // Exactly the expected set.
    let mut want: BTreeSet<String> = targets.iter().map(|t| t.name.clone()).collect();
    want.insert("metadata HTTP GET".into());
    want.insert("self".into());
    for d in dns {
        want.insert(format!("DNS {d}"));
    }
    let got: BTreeSet<String> = res.keys().cloned().collect();
    r.check(
        format!(
            "network: the probe answered for exactly the {} expected targets",
            want.len()
        ),
        got == want,
        format!(
            "missing {:?}, unexpected {:?}",
            want.difference(&got).collect::<Vec<_>>(),
            got.difference(&want).collect::<Vec<_>>()
        ),
    );
    for t in &targets {
        let ctl = controls.get(&t.name).cloned().unwrap_or_default();
        let ctl_ok = ctl == "connected" || ctl == "echo";
        let v = res.get(&t.name);
        if ctl_ok {
            r.check(
                format!("network: {} ({}:{}) is not reached", t.name, t.host, t.port),
                v.is_some() && !reached(v),
                format!("{}; control {ctl}", detail(v)),
            );
        } else {
            r.check(format!("network: {} — the positive control does not reach it, so the probe proves nothing", t.name), false, format!("{}; control {ctl}", detail(v)));
        }
    }
    let mv = res.get("metadata HTTP GET");
    r.check(
        "network: metadata HTTP GET is not answered",
        meta_ctl && mv.is_some() && !reached(mv),
        format!(
            "{}; control {}",
            detail(mv),
            if meta_ctl { "answered" } else { "FAILED" }
        ),
    );
    for (d, ok) in dns_ctl {
        let v = res.get(&format!("DNS {d}"));
        r.check(
            format!("network: DNS {d} does not resolve inside (an internal network)"),
            ok && v.is_some() && !reached(v),
            format!(
                "{}; control {}",
                detail(v),
                if ok { "resolves" } else { "FAILED" }
            ),
        );
    }
    let s = res.get("self").cloned().unwrap_or(Value::Null);
    if c.runtime == RuntimeKind::Runc {
        r.check(
            "network: IPv6 is off on the sandbox's interface",
            s.get("disable_ipv6") == Some(&json!("1")),
            s.get("disable_ipv6")
                .map(Value::to_string)
                .unwrap_or_default(),
        );
    } else {
        r.note(
            "IPv6 setting inside gVisor",
            s.get("disable_ipv6")
                .map(Value::to_string)
                .unwrap_or_default(),
        );
    }
    r.note(
        "targets run",
        targets
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>()
            .join("; "),
    );
    for id in [holder, b] {
        let _ = c.driver.stop(tenant, id).await;
    }
    let _ = c.driver.stop(other_tenant, cc).await;
}

fn first_host(subnet: &str) -> Option<String> {
    let (base, _) = subnet.split_once('/')?;
    let ip: std::net::Ipv4Addr = base.parse().ok()?;
    Some(std::net::Ipv4Addr::from(u32::from(ip) + 1).to_string())
}

fn host_http_get(addr: &str, port: u16) -> bool {
    use std::io::{Read, Write};
    let Ok(sock) = format!("{addr}:{port}").parse::<SocketAddr>() else {
        return false;
    };
    let Ok(mut s) = TcpStream::connect_timeout(&sock, Duration::from_secs(3)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
    if s.write_all(b"GET / HTTP/1.0\r\nHost: 169.254.169.254\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut buf = [0u8; 16];
    matches!(s.read(&mut buf), Ok(n) if n >= 5 && &buf[..5] == b"HTTP/")
}

/// `EGRESS_DENY` of the test proxy: the host's public IPv4 address and its IPv6 /64.
fn egress_deny() -> String {
    let mut deny = Vec::new();
    if let Some(v4) = host_addr(false) {
        deny.push(v4.to_string());
    }
    if let Some(IpAddr::V6(v6)) = host_addr(true) {
        let s = v6.segments();
        deny.push(format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3]));
    }
    deny.join(",")
}

/// The internet only through the egress proxy (R-2e): an allowed host is reached through it
/// (also with Node's own `fetch` and `NODE_USE_ENV_PROXY`, which the driver sets); private,
/// link-local, metadata and host addresses, also in their encoded forms, get the proxy's own
/// refusal (403, `deny=address`), never a 502 or a timeout. The proxy container is the only
/// hop between app networks: it listens on its service port only and forwards nothing.
async fn egress(c: &Ctx, r: &mut Report) {
    let Some(e) = &c.h.egress else { return };
    let tenant = TenantId::new();
    let app_a = NodeId::new();
    let app_b = NodeId::new();
    let routes = vec![Route::EgressProxy];
    let b = c
        .start_running(c.service(
            identity(tenant, app_b),
            vec!["serve".into()],
            routes.clone(),
        ))
        .await;
    let b_ip = c.ip_of(b);
    let b_net = ruum_runtime::network_name(&identity(tenant, app_b));
    let proxy_on_b = container_ip(&e.container, Some(&b_net)).unwrap_or_default();
    let mut refused: Vec<String> = [
        "169.254.169.254:443",
        "2852039166:443",
        "0251.0376.0251.0376:443",
        "0xa9fea9fe:443",
        "127.1:443",
        "[::ffff:169.254.169.254]:443",
        "[::ffff:a9fe:a9fe]:443",
        "[::1]:443",
        "[fd00:ec2::254]:443",
        "10.0.0.1:443",
        "172.17.0.1:443",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    refused.push(format!("{b_ip}:443"));
    if let Some(v4) = host_addr(false) {
        refused.push(format!("{v4}:443"));
        refused.push(format!("[::ffff:{v4}]:443"));
        refused.push(format!("{v4}.nip.io:443"));
    }
    if let Some(IpAddr::V6(v6)) = host_addr(true) {
        refused.push(format!("[{v6}]:443"));
        let mut s = v6.segments();
        s[7] ^= 0xbeef;
        refused.push(format!("[{}]:443", std::net::Ipv6Addr::from(s)));
    }
    let mut proxy: Vec<String> = vec!["registry.npmjs.org:443".into()];
    proxy.extend(refused.iter().cloned());
    let cfg = json!({
        "proxy": proxy,
        "fetch": ["https://registry.npmjs.org/"],
        "tcp": [
            {"name": "app B directly", "host": b_ip, "port": 8080},
            {"name": "app B through the proxy container's address on B's network", "host": proxy_on_b, "port": 8080},
        ],
    });
    let (st, res, _) = c.probe(identity(tenant, app_a), cfg, routes.clone()).await;
    r.check(
        "egress: the probe ran",
        matches!(st, InstanceState::Exited(x) if x.success()),
        format!("{st:?}"),
    );
    let npm = res.get("proxy registry.npmjs.org:443");
    r.check(
        "egress: an allowed public host is reached through the proxy (200)",
        npm.and_then(|v| v.get("status")).and_then(Value::as_i64) == Some(200),
        detail(npm),
    );
    for t in &refused {
        let v = res.get(&format!("proxy {t}"));
        let ok = v.and_then(|v| v.get("status")).and_then(Value::as_i64) == Some(403)
            && v.and_then(|v| v.get("deny")).and_then(Value::as_str) == Some("address");
        r.check(
            format!("egress: {t} gets the proxy's refusal (403, deny=address)"),
            ok,
            detail(v),
        );
    }
    let f = res.get("fetch https://registry.npmjs.org/");
    r.check(
        "egress: Node's own fetch goes through the proxy (NODE_USE_ENV_PROXY=1, HTTPS_PROXY)",
        f.and_then(|v| v.get("status")).and_then(Value::as_i64) == Some(200),
        detail(f),
    );
    // Positive control for app B: an instance on B's own network reaches it.
    let (_, bres, _) = c
        .probe(
            identity(tenant, app_b),
            json!({"tcp": [{"name": "b", "host": b_ip, "port": 8080}]}),
            routes.clone(),
        )
        .await;
    let ctl = reached(bres.get("b"));
    for k in [
        "app B directly",
        "app B through the proxy container's address on B's network",
    ] {
        let v = res.get(k);
        r.check(
            format!("egress: {k} is not reached"),
            ctl && v.is_some() && !reached(v),
            format!(
                "{}; control {}",
                detail(v),
                if ctl { "reached" } else { "FAILED" }
            ),
        );
    }
    // The sockets and forwarding of the proxy's network namespace.
    let ns = format!("container:{}", e.container);
    let out = docker(&[
        "run",
        "--rm",
        "--label",
        LABEL,
        "--network",
        &ns,
        "--user",
        "10001:10001",
        "--read-only",
        "--cap-drop",
        "ALL",
        &c.h.fixture_image,
        &format!("probe:{}", json!({"listen": true})),
    ])
    .unwrap_or_default();
    let listen = out
        .lines()
        .filter_map(|l| l.strip_prefix("probe "))
        .filter_map(|j| serde_json::from_str::<Value>(j).ok())
        .find(|v| v.get("name") == Some(&json!("listen")))
        .unwrap_or(Value::Null);
    let socks = listen
        .get("sockets")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let only_service = !socks.is_empty()
        && socks.iter().all(|s| {
            s.get("proto")
                .and_then(Value::as_str)
                .is_some_and(|p| p.starts_with("tcp"))
                && s.get("port") == Some(&json!(3128))
        });
    r.check(
        "egress: the proxy container listens only on its service port (TCP 3128), no UDP",
        only_service,
        Value::Array(socks).to_string(),
    );
    let fwd = listen.get("forwarding").cloned().unwrap_or(Value::Null);
    r.check(
        "egress: the proxy container forwards no packets (ip_forward 0)",
        fwd.get("ipv4/ip_forward") == Some(&json!("0")),
        fwd.to_string(),
    );
    // The proxy logs to standard error and standard output; read both.
    let all = Command::new("docker")
        .args(["logs", &e.container])
        .env("DOCKER_HOST", common::docker_host())
        .output()
        .map(|o| {
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
        })
        .unwrap_or_default();
    r.check(
        "egress: the proxy logged the tunnel to registry.npmjs.org",
        all.contains("registry.npmjs.org"),
        "",
    );
    r.check(
        "egress: the proxy log holds no path",
        !all.contains("GET /") && !all.contains("HEAD /"),
        "",
    );
    let _ = c.driver.stop(tenant, b).await;
}
