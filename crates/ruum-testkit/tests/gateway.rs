//! The gateway cases of card T-6 (`crates/ruum-testkit/src/gateway/`), the checks of the cases
//! themselves, and a self-test of the harness with two toy gateways.
//!
//! - Without a world every case reports planned (card G-1a implements `GatewayWorld` in
//!   `crates/ruum-gateway/tests/` and runs the same cases there).
//! - The meta checks: every case names its rule and its source, every pending entry names a case
//!   and a card, every path of `gateway.openapi.yaml` is named by a case, the reference token
//!   verifier refuses hand-made bad tokens.
//! - The self-test: a deliberately wrong toy gateway (forwards every header, passes the app's
//!   `Set-Cookie`, sets a cookie without `__Host-`, serves `..`) makes the cleaning, cookie and
//!   traversal cases fail; a toy that does it right makes them pass. So the cases can fail.

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, Response, StatusCode};
use ruum_testkit::contract;
use ruum_testkit::gateway::token::{self, Expect, TestSigner};
use ruum_testkit::gateway::{
    self, Access, AgentAudience, App, CARDS, Deployment, Env, GATEWAY_CONTRACT, GatewayHandle,
    GatewayWorld, MakeGatewayWorld, Outcome, PENDING, Relation, Req, Secret, SignIn, Tenant, User,
    WorldError, WorldResult,
};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ---------------------------------------------------------------------------------------------
// The cases without a world
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn without_a_world_every_gateway_case_is_planned() {
    let cases = gateway::gateway_cases();
    let rows = gateway::run_gateway_cases(&cases, None).await;
    let (text, ok) = gateway::render("gateway (no world)", &rows);
    println!("{text}");
    assert!(ok, "{text}");
    for (name, o) in &rows {
        assert!(matches!(o, Outcome::Planned(_)), "{name}: {o:?}");
    }
}

/// A world whose binary is missing: every case reports planned, also with
/// `RUUM_REQUIRE_IMPLEMENTED=1`.
#[tokio::test]
async fn a_missing_binary_reports_planned() {
    let make: MakeGatewayWorld = Arc::new(|| {
        Box::pin(async {
            Err::<Arc<dyn GatewayWorld>, _>(WorldError::Missing("ruum-gateway is not built".into()))
        })
    });
    let cases = gateway::gateway_cases();
    let rows = gateway::run_gateway_cases(&cases, Some(make)).await;
    for (name, o) in &rows {
        let ok = matches!(o, Outcome::Planned(_))
            || (matches!(o, Outcome::Pending(_)) && gateway::pending_reason(name).is_some());
        assert!(ok, "{name}: {o:?}");
    }
}

// ---------------------------------------------------------------------------------------------
// Meta checks
// ---------------------------------------------------------------------------------------------

#[test]
fn every_case_names_its_rule_and_its_source() {
    let mut names = BTreeSet::new();
    let gw: Vec<(&str, &str, &str)> = gateway::gateway_cases()
        .iter()
        .map(|c| (c.name, c.rule, c.source))
        .collect();
    let rt: Vec<(&str, &str, &str)> = gateway::runtime_cases()
        .iter()
        .map(|c| (c.name, c.rule, c.source))
        .collect();
    for (name, rule, source) in gw.into_iter().chain(rt) {
        assert!(names.insert(name), "two cases are named {name}");
        assert!(
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
            "{name} is not snake_case"
        );
        assert!(rule.len() >= 20, "{name} names no rule");
        assert!(
            [
                "decision 0006",
                "contracts/",
                ".openapi.yaml",
                "STEP3",
                "backlog",
                "card ",
                "design ",
                "plan seam",
                "schema.json"
            ]
            .iter()
            .any(|s| source.contains(s)),
            "{name}: its source {source:?} names no contract, decision, STEP3 section, card or \
             backlog item"
        );
    }
}

