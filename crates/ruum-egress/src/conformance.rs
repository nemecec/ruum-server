//! The conformance suite of the egress proxy (card R-2e). One suite, two targets:
//! `tests/memory.rs` runs it against the in-memory policy (the proxy handler on in-memory
//! streams, the control router called directly) and `tests/binary.rs` against the running proxy
//! (both listeners of [`crate::start`], the binary's code path, on `127.0.0.1` and a Unix socket).
//!
//! Every target gets the same fakes: a [`FakeResolver`] and one loopback socket address treated
//! as public (the test-only switch of [`MemoryPolicy::set_test_public`]), with an upstream
//! listening there. A second upstream on another loopback address with the same port stands for
//! a private address.
//!
//! No case passes on silence (decision 0006 G): every refusal checks the deny reason in
//! [`DENY_HEADER`], every "reached" checks that the upstream saw the bytes, and a case whose
//! upstream did not start fails.
//!
//! The source addresses are loopback addresses other than `127.0.0.1`: Linux only (the whole
//! `127.0.0.0/8` is loopback there).

use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tower::ServiceExt;

use crate::addr::DenyList;
use crate::api::{
    DENY_HEADER, DenyReason, Environment, Limits, NetworkRegistration, UsageReport,
    WorkloadIdentity,
};
use crate::config::{Config, ConfigError};
use crate::control::{ControlState, ControlToken, router};
use crate::limits::LimitConfig;
use crate::policy::MemoryPolicy;
use crate::proxy::{MemoryLog, ProxyState, handle};
use crate::resolver::FakeResolver;
use crate::usage::Usage;
use crate::{Options, Running, start};

/// App A's network (one address).
pub const SOURCE_A: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2));
/// App B's network.
pub const SOURCE_B: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 3));
/// A source in no registered network.
pub const SOURCE_UNKNOWN: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 9));
/// The upstream the test switch treats as public.
pub const PUBLIC_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 5));
/// The upstream that stands for a private address (never treated as public).
pub const PRIVATE_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 6));
/// `EGRESS_DENY` of the suite: a host IPv4 address, the host's IPv6 /64 and a platform range.
pub const TEST_DENY: &str = "95.217.0.10, 2a01:4f9:c010:1234::/64 37.27.0.0/24";
/// The control token of the suite (test only).
pub const TOKEN: &str = "conformance-control-token-0123456789abcdef";
/// The greeting of the test upstreams.
pub const GREETING: &[u8] = b"hello\n";

/// A byte stream to the proxy.
pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

/// What a target gets to start with.
pub struct Setup {
    /// `RUUM_EGRESS_PORTS`: 443 and the port of the test upstreams.
    pub ports: Vec<u16>,
    /// The socket address the test switch treats as public.
    pub test_public: SocketAddr,
    pub resolver: Arc<FakeResolver>,
    pub log: Arc<MemoryLog>,
}

/// One target of the suite.
#[async_trait]
pub trait Harness: Send + Sync {
    /// One request to the control interface. Returns the status and the body.
    async fn control(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<String>,
    ) -> (u16, String);

    /// A connection to the proxy listener from `source`.
    async fn connect(&self, source: IpAddr) -> io::Result<Box<dyn Stream>>;
}

// --------------------------------------------------------------------------------- upstreams

/// A test upstream: greets each connection with [`GREETING`], then echoes, and records what it
/// received.
pub struct Upstream {
    pub addr: SocketAddr,
    received: Arc<Mutex<Vec<u8>>>,
    accepted: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl Upstream {
    fn start(listener: TcpListener) -> io::Result<Upstream> {
        let addr = listener.local_addr()?;
        let received = Arc::new(Mutex::new(Vec::new()));
        let accepted = Arc::new(AtomicUsize::new(0));
        let (r, a) = (received.clone(), accepted.clone());
        let task = tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                a.fetch_add(1, Ordering::SeqCst);
                let r = r.clone();
                tokio::spawn(async move {
                    if s.write_all(GREETING).await.is_err() {
                        return;
                    }
                    let mut buf = [0u8; 4096];
                    loop {
                        match s.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => {
                                r.lock().unwrap().extend_from_slice(&buf[..n]);
                                if s.write_all(&buf[..n]).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                });
            }
        });
        Ok(Upstream {
            addr,
            received,
            accepted,
            task,
        })
    }

    pub fn received_contains(&self, needle: &[u8]) -> bool {
        let r = self.received.lock().unwrap();
        r.windows(needle.len()).any(|w| w == needle)
    }

    pub fn received_len(&self) -> usize {
        self.received.lock().unwrap().len()
    }

    /// Connections accepted so far.
    pub fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

impl Drop for Upstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Binds the public and the private upstream on the same port.
async fn upstreams() -> (Upstream, Upstream) {
    for _ in 0..50 {
        let public = TcpListener::bind(SocketAddr::new(PUBLIC_IP, 0))
            .await
            .expect("the public test upstream must start (127.0.0.5: Linux loopback)");
        let port = public.local_addr().unwrap().port();
        if let Ok(private) = TcpListener::bind(SocketAddr::new(PRIVATE_IP, port)).await {
            return (
                Upstream::start(public).unwrap(),
                Upstream::start(private).unwrap(),
            );
        }
    }
    panic!("no port free on both 127.0.0.5 and 127.0.0.6");
}

// ------------------------------------------------------------------------------- environment

/// One case's world: a fresh target, the fakes and the two upstreams.
pub struct Env {
    pub h: Box<dyn Harness>,
    pub resolver: Arc<FakeResolver>,
    pub log: Arc<MemoryLog>,
    pub public: Upstream,
    pub private: Upstream,
    /// The port of both upstreams (allowed in `RUUM_EGRESS_PORTS`).
    pub port: u16,
}

impl Env {
    pub async fn new<F, Fut>(make: F) -> Env
    where
        F: FnOnce(Setup) -> Fut,
        Fut: Future<Output = Box<dyn Harness>>,
    {
        let (public, private) = upstreams().await;
        let port = public.addr.port();
        let resolver = Arc::new(FakeResolver::new());
        let log = Arc::new(MemoryLog::default());
        let h = make(Setup {
            ports: vec![443, port],
            test_public: public.addr,
            resolver: resolver.clone(),
            log: log.clone(),
        })
        .await;
        Env {
            h,
            resolver,
            log,
            public,
            private,
            port,
        }
    }

    /// Registers a network over the control interface; panics unless `204`.
    pub async fn register(
        &self,
        id: &str,
        source: IpAddr,
        identity: WorkloadIdentity,
        allow: Option<&[&str]>,
        limits: Option<Limits>,
    ) {
        let status = self
            .register_status(id, &format!("{source}/32"), identity, allow, limits)
            .await;
        assert_eq!(status, 204, "registering {id}");
    }

