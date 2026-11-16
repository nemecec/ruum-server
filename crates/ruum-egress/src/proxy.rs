//! The proxy listener: `CONNECT host:port` only, the checks in order, the connect to the checked
//! address, the relay with its limits, the byte counters and one log line per request.

use std::fmt;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::addr::{BadHost, Host, canonical, parse_host};
use crate::api::{DENY_HEADER, DenyReason, WorkloadIdentity};
use crate::limits::ConnectionPermit;
use crate::policy::{Decision, EgressPolicy, Identified};
use crate::resolver::Resolver;
use crate::usage::Usage;

/// The largest request head the proxy reads.
pub const MAX_HEAD: usize = 8 * 1024;

/// Client connections at once from sources in no registered network (all of them together). In
/// normal operation there are none; the cap keeps them from taking the global connection cap.
pub const MAX_UNKNOWN_CONNECTIONS: usize = 64;

/// Everything a request needs.
pub struct ProxyState {
    pub policy: Arc<dyn EgressPolicy>,
    pub resolver: Arc<dyn Resolver>,
    pub usage: Arc<Usage>,
    pub log: Arc<dyn LogSink>,
    /// Resolving plus connecting to the upstream.
    pub connect_timeout: Duration,
    /// The request head must arrive within this time.
    pub head_timeout: Duration,
    /// The slots of [`MAX_UNKNOWN_CONNECTIONS`] (see [`ProxyState::unknown_slots`]).
    pub unknown: Arc<Semaphore>,
}

impl ProxyState {
    /// The slots for connections from unknown sources.
    pub fn unknown_slots() -> Arc<Semaphore> {
        Arc::new(Semaphore::new(MAX_UNKNOWN_CONNECTIONS))
    }
}

/// One line per request. It holds no path, query string, header or body.
#[derive(Clone, Debug)]
pub struct TunnelLog {
    pub source: IpAddr,
    pub network: Option<Arc<str>>,
    pub identity: Option<WorkloadIdentity>,
    /// The checked host name or IP literal; `-` when the request named none (or not validly).
    pub host: String,
    /// `0` when the request named none.
    pub port: u16,
    pub resolved: Option<SocketAddr>,
    pub status: u16,
    pub deny: Option<DenyReason>,
    pub bytes_up: u64,
    pub bytes_down: u64,
    pub duration: Duration,
}

impl fmt::Display for TunnelLog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "egress source={}", self.source)?;
        match (&self.network, &self.identity) {
            (Some(n), Some(i)) => write!(
                f,
                " network={n} tenant={} app={} deployment={} environment={}",
                i.tenant,
                i.app,
                i.deployment,
                i.environment.as_str()
            )?,
            _ => f.write_str(" network=-")?,
        }
        write!(f, " host={} port={}", self.host, self.port)?;
        match self.resolved {
            Some(a) => write!(f, " resolved={a}")?,
            None => f.write_str(" resolved=-")?,
        }
        write!(f, " status={}", self.status)?;
        if let Some(d) = self.deny {
            write!(f, " deny={}", d.as_str())?;
        }
        write!(
            f,
            " bytes_up={} bytes_down={} duration_ms={}",
            self.bytes_up,
            self.bytes_down,
            self.duration.as_millis()
        )
    }
}

pub trait LogSink: Send + Sync {
    fn tunnel(&self, rec: &TunnelLog);
}

/// The log of the binary: `tracing` at `info`, target `ruum_egress::tunnel`.
pub struct TracingLog;

impl LogSink for TracingLog {
    fn tunnel(&self, rec: &TunnelLog) {
        tracing::info!(target: "ruum_egress::tunnel", "{rec}");
    }
}

/// A log kept in memory (tests). It also writes to `tracing`.
#[derive(Default)]
pub struct MemoryLog {
    lines: std::sync::Mutex<Vec<String>>,
}

