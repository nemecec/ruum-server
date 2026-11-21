//! The T-6 world of the gateway (card G-1a; `crates/ruum-testkit/src/gateway/`, plan seam 4): it
//! starts `ruum-gateway` in process against the in-memory core and runs every T-6 case.
//!
//! - The core: `ruum-api` with in-memory services. Its public router (the platform side of the
//!   sign-in, `GET /v1/me`) and its internal router (the gateway's internal API) listen on
//!   127.0.0.1, so the gateway and the world talk to them over HTTP, as in production.
//! - The gateway: [`ruum_gateway::Gateway`] on its own 127.0.0.1 listener, with the in-memory
//!   host lookup (the world records each host as the SQL function would answer it; the function
//!   itself has its own Postgres tests in `tests/host_lookup_pg.rs`), the in-memory static sites
//!   and the core's internal API.
//! - Seeding goes through the core's stores: team tenants with exact slugs, people with a platform
//!   account (guests, without any right on an app), apps through `createApp`, deployments through
//!   the store, grants as tuples of the graph, app roles through B-2's `addAppRoleGrant` and
//!   `removeAppRoleGrant` (every seeded deployment declares the roles `member` and `approver`),
//!   agents through the agent store.
//! - The world tells the gateway when a host changes (a new live deployment, a reused host), as
//!   the journal feed does in production.
//! - Card G-1b: sandbox deployments (`seed_node_deployment`) run in the memory driver behind the
//!   core's `SandboxDirectory`; each running instance answers on its own port as the case's echo
//!   app ([`support`]).

mod support;

use support::*;

use ruum_testkit::gateway::{self, Access, AgentAudience, Env, GatewayWorld, Req};
use std::collections::HashMap;
use std::time::Duration;
use uuid::Uuid;

