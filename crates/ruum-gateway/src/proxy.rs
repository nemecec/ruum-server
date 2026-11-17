//! Cleaning both ways (decision 0006 G point 2; `gateway.openapi.yaml` "The check of every
//! request" 3 and 4), the proxy to an upstream (card G-1b: a running sandbox, or the fixed test
//! upstream of the cleaning cases; one code path for both, [`send`]) and WebSockets.
//!
//! Request ([`clean_request`]): drop every header whose name starts with `ruum-` or `ruum_` (any
//! case; CGI, WSGI and PHP read `Ruum_User` as `Ruum-User`), the hop-by-hop headers and the names
//! listed in `Connection` (before the gateway adds its own), `X-Forwarded-*` (a prefix rule),
//! `Forwarded`, `X-Real-IP`, `tracestate`, `baggage` and `traceparent` (the gateway starts a new
//! trace), `Authorization` when the gateway authenticated the request with it, and the gateway's
//! cookies from `Cookie` (several `Cookie` headers merged into one; a name is the gateway's when it
//! starts with `ruum` after an optional `__Host-` or `__Secure-` prefix, any case). The visitor's
//! IP address is never passed. Then [`add_gateway_headers`] adds `Ruum-User`, `Ruum-Request-Id`
//! and the new `traceparent`.
//!
//! Response ([`clean_response`]): drop an app's `Set-Cookie` whose name starts with `ruum` or
//! `__Host-ruum` / `__Secure-ruum` (any case), whose name is empty (`=ruum_app=x`: a browser sends
//! it back as `ruum_app=x`) or that has no `=`; drop hop-by-hop headers; add HSTS, `nosniff`,
//! `Referrer-Policy` and `frame-ancestors` ([`security_headers`]).
//!
//! # The proxy ([`send`], card G-1b)
//!
//! - **Streaming both ways.** The request body goes to the app as it arrives and the answer
//!   goes to the visitor as the app sends it; neither is read whole. A request body over
//!   [`ProxyConfig::max_body`] gets the `413` page: at once when its `Content-Length` says so
//!   (the app sees nothing), or when the stream passes the limit (the upstream request is cut
//!   off, so the app never gets a whole body). The forwarded request keeps the client's validated
//!   `Content-Length`, or is chunked when the client sent a chunked body (never both).
//! - **The upstream comes only from the core** (or the fixed test upstream). The gateway sets
//!   `Host` itself to the app's own host name; nothing of the request (its target, `Host`,
//!   `X-Forwarded-*` or `Ruum-*` headers) chooses where it goes. The client follows no redirect
//!   and uses no proxy from the environment: an app's redirect goes to the visitor as it is.
//! - **Limits.** Connecting has [`ProxyConfig::connect_timeout`] and the answer's head
//!   [`ProxyConfig::first_byte`] (`RUUM_GATEWAY_UPSTREAM_TIMEOUT`; then the `504` page); a body
//!   that stalls longer than [`ProxyConfig::read_idle`] ends. An address that takes no
//!   connection is [`Sent::NoConnection`]: the caller forgets the cached address and, for a
//!   request without a body, asks the core once more (the instance may have scaled to zero).
//!
//! # WebSockets
//!
//! An upgrade (`GET` with `Upgrade: websocket` and `upgrade` in `Connection`,
//! [`is_websocket_upgrade`]) reaches [`send`] only after the same access check as any request,
//! and gets the same cleaning and a fresh `Ruum-User`. The hop-by-hop rule drops `Connection` and
//! `Upgrade` on every other request; for this one case the forwarded request gets exactly
//! `Connection: upgrade` and `Upgrade: websocket` back, and the `101` answer the same two.
//!
//! - **Cross-site WebSocket hijacking** (decision of card G-1b, [`websocket_origin_ok`]): every
//!   app origin is its own site, and the app session cookie is `SameSite=Lax`, which a browser
//!   sends on a cross-site WebSocket handshake. So an upgrade whose `Origin` is not the app's own
//!   origin (`https://<host>`) gets the gateway's `403`. A missing `Origin` (not a browser) passes
//!   only for an agent token in `Authorization`, never with a cookie.
//! - **Shares.** Each open WebSocket takes a slot of [`crate::sandbox::SandboxHold`]: a per-app
//!   share and a global cap; over either, the `503` page before the app is asked.
//! - **A removed right ends an open WebSocket.** While a WebSocket is open, the gateway checks the
//!   host and the access again every [`ProxyConfig::ws_recheck`] (1 s; the answers come through
//!   `CoreCache`, which keeps them at most 3 s). When the right or role is gone, the user signed
//!   out of Ruum, the public approval was withdrawn, the host serves another deployment, or the
//!   core cannot answer (fail closed), the gateway writes a close frame (`1008`) to the visitor
//!   and drops both connections: within 5 s of the change. The close frame is written only
//!   between two frames of the app ([`FrameTracker`]); in the middle of a frame the connections
//!   are dropped without it.
//! - **Idle.** A WebSocket without a byte either way for [`ProxyConfig::ws_idle`]
//!   (`RUUM_GATEWAY_WS_IDLE`) closes the same way (`1001`).