impl MemoryLog {
    pub fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

impl LogSink for MemoryLog {
    fn tunnel(&self, rec: &TunnelLog) {
        TracingLog.tunnel(rec);
        self.lines
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(rec.to_string());
    }
}

/// Accepts connections until the listener fails. At most `max_connections` at once over all
/// sources; a connection above it is closed at once. Each accepted connection is admitted
/// (identified by its source and counted against its identity's connection cap, see [`admit`])
/// in this loop, before its request head is read.
pub async fn serve(listener: TcpListener, state: Arc<ProxyState>, max_connections: usize) {
    let slots = Arc::new(Semaphore::new(max_connections));
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!("egress accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let _ = stream.set_nodelay(true);
        let admission = admit(&state, peer);
        let state = state.clone();
        tokio::spawn(async move {
            run(&state, peer, admission, stream).await;
            drop(slot);
        });
    }
}

struct Ctx {
    start: Instant,
    source: IpAddr,
    who: Option<Identified>,
    host: String,
    port: u16,
    resolved: Option<SocketAddr>,
}

impl Ctx {
    fn record(&self, status: u16, deny: Option<DenyReason>, up: u64, down: u64) -> TunnelLog {
        TunnelLog {
            source: self.source,
            network: self.who.as_ref().map(|w| w.network.clone()),
            identity: self.who.as_ref().map(|w| w.identity),
            host: self.host.clone(),
            port: self.port,
            resolved: self.resolved,
            status,
            deny,
            bytes_up: up,
            bytes_down: down,
            duration: self.start.elapsed(),
        }
    }
}

async fn deny<S: AsyncWrite + Unpin>(state: &ProxyState, ctx: &Ctx, client: &mut S, r: DenyReason) {
    let status = r.status();
    let text = if status == 405 {
        "Method Not Allowed"
    } else {
        "Forbidden"
    };
    let answer = format!(
        "HTTP/1.1 {status} {text}\r\n{DENY_HEADER}: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        r.as_str()
    );
    let _ = client.write_all(answer.as_bytes()).await;
    let _ = client.shutdown().await;
    if let Some(w) = &ctx.who
        && r != DenyReason::UnknownSource
    {
        state.usage.add_denied(&w.identity);
    }
    state.log.tunnel(&ctx.record(status, Some(r), 0, 0));
}

async fn plain<S: AsyncWrite + Unpin>(state: &ProxyState, ctx: &Ctx, client: &mut S, status: u16) {
    let text = match status {
        400 => "Bad Request",
        502 => "Bad Gateway",
        _ => "Error",
    };
    let answer =
        format!("HTTP/1.1 {status} {text}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let _ = client.write_all(answer.as_bytes()).await;
    let _ = client.shutdown().await;
    state.log.tunnel(&ctx.record(status, None, 0, 0));
}

/// Reads the request head (to the empty line). Returns the head and the bytes after it, or
/// `None` when the client closes, is too slow or sends more than [`MAX_HEAD`].
async fn read_head<S: AsyncRead + Unpin>(
    client: &mut S,
    timeout: Duration,
) -> Option<(Vec<u8>, Vec<u8>)> {
    let read = async {
        let mut buf = Vec::with_capacity(1024);
        let mut tmp = [0u8; 2048];
        loop {
            let n = client.read(&mut tmp).await.ok()?;
            if n == 0 {
                return None;
            }
            let from = buf.len().saturating_sub(3);
            buf.extend_from_slice(&tmp[..n]);
            if let Some(p) = buf[from..].windows(4).position(|w| w == b"\r\n\r\n") {
                let end = from + p + 4;
                if end > MAX_HEAD {
                    return None;
                }
                let rest = buf.split_off(end);
                return Some((buf, rest));
            }
            if buf.len() > MAX_HEAD {
                return None;
            }
        }
    };
    tokio::time::timeout(timeout, read).await.ok().flatten()
}

/// What [`admit`] decided for one connection, before its request head is read.
pub enum Admission {
    /// The connection holds a slot: its identity's connection slot, or one of the unknown
    /// sources' slots (`who` is `None`).
    Admitted { who: Option<Identified>, slot: Slot },
    /// No slot was free: the identity holds `max_tunnels + PENDING_ALLOWANCE` connections, or
    /// the unknown sources hold [`MAX_UNKNOWN_CONNECTIONS`]. The connection is refused at once,
    /// without waiting for its head.
    Full { who: Option<Identified> },
}

/// The slot an admitted connection holds until it closes.
pub enum Slot {
    Identity(ConnectionPermit),
    Unknown(OwnedSemaphorePermit),
}

/// Identifies a new connection by its source address (only the network it came from) and takes
/// a connection slot for it. Called right after accept and before anything is read, so a
/// connection that sends nothing counts against its own identity, never against other apps.
pub fn admit(state: &ProxyState, peer: SocketAddr) -> Admission {
    let who = state.policy.identify(canonical(peer.ip()));
    match who {
        Some(w) => match w.limit_state.try_open_connection(&w.limits) {
            Some(p) => Admission::Admitted {
                who: Some(w),
                slot: Slot::Identity(p),
            },
            None => Admission::Full { who: Some(w) },
        },
        None => match state.unknown.clone().try_acquire_owned() {
            Ok(p) => Admission::Admitted {
                who: None,
                slot: Slot::Unknown(p),
            },
            Err(_) => Admission::Full { who: None },
        },
    }
}

/// Handles one client connection from `peer`: [`admit`], then [`run`].
pub async fn handle<S>(state: &ProxyState, peer: SocketAddr, client: S)
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let admission = admit(state, peer);
    run(state, peer, admission, client).await;
}

/// Handles one admitted (or refused) client connection from `peer`.
pub async fn run<S>(state: &ProxyState, peer: SocketAddr, admission: Admission, mut client: S)
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let mut ctx = Ctx {
        start: Instant::now(),
        source: canonical(peer.ip()),
        who: None,
        host: "-".to_string(),
        port: 0,
        resolved: None,
    };
    let _slot = match admission {
        Admission::Admitted { who, slot } => {
            ctx.who = who;
            slot
        }
        Admission::Full { who } => {
            // Answered at once (the head is not read) and closed.
            let reason = if who.is_some() {
                DenyReason::Limit
            } else {
                DenyReason::UnknownSource
            };
            ctx.who = who;
            return deny(state, &ctx, &mut client, reason).await;
        }
    };
    let Some((head, rest)) = read_head(&mut client, state.head_timeout).await else {
        return;
    };
    let head_text = String::from_utf8_lossy(&head);
    let request_line = head_text.lines().next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    let version = parts.next().unwrap_or_default();

