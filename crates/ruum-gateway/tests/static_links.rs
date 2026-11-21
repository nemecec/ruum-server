//! Static files without a key in the gateway (decision 0008-4; Kimi run 14, I-6 finding 1).
//!
//! The gateway holds no object store credential. It asks the core's internal listener for the
//! file of a path, and the core answers with the index entry and a link that reads that one
//! object for a minute. The core signs links only for a deployment that serves now and is
//! static, so a gateway that is not what it should be cannot read the files of an older release,
//! of an app that never went live, or of another tenant. These tests run the gateway and the
//! core in process ([`support::World`]); the world's object server stands for the object store
//! and accepts only links that the core's signer made.

mod support;

use support::*;

use ruum_core::identity::app_sign_in::wire::{PREFIX, StaticFileLink, StaticFileQuery};
use ruum_testkit::gateway::{self, Access, Env, GatewayWorld, Req};
use std::sync::atomic::Ordering;
use std::time::Duration;
use uuid::Uuid;

/// A public, approved app with one live static deployment: (app, deployment, host).
async fn public_site(
    w: &World,
    files: &[(&str, &[u8])],
) -> (gateway::App, gateway::Deployment, String) {
    let n = w.next();
    let t = w.seed_tenant(&format!("links{n}")).await.unwrap();
    w.set_public_approval(&t, true).await.unwrap();
    let a = w.seed_app(&t, "site", Access::Public).await.unwrap();
    let d = w
        .seed_deployment(&a, Env::Live, &gateway::files(files), None)
        .await
        .unwrap();
    w.set_live(&a, &d).await.unwrap();
    let host = format!("site.{}.{APPS_DOMAIN}", t.slug);
    (a, d, host)
}

async fn send(w: &World, req: Req) -> gateway::Resp {
    gateway::http::send(w.gateway, &req, Duration::from_secs(20))
        .await
        .unwrap()
}

/// What the core's internal listener answers the gateway for `q`.
async fn ask(w: &World, q: &StaticFileQuery) -> (u16, Option<StaticFileLink>) {
    let r = w
        .http
        .post(format!("http://{}{PREFIX}/static/file", w.internal))
        .json(q)
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    let link = if status == 200 {
        Some(r.json().await.unwrap())
    } else {
        None
    };
    (status, link)
}

fn query(app: &gateway::App, d: &gateway::Deployment, path: &str) -> StaticFileQuery {
    StaticFileQuery {
        tenant_id: World::tenant_id(&app.tenant).unwrap(),
        app_id: World::node_id(&app.id).unwrap(),
        deployment_id: d.id.parse().unwrap(),
        path: path.into(),
        navigation: false,
    }
}

/// The bytes come from the object store through the core's link: one object read per `GET`,
/// none for a `HEAD` or a matching `If-None-Match`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_gateway_reads_files_only_through_the_cores_links() {
    let w = World::new().await.unwrap();
    let (_, _, host) = public_site(&w, &[("index.html", b"<h1>linked</h1>")]).await;
    let before = w.objects.gets.load(Ordering::SeqCst);
    let r = send(&w, Req::get(&host, "/")).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.body, b"<h1>linked</h1>");
    assert_eq!(w.objects.gets.load(Ordering::SeqCst), before + 1);
    let etag = r.header("etag").unwrap().to_string();
    assert_eq!(
        etag,
        format!("\"{}\"", blake3::hash(b"<h1>linked</h1>").to_hex())
    );
    let head = send(&w, Req::new("HEAD", &host, "/index.html")).await;
    assert_eq!(head.status, 200);
    assert_eq!(head.header("content-length"), Some("15"));
    let cached = send(&w, Req::get(&host, "/").header("If-None-Match", &etag)).await;
    assert_eq!(cached.status, 304);
    assert_eq!(
        w.objects.gets.load(Ordering::SeqCst),
        before + 1,
        "a HEAD and a 304 read no object"
    );
    let missing = send(&w, Req::get(&host, "/nope.js")).await;
    assert_eq!(missing.status, 404);
    assert_eq!(w.objects.gets.load(Ordering::SeqCst), before + 1);
}

