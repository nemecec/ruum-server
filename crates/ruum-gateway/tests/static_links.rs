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
use ruum_gateway::static_files::{HttpSiteFiles, LinkAnswer, SiteFiles};
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

/// A sandbox deployment serves no static files, even when an index exists under its ID.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sandbox_deployment_gets_no_static_links() {
    let w = World::new().await.unwrap();
    let (app, _, _) = public_site(&w, &[("index.html", b"static")]).await;
    let node = w
        .seed_node_deployment(&app, Env::Preview, "http://127.0.0.1:9")
        .await
        .unwrap();
    let list = vec![(
        "secret.txt".to_string(),
        bytes::Bytes::from_static(b"T6-SANDBOX-INDEX"),
    )];
    w.sites
        .write_site(
            World::tenant_id(&app.tenant).unwrap(),
            node.id.parse().unwrap(),
            &list,
            None,
        )
        .await
        .unwrap();
    assert_eq!(ask(&w, &query(&app, &node, "secret.txt")).await.0, 404);
}

/// Only the core's own "no such file" is a `404` for the gateway: a core of an earlier release
/// (no `static/file` route) is an error, so the visitor gets the `503` page, never a false `404`
/// (Kimi k14i6, 1). And the gateway follows no redirect of the object store.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_core_without_the_route_and_a_redirect_fail_closed() {
    let w = World::new().await.unwrap();
    let (app, d, _) = public_site(&w, &[("a.txt", b"aaa")]).await;
    // The real core's link of the file (a redirect below points at it, so following the
    // redirect would succeed and the test would see it).
    let core = HttpSiteFiles::new(&format!("http://{}", w.internal)).unwrap();
    let q = query(&app, &d, "a.txt");
    let LinkAnswer::File(mut link) = core.link(&q).await.unwrap() else {
        panic!("a file");
    };
    let target = link.url.clone();
    assert_eq!(
        w.http.get(&target).send().await.unwrap().status().as_u16(),
        200,
        "the redirect target itself is a good link"
    );
    // A listener with only the redirect stands for the core of the earlier release (no
    // static/file route) and for an object store that redirects.
    let old_l = listen().await;
    let old = old_l.local_addr().unwrap();
    let redirect = axum::Router::new().route(
        "/obj",
        axum::routing::get(move || {
            let target = target.clone();
            async move { (axum::http::StatusCode::FOUND, [("location", target)]) }
        }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(old_l, redirect.into_make_service()).await;
    });
    let files = HttpSiteFiles::new(&format!("http://{old}")).unwrap();
    assert!(
        files.link(&q).await.is_err(),
        "a bare 404 is not 'not found'"
    );
    // The real core: its own 404 is "not found".
    assert_eq!(
        core.link(&query(&app, &d, "nope.txt")).await.unwrap(),
        LinkAnswer::NotFound
    );
    link.url = format!("http://{old}/obj");
    assert!(
        core.open(&link).await.is_err(),
        "a redirect is never followed"
    );
    link.url = "file:///etc/passwd".into();
    assert!(core.open(&link).await.is_err(), "only http(s) links");
}

/// The object behind a link has exactly the size of its index entry: a chunked answer (no
/// `Content-Length`) that ends early or runs long ends the stream with an error, never with a
/// clean end; a declared length that differs is refused before any byte (Kimi k14i6-recheck, 3).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_static_object_of_another_size_fails() {
    use futures::StreamExt;
    fn chunked(parts: &'static [&'static [u8]]) -> axum::body::Body {
        axum::body::Body::from_stream(futures::stream::iter(
            parts
                .iter()
                .map(|p| Ok::<_, std::io::Error>(bytes::Bytes::from_static(p))),
        ))
    }
    let l = listen().await;
    let addr = l.local_addr().unwrap();
    let objects = axum::Router::new()
        .route(
            "/exact",
            axum::routing::get(|| async { chunked(&[b"01234", b"56789"]) }),
        )
        .route(
            "/short",
            axum::routing::get(|| async { chunked(&[b"0123"]) }),
        )
        .route(
            "/long",
            axum::routing::get(|| async { chunked(&[b"01234", b"56789", b"x"]) }),
        )
        .route("/declared", axum::routing::get(|| async { "0123" }));
    tokio::spawn(async move {
        let _ = axum::serve(l, objects.into_make_service()).await;
    });
    let files = HttpSiteFiles::new("http://127.0.0.1:1").unwrap();
    let link = |path: &str| StaticFileLink {
        hash: "0".repeat(64),
        size: 10,
        media_type: "text/plain".into(),
        url: format!("http://{addr}{path}"),
    };
    async fn read(
        files: &HttpSiteFiles,
        link: StaticFileLink,
    ) -> Vec<Result<bytes::Bytes, String>> {
        files
            .open(&link)
            .await
            .unwrap()
            .map(|c| c.map_err(|e| e.to_string()))
            .collect()
            .await
    }
    let exact = read(&files, link("/exact")).await;
    assert!(exact.iter().all(Result::is_ok), "{exact:?}");
    let body: Vec<u8> = exact
        .into_iter()
        .flat_map(|c| c.unwrap().to_vec())
        .collect();
    assert_eq!(body, b"0123456789");
    let short = read(&files, link("/short")).await;
    assert!(
        short
            .last()
            .is_some_and(|c| c.as_ref().is_err_and(|e| e.contains("shorter"))),
        "{short:?}"
    );
    let long = read(&files, link("/long")).await;
    assert!(
        long.last()
            .is_some_and(|c| c.as_ref().is_err_and(|e| e.contains("longer"))),
        "{long:?}"
    );
    assert!(
        files.open(&link("/declared")).await.is_err(),
        "a declared length other than the index entry's"
    );
}
