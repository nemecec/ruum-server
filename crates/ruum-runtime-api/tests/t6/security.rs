//! The security rules of card R-3 beyond the T-6 cases. Every refusal is shown next to the
//! same app getting `200` with the right credential in the same test (decision 0006 G), and a
//! refusal counts only on the runtime API's own answer (status and error code).

use super::support::*;
use ruum_core::NodeId;
use ruum_core::actor::Context;
use ruum_core::apps::store::DeploymentState;
use ruum_core::authz::Role;
use ruum_core::nodes::NodeKind;
use ruum_runtime_api::caller::{MemoryCallerNetwork, NetworkIdentity};
use ruum_runtime_api::limits::Caps;
use ruum_testkit::gateway::token::{TestSigner, decode_unverified};
use ruum_testkit::gateway::{Binding, BindingKind, Env, Req, Resp, RuntimeApiWorld};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

fn file(name: &str, obo: bool) -> Binding {
    Binding {
        name: name.into(),
        kind: BindingKind::File(format!("R3-{name}").into_bytes()),
        on_behalf_of_user: obo,
        delegate: false,
    }
}

fn folder(name: &str, files: &[&str], obo: bool) -> Binding {
    Binding {
        name: name.into(),
        kind: BindingKind::Folder(files.iter().map(|f| f.to_string()).collect()),
        on_behalf_of_user: obo,
        delegate: false,
    }
}

fn node(app: &ruum_testkit::gateway::RtApp, binding: &str) -> String {
    app.bound_nodes
        .iter()
        .find(|(b, _)| b == binding)
        .map(|(_, n)| n.clone())
        .expect("a bound node")
}

