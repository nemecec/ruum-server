//! The supervisor: loads the launch data, starts the app, redacts its output, forwards signals,
//! reaps children and exits with the app's exit code (`docs/STEP3.md` "The sandbox supervisor",
//! point 2).

use crate::http::{self, HttpError};
use crate::protocol::{
    EXIT_LAUNCH_FAILED, EXIT_NOT_STARTED, Launch, MAX_LAUNCH_BYTES, TokenAnswer,
};
use crate::redact::{LineRedactor, MAX_KEPT_TOKENS, MAX_SECRET_VALUE_BYTES};
use nix::sys::signal::{SigSet, Signal, kill};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::Pid;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime};
use time::OffsetDateTime;

/// Where the launch data comes from.
#[derive(Clone, Debug)]
pub enum Source {
    /// The Docker driver: the launch data on standard input, which the driver closes after it
    /// (`OpenStdin`, `StdinOnce`). The app gets `/dev/null`.
    Stdin,
    /// The server's supervisor listener (Kubernetes), with the pod's projected token file.
    Http { base: SocketAddr, identity: PathBuf },
}

/// How one supervisor runs.
#[derive(Clone, Debug)]
pub struct Options {
    pub source: Source,
    /// The workload token file of the app (`TOKEN_FILE`). Docker: the driver writes it and the
    /// supervisor reads it to redact a rotated token. Kubernetes: the supervisor writes it.
    pub token_file: PathBuf,
    /// Emptied when the app ends (`/dev/termination-log`); `None` in tests.
    pub termination_log: Option<PathBuf>,
    /// The first process of the sandbox: reap every child and forward signals. Off in tests (a
    /// test process has other children and signal handlers of its own).
    pub init: bool,
    pub max_line_bytes: usize,
    /// How long the launch fetch tries (Kubernetes) before it gives up.
    pub launch_timeout: Duration,
    /// How long the output of the app's children is read after the app ended.
    pub drain: Duration,
}

impl Options {
    pub fn new(source: Source, token_file: PathBuf) -> Self {
        Self {
            source,
            token_file,
            termination_log: None,
            init: false,
            max_line_bytes: crate::protocol::MAX_LINE_BYTES,
            launch_timeout: Duration::from_secs(60),
            drain: Duration::from_secs(2),
        }
    }
}

/// The largest identity token file the supervisor reads.
const MAX_IDENTITY_BYTES: u64 = 64 * 1024;
/// One request to the launch listener at most.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// The token poll interval when the launch data names none.
const DEFAULT_TOKEN_POLL: Duration = Duration::from_secs(30);

fn read_capped(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let f = std::fs::File::open(path)
        .map_err(|e| format!("cannot open {}: {}", path.display(), e.kind()))?;
    let mut out = Vec::new();
    f.take(max + 1)
        .read_to_end(&mut out)
        .map_err(|e| format!("cannot read {}: {}", path.display(), e.kind()))?;
    if out.len() as u64 > max {
        return Err(format!("{} is too large", path.display()));
    }
    Ok(out)
}

fn identity(path: &Path) -> Result<String, String> {
    let raw = read_capped(path, MAX_IDENTITY_BYTES)?;
    let text = String::from_utf8(raw).map_err(|_| "the identity token is not text".to_owned())?;
    let t = text.trim().to_owned();
    if t.is_empty() {
        return Err("the identity token is empty".to_owned());
    }
    Ok(t)
}

/// `POST path` to the listener with the pod's identity, retried on transport errors, 429 and
/// 5xx until `deadline`. Any other status ends at once.
fn fetch(
    base: SocketAddr,
    identity_file: &Path,
    path: &str,
    deadline: Instant,
) -> Result<Vec<u8>, String> {
    let mut wait = Duration::from_millis(200);
    loop {
        let r = identity(identity_file).and_then(|tok| {
            http::post(base, path, &tok, MAX_LAUNCH_BYTES, HTTP_TIMEOUT).map_err(|e| match e {
                HttpError::Status(s) if s != 429 && s < 500 => format!("refused: status {s}"),
                other => format!("retry: {other}"),
            })
        });
        match r {
            Ok(body) => return Ok(body),
            Err(e) if e.starts_with("refused") => return Err(e),
            Err(e) => {
                if Instant::now() + wait > deadline {
                    return Err(e);
                }
                std::thread::sleep(wait);
                wait = (wait * 2).min(Duration::from_secs(5));
            }
        }
    }
}