/// Every T-6 case with the G-1a world: none fails, none reports planned (the world is there),
/// and a case is pending only when `PENDING` lists it for a later card (the world itself
/// answers pending for no step: B-2's app roles are merged, P-68).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t6_gateway_cases_pass_with_the_g1a_world() {
    let cases = gateway::gateway_cases();
    let rows = gateway::run_gateway_cases(&cases, Some(make())).await;
    let (text, ok) = gateway::render("gateway (G-1a world, in-memory core)", &rows);
    println!("{text}");
    assert!(ok, "{text}");
    for (name, o) in &rows {
        assert!(
            !matches!(o, gateway::Outcome::Planned(_)),
            "{name} is planned with the G-1a world: {o:?}"
        );
        assert!(
            !matches!(o, gateway::Outcome::Pending(_)) || gateway::pending_reason(name).is_some(),
            "{name} is pending with the G-1a world, but PENDING does not list it: {o:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The state of the code flow (card G-1a "Acceptance tests": expired, replayed, another host, a
// return_to off the origin) and the platform side of the app audience.
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_code_works_once_and_returns_to_its_path() {
    let w = World::new().await.unwrap();
    let (_, _, u, host) = app_scene(&w).await;
    let (target, nonce) = code_for(&w, &u, &host).await;
    let cookie = format!("__Host-ruum_auth={nonce}");
    let first = w.gateway_get(&host, &target, Some(&cookie)).await.unwrap();
    assert_eq!(first.status, 302);
    assert_eq!(first.header("location"), Some("/docs?a=1"));
    assert!(has_session(&first));
    let again = w.gateway_get(&host, &target, Some(&cookie)).await.unwrap();
    assert_eq!(again.status, 400, "a replayed code");
    assert!(!has_session(&again));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_state_needs_its_host_and_the_browsers_nonce() {
    let w = World::new().await.unwrap();
    let (t, _, u, host) = app_scene(&w).await;
    let b = w.seed_app(&t, "orchard", Access::Platform).await.unwrap();
    let bd = w
        .seed_deployment(
            &b,
            Env::Live,
            &gateway::files(&[("index.html", b"b")]),
            None,
        )
        .await
        .unwrap();
    w.set_live(&b, &bd).await.unwrap();
    let other = format!("orchard.acme.{APPS_DOMAIN}");
    // Another host.
    let (target, nonce) = code_for(&w, &u, &host).await;
    let r = w
        .gateway_get(&other, &target, Some(&format!("__Host-ruum_auth={nonce}")))
        .await
        .unwrap();
    assert_eq!(r.status, 400, "a state of another host");
    assert!(!has_session(&r));
    // Without the browser's nonce, or with another one.
    let (target, _) = code_for(&w, &u, &host).await;
    for cookie in [
        None,
        Some("__Host-ruum_auth=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
    ] {
        let r = w.gateway_get(&host, &target, cookie).await.unwrap();
        assert_eq!(r.status, 400, "{cookie:?}");
        assert!(!has_session(&r));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_expired_state_is_refused() {
    let w = World::new().await.unwrap();
    let (_, _, u, host) = app_scene(&w).await;
    let (target, nonce) = code_for(&w, &u, &host).await;
    let url = url::Url::parse(&format!("https://{host}{target}")).unwrap();
    let code = url
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.to_string())
        .unwrap();
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.to_string())
        .unwrap();
    // The same claims, sealed with a time in the past.
    let claims = {
        use base64::Engine as _;
        let p = state.split('.').nth(1).unwrap();
        let v: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(p)
                .unwrap(),
        )
        .unwrap();
        v["v"].clone()
    };
    let expired = w.gw.tokens.seal(&claims, -5).unwrap();
    let target = format!("/.ruum/v1/auth/callback?code={code}&state={expired}");
    let r = w
        .gateway_get(&host, &target, Some(&format!("__Host-ruum_auth={nonce}")))
        .await
        .unwrap();
    assert_eq!(r.status, 400);
    assert!(!has_session(&r));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_platform_side_checks_the_app_and_the_session() {
    let w = World::new().await.unwrap();
    let (t, a, u, host) = app_scene(&w).await;
    let secret = w
        .platform_session(World::user_id(&u).unwrap())
        .await
        .unwrap();
    let start = w
        .gateway_get(&host, "/.ruum/v1/auth/start", None)
        .await
        .unwrap();
    let login = url::Url::parse(start.header("location").unwrap()).unwrap();
    let params: HashMap<String, String> = login.query_pairs().into_owned().collect();
    let send = |q: Vec<(&str, String)>, session: bool| {
        let mut url = login.clone();
        url.query_pairs_mut().clear().extend_pairs(q);
        let mut req = w.http.get(url);
        if session {
            req = req.header("cookie", format!("ruum_session={secret}"));
        }
        req.send()
    };
    let base = |k: &str| (k.to_string(), params[k].clone());
    // Each wrong value gets the same 400.
    let unknown = Uuid::now_v7().to_string();
    for (what, q) in [
        (
            "another tenant",
            vec![("app_tenant", unknown.clone()), ("app_id", a.id.clone())],
        ),
        (
            "another app",
            vec![("app_tenant", t.id.clone()), ("app_id", unknown.clone())],
        ),
    ] {
        let mut all: Vec<(&str, String)> = q;
        for k in ["app_deployment", "app_state"] {
            all.push((k, base(k).1));
        }
        let r = send(all, true).await.unwrap();
        assert_eq!(r.status().as_u16(), 400, "{what}");
        assert!(r.headers().get("location").is_none(), "{what}");
    }
    let mut bad_state: Vec<(&str, String)> = ["app_tenant", "app_id", "app_deployment"]
        .iter()
        .map(|k| (*k, params[*k].clone()))
        .collect();
    bad_state.push(("app_state", "not a state".into()));
    assert_eq!(send(bad_state, true).await.unwrap().status().as_u16(), 400);
    // Without a platform session: the normal sign-in first, never the app's callback.
    let all: Vec<(&str, String)> = ["app_tenant", "app_id", "app_deployment", "app_state"]
        .iter()
        .map(|k| (*k, params[*k].clone()))
        .collect();
    let r = send(all, false).await.unwrap();
    assert_eq!(r.status().as_u16(), 302);
    let loc = r.headers()["location"].to_str().unwrap().to_string();
    assert!(!loc.contains(".ruum/v1/auth/callback"), "{loc}");
    assert!(!loc.contains("code="), "{loc}");
}

/// The request path only selects an entry of the deployment's own site index: it never names a
/// deployment, an index or a file object (plan seam 1; card G-1a "Static files").
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_path_never_chooses_the_object() {
    let w = World::new().await.unwrap();
    let (_, a, u, host) = app_scene(&w).await;
    let other = w
        .seed_deployment(
            &a,
            Env::Preview,
            &gateway::files(&[("secret.txt", b"T6-SECRET-OF-ANOTHER-DEPLOYMENT")]),
            None,
        )
        .await
        .unwrap();
    let (target, nonce) = code_for(&w, &u, &host).await;
    let cb = w
        .gateway_get(&host, &target, Some(&format!("__Host-ruum_auth={nonce}")))
        .await
        .unwrap();
    let session = format!(
        "__Host-ruum_app={}",
        cookie_value(&cb, "__Host-ruum_app").unwrap()
    );
    let ok = w
        .gateway_get(&host, "/index.html", Some(&session))
        .await
        .unwrap();
    assert_eq!(ok.status, 200);
    let hash = blake3::hash(b"T6-SECRET-OF-ANOTHER-DEPLOYMENT")
        .to_hex()
        .to_string();
    for target in [
        format!("/{}/secret.txt", other.id),
        format!("/sites/{}.json", other.id),
        format!("/files/{hash}"),
        format!("/{hash}"),
        "/secret.txt".to_string(),
    ] {
        let r = w.gateway_get(&host, &target, Some(&session)).await.unwrap();
        assert_eq!(r.status, 404, "{target}");
        assert!(!r.text().contains("T6-SECRET"), "{target}");
    }
}

/// The gateway holds no bucket key (decision 0008-4; backlog I6-3D): a static file comes
/// through a link that the core signed for exactly that file object; `HEAD` and a matching
/// `If-None-Match` fetch nothing, and a refused path asks for no link.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn static_files_come_through_the_cores_links() {
    let w = World::new().await.unwrap();
    let (_, a, u, host) = app_scene(&w).await;
    let (target, nonce) = code_for(&w, &u, &host).await;
    let cb = w
        .gateway_get(&host, &target, Some(&format!("__Host-ruum_auth={nonce}")))
        .await
        .unwrap();
    let session = format!(
        "__Host-ruum_app={}",
        cookie_value(&cb, "__Host-ruum_app").unwrap()
    );
    let ok = w.gateway_get(&host, "/", Some(&session)).await.unwrap();
    assert_eq!(ok.status, 200);
    assert_eq!(ok.text(), "hi");
    let hash = blake3::hash(b"hi").to_hex().to_string();
    assert_eq!(
        *w.link_hits.lock().unwrap(),
        vec![format!("apps/static/{}/files/{hash}", a.tenant.id)],
        "the file did not come through one link of its own object"
    );
    let etag = ok.header("etag").unwrap().to_string();
    assert_eq!(etag, format!("\"{hash}\""));
    let addr = w.gateway;
    let head = gateway::http::send(
        addr,
        &Req::new("HEAD", &host, "/").header("Cookie", &session),
        Duration::from_secs(20),
    )
    .await
    .unwrap();
    assert_eq!(head.status, 200);
    let cached = gateway::http::send(
        addr,
        &Req::get(&host, "/")
            .header("Cookie", &session)
            .header("If-None-Match", &etag),
        Duration::from_secs(20),
    )
    .await
    .unwrap();
    assert_eq!(cached.status, 304);
    let bad = w
        .gateway_get(&host, "/%2e%2e/index.html", Some(&session))
        .await
        .unwrap();
    assert_eq!(bad.status, 400);
    assert_eq!(
        w.link_hits.lock().unwrap().len(),
        1,
        "HEAD, a matching If-None-Match or a refused path fetched a file"
    );
}

/// Two `Authorization` headers get `400` and reach nothing: an app's own `Basic` value in front
/// cannot carry a platform token past the check to the app (review-g1a 2; decision 0006-10,
/// `gateway.openapi.yaml` "The check of every request" 3). One header of the app's own scheme
/// still passes, and one platform token alone gets `401`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_authorization_header_never_passes() {
    let w = World::new().await.unwrap();
    let (_, a, u, host) = app_scene(&w).await;
    let live = w
        .seed_deployment(
            &a,
            Env::Live,
            &gateway::files(&[("index.html", b"hi")]),
            None,
        )
        .await
        .unwrap();
    w.set_live(&a, &live).await.unwrap();
    let echo = gateway::Echo::start().await;
    w.set_upstream(&live, &echo.url).await.unwrap();
    let (target, nonce) = code_for(&w, &u, &host).await;
    let cb = w
        .gateway_get(&host, &target, Some(&format!("__Host-ruum_auth={nonce}")))
        .await
        .unwrap();
    let session = format!(
        "__Host-ruum_app={}",
        cookie_value(&cb, "__Host-ruum_app").unwrap()
    );
    let api = w.agent_token(&u, AgentAudience::Api).await.unwrap();
    let platform = format!("Bearer {}", api.expose());
    let addr = w.gateway;
    let send = |values: Vec<String>| {
        let mut req = Req::get(&host, "/").header("Cookie", &session);
        for v in &values {
            req = req.header("Authorization", v);
        }
        async move { gateway::http::send(addr, &req, Duration::from_secs(20)).await }
    };
    let basic = "Basic YXBwOnQ2b3du".to_string();
    // One value of the app's own scheme: forwarded with the session.
    let r = send(vec![basic.clone()]).await.unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(echo.hits(), 1);
    // One platform token alone: 401.
    let r = send(vec![platform.clone()]).await.unwrap();
    assert_eq!(r.status, 401);
    // Two values, in either order: 400, and the app sees nothing.
    for (what, values) in [
        ("basic first", vec![basic.clone(), platform.clone()]),
        ("token first", vec![platform.clone(), basic.clone()]),
        ("two of the app's own", vec![basic.clone(), basic.clone()]),
    ] {
        let r = send(values).await.unwrap();
        assert_eq!(r.status, 400, "{what}");
        assert!(!r.text().contains(api.expose()), "{what}");
    }
    assert_eq!(
        echo.hits(),
        1,
        "a request with two Authorization headers reached the app"
    );
    assert!(
        echo.seen()
            .iter()
            .all(|s| s.headers.iter().all(|(_, v)| !v.contains(api.expose()))),
        "the platform token reached the app"
    );
}