fn code(r: &Resp) -> String {
    r.json()
        .and_then(|v| v["error"]["code"].as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The runtime API's own refusal: the status and its error code.
fn refused_with(r: &Resp, status: u16, error: &str, what: &str) {
    assert_eq!(r.status, status, "{what}: {}", r.text());
    assert_eq!(code(r), error, "{what}");
}

async fn content(w: &World, n: &str, t: &str) -> Resp {
    w.get(&format!("/v1/nodes/{n}/content"), t).await.unwrap()
}

/// Polls until `f` holds, for at most 6 seconds (the 5 s rule and a margin for the test).
async fn within_5s<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(6) {
        if f().await {
            assert!(start.elapsed() <= Duration::from_secs(5) + Duration::from_millis(900));
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("{what} did not happen within 5 s");
}

/// A delegated token for `user` of `app` (live).
async fn delegated(
    w: &World,
    app: &ruum_testkit::gateway::RtApp,
    user: ruum_core::UserId,
) -> String {
    let seed = w.seed(app).unwrap();
    let wt = w.workload_token(app, Env::Live).await.unwrap();
    let ut = w
        .user_token_of(&seed, Env::Live, user, "user")
        .await
        .unwrap();
    let r = w.exchange(wt.expose(), ut.expose()).await.unwrap();
    assert_eq!(r.status, 200, "the exchange: {}", r.text());
    r.json().unwrap()["access_token"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Decision 0006-10: an API token at the runtime API, a user token as a workload token, a
/// delegated token at the exchange and at the workload-only operations, a workload token of a
/// foreign key, and the delegated token at the REST API: each `401`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn credentials_never_mix() {
    let w = World::new().await.unwrap();
    let app = w
        .seed_app("acme", "garden", &[file("doc", true)])
        .await
        .unwrap();
    let seed = w.seed(&app).unwrap();
    let doc = node(&app, "doc");
    let wt = w.workload_token(&app, Env::Live).await.unwrap();
    let v = w.visitor(&seed).await.unwrap();
    w.grant(v, NodeId(doc.parse().unwrap()), Role::View)
        .await
        .unwrap();
    // The right credentials work.
    assert_eq!(w.get("/v1/self", wt.expose()).await.unwrap().status, 200);
    assert_eq!(content(&w, &doc, wt.expose()).await.status, 200);
    let d = delegated(&w, &app, v).await;
    let r = content(&w, &doc, &d).await;
    assert_eq!((r.status, r.body.as_slice()), (200, b"R3-doc".as_slice()));
    // An API token of the REST API, and a user token, at the runtime API.
    let api = w.api_token(seed.tenant, seed.owner).await.unwrap();
    let ut = w.user_token_of(&seed, Env::Live, v, "user").await.unwrap();
    for (what, t) in [
        ("an API token", api.expose()),
        ("a user token", ut.expose()),
    ] {
        for path in ["/v1/self", &format!("/v1/nodes/{doc}/content")] {
            let r = w.get(path, t).await.unwrap();
            refused_with(&r, 401, "unauthenticated", &format!("{what} at {path}"));
        }
    }
    // The delegated token where only the workload token counts.
    refused_with(
        &w.exchange(&d, ut.expose()).await.unwrap(),
        401,
        "unauthenticated",
        "a delegated token at the exchange",
    );
    for path in ["/v1/self", "/v1/self/users?role=member"] {
        refused_with(
            &w.get(path, &d).await.unwrap(),
            401,
            "unauthenticated",
            path,
        );
    }
    let r = w
        .send(
            Req::post(INTERNAL_HOST, "/v1/events/t6.topic")
                .header("Authorization", &format!("Bearer {d}"))
                .json(&json!({"x": 1})),
        )
        .await
        .unwrap();
    refused_with(
        &r,
        401,
        "unauthenticated",
        "a delegated token at publishAppEvent",
    );
    // The right claims with a foreign key.
    let (_, claims) = decode_unverified(wt.expose()).unwrap();
    let forged = TestSigner::new().sign(&claims);
    refused_with(
        &w.get("/v1/self", &forged).await.unwrap(),
        401,
        "unauthenticated",
        "a workload token of a foreign key",
    );
    // App credentials at the REST API (the case covers the workload and the user token).
    assert_eq!(
        w.rest_api_status(&d).await.unwrap(),
        401,
        "a delegated token at REST"
    );
    assert_eq!(
        w.rest_api_status(api.expose()).await.unwrap(),
        200,
        "the API token at REST"
    );
}

/// With a caller network lookup, app A's valid token from app B's network is `401`, an unknown
/// peer is `401`, and `X-Forwarded-For`, `Forwarded`, `X-Real-IP` and `Ruum-*` headers change
/// nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_network_identity_decides_never_a_header() {
    let net = Arc::new(MemoryCallerNetwork::new());
    let w = World::with(Opts {
        network: Some(net.clone()),
        ..Opts::default()
    })
    .await
    .unwrap();
    let a = w.seed_app("acme", "garden", &[]).await.unwrap();
    let b = w.seed_app("acme", "orchard", &[]).await.unwrap();
    let (sa, sb) = (w.seed(&a).unwrap(), w.seed(&b).unwrap());
    let ta = w.workload_token(&a, Env::Live).await.unwrap();
    let tb = w.workload_token(&b, Env::Live).await.unwrap();
    let local: std::net::IpAddr = "127.0.0.1".parse().unwrap();
    let ident = |s: &AppSeed| NetworkIdentity {
        tenant: s.tenant,
        app: s.app,
        environment: None,
    };
    // Unknown peer.
    refused_with(
        &w.get("/v1/self", tb.expose()).await.unwrap(),
        401,
        "unauthenticated",
        "unknown peer",
    );
    net.set(local, ident(&sb));
    assert_eq!(w.get("/v1/self", tb.expose()).await.unwrap().status, 200);
    let spoof = |t: &str| {
        Req::get(INTERNAL_HOST, "/v1/self")
            .header("Authorization", &format!("Bearer {t}"))
            .header("X-Forwarded-For", "10.0.0.7")
            .header("Forwarded", "for=10.0.0.7")
            .header("X-Real-IP", "10.0.0.7")
            .header("Ruum-App", &sa.app.to_string())
            .header("Ruum-Tenant", &sa.tenant.to_string())
    };
    refused_with(
        &w.send(spoof(ta.expose())).await.unwrap(),
        401,
        "unauthenticated",
        "A from B's network",
    );
    net.set(local, ident(&sa));
    assert_eq!(w.send(spoof(ta.expose())).await.unwrap().status, 200);
    let r = w
        .send(
            Req::get(INTERNAL_HOST, "/v1/self")
                .header("Authorization", &format!("Bearer {}", tb.expose()))
                .header("Ruum-App", &sb.app.to_string())
                .header("X-Forwarded-For", "127.0.0.2"),
        )
        .await
        .unwrap();
    refused_with(
        &r,
        401,
        "unauthenticated",
        "B from A's network with B's headers",
    );
    // A network that tells the environment refuses the other one.
    net.set(
        local,
        NetworkIdentity {
            environment: Some(ruum_core::identity::app_sign_in::AppEnv::Live),
            ..ident(&sa)
        },
    );
    let tp = w.workload_token(&a, Env::Preview).await.unwrap();
    refused_with(
        &w.get("/v1/self", tp.expose()).await.unwrap(),
        401,
        "unauthenticated",
        "a preview on the live network",
    );
}

/// The intersection: a delegated token reads a bound file only while the user has `view` on
/// it, and only through the bindings it names; the app's own token reads while the binding
/// lives. Both halves separately.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_intersection_has_two_halves() {
    let w = World::new().await.unwrap();
    let app = w
        .seed_app("acme", "garden", &[file("doc", true), file("raw", false)])
        .await
        .unwrap();
    let seed = w.seed(&app).unwrap();
    let (doc, raw) = (node(&app, "doc"), node(&app, "raw"));
    let wt = w.workload_token(&app, Env::Live).await.unwrap();
    let v = w.visitor(&seed).await.unwrap();
    for n in [&doc, &raw] {
        w.grant(v, NodeId(n.parse().unwrap()), Role::View)
            .await
            .unwrap();
    }
    let d = delegated(&w, &app, v).await;
    let (_, c) = decode_unverified(&d).unwrap();
    assert_eq!(c["bindings"], json!(["doc"]));
    // The token's binding: read; the other binding: refused, though the user may view it.
    assert_eq!(content(&w, &doc, &d).await.status, 200);
    refused_with(
        &content(&w, &raw, &d).await,
        403,
        "permission_denied",
        "a binding the token does not name",
    );
    // The app's own token reads both.
    assert_eq!(content(&w, &raw, wt.expose()).await.status, 200);
    // The user loses view: the delegated read stops, the app's own read goes on.
    w.revoke(v, NodeId(doc.parse().unwrap()), Role::View)
        .await
        .unwrap();
    let r = content(&w, &doc, &d).await;
    assert!(
        matches!(r.status, 403 | 404),
        "after the user lost view: {}",
        r.status
    );
    assert!(matches!(
        code(&r).as_str(),
        "permission_denied" | "not_found"
    ));
    assert_eq!(content(&w, &doc, wt.expose()).await.status, 200);
    // A listing with a delegated token keeps only what the user sees.
    let pics = w
        .seed_app(
            "acme",
            "pics",
            &[folder("photos", &["a.jpg", "b.jpg"], true)],
        )
        .await
        .unwrap();
    let ps = w.seed(&pics).unwrap();
    let f = node(&pics, "photos");
    let pv = w.visitor(&ps).await.unwrap();
    let pw = w.workload_token(&pics, Env::Live).await.unwrap();
    let all = w
        .get(&format!("/v1/nodes/{f}/children"), pw.expose())
        .await
        .unwrap();
    assert_eq!(all.status, 200);
    let items = all.json().unwrap()["items"].as_array().cloned().unwrap();
    assert_eq!(items.len(), 2);
    let a = items.iter().find(|i| i["name"] == "a.jpg").unwrap()["node_id"]
        .as_str()
        .unwrap()
        .to_string();
    w.grant(pv, NodeId(f.parse().unwrap()), Role::View)
        .await
        .unwrap();
    let pd = delegated(&w, &pics, pv).await;
    let r = w
        .get(&format!("/v1/nodes/{f}/children"), &pd)
        .await
        .unwrap();
    assert_eq!(r.json().unwrap()["items"].as_array().unwrap().len(), 2);
    w.revoke(pv, NodeId(f.parse().unwrap()), Role::View)
        .await
        .unwrap();
    w.grant(pv, NodeId(a.parse().unwrap()), Role::View)
        .await
        .unwrap();
    let r = w
        .get(&format!("/v1/nodes/{f}/children"), &pd)
        .await
        .unwrap();
    // The user no longer sees the folder itself: refused.
    assert!(matches!(r.status, 403 | 404), "{}", r.status);
}

/// Bound content only: the parent of a bound folder (`..` by ID), a sibling folder, a node of
/// the tenant outside every binding, a trashed node, a node of a binding that a publish dropped
/// (within 5 s), and a node of another tenant (exactly like a missing node). A node below the
/// bound folder is read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bound_content_only_at_every_edge() {
    let w = World::new().await.unwrap();
    let app = w
        .seed_app("acme", "garden", &[folder("photos", &["a.jpg"], false)])
        .await
        .unwrap();
    let seed = w.seed(&app).unwrap();
    let owner = World::ctx(seed.tenant, seed.owner);
    let photos: NodeId = node(&app, "photos").parse().unwrap();
    let wt = w.workload_token(&app, Env::Live).await.unwrap();
    let t = wt.expose();
    // Below the bound folder: read.
    let sub = w
        .node(&owner, photos, "2026", NodeKind::Folder)
        .await
        .unwrap();
    let deep = w.file(&owner, sub, "deep.jpg", b"R3-deep").await.unwrap();
    let r = content(&w, &deep.to_string(), t).await;
    assert_eq!((r.status, r.body.as_slice()), (200, b"R3-deep".as_slice()));
    assert_eq!(r.header("content-length"), Some("7"));
    let r = w
        .get(&format!("/v1/nodes/{sub}/children"), t)
        .await
        .unwrap();
    assert_eq!(r.status, 200);
    // The parent (the tenant root), a sibling folder and a file there.
    let root = w.root(seed.tenant).await.unwrap();
    let sibling = w
        .node(&owner, root, "Sibling", NodeKind::Folder)
        .await
        .unwrap();
    let other = w.file(&owner, sibling, "x.txt", b"R3-x").await.unwrap();
    for (what, n) in [
        ("the parent", root),
        ("a sibling", sibling),
        ("a file there", other),
    ] {
        let r = w.get(&format!("/v1/nodes/{n}/children"), t).await.unwrap();
        refused_with(&r, 403, "permission_denied", what);
        refused_with(
            &content(&w, &n.to_string(), t).await,
            403,
            "permission_denied",
            what,
        );
    }
    // A trashed node is not found.
    let gone = w
        .file(&owner, photos, "gone.jpg", b"R3-gone")
        .await
        .unwrap();
    assert_eq!(content(&w, &gone.to_string(), t).await.status, 200);
    w.state
        .services
        .nodes
        .trash(&owner, gone, None)
        .await
        .unwrap();
    refused_with(
        &content(&w, &gone.to_string(), t).await,
        404,
        "not_found",
        "a trashed node",
    );
    // Another tenant's node answers exactly as a missing node.
    let g = w
        .seed_app("globex", "shed", &[file("doc", false)])
        .await
        .unwrap();
    let theirs = node(&g, "doc");
    let missing = Uuid::now_v7().to_string();
    let a = content(&w, &theirs, t).await;
    let b = content(&w, &missing, t).await;
    assert_eq!((a.status, a.body.clone()), (b.status, b.body.clone()));
    refused_with(&a, 404, "not_found", "another tenant's node");
    // A publish drops the binding: refused within 5 s, and the tuple is gone.
    let a_jpg = {
        let r = w
            .get(&format!("/v1/nodes/{photos}/children"), t)
            .await
            .unwrap();
        r.json().unwrap()["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["name"] == "a.jpg")
            .unwrap()["node_id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(content(&w, &a_jpg, t).await.status, 200);
    let dep = w
        .deploy(&owner, seed.app, &manifest(&[], ""), None)
        .await
        .unwrap();
    let dep = w.publish(&owner, seed.app, dep).await.unwrap();
    // The new live deployment's token (its instances get one) reads nothing of the dropped
    // binding; the old deployment's token no longer serves at all.
    let nt = w.token_for(&seed, dep, Env::Live);
    let (wr, aj, ntr) = (&w, &a_jpg, &nt);
    within_5s("the dropped binding refused", move || async move {
        content(wr, aj, ntr).await.status == 403
    })
    .await;
    within_5s("the old deployment's token refused", move || async move {
        content(wr, aj, t).await.status == 401
    })
    .await;
    let still = w
        .state
        .services
        .authz
        .check(
            &ruum_core::authz::SubjectRef::object(ruum_core::authz::ObjectRef::new(
                "app", seed.app.0,
            )),
            "view",
            &ruum_core::authz::ObjectRef::node(photos),
            None,
        )
        .await
        .unwrap();
    assert!(!still, "the tuple of the dropped binding stayed");
}

/// No stale principal: a superseded preview, an expired preview and a trashed app are refused
/// within 5 s.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_principals_are_refused() {
    let w = World::new().await.unwrap();
    let app = w
        .seed_app("acme", "garden", &[file("doc", false)])
        .await
        .unwrap();
    let seed = w.seed(&app).unwrap();
    let owner = World::ctx(seed.tenant, seed.owner);
    let m = manifest(&[], "");
    // A preview with a draft label, superseded by the next one of that label.
    let first = w.deploy(&owner, seed.app, &m, Some("x")).await.unwrap();
    let tok = |dep: Uuid| {
        let id = ruum_core::runtime::WorkloadIdentity {
            tenant: seed.tenant,
            app: seed.app,
            deployment: dep,
            environment: ruum_core::runtime::Environment::Preview,
        };
        w.issuer.issue(&id).unwrap().expose().to_string()
    };
    let t1 = tok(first);
    assert_eq!(w.get("/v1/self", &t1).await.unwrap().status, 200);
    let second = w.deploy(&owner, seed.app, &m, Some("x")).await.unwrap();
    let t2 = tok(second);
    assert_eq!(w.get("/v1/self", &t2).await.unwrap().status, 200);
    let (wr, t1r, t2r) = (&w, &t1, &t2);
    within_5s("the superseded preview refused", move || async move {
        let r = wr.get("/v1/self", t1r).await.unwrap();
        r.status == 401 && code(&r) == "unauthenticated"
    })
    .await;
    // An expired preview.
    w.state
        .services
        .apps
        .set_deployment_state(seed.tenant, second, DeploymentState::Expired)
        .await
        .unwrap();
    within_5s("the expired preview refused", move || async move {
        wr.get("/v1/self", t2r).await.unwrap().status == 401
    })
    .await;
    // A replaced live deployment (Kimi review 1): the old live token stops, the new one works.
    let old = w.token_for(&seed, seed.live, Env::Live);
    let doc = node(&app, "doc");
    assert_eq!(w.get("/v1/self", &old).await.unwrap().status, 200);
    assert_eq!(content(&w, &doc, &old).await.status, 200);
    let next = w
        .deploy(
            &owner,
            seed.app,
            &manifest(
                &[format!(
                    "  doc:\n    node: \"node:{doc}\"\n    role: view\n"
                )],
                "",
            ),
            None,
        )
        .await
        .unwrap();
    let next = w.publish(&owner, seed.app, next).await.unwrap();
    let new = w.token_for(&seed, next, Env::Live);
    let (oldr, newr) = (&old, &new);
    within_5s("the replaced live deployment refused", move || async move {
        let r = wr.get("/v1/self", oldr).await.unwrap();
        r.status == 401 && code(&r) == "unauthenticated"
    })
    .await;
    refused_with(
        &content(&w, &doc, &old).await,
        401,
        "unauthenticated",
        "the replaced live deployment reads",
    );
    let r = w.get("/v1/self", newr).await.unwrap();
    assert_eq!(r.status, 200, "the new live deployment: {}", r.text());
    assert_eq!(
        r.json().unwrap()["deployment_id"],
        json!(next.to_string()),
        "the new live deployment's self"
    );
    assert_eq!(content(&w, &doc, &new).await.status, 200);
    // A trashed app: refused within 5 s without waiting for the provider hook (no delivery
    // round runs in this test).
    w.state
        .services
        .nodes
        .trash(&owner, seed.app, None)
        .await
        .unwrap();
    within_5s("the trashed app refused", move || async move {
        wr.get("/v1/self", newr).await.unwrap().status == 401
    })
    .await;
    refused_with(
        &content(&w, &doc, &new).await,
        401,
        "unauthenticated",
        "a trashed app reads",
    );
}

/// Review of R-3, required change 1: the core's internal routes for the runtime API trust no
/// caller that a peer sends. Without the app's token, or with a token of another tenant than
/// the body names, each route is `401`; the same request with the right token is answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_core_routes_take_the_caller_only_from_the_token() {
    let w = World::new().await.unwrap();
    let acme = w
        .seed_app("acme", "garden", &[file("doc", false)])
        .await
        .unwrap();
    let globex = w
        .seed_app("globex", "garden", &[file("doc", false)])
        .await
        .unwrap();
    let (sa, sg) = (w.seed(&acme).unwrap(), w.seed(&globex).unwrap());
    let at = w.token_for(&sa, sa.live, Env::Live);
    let gt = w.token_for(&sg, sg.live, Env::Live);
    let caller = |s: &AppSeed| {
        json!({
            "tenant_id": s.tenant,
            "app_id": s.app,
            "principal": s.app.0,
            "deployment_id": s.live,
            "environment": "live",
        })
    };
    let acme_caller = caller(&sa);
    // principal: no token, globex's token, a garbage token; then acme's own token.
    assert_eq!(w.core_route("/principal", None, &acme_caller).await.0, 401);
    assert_eq!(
        w.core_route("/principal", Some(&gt), &acme_caller).await.0,
        401,
        "globex's token with acme's caller"
    );
    assert_eq!(
        w.core_route("/principal", Some("not.a.token"), &acme_caller)
            .await
            .0,
        401
    );
    let (status, body) = w.core_route("/principal", Some(&at), &acme_caller).await;
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["state"], "serving");
    assert_eq!(v["app"]["app_id"], json!(sa.app.to_string()));
    // content: acme's bound file asked with globex's token and acme's caller.
    let read = json!({
        "caller": acme_caller,
        "node_id": node(&acme, "doc"),
        "delegation": null,
    });
    assert_eq!(w.core_route("/content", None, &read).await.0, 401);
    assert_eq!(
        w.core_route("/content", Some(&gt), &read).await.0,
        401,
        "globex's token reads acme's file"
    );
    let (status, body) = w.core_route("/content", Some(&at), &read).await;
    assert_eq!((status, body.as_str()), (200, "R3-doc"));
    // A delegation in the body never comes from a workload token: the read is the app's own.
    let forged = json!({
        "caller": acme_caller,
        "node_id": node(&acme, "doc"),
        "delegation": { "bindings": ["doc"], "user": Uuid::now_v7(), "agent": null },
    });
    assert_eq!(w.core_route("/content", Some(&at), &forged).await.0, 200);
    // exchange, holders and topic: no token and a foreign token are refused.
    let ex = json!({ "caller": acme_caller, "sub": "x", "actor": "user", "on_behalf_of": null });
    let holders = json!({ "caller": acme_caller, "role": "member", "cursor": null, "limit": 10 });
    let topic = json!({ "caller": acme_caller, "topic": "orders" });
    for (route, body) in [
        ("/exchange", &ex),
        ("/holders", &holders),
        ("/topic", &topic),
    ] {
        assert_eq!(w.core_route(route, None, body).await.0, 401, "{route}");
        assert_eq!(w.core_route(route, Some(&gt), body).await.0, 401, "{route}");
        assert_eq!(w.core_route(route, Some(&at), body).await.0, 200, "{route}");
    }
}

/// `getAppSelf` names a binding's role as the manifest does (`view`), not as the graph does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn app_self_names_roles_as_the_manifest() {
    let w = World::new().await.unwrap();
    let app = w
        .seed_app("acme", "garden", &[file("doc", false)])
        .await
        .unwrap();
    let wt = w.workload_token(&app, Env::Live).await.unwrap();
    let r = w.get("/v1/self", wt.expose()).await.unwrap();
    assert_eq!(r.status, 200);
    let v = r.json().unwrap();
    assert_eq!(v["bindings"][0]["name"], "doc", "{v}");
    assert_eq!(v["bindings"][0]["role"], "view", "{v}");
}