#[test]
fn every_pending_entry_names_a_case_and_a_card() {
    let names: BTreeSet<&str> = gateway::gateway_cases()
        .iter()
        .map(|c| c.name)
        .chain(gateway::runtime_cases().iter().map(|c| c.name))
        .collect();
    let mut seen = BTreeSet::new();
    for (case, reason) in PENDING {
        assert!(seen.insert(*case), "{case} is pending twice");
        assert!(
            names.contains(case),
            "PENDING names {case}, which is not a case"
        );
        assert!(
            CARDS.iter().any(|c| reason.starts_with(&format!("{c}:"))),
            "the pending reason of {case} ({reason:?}) does not start with a card of {CARDS:?}"
        );
    }
    // Every runtime API case waits for R-3 or G-3 (no runtime API world exists in this step).
    for c in gateway::runtime_cases() {
        let r = gateway::pending_reason(c.name).unwrap_or("");
        assert!(
            r.starts_with("R-3:") || r.starts_with("G-3:"),
            "the runtime API case {} must be pending with R-3 or G-3",
            c.name
        );
    }
}

#[test]
fn every_gateway_path_is_named_by_a_case() {
    let ops = contract::parse(GATEWAY_CONTRACT).expect("the gateway contract scans");
    assert!(!ops.is_empty());
    let named: BTreeSet<&str> = gateway::gateway_cases()
        .iter()
        .flat_map(|c| c.ops.iter().copied())
        .collect();
    let ids: BTreeSet<&str> = ops.iter().map(|o| o.id.as_str()).collect();
    for op in &ops {
        assert!(
            named.contains(op.id.as_str()),
            "no gateway case names {} ({} {})",
            op.id,
            op.method,
            op.path
        );
    }
    for n in &named {
        assert!(
            ids.contains(n),
            "a gateway case names {n}, which is not in gateway.openapi.yaml"
        );
    }
}

