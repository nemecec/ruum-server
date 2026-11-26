//! The `ruum-supervisor` binary as a sandbox runs it (init mode: signals forwarded, every child
//! reaped, non-dumpable), with `/bin/sh` apps. Linux only (the build server and CI).

use ruum_supervisor::protocol::{LAUNCH_VERSION, Launch};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_ruum-supervisor");

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ruum-supervisor-bin-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn launch_json(script: &str, env: &[(&str, &str)]) -> Vec<u8> {
    launch_argv(&["/bin/sh", "-c", script], env)
}

fn launch_argv(argv: &[&str], env: &[(&str, &str)]) -> Vec<u8> {
    let l = Launch {
        v: LAUNCH_VERSION,
        argv: argv.iter().map(|a| (*a).into()).collect(),
        env: env
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect(),
        redact: Vec::new(),
        token: None,
        pids_limit: None,
        token_poll_secs: None,
    };
    serde_json::to_vec(&l).unwrap()
}

fn spawn(d: &Path, launch: &[u8]) -> std::process::Child {
    let mut c = Command::new(BIN)
        .args(["run", "--launch-stdin", "--token-file"])
        .arg(d.join("token"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The driver writes the launch data and closes standard input.
    let mut stdin = c.stdin.take().unwrap();
    stdin.write_all(launch).unwrap();
    drop(stdin);
    c
}

fn start(d: &Path, script: &str, env: &[(&str, &str)]) -> std::process::Child {
    spawn(d, &launch_json(script, env))
}

fn finish(mut c: std::process::Child) -> (i32, String, String) {
    let (mut o, mut e) = (String::new(), String::new());
    c.stdout.take().unwrap().read_to_string(&mut o).unwrap();
    c.stderr.take().unwrap().read_to_string(&mut e).unwrap();
    let code = c.wait().unwrap().code().unwrap_or(-1);
    (code, o, e)
}

#[test]
fn sigterm_reaches_the_app() {
    let d = dir("term");
    let c = start(
        &d,
        "trap 'echo got-term; exit 0' TERM; echo ready; while true; do sleep 0.1; done",
        &[],
    );
    std::thread::sleep(Duration::from_millis(500));
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(c.id() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    let start = Instant::now();
    let (code, out, err) = finish(c);
    assert_eq!(code, 0, "{out} {err}");
    assert!(out.contains("ready") && out.contains("got-term"), "{out:?}");
    assert!(start.elapsed() < Duration::from_secs(10));
}

#[test]
fn the_app_starts_with_no_blocked_signal() {
    // Review of I6-20 (Kimi re-check, finding 1): the supervisor blocks the forwarded signals in
    // its threads, and std's `Command` keeps the mask in the child. A shell resets its own mask,
    // so `sigterm_reaches_the_app` cannot see this: `cat` does not, and shows what it inherited.
    let d = dir("sigmask");
    let c = spawn(&d, &launch_argv(&["cat", "/proc/self/status"], &[]));
    let (code, out, err) = finish(c);
    assert_eq!(code, 0, "{err}");
    let blocked = out
        .lines()
        .find_map(|l| l.strip_prefix("SigBlk:"))
        .map(str::trim)
        .unwrap_or_else(|| panic!("no SigBlk line: {out:?}"));
    assert!(
        blocked.chars().all(|c| c == '0'),
        "the app inherited blocked signals: {blocked}"
    );
}

#[test]
fn a_missing_command_exits_127_in_init_mode() {
    let d = dir("missing");
    let c = spawn(
        &d,
        &launch_argv(&["/no/such/app"], &[("S", "missing-secret")]),
    );
    let (code, out, err) = finish(c);
    assert_eq!(code, 127, "{out} {err}");
    assert!(err.contains("did not start"), "{err}");
    assert!(!out.contains("missing-secret") && !err.contains("missing-secret"));
}

#[test]
fn the_app_cannot_reach_the_supervisors_output_through_proc() {
    // The supervisor is non-dumpable: its /proc files belong to root, so the app (the same user)
    // cannot open /proc/<supervisor>/fd/1 to write past the redactor.
    let d = dir("proc");
    let c = start(
        &d,
        "echo raw-$S > /proc/$PPID/fd/1 2>/dev/null && echo bypassed || echo refused; cat /proc/$PPID/environ >/dev/null 2>&1 && echo environ-read || echo environ-refused",
        &[("S", "very-secret-value")],
    );
    let (code, out, err) = finish(c);
    assert_eq!(code, 0, "{err}");
    assert!(
        !out.contains("very-secret-value"),
        "the raw value reached the output: {out:?}"
    );
    assert!(
        out.contains("refused") && !out.contains("bypassed"),
        "{out:?}"
    );
    assert!(out.contains("environ-refused"), "{out:?}");
}

#[test]
fn orphans_are_reaped_and_the_code_is_the_apps() {
    let d = dir("reap");
    let c = start(&d, "sh -c 'sleep 0.2' & sh -c 'exit 0' & wait; exit 4", &[]);
    let (code, _, err) = finish(c);
    assert_eq!(code, 4, "{err}");
}

#[test]
fn a_broken_launch_exits_125_without_a_value() {
    let d = dir("broken");
    let c = spawn(
        &d,
        br#"{"v":1,"argv":[],"env":[["K","secret-in-bad-data"]]}"#,
    );
    let (code, out, err) = finish(c);
    assert_eq!(code, 125);
    assert!(!out.contains("secret-in-bad-data") && !err.contains("secret-in-bad-data"));
    assert!(err.contains("no command"), "{err}");
}

#[test]
fn the_app_reads_no_launch_data_from_standard_input() {
    let d = dir("stdin");
    let c = start(&d, "cat; echo end-of-input", &[("S", "stdin-secret")]);
    let (code, out, err) = finish(c);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out.trim(), "end-of-input", "{out:?}");
}

#[test]
fn no_launch_data_ends_the_run() {
    let d = dir("none");
    let c = spawn(&d, b"");
    let (code, _, err) = finish(c);
    assert_eq!(code, 125);
    assert!(err.contains("does not parse"), "{err}");
}