/// Review of R-3, required change 3: a request body that trickles in is cut off with `408`
/// after the body deadline, and its permit is free again; a normal body in the same world is
/// answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_body_is_cut_off() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let w = World::with(Opts {
        caps: Some(Caps {
            per_app: 1,
            body_timeout: Duration::from_millis(700),
            ..Caps::default()
        }),
        ..Opts::default()
    })
    .await
    .unwrap();
    let app = w
        .seed_app("acme", "garden", &[file("doc", true)])
        .await
        .unwrap();
    let seed = w.seed(&app).unwrap();
    let wt = w.workload_token(&app, Env::Live).await.unwrap();
    // A normal body.
    let user = w.visitor(&seed).await.unwrap();
    let ut = w
        .user_token_of(&seed, Env::Live, user, "user")
        .await
        .unwrap();
    let r = w.exchange(wt.expose(), ut.expose()).await.unwrap();
    assert_eq!(r.status, 200, "a normal exchange: {}", r.text());
    // A body that never arrives in full.
    let mut s = tokio::net::TcpStream::connect(w.runtime).await.unwrap();
    let head = format!(
        "POST /v1/token/exchange HTTP/1.1\r\nHost: ruum.internal\r\nAuthorization: Bearer {}\r\n\
         Content-Type: application/json\r\nContent-Length: 100\r\n\r\n{{",
        wt.expose()
    );
    s.write_all(head.as_bytes()).await.unwrap();
    // While it trickles, it holds the app's one permit.
    tokio::time::sleep(Duration::from_millis(200)).await;
    refused_with(
        &w.get("/v1/self", wt.expose()).await.unwrap(),
        429,
        "rate_limited",
        "the permit held by the slow body",
    );
    let mut buf = vec![0u8; 512];
    let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf))
        .await
        .expect("the slow body was not cut off")
        .unwrap();
    let text = String::from_utf8_lossy(&buf[..n]);
    assert!(text.starts_with("HTTP/1.1 408"), "{text}");
    // The permit is free again.
    assert_eq!(w.get("/v1/self", wt.expose()).await.unwrap().status, 200);
}