/// Standard input to its end, at most [`MAX_LAUNCH_BYTES`], within `timeout`.
fn read_stdin(timeout: Duration) -> Result<Vec<u8>, String> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("launch".into())
        .spawn(move || {
            let mut out = Vec::new();
            let r = std::io::stdin()
                .lock()
                .take(MAX_LAUNCH_BYTES as u64 + 1)
                .read_to_end(&mut out)
                .map(|_| out);
            let _ = tx.send(r);
        })
        .map_err(|_| "cannot start the launch reader".to_owned())?;
    match rx.recv_timeout(timeout) {
        Ok(Ok(raw)) if raw.len() > MAX_LAUNCH_BYTES => {
            Err("the launch data is too large".to_owned())
        }
        Ok(Ok(raw)) => Ok(raw),
        Ok(Err(e)) => Err(format!("cannot read the launch data: {}", e.kind())),
        Err(_) => Err("no launch data on standard input".to_owned()),
    }
}

/// Parses and checks launch data. `with_token`: the Kubernetes answer must carry the token.
pub fn parse_launch(raw: &[u8], with_token: bool) -> Result<Launch, String> {
    let launch: Launch =
        serde_json::from_slice(raw).map_err(|_| "the launch data does not parse".to_owned())?;
    launch.check()?;
    if with_token && launch.token.is_none() {
        return Err("the launch answer has no workload token".to_owned());
    }
    Ok(launch)
}

/// Reads and checks the launch data, once. `Err` names the rule or the step, never a value.
pub fn load(opts: &Options) -> Result<Launch, String> {
    match &opts.source {
        Source::Stdin => parse_launch(&read_stdin(opts.launch_timeout)?, false),
        Source::Http { base, identity } => parse_launch(
            &fetch(
                *base,
                identity,
                "/v1/launch",
                Instant::now() + opts.launch_timeout,
            )?,
            true,
        ),
    }
}

