//! The database tools over MCP (card M-1), against the in-memory services (the pattern of
//! `mcp_server.rs`):
//!
//! - the tool list follows the planned flag of the contract (decision 0004-22): a database tool
//!   is offered exactly when its operation is no longer `x-ruum-status: planned`. The test reads
//!   the flags from `contracts/openapi.yaml`, so it holds when later cards build operations;
//! - a smoke test as an agent: create a database, wait until it is ready, read the schema and
//!   query the table `plants` (the fixture table of wave 4 G-1, made by a person through
//!   `queryDatabase`). Every value the agent gets is a `$ruum` marker, and no raw value of the
//!   table is anywhere in the tool result. The test does not say which marker (pseudonym or
//!   hidden): card Q-2 decides that;
//! - an agent's write without a grant answers `write_grant_required` with its hint in the text;
//! - `database_not_ready` keeps its `retry` hint (and the seconds of `Retry-After`).
//!
//! The smoke test runs on the memory engine (SQLite manifest), and on the Postgres engine when
//! `RUUM_DBHOST_URL` is set (required in CI and with `RUUM_DBENGINE_REQUIRE_POSTGRES`).

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use ruum_api::{ApiConfig, AppState};
use ruum_core::actor::{Actor, Context};
use ruum_core::agents::{AgentScope, Budget, NewAgent};
use ruum_core::authz::Role;
use ruum_core::databases::engine::DatabaseEngine;
use ruum_core::databases::{self, DatabaseParts, DatabaseService, Engines};
use ruum_core::identity::{VerifiedLogin, issue_user_token};
use ruum_core::providers::{ProviderState, ProviderStatus};
use ruum_core::services::MemoryHandles;
use ruum_core::{NodeId, Services, TenantId};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt as _;

const OPENAPI: &str = include_str!("../../../contracts/openapi.yaml");
const METHODS: &[&str] = &["get", "put", "post", "delete", "patch", "head", "options"];

/// The raw values of the fixture table. None may reach an agent.
const RAW_VALUES: &[&str] = &["Rosa canina", "dog rose", "Bellis perennis", "daisy"];

// ------------------------------------------------------------------------------ the contract

/// One operation of `openapi.yaml`: its tag and whether it is planned.
#[derive(Debug)]
struct Operation {
    tags: String,
    planned: bool,
}

/// The operations of the contract by `operationId` (a line scanner, like `tools_contract.rs`).
fn operations() -> BTreeMap<String, Operation> {
    let mut ops = BTreeMap::new();
    let mut in_paths = false;
    let mut tags = String::new();
    let mut current: Option<String> = None;
    for line in OPENAPI.lines() {
        if line == "paths:" {
            in_paths = true;
            continue;
        }
        if in_paths && !line.is_empty() && !line.starts_with(' ') && !line.starts_with('#') {
            in_paths = false;
        }
        if !in_paths {
            continue;
        }
        if line.starts_with("  /") {
            current = None;
            tags.clear();
        } else if let Some(m) = line.strip_prefix("    ").and_then(|l| l.strip_suffix(':')) {
            if METHODS.contains(&m) {
                current = None;
                tags.clear();
            }
        } else if let Some(t) = line.strip_prefix("      tags: ") {
            tags = t.trim().to_string();
        } else if let Some(id) = line.strip_prefix("      operationId: ") {
            let id = id.trim().to_string();
            ops.insert(
                id.clone(),
                Operation {
                    tags: tags.clone(),
                    planned: false,
                },
            );
            current = Some(id);
        } else if line == "      x-ruum-status: planned" {
            let id = current
                .as_ref()
                .expect("x-ruum-status after the operationId");
            ops.get_mut(id).unwrap().planned = true;
        }
    }
    assert_eq!(
        ops.len(),
        OPENAPI.matches("operationId: ").count(),
        "the scanner missed operations"
    );
    ops
}

/// The operations of the tag `databases`.
fn database_operations() -> BTreeMap<String, Operation> {
    let ops: BTreeMap<String, Operation> = operations()
        .into_iter()
        .filter(|(_, o)| o.tags == "[databases]")
        .collect();
    assert!(
        ops.contains_key("createDatabase") && ops.contains_key("queryDatabase"),
        "the scanner found no database operations: {ops:?}"
    );
    ops
}

// ------------------------------------------------------------------------------ the world

/// A person with a personal tenant, a folder `Garden`, and an agent of that person with `edit`
/// on the folder.
struct World {
    app: Router,
    handles: MemoryHandles,
    tenant: TenantId,
    folder: String,
    agent_token: String,
    user_token: String,
}