/// The core signs a link only for the deployment that serves now and only for a file of its
/// index: an older release (kept for a rollback), a deployment that never went live, another
/// app's ID and another tenant get `404`, although their indexes exist.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_core_signs_links_only_for_serving_static_deployments() {
    let w = World::new().await.unwrap();
    let (app, old, host) = public_site(&w, &[("old.txt", b"T6-OLD-RELEASE")]).await;
    // The live release: links work and read the object.
    let (status, link) = ask(&w, &query(&app, &old, "old.txt")).await;
    assert_eq!(status, 200);
    let link = link.unwrap();
    assert_eq!(link.size, 14);
    assert_eq!(link.media_type, "text/plain; charset=utf-8");
    let body = w.http.get(&link.url).send().await.unwrap();
    assert_eq!(body.status().as_u16(), 200);
    assert_eq!(&body.bytes().await.unwrap()[..], b"T6-OLD-RELEASE");
    // The link reads that one object and nothing else: another key with its signature fails.
    let other_key = link.url.replace(&link.hash, &"0".repeat(64));
    assert_eq!(
        w.http
            .get(&other_key)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        403
    );
    // A new release goes live; the old one keeps its index (a rollback deploys it again).
    let new = w
        .seed_deployment(
            &app,
            Env::Live,
            &gateway::files(&[("new.txt", b"new")]),
            None,
        )
        .await
        .unwrap();
    w.set_live(&app, &new).await.unwrap();
    assert!(
        w.sites
            .read_index(
                World::tenant_id(&app.tenant).unwrap(),
                old.id.parse().unwrap()
            )
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(ask(&w, &query(&app, &old, "old.txt")).await.0, 404);
    assert_eq!(ask(&w, &query(&app, &new, "new.txt")).await.0, 200);
    let r = send(&w, Req::get(&host, "/old.txt")).await;
    assert_eq!(r.status, 404);
    assert!(!r.text().contains("T6-OLD-RELEASE"));
    // A deployment that never went live (its index exists, no row serves it).
    let never = w
        .seed_deployment(
            &app,
            Env::Live,
            &gateway::files(&[("never.txt", b"never")]),
            None,
        )
        .await
        .unwrap();
    assert_eq!(ask(&w, &query(&app, &never, "never.txt")).await.0, 404);
    // The right deployment under another app ID, or another tenant.
    let mut q = query(&app, &new, "new.txt");
    q.app_id = ruum_core::NodeId(Uuid::now_v7());
    assert_eq!(ask(&w, &q).await.0, 404);
    let mut q = query(&app, &new, "new.txt");
    q.tenant_id = ruum_core::TenantId(Uuid::now_v7());
    assert_eq!(ask(&w, &q).await.0, 404);
    // A path the gateway would refuse never reaches the index.
    for bad in ["../old.txt", "a//b", "/new.txt", "%2e%2e/new.txt"] {
        assert_eq!(ask(&w, &query(&app, &new, bad)).await.0, 404, "{bad}");
    }
    // An app in the trash serves nothing, at once.
    let tenant = World::tenant_id(&app.tenant).unwrap();
    let ctx = w.owner_ctx(tenant).unwrap();
    w.state
        .services
        .nodes
        .trash(&ctx, World::node_id(&app.id).unwrap(), None)
        .await
        .unwrap();
    assert_eq!(ask(&w, &query(&app, &new, "new.txt")).await.0, 404);
}

/// A link stops working: an expired one is refused by the object store, and the gateway, which
/// never keeps links, asks again for each request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn links_are_short_lived() {
    let w = World::new().await.unwrap();
    let (app, d, host) = public_site(&w, &[("a.txt", b"aaa")]).await;
    let (_, link) = ask(&w, &query(&app, &d, "a.txt")).await;
    let link = link.unwrap();
    let ttl = link
        .url
        .split("expires=")
        .nth(1)
        .and_then(|s| s.split('&').next())
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap()
        - ruum_core::now().unix_timestamp();
    assert!(
        (1..=ruum_core::apps::static_site::LINK_TTL.as_secs() as i64).contains(&ttl),
        "{ttl}"
    );
    let path = url::Url::parse(&link.url).unwrap().path()[1..].to_string();
    let expired = w.objects.expired_link(&path, 1);
    assert_eq!(
        w.http.get(&expired).send().await.unwrap().status().as_u16(),
        403
    );
    for _ in 0..2 {
        assert_eq!(send(&w, Req::get(&host, "/a.txt")).await.status, 200);
    }
}

/// Without a static store that signs links (the server without S3), every static path gets the
/// gateway's `503` page, never a file and never an error from elsewhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_links_static_paths_are_unavailable() {
    let w = World::with(Opts {
        no_static_links: true,
        ..Opts::default()
    })
    .await
    .unwrap();
    let (app, d, host) = public_site(&w, &[("index.html", b"<h1>hidden</h1>")]).await;
    assert_eq!(ask(&w, &query(&app, &d, "index.html")).await.0, 503);
    let r = send(&w, Req::get(&host, "/")).await;
    assert_eq!(r.status, 503);
    assert!(!r.text().contains("hidden"));
    assert_eq!(w.objects.gets.load(Ordering::SeqCst), 0);
}
