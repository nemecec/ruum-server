//! The sandbox supervisor's protections inside a container, as PID 1 (backlog I620-3; design
//! `docs/STEP3.md` "The sandbox supervisor" 2): the static supervisor is the container's first
//! process (no Docker init), restricted as a sandbox (a user of its own, read-only root, no
//! capabilities, no privilege escalation), and its app tries each way around it:
//!
//! - its output reaches the container runtime's log redacted (the CRI log canary: `docker logs`
//!   reads the engine's log file, as the kubelet's `/var/log/pods` files hold the pod's);
//! - it cannot open `/proc/1/fd/1` (the supervisor's raw output), `/proc/1/mem` or
//!   `/proc/1/environ` (the supervisor is non-dumpable), while it can open its own
//!   `/proc/self/mem` (the positive control);
//! - `RLIMIT_NPROC` of the launch bounds its processes (a fork loop) and its threads (Node.js
//!   workers: every attempt settles, at least one starts and at least one is refused);
//! - last, `kill -STOP 1` and `kill -9 1`: on Linux (runc) PID 1 of a namespace ignores both from
//!   its own namespace, and the run ends normally. gVisor (runsc release-20261109.0) delivers
//!   both: SIGSTOP freezes the supervisor (its output stops, nothing raw passes) and SIGKILL ends
//!   the whole sandbox, the app with it (backlog I620-15: a hostile app can end or freeze only
//!   its own run). Either way no raw value reaches the log and no app outlives its supervisor.
//!
//! `RUUM_RUNTIME_DRIVER=runsc` runs it under gVisor (the build server: the proof of the
//! protections under runsc that the Kubernetes driver needs, decision 0008-5), the default with
//! runc (CI). Needs a Docker engine and the musl supervisor (`tests/common` `supervisor_bin`).

mod common;