/// One `GET /v1/self` from the source address `from`, on a fresh connection: the status, or
/// `None` when the server closed the connection without an answer.
async fn raw_self(to: std::net::SocketAddr, from: &str, token: &str) -> Option<u16> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let sock = tokio::net::TcpSocket::new_v4().unwrap();
    sock.bind(format!("{from}:0").parse().unwrap()).unwrap();
    let mut s = sock.connect(to).await.unwrap();
    let req = format!(
        "GET /v1/self HTTP/1.1\r\nHost: ruum.internal\r\nAuthorization: Bearer {token}\r\n\
         Connection: close\r\n\r\n"
    );
    let _ = s.write_all(req.as_bytes()).await;
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut buf)).await;
    let text = String::from_utf8_lossy(&buf);
    text.strip_prefix("HTTP/1.1 ")
        .and_then(|t| t.get(..3))
        .and_then(|c| c.parse().ok())
}

/// Kimi review 4: with a caller network lookup, one tenant's connections stop at its share,
/// and another tenant still gets in; when the first tenant's connections end, it gets in again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_tenant_cannot_hold_every_connection() {
    let net = Arc::new(MemoryCallerNetwork::new());
    let w = World::with(Opts {
        caps: Some(Caps {
            connections_per_tenant: 2,
            ..Caps::default()
        }),
        network: Some(net.clone()),
    })
    .await
    .unwrap();
    let a = w.seed_app("acme", "garden", &[]).await.unwrap();
    let b = w.seed_app("globex", "garden", &[]).await.unwrap();
    let (sa, sb) = (w.seed(&a).unwrap(), w.seed(&b).unwrap());
    let ident = |s: &AppSeed| NetworkIdentity {
        tenant: s.tenant,
        app: s.app,
        environment: None,
    };
    net.set("127.0.0.1".parse().unwrap(), ident(&sa));
    net.set("127.0.0.2".parse().unwrap(), ident(&sb));
    let ta = w.workload_token(&a, Env::Live).await.unwrap();
    let tb = w.workload_token(&b, Env::Live).await.unwrap();
    assert_eq!(
        raw_self(w.runtime, "127.0.0.1", ta.expose()).await,
        Some(200)
    );
    // Two idle connections of acme fill its share.
    let hold1 = tokio::net::TcpStream::connect(w.runtime).await.unwrap();
    let hold2 = tokio::net::TcpStream::connect(w.runtime).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        raw_self(w.runtime, "127.0.0.1", ta.expose()).await,
        None,
        "a third connection of acme"
    );
    // Globex is not touched by acme's share.
    assert_eq!(
        raw_self(w.runtime, "127.0.0.2", tb.expose()).await,
        Some(200)
    );
    drop((hold1, hold2));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        raw_self(w.runtime, "127.0.0.1", ta.expose()).await,
        Some(200)
    );
}

