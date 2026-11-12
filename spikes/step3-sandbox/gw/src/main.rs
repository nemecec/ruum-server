//! Spike S-2 (docs/STEP3.md card S-2): a throwaway gateway and egress proxy. Not product code.
//!
//! Subcommands (configuration in environment variables, so no secret is on a command line):
//!
//! - `proxy`: listen on `GW_LISTEN`, clean the request (rules of
//!   `contracts/apps/gateway.openapi.yaml`, "The check of every request" 3), add a signed
//!   `Ruum-User` (EdDSA, key in the PEM file `GW_KEY_FILE`, claims of
//!   `contracts/apps/user-token.schema.json`), forward to `GW_UPSTREAM`, clean the response (4).
//! - `static`: listen on `GW_LISTEN` and serve the objects under `GW_PREFIX/` of the Garage bucket
//!   `S3_BUCKET` (`S3_ENDPOINT`, `S3_ACCESS_KEY_ID`, `S3_SECRET_ACCESS_KEY`), with the response rules.
//! - `put <dir>`: upload every file under `<dir>` to `GW_PREFIX/<relative path>`.
//! - `clean`: delete every object of the bucket.
//! - `egress`: a CONNECT proxy on `GW_LISTEN` (docs/STEP3.md "Infrastructure work" 4): resolve the
//!   host, refuse private, loopback, link-local (metadata), CGNAT, non-global IPv6 and
//!   `EGRESS_DENY` addresses (addresses or CIDR prefixes, on the canonical IPv4 form of a mapped
//!   address), check `EGRESS_ALLOW`, connect to the checked IP, log host, port, bytes and status.

use std::{
    convert::Infallible,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path as FsPath, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use futures::TryStreamExt;
use http_body_util::{BodyExt, Empty, Full, combinators::BoxBody};
use hyper::{
    Method, Request, Response, StatusCode, Uri,
    body::Incoming,
    header::{self, HeaderMap, HeaderName, HeaderValue},
    server::conn::http1,
    service::service_fn,
};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::{TokioExecutor, TokioIo},
};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use object_store::{
    ObjectStore, ObjectStoreExt, PutPayload, aws::AmazonS3Builder, path::Path as ObjPath,
};
use tokio::net::{TcpListener, TcpStream};

type Body = BoxBody<Bytes, hyper::Error>;
type BoxError = Box<dyn std::error::Error + Send + Sync>;

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("set {name}"))
}

fn full(b: impl Into<Bytes>) -> Body {
    Full::new(b.into()).map_err(|never| match never {}).boxed()
}

fn text(status: StatusCode, msg: &'static str) -> Response<Body> {
    let mut r = Response::new(full(msg));
    *r.status_mut() = status;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    r
}