    pub async fn register_status(
        &self,
        id: &str,
        subnet: &str,
        identity: WorkloadIdentity,
        allow: Option<&[&str]>,
        limits: Option<Limits>,
    ) -> u16 {
        let reg = NetworkRegistration {
            subnet: subnet.to_string(),
            identity,
            allow: allow.map(|a| a.iter().map(|s| s.to_string()).collect()),
            limits,
        };
        let body = serde_json::to_string(&reg).unwrap();
        self.h
            .control(
                "PUT",
                &format!("/v1/networks/{id}"),
                Some(TOKEN),
                Some(body),
            )
            .await
            .0
    }

    pub async fn unregister(&self, id: &str) {
        let (status, _) = self
            .h
            .control("DELETE", &format!("/v1/networks/{id}"), Some(TOKEN), None)
            .await;
        assert_eq!(status, 204, "removing {id}");
    }

    pub async fn usage(&self) -> UsageReport {
        let (status, body) = self.h.control("GET", "/v1/usage", Some(TOKEN), None).await;
        assert_eq!(status, 200, "usage: {body}");
        serde_json::from_str(&body).unwrap()
    }

    /// Sends a raw request head from `source` and reads the answer head.
    pub async fn request(&self, source: IpAddr, head: &str) -> Outcome {
        let mut s = self
            .h
            .connect(source)
            .await
            .unwrap_or_else(|e| panic!("connect to the proxy from {source}: {e}"));
        s.write_all(head.as_bytes()).await.unwrap();
        let Some(answer) = read_head(&mut s).await else {
            return Outcome::NoAnswer;
        };
        let status: u16 = answer
            .split(' ')
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let deny = answer.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case(DENY_HEADER)
                .then(|| v.trim().to_string())
        });
        if status == 200 && deny.is_none() {
            Outcome::Tunnel(s)
        } else {
            Outcome::Refused { status, deny }
        }
    }

    /// `CONNECT host:port` from `source`.
    pub async fn connect(&self, source: IpAddr, host: &str, port: u16) -> Outcome {
        self.request(
            source,
            &format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n"),
        )
        .await
    }

    /// Waits until a log line holds every needle.
    pub async fn wait_log(&self, needles: &[&str]) -> String {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(l) = self
                .log
                .lines()
                .into_iter()
                .find(|l| needles.iter().all(|n| l.contains(n)))
            {
                return l;
            }
            assert!(
                Instant::now() < until,
                "no log line with {needles:?}: {:#?}",
                self.log.lines()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

async fn read_head(s: &mut Box<dyn Stream>) -> Option<String> {
    let mut buf = Vec::new();
    let mut b = [0u8; 1];
    loop {
        match tokio::time::timeout(Duration::from_secs(10), s.read(&mut b)).await {
            Ok(Ok(1)) => buf.push(b[0]),
            _ => return None,
        }
        if buf.ends_with(b"\r\n\r\n") {
            return Some(String::from_utf8_lossy(&buf).into_owned());
        }
        if buf.len() > 8192 {
            return None;
        }
    }
}

/// The answer of the proxy to one request.
pub enum Outcome {
    Tunnel(Box<dyn Stream>),
    Refused { status: u16, deny: Option<String> },
    NoAnswer,
}

impl Outcome {
    /// The proxy's own refusal with exactly this reason.
    #[track_caller]
    pub fn denied(self, reason: DenyReason, what: &str) {
        match self {
            Outcome::Refused { status, deny } => {
                assert_eq!(
                    deny.as_deref(),
                    Some(reason.as_str()),
                    "{what}: deny reason (status {status})"
                );
                assert_eq!(status, reason.status(), "{what}: status");
            }
            Outcome::Tunnel(_) => {
                panic!("{what}: a tunnel opened; expected deny={}", reason.as_str())
            }
            Outcome::NoAnswer => panic!("{what}: no answer; expected deny={}", reason.as_str()),
        }
    }

    #[track_caller]
    pub fn status_without_deny(self, want: u16, what: &str) {
        match self {
            Outcome::Refused { status, deny } => {
                assert_eq!(status, want, "{what}: status");
                assert_eq!(deny, None, "{what}: no deny header");
            }
            Outcome::Tunnel(_) => panic!("{what}: a tunnel opened; expected {want}"),
            Outcome::NoAnswer => panic!("{what}: no answer; expected {want}"),
        }
    }

    /// A tunnel to `up`: the greeting arrives, and a ping comes back and reached the upstream.
    pub async fn reached(self, up: &Upstream, what: &str) -> Box<dyn Stream> {
        let mut s = match self {
            Outcome::Tunnel(s) => s,
            Outcome::Refused { status, deny } => {
                panic!("{what}: refused with {status} deny={deny:?}; expected a tunnel")
            }
            Outcome::NoAnswer => panic!("{what}: no answer; expected a tunnel"),
        };
        let mut g = [0u8; 6];
        tokio::time::timeout(Duration::from_secs(5), s.read_exact(&mut g))
            .await
            .unwrap_or_else(|_| panic!("{what}: no greeting"))
            .unwrap_or_else(|e| panic!("{what}: no greeting: {e}"));
        assert_eq!(&g, GREETING, "{what}: greeting");
        let ping = format!("ping {what}\n");
        s.write_all(ping.as_bytes()).await.unwrap();
        let mut echo = vec![0u8; ping.len()];
        tokio::time::timeout(Duration::from_secs(5), s.read_exact(&mut echo))
            .await
            .unwrap_or_else(|_| panic!("{what}: no echo"))
            .unwrap_or_else(|e| panic!("{what}: no echo: {e}"));
        assert_eq!(echo, ping.as_bytes(), "{what}: echo");
        assert!(
            up.received_contains(ping.as_bytes()),
            "{what}: the upstream {} did not see the bytes",
            up.addr
        );
        s
    }
}

/// A deterministic identity per app number.
pub fn identity(app: u8) -> WorkloadIdentity {
    let id = |kind: u8| {
        uuid::Uuid::from_bytes([kind, app, 0, 0, 0, 0, 0x70, 0, 0x80, 0, 0, 0, 0, 0, 0, 1])
    };
    WorkloadIdentity {
        tenant: id(1),
        app: id(2),
        deployment: id(3),
        environment: Environment::Live,
    }
}

// ------------------------------------------------------------------------------------ cases

/// Every address rule of decision 0006 G on a resolved answer: each is refused with
/// `deny=address`, after exactly one resolution (so the refusal is the address rule's, not the
/// port's or the allow-list's).
pub async fn case_address_rules_resolved(env: &Env) {
    env.register("a", SOURCE_A, identity(1), None, None).await;
    let refused = [
        ("loopback", "127.0.0.1"),
        ("loopback other", "127.1.2.3"),
        ("private 10/8", "10.1.2.3"),
        ("private 172.16/12", "172.16.5.4"),
        ("private 172.31", "172.31.255.254"),
        ("private 192.168/16", "192.168.1.1"),
        ("CGNAT", "100.64.0.1"),
        ("CGNAT top", "100.127.255.254"),
        ("link-local", "169.254.1.1"),
        ("metadata", "169.254.169.254"),
        ("this network", "0.0.0.0"),
        ("this network 0/8", "0.1.2.3"),
        ("multicast", "224.0.0.1"),
        ("broadcast", "255.255.255.255"),
        ("reserved 240/4", "240.0.0.1"),
        ("documentation 192.0.2", "192.0.2.1"),
        ("documentation 198.51.100", "198.51.100.1"),
        ("documentation 203.0.113", "203.0.113.1"),
        ("benchmarking", "198.18.0.1"),
        ("IETF 192.0.0/24", "192.0.0.8"),
        ("IPv6 loopback", "::1"),
        ("IPv6 unspecified", "::"),
        ("IPv6 link-local", "fe80::1"),
        ("IPv6 link-local top", "febf::1"),
        ("IPv6 metadata", "fd00:ec2::254"),
        ("unique local", "fc00::1"),
        ("IPv6 multicast", "ff02::1"),
        ("Teredo", "2001::1"),
        ("IPv6 benchmarking", "2001:2::1"),
        ("IPv6 documentation", "2001:db8::1"),
        ("IPv6 documentation 3fff::/20", "3fff::1"),
        ("IPv6 documentation 3fff::/20 top", "3fff:fff:ffff::1"),
        ("6to4", "2002:a9fe:a9fe::1"),
        ("NAT64", "64:ff9b::a9fe:a9fe"),
        ("IPv4-mapped metadata", "::ffff:169.254.169.254"),
        ("IPv4-mapped loopback", "::ffff:127.0.0.1"),
        ("IPv4-compatible loopback", "::127.0.0.1"),
        ("IPv4-compatible host address", "::95.217.0.10"),
        ("host address", "95.217.0.10"),
        ("IPv4-mapped host address", "::ffff:95.217.0.10"),
        ("host IPv6 address", "2a01:4f9:c010:1234::1"),
        ("host IPv6 /64 neighbour", "2a01:4f9:c010:1234:ffff:1:2:3"),
        ("platform range", "37.27.0.9"),
        ("IPv4-mapped platform range", "::ffff:37.27.0.200"),
    ];
    for (i, (what, ip)) in refused.iter().enumerate() {
        let name = format!("r{i}.example.net");
        env.resolver.set(&name, vec![vec![ip.parse().unwrap()]]);
        env.connect(SOURCE_A, &name, 443)
            .await
            .denied(DenyReason::Address, &format!("{what} ({ip})"));
        assert_eq!(env.resolver.calls(&name), 1, "{what}: resolved once");
    }
    assert_eq!(env.public.accepted() + env.private.accepted(), 0);
}

/// IP literals and the numeric host forms are parsed like a resolver does and refused like any
/// answer, without a resolution.
pub async fn case_address_rules_literals(env: &Env) {
    env.register("a", SOURCE_A, identity(1), None, None).await;
    let refused = [
        ("loopback", "127.0.0.1"),
        ("short loopback", "127.1"),
        ("hex short loopback", "0x7f.1"),
        ("octal loopback", "017700000001"),
        ("decimal metadata", "2852039166"),
        ("octal metadata", "0251.0376.0251.0376"),
        ("hex metadata", "0xa9fea9fe"),
        ("dotted hex metadata", "0xa9.0xfe.0xa9.0xfe"),
        ("metadata", "169.254.169.254"),
        ("trailing dot", "127.0.0.1."),
        ("not a valid number", "1.2.3.256"),
        ("IPv6 loopback", "[::1]"),
        ("IPv6 metadata", "[fd00:ec2::254]"),
        ("IPv6 link-local", "[fe80::1]"),
        ("IPv4-mapped loopback", "[::ffff:127.0.0.1]"),
        ("IPv4-mapped host address", "[::ffff:95.217.0.10]"),
        ("host address", "95.217.0.10"),
        ("host IPv6 address", "[2a01:4f9:c010:1234::1]"),
        ("host IPv6 /64 neighbour", "[2a01:4f9:c010:1234:abcd::9]"),
        ("platform range", "37.27.0.1"),
        ("private", "10.0.0.1"),
    ];
    for (what, host) in refused {
        env.connect(SOURCE_A, host, 443)
            .await
            .denied(DenyReason::Address, &format!("{what} ({host})"));
    }
    assert_eq!(env.public.accepted() + env.private.accepted(), 0);
}

/// An IPv4-mapped answer of a public address is connected to as its IPv4 form.
pub async fn case_mapped_public_answer(env: &Env) {
    env.register("a", SOURCE_A, identity(1), None, None).await;
    let mapped = match PUBLIC_IP {
        IpAddr::V4(v4) => IpAddr::V6(v4.to_ipv6_mapped()),
        v6 => v6,
    };
    env.resolver.set("mapped.example.net", vec![vec![mapped]]);
    env.connect(SOURCE_A, "mapped.example.net", env.port)
        .await
        .reached(&env.public, "mapped public answer")
        .await;
    env.wait_log(&[
        "host=mapped.example.net",
        "status=200",
        &format!("resolved={}", env.public.addr),
    ])
    .await;
}

/// A name whose answers mix a public and a refused address is refused, in either order.
pub async fn case_mixed_answers(env: &Env) {
    env.register("a", SOURCE_A, identity(1), None, None).await;
    let mixes: [(&str, Vec<IpAddr>); 4] = [
        (
            "public then private",
            vec![PUBLIC_IP, "10.0.0.1".parse().unwrap()],
        ),
        (
            "private then public",
            vec!["10.0.0.1".parse().unwrap(), PUBLIC_IP],
        ),
        (
            "public then metadata",
            vec![PUBLIC_IP, "fd00:ec2::254".parse().unwrap()],
        ),
        (
            "public then mapped host",
            vec![PUBLIC_IP, "::ffff:95.217.0.10".parse().unwrap()],
        ),
    ];
    for (i, (what, answer)) in mixes.into_iter().enumerate() {
        let name = format!("mix{i}.example.net");
        env.resolver.set(&name, vec![answer]);
        env.connect(SOURCE_A, &name, env.port)
            .await
            .denied(DenyReason::Address, what);
    }
    assert_eq!(
        env.public.accepted(),
        0,
        "a mixed answer opened no connection"
    );
}

/// DNS rebinding: the resolver answers public at the check and private afterwards. The tunnel
/// reaches the public upstream, never the private one, and the name is resolved once.
pub async fn case_rebinding(env: &Env) {
    env.register("a", SOURCE_A, identity(1), None, None).await;
    env.resolver.set(
        "rebind.example.net",
        vec![vec![PUBLIC_IP], vec![PRIVATE_IP]],
    );
    let s = env
        .connect(SOURCE_A, "rebind.example.net", env.port)
        .await
        .reached(&env.public, "rebinding")
        .await;
    assert_eq!(
        env.resolver.calls("rebind.example.net"),
        1,
        "one resolution per tunnel"
    );
    assert_eq!(
        env.private.accepted(),
        0,
        "the private upstream was never connected"
    );
    assert_eq!(env.private.received_len(), 0);
    drop(s);
    // The next request resolves again and gets the private answer: refused.
    env.connect(SOURCE_A, "rebind.example.net", env.port)
        .await
        .denied(DenyReason::Address, "the later private answer");
    assert_eq!(env.private.accepted(), 0);
}

/// `egress.allow`: exact, wildcard depth, apex, case, trailing dot, label boundary; the address
/// rules still apply after it.
pub async fn case_allow_list(env: &Env) {
    env.register(
        "a",
        SOURCE_A,
        identity(1),
        Some(&["api.example.com", "*.example.org"]),
        None,
    )
    .await;
    for name in [
        "api.example.com",
        "x.example.org",
        "a.b.example.org",
        "example.org",
        "evilexample.org",
        "example.org.evil.net",
        "other.example.com",
        "private.example.org",
    ] {
        env.resolver.set(name, vec![vec![PUBLIC_IP]]);
    }
    env.resolver.set(
        "private.example.org",
        vec![vec!["10.0.0.7".parse().unwrap()]],
    );
    for (host, what) in [
        ("api.example.com", "exact"),
        ("API.Example.COM", "case"),
        ("api.example.com.", "trailing dot"),
        ("x.example.org", "wildcard, one label"),
        ("a.b.example.org", "wildcard, two labels"),
        ("X.EXAMPLE.ORG.", "wildcard, case and trailing dot"),
    ] {
        drop(
            env.connect(SOURCE_A, host, env.port)
                .await
                .reached(&env.public, what)
                .await,
        );
    }
    for (host, what) in [
        ("example.org", "apex not matched by *."),
        ("evilexample.org", "suffix without a label boundary"),
        ("example.org.evil.net", "allowed name as a prefix"),
        ("other.example.com", "a sibling of an exact entry"),
        ("x.api.example.com", "below an exact entry"),
        ("2852039166", "a numeric host never matches a name"),
        ("[2606:4700::1111]", "an IP literal never matches a name"),
    ] {
        env.connect(SOURCE_A, host, env.port)
            .await
            .denied(DenyReason::Allow, what);
    }
    assert_eq!(
        env.resolver.calls("example.org"),
        0,
        "a refused name is never resolved"
    );
    assert_eq!(env.resolver.calls("evilexample.org"), 0);
    env.connect(SOURCE_A, "private.example.org", env.port)
        .await
        .denied(DenyReason::Address, "allowed name with a private answer");

    // An empty list allows no host; a missing list allows every host.
    env.register("b", SOURCE_B, identity(2), Some(&[]), None)
        .await;
    env.connect(SOURCE_B, "api.example.com", env.port)
        .await
        .denied(DenyReason::Allow, "empty list");
    env.register("b", SOURCE_B, identity(2), None, None).await;
    drop(
        env.connect(SOURCE_B, "evilexample.org", env.port)
            .await
            .reached(&env.public, "no list")
            .await,
    );
}

/// Registrations with invalid entries, too many entries or a bad subnet are refused.
pub async fn case_registration_checks(env: &Env) {
    let too_many: Vec<String> = (0..51).map(|i| format!("h{i}.example.com")).collect();
    let too_many: Vec<&str> = too_many.iter().map(String::as_str).collect();
    for (allow, what) in [
        (vec!["example..com"], "empty label"),
        (vec!["*.com"], "wildcard of a top-level domain"),
        (vec!["1.2.3.4"], "an IP address"),
        (vec!["a.*.example.com"], "inner wildcard"),
        (too_many, "51 entries"),
    ] {
        let status = env
            .register_status("x", "127.0.0.2/32", identity(1), Some(&allow), None)
            .await;
        assert_eq!(status, 400, "{what}");
    }
    for subnet in ["nope", "127.0.0.0/7", "10.0.0.0/33", "::/16"] {
        let status = env
            .register_status("x", subnet, identity(1), None, None)
            .await;
        assert_eq!(status, 400, "subnet {subnet}");
    }
    let (status, _) = env
        .h
        .control(
            "PUT",
            "/v1/networks/bad%20id",
            Some(TOKEN),
            Some("{}".into()),
        )
        .await;
    assert_eq!(status, 400, "a bad network id");
    let (status, _) = env
        .h
        .control(
            "PUT",
            "/v1/networks/x",
            Some(TOKEN),
            Some(r#"{"subnet":"127.0.0.2/32","identity":{"tenant":"x"}}"#.into()),
        )
        .await;
    assert_eq!(status, 400, "a bad identity");
    // Nothing was registered by the refused requests.
    env.connect(SOURCE_A, "api.example.com", env.port)
        .await
        .denied(DenyReason::UnknownSource, "after refused registrations");
}

/// A port outside `RUUM_EGRESS_PORTS` is refused before any resolution.
pub async fn case_port(env: &Env) {
    env.register("a", SOURCE_A, identity(1), None, None).await;
    env.resolver.set("ssh.example.net", vec![vec![PUBLIC_IP]]);
    for port in [22u16, 80, 25, 6379] {
        env.connect(SOURCE_A, "ssh.example.net", port)
            .await
            .denied(DenyReason::Port, &format!("port {port}"));
    }
    assert_eq!(env.resolver.calls("ssh.example.net"), 0);
}

/// A name that does not resolve is a `502` without the deny header.
pub async fn case_resolve_failure(env: &Env) {
    env.register("a", SOURCE_A, identity(1), None, None).await;
    env.connect(SOURCE_A, "nowhere.example.net", env.port)
        .await
        .status_without_deny(502, "a name that does not resolve");
    env.wait_log(&["host=nowhere.example.net", "status=502"])
        .await;
}

/// Only `CONNECT`: an absolute-form request and a control request on the proxy port get `405`.
pub async fn case_method(env: &Env) {
    env.register("a", SOURCE_A, identity(1), None, None).await;
    env.request(
        SOURCE_A,
        "GET http://api.example.com/private/path?token=abc HTTP/1.1\r\nHost: api.example.com\r\n\r\n",
    )
    .await
    .denied(DenyReason::Method, "absolute-form GET");
    // The control interface is not on the proxy port: a valid control request is a refused
    // method there, and registers nothing.
    let body = serde_json::to_string(&NetworkRegistration {
        subnet: "127.0.0.4/32".into(),
        identity: identity(9),
        allow: None,
        limits: None,
    })
    .unwrap();
    env.request(
        SOURCE_A,
        &format!(
            "PUT /v1/networks/evil HTTP/1.1\r\nHost: egress\r\nAuthorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    )
    .await
    .denied(DenyReason::Method, "control request on the proxy port");
    env.connect("127.0.0.4".parse().unwrap(), "api.example.com", env.port)
        .await
        .denied(
            DenyReason::UnknownSource,
            "the subnet of the refused registration",
        );
    for (head, what) in [
        ("CONNECT api.example.com HTTP/1.1\r\n\r\n", "no port"),
        ("CONNECT api.example.com:0 HTTP/1.1\r\n\r\n", "port 0"),
        (
            "CONNECT user@api.example.com:443 HTTP/1.1\r\n\r\n",
            "userinfo",
        ),
        ("CONNECT api.example.com:443 HTTP/2.0\r\n\r\n", "HTTP/2"),
    ] {
        env.request(SOURCE_A, head)
            .await
            .status_without_deny(400, what);
    }
}

/// The app is the network the connection came from, never a header: two registrations, two
/// subnets, each source gets its own identity and rules; an unknown source and a removed
/// network get `unknown_source`.
pub async fn case_identity_by_network(env: &Env) {
    env.resolver.set("a.example.com", vec![vec![PUBLIC_IP]]);
    env.resolver.set("b.example.com", vec![vec![PUBLIC_IP]]);
    env.connect(SOURCE_UNKNOWN, "a.example.com", env.port)
        .await
        .denied(DenyReason::UnknownSource, "no registration yet");
    env.register("a", SOURCE_A, identity(1), Some(&["a.example.com"]), None)
        .await;
    env.register("b", SOURCE_B, identity(2), Some(&["b.example.com"]), None)
        .await;
    drop(
        env.connect(SOURCE_A, "a.example.com", env.port)
            .await
            .reached(&env.public, "A to A's host")
            .await,
    );
    env.connect(SOURCE_A, "b.example.com", env.port)
        .await
        .denied(DenyReason::Allow, "A to B's host");
    drop(
        env.connect(SOURCE_B, "b.example.com", env.port)
            .await
            .reached(&env.public, "B to B's host")
            .await,
    );
    env.connect(SOURCE_B, "a.example.com", env.port)
        .await
        .denied(DenyReason::Allow, "B to A's host");
    env.connect(SOURCE_UNKNOWN, "a.example.com", env.port)
        .await
        .denied(DenyReason::UnknownSource, "unknown source");

    // Headers the app controls never change the identity.
    let spoof = |host: &str| {
        format!(
            "CONNECT {host}:{p} HTTP/1.1\r\nHost: {host}:{p}\r\nX-Forwarded-For: 127.0.0.2\r\nForwarded: for=127.0.0.2\r\nX-Real-IP: 127.0.0.2\r\nProxy-Authorization: Basic YTpi\r\n\r\n",
            p = env.port
        )
    };
    env.request(SOURCE_UNKNOWN, &spoof("a.example.com"))
        .await
        .denied(
            DenyReason::UnknownSource,
            "unknown source naming A in headers",
        );
    env.request(SOURCE_B, &spoof("a.example.com"))
        .await
        .denied(DenyReason::Allow, "B naming A in headers keeps B's rules");
    env.wait_log(&[
        "source=127.0.0.3",
        &identity(2).app.to_string(),
        "deny=allow",
    ])
    .await;

    // Overlapping subnets cannot be registered.
    let status = env
        .register_status("c", "127.0.0.0/30", identity(3), None, None)
        .await;
    assert_eq!(status, 409, "a subnet overlapping A and B");

    // After DELETE the source is unknown.
    env.unregister("a").await;
    env.connect(SOURCE_A, "a.example.com", env.port)
        .await
        .denied(DenyReason::UnknownSource, "A after DELETE");
    drop(
        env.connect(SOURCE_B, "b.example.com", env.port)
            .await
            .reached(&env.public, "B after A's DELETE")
            .await,
    );
}

/// At most N tunnels at once per identity; a slot comes back when a tunnel ends.
pub async fn case_limit_concurrency(env: &Env) {
    let limits = Limits {
        max_tunnels: Some(1),
        ..Limits::default()
    };
    env.register("a", SOURCE_A, identity(1), None, Some(limits))
        .await;
    env.register("b", SOURCE_B, identity(2), None, Some(limits))
        .await;
    env.resolver.set("c.example.net", vec![vec![PUBLIC_IP]]);
    let first = env
        .connect(SOURCE_A, "c.example.net", env.port)
        .await
        .reached(&env.public, "first tunnel")
        .await;
    env.connect(SOURCE_A, "c.example.net", env.port)
        .await
        .denied(DenyReason::Limit, "second tunnel at once");
    // Another identity has its own limit.
    drop(
        env.connect(SOURCE_B, "c.example.net", env.port)
            .await
            .reached(&env.public, "B beside A")
            .await,
    );
    drop(first);
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        match env.connect(SOURCE_A, "c.example.net", env.port).await {
            Outcome::Tunnel(_) => break,
            Outcome::Refused { deny: Some(d), .. } if d == "limit" && Instant::now() < until => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Outcome::Refused { status, deny } => panic!("after the first ended: {status} {deny:?}"),
            Outcome::NoAnswer => panic!("after the first ended: no answer"),
        }
    }
}

/// A token bucket per identity: a burst, then `limit`.
pub async fn case_limit_rate(env: &Env) {
    let limits = Limits {
        burst: Some(2),
        per_minute: Some(1),
        ..Limits::default()
    };
    env.register("a", SOURCE_A, identity(1), None, Some(limits))
        .await;
    env.resolver.set("r.example.net", vec![vec![PUBLIC_IP]]);
    for i in 0..2 {
        drop(
            env.connect(SOURCE_A, "r.example.net", env.port)
                .await
                .reached(&env.public, &format!("burst {i}"))
                .await,
        );
    }
    env.connect(SOURCE_A, "r.example.net", env.port)
        .await
        .denied(DenyReason::Limit, "over the rate");
    env.wait_log(&["host=r.example.net", "deny=limit"]).await;
}

/// A tunnel without bytes closes after the idle timeout.
pub async fn case_limit_idle(env: &Env) {
    let limits = Limits {
        idle_timeout_secs: Some(1),
        ..Limits::default()
    };
    env.register("a", SOURCE_A, identity(1), None, Some(limits))
        .await;
    env.resolver.set("i.example.net", vec![vec![PUBLIC_IP]]);
    let mut s = env
        .connect(SOURCE_A, "i.example.net", env.port)
        .await
        .reached(&env.public, "idle tunnel")
        .await;
    let start = Instant::now();
    let mut b = [0u8; 16];
    let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut b))
        .await
        .expect("the idle tunnel must close")
        .unwrap_or(0);
    assert_eq!(n, 0, "no bytes, then the end");
    let took = start.elapsed();
    assert!(
        took >= Duration::from_millis(700),
        "closed after {took:?}, before the idle timeout"
    );
}

/// A busy tunnel closes at its maximum lifetime.
pub async fn case_limit_lifetime(env: &Env) {
    let limits = Limits {
        max_lifetime_secs: Some(1),
        ..Limits::default()
    };
    env.register("a", SOURCE_A, identity(1), None, Some(limits))
        .await;
    env.resolver.set("l.example.net", vec![vec![PUBLIC_IP]]);
    let start = Instant::now();
    let mut s = env
        .connect(SOURCE_A, "l.example.net", env.port)
        .await
        .reached(&env.public, "busy tunnel")
        .await;
    let mut b = [0u8; 2];
    loop {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "the tunnel outlived its lifetime"
        );
        if s.write_all(b"k\n").await.is_err() {
            break;
        }
        match tokio::time::timeout(Duration::from_secs(2), s.read_exact(&mut b)).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => break,
            Err(_) => panic!("the busy tunnel stalled"),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let took = start.elapsed();
    assert!(
        took >= Duration::from_millis(700),
        "closed after {took:?}, before its lifetime"
    );
}

/// Bytes per identity and hour, drained once.
pub async fn case_counters(env: &Env) {
    env.register("a", SOURCE_A, identity(1), None, None).await;
    env.register("b", SOURCE_B, identity(2), None, None).await;
    env.resolver.set("count.example.net", vec![vec![PUBLIC_IP]]);
    let hour = |t: time::OffsetDateTime| t.unix_timestamp() - t.unix_timestamp().rem_euclid(3600);
    let before = hour(time::OffsetDateTime::now_utc());
    // One refusal of A.
    env.connect(SOURCE_A, "count.example.net", 22)
        .await
        .denied(DenyReason::Port, "a refusal to count");
    // A sends 1000 bytes and gets the greeting and 1000 back; B sends 10.
    for (src, n) in [(SOURCE_A, 1000usize), (SOURCE_B, 10)] {
        let Outcome::Tunnel(mut s) = env.connect(src, "count.example.net", env.port).await else {
            panic!("a tunnel for the counters");
        };
        let mut g = [0u8; 6];
        s.read_exact(&mut g).await.unwrap();
        s.write_all(&vec![b'x'; n]).await.unwrap();
        let mut back = vec![0u8; n];
        s.read_exact(&mut back).await.unwrap();
        s.shutdown().await.unwrap();
        let mut rest = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut rest)).await;
        env.wait_log(&[
            &format!("source={src}"),
            "status=200",
            &format!("bytes_up={n} "),
        ])
        .await;
    }
    let after = hour(time::OffsetDateTime::now_utc());
    let report = env.usage().await;
    assert_eq!(report.overflow.bytes_up + report.overflow.bytes_down, 0);
    let sum = |id: WorkloadIdentity| {
        report
            .entries
            .iter()
            .filter(|e| e.identity == id)
            .fold((0, 0, 0, 0), |acc, e| {
                let h = e.hour.unix_timestamp();
                assert!(
                    h == before || h == after,
                    "the hour {} is the current one",
                    e.hour
                );
                assert_eq!(h.rem_euclid(3600), 0, "an hour starts at :00");
                (
                    acc.0 + e.bytes_up,
                    acc.1 + e.bytes_down,
                    acc.2 + e.tunnels,
                    acc.3 + e.denied,
                )
            })
    };
    assert_eq!(
        sum(identity(1)),
        (1000, 1006, 1, 1),
        "A: up, down, tunnels, denied"
    );
    assert_eq!(
        sum(identity(2)),
        (10, 16, 1, 0),
        "B: up, down, tunnels, denied"
    );
    assert_eq!(
        report.entries.len(),
        if before == after {
            2
        } else {
            report.entries.len()
        }
    );
    let again = env.usage().await;
    assert!(again.entries.is_empty(), "drained once: {again:?}");
}