/// The exchange: once per user token; a token past its `exp`; an app without a binding that
/// acts for the user (`403`); an unknown person.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_exchange_rules() {
    let w = World::new().await.unwrap();
    let app = w
        .seed_app("acme", "garden", &[file("doc", true)])
        .await
        .unwrap();
    let seed = w.seed(&app).unwrap();
    let wt = w.workload_token(&app, Env::Live).await.unwrap();
    let v = w.visitor(&seed).await.unwrap();
    let ut = w.user_token_of(&seed, Env::Live, v, "user").await.unwrap();
    let r = w.exchange(wt.expose(), ut.expose()).await.unwrap();
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json().unwrap()["expires_in"], 300);
    refused_with(
        &w.exchange(wt.expose(), ut.expose()).await.unwrap(),
        401,
        "unauthenticated",
        "a replayed user token",
    );
    // Past its exp (the gateway's own claims, signed by its key).
    let sub = w.subject(&seed, v).await.unwrap();
    let mut c = w.users.claims(
        format!("app:{}", seed.app.0),
        "live",
        &seed.tenant_slug,
        &ruum_gateway::token::TokenSubject {
            sub: sub.clone(),
            actor: "user",
            on_behalf_of: None,
            roles: vec![],
        },
    );
    c.iat -= 120;
    c.exp -= 120;
    let old = w.users.sign(&c).unwrap();
    refused_with(
        &w.exchange(wt.expose(), &old).await.unwrap(),
        401,
        "unauthenticated",
        "an expired user token",
    );
    // Another tenant slug in the token.
    let mut c2 = w.users.claims(
        format!("app:{}", seed.app.0),
        "live",
        "globex",
        &ruum_gateway::token::TokenSubject {
            sub,
            actor: "user",
            on_behalf_of: None,
            roles: vec![],
        },
    );
    c2.jti = Uuid::now_v7().to_string();
    refused_with(
        &w.exchange(wt.expose(), &w.users.sign(&c2).unwrap())
            .await
            .unwrap(),
        401,
        "unauthenticated",
        "another tenant",
    );
    // A person who is not in the space.
    let stranger = w.person("stranger").await.unwrap();
    let st = w
        .user_token_of(&seed, Env::Live, stranger, "user")
        .await
        .unwrap();
    refused_with(
        &w.exchange(wt.expose(), st.expose()).await.unwrap(),
        401,
        "unauthenticated",
        "a stranger",
    );
    // An app without a binding that acts for the user.
    let plain = w
        .seed_app("acme", "plain", &[file("raw", false)])
        .await
        .unwrap();
    let ps = w.seed(&plain).unwrap();
    let pw = w.workload_token(&plain, Env::Live).await.unwrap();
    let pv = w.visitor(&ps).await.unwrap();
    let pu = w.user_token_of(&ps, Env::Live, pv, "user").await.unwrap();
    refused_with(
        &w.exchange(pw.expose(), pu.expose()).await.unwrap(),
        403,
        "permission_denied",
        "no on_behalf_of_user binding",
    );
    // A preview exchanges its own token, and its delegated token says preview.
    let prw = w.workload_token(&app, Env::Preview).await.unwrap();
    let pru = w
        .user_token_of(&seed, Env::Preview, v, "user")
        .await
        .unwrap();
    let r = w.exchange(prw.expose(), pru.expose()).await.unwrap();
    assert_eq!(r.status, 200, "{}", r.text());
    let t = r.json().unwrap()["access_token"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, c) = decode_unverified(&t).unwrap();
    assert_eq!(
        (c["env"].as_str(), c["sub"].as_str()),
        (Some("preview"), Some(seed.preview.to_string().as_str()))
    );
    // Another preview of the same app: only the audience differs (the same env, tenant and
    // pairwise subject), and the token is refused.
    let owner = World::ctx(seed.tenant, seed.owner);
    let other = w
        .deploy(&owner, seed.app, &manifest(&[], ""), None)
        .await
        .unwrap();
    let ow = w
        .issuer
        .issue(&ruum_core::runtime::WorkloadIdentity {
            tenant: seed.tenant,
            app: seed.app,
            deployment: other,
            environment: ruum_core::runtime::Environment::Preview,
        })
        .unwrap();
    let pru2 = w
        .user_token_of(&seed, Env::Preview, v, "user")
        .await
        .unwrap();
    refused_with(
        &w.exchange(ow.expose(), pru2.expose()).await.unwrap(),
        401,
        "unauthenticated",
        "a preview's user token at another preview",
    );
    // The live audience with env preview: only the env differs.
    let mut ce = w.users.claims(
        format!("app:{}", seed.app.0),
        "preview",
        &seed.tenant_slug,
        &ruum_gateway::token::TokenSubject {
            sub: w.subject(&seed, v).await.unwrap(),
            actor: "user",
            on_behalf_of: None,
            roles: vec![],
        },
    );
    ce.jti = Uuid::now_v7().to_string();
    refused_with(
        &w.exchange(wt.expose(), &w.users.sign(&ce).unwrap())
            .await
            .unwrap(),
        401,
        "unauthenticated",
        "the live audience with env preview",
    );
    let obo = c["on_behalf_of"].as_str().unwrap();
    assert!(Uuid::parse_str(obo).is_ok());
    assert_ne!(
        obo,
        v.0.to_string(),
        "the delegated token shows the platform user ID"
    );
}

