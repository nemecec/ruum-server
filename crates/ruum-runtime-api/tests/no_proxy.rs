//! The runtime API's clients of the core never go through a proxy of the environment (Kimi
//! review of run 14, R-3 finding 1): every call carries the app's token, and the answers carry
//! delegated tokens and bound content.
//!
//! The workspace forbids `unsafe`, so a test cannot set `HTTP_PROXY` in its own process. The
//! test runs this binary again as a child with the proxy settings. The child checks that its
//! call reached the core directly; the parent checks that the proxy got no connection.

use ruum_core::apps::runtime_api::wire::Caller;
use ruum_core::identity::app_sign_in::AppEnv;
use ruum_core::{NodeId, TenantId};
use ruum_runtime_api::core_api::{CoreApi, HttpCore};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

const CHILD: &str = "child_core_calls_reach_the_core_directly";

#[test]
fn core_calls_never_use_a_proxy_of_the_environment() {
    let proxy = std::net::TcpListener::bind("127.0.0.1:0").expect("bind the proxy");
    proxy.set_nonblocking(true).expect("non-blocking");
    let addr = format!("http://{}", proxy.local_addr().expect("addr"));
    let out = std::process::Command::new(std::env::current_exe().expect("this test binary"))
        .args([
            CHILD,
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("HTTP_PROXY", &addr)
        .env("http_proxy", &addr)
        .env("ALL_PROXY", &addr)
        .env("all_proxy", &addr)
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .output()
        .expect("run the child");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    assert!(text.contains("1 passed"), "the child did not run: {text}");
    match proxy.accept() {
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
        Ok(_) => panic!("the proxy of the environment got a connection"),
        Err(e) => panic!("the proxy listener failed: {e}"),
    }
}

#[tokio::test]
#[ignore = "the child of core_calls_never_use_a_proxy_of_the_environment (with proxy settings)"]
async fn child_core_calls_reach_the_core_directly() {
    let core = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the core");
    let base = format!("http://{}", core.local_addr().expect("addr"));
    let server = tokio::spawn(async move {
        let (mut s, _) = core.accept().await.expect("accept");
        let mut buf = vec![0u8; 16 * 1024];
        let n = s.read(&mut buf).await.expect("read");
        let _ = s
            .write_all(
                b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )
            .await;
        String::from_utf8_lossy(&buf[..n]).to_string()
    });
    let c = HttpCore::new(&base).expect("the client");
    let caller = Caller {
        tenant_id: TenantId(Uuid::now_v7()),
        app_id: NodeId(Uuid::now_v7()),
        principal: Uuid::now_v7(),
        deployment_id: Uuid::now_v7(),
        environment: AppEnv::Live,
    };
    // The fake core answers 500: the call fails, but it must have reached the core.
    let _ = c.principal("a-workload-token", &caller).await;
    let request = tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("the core got no request (it went to the proxy)")
        .expect("the fake core");
    assert!(
        request.starts_with("POST /internal/runtime/v1/principal "),
        "{request}"
    );
}