/// The reference token verifier refuses each hand-made bad token (card T-6, acceptance tests).
#[test]
fn the_reference_verifier_refuses_bad_tokens() {
    let iss = "https://ruum.localhost";
    let app = "0190b6c4-a00d-7cc1-9a4b-1f2e3d4c5b7d";
    let dep = "0190b6c4-a00d-7cc1-9a4b-1f2e3d4c5b7e";
    let s = TestSigner::new();
    let jwks = s.jwks();
    let live = Expect::live(iss, app);
    let good = s.sign(&token::user_claims(
        iss,
        &token::live_aud(app),
        "live",
        "acme",
        "user",
    ));
    token::verify_user_token(&good, &jwks, &live).expect("a good token verifies");
    let bad = token::hand_made_bad_tokens(&s, iss, app, dep);
    let kinds: Vec<&str> = bad.iter().map(|(k, _)| *k).collect();
    for want in [
        "wrong aud",
        "wrong env",
        "no iat",
        "exp - iat over 60 s",
        "unknown typ",
        "a foreign key",
    ] {
        assert!(kinds.contains(&want), "the bad tokens miss {want}");
    }
    for (what, tok) in bad {
        assert!(
            token::verify_user_token(&tok, &jwks, &live).is_err(),
            "the reference verifier accepted a token with {what}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The self-test: two toy gateways
// ---------------------------------------------------------------------------------------------

/// The cases the toys run: cleaning, cookies and traversal.
const SELF_TEST: &[&str] = &[
    "request_headers_are_cleaned",
    "incoming_traceparent_is_replaced",
    "gateway_cookies_never_reach_the_app",
    "app_set_cookie_for_ruum_names_is_dropped",
    "gateway_cookies_follow_browser_rules",
    "traversal_never_leaves_the_deployment",
];

#[tokio::test]
async fn a_wrong_toy_gateway_fails_the_cases() {
    let rows = run_toy(Mode::Wrong).await;
    let (text, _) = gateway::render("wrong toy gateway", &rows);
    println!("{text}");
    for (name, o) in &rows {
        assert!(o.is_failed(), "the wrong toy did not fail {name}: {o:?}");
    }
}

#[tokio::test]
async fn a_right_toy_gateway_passes_the_cases() {
    let rows = run_toy(Mode::Right).await;
    let (text, _) = gateway::render("right toy gateway", &rows);
    println!("{text}");
    for (name, o) in &rows {
        assert_eq!(
            *o,
            Outcome::Passed,
            "the right toy did not pass {name}:{text}"
        );
    }
}

async fn run_toy(mode: Mode) -> Vec<(&'static str, Outcome)> {
    let cases: Vec<_> = gateway::gateway_cases()
        .into_iter()
        .filter(|c| SELF_TEST.contains(&c.name))
        .collect();
    assert_eq!(cases.len(), SELF_TEST.len(), "a self-test case is missing");
    let make: MakeGatewayWorld = Arc::new(move || {
        Box::pin(async move { Ok(Arc::new(Toy::new(mode)) as Arc<dyn GatewayWorld>) })
    });
    gateway::run_gateway_cases(&cases, Some(make)).await
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Right,
    Wrong,
}

const DOMAIN: &str = "ruumapps.localhost";
const ISS: &str = "https://ruum.localhost";

#[derive(Clone)]
struct ToyApp {
    slug: String,
    tenant: String,
    access: Access,
    live: Option<String>,
}

#[derive(Clone)]
struct ToyDep {
    app: String,
    env: Env,
    short_id: String,
    files: HashMap<String, Vec<u8>>,
    spa: Option<String>,
    upstream: Option<SocketAddr>,
}

#[derive(Default)]
struct ToyState {
    tenants: HashMap<String, (String, bool)>,
    apps: HashMap<String, ToyApp>,
    deps: HashMap<String, ToyDep>,
    grants: HashSet<(String, String, String)>,
    /// session ID -> (user, deployment)
    sessions: HashMap<String, (String, String)>,
}

struct Toy {
    mode: Mode,
    st: Arc<Mutex<ToyState>>,
    signer: Arc<TestSigner>,
}

impl Toy {
    fn new(mode: Mode) -> Toy {
        Toy {
            mode,
            st: Arc::new(Mutex::new(ToyState::default())),
            signer: Arc::new(TestSigner::new()),
        }
    }
}

fn rel(r: &Relation) -> String {
    match r {
        Relation::View => "view".into(),
        Relation::Edit => "edit".into(),
        Relation::Role(n) => format!("role:{n}"),
    }
}

fn not_in_toy<T>() -> WorldResult<T> {
    Err(WorldError::Failed("the toy world does not do this".into()))
}

/// `(app id, deployment id)` of a host.
fn resolve(st: &ToyState, host: &str) -> Option<(String, String)> {
    let host = host.split(':').next()?.to_ascii_lowercase();
    let rest = host.strip_suffix(&format!(".{DOMAIN}"))?;
    let (label, tenant) = rest.split_once('.')?;
    if tenant.contains('.') {
        return None;
    }
    let (short, slug) = match label.split_once("--") {
        Some((s, a)) => (Some(s), a),
        None => (None, label),
    };
    let (app_id, app) = st
        .apps
        .iter()
        .find(|(_, a)| a.slug == slug && a.tenant == tenant)?;
    let dep = match short {
        None => app.live.clone()?,
        Some(s) => st
            .deps
            .iter()
            .find(|(_, d)| d.app == *app_id && d.short_id == s && d.env == Env::Preview)
            .map(|(id, _)| id.clone())?,
    };
    Some((app_id.clone(), dep))
}

#[async_trait]
impl GatewayWorld for Toy {
    async fn start(&self) -> WorldResult<GatewayHandle> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| WorldError::Failed(e.to_string()))?;
        let addr = listener.local_addr().expect("address");
        let (mode, st, signer) = (self.mode, self.st.clone(), self.signer.clone());
        let router = Router::new().fallback(move |req: Request| {
            let (st, signer) = (st.clone(), signer.clone());
            async move { toy_handle(mode, st, signer, req).await }
        });
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(GatewayHandle {
            addr,
            apps_domain: DOMAIN.into(),
            issuer: ISS.into(),
            user_token_jwks: self.signer.jwks(),
            ui_origin: ISS.into(),
            platform_host: "ruum.localhost".into(),
        })
    }

    async fn seed_tenant(&self, slug: &str) -> WorldResult<Tenant> {
        let id = uuid::Uuid::now_v7().to_string();
        self.st
            .lock()
            .unwrap()
            .tenants
            .insert(slug.into(), (id.clone(), false));
        Ok(Tenant {
            id,
            slug: slug.into(),
        })
    }

    async fn seed_user(&self, tenant: &Tenant, name: &str) -> WorldResult<User> {
        Ok(User {
            id: uuid::Uuid::now_v7().to_string(),
            name: name.into(),
            tenant: tenant.clone(),
        })
    }

    async fn seed_app(&self, tenant: &Tenant, slug: &str, access: Access) -> WorldResult<App> {
        let id = uuid::Uuid::now_v7().to_string();
        self.st.lock().unwrap().apps.insert(
            id.clone(),
            ToyApp {
                slug: slug.into(),
                tenant: tenant.slug.clone(),
                access,
                live: None,
            },
        );
        Ok(App {
            id,
            slug: slug.into(),
            tenant: tenant.clone(),
        })
    }

    async fn seed_deployment(
        &self,
        app: &App,
        env: Env,
        files: &[(String, Vec<u8>)],
        spa_fallback: Option<&str>,
    ) -> WorldResult<Deployment> {
        let id = uuid::Uuid::now_v7().to_string();
        let alphabet = b"abcdefghijklmnopqrstuvwxyz234567";
        let rnd = ruum_core::identity::secret::random_bytes();
        let short_id: String = rnd[..8]
            .iter()
            .map(|b| alphabet[(*b % 32) as usize] as char)
            .collect();
        self.st.lock().unwrap().deps.insert(
            id.clone(),
            ToyDep {
                app: app.id.clone(),
                env,
                short_id: short_id.clone(),
                files: files.iter().cloned().collect(),
                spa: spa_fallback.map(str::to_string),
                upstream: None,
            },
        );
        Ok(Deployment {
            id,
            short_id,
            env,
            app_id: app.id.clone(),
        })
    }

    async fn set_live(&self, app: &App, deployment: &Deployment) -> WorldResult<()> {
        let mut st = self.st.lock().unwrap();
        if let Some(a) = st.apps.get_mut(&app.id) {
            a.live = Some(deployment.id.clone());
        }
        Ok(())
    }

    async fn grant(&self, user: &User, app: &App, relation: &Relation) -> WorldResult<()> {
        self.st
            .lock()
            .unwrap()
            .grants
            .insert((user.id.clone(), app.id.clone(), rel(relation)));
        Ok(())
    }

    async fn revoke(&self, user: &User, app: &App, relation: &Relation) -> WorldResult<()> {
        self.st
            .lock()
            .unwrap()
            .grants
            .remove(&(user.id.clone(), app.id.clone(), rel(relation)));
        Ok(())
    }

    async fn sign_in(&self, user: &User, host: &str) -> WorldResult<SignIn> {
        let mut st = self.st.lock().unwrap();
        let (_, dep) = resolve(&st, host)
            .ok_or_else(|| WorldError::Failed(format!("the toy knows no host {host}")))?;
        let sid = ruum_core::identity::secret::random_secret("toy_");
        st.sessions.insert(sid.clone(), (user.id.clone(), dep));
        Ok(SignIn {
            set_cookies: vec![format!(
                "__Host-ruum_app={sid}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=43200"
            )],
        })
    }

    async fn sign_out_platform(&self, _user: &User) -> WorldResult<()> {
        not_in_toy()
    }

    async fn agent_token(&self, _user: &User, _audience: AgentAudience) -> WorldResult<Secret> {
        not_in_toy()
    }

    async fn partner_key(&self, _app: &App, _api: &str) -> WorldResult<Secret> {
        not_in_toy()
    }

    async fn set_upstream(&self, deployment: &Deployment, url: &str) -> WorldResult<()> {
        let addr: SocketAddr = url
            .trim_start_matches("http://")
            .parse()
            .map_err(|_| WorldError::Failed(format!("not an upstream address: {url}")))?;
        if let Some(d) = self.st.lock().unwrap().deps.get_mut(&deployment.id) {
            d.upstream = Some(addr);
        }
        Ok(())
    }

    async fn set_public_approval(&self, tenant: &Tenant, approved: bool) -> WorldResult<()> {
        if let Some(t) = self.st.lock().unwrap().tenants.get_mut(&tenant.slug) {
            t.1 = approved;
        }
        Ok(())
    }

    async fn reuse_host(&self, _removed: &App, _new: &App) -> WorldResult<App> {
        not_in_toy()
    }

    async fn rest_api_status(&self, _bearer: &str) -> WorldResult<u16> {
        not_in_toy()
    }
}

fn reply(status: u16, headers: Vec<(String, String)>, body: Vec<u8>) -> Response<Body> {
    let mut r = Response::new(Body::from(body));
    *r.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    for (n, v) in headers {
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(n.as_bytes()),
            HeaderValue::from_str(&v),
        ) {
            r.headers_mut().append(n, v);
        }
    }
    r
}