/// `listAppSelfUsers`: pages within the contract's limit, a preview sees its own manifest.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn holders_are_pairwise_and_bounded() {
    let w = World::new().await.unwrap();
    let app = w.seed_app("acme", "garden", &[]).await.unwrap();
    let seed = w.seed(&app).unwrap();
    let wt = w.workload_token(&app, Env::Live).await.unwrap();
    for _ in 0..3 {
        w.visitor(&seed).await.unwrap();
    }
    let r = w
        .get("/v1/self/users?role=member&limit=2", wt.expose())
        .await
        .unwrap();
    assert_eq!(r.status, 200);
    let v = r.json().unwrap();
    assert!(v["items"].as_array().unwrap().len() <= 2);
    for bad in ["limit=0", "limit=201", "limit=x"] {
        let r = w
            .get(&format!("/v1/self/users?role=member&{bad}"), wt.expose())
            .await
            .unwrap();
        refused_with(&r, 400, "bad_request", bad);
    }
    refused_with(
        &w.get("/v1/self/users?role=Bad", wt.expose()).await.unwrap(),
        400,
        "bad_request",
        "a bad role",
    );
    // A preview whose manifest has a role that the live one has not: listed as empty.
    let owner = World::ctx(seed.tenant, seed.owner);
    let m = manifest(&[], "").replace("approver:", "reviewer:");
    let p = w.deploy(&owner, seed.app, &m, None).await.unwrap();
    let pt = w
        .issuer
        .issue(&ruum_core::runtime::WorkloadIdentity {
            tenant: seed.tenant,
            app: seed.app,
            deployment: p,
            environment: ruum_core::runtime::Environment::Preview,
        })
        .unwrap();
    let r = w
        .get("/v1/self/users?role=reviewer", pt.expose())
        .await
        .unwrap();
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json().unwrap()["items"], json!([]));
    refused_with(
        &w.get("/v1/self/users?role=approver", pt.expose())
            .await
            .unwrap(),
        404,
        "not_found",
        "a role only the live manifest has",
    );
}

