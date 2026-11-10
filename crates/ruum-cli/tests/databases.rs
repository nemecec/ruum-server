//! `ruum db ...` against a real Ruum API server (the in-memory services on a local port), as a
//! person (`--token`) and as an agent (`--api-key`), and the error output against a test server
//! that plays the contract.

use ruum_api::AppState;
use ruum_core::actor::{Actor, Context};
use ruum_core::agents::{AgentScope, Budget, NewAgent};
use ruum_core::authz::Role;
use ruum_core::identity::{VerifiedLogin, issue_user_token};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const RAW_VALUES: &[&str] = &["Rosa canina", "dog rose", "Bellis perennis", "daisy"];

struct Server {
    url: String,
    key: String,
    token: String,
    dir: tempfile::TempDir,
}

async fn server() -> Server {
    let state = AppState::in_memory();
    let s = &state.services;
    let user = s
        .identity
        .upsert_from_login(&VerifiedLogin {
            issuer: "https://idp.ruum.test".into(),
            subject: format!("mari-{}", uuid::Uuid::now_v7()),
            email: Some(format!("mari-{}@example.test", uuid::Uuid::now_v7())),
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
                name: "Garden bot".into(),
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
    let token = issue_user_token(s.tokens.as_ref(), user.id, 600).unwrap();
    let app = ruum_api::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Server {
        url: format!("http://{addr}"),
        key,
        token,
        dir: tempfile::tempdir().unwrap(),
    }
}

impl Server {
    async fn run(&self, auth: [&str; 2], args: &[&str]) -> anyhow::Result<String> {
        let creds = self.dir.path().join("creds.json").display().to_string();
        let mut all = vec![
            "--credentials",
            &creds,
            "--url",
            &self.url,
            auth[0],
            auth[1],
        ];
        all.extend_from_slice(args);
        let mut out = Vec::new();
        ruum_cli::run_args(all, &mut out).await?;
        Ok(String::from_utf8(out).unwrap())
    }

    /// As the person.
    async fn person(&self, args: &[&str]) -> anyhow::Result<String> {
        self.run(["--token", &self.token], args).await
    }

    /// As the agent of the person.
    async fn agent(&self, args: &[&str]) -> anyhow::Result<String> {
        self.run(["--api-key", &self.key], args).await
    }
}

fn ok(r: anyhow::Result<String>) -> String {
    r.unwrap_or_else(|e| panic!("{}", ruum_cli::report(&e)))
}

#[tokio::test]
async fn people_and_agents_work_with_a_database() {
    let srv = server().await;
    ok(srv.person(&["mkdir", "/Projects"]).await);
    let out = ok(srv
        .person(&["db", "create", "/Projects", "garden", "--engine", "sqlite"])
        .await);
    assert!(out.contains("created /Projects/garden"), "{out}");

    // Wait until it is ready.
    let mut ready = false;
    for _ in 0..200 {
        let out = ok(srv.person(&["db", "show", "/Projects/garden"]).await);
        if out.contains("status: ready") {
            assert!(out.contains("engine: sqlite 3"), "{out}");
            assert!(out.contains("limits: statement timeout"), "{out}");
            ready = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(ready, "the database is not ready");

    // A person makes the table (a schema change runs through queryDatabase) and sees values.
    ok(srv
        .person(&[
            "db",
            "query",
            "/Projects/garden",
            "create table plants (id integer primary key, latin_name text not null, common_name text)",
            "--write",
        ])
        .await);
    let out = ok(srv
        .person(&[
            "db",
            "query",
            "/Projects/garden",
            "insert into plants (id, latin_name, common_name) values (?1, ?2, ?3), (2, 'Bellis perennis', 'daisy')",
            "--write",
            "--param",
            "1",
            "--param",
            "Rosa canina",
            "--param",
            "dog rose",
        ])
        .await);
    assert!(out.contains("2 affected"), "{out}");
    let out = ok(srv
        .person(&[
            "db",
            "query",
            "/Projects/garden",
            "select latin_name, common_name from plants order by id",
        ])
        .await);
    assert!(out.contains("Rosa canina"), "{out}");
    assert!(out.contains("2 rows"), "{out}");

    // The agent sees markers, never the values.
    let out = ok(srv
        .agent(&[
            "db",
            "query",
            "/Projects/garden",
            "select id, latin_name, common_name from plants",
        ])
        .await);
    assert!(
        out.contains("<pseudonym ") || out.contains("<hidden "),
        "{out}"
    );
    for raw in RAW_VALUES {
        assert!(!out.contains(raw), "{raw} reached the agent: {out}");
    }
    let json_out = ok(srv
        .agent(&[
            "--json",
            "db",
            "query",
            "/Projects/garden",
            "select latin_name from plants",
        ])
        .await);
    let v: Value = serde_json::from_str(&json_out).unwrap();
    assert!(v["rows"][0][0]["$ruum"].is_string(), "{json_out}");

    // The schema, the columns, the query log, the manifest.
    let out = ok(srv.agent(&["db", "schema", "/Projects/garden"]).await);
    assert!(
        out.contains("plants") && out.contains("latin_name"),
        "{out}"
    );
    let out = ok(srv.agent(&["db", "columns", "/Projects/garden"]).await);
    assert!(out.contains("plants.latin_name"), "{out}");
    let out = ok(srv.person(&["db", "queries", "/Projects/garden"]).await);
    assert!(out.contains("select"), "{out}");
    let out = ok(srv.person(&["db", "manifest", "/Projects/garden"]).await);
    let m: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(m["engine"], "sqlite", "{out}");

    // The agent suggests, the person confirms.
    let out = ok(srv
        .agent(&[
            "db",
            "suggest",
            "/Projects/garden",
            "plants.latin_name=normal",
            "--reason",
            "plant names",
        ])
        .await);
    assert!(out.contains("suggested 1 column class"), "{out}");
    let err = srv
        .agent(&[
            "db",
            "confirm",
            "/Projects/garden",
            "plants.latin_name=normal",
        ])
        .await
        .unwrap_err();
    assert!(
        ruum_cli::report(&err).contains("user_required"),
        "{}",
        ruum_cli::report(&err)
    );
    let out = ok(srv
        .person(&[
            "db",
            "confirm",
            "/Projects/garden",
            "plants.latin_name=normal",
            "plants.common_name=normal",
        ])
        .await);
    assert!(out.contains("confirmed 2 column classes"), "{out}");

    // Without a grant the agent cannot write; the person gives one and ends it.
    let err = srv
        .agent(&[
            "db",
            "query",
            "/Projects/garden",
            "delete from plants where id = 2",
            "--write",
        ])
        .await
        .unwrap_err();
    let text = ruum_cli::report(&err);
    assert!(
        text.contains("write_grant_required") && text.contains("\nhint: "),
        "{text}"
    );
    let err = srv
        .agent(&[
            "db",
            "grant",
            "/Projects/garden",
            "--agent",
            "Garden bot",
            "--kind",
            "read-personal",
            "--minutes",
            "30",
        ])
        .await
        .unwrap_err();
    assert!(
        ruum_cli::report(&err).contains("user_required"),
        "{}",
        ruum_cli::report(&err)
    );
    let out = ok(srv
        .person(&[
            "db",
            "grant",
            "/Projects/garden",
            "--agent",
            "Garden bot",
            "--kind",
            "read-personal",
            "--minutes",
            "30",
        ])
        .await);
    assert!(out.contains("granted read_personal"), "{out}");
    let grants: Value = serde_json::from_str(&ok(srv
        .person(&["--json", "db", "grants", "/Projects/garden"])
        .await))
    .unwrap();
    let grant_id = grants[0]["id"].as_str().expect("a grant").to_string();
    let out = ok(srv
        .person(&["db", "grants", "/Projects/garden", "--active"])
        .await);
    assert!(out.contains(&grant_id), "{out}");
    let out = ok(srv
        .person(&["db", "revoke", "/Projects/garden", &grant_id])
        .await);
    assert!(out.contains("revoked"), "{out}");
    let out = ok(srv
        .person(&["db", "grants", "/Projects/garden", "--active"])
        .await);
    assert!(!out.contains(&grant_id), "{out}");

    // A folder is not a database.
    let err = srv.person(&["db", "show", "/Projects"]).await.unwrap_err();
    assert!(ruum_cli::report(&err).contains("not a database"));
}

#[tokio::test]
async fn not_ready_prints_the_hint_and_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "tok", "token_type": "Bearer", "expires_in": 900})))
        .mount(&server)
        .await;
    let db = "00000000-0000-7000-8000-000000000009";
    Mock::given(method("GET"))
        .and(path("/v1/nodes/by-path/garden"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": db, "name": "garden", "kind": "typed", "type": "database"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/v1/databases/{db}/query")))
        .respond_with(
            ResponseTemplate::new(503)
                .insert_header("Retry-After", "5")
                .set_body_json(json!({"error": {
                    "code": "database_not_ready",
                    "message": "the database is not ready for queries right now; nothing ran",
                    "hint": "retry"}})),
        )
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let creds = dir.path().join("creds.json").display().to_string();
    let url = server.uri();
    let mut out = Vec::new();
    let err = ruum_cli::run_args(
        [
            "--credentials",
            &creds,
            "--url",
            &url,
            "--api-key",
            "ruum_agent_testkey",
            "db",
            "query",
            "/garden",
            "select 1",
        ],
        &mut out,
    )
    .await
    .unwrap_err();
    assert_eq!(
        ruum_cli::report(&err),
        "error: database_not_ready: the database is not ready for queries right now; nothing ran\nhint: retry\nretry after: 5 s"
    );
}

#[tokio::test]
async fn there_are_no_commands_for_planned_operations() {
    for sub in [
        "migrations",
        "migrate",
        "branch",
        "branches",
        "restore",
        "approvals",
    ] {
        let mut out = Vec::new();
        let r = ruum_cli::run_args(["db", sub, "/garden"], &mut out).await;
        assert!(r.is_err(), "ruum db {sub} exists");
    }
}
