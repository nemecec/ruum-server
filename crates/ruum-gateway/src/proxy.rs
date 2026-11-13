//! Cleaning both ways (decision 0006 G point 2; `gateway.openapi.yaml` "The check of every
//! request" 3 and 4) and the fixed test upstream. Card G-1b adds the proxy to a running sandbox
//! and reuses the cleaning functions.
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

use http::header::{HeaderMap, HeaderName, HeaderValue};
use std::collections::HashMap;
use std::sync::RwLock;
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

#[cfg(test)]
mod tests {
    use super::*;

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
