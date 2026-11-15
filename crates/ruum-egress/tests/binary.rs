//! The conformance suite against the running proxy (card R-2e): both listeners of
//! `ruum_egress::start` (the binary's code path) on 127.0.0.1 and a Unix socket, with the fake
//! resolver in process. Then the built binary itself (`ruum-egress`, the system resolver): the
//! start-up checks, the control listener, the token never in its log, and the address rules on
//! IP literals. Linux only (loopback source addresses other than 127.0.0.1).
#![cfg(target_os = "linux")]

use std::net::{IpAddr, SocketAddr};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ruum_egress::conformance::{
    SOURCE_A, SOURCE_UNKNOWN, TEST_DENY, TOKEN, TempDir, binary_harness, http_request,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

ruum_egress::conformance_tests!(binary_harness);

const BIN: &str = env!("CARGO_BIN_EXE_ruum-egress");

/// The binary, started with exactly these environment values.
struct Proc {
    child: Child,
    log: Arc<Mutex<String>>,
}

impl Proc {
    fn spawn(vars: &[(&str, String)]) -> Proc {
        let mut cmd = Command::new(BIN);
        cmd.env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (k, v) in vars {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("the ruum-egress binary starts");
        let log = Arc::new(Mutex::new(String::new()));
        for out in [
            child
                .stderr
                .take()
                .map(|e| Box::new(e) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
            child
                .stdout
                .take()
                .map(|e| Box::new(e) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let log = log.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(out).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    let mut g = log.lock().unwrap();
                    g.push_str(&l);
                    g.push('\n');
                }
            });
        }
        Proc { child, log }
    }

    fn log(&self) -> String {
        self.log.lock().unwrap().clone()
    }

    async fn wait_log(&self, needle: &str) -> String {
        for _ in 0..200 {
            if let Some(l) = self.log().lines().find(|l| l.contains(needle)) {
                return l.to_string();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("no line with `{needle}` in the log:\n{}", self.log());
    }

    /// Waits for the process to end by itself; returns its exit code and its log.
    async fn exit(mut self) -> (Option<i32>, String) {
        let status = tokio::time::timeout(Duration::from_secs(20), self.child.wait())
            .await
            .expect("the binary must stop by itself")
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        (status.code(), self.log())
    }

    async fn kill(mut self) -> String {
        let _ = self.child.kill().await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        self.log()
    }
}

fn base_vars(dir: &TempDir) -> Vec<(&'static str, String)> {
    let token_file = dir.path.join("token");
    std::fs::write(&token_file, format!("{TOKEN}\n")).unwrap();
    vec![
        ("RUUM_EGRESS_LISTEN", "127.0.0.1:0".into()),
        (
            "RUUM_EGRESS_CONTROL",
            format!("unix:{}", dir.path.join("control.sock").display()),
        ),
        (
            "RUUM_EGRESS_CONTROL_TOKEN_FILE",
            token_file.display().to_string(),
        ),
        ("EGRESS_DENY", TEST_DENY.into()),
        ("RUUM_EGRESS_LOG", "info".into()),
    ]
}

fn with(
    mut vars: Vec<(&'static str, String)>,
    k: &'static str,
    v: &str,
) -> Vec<(&'static str, String)> {
    vars.retain(|(n, _)| *n != k);
    vars.push((k, v.to_string()));
    vars
}

#[tokio::test]
async fn binary_bad_deny_entry_stops_the_start() {
    let dir = TempDir::new();
    for bad in [
        "95.217.0.10, 95.217.0.300",
        "95.217.0.10,,37.27.0.0/24",
        "host.example.com",
    ] {
        let (code, log) = Proc::spawn(&with(base_vars(&dir), "EGRESS_DENY", bad))
            .exit()
            .await;
        assert_eq!(code, Some(2), "EGRESS_DENY={bad}: {log}");
        assert!(log.contains("EGRESS_DENY"), "a clear error: {log}");
        assert!(!log.contains("ruum-egress started"), "{log}");
    }
}

#[tokio::test]
async fn binary_empty_deny_stops_the_start_outside_dev_mode() {
    let dir = TempDir::new();
    let mut vars = base_vars(&dir);
    vars.retain(|(k, _)| *k != "EGRESS_DENY");
    let (code, log) = Proc::spawn(&vars).exit().await;
    assert_eq!(code, Some(2), "{log}");
    assert!(log.contains("EGRESS_DENY is empty"), "{log}");

    let p = Proc::spawn(&with(vars, "RUUM_EGRESS_DEV", "1"));
    p.wait_log("ruum-egress started").await;
    p.wait_log("dev mode").await;
    p.kill().await;
}

#[tokio::test]
async fn binary_control_on_a_served_network_stops_the_start() {
    let dir = TempDir::new();
    for (listen, control) in [
        ("0.0.0.0:0", "127.0.0.1:0"),
        ("127.0.0.1:0", "127.0.0.1:0"),
        ("127.0.0.1:0", "127.0.0.2:0"),
    ] {
        let vars = with(
            with(base_vars(&dir), "RUUM_EGRESS_LISTEN", listen),
            "RUUM_EGRESS_CONTROL",
            control,
        );
        let (code, log) = Proc::spawn(&vars).exit().await;
        assert_eq!(code, Some(2), "listen {listen}, control {control}: {log}");
        assert!(log.contains("on a network the proxy also serves"), "{log}");
    }
    // A short token stops it too, and the log never holds a token value.
    let short = dir.path.join("short");
    std::fs::write(&short, "too-short-token").unwrap();
    let (code, log) = Proc::spawn(&with(
        base_vars(&dir),
        "RUUM_EGRESS_CONTROL_TOKEN_FILE",
        &short.display().to_string(),
    ))
    .exit()
    .await;
    assert_eq!(code, Some(2), "{log}");
    assert!(!log.contains("too-short-token"), "{log}");
}

/// Sends a request head from `source` and returns the status and the deny reason.
async fn send(proxy: SocketAddr, source: IpAddr, head: &str) -> (u16, Option<String>) {
    let sock = tokio::net::TcpSocket::new_v4().unwrap();
    sock.bind(SocketAddr::new(source, 0)).unwrap();
    let mut s = sock.connect(proxy).await.unwrap();
    s.write_all(head.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let mut b = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        let n = tokio::time::timeout(Duration::from_secs(10), s.read(&mut b))
            .await
            .expect("an answer")
            .unwrap();
        assert_eq!(
            n,
            1,
            "the proxy closed without an answer: {:?}",
            String::from_utf8_lossy(&buf)
        );
        buf.push(b[0]);
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status = text
        .split(' ')
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let deny = text
        .lines()
        .find_map(|l| l.strip_prefix("X-Ruum-Egress-Deny: ").map(str::to_string));
    (status, deny)
}

#[tokio::test]
async fn binary_end_to_end() {
    let dir = TempDir::new();
    let p = Proc::spawn(&base_vars(&dir));
    let started = p.wait_log("ruum-egress started").await;
    let proxy: SocketAddr = started
        .split_whitespace()
        .find_map(|w| w.strip_prefix("listen="))
        .and_then(|a| a.parse().ok())
        .unwrap_or_else(|| panic!("no listen address in `{started}`"));
    let socket = dir.path.join("control.sock");
    let control = |method: &'static str,
                   path: &'static str,
                   token: Option<&'static str>,
                   body: Option<String>| {
        let socket = socket.clone();
        async move {
            let s = tokio::net::UnixStream::connect(&socket).await.unwrap();
            http_request(s, method, path, token, body).await
        }
    };

    // The control listener: a wrong token is refused, the right one registers.
    let body = r#"{"subnet":"127.0.0.2/32","identity":{"tenant":"0190f0e2-0000-7000-8000-000000000001","app":"0190f0e2-0000-7000-8000-000000000002","deployment":"0190f0e2-0000-7000-8000-000000000003","environment":"live"}}"#.to_string();
    assert_eq!(
        control(
            "PUT",
            "/v1/networks/a",
            Some("wrong-token"),
            Some(body.clone())
        )
        .await
        .0,
        401
    );
    assert_eq!(
        control("PUT", "/v1/networks/a", None, Some(body.clone()))
            .await
            .0,
        401
    );
    assert_eq!(
        control("PUT", "/v1/networks/a", Some(TOKEN), Some(body.clone()))
            .await
            .0,
        204
    );

    let connect = |host: &str| format!("CONNECT {host}:443 HTTP/1.1\r\nHost: {host}:443\r\n\r\n");
    assert_eq!(
        send(proxy, SOURCE_UNKNOWN, &connect("registry.npmjs.org")).await,
        (403, Some("unknown_source".into()))
    );
    for host in [
        "127.0.0.1",
        "127.1",
        "0x7f.1",
        "2852039166",
        "0251.0376.0251.0376",
        "0xa9fea9fe",
        "169.254.169.254",
        "[::1]",
        "[fd00:ec2::254]",
        "[fe80::1]",
        "[::ffff:127.0.0.1]",
        "[::ffff:95.217.0.10]",
        "95.217.0.10",
        "[2a01:4f9:c010:1234::1]",
        "[2a01:4f9:c010:1234:ffff::5]",
        "37.27.0.5",
    ] {
        assert_eq!(
            send(proxy, SOURCE_A, &connect(host)).await,
            (403, Some("address".into())),
            "{host}"
        );
    }
    assert_eq!(
        send(
            proxy,
            SOURCE_A,
            "CONNECT registry.npmjs.org:22 HTTP/1.1\r\n\r\n"
        )
        .await,
        (403, Some("port".into()))
    );
    assert_eq!(
        send(
            proxy,
            SOURCE_A,
            "GET http://example.com/private/path?token=abc HTTP/1.1\r\nHost: example.com\r\nX-Secret: hunter2\r\n\r\n"
        )
        .await,
        (405, Some("method".into()))
    );
    // The control interface is not on the proxy port.
    let put = format!(
        "PUT /v1/networks/evil HTTP/1.1\r\nHost: egress\r\nAuthorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body.replace("127.0.0.2/32", "127.0.0.4/32")
    );
    assert_eq!(
        send(proxy, SOURCE_A, &put).await,
        (405, Some("method".into()))
    );
    assert_eq!(
        send(
            proxy,
            "127.0.0.4".parse().unwrap(),
            &connect("registry.npmjs.org")
        )
        .await,
        (403, Some("unknown_source".into()))
    );

    let (status, usage) = control("GET", "/v1/usage", Some(TOKEN), None).await;
    assert_eq!(status, 200, "{usage}");
    let report: ruum_egress::api::UsageReport = serde_json::from_str(&usage).unwrap();
    let denied: u64 = report.entries.iter().map(|e| e.denied).sum();
    assert_eq!(denied, 19, "16 addresses, a port and two methods: {usage}");

    let log = p.kill().await;
    assert!(log.contains("deny=address"), "{log}");
    assert!(log.contains("deny=unknown_source"), "{log}");
    assert!(log.contains("egress network registered"), "{log}");
    for secret in [
        TOKEN,
        "/private",
        "token=abc",
        "hunter2",
        "X-Secret",
        "Bearer",
    ] {
        assert!(
            !log.contains(secret),
            "the binary's log holds `{secret}`:\n{log}"
        );
    }
}