/// The log holds no path, query string or header: for a tunnel with headers and for a refused
/// absolute-form request. Each request has its line.
pub async fn case_logs(env: &Env) {
    env.register("a", SOURCE_A, identity(1), None, None).await;
    env.resolver.set("logs.example.net", vec![vec![PUBLIC_IP]]);
    let p = env.port;
    let o = env
        .request(
            SOURCE_A,
            &format!(
                "CONNECT logs.example.net:{p} HTTP/1.1\r\nHost: logs.example.net:{p}\r\nProxy-Authorization: Basic c2VjcmV0LXZhbHVl\r\nX-Secret-Header: hunter2\r\nUser-Agent: npm/10\r\n\r\n"
            ),
        )
        .await;
    drop(o.reached(&env.public, "tunnel with headers").await);
    env.request(
        SOURCE_A,
        "GET http://logs.example.net/private/path?token=abc HTTP/1.1\r\nHost: logs.example.net\r\nCookie: session=s3cr3t\r\n\r\n",
    )
    .await
    .denied(DenyReason::Method, "absolute-form request");
    env.request(
        SOURCE_A,
        "POST https://logs.example.net/upload?key=abc HTTP/1.1\r\nHost: logs.example.net\r\n\r\n",
    )
    .await
    .denied(DenyReason::Method, "absolute-form POST");
    let tunnel = env.wait_log(&["host=logs.example.net", "status=200"]).await;
    assert!(
        tunnel.contains(&format!("port={p}")) && tunnel.contains("bytes_up="),
        "{tunnel}"
    );
    env.wait_log(&["status=405", "deny=method", "host=-"]).await;
    let all = env.log.lines().join("\n");
    for secret in [
        "/private",
        "path?",
        "token=abc",
        "key=abc",
        "/upload",
        "s3cr3t",
        "Cookie",
        "c2VjcmV0LXZhbHVl",
        "hunter2",
        "Proxy-Authorization",
        "X-Secret-Header",
        "User-Agent",
        "npm/10",
        "http://",
        "https://",
        TOKEN,
    ] {
        assert!(!all.contains(secret), "the log holds `{secret}`:\n{all}");
    }
}