/// The Postgres engine of `RUUM_DBHOST_URL`, or `None` (the test is skipped) when it is not set
/// and not required.
async fn postgres_engine() -> Option<Arc<dyn DatabaseEngine>> {
    use ruum_core::databases::engine::postgres::{PostgresConfig, PostgresEngine};
    let required = std::env::var_os("CI").is_some()
        || std::env::var_os("RUUM_DBENGINE_REQUIRE_POSTGRES").is_some();
    let Some(config) = PostgresConfig::from_env() else {
        assert!(
            !required,
            "RUUM_DBHOST_URL is not set: this test needs the data cluster"
        );
        eprintln!("skipped: RUUM_DBHOST_URL is not set");
        return None;
    };
    match PostgresEngine::connect(config).await {
        Ok(e) => Some(Arc::new(e)),
        Err(e) => {
            assert!(!required, "the data cluster: {e}");
            eprintln!("skipped: the data cluster does not answer: {e}");
            None
        }
    }
}

async fn world(postgres: Option<Arc<dyn DatabaseEngine>>) -> World {
    let (mut services, handles) = Services::in_memory_with_handles();
    if let Some(pg) = postgres {
        let d = services.databases.deps().clone();
        let keys = services.tenant_keys.clone();
        databases::install(
            &mut services,
            DatabaseParts {
                engines: Engines {
                    postgres: Some(pg),
                    sqlite: Engines::memory().sqlite,
                },
                keys,
                store: d.store.clone(),
                typed: d.typed.clone(),
                columns: d.columns.clone(),
                masking: d.masking.clone(),
                grants: d.grants.clone(),
                queries: d.queries.log.clone(),
            },
        )
        .expect("install the Postgres engine");
    }
    // A query of a database that is not ready waits 10 s by default; the test needs less.
    let mut deps = services.databases.deps().clone();
    deps.queries.not_ready_wait = Duration::from_millis(300);
    services.databases = Arc::new(DatabaseService::new(deps));

    let state = AppState::new(services, ApiConfig::default());
    let s = &state.services;
    let u = s
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
        .expect("user");
    let ctx = Context {
        tenant: u.home_tenant_id,
        actor: Actor::user(u.id),
        scope: None,
        idempotency_key: None,
    };
    let user_token = issue_user_token(s.tokens.as_ref(), u.id, 600).expect("user token");
    let app = ruum_mcp::router(state.clone());
    let tenant = u.home_tenant_id.0.to_string();
    let user = |name: &'static str, args: Value| {
        let (app, token, tenant) = (app.clone(), user_token.clone(), tenant.clone());
        async move {
            call_with(
                &app,
                &token,
                &[("ruum-tenant", tenant.as_str())],
                name,
                args,
            )
            .await
        }
    };
    let root = user("get_root", json!({})).await;
    assert_eq!(root["isError"], json!(false), "{root}");
    let folder = user(
        "create_node",
        json!({"body": {"parent_id": root["structuredContent"]["id"], "name": "Garden",
                        "kind": "folder"}}),
    )
    .await;
    assert_eq!(folder["isError"], json!(false), "{folder}");
    let folder = folder["structuredContent"]["node"]["id"]
        .as_str()
        .expect("folder id")
        .to_string();
    let agent = s
        .agents
        .register(
            &ctx,
            NewAgent {
                name: "Garden helper".into(),
                description: None,
                scope: AgentScope {
                    subtree: Some(NodeId(folder.parse().unwrap())),
                    max_role: Role::Edit,
                    expires_at: None,
                    budget: Budget::default(),
                },
            },
        )
        .await
        .expect("agent");
    let (_key, secret) = s
        .agents
        .create_key(&ctx, agent.id, Some("test".into()), None)
        .await
        .expect("key");
    let token = s.agents.exchange_key(&secret).await.expect("exchange");
    World {
        app,
        handles,
        tenant: u.home_tenant_id,
        folder,
        agent_token: token.access_token,
        user_token,
    }
}

impl World {
    async fn agent(&self, name: &str, args: Value) -> Value {
        call_with(&self.app, &self.agent_token, &[], name, args).await
    }

    async fn person(&self, name: &str, args: Value) -> Value {
        let tenant = self.tenant.0.to_string();
        call_with(
            &self.app,
            &self.user_token,
            &[("ruum-tenant", tenant.as_str())],
            name,
            args,
        )
        .await
    }