    // 1. Who: only the network the connection came from, decided at accept (`admit`), never
    // from anything in the head.
    let Some(who) = ctx.who.clone() else {
        return deny(state, &ctx, &mut client, DenyReason::UnknownSource).await;
    };

    // 2. CONNECT only: the proxy never sees a path or a query string.
    if method != "CONNECT" {
        ctx.host = "-".to_string();
        return deny(state, &ctx, &mut client, DenyReason::Method).await;
    }
    if !version.starts_with("HTTP/1.") || parts.next().is_some() {
        return plain(state, &ctx, &mut client, 400).await;
    }

    // 3. The target: host:port.
    let Some((raw_host, raw_port)) = target.rsplit_once(':') else {
        return plain(state, &ctx, &mut client, 400).await;
    };
    let port = match raw_port.parse::<u16>() {
        Ok(p) if p != 0 && raw_port.bytes().all(|b| b.is_ascii_digit()) => p,
        _ => return plain(state, &ctx, &mut client, 400).await,
    };
    ctx.port = port;
    let host = match parse_host(raw_host) {
        Ok(h) => h,
        Err(BadHost::BadNumber) => {
            return deny(state, &ctx, &mut client, DenyReason::Address).await;
        }
        Err(BadHost::Malformed) => return plain(state, &ctx, &mut client, 400).await,
    };
    ctx.host = host.to_string();

    // 4. The port and the allow-list, before any DNS query.
    if let Err(r) = state.policy.precheck(&who, &host, port) {
        return deny(state, &ctx, &mut client, r).await;
    }

    // 5. The limits of the identity.
    let Some(permit) = who.limit_state.try_acquire(&who.limits) else {
        return deny(state, &ctx, &mut client, DenyReason::Limit).await;
    };