use common::{LABEL, docker, engine_or_skip, runtime_kind, supervisor_bin};
use ruum_runtime::RuntimeKind;
use ruum_supervisor::protocol::{LAUNCH_VERSION, Launch};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The process limit the launch sets (the Kubernetes driver's default is 512).
const PIDS: u64 = 64;
/// A user no other container of the engine uses: under runc `RLIMIT_NPROC` counts every process
/// of the user on the host.
const UID: &str = "10077:10077";

const SCRIPT: &str = r#"
echo "canary-out-$S"
echo "canary-err-$S" >&2
echo "raw-$S" > /proc/1/fd/1 2>/dev/null && echo fd1-open || echo fd1-refused
( : < /proc/1/mem ) 2>/dev/null && echo mem-open || echo mem-refused
( : < /proc/self/mem ) 2>/dev/null && echo mem-control-open || echo mem-control-refused
cat /proc/1/environ >/dev/null 2>&1 && echo environ-read || echo environ-refused
node -e '
const { Worker } = require("worker_threads");
let online = 0, failed = 0;
for (let i = 0; i < 200; i++) {
  let settled = false;
  const settle = (ok) => { if (!settled) { settled = true; if (ok) online++; else failed++; } };
  try {
    const w = new Worker("setTimeout(() => {}, 60000)", { eval: true });
    w.on("online", () => settle(true));
    w.on("error", () => settle(false));
    w.on("exit", () => settle(false));
  } catch (e) { settle(false); }
}
const start = Date.now();
const wait = setInterval(() => {
  if (online + failed < 200 && Date.now() - start < 30000) return;
  clearInterval(wait);
  console.log("workers=" + online);
  console.log("worker-failures=" + failed);
  console.log("threads=" + require("fs").readdirSync("/proc/self/task").length);
  process.exit(0);
}, 100);
' 2>/dev/null || echo threads-failed
( n=0; while [ $n -lt 200 ]; do sleep 2 & n=$((n+1)); echo "started=$n"; done ) 2>/dev/null | tail -n 1
echo done
sleep 1
kill -STOP 1 2>/dev/null
echo "after-stop-$S"
kill -9 1 2>/dev/null
echo "after-kill-$S"
echo signals-ignored
"#;

#[test]
fn the_supervisor_holds_as_pid1_of_a_sandbox() {
    if !engine_or_skip("the_supervisor_holds_as_pid1_of_a_sandbox") {
        return;
    }
    let Some(bin) = supervisor_bin() else {
        eprintln!("not run: no musl supervisor");
        return;
    };
    let runtime = match runtime_kind() {
        RuntimeKind::Runsc => "runsc",
        RuntimeKind::Runc => "runc",
    };
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/fixture");
    let image = "ruum-r2a-fixture:test";
    docker(&["build", "-q", "--label", LABEL, "-t", image, dir]).expect("fixture image");
    let secret = format!("pid1-secret-{}", uuid::Uuid::now_v7().simple());
    let launch = serde_json::to_vec(&Launch {
        v: LAUNCH_VERSION,
        argv: vec!["/bin/sh".into(), "-c".into(), SCRIPT.into()],
        env: vec![("S".into(), secret.clone())],
        redact: Vec::new(),
        token: None,
        pids_limit: Some(PIDS),
        token_poll_secs: None,
    })
    .unwrap();
    let name = format!("ruum-sup-pid1-{}", uuid::Uuid::now_v7().simple());
    let mount = format!(
        "type=bind,src={},dst=/ruum/bin/ruum-supervisor,ro",
        bin.display()
    );
    let host = common::docker_host();
    let mut run = Command::new("docker")
        .env("DOCKER_HOST", &host)
        .args([
            "run",
            "-i",
            "--name",
            &name,
            "--label",
            LABEL,
            "--runtime",
            runtime,
            "--user",
            UID,
            "--read-only",
            "--tmpfs",
            "/tmp:rw,size=16m",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--network",
            "none",
            "--memory",
            "512m",
            "--log-driver",
            "json-file",
            "--mount",
            &mount,
            "--entrypoint",
            "/ruum/bin/ruum-supervisor",
            image,
            "run",
            "--launch-stdin",
            "--token-file",
            "/tmp/token",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("docker run");
    {
        let mut stdin = run.stdin.take().unwrap();
        stdin.write_all(&launch).unwrap();
    }
    // A supervisor that a signal stopped would never end: the test ends it.
    let deadline = Instant::now() + Duration::from_secs(240);
    let status = loop {
        if let Some(st) = run.try_wait().expect("docker run") {
            break Some(st);
        }
        if Instant::now() > deadline {
            let _ = docker(&["rm", "-f", &name]);
            let _ = run.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let mut run_err = String::new();
    if let Some(mut e) = run.stderr.take() {
        let _ = e.read_to_string(&mut run_err);
    }
    let code = status.and_then(|s| s.code());
    // The engine's log file of the container: what the container runtime wrote.
    let logs = Command::new("docker")
        .env("DOCKER_HOST", &host)
        .args(["logs", &name])
        .output()
        .expect("docker logs");
    let _ = docker(&["rm", "-f", &name]);
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&logs.stdout),
        String::from_utf8_lossy(&logs.stderr)
    );
    let ctx = format!("runtime {runtime}, exit {code:?}, docker stderr {run_err:?}, logs:\n{all}");
    assert!(
        !all.contains(&secret),
        "a raw value in the runtime's log: {ctx}"
    );
    assert!(
        all.contains("canary-out-<hidden secret>") && all.contains("canary-err-<hidden secret>"),
        "{ctx}"
    );
    // `/proc/1/mem` is only opened (the access check is at the open; a read at an unmapped
    // address fails even after an open that passed), with the app's own `/proc/self/mem` as the
    // positive control (Kimi, run 17, finding 6).
    for line in [
        "fd1-refused",
        "mem-refused",
        "mem-control-open",
        "environ-refused",
        "done",
    ] {
        assert!(all.lines().any(|l| l == line), "no line {line}: {ctx}");
    }
    // The signals to PID 1, last (backlog I620-15).
    let ignored = all.lines().any(|l| l == "signals-ignored");
    if ignored {
        // Linux semantics: the supervisor ignored both and went on redacting.
        assert_eq!(code, Some(0), "{ctx}");
        assert!(
            all.contains("after-stop-<hidden secret>")
                && all.contains("after-kill-<hidden secret>"),
            "{ctx}"
        );
    } else {
        // gVisor's: SIGKILL ended the sandbox (137), and nothing the app wrote after the
        // SIGSTOP reached the log.
        assert_eq!(
            code,
            Some(137),
            "the run did not end with its supervisor: {ctx}"
        );
        assert!(
            !all.contains("after-stop-") && !all.contains("after-kill-"),
            "{ctx}"
        );
    }
    if matches!(runtime_kind(), RuntimeKind::Runc) {
        assert!(
            ignored,
            "under runc PID 1 must ignore SIGKILL and SIGSTOP: {ctx}"
        );
    }
    eprintln!(
        "{runtime}: PID 1 {} SIGSTOP and SIGKILL from its app",
        if ignored { "ignores" } else { "takes" }
    );
    let num = |key: &str| -> Option<u64> {
        all.lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|v| v.trim().parse().ok())
    };
    // The supervisor and the shell count too: fewer sleeps than the limit, and far fewer than
    // the 200 the loop tried (the limit held, not the loop's end).
    let started = num("started=").expect("the fork count");
    assert!(
        started < PIDS,
        "{started} processes started under RLIMIT_NPROC {PIDS}: {ctx}"
    );
    eprintln!(
        "{runtime}: {started} processes started of 200 tried, workers {:?} (refused {:?}), threads {:?}, under RLIMIT_NPROC {PIDS}",
        num("workers="),
        num("worker-failures="),
        num("threads=")
    );
    // Node.js ran, every one of the 200 worker attempts settled (online or failed), at least
    // one worker started and at least one was refused (Kimi, run 17, finding 6 and its
    // re-check: a Node.js that does not start, or a count taken before the attempts settled,
    // proves nothing).
    let workers = num("workers=").expect("no worker count: Node.js did not run");
    let failures = num("worker-failures=").expect("no worker failure count");
    assert_eq!(
        workers + failures,
        200,
        "the worker attempts did not settle: {ctx}"
    );
    assert!(
        workers >= 1 && failures >= 1,
        "{workers} workers started and {failures} refused under RLIMIT_NPROC {PIDS}: {ctx}"
    );
    let threads = num("threads=").expect("no thread count");
    assert!(
        threads <= PIDS,
        "{threads} threads past RLIMIT_NPROC {PIDS}: {ctx}"
    );
}
