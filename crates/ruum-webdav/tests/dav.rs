//! WebDAV tests: sign-in, the operations, versions, permissions, scopes and tenant isolation.
//! They run the `/dav` router in memory on the fixture world of `ruum-testkit` (two team
//! tenants with every principal), with the fake tree of `support` until the nodes and blobs
//! areas are merged.

mod support;

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use base64::Engine;
use http_body_util::BodyExt;
use ruum_core::actor::{Actor, Context, PrincipalKind};
use ruum_core::journal::EventType;
use ruum_testkit::fixtures::{Credentials, TenantFixture, Who, World, seed};
use support::{Harness, harness};
use tower::ServiceExt;

struct T {
    h: Harness,
    w: World,
    app: axum::Router,
}

async fn setup() -> T {
    let h = harness();
    let w = seed(&support::app_state(&h.services)).await;
    assert!(w.seeded, "the fixture world must be seeded");
    let app = ruum_webdav::router(h.services.clone());
    T { h, w, app }
}

#[derive(Clone)]
enum Auth {
    None,
    Basic(String, String),
    Bearer(String),
}

fn member_basic(t: &TenantFixture) -> Auth {
    Auth::Basic(
        t.principal(Who::Member).email.clone().unwrap(),
        t.app_password_secret.clone(),
    )
}

fn bearer(t: &TenantFixture, who: Who) -> Auth {
    match &t.principal(who).credentials {
        Credentials::Bearer(tok) => Auth::Bearer(tok.clone()),
        _ => panic!("{} has no bearer token", who.name()),
    }
}

/// Makes an app password for a user of the fixture and returns the Basic credentials.
async fn app_password(h: &Harness, t: &TenantFixture, who: Who) -> Auth {
    let ctx = Context {
        tenant: t.id,
        actor: Actor::user(t.user_id(who)),
        scope: None,
        idempotency_key: None,
    };
    let (_, secret) = h
        .services
        .identity
        .create_app_password(&ctx, "test".into(), None)
        .await
        .unwrap();
    Auth::Basic(t.principal(who).email.clone().unwrap(), secret)
}