fn security_headers(env: Env) -> Vec<(String, String)> {
    let fa = match env {
        Env::Live => "'none'".to_string(),
        Env::Preview => ISS.to_string(),
    };
    vec![
        (
            "strict-transport-security".into(),
            "max-age=31536000".into(),
        ),
        ("x-content-type-options".into(), "nosniff".into()),
        (
            "referrer-policy".into(),
            "strict-origin-when-cross-origin".into(),
        ),
        (
            "content-security-policy".into(),
            format!("frame-ancestors {fa}"),
        ),
    ]
}

fn media_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css",
        "js" => "text/javascript",
        "png" => "image/png",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Whether a cookie name is one of the gateway's (after an optional `__Host-`/`__Secure-`).
fn gateway_cookie(name: &str) -> bool {
    let l = name.trim().to_ascii_lowercase();
    let l = l
        .strip_prefix("__host-")
        .or_else(|| l.strip_prefix("__secure-"))
        .unwrap_or(&l);
    l == "ruum_app" || l == "ruum_visit"
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

async fn toy_handle(
    mode: Mode,
    st: Arc<Mutex<ToyState>>,
    signer: Arc<TestSigner>,
    req: Request,
) -> Response<Body> {
    let (parts, body) = req.into_parts();
    let body = axum::body::to_bytes(body, 1 << 20)
        .await
        .map(|b| b.to_vec())
        .unwrap_or_default();
    let host = parts
        .headers
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();
    let path = parts.uri.path().to_string();
    let target = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| path.clone());
    let headers: Vec<(String, String)> = parts
        .headers
        .iter()
        .map(|(n, v)| {
            (
                n.as_str().to_string(),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect();
    let cookie_pairs: Vec<(String, String)> = headers
        .iter()
        .filter(|(n, _)| n == "cookie")
        .flat_map(|(_, v)| {
            v.split(';')
                .filter_map(|p| {
                    p.trim()
                        .split_once('=')
                        .map(|(a, b)| (a.to_string(), b.to_string()))
                })
                .collect::<Vec<_>>()
        })
        .collect();

    // Host, access check.
    let (app_id, dep_id, app, dep, tenant_ok, user) = {
        let st = st.lock().unwrap();
        let Some((app_id, dep_id)) = resolve(&st, &host) else {
            return reply(404, vec![], b"toy: not found".to_vec());
        };
        let app = st.apps[&app_id].clone();
        let dep = st.deps[&dep_id].clone();
        let tenant_ok = st.tenants.get(&app.tenant).map(|t| t.1).unwrap_or(false);
        let user = cookie_pairs
            .iter()
            .find(|(n, _)| n == "__Host-ruum_app")
            .and_then(|(_, sid)| st.sessions.get(sid))
            .filter(|(_, d)| *d == dep_id)
            .map(|(u, _)| u.clone())
            .filter(|u| {
                let need = if dep.env == Env::Live { "view" } else { "edit" };
                st.grants
                    .contains(&(u.clone(), app_id.clone(), need.to_string()))
            });
        (app_id, dep_id, app, dep, tenant_ok, user)
    };
    if path.starts_with("/.ruum/") {
        return reply(404, security_headers(dep.env), b"toy: reserved".to_vec());
    }
    let allowed = match app.access {
        Access::Public => tenant_ok,
        Access::Platform => user.is_some(),
    };
    if !allowed {
        return reply(
            302,
            vec![("location".into(), "/.ruum/v1/auth/start?return_to=/".into())],
            vec![],
        );
    }

    // The upstream.
    if let Some(up) = dep.upstream {
        let mut fwd = Req::new(parts.method.as_str(), &up.to_string(), &target).body(body);
        match mode {
            Mode::Wrong => {
                for (n, v) in &headers {
                    if n != "host" && n != "content-length" {
                        fwd = fwd.header(n, v);
                    }
                }
            }
            Mode::Right => {
                let conn: Vec<String> = headers
                    .iter()
                    .filter(|(n, _)| n == "connection")
                    .flat_map(|(_, v)| {
                        v.split(',')
                            .map(|s| s.trim().to_ascii_lowercase())
                            .collect::<Vec<_>>()
                    })
                    .collect();
                let hop = [
                    "connection",
                    "keep-alive",
                    "proxy-authenticate",
                    "proxy-authorization",
                    "te",
                    "trailer",
                    "transfer-encoding",
                    "upgrade",
                    "host",
                    "content-length",
                    "forwarded",
                    "x-real-ip",
                    "tracestate",
                    "baggage",
                    "traceparent",
                    "cookie",
                ];
                for (n, v) in &headers {
                    let l = n.to_ascii_lowercase();
                    if l.starts_with("ruum-")
                        || l.starts_with("ruum_")
                        || l.starts_with("x-forwarded-")
                        || hop.contains(&l.as_str())
                        || conn.contains(&l)
                    {
                        continue;
                    }
                    fwd = fwd.header(n, v);
                }
                let kept: Vec<String> = cookie_pairs
                    .iter()
                    .filter(|(n, _)| !gateway_cookie(n))
                    .map(|(n, v)| format!("{n}={v}"))
                    .collect();
                if !kept.is_empty() {
                    fwd = fwd.header("cookie", &kept.join("; "));
                }
                let (aud, env) = match dep.env {
                    Env::Live => (token::live_aud(&app_id), "live"),
                    Env::Preview => (token::preview_aud(&app_id, &dep_id), "preview"),
                };
                let actor = if user.is_some() { "user" } else { "anonymous" };
                let claims = token::user_claims(ISS, &aud, env, &app.tenant, actor);
                fwd = fwd
                    .header("Ruum-User", &signer.sign(&claims))
                    .header("Ruum-Request-Id", &uuid::Uuid::now_v7().to_string());
                let r = ruum_core::identity::secret::random_bytes();
                fwd = fwd.header(
                    "traceparent",
                    &format!("00-{}-{}-01", hex(&r[..16]), hex(&r[16..24])),
                );
            }
        }
        let Ok(resp) = ruum_testkit::gateway::http::send(up, &fwd, Duration::from_secs(10)).await
        else {
            return reply(502, vec![], b"toy: upstream".to_vec());
        };
        let mut out: Vec<(String, String)> = Vec::new();
        for (n, v) in resp.headers {
            if ["content-length", "transfer-encoding", "connection"].contains(&n.as_str()) {
                continue;
            }
            if mode == Mode::Right && n == "set-cookie" {
                let name = v.split(';').next().unwrap_or("");
                let Some((name, _)) = name.split_once('=') else {
                    continue;
                };
                let l = name.trim().to_ascii_lowercase();
                if l.is_empty() || l.starts_with("ruum") || l.starts_with("__host-ruum") {
                    continue;
                }
            }
            out.push((n, v));
        }
        match mode {
            Mode::Right => out.extend(security_headers(dep.env)),
            Mode::Wrong => out.push((
                "set-cookie".into(),
                format!("ruum_visit=toy; Path=/; Domain={DOMAIN}"),
            )),
        }
        return reply(resp.status, out, resp.body);
    }

    // Static files.
    match mode {
        Mode::Right => {
            if path.contains('%') || path.contains('\\') || path.chars().any(|c| c.is_control()) {
                return reply(400, security_headers(dep.env), b"toy: bad path".to_vec());
            }
            let p = path.strip_prefix('/').unwrap_or(&path);
            let (p, dir) = match p.strip_suffix('/') {
                Some(x) => (x, true),
                None => (p, p.is_empty()),
            };
            let segs: Vec<&str> = if p.is_empty() {
                vec![]
            } else {
                p.split('/').collect()
            };
            if segs.iter().any(|s| s.is_empty() || *s == "." || *s == "..") {
                return reply(400, security_headers(dep.env), b"toy: bad path".to_vec());
            }
            let key = segs.join("/");
            let index = if key.is_empty() {
                "index.html".to_string()
            } else {
                format!("{key}/index.html")
            };
            let found = if dir {
                dep.files.get(&index).map(|b| (index.clone(), b.clone()))
            } else {
                dep.files
                    .get(&key)
                    .map(|b| (key.clone(), b.clone()))
                    .or_else(|| dep.files.get(&index).map(|b| (index.clone(), b.clone())))
            };
            let navigation = headers
                .iter()
                .any(|(n, v)| n == "sec-fetch-mode" && v == "navigate");
            let found = found.or_else(|| {
                let spa = dep.spa.clone().filter(|_| navigation)?;
                dep.files.get(&spa).map(|b| (spa.clone(), b.clone()))
            });
            match found {
                Some((k, b)) => {
                    let mut h = security_headers(dep.env);
                    h.push(("content-type".into(), media_type(&k).into()));
                    reply(200, h, b)
                }
                None => reply(404, security_headers(dep.env), b"toy: not found".to_vec()),
            }
        }
        Mode::Wrong => {
            // Decodes the path and resolves `..` over one key space of every deployment:
            // `<app id>/<deployment id>/<path>`.
            let decoded = percent_decode(&path).replace('\\', "/");
            let mut stack: Vec<String> = vec![app_id.clone(), dep_id.clone()];
            for seg in decoded.split('/') {
                match seg {
                    "" | "." => {}
                    ".." => {
                        stack.pop();
                    }
                    s => stack.push(s.to_string()),
                }
            }
            if decoded.ends_with('/') || stack.len() == 2 {
                stack.push("index.html".into());
            }
            let st = st.lock().unwrap();
            let file = (stack.len() >= 3)
                .then(|| {
                    let (a, d) = (&stack[0], &stack[1]);
                    st.deps
                        .get(d)
                        .filter(|dd| dd.app == *a)
                        .and_then(|dd| dd.files.get(&stack[2..].join("/")).cloned())
                })
                .flatten();
            match file {
                Some(b) => reply(200, vec![], b),
                None => reply(404, vec![], b"toy: not found".to_vec()),
            }
        }
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn the_toy_resolves_hosts() {
    let mut st = ToyState::default();
    st.apps.insert(
        "a1".into(),
        ToyApp {
            slug: "garden".into(),
            tenant: "acme".into(),
            access: Access::Platform,
            live: Some("d1".into()),
        },
    );
    assert_eq!(
        resolve(&st, "garden.acme.ruumapps.localhost"),
        Some(("a1".into(), "d1".into()))
    );
    assert_eq!(resolve(&st, "x.garden.acme.ruumapps.localhost"), None);
    assert_eq!(percent_decode("/%2e%2e/a%2Fb"), "/../a/b");
}
