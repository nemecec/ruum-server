//! The CLI against a real Ruum API server (the in-memory services on a local port): login with
//! an agent API key and token exchange, whoami, agents, changes, errors. The file commands run
//! end to end when the nodes area is built; until then the test reports them as pending.

use ruum_api::AppState;
use ruum_core::actor::{Actor, Context};
use ruum_core::agents::{AgentScope, Budget, NewAgent};
use ruum_core::authz::Role;
use ruum_core::identity::VerifiedLogin;

struct Server {
    url: String,
    key: String,
    agent_id: String,
    email: String,
    dir: tempfile::TempDir,
}

async fn server() -> Server {
    let state = AppState::in_memory();
    let s = &state.services;
    let email = format!("mari-{}@example.test", uuid::Uuid::now_v7());
    let user = s
        .identity
        .upsert_from_login(&VerifiedLogin {
            issuer: "https://idp.ruum.test".into(),
            subject: format!("mari-{}", uuid::Uuid::now_v7()),
            email: Some(email.clone()),
            email_verified: true,
            name: Some("Mari".into()),
            id_token: None,
        })
        .await
        .unwrap();
    let ctx = Context {
        tenant: user.home_tenant_id,
        actor: Actor::user(user.id),
        scope: None,
        idempotency_key: None,
    };
    let agent = s
        .agents
        .register(
            &ctx,
            NewAgent {
                name: "CLI bot".into(),
                description: None,
                scope: AgentScope {
                    subtree: None,
                    max_role: Role::Edit,
                    expires_at: None,
                    budget: Budget::default(),
                },
            },
        )
        .await
        .unwrap();
    let (_k, key) = s
        .agents
        .create_key(&ctx, agent.id, Some("cli".into()), None)
        .await
        .unwrap();
    let app = ruum_api::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Server {
        url: format!("http://{addr}"),
        key,
        agent_id: agent.id.0.to_string(),
        email,
        dir: tempfile::tempdir().unwrap(),
    }
}

impl Server {
    fn creds(&self) -> String {
        self.dir
            .path()
            .join("credentials.json")
            .display()
            .to_string()
    }

    /// Runs `ruum <args>` with this server's credentials file; returns the output.
    async fn ruum(&self, args: &[&str]) -> anyhow::Result<String> {
        let creds = self.creds();
        let mut all = vec!["--credentials", creds.as_str(), "--url", self.url.as_str()];
        all.extend_from_slice(args);
        let mut out = Vec::new();
        ruum_cli::run_args(all, &mut out).await?;
        Ok(String::from_utf8(out).unwrap())
    }
}

#[tokio::test]
async fn login_whoami_agents_and_changes() {
    let srv = server().await;

    let err = srv.ruum(&["whoami"]).await.unwrap_err();
    assert!(ruum_cli::report(&err).contains("not signed in"), "{err:#}");

    let out = srv.ruum(&["login", "--key", &srv.key]).await.unwrap();
    assert!(out.contains("CLI bot"), "{out}");
    assert!(out.contains(&srv.email), "{out}");

    // The key and a token are saved, readable only by the user.
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(srv.creds()).unwrap()).unwrap();
    assert_eq!(saved["api_key"], srv.key.as_str());
    assert!(saved["access_token"].is_string());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(srv.creds()).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "credentials must be private: {mode:o}");
    }

    let out = srv.ruum(&["whoami"]).await.unwrap();
    assert!(out.contains(&srv.agent_id), "{out}");
    let out = srv.ruum(&["--json", "whoami"]).await.unwrap();
    let me: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(me["actor"]["kind"], "agent");

    let out = srv.ruum(&["agents"]).await.unwrap();
    assert!(out.contains(&srv.agent_id), "{out}");

    let out = srv.ruum(&["changes"]).await.unwrap();
    assert!(out.contains("next cursor:"), "{out}");

    // A broken saved token is replaced by a new exchange.
    let mut saved = saved;
    saved["access_token"] = "broken".into();
    std::fs::write(srv.creds(), saved.to_string()).unwrap();
    let out = srv.ruum(&["whoami"]).await.unwrap();
    assert!(out.contains(&srv.agent_id), "{out}");

    let out = srv.ruum(&["logout"]).await.unwrap();
    assert!(out.contains("signed out"), "{out}");
    assert!(!std::path::Path::new(&srv.creds()).exists());
}