struct Res {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Res {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    fn header(&self, name: &str) -> String {
        self.headers
            .get_all(name)
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

async fn send(
    app: &axum::Router,
    method: &str,
    path: &str,
    auth: &Auth,
    headers: &[(&str, &str)],
    body: Body,
) -> Res {
    let mut req = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(path);
    match auth {
        Auth::None => {}
        Auth::Basic(u, p) => {
            let v = base64::engine::general_purpose::STANDARD.encode(format!("{u}:{p}"));
            req = req.header("authorization", format!("Basic {v}"));
        }
        Auth::Bearer(tok) => req = req.header("authorization", format!("Bearer {tok}")),
    }
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    Res {
        status,
        headers,
        body,
    }
}

async fn get(app: &axum::Router, path: &str, auth: &Auth) -> Res {
    send(app, "GET", path, auth, &[], Body::empty()).await
}

async fn put(app: &axum::Router, path: &str, auth: &Auth, data: &[u8]) -> Res {
    send(
        app,
        "PUT",
        path,
        auth,
        &[("content-length", &data.len().to_string())],
        Body::from(data.to_vec()),
    )
    .await
}

async fn propfind(app: &axum::Router, path: &str, auth: &Auth, depth: &str) -> Res {
    send(
        app,
        "PROPFIND",
        path,
        auth,
        &[("depth", depth)],
        Body::empty(),
    )
    .await
}

async fn mkcol(app: &axum::Router, path: &str, auth: &Auth) -> Res {
    send(app, "MKCOL", path, auth, &[], Body::empty()).await
}

async fn mv(app: &axum::Router, method: &str, from: &str, to: &str, auth: &Auth) -> Res {
    let dest = format!("https://ruum.localhost{to}");
    send(
        app,
        method,
        from,
        auth,
        &[("destination", &dest)],
        Body::empty(),
    )
    .await
}

// ------------------------------------------------------------------------------- sign-in

#[tokio::test]
async fn unauthenticated_requests_get_both_challenges() {
    let t = setup().await;
    let r = propfind(&t.app, "/dav/", &Auth::None, "1").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let challenge = r.header("www-authenticate");
    assert!(challenge.contains("Basic realm=\"Ruum\""), "{challenge}");
    assert!(challenge.contains("Bearer"), "{challenge}");
    assert!(r.text().contains("unauthenticated"));
}

#[tokio::test]
async fn other_credentials_are_refused() {
    let t = setup().await;
    let a = &t.w.a;
    let email = a.principal(Who::Member).email.clone().unwrap();
    let cases = [
        // A wrong app password.
        Auth::Basic(email.clone(), "ruum_app_wrong".into()),
        // The right password with another user's email.
        Auth::Basic(
            a.principal(Who::Admin).email.clone().unwrap(),
            a.app_password_secret.clone(),
        ),
        // A password that is not an app password.
        Auth::Basic(email, "hunter2".into()),
        // A user access token: WebDAV takes only agent tokens.
        bearer(a, Who::Owner),
        // A share-link token.
        bearer(a, Who::ShareLink),
        // An API key is never a bearer token.
        Auth::Bearer(a.agent_key_secret.clone()),
        Auth::Bearer("garbage".into()),
    ];
    for auth in cases {
        let r = propfind(&t.app, "/dav/", &auth, "0").await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text());
        assert!(r.header("www-authenticate").contains("Basic"));
    }
}

#[tokio::test]
async fn options_advertise_class_2_for_finder_and_windows() {
    let t = setup().await;
    let auth = member_basic(&t.w.a);
    let r = send(&t.app, "OPTIONS", "/dav/", &auth, &[], Body::empty()).await;
    assert_eq!(r.status, StatusCode::OK);
    let dav = r.header("dav");
    assert!(dav.split(',').any(|c| c.trim() == "2"), "DAV: {dav}");
    assert_eq!(r.header("ms-author-via"), "DAV");
    let allow = r.header("allow");
    for m in ["PROPFIND", "LOCK", "UNLOCK", "COPY"] {
        assert!(allow.contains(m), "Allow: {allow}");
    }
    // On a file; PATCH (partial update) is not offered.
    let path = format!("/dav/Projects/{}", t.w.a.plan_name);
    let r = send(&t.app, "OPTIONS", &path, &auth, &[], Body::empty()).await;
    let allow = r.header("allow");
    for m in ["GET", "PUT", "MOVE", "DELETE", "LOCK"] {
        assert!(allow.contains(m), "Allow: {allow}");
    }
    assert!(!allow.contains("PATCH"), "Allow: {allow}");
    // On a free name.
    let r = send(&t.app, "OPTIONS", "/dav/new", &auth, &[], Body::empty()).await;
    assert!(r.header("allow").contains("MKCOL"));
    // `/dav` without the slash works too.
    let r = send(&t.app, "OPTIONS", "/dav", &auth, &[], Body::empty()).await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn lock_and_unlock_answer_for_finder() {
    let t = setup().await;
    let auth = member_basic(&t.w.a);
    let path = format!("/dav/Projects/{}", t.w.a.plan_name);
    let body = r#"<?xml version="1.0" encoding="utf-8"?>
<D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope>
<D:locktype><D:write/></D:locktype><D:owner><D:href>finder</D:href></D:owner></D:lockinfo>"#;
    let r = send(
        &t.app,
        "LOCK",
        &path,
        &auth,
        &[("timeout", "Second-600"), ("content-type", "text/xml")],
        Body::from(body),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let token = r.header("lock-token");
    assert!(token.starts_with('<'), "Lock-Token: {token}");
    let r = send(
        &t.app,
        "UNLOCK",
        &path,
        &auth,
        &[("lock-token", &token)],
        Body::empty(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
}

// ------------------------------------------------------------------------------- operations

#[tokio::test]
async fn a_member_works_through_dav_and_every_write_is_the_user() {
    let t = setup().await;
    let a = &t.w.a;
    let auth = member_basic(a);
    let member = a.user_id(Who::Member);
    let before = t.h.journal.entries(a.id).len();

    // PROPFIND of the tenant root.
    let r = propfind(&t.app, "/dav/", &auth, "1").await;
    assert_eq!(r.status, StatusCode::MULTI_STATUS);
    let xml = r.text();
    assert!(xml.contains("/dav/Projects/"), "{xml}");
    assert!(xml.contains("/dav/Private/"), "{xml}");
    assert!(xml.contains("collection"), "{xml}");

    // MKCOL, and again (the name is taken).
    assert_eq!(
        mkcol(&t.app, "/dav/Docs", &auth).await.status,
        StatusCode::CREATED
    );
    assert_eq!(
        mkcol(&t.app, "/dav/Docs", &auth).await.status,
        StatusCode::METHOD_NOT_ALLOWED
    );
    // MKCOL without a parent.
    assert_eq!(
        mkcol(&t.app, "/dav/Nope/Inner", &auth).await.status,
        StatusCode::CONFLICT
    );

    // PUT a new file: a new node with one version.
    let r = put(&t.app, "/dav/Docs/hello.txt", &auth, b"hello world").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let docs = t.h.tree.child(a.root, "Docs").unwrap();
    let file = t.h.tree.child(docs.id, "hello.txt").unwrap();
    let v1 = file.head_version.unwrap();
    assert_eq!(r.header("etag"), format!("\"{v1}\""));
    assert_eq!(file.node_type.as_deref(), Some("text/plain"));

    // GET, HEAD and ranges.
    let r = get(&t.app, "/dav/Docs/hello.txt", &auth).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body, b"hello world");
    assert_eq!(r.header("etag"), format!("\"{v1}\""));
    assert!(r.header("content-type").starts_with("text/plain"));
    let r = send(
        &t.app,
        "GET",
        "/dav/Docs/hello.txt",
        &auth,
        &[("range", "bytes=6-10")],
        Body::empty(),
    )
    .await;
    assert_eq!(r.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.body, b"world");
    assert_eq!(r.header("content-range"), "bytes 6-10/11");
    let r = send(
        &t.app,
        "GET",
        "/dav/Docs/hello.txt",
        &auth,
        &[("range", "bytes=-5")],
        Body::empty(),
    )
    .await;
    assert_eq!(r.body, b"world");
    let r = send(
        &t.app,
        "HEAD",
        "/dav/Docs/hello.txt",
        &auth,
        &[],
        Body::empty(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.header("content-length"), "11");
    let r = send(
        &t.app,
        "GET",
        "/dav/Docs/hello.txt",
        &auth,
        &[("if-none-match", &format!("\"{v1}\""))],
        Body::empty(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_MODIFIED);

    // PUT again: a new version of the same node.
    let r = put(&t.app, "/dav/Docs/hello.txt", &auth, b"hello again!").await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let versions = t.h.tree.versions_of(file.id);
    assert_eq!(versions.len(), 2);
    assert_eq!(
        get(&t.app, "/dav/Docs/hello.txt", &auth).await.body,
        b"hello again!"
    );
    // A PUT with a stale If-Match is refused.
    let r = send(
        &t.app,
        "PUT",
        "/dav/Docs/hello.txt",
        &auth,
        &[("if-match", &format!("\"{v1}\"")), ("content-length", "1")],
        Body::from("x"),
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(t.h.tree.versions_of(file.id).len(), 2);

    // PROPFIND of the file.
    let r = propfind(&t.app, "/dav/Docs/hello.txt", &auth, "0").await;
    assert_eq!(r.status, StatusCode::MULTI_STATUS);
    assert!(
        r.text()
            .contains("<D:getcontentlength>12</D:getcontentlength>"),
        "{}",
        r.text()
    );

    // MOVE in the same folder is a rename; to another folder a move.
    let r = mv(
        &t.app,
        "MOVE",
        "/dav/Docs/hello.txt",
        "/dav/Docs/renamed.txt",
        &auth,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(
        get(&t.app, "/dav/Docs/hello.txt", &auth).await.status,
        StatusCode::NOT_FOUND
    );
    let r = mv(
        &t.app,
        "MOVE",
        "/dav/Docs/renamed.txt",
        "/dav/Projects/renamed.txt",
        &auth,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let moved = t.h.tree.node(file.id).unwrap();
    assert_eq!(moved.parent_id, Some(a.projects));
    assert_eq!(moved.name, "renamed.txt");
    // A case-only rename finds the node itself.
    let r = mv(
        &t.app,
        "MOVE",
        "/dav/Projects/renamed.txt",
        "/dav/Projects/Renamed.txt",
        &auth,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(t.h.tree.node(file.id).unwrap().name, "Renamed.txt");

    // COPY a file and a folder.
    let r = mv(
        &t.app,
        "COPY",
        "/dav/Projects/Renamed.txt",
        "/dav/Docs/copy.txt",
        &auth,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(
        get(&t.app, "/dav/Docs/copy.txt", &auth).await.body,
        b"hello again!"
    );
    let r = mv(&t.app, "COPY", "/dav/Docs", "/dav/Docs2", &auth).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(
        get(&t.app, "/dav/Docs2/copy.txt", &auth).await.body,
        b"hello again!"
    );
    // Overwrite: F onto an existing node.
    let r = send(
        &t.app,
        "COPY",
        "/dav/Docs/copy.txt",
        &auth,
        &[
            ("destination", "/dav/Projects/Renamed.txt"),
            ("overwrite", "F"),
        ],
        Body::empty(),
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED);
    // Overwrite (default T) of a file by a file: a new version of the same node.
    let r = mv(
        &t.app,
        "COPY",
        "/dav/Docs/copy.txt",
        "/dav/Projects/Renamed.txt",
        &auth,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    assert!(t.h.tree.node(file.id).unwrap().trashed_at.is_none());
    assert_eq!(t.h.tree.versions_of(file.id).len(), 3);
    // Overwrite of a folder: the old folder goes to the trash.
    let docs2 = t.h.tree.child(a.root, "Docs2").unwrap();
    let r = mv(&t.app, "COPY", "/dav/Docs", "/dav/Docs2", &auth).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    assert!(t.h.tree.node(docs2.id).unwrap().trashed_at.is_some());

    // DELETE a folder: one node to the trash.
    let r = send(&t.app, "DELETE", "/dav/Docs2", &auth, &[], Body::empty()).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    assert_eq!(
        propfind(&t.app, "/dav/Docs2", &auth, "0").await.status,
        StatusCode::NOT_FOUND
    );
    let entries = t.h.journal.entries(a.id);
    let trashed: Vec<_> = entries[before..]
        .iter()
        .filter(|e| e.event == EventType::NodeTrashed)
        .collect();
    assert_eq!(
        trashed.len(),
        2,
        "one for the overwrite, one for the folder"
    );
    // The root cannot be deleted or moved.
    let r = send(&t.app, "DELETE", "/dav/", &auth, &[], Body::empty()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // Every write through the app password is the member (decision 0001-7).
    let new = &entries[before..];
    assert!(new.len() >= 10, "{} entries", new.len());
    for e in new {
        assert_eq!(e.actor.actor.kind, PrincipalKind::User, "{:?}", e.event);
        assert_eq!(e.actor.actor.id.0, member.0);
        assert_eq!(e.actor.on_behalf_of, None);
    }
}

#[tokio::test]
async fn large_files_are_chunked_and_read_in_ranges() {
    let t = setup().await;
    let auth = member_basic(&t.w.a);
    // 6 MiB of varied bytes: the default chunking cuts it into a few chunks.
    let data: Vec<u8> = (0..6 * 1024 * 1024u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
        .collect();
    let r = put(&t.app, "/dav/Projects/big.bin", &auth, &data).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = get(&t.app, "/dav/Projects/big.bin", &auth).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body.len(), data.len());
    assert!(r.body == data);
    let r = send(
        &t.app,
        "GET",
        "/dav/Projects/big.bin",
        &auth,
        &[("range", "bytes=3000000-3000099")],
        Body::empty(),
    )
    .await;
    assert_eq!(r.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.body, &data[3_000_000..3_000_100]);
    // The same content again shares every chunk: the second version uploads nothing new.
    let r = put(&t.app, "/dav/Projects/big.bin", &auth, &data).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn finder_style_puts_work() {
    let t = setup().await;
    let auth = member_basic(&t.w.a);
    // Finder first makes an empty file, then sends the content chunked, without a length.
    let r = put(&t.app, "/dav/Projects/f.txt", &auth, b"").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(get(&t.app, "/dav/Projects/f.txt", &auth).await.body, b"");
    let parts: Vec<Result<bytes::Bytes, std::io::Error>> = vec![
        Ok(bytes::Bytes::from_static(b"from ")),
        Ok(bytes::Bytes::from_static(b"finder")),
    ];
    let r = send(
        &t.app,
        "PUT",
        "/dav/Projects/f.txt",
        &auth,
        &[("x-expected-entity-length", "11")],
        Body::from_stream(futures::stream::iter(parts)),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    assert_eq!(
        get(&t.app, "/dav/Projects/f.txt", &auth).await.body,
        b"from finder"
    );
    // A body shorter than its length stores nothing.
    let r = send(
        &t.app,
        "PUT",
        "/dav/Projects/f.txt",
        &auth,
        &[("x-expected-entity-length", "100")],
        Body::from("short"),
    )
    .await;
    assert!(r.status.is_client_error() || r.status.is_server_error());
    assert_eq!(
        get(&t.app, "/dav/Projects/f.txt", &auth).await.body,
        b"from finder"
    );
    // Names with spaces and non-ASCII letters.
    let r = put(&t.app, "/dav/Projects/%C3%A4%20b.txt", &auth, b"x").await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = propfind(&t.app, "/dav/Projects/", &auth, "1").await;
    assert!(
        r.text().contains("/dav/Projects/%C3%A4%20b.txt"),
        "{}",
        r.text()
    );
    // If-None-Match: * on an existing file is refused.
    let r = send(
        &t.app,
        "PUT",
        "/dav/Projects/f.txt",
        &auth,
        &[("if-none-match", "*"), ("content-length", "1")],
        Body::from("y"),
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED);
    // Partial PUT is not supported.
    let r = send(
        &t.app,
        "PUT",
        "/dav/Projects/f.txt",
        &auth,
        &[("content-range", "bytes 0-0/11"), ("content-length", "1")],
        Body::from("y"),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(
        get(&t.app, "/dav/Projects/f.txt", &auth).await.body,
        b"from finder"
    );
}

#[tokio::test]
async fn safe_save_by_move_keeps_the_node_and_its_versions() {
    let t = setup().await;
    let a = &t.w.a;
    let auth = member_basic(a);
    let r = put(&t.app, "/dav/Projects/a.txt", &auth, b"first").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let file = t.h.tree.child(a.projects, "a.txt").unwrap();
    // The editor writes a temporary file and moves it over the original.
    let r = put(&t.app, "/dav/Projects/.a.txt.tmp", &auth, b"second").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let tmp = t.h.tree.child(a.projects, ".a.txt.tmp").unwrap();
    let r = mv(
        &t.app,
        "MOVE",
        "/dav/Projects/.a.txt.tmp",
        "/dav/Projects/a.txt",
        &auth,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let now = t.h.tree.child(a.projects, "a.txt").unwrap();
    assert_eq!(now.id, file.id, "a.txt keeps its node ID");
    assert_eq!(t.h.tree.versions_of(file.id).len(), 2);
    assert_eq!(
        get(&t.app, "/dav/Projects/a.txt", &auth).await.body,
        b"second"
    );
    assert!(t.h.tree.node(tmp.id).unwrap().trashed_at.is_some());
    assert_eq!(
        get(&t.app, "/dav/Projects/.a.txt.tmp", &auth).await.status,
        StatusCode::NOT_FOUND
    );
    // A guest with edit may write a version but not trash the source: nothing changes.
    let guest = app_password(&t.h, a, Who::GuestEdit).await;
    let r = put(&t.app, "/dav/Projects/g.tmp", &guest, b"guest").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = mv(
        &t.app,
        "MOVE",
        "/dav/Projects/g.tmp",
        "/dav/Projects/a.txt",
        &guest,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(t.h.tree.versions_of(file.id).len(), 2);
}

#[tokio::test]
async fn a_failed_overwrite_restores_the_old_destination() {
    let t = setup().await;
    let a = &t.w.a;
    let auth = member_basic(a);
    // The seed trashes a node too: count only the trashes of this test.
    let trashed_before = trashed_count(&t, a.id);
    assert_eq!(
        mkcol(&t.app, "/dav/Projects/D", &auth).await.status,
        StatusCode::CREATED
    );
    assert_eq!(
        mkcol(&t.app, "/dav/Projects/D/In", &auth).await.status,
        StatusCode::CREATED
    );
    // MOVE a folder over a node that holds it: refused before anything changes.
    let r = mv(
        &t.app,
        "MOVE",
        "/dav/Projects/D/In",
        "/dav/Projects/D",
        &auth,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(
        propfind(&t.app, "/dav/Projects/D/In", &auth, "0")
            .await
            .status,
        StatusCode::MULTI_STATUS
    );
    // MOVE a folder into its own subtree, over an existing node there.
    assert_eq!(
        mkcol(&t.app, "/dav/Projects/D/In/X", &auth).await.status,
        StatusCode::CREATED
    );
    let r = mv(
        &t.app,
        "MOVE",
        "/dav/Projects/D",
        "/dav/Projects/D/In/X",
        &auth,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(
        propfind(&t.app, "/dav/Projects/D/In/X", &auth, "0")
            .await
            .status,
        StatusCode::MULTI_STATUS
    );
    assert_eq!(trashed_count(&t, a.id), trashed_before);
}

fn trashed_count(t: &T, tenant: ruum_core::ids::TenantId) -> usize {
    t.h.journal
        .entries(tenant)
        .iter()
        .filter(|e| e.event == EventType::NodeTrashed)
        .count()
}

#[tokio::test]
async fn lock_on_a_new_name_makes_the_file() {
    let t = setup().await;
    let a = &t.w.a;
    let auth = member_basic(a);
    let body = r#"<?xml version="1.0" encoding="utf-8"?>
<D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope>
<D:locktype><D:write/></D:locktype><D:owner><D:href>office</D:href></D:owner></D:lockinfo>"#;
    let r = send(
        &t.app,
        "LOCK",
        "/dav/Projects/new.docx",
        &auth,
        &[("timeout", "Second-600"), ("content-type", "text/xml")],
        Body::from(body),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let token = r.header("lock-token");
    let file = t.h.tree.child(a.projects, "new.docx").unwrap();
    assert_eq!(file.size_bytes, 0);
    let r = send(
        &t.app,
        "PUT",
        "/dav/Projects/new.docx",
        &auth,
        &[("if", &format!("({token})")), ("content-length", "7")],
        Body::from("content"),
    )
    .await;
    assert!(
        r.status == StatusCode::NO_CONTENT || r.status == StatusCode::CREATED,
        "{} {}",
        r.status,
        r.text()
    );
    let now = t.h.tree.child(a.projects, "new.docx").unwrap();
    assert_eq!(now.id, file.id, "one file");
    assert_eq!(
        get(&t.app, "/dav/Projects/new.docx", &auth).await.body,
        b"content"
    );
    // A guest with view cannot lock a new name into existence.
    let guest = app_password(&t.h, a, Who::GuestView).await;
    let r = send(
        &t.app,
        "LOCK",
        "/dav/Projects/g.docx",
        &guest,
        &[("timeout", "Second-600"), ("content-type", "text/xml")],
        Body::from(body),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(t.h.tree.child(a.projects, "g.docx").is_none());
}

#[tokio::test]
async fn bodies_over_the_plan_limit_are_refused() {
    let t = setup().await;
    let auth = member_basic(&t.w.a);
    // A declared size over the largest file of the plan: refused before the body is read.
    let r = send(
        &t.app,
        "PUT",
        "/dav/Projects/huge.bin",
        &auth,
        &[("content-length", "1099511627776")],
        Body::from("x"),
    )
    .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text());
    // A lying X-Expected-Entity-Length over the free storage.
    let r = send(
        &t.app,
        "PUT",
        "/dav/Projects/huge.bin",
        &auth,
        &[("x-expected-entity-length", "1099511627776")],
        Body::from("x"),
    )
    .await;
    assert!(
        r.status == StatusCode::PAYLOAD_TOO_LARGE || r.status == StatusCode::INSUFFICIENT_STORAGE,
        "{}",
        r.status
    );
    assert!(t.h.tree.child(t.w.a.projects, "huge.bin").is_none());
}

// ------------------------------------------------------------------------------- permissions

#[tokio::test]
async fn guest_with_view_reads_but_does_not_write() {
    let t = setup().await;
    let a = &t.w.a;
    let auth = app_password(&t.h, a, Who::GuestView).await;
    let plan = format!("/dav/Projects/{}", a.plan_name);
    assert_eq!(
        propfind(&t.app, "/dav/Projects", &auth, "1").await.status,
        StatusCode::MULTI_STATUS
    );
    let r = get(&t.app, &plan, &auth).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        put(&t.app, "/dav/Projects/x.txt", &auth, b"x").await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        put(&t.app, &plan, &auth, b"x").await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        mkcol(&t.app, "/dav/Projects/D", &auth).await.status,
        StatusCode::FORBIDDEN
    );
    let r = send(&t.app, "DELETE", &plan, &auth, &[], Body::empty()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = mv(&t.app, "MOVE", &plan, "/dav/Projects/other.txt", &auth).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // What the guest cannot see does not exist.
    assert_eq!(
        propfind(&t.app, "/dav/Private", &auth, "0").await.status,
        StatusCode::NOT_FOUND
    );
    // The root is not secret but not readable: a guest mounts the shared folder's URL.
    assert_eq!(
        propfind(&t.app, "/dav/", &auth, "0").await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        mv(&t.app, "COPY", &plan, "/dav/Private/x.txt", &auth)
            .await
            .status,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn guest_with_edit_writes_but_does_not_delete_or_move_out() {
    let t = setup().await;
    let a = &t.w.a;
    let auth = app_password(&t.h, a, Who::GuestEdit).await;
    assert_eq!(
        put(&t.app, "/dav/Projects/e.txt", &auth, b"e").await.status,
        StatusCode::CREATED
    );
    assert_eq!(
        put(&t.app, "/dav/Projects/e.txt", &auth, b"e2")
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        mkcol(&t.app, "/dav/Projects/E", &auth).await.status,
        StatusCode::CREATED
    );
    // Rename needs edit; a move to another folder and delete need manage.
    let r = mv(
        &t.app,
        "MOVE",
        "/dav/Projects/e.txt",
        "/dav/Projects/f.txt",
        &auth,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = mv(
        &t.app,
        "MOVE",
        "/dav/Projects/f.txt",
        "/dav/Projects/E/f.txt",
        &auth,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = send(
        &t.app,
        "DELETE",
        "/dav/Projects/f.txt",
        &auth,
        &[],
        Body::empty(),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // Copy needs view on the node and edit on the new parent.
    let r = mv(
        &t.app,
        "COPY",
        "/dav/Projects/f.txt",
        "/dav/Projects/E/f.txt",
        &auth,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    // A guest with manage deletes.
    let m = app_password(&t.h, a, Who::GuestManage).await;
    let r = send(
        &t.app,
        "DELETE",
        "/dav/Projects/f.txt",
        &m,
        &[],
        Body::empty(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn agents_act_within_their_scope() {
    let t = setup().await;
    let a = &t.w.a;
    let plan = format!("/dav/Projects/{}", a.plan_name);

    // The narrow agent: scope Projects, max role view.
    let narrow = bearer(a, Who::Agent);
    let before = t.h.journal.entries(a.id).len();
    assert_eq!(
        propfind(&t.app, "/dav/Projects", &narrow, "1").await.status,
        StatusCode::MULTI_STATUS
    );
    assert_eq!(get(&t.app, &plan, &narrow).await.status, StatusCode::OK);
    assert_eq!(
        put(&t.app, &plan, &narrow, b"x").await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        put(&t.app, "/dav/Projects/n.txt", &narrow, b"x")
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        propfind(&t.app, "/dav/Private", &narrow, "0").await.status,
        StatusCode::NOT_FOUND
    );
    // A scoped agent mounts its scope by URL; the root answers 403, as `NodeStore::root`.
    assert_eq!(
        propfind(&t.app, "/dav/", &narrow, "0").await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(t.h.journal.entries(a.id).len(), before);

    // The agent with scope Private and max role manage writes there, for the member.
    let out = bearer(a, Who::AgentOut);
    let r = put(&t.app, "/dav/Private/a.txt", &out, b"agent").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(
        propfind(&t.app, "/dav/Projects", &out, "0").await.status,
        StatusCode::NOT_FOUND
    );
    let r = mv(
        &t.app,
        "COPY",
        "/dav/Private/a.txt",
        "/dav/Projects/a.txt",
        &out,
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CONFLICT,
        "the destination is outside the scope"
    );
    let entries = t.h.journal.entries(a.id);
    let last = entries.last().unwrap();
    assert_eq!(last.actor.actor.kind, PrincipalKind::Agent);
    assert_eq!(last.actor.actor.id.0, a.agent_id(Who::AgentOut).0);
    assert_eq!(last.actor.on_behalf_of, Some(a.user_id(Who::Member)));

    // The agent of guest_view never gets more than view.
    let g = bearer(a, Who::AgentOfGuestView);
    assert_eq!(get(&t.app, &plan, &g).await.status, StatusCode::OK);
    assert_eq!(
        put(&t.app, "/dav/Projects/g.txt", &g, b"x").await.status,
        StatusCode::FORBIDDEN
    );
}

// ------------------------------------------------------------------------------- isolation

#[tokio::test]
async fn each_credential_sees_only_its_tenant() {
    let t = setup().await;
    let (a, b) = (&t.w.a, &t.w.b);
    let auth_b = member_basic(b);
    // B's member sees B's tree, with nothing of A in it.
    // Depth infinity is refused (propfind-finite-depth); clients walk the tree.
    let r = propfind(&t.app, "/dav/Projects", &auth_b, "infinity").await;
    assert_eq!(r.status, StatusCode::NOT_IMPLEMENTED);
    let r = propfind(&t.app, "/dav/Projects", &auth_b, "1").await;
    assert_eq!(r.status, StatusCode::MULTI_STATUS);
    let xml = r.text();
    assert!(xml.contains(&b.plan_name), "{xml}");
    assert!(!xml.contains(a.prefix), "{xml}");
    let a_plan = format!("/dav/Projects/{}", a.plan_name);
    assert_eq!(
        get(&t.app, &a_plan, &auth_b).await.status,
        StatusCode::NOT_FOUND
    );
    // A tenant header does not move a credential to another tenant.
    let r = send(
        &t.app,
        "PROPFIND",
        "/dav/Projects",
        &auth_b,
        &[("depth", "1"), ("ruum-tenant", &a.id.to_string())],
        Body::empty(),
    )
    .await;
    assert!(!r.text().contains(a.prefix));
    // B's agent token sees nothing of A.
    let r = propfind(&t.app, "/dav/Private", &bearer(b, Who::AgentOut), "1").await;
    assert_eq!(r.status, StatusCode::MULTI_STATUS);
    assert!(!r.text().contains(a.prefix));
    // A PUT to the same path makes a file in B, not a version in A.
    assert_eq!(
        put(&t.app, &a_plan, &auth_b, b"x").await.status,
        StatusCode::CREATED
    );
    assert_eq!(t.h.tree.versions_of(a.plan).len(), 2);
}

#[tokio::test]
async fn a_removed_member_loses_dav_and_the_rest_api_refuses_app_passwords() {
    let t = setup().await;
    let a = &t.w.a;
    let auth = member_basic(a);
    assert_eq!(
        propfind(&t.app, "/dav/", &auth, "0").await.status,
        StatusCode::MULTI_STATUS
    );

    // The REST API refuses the app password, as Bearer and as Basic.
    let api = ruum_api::router(support::app_state(&t.h.services));
    let r = send(
        &api,
        "GET",
        "/v1/me",
        &Auth::Bearer(a.app_password_secret.clone()),
        &[],
        Body::empty(),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = send(&api, "GET", "/v1/me", &auth, &[], Body::empty()).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    // An admin removes the member: the app password stops at once.
    let admin = Context {
        tenant: a.id,
        actor: Actor::user(a.user_id(Who::Admin)),
        scope: None,
        idempotency_key: None,
    };
    t.h.services
        .tenants
        .remove_member(&admin, a.user_id(Who::Member))
        .await
        .unwrap();
    let r = propfind(&t.app, "/dav/", &auth, "0").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn propfind_gives_the_properties_finder_asks_for() {
    let t = setup().await;
    let a = &t.w.a;
    let body = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:"><D:prop><D:getlastmodified/><D:getcontentlength/>
<D:creationdate/><D:resourcetype/><D:getetag/><D:quota-available-bytes/>
<D:quota-used-bytes/></D:prop></D:propfind>"#;
    let r = send(
        &t.app,
        "PROPFIND",
        "/dav/",
        &member_basic(a),
        &[("depth", "1"), ("content-type", "text/xml")],
        Body::from(body),
    )
    .await;
    assert_eq!(r.status, StatusCode::MULTI_STATUS);
    let xml = r.text();
    for p in [
        "getlastmodified",
        "creationdate",
        "<D:collection",
        "getetag",
        "quota-available-bytes",
        "quota-used-bytes",
    ] {
        assert!(xml.contains(p), "{p} in {xml}");
    }
    // Guests may not read the tenant usage: the quota is left out, the listing still works.
    let g = app_password(&t.h, a, Who::GuestView).await;
    let r = send(
        &t.app,
        "PROPFIND",
        "/dav/Projects/",
        &g,
        &[("depth", "1"), ("content-type", "text/xml")],
        Body::from(body),
    )
    .await;
    assert_eq!(r.status, StatusCode::MULTI_STATUS);
    assert!(r.text().contains(&a.plan_name));
}

#[tokio::test]
async fn the_router_merges_with_the_rest_api_as_in_the_server() {
    let t = setup().await;
    // `ruum-server` merges the routers; overlapping routes would panic here.
    let app = ruum_api::router(support::app_state(&t.h.services))
        .merge(ruum_webdav::router(t.h.services.clone()));
    let r = propfind(&app, "/dav/", &member_basic(&t.w.a), "0").await;
    assert_eq!(r.status, StatusCode::MULTI_STATUS);
    let r = send(&app, "GET", "/v1/me", &Auth::None, &[], Body::empty()).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}
