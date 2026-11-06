//! Serves the WebDAV host (at `/`) on the fixture world of `ruum-testkit` with the in-memory stores, for manual
//! checks with curl, `mount_webdav` or Finder without the laptop stack.
//!
//! ```sh
//! cargo run -p ruum-webdav --example dav_fake_server -- 127.0.0.1:8497 /tmp/dav-creds.env
//! ```
//!
//! It writes the credentials of tenant A's member (email and app password) and of the agent
//! with scope `Private` (access token) to the given file, as shell variables.

#[path = "../tests/support/mod.rs"]
mod support;

use ruum_testkit::fixtures::{Credentials, Who, seed};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let bind = args.next().unwrap_or_else(|| "127.0.0.1:8497".into());
    let creds = args.next().expect("a file for the credentials");
    let h = support::harness();
    let w = seed(&support::app_state(&h.services)).await;
    assert!(w.seeded, "the fixture world must be seeded");
    let a = &w.a;
    let agent = match &a.principal(Who::AgentOut).credentials {
        Credentials::Bearer(t) => t.clone(),
        _ => String::new(),
    };
    std::fs::write(
        &creds,
        format!(
            "DAV_USER='{}'\nDAV_PASS='{}'\nDAV_AGENT='{}'\nDAV_PLAN='{}'\n",
            a.principal(Who::Member).email.clone().unwrap_or_default(),
            a.app_password_secret,
            agent,
            a.plan_name,
        ),
    )
    .expect("write the credentials");
    let listener = tokio::net::TcpListener::bind(&bind).await.expect("bind");
    eprintln!("dav_fake_server: http://{bind}/ (credentials in {creds})");
    axum::serve(listener, ruum_webdav::router(h.services))
        .await
        .expect("serve");
}
