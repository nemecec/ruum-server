//! Sign-in on the app origin, the app session and visits (`gateway.openapi.yaml`, "Sign-in on an
//! app origin" and "Visits"; `ruum_core::identity::app_sign_in` has the platform side).
//!
//! - `GET /.ruum/v1/auth/start?return_to=<path>`: `return_to` must be a path on this origin
//!   ([`valid_return_to`]; `//host`, a scheme, a backslash or a control byte is `400`). The gateway
//!   seals a state (the host, `return_to`, a nonce, the tenant, the app and the deployment the
//!   host resolves to; 10 minutes), puts the nonce in `__Host-ruum_auth`, and sends the browser to
//!   the platform's `loginStart` with the app audience.
//! - `GET /.ruum/v1/auth/callback?code&state`: the state must open, name this host and the app
//!   (and the preview) the host resolves to now, and carry the nonce of the browser's
//!   `__Host-ruum_auth`; then the core redeems the code once. The answer sets `__Host-ruum_app`
//!   (a random session secret, never a token; 12 hours) and redirects to `return_to`. Anything
//!   else is `400` and sets no session.
//! - `POST /.ruum/v1/auth/logout`: ends the session in the core (`204`).
//!
//! Every cookie of the gateway is `__Host-`, `Secure`, `HttpOnly`, `SameSite=Lax`, `Path=/`,
//! without `Domain` (backlog S-41). The visit cookie `__Host-ruum_visit` ends with the browser
//! session; [`Visits`] maps each visit to the trace IDs the gateway started (bounded), for the
//! browser telemetry (G-2) and the report origin (G-4).

use crate::core_api::credential_key;
use crate::proxy::gateway_cookie;
use crate::{Gateway, Site, pages};
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ruum_core::identity::app_sign_in::AppEnv;
use ruum_core::identity::app_sign_in::wire::RedeemRequest;
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

/// The app session cookie (a session secret).
pub const APP_COOKIE: &str = "__Host-ruum_app";
/// The visit cookie (ends with the browser session).
pub const VISIT_COOKIE: &str = "__Host-ruum_visit";
/// The cookie with the nonce of a sign-in in progress.
pub const AUTH_COOKIE: &str = "__Host-ruum_auth";
/// How long a sign-in may take from start to callback.
pub const STATE_SECONDS: i64 = 600;

/// A `Set-Cookie` value of the gateway: `__Host-` rules, `HttpOnly`, `SameSite=Lax`.
pub fn set_cookie(name: &str, value: &str, max_age: Option<i64>) -> String {
    let mut c = format!("{name}={value}; Path=/; Secure; HttpOnly; SameSite=Lax");
    if let Some(m) = max_age {
        c.push_str(&format!("; Max-Age={m}"));
    }
    c
}

/// A random value for a cookie: 32 bytes as base64url (43 characters).
pub fn random_value() -> String {
    let mut b = [0u8; 32];
    rand::fill(&mut b[..]);
    URL_SAFE_NO_PAD.encode(b)
}