/// The control interface: no token or a wrong one is refused, the right one works.
pub async fn case_control_auth(env: &Env) {
    let body = serde_json::to_string(&NetworkRegistration {
        subnet: "127.0.0.2/32".into(),
        identity: identity(1),
        allow: None,
        limits: None,
    })
    .unwrap();
    let wrong_long = format!("{TOKEN}x");
    let wrong_short = &TOKEN[..TOKEN.len() - 1];
    for (token, what) in [
        (None, "no token"),
        (Some("wrong"), "a wrong token"),
        (Some(wrong_long.as_str()), "the token with a byte more"),
        (Some(wrong_short), "the token with a byte less"),
    ] {
        for (method, path, b) in [
            ("PUT", "/v1/networks/a", Some(body.clone())),
            ("DELETE", "/v1/networks/a", None),
            ("GET", "/v1/usage", None),
        ] {
            let (status, text) = env.h.control(method, path, token, b).await;
            assert_eq!(status, 401, "{what}: {method} {path}");
            assert!(text.contains("unauthorized"), "{what}: {text}");
        }
    }
    env.connect(SOURCE_A, "x.example.net", env.port)
        .await
        .denied(
            DenyReason::UnknownSource,
            "nothing registered without the token",
        );
    let (status, _) = env
        .h
        .control("PUT", "/v1/networks/a", Some(TOKEN), Some(body))
        .await;
    assert_eq!(status, 204);
    let (status, _) = env.h.control("GET", "/v1/nothing", Some(TOKEN), None).await;
    assert_eq!(status, 404);
}