    // 6. Resolve once, check every answer, connect to the checked address.
    let deadline = tokio::time::Instant::now() + state.connect_timeout;
    let resolved = match &host {
        Host::Ip(ip) => vec![*ip],
        Host::Name(name) => {
            match tokio::time::timeout_at(deadline, state.resolver.resolve(name)).await {
                Ok(Ok(a)) if !a.is_empty() => a,
                _ => return plain(state, &ctx, &mut client, 502).await,
            }
        }
    };
    let checked = match state.policy.decide(&who, &host, port, &resolved) {
        Decision::Allow(ip) => ip,
        Decision::Deny(r) => return deny(state, &ctx, &mut client, r).await,
    };
    // Every answer passed the address rules (one refused answer refuses them all), so each is a
    // checked address: try them in order, the one `decide` named first (an IPv6 answer first
    // fails at once on a network without IPv6). No new resolution.
    let mut candidates = vec![checked];
    for ip in &resolved {
        let ip = canonical(*ip);
        if !candidates.contains(&ip) {
            candidates.push(ip);
        }
    }
    let mut connected = None;
    for checked in candidates {
        let target = SocketAddr::new(checked, port);
        ctx.resolved = Some(target);
        match tokio::time::timeout_at(deadline, TcpStream::connect(target)).await {
            Ok(Ok(s)) => {
                connected = Some(s);
                break;
            }
            Ok(Err(_)) => continue,
            Err(_) => break,
        }
    }
    let Some(mut upstream) = connected else {
        return plain(state, &ctx, &mut client, 502).await;
    };
    let _ = upstream.set_nodelay(true);
    if client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await
        .is_err()
    {
        state.log.tunnel(&ctx.record(200, None, 0, 0));
        return;
    }
    state.usage.add_tunnel(&who.identity);

    // 7. Relay, counting bytes as they pass.
    let usage = state.usage.clone();
    let identity = who.identity;
    let mut early_up = 0u64;
    if !rest.is_empty() {
        usage.add_bytes(&identity, rest.len() as u64, 0);
        early_up = rest.len() as u64;
        if upstream.write_all(&rest).await.is_err() {
            state.log.tunnel(&ctx.record(200, None, early_up, 0));
            return;
        }
    }
    let (up, down) = relay(
        client,
        upstream,
        who.limits.idle_timeout,
        who.limits.max_lifetime,
        |n| usage.add_bytes(&identity, n, 0),
        |n| usage.add_bytes(&identity, 0, n),
    )
    .await;
    drop(permit);
    state
        .log
        .tunnel(&ctx.record(200, None, early_up + up, down));
}

/// Copies both ways until both sides close, an error, the idle timeout or the lifetime. Returns
/// the bytes up (client to upstream) and down.
pub async fn relay<C, U>(
    client: C,
    upstream: U,
    idle: Duration,
    lifetime: Duration,
    on_up: impl Fn(u64),
    on_down: impl Fn(u64),
) -> (u64, u64)
where
    C: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
{
    let start = tokio::time::Instant::now();
    let last = AtomicU64::new(0);
    let up = AtomicU64::new(0);
    let down = AtomicU64::new(0);
    let (mut cr, mut cw) = tokio::io::split(client);
    let (mut ur, mut uw) = tokio::io::split(upstream);

    let touch = || {
        last.store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
    };
    let pumps = async {
        let a = pump(&mut cr, &mut uw, &up, &on_up, &touch);
        let b = pump(&mut ur, &mut cw, &down, &on_down, &touch);
        let _ = tokio::try_join!(a, b);
    };
    let watchdog = async {
        loop {
            let seen = Duration::from_millis(last.load(Ordering::Relaxed));
            let due = start + seen + idle;
            if tokio::time::Instant::now() >= due {
                return;
            }
            tokio::time::sleep_until(due).await;
        }
    };
    tokio::select! {
        _ = pumps => {}
        _ = watchdog => {}
        _ = tokio::time::sleep_until(start + lifetime) => {}
    }
    let _ = cw.shutdown().await;
    let _ = uw.shutdown().await;
    (up.load(Ordering::Relaxed), down.load(Ordering::Relaxed))
}

async fn pump<R, W>(
    r: &mut R,
    w: &mut W,
    count: &AtomicU64,
    on_bytes: &impl Fn(u64),
    touch: &impl Fn(),
) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = r.read(&mut buf).await?;
        if n == 0 {
            // Half close: the other direction goes on until it ends too.
            let _ = w.shutdown().await;
            return Ok(());
        }
        touch();
        count.fetch_add(n as u64, Ordering::Relaxed);
        on_bytes(n as u64);
        w.write_all(&buf[..n]).await?;
        touch();
    }
}