use crate::check::Visitor;
use crate::sandbox::Slot;
use crate::{Gateway, Site, pages};
use axum::body::Body;
use axum::extract::Request;
use axum::response::Response;
use futures::{StreamExt, TryStreamExt};
use http::header::{HeaderMap, HeaderName, HeaderValue};
use http::{Method, StatusCode, header};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

/// The hop-by-hop headers (RFC 9110 7.6.1, and the old `Proxy-Connection`).
pub const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "trailers",
    "transfer-encoding",
    "upgrade",
];

/// Whether a header name (lower case) never passes from the internet to an app.
fn dropped_request_header(name: &str) -> bool {
    name.starts_with("ruum-")
        || name.starts_with("ruum_")
        || name.starts_with("x-forwarded-")
        || matches!(
            name,
            "forwarded"
                | "x-real-ip"
                | "tracestate"
                | "baggage"
                | "traceparent"
                | "host"
                | "cookie"
        )
        || HOP_BY_HOP.contains(&name)
}

/// The name of a cookie without a `__Host-` or `__Secure-` prefix (any case), lower case.
fn bare_cookie_name(name: &str) -> String {
    let lower = name.trim().to_ascii_lowercase();
    for p in ["__host-", "__secure-"] {
        if let Some(rest) = lower.strip_prefix(p) {
            return rest.to_string();
        }
    }
    lower
}

/// Whether a cookie name belongs to the gateway (it never reaches the app, and an app may not set
/// it): `ruum...` after an optional `__Host-` or `__Secure-` prefix, any case.
pub fn gateway_cookie_name(name: &str) -> bool {
    bare_cookie_name(name).starts_with("ruum")
}

/// The `Cookie` header for the app: every `Cookie` header of the request merged, without the
/// gateway's cookies and empty pairs. `None` when nothing is left.
pub fn app_cookie_header(headers: &HeaderMap) -> Option<String> {
    let mut kept: Vec<&str> = Vec::new();
    for v in headers.get_all(http::header::COOKIE) {
        let Ok(v) = v.to_str() else { continue };
        for pair in v.split(';') {
            let pair = pair.trim();
            if pair.is_empty() {
                continue;
            }
            let name = pair.split_once('=').map(|(n, _)| n).unwrap_or("");
            // A pair without `=` is a nameless cookie: its value may look like `ruum_app=...`
            // only with a `=`, so it is kept as the app's.
            if !name.is_empty() && gateway_cookie_name(name) {
                continue;
            }
            kept.push(pair);
        }
    }
    (!kept.is_empty()).then(|| kept.join("; "))
}

/// The value of the gateway's cookie `name` (exact name, as the gateway sets it) in the request.
pub fn gateway_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|p| p.trim().split_once('='))
        .find(|(n, _)| *n == name)
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The headers of a request for the app (see the module docs). `strip_authorization` is true when
/// the gateway authenticated the request with its `Authorization` header.
pub fn clean_request(headers: &HeaderMap, strip_authorization: bool) -> HeaderMap {
    // The names that `Connection` lists, read before anything is removed.
    let listed: Vec<String> = headers
        .get_all(http::header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|n| n.trim().to_ascii_lowercase())
        .filter(|n| !n.is_empty())
        .collect();
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        let n = name.as_str();
        if dropped_request_header(n) || listed.iter().any(|l| l == n) {
            continue;
        }
        if strip_authorization && n == "authorization" {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    if let Some(c) = app_cookie_header(headers)
        && let Ok(v) = HeaderValue::from_str(&c)
    {
        out.insert(http::header::COOKIE, v);
    }
    out
}

/// A new W3C trace context: `00-<32 hex trace>-<16 hex span>-01`, never all zeros.
pub fn new_traceparent() -> (String, String) {
    let mut trace = [0u8; 16];
    let mut span = [0u8; 8];
    loop {
        rand::fill(&mut trace[..]);
        rand::fill(&mut span[..]);
        if trace.iter().any(|b| *b != 0) && span.iter().any(|b| *b != 0) {
            break;
        }
    }
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let trace_id = hex(&trace);
    (format!("00-{trace_id}-{}-01", hex(&span)), trace_id)
}

/// Adds the gateway's own headers after [`clean_request`].
pub fn add_gateway_headers(
    out: &mut HeaderMap,
    user_token: &str,
    request_id: Uuid,
    traceparent: &str,
) {
    if let Ok(v) = HeaderValue::from_str(user_token) {
        out.insert(HeaderName::from_static("ruum-user"), v);
    }
    if let Ok(v) = HeaderValue::from_str(&request_id.to_string()) {
        out.insert(HeaderName::from_static("ruum-request-id"), v);
    }
    if let Ok(v) = HeaderValue::from_str(traceparent) {
        out.insert(HeaderName::from_static("traceparent"), v);
    }
}

/// Whether an app's `Set-Cookie` value may reach the browser (see the module docs).
pub fn app_set_cookie_allowed(raw: &str) -> bool {
    let first = raw.split(';').next().unwrap_or("");
    let Some((name, _)) = first.split_once('=') else {
        return false;
    };
    let name = name.trim();
    !name.is_empty() && !gateway_cookie_name(name)
}

/// The environment of an app origin, for `frame-ancestors`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frame {
    /// A live app: `'none'` unless the app sets its own value.
    Live,
    /// A preview: only the Ruum UI origin, whatever the app sends.
    Preview,
}