// ---------------------------------------------------------------------------------------------
// Header and cookie rules (the part G-1a takes over).

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Headers a client may never send to an app (besides `Ruum-*` and every `X-Forwarded-*`, see
/// `is_client_dropped`): trace headers the gateway sets itself, and forwarding headers. The
/// gateway does not pass the visitor's IP address to apps (design 9.6: apps know end users only by
/// a pairwise pseudonym), so it adds none of the forwarding headers either.
const CLIENT_DROPPED: &[&str] = &[
    "traceparent",
    "tracestate",
    "baggage",
    "forwarded",
    "x-real-ip",
];

/// A forwarding or trace header from the internet: the list above and every name that starts with
/// `x-forwarded-` (`-For`, `-Host`, `-Proto`, `-Port`, `-Prefix`, `-Server`, `-Ssl`, ...).
pub fn is_client_dropped(name: &HeaderName) -> bool {
    let n = name.as_str(); // lower case already
    n.starts_with("x-forwarded-") || CLIENT_DROPPED.contains(&n)
}

/// `Ruum-User`, `ruum_user`, `RUUM-anything`: some app frameworks (CGI, WSGI, PHP) map `_` to `-`,
/// so `Ruum_User` would reach them as `HTTP_RUUM_USER`. Both spellings are platform headers.
pub fn is_platform_header(name: &HeaderName) -> bool {
    let n = name.as_str(); // lower case already
    n == "ruum" || n.starts_with("ruum-") || n.starts_with("ruum_")
}

/// The gateway's cookies and every cookie an app may not set: `__Host-ruum*`, `__Secure-ruum*`,
/// `ruum*` (case-insensitive; the contract names `__Host-ruum_app`, `__Host-ruum_visit` and any
/// `ruum` name).
pub fn is_platform_cookie(name: &[u8]) -> bool {
    let n = String::from_utf8_lossy(name).trim().to_ascii_lowercase();
    let n = n
        .strip_prefix("__host-")
        .or_else(|| n.strip_prefix("__secure-"))
        .unwrap_or(&n);
    n.starts_with("ruum")
}

fn strip_hop_by_hop(h: &mut HeaderMap) {
    let listed: Vec<String> = h
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    for n in listed {
        if let Ok(name) = HeaderName::from_bytes(n.as_bytes()) {
            h.remove(name);
        }
    }
    for n in HOP_BY_HOP {
        h.remove(*n);
    }
}

/// The `Cookie` headers without the platform's cookies, merged into one (HTTP/2 may send several).
fn clean_cookie(h: &mut HeaderMap) {
    let mut kept: Vec<Vec<u8>> = Vec::new();
    for v in h.get_all(header::COOKIE) {
        for pair in v.as_bytes().split(|b| *b == b';') {
            let pair = pair.trim_ascii();
            if pair.is_empty() {
                continue;
            }
            let name = pair.split(|b| *b == b'=').next().unwrap_or_default();
            if !is_platform_cookie(name) {
                kept.push(pair.to_vec());
            }
        }
    }
    h.remove(header::COOKIE);
    if !kept.is_empty()
        && let Ok(v) = HeaderValue::from_bytes(&kept.join(&b"; "[..]))
    {
        h.insert(header::COOKIE, v);
    }
}

pub fn clean_request(h: &mut HeaderMap) {
    strip_hop_by_hop(h);
    let dropped: Vec<HeaderName> = h
        .keys()
        .filter(|n| is_platform_header(n) || is_client_dropped(n))
        .cloned()
        .collect();
    for n in dropped {
        h.remove(n);
    }
    clean_cookie(h);
    // A partner key or an agent token is the gateway's, never the app's. The spike knows only the
    // partner key prefix. The agent token is a JWT (no prefix), so G-1a removes `Authorization`
    // whenever the gateway authenticated the request with it, not by a prefix match (section G).
    let drop_auth = h.get_all(header::AUTHORIZATION).iter().any(|v| {
        let v = String::from_utf8_lossy(v.as_bytes()).to_ascii_lowercase();
        v.starts_with("bearer ruum_") || v.contains("ruum_appkey_")
    });
    if drop_auth {
        h.remove(header::AUTHORIZATION);
    }
}

pub fn clean_response(h: &mut HeaderMap) {
    strip_hop_by_hop(h);
    let kept: Vec<HeaderValue> = h
        .get_all(header::SET_COOKIE)
        .iter()
        .filter(|v| {
            let name = v
                .as_bytes()
                .split(|b| *b == b'=')
                .next()
                .unwrap_or_default();
            !is_platform_cookie(name)
        })
        .cloned()
        .collect();
    h.remove(header::SET_COOKIE);
    for v in kept {
        h.append(header::SET_COOKIE, v);
    }
    h.insert(
        header::STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=31536000"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    if !h.contains_key(header::CONTENT_SECURITY_POLICY) {
        h.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("frame-ancestors 'none'"),
        );
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn hex(n: usize) -> String {
    let mut s = String::new();
    while s.len() < n {
        s.push_str(&uuid::Uuid::new_v4().simple().to_string());
    }
    s.truncate(n);
    s
}

struct ProxyCfg {
    upstream: Uri,
    key: EncodingKey,
    iss: String,
    aud: String,
    env: String,
    client: Client<HttpConnector, Incoming>,
}

fn mint(cfg: &ProxyCfg) -> String {
    let iat = now();
    let claims = serde_json::json!({
        "iss": cfg.iss, "aud": cfg.aud, "typ": "app_user", "env": cfg.env,
        "sub": "pairwise:s2spikevisitor", "tenant": "spike", "roles": [], "node_roles": {},
        "actor": "user", "iat": iat, "exp": iat + 60, "jti": uuid::Uuid::now_v7().to_string(),
    });
    let mut h = Header::new(Algorithm::EdDSA);
    h.kid = Some("s2-spike".into());
    jsonwebtoken::encode(&h, &claims, &cfg.key).expect("sign")
}

async fn proxy(cfg: Arc<ProxyCfg>, req: Request<Incoming>) -> Result<Response<Body>, Infallible> {
    let (mut parts, body) = req.into_parts();
    clean_request(&mut parts.headers);
    let token = mint(&cfg);
    let h = &mut parts.headers;
    h.insert(
        "ruum-user",
        HeaderValue::from_str(&token).expect("token is ASCII"),
    );
    h.insert(
        "ruum-request-id",
        HeaderValue::from_str(&uuid::Uuid::now_v7().to_string()).expect("ASCII"),
    );
    h.insert(
        "traceparent",
        HeaderValue::from_str(&format!("00-{}-{}-01", hex(32), hex(16))).expect("ASCII"),
    );
    let pq = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
    let mut up = cfg.upstream.clone().into_parts();
    up.path_and_query = pq.parse().ok();
    parts.uri = Uri::from_parts(up).expect("uri");
    match cfg.client.request(Request::from_parts(parts, body)).await {
        Ok(resp) => {
            let (mut rp, rb) = resp.into_parts();
            clean_response(&mut rp.headers);
            Ok(Response::from_parts(rp, rb.boxed()))
        }
        Err(e) => {
            eprintln!("upstream error: {e}");
            let mut r = text(StatusCode::BAD_GATEWAY, "bad gateway\n");
            clean_response(r.headers_mut());
            Ok(r)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Static files from Garage.

fn store() -> Arc<dyn ObjectStore> {
    Arc::new(
        AmazonS3Builder::new()
            .with_endpoint(env("S3_ENDPOINT"))
            .with_region(std::env::var("S3_REGION").unwrap_or_else(|_| "garage".into()))
            .with_bucket_name(env("S3_BUCKET"))
            .with_access_key_id(env("S3_ACCESS_KEY_ID"))
            .with_secret_access_key(env("S3_SECRET_ACCESS_KEY"))
            .with_allow_http(true)
            .with_virtual_hosted_style_request(false)
            .build()
            .expect("s3 store"),
    )
}

/// The object key of a request path, or `None` when the path is not a plain file path (no `..`,
/// `.`, empty segment, `%`, backslash or control byte; a percent-encoded path is refused, not
/// decoded, so no second decoding can reach another prefix).
pub fn static_key(prefix: &str, path: &str) -> Option<String> {
    let rel = path.strip_prefix('/')?;
    let rel = if rel.is_empty() || rel.ends_with('/') {
        format!("{rel}index.html")
    } else {
        rel.to_string()
    };
    for seg in rel.split('/') {
        if seg.is_empty()
            || seg == "."
            || seg == ".."
            || seg
                .bytes()
                .any(|b| b == b'%' || b == b'\\' || b.is_ascii_control())
        {
            return None;
        }
    }
    Some(format!("{prefix}/{rel}"))
}

fn content_type(key: &str) -> &'static str {
    match key.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

async fn serve_static(
    store: Arc<dyn ObjectStore>,
    prefix: Arc<String>,
    req: Request<Incoming>,
) -> Result<Response<Body>, Infallible> {
    let mut resp = if req.method() != Method::GET && req.method() != Method::HEAD {
        text(StatusCode::METHOD_NOT_ALLOWED, "method not allowed\n")
    } else {
        match static_key(&prefix, req.uri().path()) {
            None => text(StatusCode::NOT_FOUND, "not found\n"),
            Some(key) => match ObjPath::parse(&key) {
                Err(_) => text(StatusCode::NOT_FOUND, "not found\n"),
                Ok(p) => match store.get(&p).await {
                    Ok(got) => match got.bytes().await {
                        Ok(b) => {
                            let mut r = Response::new(if req.method() == Method::HEAD {
                                Empty::new().map_err(|never| match never {}).boxed()
                            } else {
                                full(b)
                            });
                            r.headers_mut().insert(
                                header::CONTENT_TYPE,
                                HeaderValue::from_static(content_type(&key)),
                            );
                            r
                        }
                        Err(e) => {
                            eprintln!("read {key}: {e}");
                            text(StatusCode::BAD_GATEWAY, "bad gateway\n")
                        }
                    },
                    Err(object_store::Error::NotFound { .. }) => {
                        text(StatusCode::NOT_FOUND, "not found\n")
                    }
                    Err(e) => {
                        eprintln!("get {key}: {e}");
                        text(StatusCode::BAD_GATEWAY, "bad gateway\n")
                    }
                },
            },
        }
    };
    clean_response(resp.headers_mut());
    Ok(resp)
}

fn files(dir: &FsPath, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        if p.is_dir() {
            files(&p, out)?;
        } else if p.is_file() {
            out.push(p);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Egress proxy.

/// The address the kernel connects to: an IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) is the IPv4
/// address. Every check (public, deny list) runs on this form, so `::ffff:<host IP>` cannot pass a
/// deny entry written as `<host IP>`.
pub fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

pub fn is_public(ip: IpAddr) -> bool {
    match canonical(ip) {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            // Only global unicast (2000::/3) can pass. That alone refuses loopback, unspecified,
            // IPv4-compatible `::/96`, NAT64 `64:ff9b::/96` and `64:ff9b:1::/48` (they translate
            // to any IPv4), discard `100::/64`, unique local, link local and multicast.
            let s = v6.segments();
            (s[0] & 0xe000) == 0x2000
                && !((s[0] == 0x2001 && s[1] < 0x0200) // 2001::/23: Teredo, benchmarking, ORCHID
                    || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
                    || s[0] == 0x2002 // 6to4 (embeds any IPv4)
                    || (s[0] & 0xfff0) == 0x3ff0) // documentation 3fff::/20
        }
    }
}

/// One entry of `EGRESS_DENY`: an address or a CIDR prefix (`a.b.c.d/n`, `x::/n`). The host's
/// IPv6 is a whole `/64`, so single addresses are not enough.
#[derive(Clone, Copy, Debug)]
pub struct Cidr {
    net: IpAddr,
    len: u8,
}

impl Cidr {
    pub fn parse(s: &str) -> Option<Cidr> {
        let (a, l) = s.split_once('/').map_or((s, None), |(a, l)| (a, Some(l)));
        let net = canonical(a.trim().parse().ok()?);
        let max = if net.is_ipv4() { 32 } else { 128 };
        let len = match l {
            Some(l) => l.trim().parse().ok().filter(|l| *l <= max)?,
            None => max,
        };
        Some(Cidr { net, len })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        // Compare the top `len` bits; `checked_shr` covers `len == 0` (shift by the full width).
        let same = |a: u128, n: u128, width: u32| {
            let shift = width - u32::from(self.len);
            a.checked_shr(shift).unwrap_or(0) == n.checked_shr(shift).unwrap_or(0)
        };
        match (canonical(ip), self.net) {
            (IpAddr::V4(a), IpAddr::V4(n)) => same(u32::from(a).into(), u32::from(n).into(), 32),
            (IpAddr::V6(a), IpAddr::V6(n)) => same(a.into(), n.into(), 128),
            _ => false,
        }
    }
}

fn is_public_v4(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    !(v4.is_private()
        || v4.is_loopback()
        || v4.is_link_local() // 169.254.0.0/16, the metadata address
        || v4.is_broadcast()
        || v4.is_multicast()
        || v4.is_documentation()
        || v4.is_unspecified()
        || o[0] == 0
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // CGNAT 100.64.0.0/10
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24
        || (o[0] == 198 && (o[1] & 0xfe) == 18) // benchmarking
        || o[0] >= 240)
}

struct EgressCfg {
    allow: Vec<String>,
    deny: Vec<Cidr>,
    ports: Vec<u16>,
}

fn host_allowed(allow: &[String], host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    allow.iter().any(|a| {
        if let Some(suffix) = a.strip_prefix("*.") {
            host.ends_with(&format!(".{suffix}"))
        } else {
            *a == host
        }
    })
}

async fn egress(cfg: Arc<EgressCfg>, peer: SocketAddr, mut client: TcpStream) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 1024];
    // Read the request head (max 8 KiB).
    loop {
        match client.read(&mut tmp).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > 8192 {
            return;
        }
    }
    let head = String::from_utf8_lossy(&buf).to_string();
    let first = head.lines().next().unwrap_or_default();
    let mut it = first.split_whitespace();
    let (method, target) = (it.next().unwrap_or_default(), it.next().unwrap_or_default());
    let reply = |code: &str| format!("HTTP/1.1 {code}\r\nContent-Length: 0\r\n\r\n");
    let log = |host: &str, port: &str, status: u16, up: u64, down: u64| {
        eprintln!(
            "egress peer={} host={host} port={port} status={status} bytes_up={up} bytes_down={down}",
            peer.ip()
        );
    };
    if method != "CONNECT" {
        let _ = client
            .write_all(reply("405 Method Not Allowed").as_bytes())
            .await;
        log("-", "-", 405, 0, 0);
        return;
    }
    let Some((host, port_s)) = target.rsplit_once(':') else {
        let _ = client.write_all(reply("400 Bad Request").as_bytes()).await;
        return;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let port: u16 = port_s.parse().unwrap_or(0);
    if !cfg.ports.contains(&port) || !host_allowed(&cfg.allow, host) {
        let _ = client.write_all(reply("403 Forbidden").as_bytes()).await;
        log(host, port_s, 403, 0, 0);
        return;
    }
    // Resolve here; connect to a checked address only (no second lookup, no DNS rebinding).
    let addrs: Vec<SocketAddr> = match tokio::net::lookup_host((host, port)).await {
        Ok(a) => a.collect(),
        Err(_) => {
            let _ = client.write_all(reply("502 Bad Gateway").as_bytes()).await;
            log(host, port_s, 502, 0, 0);
            return;
        }
    };
    // Check and connect to the canonical form (`::ffff:a.b.c.d` becomes `a.b.c.d`).
    let ok: Vec<SocketAddr> = addrs
        .into_iter()
        .map(|a| SocketAddr::new(canonical(a.ip()), a.port()))
        .filter(|a| is_public(a.ip()) && !cfg.deny.iter().any(|d| d.contains(a.ip())))
        .collect();
    let Some(addr) = ok.first() else {
        let _ = client.write_all(reply("403 Forbidden").as_bytes()).await;
        log(host, port_s, 403, 0, 0);
        return;
    };
    let mut upstream = match TcpStream::connect(addr).await {
        Ok(s) => s,
        Err(_) => {
            let _ = client.write_all(reply("502 Bad Gateway").as_bytes()).await;
            log(host, port_s, 502, 0, 0);
            return;
        }
    };
    if client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await
        .is_err()
    {
        return;
    }
    // Bytes after the head belong to the tunnel.
    let pos = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(0) + 4;
    if pos < buf.len() && upstream.write_all(&buf[pos..]).await.is_err() {
        return;
    }
    let (up, down) = tokio::io::copy_bidirectional(&mut client, &mut upstream)
        .await
        .unwrap_or((0, 0));
    log(host, port_s, 200, up, down);
}

// ---------------------------------------------------------------------------------------------

async fn listen() -> TcpListener {
    let addr: SocketAddr = env("GW_LISTEN").parse().expect("GW_LISTEN");
    TcpListener::bind(addr).await.expect("bind")
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("proxy") => {
            let pem = std::fs::read(env("GW_KEY_FILE"))?;
            let cfg = Arc::new(ProxyCfg {
                upstream: env("GW_UPSTREAM").parse()?,
                key: EncodingKey::from_ed_pem(&pem)?,
                iss: std::env::var("GW_ISS").unwrap_or_else(|_| "https://ruum.localhost".into()),
                aud: env("GW_AUD"),
                env: std::env::var("GW_ENV").unwrap_or_else(|_| "live".into()),
                client: Client::builder(TokioExecutor::new()).build_http(),
            });
            let l = listen().await;
            eprintln!("proxy listening on {}", l.local_addr()?);
            loop {
                let (s, _) = l.accept().await?;
                let cfg = cfg.clone();
                tokio::spawn(async move {
                    let svc = service_fn(move |req| proxy(cfg.clone(), req));
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(s), svc)
                        .await;
                });
            }
        }
        Some("static") => {
            let st = store();
            let prefix = Arc::new(env("GW_PREFIX"));
            let l = listen().await;
            eprintln!("static listening on {}", l.local_addr()?);
            loop {
                let (s, _) = l.accept().await?;
                let (st, prefix) = (st.clone(), prefix.clone());
                tokio::spawn(async move {
                    let svc = service_fn(move |req| serve_static(st.clone(), prefix.clone(), req));
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(s), svc)
                        .await;
                });
            }
        }
        Some("put") => {
            let dir = PathBuf::from(args.get(2).expect("put <dir>"));
            let prefix = env("GW_PREFIX");
            let st = store();
            let mut all = Vec::new();
            files(&dir, &mut all)?;
            for f in all {
                let rel = f.strip_prefix(&dir)?.to_string_lossy().replace('\\', "/");
                let key = if prefix.is_empty() {
                    rel
                } else {
                    format!("{prefix}/{rel}")
                };
                st.put(&ObjPath::parse(&key)?, PutPayload::from(std::fs::read(&f)?))
                    .await?;
                println!("put {key}");
            }
            Ok(())
        }
        Some("clean") => {
            let st = store();
            let all: Vec<_> = st.list(None).try_collect().await?;
            for m in all {
                st.delete(&m.location).await?;
            }
            Ok(())
        }
        Some("egress") => {
            let cfg = Arc::new(EgressCfg {
                allow: std::env::var("EGRESS_ALLOW")
                    .unwrap_or_default()
                    .split(',')
                    .map(|s| s.trim().to_ascii_lowercase())
                    .filter(|s| !s.is_empty())
                    .collect(),
                deny: std::env::var("EGRESS_DENY")
                    .unwrap_or_default()
                    .split(',')
                    .filter_map(|s| Cidr::parse(s.trim()))
                    .collect(),
                ports: std::env::var("EGRESS_PORTS")
                    .unwrap_or_else(|_| "443".into())
                    .split(',')
                    .filter_map(|s| s.trim().parse().ok())
                    .collect(),
            });
            let l = listen().await;
            eprintln!("egress listening on {}", l.local_addr()?);
            loop {
                let (s, peer) = l.accept().await?;
                tokio::spawn(egress(cfg.clone(), peer, s));
            }
        }
        _ => {
            eprintln!("usage: s2-gw proxy|static|put <dir>|clean|egress");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_cleaning() {
        let mut h = HeaderMap::new();
        h.insert("ruum-user", HeaderValue::from_static("forged"));
        h.insert("ruum_user", HeaderValue::from_static("forged"));
        h.insert("ruum-request-id", HeaderValue::from_static("x"));
        h.insert("traceparent", HeaderValue::from_static("00-a-b-01"));
        h.insert("connection", HeaderValue::from_static("x-keep"));
        h.insert("x-keep", HeaderValue::from_static("hop"));
        h.append(
            header::COOKIE,
            HeaderValue::from_static("__Host-ruum_app=s; a=1"),
        );
        h.append(
            header::COOKIE,
            HeaderValue::from_static("__host-RUUM_visit=v;b=2; ruum_x=3"),
        );
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer ruum_appkey_abc"),
        );
        clean_request(&mut h);
        assert!(h.keys().all(|n| !is_platform_header(n)));
        assert!(h.get("traceparent").is_none() && h.get("x-keep").is_none());
        assert_eq!(h.get(header::COOKIE).unwrap(), "a=1; b=2");
        assert!(h.get(header::AUTHORIZATION).is_none());
    }

    #[test]
    fn forwarding_headers_dropped() {
        let mut h = HeaderMap::new();
        for n in [
            "x-forwarded-for",
            "x-forwarded-host",
            "x-forwarded-proto",
            "x-forwarded-port",
            "x-forwarded-prefix",
            "x-forwarded-server",
            "x-forwarded-ssl",
            "forwarded",
            "x-real-ip",
            "tracestate",
            "baggage",
        ] {
            h.insert(
                HeaderName::from_static(n),
                HeaderValue::from_static("forged"),
            );
        }
        h.insert("x-app", HeaderValue::from_static("kept"));
        clean_request(&mut h);
        assert_eq!(h.len(), 1, "{h:?}");
        assert_eq!(h.get("x-app").unwrap(), "kept");
    }

    #[test]
    fn egress_deny_list() {
        let deny: Vec<Cidr> = ["203.0.113.7", "2001:db8:7:1::/64", "198.51.100.0/24"]
            .iter()
            .map(|s| Cidr::parse(s).unwrap())
            .collect();
        let denied = |s: &str| deny.iter().any(|d| d.contains(s.parse().unwrap()));
        assert!(denied("203.0.113.7"));
        assert!(denied("::ffff:203.0.113.7"), "mapped form of a denied IPv4");
        assert!(denied("2001:db8:7:1::1"));
        assert!(denied("2001:db8:7:1:ffff::9"));
        assert!(denied("198.51.100.200"));
        assert!(!denied("203.0.113.8"));
        assert!(!denied("2001:db8:7:2::1"));
        assert!(
            Cidr::parse("0.0.0.0/0")
                .unwrap()
                .contains("8.8.8.8".parse().unwrap())
        );
        assert!(
            Cidr::parse("::/0")
                .unwrap()
                .contains("2606::1".parse().unwrap())
        );
        assert!(Cidr::parse("1.2.3.4/33").is_none());
        assert_eq!(
            canonical("::ffff:203.0.113.7".parse().unwrap()),
            "203.0.113.7".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn response_cleaning() {
        let mut h = HeaderMap::new();
        for c in [
            "__Host-ruum_app=x; Path=/",
            "ruum_session=1",
            "__Secure-Ruum=2",
            "app=ok; Path=/",
        ] {
            h.append(header::SET_COOKIE, HeaderValue::from_static(c));
        }
        clean_response(&mut h);
        let left: Vec<_> = h.get_all(header::SET_COOKIE).iter().collect();
        assert_eq!(left, vec!["app=ok; Path=/"]);
        assert_eq!(h.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(), "nosniff");
    }

    #[test]
    fn static_keys() {
        assert_eq!(static_key("p", "/").as_deref(), Some("p/index.html"));
        assert_eq!(static_key("p", "/a/").as_deref(), Some("p/a/index.html"));
        assert_eq!(static_key("p", "/a/b.js").as_deref(), Some("p/a/b.js"));
        for bad in ["/../x", "/a/../../x", "/%2e%2e/x", "/a//b", "/./x", "/a\\b"] {
            assert_eq!(static_key("p", bad), None, "{bad}");
        }
    }

    #[test]
    fn egress_addresses() {
        for bad in [
            "169.254.169.254",
            "127.0.0.1",
            "10.1.2.3",
            "172.17.0.1",
            "192.168.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:169.254.169.254",
            "64:ff9b::a9fe:a9fe",
        ] {
            assert!(!is_public(bad.parse().unwrap()), "{bad}");
        }
        for bad in [
            "::a9fe:a9fe",          // IPv4-compatible
            "64:ff9b:1::a9fe:a9fe", // NAT64 local use
            "2001::1",              // Teredo
            "2001:2::1",            // benchmarking
            "100::1",               // discard
            "2002:a9fe:a9fe::1",    // 6to4
            "2001:db8::1",
            "3fff::1",
            "ff02::1",
        ] {
            assert!(!is_public(bad.parse().unwrap()), "{bad}");
        }
        assert!(is_public("1.1.1.1".parse().unwrap()));
        assert!(is_public("2606:4700::1111".parse().unwrap()));
        assert!(is_public("::ffff:1.1.1.1".parse().unwrap()));
        assert!(host_allowed(&["*.example.com".into()], "a.example.com"));
        assert!(!host_allowed(&["*.example.com".into()], "example.com.evil"));
    }
}
