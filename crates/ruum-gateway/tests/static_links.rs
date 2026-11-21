//! Static files without a bucket key in the gateway (decision 0008-4, backlog I6-3D).
//!
//! - The core's `static/file` and `static/content` answer only for a serving deployment of the
//!   tenant that the query names, and `static/content` gives only a file that the deployment's
//!   index names: a gateway that asks for anything else gets nothing.
//! - The gateway fetches a file through the core's read link and checks it against the index
//!   entry: a longer object, a missing object and a link that is not a URL never become a
//!   wrong answer.

mod support;

use support::*;

use axum::Router;
use axum::routing::get;
use futures::StreamExt;
use ruum_core::apps::static_site::IndexEntry;
use ruum_core::apps::static_site::wire::{StaticFileAnswer, StaticFileQuery};
use ruum_core::identity::app_sign_in::wire::PREFIX;
use ruum_core::{NodeId, TenantId};
use ruum_gateway::static_files::{CoreStatic, StaticSource};
use ruum_testkit::gateway::{Access, Env, GatewayWorld};
use serde_json::{Value, json};
use uuid::Uuid;

async fn core_post(w: &World, route: &str, body: &Value) -> reqwest::Response {
    w.http
        .post(format!("http://{}{PREFIX}{route}", w.internal))
        .json(body)
        .send()
        .await
        .expect("the core's internal listener")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_core_gives_files_only_of_a_serving_deployment_of_the_named_tenant() {
    let w = World::new().await.unwrap();
    let acme = w.seed_tenant("acme").await.unwrap();
    let globex = w.seed_tenant("globex").await.unwrap();
    let app = w.seed_app(&acme, "garden", Access::Platform).await.unwrap();
    let files = vec![
        ("index.html".to_string(), b"<h1>acme</h1>".to_vec()),
        ("secret.txt".to_string(), b"acme only".to_vec()),
    ];
    let d = w
        .seed_deployment(&app, Env::Preview, &files, None)
        .await
        .unwrap();
    let file = |tenant: &str, path: &str| {
        json!({
            "tenant_id": tenant, "app_id": app.id, "deployment_id": d.id,
            "path": path, "navigation": false,
        })
    };
    // The positive control: the file of the serving preview, with its entry.
    let r = core_post(&w, "/static/file", &file(&acme.id, "/secret.txt")).await;
    assert_eq!(r.status(), 200);
    let a: StaticFileAnswer = r.json().await.unwrap();
    let StaticFileAnswer::Found { entry, link } = a else {
        panic!("not found: {a:?}");
    };
    assert_eq!(entry.size, 9);
    assert_eq!(link, None, "the in-process store signs no links");
    let content = |tenant: &str, hash: &str| json!({ "tenant_id": tenant, "app_id": app.id, "deployment_id": d.id, "hash": hash });
    let r = core_post(&w, "/static/content", &content(&acme.id, &entry.hash)).await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.bytes().await.unwrap().as_ref(), b"acme only");
    // Another tenant in the query: the deployment does not serve there.
    let r = core_post(&w, "/static/file", &file(&globex.id, "/secret.txt")).await;
    let a: StaticFileAnswer = r.json().await.unwrap();
    assert_eq!(a, StaticFileAnswer::NotServing);
    let r = core_post(&w, "/static/content", &content(&globex.id, &entry.hash)).await;
    assert_eq!(r.status(), 404, "another tenant gets no bytes");
    // An unknown deployment of the app.
    let mut q = file(&acme.id, "/secret.txt");
    q["deployment_id"] = json!(Uuid::now_v7());
    let a: StaticFileAnswer = core_post(&w, "/static/file", &q)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(a, StaticFileAnswer::NotServing);
    // A bad path and a path that the index does not name.
    for p in ["/../secret.txt", "/%2e%2e/x", "/nothing.txt"] {
        let a: StaticFileAnswer = core_post(&w, "/static/file", &file(&acme.id, p))
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(a, StaticFileAnswer::NotFound, "{p}");
    }
    // A hash that the index does not name (another object of the tenant, or a made-up one).
    let other = blake3::hash(b"not in this site").to_hex().to_string();
    let r = core_post(&w, "/static/content", &content(&acme.id, &other)).await;
    assert_eq!(r.status(), 404);
}

/// A fake bucket: `/ok` holds `hello`, `/long` a longer object, `/gone` nothing.
async fn bucket() -> String {
    let app = Router::new()
        .route("/ok", get(|| async { "hello" }))
        .route("/long", get(|| async { "hello, and more" }))
        .route(
            "/gone",
            get(|| async { (axum::http::StatusCode::NOT_FOUND, "") }),
        );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(l, app).await;
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn a_link_is_fetched_and_checked_against_the_index_entry() {
    let base = bucket().await;
    // The core is not asked when there is a link.
    let s = CoreStatic::new("http://127.0.0.1:1").unwrap();
    let q = StaticFileQuery {
        tenant_id: TenantId::new(),
        app_id: NodeId::new(),
        deployment_id: Uuid::now_v7(),
        path: "/hello.txt".into(),
        navigation: false,
    };
    let entry = IndexEntry {
        hash: blake3::hash(b"hello").to_hex().to_string(),
        size: 5,
        media_type: "text/plain".into(),
    };
    let mut body = s
        .open(&q, &entry, Some(&format!("{base}/ok")))
        .await
        .unwrap()
        .expect("the object");
    let mut got = Vec::new();
    while let Some(c) = body.next().await {
        got.extend_from_slice(&c.unwrap());
    }
    assert_eq!(got, b"hello");
    assert!(
        s.open(&q, &entry, Some(&format!("{base}/long")))
            .await
            .is_err(),
        "an object longer than its entry"
    );
    assert!(
        s.open(&q, &entry, Some(&format!("{base}/gone")))
            .await
            .unwrap()
            .is_none(),
        "a missing object is the gateway's 404"
    );
    assert!(
        s.open(&q, &entry, Some("file:///etc/passwd"))
            .await
            .is_err()
    );
    assert!(s.open(&q, &entry, Some("not a url")).await.is_err());
    let huge = IndexEntry {
        size: ruum_gateway::static_files::MAX_FILE_BYTES + 1,
        ..entry.clone()
    };
    assert!(
        s.open(&q, &huge, Some(&format!("{base}/ok")))
            .await
            .is_err(),
        "over the gateway's file limit"
    );
}