/// Removes the `frame-ancestors` directives of one CSP value.
fn without_frame_ancestors(csp: &str) -> String {
    csp.split(';')
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .filter(|d| {
            !d.to_ascii_lowercase()
                .split_whitespace()
                .next()
                .is_some_and(|n| n == "frame-ancestors")
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Adds the response headers of every answer of an app origin: HSTS, `nosniff`,
/// `Referrer-Policy` and the `frame-ancestors` of the environment. The app's own
/// `Referrer-Policy` stays; a preview's CSP loses its `frame-ancestors` for the UI origin's.
pub fn security_headers(h: &mut HeaderMap, frame: Frame, ui_origin: &str) {
    h.insert(
        http::header::STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=31536000"),
    );
    h.insert(
        http::header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if !h.contains_key(http::header::REFERRER_POLICY) {
        h.insert(
            http::header::REFERRER_POLICY,
            HeaderValue::from_static("strict-origin-when-cross-origin"),
        );
    }
    let csps: Vec<String> = h
        .get_all(http::header::CONTENT_SECURITY_POLICY)
        .iter()
        .filter_map(|v| v.to_str().ok().map(str::to_string))
        .collect();
    match frame {
        Frame::Live => {
            let own = csps.iter().any(|c| {
                c.split([';', ','])
                    .any(|d| d.trim().to_ascii_lowercase().starts_with("frame-ancestors"))
            });
            if !own {
                h.append(
                    http::header::CONTENT_SECURITY_POLICY,
                    HeaderValue::from_static("frame-ancestors 'none'"),
                );
            }
        }
        Frame::Preview => {
            h.remove(http::header::CONTENT_SECURITY_POLICY);
            for c in csps {
                let rest = without_frame_ancestors(&c);
                if !rest.is_empty()
                    && let Ok(v) = HeaderValue::from_str(&rest)
                {
                    h.append(http::header::CONTENT_SECURITY_POLICY, v);
                }
            }
            let fa = format!("frame-ancestors {}", ui_origin.trim_end_matches('/'));
            if let Ok(v) = HeaderValue::from_str(&fa) {
                h.append(http::header::CONTENT_SECURITY_POLICY, v);
            }
        }
    }
}

/// The headers of an app's answer for the browser (see the module docs).
pub fn clean_response(headers: &HeaderMap, frame: Frame, ui_origin: &str) -> HeaderMap {
    let listed: Vec<String> = headers
        .get_all(http::header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|n| n.trim().to_ascii_lowercase())
        .collect();
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        let n = name.as_str();
        if HOP_BY_HOP.contains(&n) || listed.iter().any(|l| l == n) || n == "content-length" {
            continue;
        }
        if n == "set-cookie" && !value.to_str().map(app_set_cookie_allowed).unwrap_or(false) {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    security_headers(&mut out, frame, ui_origin);
    out
}

/// The fixed test upstream of the cleaning cases (`set_upstream` of the T-6 world): a deployment
/// whose requests go to a URL instead of its static files, before G-1b exists. Never set from the
/// environment; the production binary has none.
#[derive(Default)]
pub struct TestUpstreams {
    map: RwLock<HashMap<Uuid, String>>,
}

impl TestUpstreams {
    pub fn set(&self, deployment: Uuid, url: &str) {
        self.map
            .write()
            .expect("upstreams lock")
            .insert(deployment, url.trim_end_matches('/').to_string());
    }

    pub fn get(&self, deployment: Uuid) -> Option<String> {
        self.map
            .read()
            .expect("upstreams lock")
            .get(&deployment)
            .cloned()
    }
}

/// The largest request body the proxy forwards (16 MiB).
pub const MAX_FORWARD_BODY: u64 = 16 * 1024 * 1024;

/// The settings of the proxy (see the module docs).
#[derive(Clone, Debug)]
pub struct ProxyConfig {
    pub connect_timeout: Duration,
    /// Until the head of the answer (`RUUM_GATEWAY_UPSTREAM_TIMEOUT`, default 60 s).
    pub first_byte: Duration,
    /// The longest pause inside an answer's body.
    pub read_idle: Duration,
    /// A WebSocket without traffic closes after this long (`RUUM_GATEWAY_WS_IDLE`, default
    /// 10 min).
    pub ws_idle: Duration,
    /// How often an open WebSocket's access is checked again (at most 1 s, so a removed right
    /// ends it within 5 s together with the 3 s cache).
    pub ws_recheck: Duration,
    pub max_body: u64,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            first_byte: Duration::from_secs(60),
            read_idle: Duration::from_secs(300),
            ws_idle: Duration::from_secs(600),
            ws_recheck: Duration::from_secs(1),
            max_body: MAX_FORWARD_BODY,
        }
    }
}

/// The HTTP/1.1 client of the proxy: no redirects, no proxy from the environment.
pub fn client(cfg: &ProxyConfig) -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .http1_only()
        .connect_timeout(cfg.connect_timeout)
        .read_timeout(cfg.read_idle)
        .pool_idle_timeout(Duration::from_secs(30))
        .build()
        .expect("an HTTP client without TLS settings builds")
}

/// Whether a request is a WebSocket upgrade: `GET`, `Upgrade: websocket` and the token
/// `upgrade` in `Connection` (names and values without case).
pub fn is_websocket_upgrade(method: &Method, headers: &HeaderMap) -> bool {
    let upgrade = headers
        .get_all(header::UPGRADE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| {
            v.split(',')
                .any(|t| t.trim().eq_ignore_ascii_case("websocket"))
        });
    let connection = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| {
            v.split(',')
                .any(|t| t.trim().eq_ignore_ascii_case("upgrade"))
        });
    method == Method::GET && upgrade && connection
}

/// The `Origin` rule of WebSocket upgrades (see the module docs): exactly one `Origin`, equal to
/// `https://<host>`; none only for an agent token.
pub fn websocket_origin_ok(headers: &HeaderMap, host: &str, visitor: &Visitor) -> bool {
    let mut all = headers.get_all(header::ORIGIN).iter();
    match (all.next(), all.next()) {
        (None, _) => matches!(visitor, Visitor::Agent { .. }),
        (Some(v), None) => v
            .to_str()
            .map(|o| o.eq_ignore_ascii_case(&format!("https://{host}")))
            .unwrap_or(false),
        (Some(_), Some(_)) => false,
    }
}

/// A request that passed the check, ready to be sent.
pub struct Outgoing {
    pub method: Method,
    /// The path and query.
    pub target: String,
    /// The headers as the visitor sent them (cleaned at each send).
    pub headers: HeaderMap,
    /// The body; `None` for a request without one (it can be sent again).
    pub body: Option<Body>,
    /// The visitor's side of a WebSocket upgrade.
    pub upgrade: Option<hyper::upgrade::OnUpgrade>,
    chunked: bool,
}

impl Outgoing {
    /// Takes the request apart. `Err` is the `413` page of a declared body over the limit.
    pub fn new(mut req: Request, site: &Site, max_body: u64) -> Result<Self, Box<Response>> {
        let upgrade =
            is_websocket_upgrade(req.method(), req.headers()).then(|| hyper::upgrade::on(&mut req));
        let (parts, body) = req.into_parts();
        let chunked = parts.headers.contains_key(header::TRANSFER_ENCODING);
        let length = parts
            .headers
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        if length.is_some_and(|n| n > max_body) {
            return Err(Box::new(pages::too_large(site.look())));
        }
        let has_body = upgrade.is_none() && (chunked || length.is_some_and(|n| n > 0));
        Ok(Self {
            method: parts.method,
            target: parts
                .uri
                .path_and_query()
                .map(|p| p.as_str().to_string())
                .unwrap_or_else(|| "/".into()),
            headers: parts.headers,
            body: has_body.then_some(body),
            upgrade,
            chunked,
        })
    }
}

/// The result of [`send`].
pub enum Sent {
    /// The answer for the visitor (the app's, or a page of the gateway).
    Answer(Response),
    /// The address took no connection. `Some`: nothing was sent and the request can go to
    /// another address; `None`: its body is gone.
    NoConnection(Option<Outgoing>),
}

/// Who sends: the visitor that passed the check.
pub struct Sender<'a> {
    pub visitor: &'a Visitor,
    pub visit: &'a str,
    pub strip_authorization: bool,
}