/// `publishAppEvent`: a declared topic answers `501` (delivery is R-6), an undeclared one
/// `403`, a body past 256 KiB `413` while it is read, more than 200 properties `400`, a preview
/// `403`, a bad topic `400`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_wait_for_delivery() {
    let w = World::new().await.unwrap();
    let extra = "apis:\n  orders:\n    visibility: internal\n    protocol: events\n    spec: events.yaml\n    scopes: [\"read\"]\n";
    let app = w.seed_with("acme", "garden", &[], extra).await.unwrap();
    let wt = w.workload_token(&app, Env::Live).await.unwrap();
    let post = |topic: &str, t: &str, body: Vec<u8>| {
        Req::post(INTERNAL_HOST, &format!("/v1/events/{topic}"))
            .header("Authorization", &format!("Bearer {t}"))
            .header("Content-Type", "application/json")
            .body(body)
    };
    let r = w
        .send(post("orders.created", wt.expose(), b"{\"x\":1}".to_vec()))
        .await
        .unwrap();
    refused_with(&r, 501, "not_implemented", "a declared topic");
    assert_eq!(
        r.json().unwrap()["error"]["hint"],
        "event_delivery_not_ready"
    );
    let r = w
        .send(post("shipping.created", wt.expose(), b"{\"x\":1}".to_vec()))
        .await
        .unwrap();
    refused_with(&r, 403, "permission_denied", "an undeclared topic");
    let mut big = b"{\"x\":\"".to_vec();
    big.extend(std::iter::repeat_n(b'a', 256 * 1024));
    big.extend(b"\"}");
    let r = w
        .send(post("orders.created", wt.expose(), big))
        .await
        .unwrap();
    refused_with(&r, 413, "payload_too_large", "a large event");
    let many: Value = (0..201)
        .map(|i| (format!("k{i}"), json!(i)))
        .collect::<serde_json::Map<_, _>>()
        .into();
    let r = w
        .send(post(
            "orders.created",
            wt.expose(),
            many.to_string().into_bytes(),
        ))
        .await
        .unwrap();
    refused_with(&r, 400, "bad_request", "201 properties");
    let r = w
        .send(post("Orders", wt.expose(), b"{}".to_vec()))
        .await
        .unwrap();
    refused_with(&r, 400, "bad_request", "a bad topic");
    let pt = w.workload_token(&app, Env::Preview).await.unwrap();
    let r = w
        .send(post("orders.created", pt.expose(), b"{\"x\":1}".to_vec()))
        .await
        .unwrap();
    refused_with(&r, 403, "permission_denied", "a preview");
}