/// The start-up checks: a bad deny entry, an empty deny list outside dev mode, a control address
/// on a served network, a missing or short token. Errors never hold the token.
pub fn case_startup_checks() {
    let dir = TempDir::new();
    let token_file = dir.path.join("token");
    std::fs::write(&token_file, format!("{TOKEN}\n")).unwrap();
    let short_file = dir.path.join("short");
    std::fs::write(&short_file, "short").unwrap();
    let base = |over: &[(&str, &str)]| {
        let mut v: Vec<(String, String)> = vec![
            ("RUUM_EGRESS_LISTEN".into(), "0.0.0.0:3128".into()),
            (
                "RUUM_EGRESS_CONTROL".into(),
                "unix:/run/egress/control.sock".into(),
            ),
            (
                "RUUM_EGRESS_CONTROL_TOKEN_FILE".into(),
                token_file.display().to_string(),
            ),
            ("EGRESS_DENY".into(), TEST_DENY.into()),
        ];
        for (k, val) in over {
            v.retain(|(n, _)| n != k);
            v.push((k.to_string(), val.to_string()));
        }
        Config::from_vars(move |name| v.iter().find(|(n, _)| n == name).map(|(_, x)| x.clone()))
    };
    let ok = base(&[]).expect("the base configuration starts");
    assert_eq!(ok.deny.entries().len(), 3);
    assert_eq!(ok.ports, vec![443]);
    assert!(!ok.dev);
    assert!(
        !format!("{ok:?}").contains(TOKEN),
        "Debug of the config holds the token"
    );

    for deny in [
        "95.217.0.10, 300.1.1.1",
        "95.217.0.10,,37.27.0.0/24",
        "nope",
        "10.0.0.0/40",
    ] {
        match base(&[("EGRESS_DENY", deny)]) {
            Err(ConfigError::BadDeny(m)) => assert!(m.contains("EGRESS_DENY"), "{m}"),
            other => panic!("EGRESS_DENY={deny}: {other:?}"),
        }
    }
    assert!(matches!(
        base(&[("EGRESS_DENY", "")]),
        Err(ConfigError::EmptyDeny)
    ));
    assert!(matches!(
        base(&[("EGRESS_DENY", " ")]),
        Err(ConfigError::EmptyDeny)
    ));
    let dev = base(&[("EGRESS_DENY", ""), ("RUUM_EGRESS_DEV", "1")]).expect("dev mode starts");
    assert!(dev.dev && dev.deny.is_empty());
    assert!(matches!(
        base(&[("EGRESS_DENY", ""), ("RUUM_EGRESS_DEV", "true")]),
        Err(ConfigError::EmptyDeny)
    ));

    for (listen, control) in [
        ("0.0.0.0:3128", "127.0.0.1:9000"),
        ("[::]:3128", "[::1]:9000"),
        ("127.0.0.1:3128", "127.0.0.1:9000"),
        ("10.0.0.2:3128", "10.0.0.2:9000"),
        ("10.0.0.2:3128", "10.0.0.3:9000"),
        ("10.0.0.2:3128", "0.0.0.0:9000"),
    ] {
        match base(&[
            ("RUUM_EGRESS_LISTEN", listen),
            ("RUUM_EGRESS_CONTROL", control),
        ]) {
            Err(ConfigError::ControlOnServedNetwork { .. }) => {}
            other => panic!("listen {listen}, control {control}: {other:?}"),
        }
    }
    base(&[
        ("RUUM_EGRESS_LISTEN", "10.0.0.2:3128"),
        ("RUUM_EGRESS_CONTROL", "127.0.0.1:9000"),
    ])
    .expect("loopback control beside a specific proxy address");

    let short = short_file.display().to_string();
    let missing = dir.path.join("missing").display().to_string();
    for (k, v) in [
        ("RUUM_EGRESS_CONTROL_TOKEN_FILE", short.as_str()),
        ("RUUM_EGRESS_CONTROL_TOKEN_FILE", missing.as_str()),
        ("RUUM_EGRESS_CONTROL_TOKEN_FILE", ""),
        ("RUUM_EGRESS_CONTROL", ""),
        ("RUUM_EGRESS_CONTROL", "unix:"),
        ("RUUM_EGRESS_PORTS", "443,0"),
        ("RUUM_EGRESS_PORTS", "443,https"),
        ("RUUM_EGRESS_MAX_TUNNELS", "0"),
        ("RUUM_EGRESS_LISTEN", "nope"),
    ] {
        let r = base(&[(k, v)]);
        let text = format!("{r:?}");
        assert!(r.is_err(), "{k}={v} must stop the start");
        assert!(!text.contains(TOKEN));
    }
}