/// A body stream that fails after `max` bytes and notes it in `over`.
fn limited(body: Body, max: u64, over: Arc<AtomicBool>) -> reqwest::Body {
    let seen = Arc::new(AtomicU64::new(0));
    let stream = body.into_data_stream().map(move |chunk| {
        let chunk = chunk.map_err(std::io::Error::other)?;
        let total = seen.fetch_add(chunk.len() as u64, Ordering::SeqCst) + chunk.len() as u64;
        if total > max {
            over.store(true, Ordering::SeqCst);
            return Err(std::io::Error::other("the request body is over the limit"));
        }
        Ok(chunk)
    });
    reqwest::Body::wrap_stream(stream)
}

/// Sends a request that passed the check to `upstream` (`http://<host>:<port>`), cleaned both
/// ways (see the module docs). A WebSocket upgrade that the app accepts is joined here.
pub async fn send(
    gw: &Arc<Gateway>,
    site: &Site,
    from: &Sender<'_>,
    mut out: Outgoing,
    upstream: &str,
) -> Sent {
    let cfg = &gw.proxy;
    if out.upgrade.is_some() && !websocket_origin_ok(&out.headers, &site.name, from.visitor) {
        tracing::warn!(
            target: "ruum::security",
            event = "gateway.websocket_origin_refused",
            "a WebSocket upgrade from another origin was refused"
        );
        return Sent::Answer(pages::forbidden(site.look()));
    }
    let socket = match &out.upgrade {
        Some(_) => match gw.sandboxes.open_socket(site.resolved.deployment_id) {
            Some(slot) => Some(slot),
            None => {
                tracing::warn!(target: "ruum::gateway", "the WebSocket share of an app is full");
                return Sent::Answer(pages::unavailable(site.look()));
            }
        },
        None => None,
    };
    let subject = crate::subject_of(from.visitor, from.visit);
    let claims = gw.tokens.claims(
        site.audience(),
        site.info.environment.as_str(),
        &site.host.tenant_slug,
        &subject,
    );
    let Ok(user_token) = gw.tokens.sign(&claims) else {
        return Sent::Answer(pages::unavailable(site.look()));
    };
    let (traceparent, trace_id) = new_traceparent();
    let session = match from.visitor {
        Visitor::User { session, .. } => Some(session.as_str()),
        _ => None,
    };
    gw.visits.record(from.visit, session, trace_id);
    let mut headers = clean_request(&out.headers, from.strip_authorization);
    // One framing only: the validated length, or chunked; no length without a body.
    if out.chunked || out.body.is_none() {
        let zero = headers
            .get(header::CONTENT_LENGTH)
            .is_some_and(|v| v.as_bytes() == b"0");
        if out.chunked || !zero {
            headers.remove(header::CONTENT_LENGTH);
        }
    }
    if out.upgrade.is_some() {
        // The one exception to the hop-by-hop rule (see the module docs).
        headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    }
    if let Ok(h) = HeaderValue::from_str(&site.name) {
        headers.insert(header::HOST, h);
    }
    add_gateway_headers(&mut headers, &user_token, Uuid::now_v7(), &traceparent);
    let over = Arc::new(AtomicBool::new(false));
    let body = out.body.take();
    let had_body = body.is_some();
    let mut request = gw
        .upstream_http
        .request(out.method.clone(), format!("{upstream}{}", out.target))
        .headers(headers);
    if let Some(b) = body {
        request = request.body(limited(b, cfg.max_body, over.clone()));
    }
    let up = match tokio::time::timeout(cfg.first_byte, request.send()).await {
        Err(_) => return Sent::Answer(pages::timeout(site.look())),
        Ok(Err(_)) if over.load(Ordering::SeqCst) => {
            return Sent::Answer(pages::too_large(site.look()));
        }
        Ok(Err(e)) if e.is_connect() => {
            tracing::warn!(target: "ruum::gateway", "the upstream took no connection");
            return Sent::NoConnection((!had_body).then_some(out));
        }
        Ok(Err(e)) if e.is_timeout() => return Sent::Answer(pages::timeout(site.look())),
        Ok(Err(e)) => {
            tracing::warn!(target: "ruum::gateway", error = %e.without_url(), "the upstream did not answer");
            return Sent::Answer(pages::page(
                StatusCode::BAD_GATEWAY,
                "The app did not answer.",
                site.frame(),
                &gw.config.ui_origin,
            ));
        }
        Ok(Ok(r)) => r,
    };
    let status = up.status();
    let mut headers = clean_response(up.headers(), site.frame(), &gw.config.ui_origin);
    if status == StatusCode::SWITCHING_PROTOCOLS {
        let (Some(visitor_side), Some(slot)) = (out.upgrade.take(), socket) else {
            // A 101 to a request that asked for no upgrade.
            return Sent::Answer(pages::not_running(site.look()));
        };
        headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        tokio::spawn(splice(
            gw.clone(),
            site.clone(),
            out.headers,
            visitor_side,
            up,
            slot,
        ));
        let mut r = Response::new(Body::empty());
        *r.status_mut() = status;
        *r.headers_mut() = headers;
        return Sent::Answer(r);
    }
    let stream = up.bytes_stream().map_err(std::io::Error::other);
    let mut r = Response::new(Body::from_stream(stream));
    *r.status_mut() = status;
    *r.headers_mut() = headers;
    Sent::Answer(r)
}