/// A cap answers `429` with `Retry-After`; a host other than `ruum.internal` is `404`; a client
/// that sends its request head too slowly is cut off.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_listener_has_limits() {
    let w = World::with(Opts {
        caps: Some(Caps {
            per_app: 0,
            header_timeout: Duration::from_secs(1),
            ..Caps::default()
        }),
        ..Opts::default()
    })
    .await
    .unwrap();
    let app = w.seed_app("acme", "garden", &[]).await.unwrap();
    let wt = w.workload_token(&app, Env::Live).await.unwrap();
    let r = w.get("/v1/self", wt.expose()).await.unwrap();
    refused_with(&r, 429, "rate_limited", "the app cap");
    assert_eq!(r.header("retry-after"), Some("1"));
    let r = w
        .send(
            Req::get("ruum.example", "/v1/self")
                .header("Authorization", &format!("Bearer {}", wt.expose())),
        )
        .await
        .unwrap();
    refused_with(&r, 404, "not_found", "another host");
    // A slow request head.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(w.runtime).await.unwrap();
    s.write_all(b"GET /v1/self HTTP/1.1\r\nHost: ruum.internal\r\n")
        .await
        .unwrap();
    let mut buf = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut buf)).await;
    assert!(
        read.is_ok(),
        "the connection stayed open past the header timeout"
    );
}

/// A world with the default caps answers the same app normally (the positive control of the
/// cap test above).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_default_caps_let_requests_through() {
    let w = World::new().await.unwrap();
    let app = w.seed_app("acme", "garden", &[]).await.unwrap();
    let wt = w.workload_token(&app, Env::Live).await.unwrap();
    assert_eq!(w.get("/v1/self", wt.expose()).await.unwrap().status, 200);
    let _: &Context = &World::ctx(w.seed(&app).unwrap().tenant, w.seed(&app).unwrap().owner);
}

/// Both the binding row and its tuple are needed: a tuple without a row (written by hand) reads
/// nothing, and a row whose tuple is gone reads nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_row_and_the_tuple_are_both_needed() {
    let w = World::new().await.unwrap();
    let app = w
        .seed_app("acme", "garden", &[file("doc", false)])
        .await
        .unwrap();
    let seed = w.seed(&app).unwrap();
    let owner = World::ctx(seed.tenant, seed.owner);
    let wt = w.workload_token(&app, Env::Live).await.unwrap();
    let doc = node(&app, "doc");
    assert_eq!(content(&w, &doc, wt.expose()).await.status, 200);
    let root = w.root(seed.tenant).await.unwrap();
    let stray = w
        .file(&owner, root, "stray.txt", b"R3-stray")
        .await
        .unwrap();
    w.app_tuple(seed.app.0, stray, true).await.unwrap();
    refused_with(
        &content(&w, &stray.to_string(), wt.expose()).await,
        403,
        "permission_denied",
        "a tuple without a row",
    );
    w.app_tuple(seed.app.0, NodeId(doc.parse().unwrap()), false)
        .await
        .unwrap();
    refused_with(
        &content(&w, &doc, wt.expose()).await,
        403,
        "permission_denied",
        "a row without its tuple",
    );
}

/// Headers never choose the tenant or the app: a globex app that names acme's tenant and app in
/// `Ruum-*` headers reads acme's bound file exactly as a missing node.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn headers_never_choose_the_tenant() {
    let w = World::new().await.unwrap();
    let acme = w
        .seed_app("acme", "garden", &[file("doc", false)])
        .await
        .unwrap();
    let globex = w
        .seed_app("globex", "garden", &[file("doc", false)])
        .await
        .unwrap();
    let (sa, sg) = (w.seed(&acme).unwrap(), w.seed(&globex).unwrap());
    let gt = w.workload_token(&globex, Env::Live).await.unwrap();
    let mine = node(&globex, "doc");
    let theirs = node(&acme, "doc");
    assert_eq!(content(&w, &mine, gt.expose()).await.status, 200);
    let with_headers = |n: &str| {
        Req::get(INTERNAL_HOST, &format!("/v1/nodes/{n}/content"))
            .header("Authorization", &format!("Bearer {}", gt.expose()))
            .header("Ruum-Tenant", &sa.tenant.to_string())
            .header("Ruum-App", &sa.app.to_string())
            .header("X-Forwarded-For", "10.0.0.9")
    };
    let a = w.send(with_headers(&theirs)).await.unwrap();
    let b = content(&w, &Uuid::now_v7().to_string(), gt.expose()).await;
    refused_with(&a, 404, "not_found", "acme's file with acme's headers");
    assert_eq!(a.body, b.body);
    // The headers do not stop globex's own reads either.
    assert_eq!(w.send(with_headers(&mine)).await.unwrap().status, 200);
    let _ = sg;
}
