//! The gateway cases of card T-6 (`crates/ruum-testkit/src/gateway/`), the checks of the cases
//! themselves, and a self-test of the harness with two toy gateways.
//!
//! - Without a world every case reports planned (card G-1a implements `GatewayWorld` in
//!   `crates/ruum-gateway/tests/` and runs the same cases there).
//! - The meta checks: every case names its rule and its source, every pending entry names a case
//!   and a card, every path of `gateway.openapi.yaml` is named by a case, the reference token
//!   verifier refuses hand-made bad tokens.
//! - The outcome rules: a world cannot switch a case off (a `Missing` after `start()`, or a
//!   `Pending` that `WORLD_PENDING` does not allow, fails the case).
//! - The self-test: a deliberately wrong toy gateway (forwards every header, passes the app's
//!   `Set-Cookie`, sets a cookie without `__Host-`, serves `..`, accepts a sign-in callback on
//!   another host, checks the `Host`'s app but forwards an absolute-form target to the app its
//!   authority names) makes the cleaning, cookie, traversal, sign-in and framing cases fail, each
//!   for its intended reason; a toy that does it right makes them pass. So the cases can fail.

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, Response, StatusCode};
use ruum_testkit::contract;
use ruum_testkit::gateway::token::{self, Expect, TestSigner};
use ruum_testkit::gateway::{
    self, Access, AgentAudience, App, CARDS, Deployment, Env, GATEWAY_CONTRACT, GatewayHandle,
    GatewayWorld, MakeGatewayWorld, Outcome, PENDING, Relation, Req, Secret, Tenant, User,
    WORLD_PENDING, WORLD_PENDING_STEPS, WorldError, WorldResult,
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

/// A world that starts, and then answers every step with the same error.
struct Stub(WorldError);

impl Stub {
    fn err<T>(&self) -> WorldResult<T> {
        Err(self.0.clone())
    }
}

#[async_trait]
impl GatewayWorld for Stub {
    async fn start(&self) -> WorldResult<GatewayHandle> {
        Ok(GatewayHandle {
            // Nothing listens there: a case that sends a request fails.
            addr: "127.0.0.1:9".parse().expect("address"),
            apps_domain: DOMAIN.into(),
            issuer: ISS.into(),
            user_token_jwks: TestSigner::new().jwks(),
            ui_origin: ISS.into(),
            platform_host: "ruum.localhost".into(),
        })
    }
    async fn seed_tenant(&self, _: &str) -> WorldResult<Tenant> {
        self.err()
    }
    async fn seed_user(&self, _: &Tenant, _: &str) -> WorldResult<User> {
        self.err()
    }
    async fn seed_member(&self, _: &User, _: &Tenant) -> WorldResult<()> {
        self.err()
    }
    async fn seed_app(&self, _: &Tenant, _: &str, _: Access) -> WorldResult<App> {
        self.err()
    }
    async fn seed_deployment(
        &self,
        _: &App,
        _: Env,
        _: &[(String, Vec<u8>)],
        _: Option<&str>,
    ) -> WorldResult<Deployment> {
        self.err()
    }
    async fn set_live(&self, _: &App, _: &Deployment) -> WorldResult<()> {
        self.err()
    }
    async fn grant(&self, _: &User, _: &App, _: &Relation) -> WorldResult<()> {
        self.err()
    }
    async fn revoke(&self, _: &User, _: &App, _: &Relation) -> WorldResult<()> {
        self.err()
    }
    async fn platform_authorize(&self, _: &User, _: &str) -> WorldResult<String> {
        self.err()
    }
    async fn sign_out_platform(&self, _: &User) -> WorldResult<()> {
        self.err()
    }
    async fn agent_token(&self, _: &User, _: AgentAudience) -> WorldResult<Secret> {
        self.err()
    }
    async fn partner_key(&self, _: &App, _: &str) -> WorldResult<Secret> {
        self.err()
    }
    async fn set_upstream(&self, _: &Deployment, _: &str) -> WorldResult<()> {
        self.err()
    }
    async fn set_public_approval(&self, _: &Tenant, _: bool) -> WorldResult<()> {
        self.err()
    }
    async fn reuse_host(&self, _: &App, _: &App) -> WorldResult<App> {
        self.err()
    }
    async fn rest_api_status(&self, _: &str) -> WorldResult<u16> {
        self.err()
    }
}

/// A world that answers `Missing` or `Pending` from its seeding steps (after `start()`) cannot
/// make a case planned or pending: every case that runs fails.
#[tokio::test]
async fn a_world_cannot_switch_cases_off() {
    let cases = gateway::gateway_cases();
    for err in [
        WorldError::Missing("not built".into()),
        WorldError::Pending {
            card: "B-2".into(),
            why: "roles".into(),
        },
        WorldError::Pending {
            card: "G-1a".into(),
            why: "later".into(),
        },
        WorldError::Pending {
            card: "Z-9".into(),
            why: "no such card".into(),
        },
    ] {
        let e = err.clone();
        let make: MakeGatewayWorld = Arc::new(move || {
            let e = e.clone();
            Box::pin(async move { Ok(Arc::new(Stub(e)) as Arc<dyn GatewayWorld>) })
        });
        let rows = gateway::run_gateway_cases(&cases, Some(make)).await;
        for (name, o) in &rows {
            if gateway::pending_reason(name).is_some() {
                assert!(matches!(o, Outcome::Pending(_)), "{name}: {o:?}");
            } else {
                assert!(o.is_failed(), "{err:?} switched {name} off: {o:?}");
            }
        }
    }
    // Pending from make() is not planned either.
    let make: MakeGatewayWorld = Arc::new(|| {
        Box::pin(async {
            Err::<Arc<dyn GatewayWorld>, _>(WorldError::Pending {
                card: "B-2".into(),
                why: "x".into(),
            })
        })
    });
    let rows = gateway::run_gateway_cases(&cases, Some(make)).await;
    for (name, o) in &rows {
        assert!(
            o.is_failed() || gateway::pending_reason(name).is_some(),
            "{name}: {o:?}"
        );
    }
}

/// The three app role cases. They waited for B-2; B-2 is merged (backlog P-68).
const ROLE_CASES: &[&str] = &[
    "an_active_app_role_gives_use",
    "a_removed_role_stops_within_5_seconds",
    "the_roles_claim_lists_active_app_roles",
];

/// An app role step may no longer answer pending (B-2 is merged): pending B-2, pending with
/// another card or `Missing` from granting a role fails each role case, and no world may answer
/// pending in them.
#[tokio::test]
async fn an_app_role_step_may_not_answer_pending() {
    let role_cases: Vec<_> = gateway::gateway_cases()
        .into_iter()
        .filter(|c| ROLE_CASES.contains(&c.name))
        .collect();
    assert_eq!(role_cases.len(), ROLE_CASES.len(), "a role case is missing");
    for name in ROLE_CASES {
        assert!(
            !WORLD_PENDING.iter().any(|(c, _)| c == name),
            "WORLD_PENDING still lets a world switch {name} off"
        );
        assert!(
            gateway::pending_reason(name).is_none(),
            "PENDING still lists {name}"
        );
    }
    for answer in [
        WorldError::Pending {
            card: "B-2".into(),
            why: "app roles".into(),
        },
        WorldError::Pending {
            card: "G-1a".into(),
            why: "later".into(),
        },
        WorldError::Missing("no roles".into()),
    ] {
        let a = answer.clone();
        let make: MakeGatewayWorld = Arc::new(move || {
            let a = a.clone();
            Box::pin(async move {
                Ok(Arc::new(Toy::with_role_answer(Mode::Right, a)) as Arc<dyn GatewayWorld>)
            })
        });
        let rows = gateway::run_gateway_cases(&role_cases, Some(make)).await;
        let (text, _) = gateway::render("role steps", &rows);
        for (name, o) in &rows {
            assert!(o.is_failed(), "{answer:?} switched {name} off: {o:?}{text}");
        }
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
    // A world may answer pending only in the listed cases, with a known card that is not G-1a.
    for (case, card) in WORLD_PENDING.iter().chain(WORLD_PENDING_STEPS.iter()) {
        assert!(
            CARDS.contains(card) && *card != "G-1a",
            "{case}: a world may not answer pending with {card}"
        );
    }
    for (case, _) in WORLD_PENDING {
        assert!(
            names.contains(case),
            "WORLD_PENDING names {case}, which is not a case"
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
        "expired 2 s ago",
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

/// The cases the toys run (cleaning, cookies, traversal, the sign-in), each with the part of
/// the failure message that names why the wrong toy must fail it. A case that fails for another
/// reason (a step the toy does not do, a changed scene) breaks the self-test.
const SELF_TEST: &[(&str, &str)] = &[
    (
        "request_headers_are_cleaned",
        "the app got a forged value in ruum-user",
    ),
    (
        "incoming_traceparent_is_replaced",
        "the incoming trace ID was kept",
    ),
    (
        "gateway_cookies_never_reach_the_app",
        "the app got the cookies [",
    ),
    (
        "app_set_cookie_for_ruum_names_is_dropped",
        "the browser got the app's cookie",
    ),
    (
        "gateway_cookies_follow_browser_rules",
        "the gateway's cookie ruum_visit has no __Host- prefix",
    ),
    (
        "traversal_never_leaves_the_deployment",
        "/sub/../index.html: expected the gateway's refusal [400, 404], got 200",
    ),
    (
        "the_sign_in_code_is_bound_to_its_host_and_used_once",
        "with the callback of a sign-in on app A: expected the gateway's refusal [400], got 302",
    ),
    (
        "hostile_framing_never_passes_the_check",
        "(absolute form, Host of app A): the request reached app B",
    ),
];

#[tokio::test]
async fn a_wrong_toy_gateway_fails_the_cases() {
    let rows = run_toy(Mode::Wrong).await;
    let (text, _) = gateway::render("wrong toy gateway", &rows);
    println!("{text}");
    for (name, o) in &rows {
        let why = SELF_TEST
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, w)| *w)
            .expect("a self-test case");
        match o {
            Outcome::Failed(m) => assert!(
                m.contains(why),
                "the wrong toy failed {name} for another reason than {why:?}: {m}"
            ),
            o => panic!("the wrong toy did not fail {name}: {o:?}"),
        }
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
        .filter(|c| SELF_TEST.iter().any(|(n, _)| *n == c.name))
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
    /// sign-in state -> (host, return_to, nonce of the state cookie)
    states: HashMap<String, (String, String, String)>,
    /// one-time code -> (user, state)
    codes: HashMap<String, (String, String)>,
}

struct Toy {
    mode: Mode,
    st: Arc<Mutex<ToyState>>,
    signer: Arc<TestSigner>,
    /// What granting or removing an app role answers (none: the toy does it).
    role_answer: Option<WorldError>,
}

impl Toy {
    fn new(mode: Mode) -> Toy {
        Toy {
            mode,
            st: Arc::new(Mutex::new(ToyState::default())),
            signer: Arc::new(TestSigner::new()),
            role_answer: None,
        }
    }

    fn with_role_answer(mode: Mode, answer: WorldError) -> Toy {
        Toy {
            role_answer: Some(answer),
            ..Toy::new(mode)
        }
    }

    fn role_step(&self, r: &Relation) -> WorldResult<()> {
        match (r, &self.role_answer) {
            (Relation::Role(_), Some(e)) => Err(e.clone()),
            _ => Ok(()),
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

    async fn seed_member(&self, _user: &User, _tenant: &Tenant) -> WorldResult<()> {
        Ok(())
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
        self.role_step(relation)?;
        self.st
            .lock()
            .unwrap()
            .grants
            .insert((user.id.clone(), app.id.clone(), rel(relation)));
        Ok(())
    }

    async fn revoke(&self, user: &User, app: &App, relation: &Relation) -> WorldResult<()> {
        self.role_step(relation)?;
        self.st
            .lock()
            .unwrap()
            .grants
            .remove(&(user.id.clone(), app.id.clone(), rel(relation)));
        Ok(())
    }

    /// The toy platform: issues a code for the state of the toy's sign-in start.
    async fn platform_authorize(&self, user: &User, login_location: &str) -> WorldResult<String> {
        let host = gateway::query_param(login_location, "app_host");
        let state = gateway::query_param(login_location, "app_state");
        let (Some(host), Some(state)) = (host, state) else {
            return Err(WorldError::Failed(
                "the toy platform got no app_host or app_state".into(),
            ));
        };
        let mut st = self.st.lock().unwrap();
        if !st.states.contains_key(&state) {
            return Err(WorldError::Failed(
                "the toy platform knows no such state".into(),
            ));
        }
        let code = ruum_core::identity::secret::random_secret("code_");
        st.codes
            .insert(code.clone(), (user.id.clone(), state.clone()));
        Ok(format!(
            "https://{host}/.ruum/v1/auth/callback?code={code}&state={state}"
        ))
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

    // One host only: two Host headers get 400 (both toys). The right toy also refuses an
    // absolute-form target whose authority is not the Host; the wrong toy checks the Host's
    // app and later forwards to the upstream of the app the authority names.
    if parts.headers.get_all("host").iter().count() > 1 {
        return reply(400, vec![], b"toy: two hosts".to_vec());
    }
    let authority = parts
        .uri
        .authority()
        .map(|a| a.as_str().to_ascii_lowercase());
    if mode == Mode::Right
        && authority
            .as_deref()
            .is_some_and(|a| a != host.to_ascii_lowercase())
    {
        return reply(400, vec![], b"toy: another authority".to_vec());
    }

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
    let host_lc = host.split(':').next().unwrap_or("").to_ascii_lowercase();
    if path == "/.ruum/v1/auth/start" {
        return toy_start(&st, &host_lc, &target, dep.env);
    }
    if path == "/.ruum/v1/auth/callback" {
        return toy_callback(
            mode,
            &st,
            &host_lc,
            &dep_id,
            &target,
            &cookie_pairs,
            dep.env,
        );
    }
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

    // The upstream (the wrong toy: of the app the absolute-form authority names).
    let upstream = match (&mode, &authority) {
        (Mode::Wrong, Some(a)) => {
            let st = st.lock().unwrap();
            resolve(&st, a).and_then(|(_, d)| st.deps.get(&d).and_then(|d| d.upstream))
        }
        _ => dep.upstream,
    };
    if let Some(up) = upstream {
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

/// `/.ruum/v1/auth/start`: a state bound to the host and `return_to`, its nonce in a cookie.
fn toy_start(st: &Mutex<ToyState>, host: &str, target: &str, env: Env) -> Response<Body> {
    let rt = gateway::query_param(target, "return_to").unwrap_or_else(|| "/".into());
    let ok = rt.starts_with('/')
        && !rt.starts_with("//")
        && !rt.contains('\\')
        && !rt.chars().any(|c| c.is_control());
    if !ok {
        return reply(400, security_headers(env), b"toy: bad return_to".to_vec());
    }
    let state = ruum_core::identity::secret::random_secret("st_");
    let nonce = ruum_core::identity::secret::random_secret("n_");
    st.lock()
        .unwrap()
        .states
        .insert(state.clone(), (host.to_string(), rt, nonce.clone()));
    let mut h = security_headers(env);
    h.push((
        "location".into(),
        format!("{ISS}/v1/auth/login?app_host={host}&app_state={state}"),
    ));
    h.push((
        "set-cookie".into(),
        format!("__Host-ruum_auth={nonce}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=600"),
    ));
    reply(302, h, vec![])
}

/// `/.ruum/v1/auth/callback`. The right toy checks the host of the state and the state cookie,
/// and uses a code once; the wrong toy checks neither.
fn toy_callback(
    mode: Mode,
    st: &Mutex<ToyState>,
    host: &str,
    dep_id: &str,
    target: &str,
    cookies: &[(String, String)],
    env: Env,
) -> Response<Body> {
    let refuse = || reply(400, security_headers(env), b"toy: bad callback".to_vec());
    let (Some(code), Some(state)) = (
        gateway::query_param(target, "code"),
        gateway::query_param(target, "state"),
    ) else {
        return refuse();
    };
    let mut st = st.lock().unwrap();
    let Some((s_host, rt, nonce)) = st.states.get(&state).cloned() else {
        return refuse();
    };
    let Some((user, c_state)) = st.codes.get(&code).cloned() else {
        return refuse();
    };
    if c_state != state {
        return refuse();
    }
    if mode == Mode::Right {
        let cookie = cookies
            .iter()
            .find(|(n, _)| n == "__Host-ruum_auth")
            .map(|(_, v)| v.as_str());
        if s_host != host || cookie != Some(nonce.as_str()) {
            return refuse();
        }
        st.codes.remove(&code);
    }
    let sid = ruum_core::identity::secret::random_secret("toy_");
    st.sessions.insert(sid.clone(), (user, dep_id.to_string()));
    let mut h = security_headers(env);
    h.push(("location".into(), rt));
    h.push((
        "set-cookie".into(),
        format!("__Host-ruum_app={sid}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=43200"),
    ));
    h.push((
        "set-cookie".into(),
        "__Host-ruum_auth=; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0".into(),
    ));
    reply(302, h, vec![])
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
