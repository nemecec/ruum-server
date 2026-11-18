//! The proxy to sandboxes (card G-1b "Acceptance tests" 2), against the T-6 world of
//! `tests/support`: the in-memory core with its `SandboxDirectory`, the memory driver, and the
//! gateway on its own listener. Every probe that checks "not reached" has a positive control
//! that reaches the same target in the same test, and a refusal counts only as the gateway's own
//! answer (its status and page), never as a timeout or a connection error.

mod support;

use support::*;

use ruum_core::identity::app_sign_in::AppEnv;
use ruum_core::identity::app_sign_in::wire::{UpstreamAnswer, UpstreamQuery};
use ruum_gateway::proxy::ProxyConfig;
use ruum_gateway::sandbox::{HoldConfig, HttpSandboxes, Sandboxes};
use ruum_testkit::gateway::echo::{WsClient, WsOpen, ws_open};
use ruum_testkit::gateway::{
    self, Access, App, Deployment, Echo, Env, GatewayWorld, Relation, Req, Resp, Tenant, User,
    WsEcho,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use uuid::Uuid;

const T: Duration = Duration::from_secs(20);

async fn send(w: &World, req: Req) -> Resp {
    gateway::http::send(w.gateway, &req, T)
        .await
        .expect("the gateway answers")
}

/// A public app (approved) of a new tenant with a live node22 deployment that answers as
/// `upstream`. Anonymous visitors pass the check.
async fn public_sandbox(
    w: &World,
    tenant: &str,
    slug: &str,
    upstream: &str,
) -> (String, Deployment, Tenant) {
    let t = w.seed_tenant(tenant).await.unwrap();
    w.set_public_approval(&t, true).await.unwrap();
    let a = w.seed_app(&t, slug, Access::Public).await.unwrap();
    let d = w
        .seed_node_deployment(&a, Env::Live, upstream)
        .await
        .unwrap();
    w.set_live(&a, &d).await.unwrap();
    (format!("{slug}.{tenant}.{APPS_DOMAIN}"), d, t)
}

/// The app session cookie of `user` on `host`, through the gateway's real code flow.
async fn session(w: &World, user: &User, host: &str) -> String {
    let (target, nonce) = code_for(w, user, host).await;
    let cb = w
        .gateway_get(host, &target, Some(&format!("__Host-ruum_auth={nonce}")))
        .await
        .unwrap();
    format!(
        "__Host-ruum_app={}",
        cookie_value(&cb, "__Host-ruum_app").expect("a session")
    )
}

/// A small app: `/slow` sends its first chunk and waits for `release` before its last one;
/// `/redirect` answers 302 to `location`; `/body` reads its body whole and records its length;
/// anything else answers `app`.
struct TestApp {
    url: String,
    release: Arc<tokio::sync::Notify>,
    last_sent: Arc<AtomicBool>,
    bodies: Arc<std::sync::Mutex<Vec<usize>>>,
    hits: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl TestApp {
    async fn start(location: &str) -> TestApp {
        use axum::body::Body;
        use axum::extract::Request;
        use axum::response::Response;
        let release = Arc::new(tokio::sync::Notify::new());
        let last_sent = Arc::new(AtomicBool::new(false));
        let bodies: Arc<std::sync::Mutex<Vec<usize>>> = Arc::default();
        let hits = Arc::new(AtomicUsize::new(0));
        let (r, l, b, h, loc) = (
            release.clone(),
            last_sent.clone(),
            bodies.clone(),
            hits.clone(),
            location.to_string(),
        );
        let router = axum::Router::new().fallback(move |req: Request| {
            let (r, l, b, h, loc) = (r.clone(), l.clone(), b.clone(), h.clone(), loc.clone());
            async move {
                h.fetch_add(1, Ordering::SeqCst);
                match req.uri().path() {
                    "/slow" => {
                        let stream = futures::stream::unfold(0u8, move |step| {
                            let (r, l) = (r.clone(), l.clone());
                            async move {
                                match step {
                                    0 => Some((
                                        Ok::<_, std::io::Error>(bytes::Bytes::from(
                                            "first-chunk\n",
                                        )),
                                        1,
                                    )),
                                    1 => {
                                        r.notified().await;
                                        l.store(true, Ordering::SeqCst);
                                        Some((Ok(bytes::Bytes::from("last-chunk\n")), 2))
                                    }
                                    _ => None,
                                }
                            }
                        });
                        Response::new(Body::from_stream(stream))
                    }
                    "/redirect" => Response::builder()
                        .status(302)
                        .header("location", loc)
                        .body(Body::empty())
                        .unwrap(),
                    "/body" => {
                        if let Ok(all) = axum::body::to_bytes(req.into_body(), 1 << 24).await {
                            b.lock().unwrap().push(all.len());
                        }
                        Response::new(Body::from("got"))
                    }
                    _ => Response::new(Body::from("app")),
                }
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        TestApp {
            url: format!("http://{addr}"),
            release,
            last_sent,
            bodies,
            hits,
            task,
        }
    }
}

/// Reads from `s` until `buf` holds `needle`, at most `limit`.
async fn read_until(s: &mut TcpStream, buf: &mut Vec<u8>, needle: &[u8], limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if buf.windows(needle.len()).any(|w| w == needle) {
            return true;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        let mut chunk = [0u8; 4096];
        match tokio::time::timeout(left, s.read(&mut chunk)).await {
            Ok(Ok(n)) if n > 0 => buf.extend_from_slice(&chunk[..n]),
            _ => return false,
        }
    }
}

fn status_of(raw: &[u8]) -> Option<u16> {
    std::str::from_utf8(raw.get(9..12)?).ok()?.parse().ok()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answers_stream_and_the_body_limit_holds() {
    let w = World::with(Opts {
        proxy: ProxyConfig {
            max_body: 1024,
            ..ProxyConfig::default()
        },
        ..Opts::default()
    })
    .await
    .unwrap();
    let app = TestApp::start("/").await;
    let (host, _, _) = public_sandbox(&w, "acme", "garden", &app.url).await;
    // The first chunk reaches the visitor before the app sends its last one.
    let mut s = TcpStream::connect(w.gateway).await.unwrap();
    s.write_all(
        format!("GET /slow HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut buf = Vec::new();
    assert!(
        read_until(&mut s, &mut buf, b"first-chunk", Duration::from_secs(5)).await,
        "the first chunk did not arrive: {}",
        String::from_utf8_lossy(&buf)
    );
    assert_eq!(status_of(&buf), Some(200));
    assert!(!app.last_sent.load(Ordering::SeqCst));
    app.release.notify_one();
    assert!(read_until(&mut s, &mut buf, b"last-chunk", Duration::from_secs(5)).await);
    // A body within the limit reaches the app whole (positive control).
    let ok = send(&w, Req::post(&host, "/body").body(vec![b'a'; 100])).await;
    assert_eq!(ok.status, 200);
    assert_eq!(*app.bodies.lock().unwrap(), vec![100]);
    let hits = app.hits.load(Ordering::SeqCst);
    // A declared body over the limit: 413 at once, the app sees nothing.
    let big = send(&w, Req::post(&host, "/body").body(vec![b'a'; 1025])).await;
    assert_eq!(big.status, 413);
    assert!(big.text().contains("too large"));
    assert_eq!(app.hits.load(Ordering::SeqCst), hits);
    // A chunked body that passes the limit: 413, and the app never gets a whole body.
    let mut s = TcpStream::connect(w.gateway).await.unwrap();
    let mut raw = format!(
        "POST /body HTTP/1.1\r\nHost: {host}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
    )
    .into_bytes();
    for _ in 0..3 {
        raw.extend_from_slice(b"258\r\n");
        raw.extend(std::iter::repeat_n(b'b', 600));
        raw.extend_from_slice(b"\r\n");
    }
    raw.extend_from_slice(b"0\r\n\r\n");
    s.write_all(&raw).await.unwrap();
    let mut answer = Vec::new();
    let _ = tokio::time::timeout(T, s.read_to_end(&mut answer)).await;
    assert_eq!(
        status_of(&answer),
        Some(413),
        "{}",
        String::from_utf8_lossy(&answer)
    );
    assert_eq!(*app.bodies.lock().unwrap(), vec![100]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_request_never_chooses_the_upstream() {
    let w = World::new().await.unwrap();
    let decoy = Echo::start().await;
    let app = Echo::start().await;
    let (host, _, _) = public_sandbox(&w, "acme", "garden", &app.url).await;
    let d = format!("127.0.0.1:{}", decoy.addr.port());
    // Positive control: the app's own instance answers.
    let r = send(&w, Req::get(&host, "/")).await;
    assert!(gateway::is_echo(&r), "{} {}", r.status, r.text());
    // An absolute-form target naming the decoy: the gateway's 400.
    let r = send(&w, Req::get(&host, &format!("http://{d}/x"))).await;
    assert_eq!(r.status, 400);
    assert!(!gateway::is_echo(&r));
    // An absolute-form target naming the app's own host: the app.
    let r = send(&w, Req::get(&host, &format!("http://{host}/abs"))).await;
    assert!(gateway::is_echo(&r));
    // A Host that names the decoy: the gateway's 404.
    let r = send(&w, Req::get(&d, "/x")).await;
    assert_eq!(r.status, 404);
    assert!(!gateway::is_echo(&r));
    // Headers that try to name an upstream, and paths that look like one: the app or the
    // gateway's refusal, never the decoy.
    for req in [
        Req::get(&host, "/h").header("X-Forwarded-Host", &d),
        Req::get(&host, "/h").header("Forwarded", &format!("host={d}")),
        Req::get(&host, "/h").header("Ruum-Upstream", &format!("http://{d}")),
        Req::get(&host, "/h").header("X-Ruum-Upstream", &format!("http://{d}")),
        Req::get(&host, "/h").header("X-Original-URL", &format!("http://{d}/")),
        Req::get(&host, &format!("/../http://{d}/")),
        Req::get(&host, &format!("//{d}/x")),
        Req::get(&host, &format!("/%2e%2e/http:%2f%2f{d}/")),
    ] {
        let label = req.label();
        let r = send(&w, req).await;
        if gateway::is_echo(&r) {
            let v = r.json().unwrap();
            assert_eq!(
                gateway::echoed_header(&v, "host"),
                vec![host.clone()],
                "{label}"
            );
        } else {
            assert!(
                matches!(r.status, 400 | 404),
                "{label}: {} {}",
                r.status,
                r.text()
            );
        }
    }
    assert_eq!(decoy.hits(), 0, "a request reached the decoy");
    assert!(app.hits() >= 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redirects_of_the_app_reach_the_visitor() {
    let w = World::new().await.unwrap();
    let decoy = Echo::start().await;
    let target = format!("{}/followed", decoy.url);
    let app = TestApp::start(&target).await;
    let (host, _, _) = public_sandbox(&w, "acme", "garden", &app.url).await;
    let r = send(&w, Req::get(&host, "/redirect")).await;
    assert_eq!(r.status, 302);
    assert_eq!(r.header("location"), Some(target.as_str()));
    assert_eq!(decoy.hits(), 0, "the gateway followed the app's redirect");
    // Positive control: the decoy answers when asked directly.
    let direct = gateway::http::send(decoy.addr, &Req::get("decoy", "/followed"), T)
        .await
        .unwrap();
    assert!(gateway::is_echo(&direct));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn preview_and_live_hosts_reach_their_own_instance() {
    let w = World::new().await.unwrap();
    let live_echo = Echo::start().await;
    let preview_echo = Echo::start().await;
    let t = w.seed_tenant("acme").await.unwrap();
    let u = w.seed_user(&t, "mari").await.unwrap();
    let a = w.seed_app(&t, "garden", Access::Platform).await.unwrap();
    let live = w
        .seed_node_deployment(&a, Env::Live, &live_echo.url)
        .await
        .unwrap();
    w.set_live(&a, &live).await.unwrap();
    let preview = w
        .seed_node_deployment(&a, Env::Preview, &preview_echo.url)
        .await
        .unwrap();
    w.grant(&u, &a, &Relation::View).await.unwrap();
    w.grant(&u, &a, &Relation::Edit).await.unwrap();
    let live_host = format!("garden.acme.{APPS_DOMAIN}");
    let preview_host = format!("{}--garden.acme.{APPS_DOMAIN}", preview.short_id);
    let live_cookie = session(&w, &u, &live_host).await;
    let preview_cookie = session(&w, &u, &preview_host).await;
    let r = send(
        &w,
        Req::get(&live_host, "/l").header("Cookie", &live_cookie),
    )
    .await;
    assert!(gateway::is_echo(&r), "{} {}", r.status, r.text());
    assert_eq!((live_echo.hits(), preview_echo.hits()), (1, 0));
    let r = send(
        &w,
        Req::get(&preview_host, "/p").header("Cookie", &preview_cookie),
    )
    .await;
    assert!(gateway::is_echo(&r), "{} {}", r.status, r.text());
    assert_eq!((live_echo.hits(), preview_echo.hits()), (1, 1));
    // The preview's token never reaches the live app: the live session does not open the
    // preview, the preview session does not open the live app.
    let r = send(
        &w,
        Req::get(&preview_host, "/x").header("Cookie", &live_cookie),
    )
    .await;
    assert!(!gateway::is_echo(&r));
    let r = send(
        &w,
        Req::get(&live_host, "/x").header("Cookie", &preview_cookie),
    )
    .await;
    assert!(!gateway::is_echo(&r));
    assert_eq!((live_echo.hits(), preview_echo.hits()), (1, 1));
    let tokens: Vec<String> = live_echo
        .seen()
        .iter()
        .chain(preview_echo.seen().iter())
        .map(|s| s.header_all("ruum-user").concat())
        .collect();
    assert_ne!(tokens[0], tokens[1]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_internal_route_gives_no_upstream_to_a_forged_query() {
    let w = World::new().await.unwrap();
    let app = Echo::start().await;
    let t = w.seed_tenant("acme").await.unwrap();
    let other = w.seed_tenant("other").await.unwrap();
    let a = w.seed_app(&t, "garden", Access::Platform).await.unwrap();
    let b = w
        .seed_app(&other, "garden", Access::Platform)
        .await
        .unwrap();
    let d = w
        .seed_node_deployment(&a, Env::Preview, &app.url)
        .await
        .unwrap();
    let core = HttpSandboxes::new(&format!("http://{}", w.internal)).unwrap();
    let q = w.query(&d).unwrap();
    // Positive control.
    assert!(matches!(
        core.upstream(&q).await.unwrap(),
        UpstreamAnswer::Ready { .. }
    ));
    let other_id = other.id.parse().unwrap();
    for bad in [
        UpstreamQuery {
            tenant_id: other_id,
            ..q.clone()
        },
        UpstreamQuery {
            tenant_id: other_id,
            app_id: b.id.parse().unwrap(),
            ..q.clone()
        },
        UpstreamQuery {
            deployment_id: Uuid::now_v7(),
            ..q.clone()
        },
        UpstreamQuery {
            environment: AppEnv::Live,
            ..q.clone()
        },
    ] {
        assert_eq!(
            core.upstream(&bad).await.unwrap(),
            UpstreamAnswer::NotRunning,
            "{bad:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn twenty_requests_to_a_stopped_app_cause_one_start() {
    let w = World::new().await.unwrap();
    let app = Echo::start().await;
    let (host, d, _) = public_sandbox(&w, "acme", "garden", &app.url).await;
    assert!(gateway::is_echo(&send(&w, Req::get(&host, "/")).await));
    w.stop_instance(&d).await.unwrap();
    w.driver.set_delay(Duration::from_millis(300));
    let before = w.driver.starts();
    let answers = futures::future::join_all((0..20).map(|i| {
        let req = Req::get(&host, &format!("/r{i}"));
        let addr = w.gateway;
        async move { gateway::http::send(addr, &req, T).await.unwrap() }
    }))
    .await;
    for r in &answers {
        assert!(gateway::is_echo(r), "{} {}", r.status, r.text());
    }
    assert_eq!(
        w.driver.starts() - before,
        1,
        "one start for all held requests"
    );
    assert_eq!(app.hits(), 21);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_start_slower_than_the_hold_gets_the_starting_page() {
    let w = World::with(Opts {
        hold: HoldConfig {
            hold: Duration::from_secs(1),
            ..HoldConfig::default()
        },
        ..Opts::default()
    })
    .await
    .unwrap();
    let app = Echo::start().await;
    let (host, d, _) = public_sandbox(&w, "acme", "garden", &app.url).await;
    w.stop_instance(&d).await.unwrap();
    w.driver.set_delay(Duration::from_millis(2500));
    let t = Instant::now();
    let r = send(&w, Req::get(&host, "/")).await;
    assert_eq!(r.status, 503);
    assert!(
        t.elapsed() >= Duration::from_millis(900),
        "{:?}",
        t.elapsed()
    );
    assert_eq!(r.header("retry-after"), Some("5"));
    assert!(r.text().contains("starting"));
    assert!(!r.text().contains("127.0.0.1"));
    // The start goes on in the background: a later request is answered (positive control).
    let mut ok = false;
    for _ in 0..30 {
        if gateway::is_echo(&send(&w, Req::get(&host, "/later")).await) {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(ok, "the app never came up");
    assert_eq!(w.driver.starts(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_apps_cold_start_cannot_take_the_slots_of_another() {
    let w = World::with(Opts {
        hold: HoldConfig {
            hold: Duration::from_secs(8),
            per_app: 2,
            ..HoldConfig::default()
        },
        ..Opts::default()
    })
    .await
    .unwrap();
    let app_a = Echo::start().await;
    let app_b = Echo::start().await;
    let (host_a, da, _) = public_sandbox(&w, "acme", "garden", &app_a.url).await;
    let (host_b, db, _) = public_sandbox(&w, "beta", "orchard", &app_b.url).await;
    w.stop_instance(&da).await.unwrap();
    w.stop_instance(&db).await.unwrap();
    w.driver.set_delay(Duration::from_millis(1500));
    let addr = w.gateway;
    let held_a: Vec<_> = (0..2)
        .map(|i| {
            let req = Req::get(&host_a, &format!("/a{i}"));
            tokio::spawn(async move { gateway::http::send(addr, &req, T).await.unwrap() })
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let b = {
        let req = Req::get(&host_b, "/b");
        tokio::spawn(async move { gateway::http::send(addr, &req, T).await.unwrap() })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    // App A's third request is over A's share: the starting page at once.
    let t = Instant::now();
    let over = send(&w, Req::get(&host_a, "/a-over")).await;
    assert_eq!(over.status, 503);
    assert!(over.header("retry-after").is_some());
    assert!(
        t.elapsed() < Duration::from_millis(500),
        "{:?}",
        t.elapsed()
    );
    // App B's request was held and is answered; so are A's two.
    let rb = b.await.unwrap();
    assert!(gateway::is_echo(&rb), "{} {}", rb.status, rb.text());
    for h in held_a {
        let r = h.await.unwrap();
        assert!(gateway::is_echo(&r), "{} {}", r.status, r.text());
    }
    assert_eq!(app_a.hits(), 2);
    assert_eq!(app_b.hits(), 1);
    assert_eq!(w.gw.sandboxes.held(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_pages_name_nothing_internal() {
    let w = World::with(Opts {
        proxy: ProxyConfig {
            first_byte: Duration::from_secs(1),
            ..ProxyConfig::default()
        },
        ..Opts::default()
    })
    .await
    .unwrap();
    // Crashed: 502.
    let app = Echo::start().await;
    let (host, d, _) = public_sandbox(&w, "acme", "garden", &app.url).await;
    assert!(gateway::is_echo(&send(&w, Req::get(&host, "/")).await));
    let instance = w.crash_instance(&d).await.unwrap();
    let r = send(&w, Req::get(&host, "/")).await;
    assert_eq!(r.status, 502);
    let body = r.text();
    for leak in [
        app.url.as_str(),
        "127.0.0.1",
        &instance.to_string(),
        &instance.0.simple().to_string(),
        "exit",
        "memory",
        "exited",
    ] {
        assert!(!body.contains(leak), "the 502 page holds {leak:?}: {body}");
    }
    // An app that takes the connection and never answers: 504.
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let silent_addr = silent.local_addr().unwrap();
    let keep = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((s, _)) = silent.accept().await {
            held.push(s);
        }
    });
    let (host, _, _) =
        public_sandbox(&w, "beta", "orchard", &format!("http://{silent_addr}")).await;
    let r = send(&w, Req::get(&host, "/")).await;
    assert_eq!(r.status, 504);
    assert!(!r.text().contains("127.0.0.1"));
    assert!(!r.text().contains(&silent_addr.port().to_string()));
    keep.abort();
}

fn upgrade(host: &str, origin: &str) -> Req {
    Req::get(host, "/ws")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("Origin", origin)
}

async fn open(w: &World, req: Req) -> WsClient {
    match ws_open(w.gateway, &req, T).await.unwrap() {
        WsOpen::Open(_, c) => c,
        WsOpen::Refused(r) => panic!("expected 101, got {} {}", r.status, r.text()),
    }
}

/// Whether the WebSocket still echoes a frame.
async fn echoes(c: &mut WsClient, text: &str) -> bool {
    if c.send_text(text).await.is_err() {
        return false;
    }
    matches!(
        c.recv(Duration::from_secs(2)).await,
        Ok(Some(f)) if f.opcode == 1 && f.text() == text
    )
}

/// Waits until the gateway closes `c` (a close frame `1008`, or the end of the connection);
/// returns how long after `since` that was.
async fn closed_after(c: &mut WsClient, since: Instant, bound: Duration) -> Option<Duration> {
    while since.elapsed() < bound {
        if c.send_text("still?").await.is_err() {
            return Some(since.elapsed());
        }
        match c.recv(Duration::from_millis(500)).await {
            Ok(None) => return Some(since.elapsed()),
            Ok(Some(f)) if f.opcode == 8 => {
                assert_eq!(f.close_code(), Some(1008));
                return Some(since.elapsed());
            }
            _ => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_removed_role_ends_an_open_websocket_within_5_seconds() {
    let w = World::new().await.unwrap();
    let ws = WsEcho::start().await;
    let t = w.seed_tenant("acme").await.unwrap();
    let a: App = w.seed_app(&t, "garden", Access::Platform).await.unwrap();
    let d = w
        .seed_node_deployment(&a, Env::Live, &ws.url)
        .await
        .unwrap();
    w.set_live(&a, &d).await.unwrap();
    let kept = w.seed_user(&t, "kept").await.unwrap();
    let lost = w.seed_user(&t, "lost").await.unwrap();
    let member = Relation::Role("member".into());
    w.grant(&kept, &a, &member).await.unwrap();
    w.grant(&lost, &a, &member).await.unwrap();
    let host = format!("garden.acme.{APPS_DOMAIN}");
    let own = format!("https://{host}");
    let kept_cookie = session(&w, &kept, &host).await;
    let lost_cookie = session(&w, &lost, &host).await;
    let mut kc = open(&w, upgrade(&host, &own).header("Cookie", &kept_cookie)).await;
    let mut lc = open(&w, upgrade(&host, &own).header("Cookie", &lost_cookie)).await;
    assert!(echoes(&mut kc, "k1").await && echoes(&mut lc, "l1").await);
    w.revoke(&lost, &a, &member).await.unwrap();
    let since = Instant::now();
    let took = closed_after(&mut lc, since, Duration::from_secs(6))
        .await
        .expect("the WebSocket stayed open after the role was removed");
    assert!(
        took <= Duration::from_secs(5) + gateway::POLL_ALLOWANCE,
        "{took:?}"
    );
    // Positive control: the WebSocket whose role stays is open over the same time.
    while since.elapsed() < Duration::from_secs(6) {
        assert!(echoes(&mut kc, "k2").await, "the kept WebSocket closed");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn websockets_keep_the_origin_rule_their_share_and_the_public_approval() {
    let w = World::with(Opts {
        hold: HoldConfig {
            ws_per_app: 1,
            ..HoldConfig::default()
        },
        ..Opts::default()
    })
    .await
    .unwrap();
    let ws_a = WsEcho::start().await;
    let ws_b = WsEcho::start().await;
    let (host_a, _, t_a) = public_sandbox(&w, "acme", "garden", &ws_a.url).await;
    let (host_b, _, _) = public_sandbox(&w, "beta", "orchard", &ws_b.url).await;
    // Another origin: the gateway's 403, nothing reaches the app.
    let r = send(&w, upgrade(&host_a, &format!("https://{host_b}"))).await;
    assert_eq!(r.status, 403);
    let r = send(
        &w,
        Req::get(&host_a, "/ws")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ=="),
    )
    .await;
    assert_eq!(
        r.status, 403,
        "an upgrade without Origin and without a token"
    );
    assert_eq!(ws_a.hits(), 0);
    // The own origin opens (positive control).
    let mut c = open(&w, upgrade(&host_a, &format!("https://{host_a}"))).await;
    assert!(echoes(&mut c, "a1").await);
    // A second WebSocket of app A is over its share: 503; app B still opens one.
    let r = send(&w, upgrade(&host_a, &format!("https://{host_a}"))).await;
    assert_eq!(r.status, 503);
    assert_eq!(ws_a.upgrades(), 1);
    let mut cb = open(&w, upgrade(&host_b, &format!("https://{host_b}"))).await;
    assert!(echoes(&mut cb, "b1").await);
    // A withdrawn public approval ends the anonymous WebSockets of the tenant within 5 s.
    w.set_public_approval(&t_a, false).await.unwrap();
    let since = Instant::now();
    let took = closed_after(&mut c, since, Duration::from_secs(6))
        .await
        .expect("the WebSocket stayed open after the approval was withdrawn");
    assert!(
        took <= Duration::from_secs(5) + gateway::POLL_ALLOWANCE,
        "{took:?}"
    );
    assert!(
        echoes(&mut cb, "b2").await,
        "another tenant's WebSocket closed"
    );
}

/// review-g1b 1: on a public app every visitor passes the check, so the recheck must find the
/// visitor of the handshake again. A person who signs out of Ruum (and would now pass as
/// anonymous) and a person whose app role is removed (and would now pass with other roles) lose
/// their WebSockets within 5 s; an anonymous WebSocket on the same app stays open.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn on_a_public_app_a_sign_out_or_a_removed_role_ends_the_websocket() {
    let w = World::new().await.unwrap();
    let ws = WsEcho::start().await;
    let (host, d, t) = public_sandbox(&w, "acme", "garden", &ws.url).await;
    let a = App {
        id: w.query(&d).unwrap().app_id.to_string(),
        slug: "garden".into(),
        tenant: t.clone(),
    };
    let own = format!("https://{host}");
    let leaving = w.seed_user(&t, "leaving").await.unwrap();
    let demoted = w.seed_user(&t, "demoted").await.unwrap();
    let member = Relation::Role("member".into());
    w.grant(&demoted, &a, &member).await.unwrap();
    let leaving_cookie = session(&w, &leaving, &host).await;
    let demoted_cookie = session(&w, &demoted, &host).await;
    let mut lc = open(&w, upgrade(&host, &own).header("Cookie", &leaving_cookie)).await;
    let mut dc = open(&w, upgrade(&host, &own).header("Cookie", &demoted_cookie)).await;
    let mut anon = open(&w, upgrade(&host, &own)).await;
    assert!(echoes(&mut lc, "l1").await);
    assert!(echoes(&mut dc, "d1").await);
    assert!(echoes(&mut anon, "a1").await);
    // The handshakes carried the person and the role (the app acts on them).
    let tokens: Vec<String> = ws
        .seen()
        .iter()
        .map(|s| s.header_all("ruum-user").concat())
        .collect();
    assert_eq!(tokens.len(), 3);
    w.sign_out_platform(&leaving).await.unwrap();
    w.revoke(&demoted, &a, &member).await.unwrap();
    let since = Instant::now();
    for (label, c) in [("signed out", &mut lc), ("role removed", &mut dc)] {
        let took = closed_after(c, since, Duration::from_secs(6))
            .await
            .unwrap_or_else(|| panic!("the WebSocket stayed open ({label})"));
        assert!(
            took <= Duration::from_secs(5) + gateway::POLL_ALLOWANCE,
            "{label}: {took:?}"
        );
    }
    // Positive control: the anonymous WebSocket of the same app is open over the same time.
    while since.elapsed() < Duration::from_secs(6) {
        assert!(
            echoes(&mut anon, "a2").await,
            "the anonymous WebSocket closed"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// The handshake of a WebSocket on a raw connection: the `101` head is read, nothing more.
async fn raw_upgrade(w: &World, host: &str) -> TcpStream {
    let mut s = TcpStream::connect(w.gateway).await.unwrap();
    s.write_all(
        format!(
            "GET /ws HTTP/1.1\r\nHost: {host}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
             Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Origin: https://{host}\r\n\r\n"
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut b = [0u8; 1];
        let n = tokio::time::timeout(T, s.read(&mut b))
            .await
            .unwrap()
            .unwrap();
        assert!(n > 0, "the gateway closed the handshake");
        head.push(b[0]);
    }
    assert_eq!(
        status_of(&head),
        Some(101),
        "{}",
        String::from_utf8_lossy(&head)
    );
    s
}

/// review-g1b 2: a visitor that sends and never reads fills the buffers both ways (the echo app
/// stops reading once it cannot write back), so a copy of the gateway waits on a write that
/// never ends. The recheck still runs: a withdrawn approval closes the connection and frees its
/// WebSocket slot within 5 s. Positive control: a reading visitor of another tenant keeps its
/// WebSocket over the same time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_visitor_that_stops_reading_cannot_outlive_the_recheck() {
    let w = World::new().await.unwrap();
    let ws_a = WsEcho::start().await;
    let ws_b = WsEcho::start().await;
    let (host_a, _, t_a) = public_sandbox(&w, "acme", "garden", &ws_a.url).await;
    let (host_b, _, _) = public_sandbox(&w, "beta", "orchard", &ws_b.url).await;
    let mut reader = open(&w, upgrade(&host_b, &format!("https://{host_b}"))).await;
    assert!(echoes(&mut reader, "b1").await);
    let s = raw_upgrade(&w, &host_a).await;
    assert_eq!(w.gw.sandboxes.open_sockets(), 2);
    let (rd, mut wr) = s.into_split();
    // Masked binary frames (mask 0) of 60000 bytes, sent until the connection blocks or ends.
    let written = Arc::new(AtomicUsize::new(0));
    let writer = {
        let written = written.clone();
        tokio::spawn(async move {
            let mut frame = vec![0x82, 0x80 | 126];
            frame.extend_from_slice(&60000u16.to_be_bytes());
            frame.extend_from_slice(&[0, 0, 0, 0]);
            frame.extend(std::iter::repeat_n(b'x', 60000));
            while wr.write_all(&frame).await.is_ok() {
                written.fetch_add(frame.len(), Ordering::SeqCst);
            }
        })
    };
    // Wait until the writes stall (no progress for 1 s).
    let mut seen = 0;
    let stall = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let now = written.load(Ordering::SeqCst);
        if now == seen && now > 0 {
            break;
        }
        seen = now;
        assert!(stall.elapsed() < T, "the writes never stalled");
    }
    assert_eq!(
        w.gw.sandboxes.open_sockets(),
        2,
        "the stalled WebSocket is open"
    );
    w.set_public_approval(&t_a, false).await.unwrap();
    let since = Instant::now();
    while w.gw.sandboxes.open_sockets() > 1 {
        assert!(
            since.elapsed() <= Duration::from_secs(5) + gateway::POLL_ALLOWANCE,
            "the stalled WebSocket kept its slot after the approval was withdrawn"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Positive control: the reading WebSocket is open over the same time.
    while since.elapsed() < Duration::from_secs(6) {
        assert!(
            echoes(&mut reader, "b2").await,
            "the reading WebSocket closed"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert_eq!(w.gw.sandboxes.open_sockets(), 1);
    writer.abort();
    drop(rd);
}

/// review-g1b 3: the shares hold per tenant too, over all its apps and deployments. Tenant A
/// with two apps reaches its WebSocket share; a third app of A gets `503`, while tenant B still
/// opens one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_tenant_cannot_take_the_websockets_of_another() {
    let w = World::with(Opts {
        hold: HoldConfig {
            ws_per_app: 2,
            ws_per_tenant: 3,
            ..HoldConfig::default()
        },
        ..Opts::default()
    })
    .await
    .unwrap();
    let echo = WsEcho::start().await;
    let (host_a1, _, t_a) = public_sandbox(&w, "acme", "garden", &echo.url).await;
    let a2 = w.seed_app(&t_a, "orchard", Access::Public).await.unwrap();
    let d2 = w
        .seed_node_deployment(&a2, Env::Live, &echo.url)
        .await
        .unwrap();
    w.set_live(&a2, &d2).await.unwrap();
    let host_a2 = format!("orchard.acme.{APPS_DOMAIN}");
    let a3 = w.seed_app(&t_a, "meadow", Access::Public).await.unwrap();
    let d3 = w
        .seed_node_deployment(&a3, Env::Live, &echo.url)
        .await
        .unwrap();
    w.set_live(&a3, &d3).await.unwrap();
    let host_a3 = format!("meadow.acme.{APPS_DOMAIN}");
    let (host_b, _, _) = public_sandbox(&w, "beta", "grove", &echo.url).await;
    let own = |h: &str| format!("https://{h}");
    let mut kept = Vec::new();
    for h in [&host_a1, &host_a1, &host_a2] {
        let mut c = open(&w, upgrade(h, &own(h))).await;
        assert!(echoes(&mut c, "a").await);
        kept.push(c);
    }
    // App A1 is at its share of 2 (the per-app share counts per app).
    let r = send(&w, upgrade(&host_a1, &own(&host_a1))).await;
    assert_eq!(r.status, 503);
    // App A3 has no socket, but tenant A is at its share of 3.
    let upgrades = echo.upgrades();
    let r = send(&w, upgrade(&host_a3, &own(&host_a3))).await;
    assert_eq!(r.status, 503);
    assert_eq!(
        echo.upgrades(),
        upgrades,
        "the app was asked over the share"
    );
    // Tenant B still opens one (positive control).
    let mut cb = open(&w, upgrade(&host_b, &own(&host_b))).await;
    assert!(echoes(&mut cb, "b").await);
    assert_eq!(w.gw.sandboxes.open_sockets(), 4);
    // A freed socket of tenant A makes room for app A3.
    drop(kept.pop());
    let since = Instant::now();
    while w.gw.sandboxes.open_sockets() > 3 {
        assert!(
            since.elapsed() < Duration::from_secs(5),
            "the slot was not freed"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut c3 = open(&w, upgrade(&host_a3, &own(&host_a3))).await;
    assert!(echoes(&mut c3, "a3").await);
}

/// review-g1b 3: the hold share of a tenant holds over its deployments: two cold deployments of
/// tenant A fill its share, a third gets `503` at once, while tenant B's cold request is held
/// and answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_tenants_cold_starts_cannot_take_the_hold_of_another() {
    let w = World::with(Opts {
        hold: HoldConfig {
            hold: Duration::from_secs(8),
            per_app: 2,
            per_tenant: 2,
            ..HoldConfig::default()
        },
        ..Opts::default()
    })
    .await
    .unwrap();
    let echo = Echo::start().await;
    let (host_a1, da1, t_a) = public_sandbox(&w, "acme", "garden", &echo.url).await;
    let a2 = w.seed_app(&t_a, "orchard", Access::Public).await.unwrap();
    let da2 = w
        .seed_node_deployment(&a2, Env::Live, &echo.url)
        .await
        .unwrap();
    w.set_live(&a2, &da2).await.unwrap();
    let host_a2 = format!("orchard.acme.{APPS_DOMAIN}");
    let a3 = w.seed_app(&t_a, "meadow", Access::Public).await.unwrap();
    let da3 = w
        .seed_node_deployment(&a3, Env::Live, &echo.url)
        .await
        .unwrap();
    w.set_live(&a3, &da3).await.unwrap();
    let host_a3 = format!("meadow.acme.{APPS_DOMAIN}");
    let (host_b, db, _) = public_sandbox(&w, "beta", "grove", &echo.url).await;
    for d in [&da1, &da2, &da3, &db] {
        w.stop_instance(d).await.unwrap();
    }
    w.driver.set_delay(Duration::from_millis(1500));
    let addr = w.gateway;
    let held: Vec<_> = [&host_a1, &host_a2]
        .into_iter()
        .map(|h| {
            let req = Req::get(h, "/held");
            tokio::spawn(async move { gateway::http::send(addr, &req, T).await.unwrap() })
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let b = {
        let req = Req::get(&host_b, "/b");
        tokio::spawn(async move { gateway::http::send(addr, &req, T).await.unwrap() })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    // A third deployment of tenant A is within its app's share, but over the tenant's: 503 at
    // once.
    let t = Instant::now();
    let over = send(&w, Req::get(&host_a3, "/over")).await;
    assert_eq!(over.status, 503);
    assert!(over.header("retry-after").is_some());
    assert!(
        t.elapsed() < Duration::from_millis(500),
        "{:?}",
        t.elapsed()
    );
    // Tenant B's held request is answered, and so are tenant A's two.
    let rb = b.await.unwrap();
    assert!(gateway::is_echo(&rb), "{} {}", rb.status, rb.text());
    for h in held {
        let r = h.await.unwrap();
        assert!(gateway::is_echo(&r), "{} {}", r.status, r.text());
    }
    assert_eq!(w.gw.sandboxes.held(), 0);
}