/// Empties the termination message file (the kubelet copies it into the pod's status).
fn empty_termination_log(opts: &Options) {
    if let Some(t) = &opts.termination_log {
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(t);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Which {
    Out,
    Err,
}

/// The redaction state of a run: one redactor per stream, the values, the writers.
struct Shared {
    out: LineRedactor,
    err: LineRedactor,
    out_w: Box<dyn Write + Send>,
    err_w: Box<dyn Write + Send>,
    /// A stream that was finished (its rest let out): later bytes of it are dropped.
    out_closed: bool,
    err_closed: bool,
    base: Vec<String>,
    tokens: VecDeque<String>,
    /// The token file's modification time and length at the last read (Docker).
    token_seen: Option<(SystemTime, u64)>,
}

impl Shared {
    fn values(&self) -> Vec<String> {
        let mut v = self.base.clone();
        v.extend(self.tokens.iter().cloned());
        v
    }

    /// Adds a token to both redactors (before the app can print it, for the Kubernetes path).
    fn add_token(&mut self, token: String) {
        if token.is_empty() || token.len() > MAX_SECRET_VALUE_BYTES || self.tokens.contains(&token)
        {
            return;
        }
        self.tokens.push_back(token);
        while self.tokens.len() > MAX_KEPT_TOKENS {
            self.tokens.pop_front();
        }
        let all = self.values();
        self.out.set_values(all.iter().map(String::as_str));
        self.err.set_values(all.iter().map(String::as_str));
    }

    /// Docker: reads the driver's token file again when it changed (a rotation), so a new token
    /// is redacted before the output that follows the change goes out.
    fn refresh_token_file(&mut self, path: &Path) {
        let Ok(meta) = std::fs::metadata(path) else {
            return;
        };
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let stamp = (modified, meta.len());
        // A file system's clock can be coarse (a few milliseconds): a stamp of a very recent
        // change may hide a second write in the same tick, so it is read again.
        let recent = modified
            .elapsed()
            .map_or(true, |e| e < Duration::from_millis(100));
        if self.token_seen == Some(stamp) && !recent {
            return;
        }
        self.token_seen = Some(stamp);
        if let Ok(raw) = read_capped(path, MAX_SECRET_VALUE_BYTES as u64)
            && let Ok(t) = String::from_utf8(raw)
        {
            self.add_token(t);
        }
    }

    fn emit(&mut self, which: Which, lines: Vec<(OffsetDateTime, String)>) {
        let w = match which {
            Which::Out => &mut self.out_w,
            Which::Err => &mut self.err_w,
        };
        for (_, l) in lines {
            let _ = w.write_all(l.as_bytes());
            let _ = w.write_all(b"\n");
        }
        let _ = w.flush();
    }

    fn push(&mut self, which: Which, bytes: &[u8], token_file: Option<&Path>) {
        let closed = match which {
            Which::Out => self.out_closed,
            Which::Err => self.err_closed,
        };
        if closed {
            return;
        }
        if let Some(p) = token_file {
            self.refresh_token_file(p);
        }
        let now = OffsetDateTime::now_utc();
        let lines = match which {
            Which::Out => self.out.push_bytes(bytes, now),
            Which::Err => self.err.push_bytes(bytes, now),
        };
        self.emit(which, lines);
    }

    /// Lets out the held-back rest of a stream (redacted) and drops whatever comes later.
    fn finish(&mut self, which: Which) {
        let now = OffsetDateTime::now_utc();
        let lines = match which {
            Which::Out if !self.out_closed => {
                self.out_closed = true;
                self.out.finish(now)
            }
            Which::Err if !self.err_closed => {
                self.err_closed = true;
                self.err.finish(now)
            }
            _ => return,
        };
        self.emit(which, lines);
    }
}

fn pump(
    mut r: impl Read,
    which: Which,
    shared: Arc<Mutex<Shared>>,
    token_file: Option<PathBuf>,
    done: mpsc::Sender<Which>,
) {
    let mut buf = vec![0u8; 8192];
    loop {
        match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => shared.lock().unwrap_or_else(|p| p.into_inner()).push(
                which,
                &buf[..n],
                token_file.as_deref(),
            ),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    shared
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .finish(which);
    let _ = done.send(which);
}

/// Writes the token file whole: a new file under a fresh name, renamed over the old one, so a
/// reader never sees a partial token. The app (the same user) can write the folder, so the new
/// file is made with `O_CREAT | O_EXCL` (`create_new`): an existing name, also a symbolic link
/// the app planted (say to `/proc/self/fd/1`, the supervisor's raw output), fails instead of
/// being followed, and another name is tried. `rename` replaces a link at the target itself and
/// never follows it (Kimi review of I6-20, finding 1).
fn write_token_file(path: &Path, token: &str) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    static SEQ: AtomicI32 = AtomicI32::new(0);
    let dir = path.parent().unwrap_or(Path::new("."));
    for _ in 0..16 {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let tmp = dir.join(format!(
            ".token-{}-{}-{nanos}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let mut f = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o440)
            .open(&tmp)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        let written = f.write_all(token.as_bytes()).and_then(|()| f.sync_all());
        drop(f);
        if let Err(e) = written.and_then(|()| std::fs::rename(&tmp, path)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no free name for the token file",
    ))
}

/// The PID of the app, for the signal thread (0 before it starts).
static APP: AtomicI32 = AtomicI32::new(0);

/// The signals the supervisor forwards to the app.
fn forwarded() -> SigSet {
    let mut s = SigSet::empty();
    for sig in [
        Signal::SIGTERM,
        Signal::SIGINT,
        Signal::SIGHUP,
        Signal::SIGQUIT,
        Signal::SIGUSR1,
        Signal::SIGUSR2,
    ] {
        s.add(sig);
    }
    s
}

/// Runs the app of `launch` to its end and returns the exit code for the supervisor: the app's
/// code, 128 + the signal that ended it, or [`EXIT_NOT_STARTED`]. `out` and `err` get the
/// redacted lines (the process's own standard output and standard error in the binary).
pub fn supervise(
    opts: &Options,
    launch: Launch,
    out: Box<dyn Write + Send>,
    err: Box<dyn Write + Send>,
) -> i32 {
    empty_termination_log(opts);
    let mut base: Vec<String> = launch.values();
    if let Some(t) = &launch.token {
        base.retain(|v| v != t);
    }
    let values: Vec<String> = base
        .iter()
        .cloned()
        .chain(launch.token.iter().cloned())
        .collect();
    let red = || {
        LineRedactor::new(values.iter().map(String::as_str))
            .with_max_line_bytes(opts.max_line_bytes)
    };
    let shared = Arc::new(Mutex::new(Shared {
        out: red(),
        err: red(),
        out_w: out,
        err_w: err,
        out_closed: false,
        err_closed: false,
        base,
        tokens: launch.token.iter().cloned().collect(),
        token_seen: None,
    }));
    let say = |msg: &str| {
        let mut s = shared.lock().unwrap_or_else(|p| p.into_inner());
        let _ = writeln!(s.err_w, "ruum-supervisor: {msg}");
        let _ = s.err_w.flush();
    };
    // Kubernetes: the token file comes from the supervisor. Docker: the driver's file is read
    // before the app starts, so a token printed at once is known.
    let token_file = match &opts.source {
        Source::Http { .. } => {
            if let Some(t) = &launch.token
                && let Err(e) = write_token_file(&opts.token_file, t)
            {
                say(&format!("cannot write the token file: {}", e.kind()));
                return EXIT_LAUNCH_FAILED;
            }
            None
        }
        Source::Stdin => {
            shared
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .refresh_token_file(&opts.token_file);
            Some(opts.token_file.clone())
        }
    };
    // Signals: blocked in every thread made from here on; one thread waits for them and forwards
    // them to the app.
    if opts.init {
        let set = forwarded();
        if set.thread_block().is_err() {
            say("cannot block the forwarded signals");
            return EXIT_LAUNCH_FAILED;
        }
        let spawned = std::thread::Builder::new()
            .name("signals".into())
            .spawn(move || {
                loop {
                    let Ok(sig) = set.wait() else { continue };
                    let pid = APP.load(Ordering::SeqCst);
                    if pid > 0 {
                        let _ = kill(Pid::from_raw(pid), sig);
                    } else {
                        std::process::exit(128 + sig as i32);
                    }
                }
            });
        if spawned.is_err() {
            say("cannot start the signal thread");
            return EXIT_LAUNCH_FAILED;
        }
    }
    // Kubernetes: the current token every poll interval, redacted first, then written.
    if let (Source::Http { base, identity }, Some(_)) = (&opts.source, &launch.token) {
        let (base, identity) = (*base, identity.clone());
        let path = opts.token_file.clone();
        let shared = shared.clone();
        let every = launch
            .token_poll_secs
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_TOKEN_POLL)
            .max(Duration::from_secs(1));
        let _ = std::thread::Builder::new()
            .name("token".into())
            .spawn(move || {
                let mut current = String::new();
                loop {
                    std::thread::sleep(every);
                    let got = fetch(base, &identity, "/v1/token", Instant::now() + every)
                        .and_then(|raw| {
                            serde_json::from_slice::<TokenAnswer>(&raw)
                                .map_err(|_| "the token answer does not parse".to_owned())
                        })
                        .and_then(|a| a.check().map(|()| a.token));
                    match got {
                        Ok(t) if t != current => {
                            shared
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .add_token(t.clone());
                            if write_token_file(&path, &t).is_ok() {
                                current = t;
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            let mut s = shared.lock().unwrap_or_else(|p| p.into_inner());
                            let _ = writeln!(
                                s.err_w,
                                "ruum-supervisor: the token renewal failed ({e})"
                            );
                            let _ = s.err_w.flush();
                        }
                    }
                }
            });
    }
    if let Some(n) = launch.pids_limit
        && nix::sys::resource::setrlimit(nix::sys::resource::Resource::RLIMIT_NPROC, n, n).is_err()
    {
        say("cannot set the PID limit");
        return EXIT_LAUNCH_FAILED;
    }
    // Init mode: this thread blocks the forwarded signals, and a child inherits the mask (std's
    // `Command` keeps it), so an app that never resets its mask (most compiled programs) would
    // never see a forwarded SIGTERM. The supervisor's own `exec` step empties the mask, then
    // replaces itself with the app (no `unsafe` here: the workspace forbids it).
    let mut cmd = if opts.init {
        let mut c = Command::new(SELF_EXE);
        c.arg("exec").arg("--").args(&launch.argv);
        c
    } else {
        let mut c = Command::new(&launch.argv[0]);
        c.args(&launch.argv[1..]);
        c
    };
    cmd.envs(launch.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            say(&format!("the app's command did not start: {}", e.kind()));
            return EXIT_NOT_STARTED;
        }
    };
    let pid = Pid::from_raw(child.id() as i32);
    APP.store(pid.as_raw(), Ordering::SeqCst);
    let (done_tx, done_rx) = mpsc::channel();
    let mut pumps = 0;
    for (stream, which) in [
        (
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
            Which::Out,
        ),
        (
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
            Which::Err,
        ),
    ] {
        let Some(stream) = stream else { continue };
        let (shared, tf, tx) = (shared.clone(), token_file.clone(), done_tx.clone());
        let started = std::thread::Builder::new()
            .name("pump".into())
            .spawn(move || pump(stream, which, shared, tf, tx));
        if started.is_err() {
            let _ = kill(pid, Signal::SIGKILL);
            say("cannot start an output thread");
            let _ = waitpid(pid, None);
            return EXIT_LAUNCH_FAILED;
        }
        pumps += 1;
    }
    drop(done_tx);
    let code = loop {
        let w = if opts.init {
            waitpid(None, None)
        } else {
            waitpid(pid, None)
        };
        match w {
            Ok(WaitStatus::Exited(p, c)) if p == pid => break c,
            Ok(WaitStatus::Signaled(p, s, _)) if p == pid => break 128 + s as i32,
            // An orphan reparented to the supervisor (PID 1), or a stop or continue report.
            Ok(_) => continue,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => break EXIT_LAUNCH_FAILED,
        }
    };
    APP.store(0, Ordering::SeqCst);
    // The app's children may still hold the pipes: read what comes within the drain time, then
    // let out the rest of each stream and stop.
    let deadline = Instant::now() + opts.drain;
    let mut finished = 0;
    while finished < pumps {
        let left = deadline.saturating_duration_since(Instant::now());
        match done_rx.recv_timeout(left) {
            Ok(_) => finished += 1,
            Err(_) => break,
        }
    }
    {
        let mut s = shared.lock().unwrap_or_else(|p| p.into_inner());
        s.finish(Which::Out);
        s.finish(Which::Err);
    }
    if let Some(t) = &opts.termination_log {
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(t);
    }
    code
}

/// The supervisor's own binary, for the `exec` step of init mode.
const SELF_EXE: &str = "/proc/self/exe";

/// The `exec` step of init mode (`ruum-supervisor exec -- <argv...>`): empties the signal mask
/// this process inherited from the supervisor and replaces it with the app. Returns only when
/// the app's command did not start.
pub fn exec_app(argv: &[String]) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    if SigSet::empty().thread_set_mask().is_err() {
        return std::io::Error::other("cannot empty the signal mask");
    }
    Command::new(&argv[0]).args(&argv[1..]).exec()
}

/// Copies the running binary to `dest` (mode 0555), for the init container of a Kubernetes
/// sandbox: `ruum-supervisor install /ruum/bin/ruum-supervisor`.
pub fn install(dest: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let me =
        std::env::current_exe().map_err(|e| format!("cannot find the binary: {}", e.kind()))?;
    let tmp = dest.with_extension("new");
    std::fs::copy(&me, &tmp).map_err(|e| format!("cannot copy the binary: {}", e.kind()))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o555))
        .map_err(|e| format!("cannot set the mode: {}", e.kind()))?;
    std::fs::rename(&tmp, dest).map_err(|e| format!("cannot move the binary: {}", e.kind()))
}