/// The close frame `1008` (policy violation): the access ended.
pub const CLOSE_POLICY: [u8; 4] = [0x88, 0x02, 0x03, 0xF0];
/// The close frame `1001` (going away): idle.
pub const CLOSE_IDLE: [u8; 4] = [0x88, 0x02, 0x03, 0xE9];

/// Follows the frame borders of a WebSocket byte stream (RFC 6455 5.2), so the gateway writes
/// its own close frame only between two frames of the app.
#[derive(Debug, Default)]
pub struct FrameTracker {
    head: Vec<u8>,
    remaining: u64,
}

impl FrameTracker {
    /// Whether the stream is between two frames.
    pub fn at_border(&self) -> bool {
        self.head.is_empty() && self.remaining == 0
    }

    /// The length of a frame head and of its payload, once the head says both.
    fn parse(h: &[u8]) -> Option<(usize, u64)> {
        if h.len() < 2 {
            return None;
        }
        let masked = h[1] & 0x80 != 0;
        let short = h[1] & 0x7f;
        let ext = match short {
            126 => 2,
            127 => 8,
            _ => 0,
        };
        if h.len() < 2 + ext {
            return None;
        }
        let payload = match short {
            126 => u64::from(u16::from_be_bytes([h[2], h[3]])),
            127 => u64::from_be_bytes([h[2], h[3], h[4], h[5], h[6], h[7], h[8], h[9]]),
            n => u64::from(n),
        };
        Some((2 + ext + if masked { 4 } else { 0 }, payload))
    }