    /// The agent makes a database in its folder and waits until it is ready; then the person
    /// makes the table `plants` (wave 4 G-1) through `queryDatabase`.
    async fn garden(&self, engine: &str) -> String {
        let version = if engine == "postgres" { "17" } else { "3" };
        let r = self
            .agent(
                "create_database",
                json!({"body": {"parent_id": self.folder, "name": "garden",
                    "manifest": {"engine": engine, "version": version, "plan": "shared-small"}}}),
            )
            .await;
        assert_eq!(r["isError"], json!(false), "{r}");
        let id = r["structuredContent"]["database"]["node"]["id"]
            .as_str()
            .expect("database id")
            .to_string();
        let mut state = String::new();
        for _ in 0..600 {
            let r = self.agent("get_database", json!({"node_id": id})).await;
            assert_eq!(r["isError"], json!(false), "{r}");
            state = r["structuredContent"]["status"]["state"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if state == "ready" {
                break;
            }
            assert!(matches!(state.as_str(), "provisioning" | "updating"), "{r}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(state, "ready", "the database is not ready after 30 s");
        for sql in [
            "create table plants (id integer primary key, latin_name text not null, \
             common_name text)",
            "insert into plants (id, latin_name, common_name) values \
             (1, 'Rosa canina', 'dog rose'), (2, 'Bellis perennis', 'daisy')",
        ] {
            let r = self
                .person(
                    "query_database",
                    json!({"node_id": id, "body": {"sql": sql, "mode": "write"}}),
                )
                .await;
            assert_eq!(r["isError"], json!(false), "{sql}: {r}");
        }
        id
    }
}

// ------------------------------------------------------------------------------ MCP calls

async fn call_with(
    app: &Router,
    token: &str,
    extra: &[(&str, &str)],
    name: &str,
    args: Value,
) -> Value {
    let (status, v) = rpc(
        app,
        token,
        extra,
        json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
               "params": {"name": name, "arguments": args}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v["result"].clone()
}

async fn rpc(
    app: &Router,
    token: &str,
    extra: &[(&str, &str)],
    body: Value,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::HOST, "localhost")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    let res = app
        .clone()
        .oneshot(b.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    (status, parse_body(&text))
}

/// The JSON body, or the last `data:` line of an SSE answer.
fn parse_body(text: &str) -> Value {
    if let Ok(v) = serde_json::from_str(text) {
        return v;
    }
    text.lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
        .next_back()
        .unwrap_or(Value::Null)
}

fn error_code(result: &Value) -> &str {
    assert_eq!(
        result["isError"],
        json!(true),
        "expected a tool error: {result}"
    );
    result["structuredContent"]["error"]["code"]
        .as_str()
        .unwrap_or_default()
}

/// The text blocks of a tool result, joined.
fn text_of(result: &Value) -> String {
    result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// No raw value of the fixture table is anywhere in the tool result (text and structured
/// content, also inside JSON escapes).
fn assert_no_raw_value(what: &str, result: &Value) {
    let all = result.to_string();
    for raw in RAW_VALUES {
        assert!(
            !all.contains(raw),
            "{what}: the raw value {raw:?} reached the agent: {result}"
        );
    }
}

// ------------------------------------------------------------------------------ the tests

/// Every database tool in `tools/list` belongs to an operation that is not planned, and every
/// database operation that is not planned and has a tool is listed. Derived from the contract,
/// with no counts, so cards that build more operations keep it true.
#[tokio::test]
async fn a_database_tool_is_listed_exactly_when_its_operation_is_built() {
    let w = world(None).await;
    let (status, v) = rpc(
        &w.app,
        &w.agent_token,
        &[],
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let listed: BTreeSet<String> = v["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    let ops = database_operations();
    let catalog = ruum_mcp::Catalog::load();
    let mut database_tools = 0;
    for tool in &catalog.tools {
        let Some(op) = ops.get(&tool.operation) else {
            continue;
        };
        database_tools += 1;
        // The generated file and the contract agree on the flag.
        assert_eq!(
            tool.planned, op.planned,
            "{}: the planned flag of tools.json differs from openapi.yaml (regenerate it)",
            tool.name
        );
        assert_eq!(
            listed.contains(&tool.name),
            !op.planned,
            "{} ({}): listed must equal not planned",
            tool.name,
            tool.operation
        );
    }
    assert!(database_tools > 0, "no database tool in the catalog");
    // No listed tool maps to a planned database operation (also one that is not in the catalog
    // file by name).
    for name in &listed {
        let spec = catalog
            .tools
            .iter()
            .find(|t| &t.name == name)
            .expect("a listed tool is in the catalog");
        if let Some(op) = ops.get(&spec.operation) {
            assert!(
                !op.planned,
                "{name} is listed but {} is planned",
                spec.operation
            );
        }
    }
    // A planned tool cannot be called either.
    for tool in catalog.tools.iter().filter(|t| t.planned) {
        if ops.contains_key(&tool.operation) {
            let (_, v) = rpc(
                &w.app,
                &w.agent_token,
                &[],
                json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                       "params": {"name": tool.name, "arguments": {}}}),
            )
            .await;
            assert!(
                v["error"].is_object() || v["result"]["isError"] == json!(true),
                "{} can be called: {v}",
                tool.name
            );
        }
    }
}

async fn smoke(w: &World, engine: &str) {
    let id = w.garden(engine).await;

    // The schema: names and types, never values.
    let schema = w.agent("get_database_schema", json!({"node_id": id})).await;
    assert_eq!(schema["isError"], json!(false), "{schema}");
    let tables = schema["structuredContent"]["tables"]
        .as_array()
        .expect("tables");
    let plants = tables
        .iter()
        .find(|t| t["name"] == "plants")
        .unwrap_or_else(|| panic!("no table plants: {schema}"));
    let columns: Vec<&str> = plants["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(columns, ["id", "latin_name", "common_name"], "{schema}");
    assert_no_raw_value("get_database_schema", &schema);

    // The query: every value is a marker.
    let r = w
        .agent(
            "query_database",
            json!({"node_id": id, "body": {
                "sql": "select id, latin_name, common_name from plants"}}),
        )
        .await;
    assert_eq!(r["isError"], json!(false), "{r}");
    let result = &r["structuredContent"];
    assert_eq!(result["content_trust"], "untrusted", "{r}");
    let rows = result["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 2, "{r}");
    let mut cells = 0;
    for row in rows {
        for cell in row.as_array().expect("a row is an array") {
            if cell.is_null() {
                continue;
            }
            cells += 1;
            let kind = cell["$ruum"].as_str().unwrap_or_default();
            assert!(
                matches!(kind, "pseudonym" | "hidden"),
                "a value reached the agent without a marker: {cell} in {r}"
            );
        }
    }
    assert_eq!(cells, 6, "{r}");
    assert_no_raw_value("query_database", &r);

    // An agent's write without a `write_live` grant: the hint is in the text of the result.
    let r = w
        .agent(
            "query_database",
            json!({"node_id": id, "body": {
                "sql": "delete from plants where id = 1", "mode": "write"}}),
        )
        .await;
    assert_eq!(error_code(&r), "write_grant_required", "{r}");
    let hint = r["structuredContent"]["error"]["hint"]
        .as_str()
        .unwrap_or_default();
    assert!(!hint.is_empty(), "no hint: {r}");
    assert!(
        text_of(&r).contains(&format!("hint: {hint}")),
        "the hint is not in the text: {r}"
    );
    // The rows are still there (as the person sees them).
    let r = w
        .person(
            "query_database",
            json!({"node_id": id, "body": {"sql": "select count(*) from plants"}}),
        )
        .await;
    assert_eq!(r["isError"], json!(false), "{r}");
    assert_eq!(r["structuredContent"]["rows"][0][0], json!(2), "{r}");

    // While the database is updating, an agent's query waits, then gets the retryable error.
    let node = NodeId(id.parse().unwrap());
    use ruum_core::providers::store::TypedNodeStore as _;
    let row = w
        .handles
        .typed
        .get(w.tenant, node)
        .await
        .unwrap()
        .expect("typed row");
    w.handles
        .typed
        .report(
            w.tenant,
            &ProviderStatus {
                node_id: node,
                state: ProviderState::Updating,
                generation: row.generation + 1000,
                observed_version_id: row.observed_version,
                reason: None,
                message: None,
                detail: None,
                export: None,
                at: ruum_core::now(),
            },
        )
        .await
        .unwrap();
    let r = w
        .agent(
            "query_database",
            json!({"node_id": id, "body": {"sql": "select count(*) from plants"}}),
        )
        .await;
    assert_eq!(error_code(&r), "database_not_ready", "{r}");
    assert_eq!(r["structuredContent"]["status"], json!(503), "{r}");
    assert_eq!(r["structuredContent"]["error"]["hint"], "retry", "{r}");
    assert!(text_of(&r).contains("hint: retry"), "{r}");
    assert!(
        r["structuredContent"]["retry_after"].as_u64().is_some(),
        "{r}"
    );
}

#[tokio::test]
async fn an_agent_creates_and_queries_a_database_and_sees_markers() {
    let w = world(None).await;
    smoke(&w, "sqlite").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_creates_and_queries_a_postgres_database_and_sees_markers() {
    let Some(pg) = postgres_engine().await else {
        return;
    };
    let w = world(Some(pg)).await;
    smoke(&w, "postgres").await;
}