// ---------------------------------------------------------------------------------- targets

/// A temporary folder, removed on drop.
pub struct TempDir {
    pub path: std::path::PathBuf,
}

impl TempDir {
    pub fn new() -> TempDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "ruum-egress-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }
}

impl Default for TempDir {
    fn default() -> Self {
        TempDir::new()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The in-memory target: [`MemoryPolicy`], the proxy handler on in-memory streams and the control
/// router called directly.
pub struct MemoryHarness {
    state: Arc<ProxyState>,
    router: axum::Router,
    next_port: AtomicU16,
}

pub async fn memory_harness(setup: Setup) -> Box<dyn Harness> {
    let policy = Arc::new(MemoryPolicy::new(
        DenyList::parse(TEST_DENY).unwrap(),
        setup.ports,
        // A large burst, so the cases that make many requests are not limited; the limit
        // cases lower it through their registration.
        LimitConfig {
            burst: 1000,
            ..LimitConfig::default()
        },
        64,
    ));
    policy.set_test_public(setup.test_public);
    let usage = Arc::new(Usage::new());
    let router = router(ControlState {
        policy: policy.clone(),
        usage: usage.clone(),
        token: ControlToken::new(TOKEN).unwrap(),
    });
    let state = Arc::new(ProxyState {
        policy,
        resolver: setup.resolver,
        usage,
        log: setup.log,
        connect_timeout: Duration::from_secs(2),
        head_timeout: Duration::from_secs(5),
    });
    Box::new(MemoryHarness {
        state,
        router,
        next_port: AtomicU16::new(40000),
    })
}

#[async_trait]
impl Harness for MemoryHarness {
    async fn control(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<String>,
    ) -> (u16, String) {
        let mut b = axum::http::Request::builder().method(method).uri(path);
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        if body.is_some() {
            b = b.header("content-type", "application/json");
        }
        let req = b
            .body(axum::body::Body::from(body.unwrap_or_default()))
            .unwrap();
        let resp = self.router.clone().oneshot(req).await.unwrap();
        let status = resp.status().as_u16();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn connect(&self, source: IpAddr) -> io::Result<Box<dyn Stream>> {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let peer = SocketAddr::new(source, self.next_port.fetch_add(1, Ordering::SeqCst));
        let state = self.state.clone();
        tokio::spawn(async move { handle(&state, peer, server).await });
        Ok(Box::new(client))
    }
}

/// The running proxy: [`crate::start`] with the binary's configuration parser, a proxy listener
/// on `127.0.0.1` and the control listener on a Unix socket, reached over real sockets.
pub struct BinaryHarness {
    running: Running,
    socket: std::path::PathBuf,
    _dir: TempDir,
}

pub async fn binary_harness(setup: Setup) -> Box<dyn Harness> {
    let dir = TempDir::new();
    let token_file = dir.path.join("token");
    std::fs::write(&token_file, format!("{TOKEN}\n")).unwrap();
    let socket = dir.path.join("control.sock");
    let ports = setup
        .ports
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let vars: Vec<(&str, String)> = vec![
        ("RUUM_EGRESS_LISTEN", "127.0.0.1:0".into()),
        ("RUUM_EGRESS_CONTROL", format!("unix:{}", socket.display())),
        (
            "RUUM_EGRESS_CONTROL_TOKEN_FILE",
            token_file.display().to_string(),
        ),
        ("RUUM_EGRESS_PORTS", ports),
        ("EGRESS_DENY", TEST_DENY.into()),
        ("RUUM_EGRESS_CONNECT_TIMEOUT_SECS", "2".into()),
        ("RUUM_EGRESS_MAX_NETWORKS", "64".into()),
        ("RUUM_EGRESS_BURST", "1000".into()),
    ];
    let config = Config::from_vars(|n| vars.iter().find(|(k, _)| *k == n).map(|(_, v)| v.clone()))
        .expect("the suite's configuration");
    let running = start(
        config,
        Options {
            resolver: setup.resolver,
            log: setup.log,
            test_public: Some(setup.test_public),
        },
    )
    .await
    .expect("the proxy starts");
    Box::new(BinaryHarness {
        running,
        socket,
        _dir: dir,
    })
}

impl Drop for BinaryHarness {
    fn drop(&mut self) {
        self.running.abort();
    }
}

/// One HTTP/1.1 request over a stream (`Connection: close`); returns the status and the body.
pub async fn http_request<S: AsyncRead + AsyncWrite + Unpin>(
    mut s: S,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<String>,
) -> (u16, String) {
    let body = body.unwrap_or_default();
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: egress\r\nConnection: close\r\n");
    if let Some(t) = token {
        head.push_str(&format!("Authorization: Bearer {t}\r\n"));
    }
    if !body.is_empty() {
        head.push_str("Content-Type: application/json\r\n");
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    head.push_str(&body);
    // A refused request may be answered and closed before the body is read: write errors and a
    // reset after the answer are not failures; the status decides.
    let _ = s.write_all(head.as_bytes()).await;
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut out))
        .await
        .expect("the control answer");
    let text = String::from_utf8_lossy(&out).into_owned();
    let status = text
        .split(' ')
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (status, body)
}

#[async_trait]
impl Harness for BinaryHarness {
    async fn control(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<String>,
    ) -> (u16, String) {
        let s = tokio::net::UnixStream::connect(&self.socket).await.unwrap();
        http_request(s, method, path, token, body).await
    }

    async fn connect(&self, source: IpAddr) -> io::Result<Box<dyn Stream>> {
        let sock = tokio::net::TcpSocket::new_v4()?;
        sock.bind(SocketAddr::new(source, 0))?;
        let s = sock.connect(self.running.proxy_addr).await?;
        Ok(Box::new(s))
    }
}

/// One `#[tokio::test]` per case against the target that `$make` builds.
#[macro_export]
macro_rules! conformance_tests {
    ($make:path) => {
        $crate::conformance_tests!(@cases $make;
            address_rules_resolved => case_address_rules_resolved,
            address_rules_literals => case_address_rules_literals,
            mapped_public_answer => case_mapped_public_answer,
            mixed_answers => case_mixed_answers,
            rebinding => case_rebinding,
            allow_list => case_allow_list,
            registration_checks => case_registration_checks,
            port => case_port,
            resolve_failure => case_resolve_failure,
            method => case_method,
            identity_by_network => case_identity_by_network,
            limit_concurrency => case_limit_concurrency,
            limit_rate => case_limit_rate,
            limit_idle => case_limit_idle,
            limit_lifetime => case_limit_lifetime,
            counters => case_counters,
            logs => case_logs,
            control_auth => case_control_auth,
        );
    };
    (@cases $make:path; $($name:ident => $case:ident),* $(,)?) => {
        $(
            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn $name() {
                let env = $crate::conformance::Env::new($make).await;
                $crate::conformance::$case(&env).await;
            }
        )*
    };
}