    /// Takes the next bytes of the stream.
    pub fn feed(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            if self.remaining > 0 {
                let n = usize::try_from(self.remaining)
                    .unwrap_or(usize::MAX)
                    .min(data.len());
                self.remaining -= n as u64;
                data = &data[n..];
                continue;
            }
            self.head.push(data[0]);
            data = &data[1..];
            if let Some((head, payload)) = Self::parse(&self.head)
                && self.head.len() == head
            {
                self.head.clear();
                self.remaining = payload;
            }
        }
    }
}

/// Joins the visitor's and the app's upgraded connections until one ends, the access ends or
/// the WebSocket is idle (see the module docs).
async fn splice(
    gw: Arc<Gateway>,
    site: Site,
    headers: HeaderMap,
    visitor_side: hyper::upgrade::OnUpgrade,
    app_side: reqwest::Response,
    _slot: Slot,
) {
    let cfg = gw.proxy.clone();
    let (app, visitor) = match tokio::join!(app_side.upgrade(), visitor_side) {
        (Ok(a), Ok(v)) => (a, hyper_util::rt::TokioIo::new(v)),
        _ => {
            tracing::warn!(target: "ruum::gateway", "a WebSocket upgrade did not complete");
            return;
        }
    };
    let (mut vr, mut vw) = tokio::io::split(visitor);
    let (mut ar, mut aw) = tokio::io::split(app);
    let mut frames = FrameTracker::default();
    let mut from_visitor = vec![0u8; 16 * 1024];
    let mut from_app = vec![0u8; 16 * 1024];
    let mut tick = tokio::time::interval(cfg.ws_recheck);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last = tokio::time::Instant::now();
    let close = loop {
        tokio::select! {
            r = vr.read(&mut from_visitor) => match r {
                Ok(0) | Err(_) => break None,
                Ok(n) => {
                    if aw.write_all(&from_visitor[..n]).await.is_err() || aw.flush().await.is_err() {
                        break None;
                    }
                    last = tokio::time::Instant::now();
                }
            },
            r = ar.read(&mut from_app) => match r {
                Ok(0) | Err(_) => break None,
                Ok(n) => {
                    frames.feed(&from_app[..n]);
                    if vw.write_all(&from_app[..n]).await.is_err() || vw.flush().await.is_err() {
                        break None;
                    }
                    last = tokio::time::Instant::now();
                }
            },
            _ = tick.tick() => {
                if last.elapsed() >= cfg.ws_idle {
                    break Some(CLOSE_IDLE);
                }
                if !crate::still_allowed(&gw, &site, &headers).await {
                    tracing::info!(target: "ruum::gateway", "an open WebSocket lost its access");
                    break Some(CLOSE_POLICY);
                }
            }
        }
    };
    if let Some(frame) = close
        && frames.at_border()
    {
        let _ = vw.write_all(&frame).await;
        let _ = vw.flush().await;
    }
    let _ = vw.shutdown().await;
    let _ = aw.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_borders_are_followed() {
        let mut t = FrameTracker::default();
        assert!(t.at_border());
        // A short unmasked text frame, fed byte by byte.
        for b in [0x81u8, 0x03, b'a', b'b'] {
            t.feed(&[b]);
            assert!(!t.at_border());
        }
        t.feed(b"c");
        assert!(t.at_border());
        // A 126 frame (300 bytes) and a masked empty frame in one write.
        let mut data = vec![0x82u8, 126, 0x01, 0x2c];
        data.extend(std::iter::repeat_n(7u8, 300));
        data.extend([0x89u8, 0x80, 1, 2, 3, 4]);
        t.feed(&data[..10]);
        assert!(!t.at_border());
        t.feed(&data[10..]);
        assert!(t.at_border());
        // A 127 frame head split in the middle of its length.
        let mut big = vec![0x82u8, 127, 0, 0, 0, 0];
        t.feed(&big);
        assert!(!t.at_border());
        big = vec![0, 0, 0, 2, 9, 9];
        t.feed(&big);
        assert!(t.at_border());
    }

    #[test]
    fn websocket_upgrades_are_recognized_narrowly() {
        let h = map(&[
            ("Upgrade", "websocket"),
            ("Connection", "keep-alive, Upgrade"),
        ]);
        assert!(is_websocket_upgrade(&Method::GET, &h));
        assert!(!is_websocket_upgrade(&Method::POST, &h));
        let h2c = map(&[("Upgrade", "h2c"), ("Connection", "Upgrade")]);
        assert!(!is_websocket_upgrade(&Method::GET, &h2c));
        let no_conn = map(&[("Upgrade", "websocket")]);
        assert!(!is_websocket_upgrade(&Method::GET, &no_conn));
        // The cleaning still drops both for every request; send adds them back only here.
        let out = clean_request(&h, false);
        assert!(out.get("upgrade").is_none() && out.get("connection").is_none());
    }

    #[test]
    fn a_websocket_needs_the_apps_own_origin() {
        let user = Visitor::Anonymous;
        let agent = Visitor::Agent {
            sub: "pairwise:a".into(),
            on_behalf_of: "pairwise:b".into(),
        };
        let host = "garden.acme.ruumapps.net";
        let ok = map(&[("Origin", "https://garden.acme.ruumapps.net")]);
        assert!(websocket_origin_ok(&ok, host, &user));
        for bad in [
            "https://evil.example",
            "http://garden.acme.ruumapps.net",
            "https://garden.acme.ruumapps.net.evil.example",
            "https://orchard.acme.ruumapps.net",
            "null",
        ] {
            assert!(
                !websocket_origin_ok(&map(&[("Origin", bad)]), host, &user),
                "{bad}"
            );
            assert!(
                !websocket_origin_ok(&map(&[("Origin", bad)]), host, &agent),
                "{bad}"
            );
        }
        assert!(!websocket_origin_ok(&HeaderMap::new(), host, &user));
        assert!(websocket_origin_ok(&HeaderMap::new(), host, &agent));
        let two = map(&[
            ("Origin", "https://garden.acme.ruumapps.net"),
            ("Origin", "https://evil.example"),
        ]);
        assert!(!websocket_origin_ok(&two, host, &user));
    }

    fn map(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (n, v) in pairs {
            h.append(
                HeaderName::from_bytes(n.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn requests_lose_ruum_forwarding_trace_and_hop_headers() {
        let h = map(&[
            ("Ruum-User", "x"),
            ("ruum_user", "x"),
            ("RUUM-Anything", "x"),
            ("X-Forwarded-For", "203.0.113.7"),
            ("x-forwarded-anything", "x"),
            ("Forwarded", "for=x"),
            ("X-Real-IP", "x"),
            ("tracestate", "x"),
            ("baggage", "x"),
            (
                "traceparent",
                "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
            ),
            ("Keep-Alive", "timeout=5"),
            ("TE", "trailers"),
            ("Proxy-Authorization", "x"),
            ("Connection", "close, X-Drop-Me"),
            ("X-Drop-Me", "x"),
            ("X-Keep", "kept"),
            ("Authorization", "Basic abc"),
            ("Host", "garden.acme.ruumapps.localhost"),
        ]);
        let out = clean_request(&h, false);
        let mut names: Vec<&str> = out.keys().map(|k| k.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["authorization", "x-keep"], "{names:?}");
        let out = clean_request(&h, true);
        assert!(out.get("authorization").is_none());
        let mut out = clean_request(&h, true);
        add_gateway_headers(&mut out, "tok", Uuid::now_v7(), &new_traceparent().0);
        assert_eq!(out.get_all("ruum-user").iter().count(), 1);
        assert_eq!(out.get_all("traceparent").iter().count(), 1);
        assert_ne!(
            out.get("traceparent").unwrap(),
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"
        );
    }

    #[test]
    fn the_gateways_cookies_are_removed_by_name_rules() {
        let h = map(&[
            ("Cookie", "__Host-ruum_app=s; theme=dark"),
            ("Cookie", "__Secure-ruum_app=x; sid=abc; RUUM_VISIT=y"),
            (
                "Cookie",
                "__host-Ruum_Visit=z; lang=et; ruum_app=w; __Secure-RUUM_visit=v; ;",
            ),
        ]);
        let out = clean_request(&h, false);
        let c: Vec<&str> = out
            .get_all("cookie")
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect();
        assert_eq!(c, vec!["theme=dark; sid=abc; lang=et"]);
        assert_eq!(gateway_cookie(&h, "__Host-ruum_app").as_deref(), Some("s"));
        assert_eq!(gateway_cookie(&h, "__Host-ruum_visit"), None);
        for n in [
            "__Host-ruum_app",
            "ruum_app",
            "RUUM_X",
            "__SECURE-ruumish",
            "__host-RUUM",
        ] {
            assert!(gateway_cookie_name(n), "{n}");
        }
        for n in ["theme", "__Host-app", "xruum", "__Host-"] {
            assert!(!gateway_cookie_name(n), "{n}");
        }
        assert_eq!(app_cookie_header(&map(&[("Cookie", "ruum_app=1")])), None);
    }

    #[test]
    fn an_apps_set_cookie_for_ruum_names_is_dropped() {
        for bad in [
            "ruum_app=1; Path=/",
            "__Host-ruum_app=1; Path=/; Secure",
            "RUUM_session=1",
            "Ruumish=1",
            "__host-RUUM_visit=1",
            "__Secure-ruum=1",
            "=ruum_app=x",
            "ruum_app_x",
            "t6bad8",
            " =x",
        ] {
            assert!(!app_set_cookie_allowed(bad), "{bad}");
        }
        for ok in [
            "theme=1; Path=/",
            "cart=2; HttpOnly",
            "__Host-app=1; Secure; Path=/",
        ] {
            assert!(app_set_cookie_allowed(ok), "{ok}");
        }
    }

    #[test]
    fn frame_ancestors_follow_the_environment() {
        let mut h = HeaderMap::new();
        security_headers(&mut h, Frame::Live, "https://ruum.localhost");
        assert_eq!(
            h.get("content-security-policy").unwrap(),
            "frame-ancestors 'none'"
        );
        assert_eq!(h.get("x-content-type-options").unwrap(), "nosniff");
        assert!(h.get("strict-transport-security").is_some());
        assert!(h.get("referrer-policy").is_some());
        let mut h = map(&[(
            "Content-Security-Policy",
            "frame-ancestors https://partner.example",
        )]);
        security_headers(&mut h, Frame::Live, "https://ruum.localhost");
        let v: Vec<_> = h.get_all("content-security-policy").iter().collect();
        assert_eq!(v, vec!["frame-ancestors https://partner.example"]);
        let mut h = map(&[(
            "Content-Security-Policy",
            "default-src 'self'; frame-ancestors https://partner.example",
        )]);
        security_headers(&mut h, Frame::Preview, "https://ruum.localhost/");
        let v: Vec<_> = h
            .get_all("content-security-policy")
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect();
        assert_eq!(
            v,
            vec![
                "default-src 'self'",
                "frame-ancestors https://ruum.localhost"
            ]
        );
    }

    #[test]
    fn responses_lose_hop_headers_and_ruum_cookies() {
        let h = map(&[
            ("Set-Cookie", "ruum_app=1"),
            ("Set-Cookie", "theme=dark"),
            ("Connection", "close, X-Hop"),
            ("X-Hop", "1"),
            ("Transfer-Encoding", "chunked"),
            ("Content-Type", "text/plain"),
        ]);
        let out = clean_response(&h, Frame::Live, "https://ruum.localhost");
        let sc: Vec<_> = out.get_all("set-cookie").iter().collect();
        assert_eq!(sc, vec!["theme=dark"]);
        assert!(out.get("x-hop").is_none() && out.get("transfer-encoding").is_none());
        assert_eq!(out.get("content-type").unwrap(), "text/plain");
    }

    #[test]
    fn traceparents_are_well_formed() {
        let (tp, id) = new_traceparent();
        let parts: Vec<&str> = tp.split('-').collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(
            (parts[0], parts[1].len(), parts[2].len(), parts[3]),
            ("00", 32, 16, "01")
        );
        assert_eq!(parts[1], id);
        assert_ne!(new_traceparent().1, id);
    }
}