#[cfg(test)]
mod tests {
    //! The supervisor with real child processes (`/bin/sh`), in test mode (no reaping of other
    //! children, no signal thread).

    use super::*;
    use crate::protocol::LAUNCH_VERSION;

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Buf {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ruum-supervisor-test-{}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn launch(script: &str, env: &[(&str, &str)]) -> Launch {
        Launch {
            v: LAUNCH_VERSION,
            argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
            env: env
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
            redact: vec!["old-value-1234".into()],
            token: None,
            pids_limit: None,
            token_poll_secs: None,
        }
    }

    fn run(l: Launch, token_file: &Path) -> (i32, String, String) {
        let opts = Options::new(Source::Stdin, token_file.to_owned());
        let (o, e) = (Buf::default(), Buf::default());
        let code = supervise(&opts, l, Box::new(o.clone()), Box::new(e.clone()));
        (code, o.text(), e.text())
    }

    #[test]
    fn secret_values_and_tokens_never_leave_unredacted() {
        let d = tmp("redact");
        let tf = d.join("token");
        std::fs::write(&tf, "tok-AAAA-1111").unwrap();
        let script = r#"
            echo "env=$API_KEY"
            printf 'split=%s' "${API_KEY%????}"; printf '%s\n' "${API_KEY#????????}"
            echo "old=old-value-1234"
            echo "token=$(cat "$TF")" >&2
            printf 'tok-BBBB-22222' > "$TF"
            echo "new=$(cat "$TF")"
            echo plain-visible
            exit 7
        "#;
        let mut l = launch(script, &[("API_KEY", "s3cr3t-abcdef")]);
        l.argv[2] = script.replace("$TF", &tf.display().to_string());
        let (code, out, err) = run(l, &tf);
        assert_eq!(code, 7);
        for v in [
            "s3cr3t-abcdef",
            "old-value-1234",
            "tok-AAAA-1111",
            "tok-BBBB-22222",
        ] {
            assert!(
                !out.contains(v) && !err.contains(v),
                "{v} leaked: {out:?} {err:?}"
            );
        }
        assert!(out.contains("env=<hidden secret>"), "{out:?}");
        assert!(out.contains("plain-visible"), "{out:?}");
        assert!(err.contains("token=<hidden secret>"), "{err:?}");
        assert!(out.contains("new=<hidden secret>"), "{out:?}");
    }

    #[test]
    fn the_exit_code_and_a_signal_are_reported() {
        let d = tmp("exit");
        let (code, _, _) = run(launch("exit 3", &[]), &d.join("none"));
        assert_eq!(code, 3);
        let (code, _, _) = run(launch("kill -9 $$", &[]), &d.join("none"));
        assert_eq!(code, 128 + 9);
        let mut l = launch("", &[]);
        l.argv = vec!["/nonexistent/app".into()];
        let (code, _, err) = run(l, &d.join("none"));
        assert_eq!(code, EXIT_NOT_STARTED);
        assert!(err.contains("did not start"), "{err}");
    }

    #[test]
    fn a_background_child_does_not_hold_the_supervisor() {
        let d = tmp("bg");
        let start = Instant::now();
        let (code, out, _) = run(
            launch("(sleep 30; echo late) & echo early", &[]),
            &d.join("none"),
        );
        assert_eq!(code, 0);
        assert!(out.contains("early") && !out.contains("late"), "{out:?}");
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn a_long_line_is_cut_and_still_redacted() {
        let d = tmp("long");
        let secret = "zz-secret-zz";
        let script = format!("head -c 40000 /dev/zero | tr '\\0' 'a'; echo {secret}");
        let (code, out, _) = run(launch(&script, &[("S", secret)]), &d.join("none"));
        assert_eq!(code, 0);
        assert!(!out.contains(secret), "leaked");
        assert!(
            out.lines()
                .all(|l| l.len() <= crate::protocol::MAX_LINE_BYTES),
            "a line over the cap"
        );
    }

    #[test]
    fn launch_data_is_checked() {
        let ok = serde_json::to_vec(&launch("true", &[("A", "b")])).unwrap();
        assert!(parse_launch(&ok, false).is_ok());
        assert!(parse_launch(&ok, true).unwrap_err().contains("token"));
        assert!(parse_launch(b"{not json", false).is_err());
        let mut l = launch("true", &[]);
        l.v = 9;
        let bad = serde_json::to_vec(&l).unwrap();
        assert!(parse_launch(&bad, false).unwrap_err().contains("version"));
    }

    #[test]
    fn the_http_launch_writes_the_token_and_refuses_a_403() {
        use std::net::TcpListener;
        let d = tmp("http");
        let idf = d.join("id");
        std::fs::write(&idf, "projected-token\n").unwrap();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let mut ok = launch(
            "cat \"$SUP_TEST_TOKEN_FILE\"; echo; echo done",
            &[("K", "v4lue-secret")],
        );
        ok.token = Some("the-workload-token".into());
        let body = serde_json::to_vec(&ok).unwrap();
        let server = std::thread::spawn(move || {
            for answer in [Some(body), None] {
                let (mut s, _) = l.accept().unwrap();
                let mut req = Vec::new();
                let mut buf = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = s.read(&mut buf).unwrap();
                    req.extend_from_slice(&buf[..n]);
                }
                let req = String::from_utf8(req).unwrap();
                assert!(
                    req.contains("Authorization: Bearer projected-token\r\n"),
                    "{req}"
                );
                match answer {
                    Some(b) => {
                        let head =
                            format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n", b.len());
                        s.write_all(head.as_bytes()).unwrap();
                        s.write_all(&b).unwrap();
                    }
                    None => s
                        .write_all(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n")
                        .unwrap(),
                }
            }
        });
        let tf = d.join("token");
        let mut opts = Options::new(
            Source::Http {
                base: addr,
                identity: idf,
            },
            tf.clone(),
        );
        opts.launch_timeout = Duration::from_secs(5);
        let got = load(&opts).expect("the launch data");
        assert_eq!(got.token.as_deref(), Some("the-workload-token"));
        let mut got = got;
        got.env
            .push(("SUP_TEST_TOKEN_FILE".into(), tf.display().to_string()));
        let (o, e) = (Buf::default(), Buf::default());
        let code = supervise(&opts, got, Box::new(o.clone()), Box::new(e.clone()));
        assert_eq!(code, 0, "{}", e.text());
        assert_eq!(std::fs::read_to_string(&tf).unwrap(), "the-workload-token");
        let out = o.text();
        assert!(
            !out.contains("the-workload-token") && out.contains("done"),
            "{out:?}"
        );
        // The second request is refused: no retry, an error.
        let err = load(&opts).unwrap_err();
        assert!(err.contains("403"), "{err}");
        server.join().unwrap();
    }

    /// A link the app planted at a name the supervisor might use is never followed: the token
    /// goes into the token file only (Kimi review of I6-20, finding 1).
    #[test]
    fn the_token_write_follows_no_planted_link() {
        let d = tmp("link");
        let victim = d.join("victim");
        std::fs::write(&victim, "untouched").unwrap();
        let tf = d.join("token");
        for name in ["token.new", ".token-0-0-0"] {
            std::os::unix::fs::symlink(&victim, d.join(name)).unwrap();
        }
        write_token_file(&tf, "the-token-value").unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched");
        assert_eq!(std::fs::read_to_string(&tf).unwrap(), "the-token-value");
        // The token file itself replaced by a link: the rename replaces the link.
        std::fs::remove_file(&tf).unwrap();
        std::os::unix::fs::symlink(&victim, &tf).unwrap();
        write_token_file(&tf, "second-value").unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched");
        assert!(
            !std::fs::symlink_metadata(&tf)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn install_copies_an_executable() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp("install");
        let dest = d.join("ruum-supervisor");
        install(&dest).unwrap();
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o555);
    }
}