#[tokio::test]
async fn a_wrong_key_is_refused_with_the_server_error() {
    let srv = server().await;
    let err = srv
        .ruum(&["login", "--key", "ruum_agent_not-a-real-key"])
        .await
        .unwrap_err();
    let text = ruum_cli::report(&err);
    assert!(text.starts_with("error: "), "{text}");
    assert!(!std::path::Path::new(&srv.creds()).exists());

    let err = srv.ruum(&["login", "--key", "hello"]).await.unwrap_err();
    assert!(ruum_cli::report(&err).contains("ruum_agent_"));

    // A key on the command line works without login and is not saved.
    let out = srv
        .ruum(&["--api-key", srv.key.as_str(), "whoami"])
        .await
        .unwrap();
    assert!(out.contains(&srv.agent_id));
    assert!(!std::path::Path::new(&srv.creds()).exists());
}

#[tokio::test]
async fn file_commands_end_to_end() {
    let srv = server().await;
    srv.ruum(&["login", "--key", &srv.key]).await.unwrap();
    // An operation of an area that is not built yet answers `not_implemented`: pending. With
    // RUUM_REQUIRE_IMPLEMENTED=1 (docs/TESTING.md) a pending check fails.
    let require =
        std::env::var_os("RUUM_REQUIRE_IMPLEMENTED").is_some_and(|v| !v.is_empty() && v != "0");
    match file_flow(&srv).await {
        Ok(()) => {}
        Err(e) if !require && is_not_implemented(&e) => {
            eprintln!("pending: {}", ruum_cli::report(&e));
        }
        Err(e) => panic!("{}", ruum_cli::report(&e)),
    }
}

fn is_not_implemented(e: &anyhow::Error) -> bool {
    e.downcast_ref::<ruum_cli::client::ApiError>()
        .is_some_and(|a| a.code == "not_implemented")
}

async fn file_flow(srv: &Server) -> anyhow::Result<()> {
    srv.ruum(&["ls"]).await?;
    let local = srv.dir.path().join("plan.md");
    std::fs::write(&local, b"# Garden plan\n").unwrap();
    let local_s = local.display().to_string();

    srv.ruum(&["mkdir", "-p", "/Projects/Garden"]).await?;
    let out = srv.ruum(&["upload", &local_s, "/Projects/Garden/"]).await?;
    assert!(out.contains("uploaded"), "{out}");
    let out = srv.ruum(&["ls", "/Projects/Garden"]).await?;
    assert!(out.contains("plan.md"), "{out}");

    std::fs::write(&local, b"# Garden plan\n\nv2\n").unwrap();
    srv.ruum(&["upload", &local_s, "/Projects/Garden/plan.md"])
        .await?;
    let out = srv
        .ruum(&["--json", "versions", "/Projects/Garden/plan.md"])
        .await?;
    let versions: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(versions.as_array().unwrap().len(), 2, "{out}");

    let back = srv.dir.path().join("back.md");
    srv.ruum(&[
        "download",
        "/Projects/Garden/plan.md",
        &back.display().to_string(),
    ])
    .await?;
    assert_eq!(std::fs::read(&back).unwrap(), b"# Garden plan\n\nv2\n");

    srv.ruum(&["mv", "/Projects/Garden/plan.md", "/Projects/plan-v2.md"])
        .await?;
    let out = srv.ruum(&["ls", "/Projects"]).await?;
    assert!(out.contains("plan-v2.md"), "{out}");

    srv.ruum(&["rm", "/Projects/plan-v2.md"]).await?;
    let out = srv.ruum(&["ls", "/Projects"]).await?;
    assert!(!out.contains("plan-v2.md"), "{out}");

    let out = srv.ruum(&["changes"]).await?;
    assert!(out.contains("node.created"), "{out}");
    Ok(())
}