fn is_random_value(v: &str) -> bool {
    v.len() == 43
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Whether `r` is a path on this origin for `return_to`: `/`, or `/` followed by anything but
/// `/` or `\`, with printable ASCII only and no `\` anywhere (a browser reads `/\host` as
/// `//host`).
pub fn valid_return_to(r: &str) -> bool {
    if !r.starts_with('/') || r.len() > 2048 {
        return false;
    }
    if !r.bytes().all(|b| (0x21..=0x7e).contains(&b)) || r.contains('\\') {
        return false;
    }
    r == "/" || !r[1..].starts_with('/')
}

/// The query parameters of a request, decoded.
fn query(uri: &axum::http::Uri) -> HashMap<String, String> {
    url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
        .into_owned()
        .collect()
}

fn redirect(location: &str) -> Response {
    let mut r = Response::new(Body::empty());
    *r.status_mut() = StatusCode::FOUND;
    if let Ok(v) = HeaderValue::from_str(location) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn add_cookie(r: &mut Response, c: &str) {
    if let Ok(v) = HeaderValue::from_str(c) {
        r.headers_mut().append(header::SET_COOKIE, v);
    }
}

/// The location of the gateway's sign-in start for a request target.
pub fn sign_in_location(target: &str) -> String {
    let rt = if valid_return_to(target) { target } else { "/" };
    let q: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("return_to", rt)
        .finish();
    format!("/.ruum/v1/auth/start?{q}")
}

/// A path under `/.ruum/` (they all belong to the gateway).
pub async fn gateway_path(
    gw: &Gateway,
    site: &Site,
    method: &Method,
    uri: &axum::http::Uri,
    headers: &HeaderMap,
) -> Response {
    match (method.as_str(), uri.path()) {
        ("GET", "/.ruum/v1/auth/start") => start(gw, site, uri),
        ("GET", "/.ruum/v1/auth/callback") => callback(gw, site, uri, headers).await,
        ("POST", "/.ruum/v1/auth/logout") => logout(gw, site, headers).await,
        // The browser script and telemetry (G-2) and the report handoff (G-4) are not built
        // yet; like every other path here, they never reach the app.
        _ => pages::not_found(site.look()),
    }
}

fn start(gw: &Gateway, site: &Site, uri: &axum::http::Uri) -> Response {
    let q = query(uri);
    let return_to = q.get("return_to").map(String::as_str).unwrap_or("/");
    if !valid_return_to(return_to) {
        return pages::bad_request(site.look());
    }
    let nonce = random_value();
    let state = json!({
        "host": site.name,
        "rt": return_to,
        "n": nonce,
        "t": site.resolved.tenant_id,
        "a": site.resolved.app_id,
        "d": site.resolved.deployment_id,
    });
    let Ok(sealed) = gw.tokens.seal(&state, STATE_SECONDS) else {
        return pages::unavailable(site.look());
    };
    let login: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("app_tenant", &site.resolved.tenant_id.to_string())
        .append_pair("app_id", &site.resolved.app_id.to_string())
        .append_pair("app_deployment", &site.resolved.deployment_id.to_string())
        .append_pair("app_state", &sealed)
        .finish();
    let mut r = redirect(&format!(
        "{}/v1/auth/login?{login}",
        gw.config.platform_url.trim_end_matches('/')
    ));
    add_cookie(
        &mut r,
        &set_cookie(AUTH_COOKIE, &nonce, Some(STATE_SECONDS)),
    );
    crate::proxy::security_headers(r.headers_mut(), site.frame(), &gw.config.ui_origin);
    r
}

async fn callback(
    gw: &Gateway,
    site: &Site,
    uri: &axum::http::Uri,
    headers: &HeaderMap,
) -> Response {
    let refuse = || {
        let mut r = pages::bad_request(site.look());
        add_cookie(&mut r, &set_cookie(AUTH_COOKIE, "", Some(0)));
        r
    };
    let q = query(uri);
    let (Some(code), Some(state)) = (q.get("code"), q.get("state")) else {
        return refuse();
    };
    let Some(v) = gw.tokens.open(state) else {
        return refuse();
    };
    let same_app = v["host"] == site.name.as_str()
        && v["t"] == json!(site.resolved.tenant_id)
        && v["a"] == json!(site.resolved.app_id)
        && (site.info.environment == AppEnv::Live || v["d"] == json!(site.resolved.deployment_id));
    let nonce_ok = match (v["n"].as_str(), gateway_cookie(headers, AUTH_COOKIE)) {
        (Some(n), Some(c)) => {
            ruum_core::identity::secret::constant_time_eq(n.as_bytes(), c.as_bytes())
        }
        _ => false,
    };
    let Some(return_to) = v["rt"].as_str().filter(|r| valid_return_to(r)) else {
        return refuse();
    };
    if !same_app || !nonce_ok {
        return refuse();
    }
    let redeemed = gw
        .core
        .redeem(&RedeemRequest {
            binding: site.binding,
            code: code.clone(),
            state: state.clone(),
        })
        .await;
    let session = match redeemed {
        Ok(Some(s)) if is_random_value(&s.session) => s,
        Ok(_) => return refuse(),
        Err(e) => {
            tracing::warn!(target: "ruum::gateway", error = %e, "the sign-in callback failed");
            return pages::unavailable(site.look());
        }
    };
    let mut r = redirect(return_to);
    add_cookie(
        &mut r,
        &set_cookie(APP_COOKIE, &session.session, Some(session.max_age)),
    );
    add_cookie(&mut r, &set_cookie(AUTH_COOKIE, "", Some(0)));
    crate::proxy::security_headers(r.headers_mut(), site.frame(), &gw.config.ui_origin);
    r
}

async fn logout(gw: &Gateway, site: &Site, headers: &HeaderMap) -> Response {
    if let Some(secret) = gateway_cookie(headers, APP_COOKIE)
        && let Err(e) = gw.core.end_session(site.resolved.tenant_id, &secret).await
    {
        tracing::warn!(target: "ruum::gateway", error = %e, "the logout failed");
        return pages::unavailable(site.look());
    }
    let mut r = Response::new(Body::empty());
    *r.status_mut() = StatusCode::NO_CONTENT;
    add_cookie(&mut r, &set_cookie(APP_COOKIE, "", Some(0)));
    crate::proxy::security_headers(r.headers_mut(), site.frame(), &gw.config.ui_origin);
    r
}

/// The visit of a request: the value of `__Host-ruum_visit`, or a new one (then the answer must
/// set the cookie: the second value is true).
pub fn visit_of(headers: &HeaderMap) -> (String, bool) {
    match gateway_cookie(headers, VISIT_COOKIE).filter(|v| is_random_value(v)) {
        Some(v) => (v, false),
        None => (random_value(), true),
    }
}

/// The `Set-Cookie` of a new visit.
pub fn visit_cookie(visit: &str) -> String {
    set_cookie(VISIT_COOKIE, visit, None)
}

/// The anonymous subject of a visit (`anonymous:<id>`): a hash of the visit, so the app never
/// learns the cookie's value.
pub fn anonymous_subject(visit: &str) -> String {
    let h = blake3::hash(format!("ruum-visit-subject\0{visit}").as_bytes());
    format!("anonymous:{}", URL_SAFE_NO_PAD.encode(&h.as_bytes()[..16]))
}

/// The most visits [`Visits`] keeps.
pub const VISITS_MAX: usize = 100_000;
/// The most trace IDs it keeps per visit.
pub const TRACES_PER_VISIT: usize = 64;

/// A trace that the gateway started for a visit (and an app session, if any).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisitTrace {
    pub trace_id: String,
    /// The hash of the app session secret, never the secret.
    pub session: Option<[u8; 32]>,
}

#[derive(Default)]
struct VisitMap {
    order: VecDeque<[u8; 32]>,
    traces: HashMap<[u8; 32], VecDeque<VisitTrace>>,
}

/// The server-side map of the trace IDs that the gateway started per visit (and session).
/// Telemetry and reports attach records only through it, never through a visit or trace ID that
/// the client sends (review R1). Bounded: the oldest visits and traces go first.
#[derive(Default)]
pub struct Visits {
    inner: Mutex<VisitMap>,
}

impl Visits {
    pub fn record(&self, visit: &str, session: Option<&str>, trace_id: String) {
        let key = credential_key(visit);
        let mut m = self.inner.lock().expect("visits lock");
        if !m.traces.contains_key(&key) {
            if m.order.len() >= VISITS_MAX
                && let Some(old) = m.order.pop_front()
            {
                m.traces.remove(&old);
            }
            m.order.push_back(key);
        }
        let list = m.traces.entry(key).or_default();
        if list.len() >= TRACES_PER_VISIT {
            list.pop_front();
        }
        list.push_back(VisitTrace {
            trace_id,
            session: session.map(credential_key),
        });
    }

    /// The traces of a visit, oldest first.
    pub fn traces(&self, visit: &str) -> Vec<VisitTrace> {
        let m = self.inner.lock().expect("visits lock");
        m.traces
            .get(&credential_key(visit))
            .map(|l| l.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// How many visits the map holds.
    pub fn len(&self) -> usize {
        self.inner.lock().expect("visits lock").order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn return_to_stays_on_the_origin() {
        for ok in ["/", "/a", "/a/b?c=d&e=%2F", "/.ruum/x", "/a//b"] {
            assert!(valid_return_to(ok), "{ok}");
        }
        for bad in [
            "",
            "a",
            "//evil",
            "//evil/x",
            "/\\evil",
            "/a\\b",
            "https://evil/",
            "javascript:alert(1)",
            "/a b",
            "/a\r\nSet-Cookie: x=1",
            "/\u{7f}",
            "/é",
        ] {
            assert!(!valid_return_to(bad), "{bad:?}");
        }
        assert!(!valid_return_to(&format!("/{}", "a".repeat(2048))));
    }

    #[test]
    fn the_sign_in_location_carries_a_safe_return_to() {
        assert_eq!(
            sign_in_location("/private?q=1"),
            "/.ruum/v1/auth/start?return_to=%2Fprivate%3Fq%3D1"
        );
        assert_eq!(
            sign_in_location("//evil"),
            "/.ruum/v1/auth/start?return_to=%2F"
        );
    }

    #[test]
    fn gateway_cookies_follow_host_rules() {
        let c = set_cookie(APP_COOKIE, "v", Some(43200));
        assert_eq!(
            c,
            "__Host-ruum_app=v; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=43200"
        );
        assert!(!visit_cookie("v").contains("Max-Age"));
        assert!(!c.to_ascii_lowercase().contains("domain"));
        for n in [APP_COOKIE, VISIT_COOKIE, AUTH_COOKIE] {
            assert!(n.starts_with("__Host-ruum"));
            assert!(crate::proxy::gateway_cookie_name(n));
        }
    }

    #[test]
    fn visits_are_random_and_their_subjects_hide_them() {
        let (v, new) = visit_of(&HeaderMap::new());
        assert!(new && is_random_value(&v));
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("{VISIT_COOKIE}={v}")).unwrap(),
        );
        assert_eq!(visit_of(&h), (v.clone(), false));
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("__Host-ruum_visit=short"),
        );
        assert!(visit_of(&h).1);
        let s = anonymous_subject(&v);
        assert!(s.starts_with("anonymous:") && !s.contains(&v));
        assert_eq!(s.len(), "anonymous:".len() + 22);
    }

    #[test]
    fn the_visit_map_is_bounded() {
        let m = Visits::default();
        let v = random_value();
        for i in 0..(TRACES_PER_VISIT + 5) {
            m.record(&v, Some("s"), format!("{i:032x}"));
        }
        let t = m.traces(&v);
        assert_eq!(t.len(), TRACES_PER_VISIT);
        assert_eq!(t[0].trace_id, format!("{:032x}", 5));
        assert_eq!(t[0].session, Some(credential_key("s")));
        assert_eq!(m.len(), 1);
        assert!(m.traces("other").is_empty());
    }
}
