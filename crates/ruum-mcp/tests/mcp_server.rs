//! The MCP endpoint against the in-memory services: OAuth protection, the tool list, and tool
//! calls that run the REST operations with the caller's rights.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use ruum_api::AppState;
use ruum_core::actor::{Actor, Context};
use ruum_core::agents::{AgentScope, Budget, NewAgent};
use ruum_core::authz::Role;
use ruum_core::identity::{AccessClaims, TokenType, VerifiedLogin, issue_user_token};
use ruum_core::{TenantId, UserId};
use serde_json::{Value, json};
use tower::ServiceExt as _;

/// One signed-in world: a user with a personal tenant and an agent of that user.
struct World {
    app: Router,
    state: AppState,
    user: UserId,
    tenant: TenantId,
    agent_id: uuid::Uuid,
    agent_token: String,
    agent_key: String,
    user_token: String,
}

async fn user(state: &AppState, name: &str) -> ruum_core::identity::User {
    state
        .services
        .identity
        .upsert_from_login(&VerifiedLogin {
            issuer: "https://idp.ruum.test".into(),
            subject: format!("{name}-{}", uuid::Uuid::now_v7()),
            email: Some(format!("{name}-{}@example.test", uuid::Uuid::now_v7())),
            email_verified: true,
            name: Some(name.into()),
            id_token: None,
        })
        .await
        .expect("user")
}

async fn world() -> World {
    let state = AppState::in_memory();
    let s = &state.services;
    let u = user(&state, "mari").await;
    let ctx = Context {
        tenant: u.home_tenant_id,
        actor: Actor::user(u.id),
        scope: None,
        idempotency_key: None,
    };
    let agent = s
        .agents
        .register(
            &ctx,
            NewAgent {
                name: "Garden builder".into(),
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
        .expect("agent");
    let (_key, secret) = s
        .agents
        .create_key(&ctx, agent.id, Some("test".into()), None)
        .await
        .expect("key");
    let token = s.agents.exchange_key(&secret).await.expect("exchange");
    let user_token = issue_user_token(s.tokens.as_ref(), u.id, 600).expect("user token");
    World {
        app: ruum_mcp::router(state.clone()),
        state: state.clone(),
        user: u.id,
        tenant: u.home_tenant_id,
        agent_id: agent.id.0,
        agent_token: token.access_token,
        agent_key: secret,
        user_token,
    }
}

/// Sends one JSON-RPC message to `/mcp`.
async fn rpc(
    app: &Router,
    token: Option<&str>,
    extra: &[(&str, &str)],
    body: Value,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut b = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::HOST, "localhost")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    let res = app
        .clone()
        .oneshot(b.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    let v = parse_body(&text);
    (status, headers, v)
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

fn init() -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": {"name": "ruum-test", "version": "1.0"}
    }})
}

async fn call(app: &Router, token: &str, name: &str, args: Value) -> Value {
    call_with(app, token, &[], name, args).await
}

async fn call_with(
    app: &Router,
    token: &str,
    extra: &[(&str, &str)],
    name: &str,
    args: Value,
) -> Value {
    let (status, _, v) = rpc(
        app,
        Some(token),
        extra,
        json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
               "params": {"name": name, "arguments": args}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v["result"].clone()
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

#[tokio::test]
async fn without_a_token_mcp_answers_401_with_resource_metadata() {
    let w = world().await;
    let (status, headers, body) = rpc(&w.app, None, &[], init()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let www = headers
        .get(header::WWW_AUTHENTICATE)
        .expect("WWW-Authenticate")
        .to_str()
        .unwrap();
    assert_eq!(
        www,
        "Bearer resource_metadata=\"https://ruum.localhost/.well-known/oauth-protected-resource\""
    );
    assert_eq!(body["error"]["code"], "unauthenticated");
    assert!(body["error"]["hint"].is_string());
}

#[tokio::test]
async fn invalid_tokens_and_api_keys_get_401() {
    let w = world().await;
    for bad in ["not-a-token", w.agent_key.as_str()] {
        let (status, headers, body) = rpc(&w.app, Some(bad), &[], init()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{bad}: {body}");
        assert!(headers.contains_key(header::WWW_AUTHENTICATE));
        assert_eq!(body["error"]["code"], "unauthenticated");
    }
}

#[tokio::test]
async fn a_disabled_agent_is_refused() {
    let w = world().await;
    let ctx = Context {
        tenant: w.tenant,
        actor: Actor::user(w.user),
        scope: None,
        idempotency_key: None,
    };
    w.state
        .services
        .agents
        .set_disabled(&ctx, ruum_core::AgentId(w.agent_id), true)
        .await
        .unwrap();
    let (status, _, body) = rpc(&w.app, Some(&w.agent_token), &[], init()).await;
    assert!(status.is_client_error(), "{status}: {body}");
    assert_eq!(body["error"]["code"], "agent_disabled");
}

#[tokio::test]
async fn initialize_and_list_every_tool_of_the_contract() {
    let w = world().await;
    let (status, _, v) = rpc(&w.app, Some(&w.agent_token), &[], init()).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["result"]["serverInfo"]["name"], "ruum");
    assert!(v["result"]["capabilities"]["tools"].is_object());

    let (status, _, v) = rpc(
        &w.app,
        Some(&w.agent_token),
        &[],
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let tools = v["result"]["tools"].as_array().expect("tools");
    let catalog = ruum_mcp::Catalog::load();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    // Tools of planned operations (decision 0004-22) are not offered.
    let expected: Vec<&str> = catalog.offered().map(|t| t.name.as_str()).collect();
    assert_eq!(names, expected);
    // 50 step 1 tools, the three database tools of card D-1, the two of card K-1
    // (confirmColumnClasses is users only: no tool), the two grant tools of card A-1
    // (`createDatabaseAccessGrant` is users only, so it has no tool), the three of card Q-1 and
    // the four approval tools of card A-2 (`decideDatabaseApproval` is users only: no tool).
    assert_eq!(names.len(), 64);
    for t in [
        "create_database",
        "get_database",
        "update_database_manifest",
        "list_database_columns",
        "suggest_column_classes",
        "list_database_access_grants",
        "remove_database_access_grant",
        "get_database_schema",
        "query_database",
        "list_database_queries",
    ] {
        assert!(names.contains(&t), "{t} is offered");
    }
    for t in catalog.tools.iter().filter(|t| t.planned) {
        assert!(!names.contains(&t.name.as_str()), "{} is planned", t.name);
    }
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
        assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()));
    }
    let get_node = tools.iter().find(|t| t["name"] == "get_node").unwrap();
    assert_eq!(get_node["inputSchema"]["required"], json!(["node_id"]));
    assert_eq!(get_node["annotations"]["readOnlyHint"], true);
    let write = tools.iter().find(|t| t["name"] == "write_file").unwrap();
    assert!(write["inputSchema"]["properties"]["content"].is_object());
    assert!(write["inputSchema"]["properties"]["body"].is_null());
}

#[tokio::test]
async fn get_me_shows_the_agent_and_its_user() {
    let w = world().await;
    let r = call(&w.app, &w.agent_token, "get_me", json!({})).await;
    assert_eq!(r["isError"], json!(false), "{r}");
    let me = &r["structuredContent"];
    let text = me.to_string();
    assert!(text.contains(&w.agent_id.to_string()), "{me}");
    assert!(text.contains(&w.user.0.to_string()), "{me}");
}

#[tokio::test]
async fn tenant_tools_work_in_the_own_tenant_and_hide_others() {
    let w = world().await;
    let r = call(
        &w.app,
        &w.agent_token,
        "get_tenant",
        json!({"tenant_id": w.tenant.0.to_string()}),
    )
    .await;
    assert_eq!(r["isError"], json!(false), "{r}");
    assert_eq!(r["structuredContent"]["id"], w.tenant.0.to_string());

    let r = call(
        &w.app,
        &w.agent_token,
        "get_limits",
        json!({"tenant_id": w.tenant.0.to_string()}),
    )
    .await;
    assert_eq!(r["isError"], json!(false), "{r}");

    let r = call(&w.app, &w.agent_token, "list_tenants", json!({})).await;
    assert_eq!(r["isError"], json!(false), "{r}");

    // Another user's tenant: the agent gets the same answer as for an unknown one.
    let other = user(&w.state, "juhan").await;
    let r = call(
        &w.app,
        &w.agent_token,
        "get_tenant",
        json!({"tenant_id": other.home_tenant_id.0.to_string()}),
    )
    .await;
    let code = error_code(&r);
    assert!(matches!(code, "not_found" | "permission_denied"), "{r}");
    assert!(
        !r.to_string().contains(&other.email),
        "no data of the other tenant: {r}"
    );
}

#[tokio::test]
async fn change_tools_return_the_journal() {
    let w = world().await;
    let r = call(&w.app, &w.agent_token, "get_latest_cursor", json!({})).await;
    assert_eq!(r["isError"], json!(false), "{r}");
    let r = call(&w.app, &w.agent_token, "list_changes", json!({"limit": 10})).await;
    assert_eq!(r["isError"], json!(false), "{r}");
}

#[tokio::test]
async fn tool_errors_carry_the_ruum_code_and_hint() {
    let w = world().await;
    // A missing required argument: refused before the API is called.
    let r = call(&w.app, &w.agent_token, "get_node", json!({})).await;
    assert_eq!(error_code(&r), "validation_failed");
    assert!(r["structuredContent"]["error"]["hint"].is_string());
    assert!(r["content"][0]["text"].as_str().unwrap().contains("hint:"));

    // An unknown argument.
    let r = call(&w.app, &w.agent_token, "get_root", json!({"tenant": "x"})).await;
    assert_eq!(error_code(&r), "validation_failed");

    // An error of the REST operation: the code and the status come through.
    let r = call(
        &w.app,
        &w.agent_token,
        "get_tenant",
        json!({"tenant_id": "not-a-uuid"}),
    )
    .await;
    let code = error_code(&r);
    assert!(
        matches!(code, "bad_request" | "validation_failed" | "not_found"),
        "{r}"
    );
    assert!(r["structuredContent"]["status"].as_u64().unwrap() >= 400);

    // A node that does not exist (or an area that is not built yet).
    let r = call(
        &w.app,
        &w.agent_token,
        "get_node",
        json!({"node_id": uuid::Uuid::now_v7().to_string()}),
    )
    .await;
    if !pending(&r) {
        assert_eq!(error_code(&r), "not_found", "{r}");
    }
}

#[tokio::test]
async fn an_unknown_tool_is_a_protocol_error() {
    let w = world().await;
    let (status, _, v) = rpc(
        &w.app,
        Some(&w.agent_token),
        &[],
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"name": "drop_database", "arguments": {}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(v["error"].is_object(), "{v}");
}

#[tokio::test]
async fn user_only_operations_cannot_be_called_by_agents() {
    // list_members is a tool and works for the agent.
    let w = world().await;
    let r = call(
        &w.app,
        &w.agent_token,
        "list_members",
        json!({"tenant_id": w.tenant.0.to_string()}),
    )
    .await;
    assert_eq!(r["isError"], json!(false), "{r}");

    let ctx = user_ctx(&w);
    let agents_before = w
        .state
        .services
        .agents
        .list(
            &ctx,
            None,
            ruum_core::paging::PageRequest {
                cursor: None,
                limit: Some(100),
            },
        )
        .await
        .unwrap()
        .items
        .len();
    let catalog = ruum_mcp::Catalog::load();
    // Each user-only operation, called by the names an agent could guess: none is a tool, so
    // the server answers with a protocol error and changes nothing.
    for (op, name, args) in [
        (
            "registerAgent",
            "register_agent",
            json!({"name": "Rogue", "scope": {"max_role": "manage"}}),
        ),
        (
            "createTenant",
            "create_tenant",
            json!({"name": "Rogue tenant"}),
        ),
        (
            "updateMember",
            "update_member",
            json!({"tenant_id": w.tenant.0.to_string(), "user_id": w.user.0.to_string(),
                   "role": "guest"}),
        ),
        (
            "removeMember",
            "remove_member",
            json!({"tenant_id": w.tenant.0.to_string(), "user_id": w.user.0.to_string()}),
        ),
        (
            "createAppPassword",
            "create_app_password",
            json!({"name": "x"}),
        ),
    ] {
        assert!(catalog.tools.iter().all(|t| t.operation != op), "{op}");
        let (status, _, v) = rpc(
            &w.app,
            Some(&w.agent_token),
            &[],
            json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call",
                   "params": {"name": name, "arguments": args}}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{name}: {v}");
        assert!(v["error"].is_object(), "{name} must be refused: {v}");
        assert!(v["result"].is_null(), "{name}: {v}");
    }
    let agents_after = w
        .state
        .services
        .agents
        .list(
            &ctx,
            None,
            ruum_core::paging::PageRequest {
                cursor: None,
                limit: Some(100),
            },
        )
        .await
        .unwrap()
        .items
        .len();
    assert_eq!(agents_before, agents_after, "no agent was registered");
    let members = call(
        &w.app,
        &w.agent_token,
        "list_members",
        json!({"tenant_id": w.tenant.0.to_string()}),
    )
    .await;
    assert!(
        members.to_string().contains(&w.user.0.to_string()),
        "the owner is still a member: {members}"
    );
}

#[tokio::test]
async fn a_token_for_the_mcp_url_works() {
    let w = world().await;
    let s = &w.state.services;
    let mut claims = s.tokens.verify(&w.agent_token).unwrap();
    claims.aud = format!("{}/mcp", s.tokens.audience().trim_end_matches('/'));
    let mcp_token = s.tokens.issue(&claims).unwrap();
    let r = call(&w.app, &mcp_token, "get_me", json!({})).await;
    assert_eq!(r["isError"], json!(false), "{r}");
    assert!(r.to_string().contains(&w.agent_id.to_string()));

    // A token for another audience is refused at the door.
    let mut other = AccessClaims::new(s.tokens.as_ref(), TokenType::Agent, w.agent_id, 60);
    other.aud = "https://elsewhere.test".into();
    let other = s.tokens.issue(&other).unwrap();
    let (status, _, _) = rpc(&w.app, Some(&other), &[], init()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_user_token_names_its_tenant_with_the_header() {
    let w = world().await;
    let tenant = w.tenant.0.to_string();
    let r = call_with(
        &w.app,
        &w.user_token,
        &[("ruum-tenant", tenant.as_str())],
        "get_latest_cursor",
        json!({}),
    )
    .await;
    assert_eq!(r["isError"], json!(false), "{r}");
    // Without the header the API asks for it.
    let r = call(&w.app, &w.user_token, "get_latest_cursor", json!({})).await;
    assert_eq!(error_code(&r), "bad_request");
}

fn user_ctx(w: &World) -> Context {
    Context {
        tenant: w.tenant,
        actor: Actor::user(w.user),
        scope: None,
        idempotency_key: None,
    }
}

/// True when the tool answered `not_implemented` (an area that is not built yet): the check is
/// pending. With `RUUM_REQUIRE_IMPLEMENTED=1` a pending check fails (docs/TESTING.md).
fn pending(result: &Value) -> bool {
    if result["isError"] != json!(true)
        || result["structuredContent"]["error"]["code"] != "not_implemented"
    {
        return false;
    }
    if std::env::var_os("RUUM_REQUIRE_IMPLEMENTED").is_some_and(|v| v != "0" && !v.is_empty()) {
        panic!("not implemented (RUUM_REQUIRE_IMPLEMENTED is set): {result}");
    }
    eprintln!("pending: {result}");
    true
}

/// A new agent of the world's user with `scope`; returns its access token.
async fn scoped_agent(w: &World, name: &str, scope: AgentScope) -> String {
    let s = &w.state.services;
    let ctx = user_ctx(w);
    let agent = s
        .agents
        .register(
            &ctx,
            NewAgent {
                name: name.into(),
                description: None,
                scope,
            },
        )
        .await
        .expect("agent");
    let (_key, secret) = s
        .agents
        .create_key(&ctx, agent.id, Some("test".into()), None)
        .await
        .expect("key");
    s.agents
        .exchange_key(&secret)
        .await
        .expect("exchange")
        .access_token
}

fn scope(subtree: Option<uuid::Uuid>, max_role: Role) -> AgentScope {
    AgentScope {
        subtree: subtree.map(ruum_core::NodeId),
        max_role,
        expires_at: None,
        budget: Budget::default(),
    }
}

/// Creates a node with the world's agent; `None` when the nodes area is not built yet.
async fn create_node(w: &World, parent: &str, name: &str, kind: &str) -> Option<String> {
    let r = call(
        &w.app,
        &w.agent_token,
        "create_node",
        json!({"body": {"parent_id": parent, "name": name, "kind": kind}}),
    )
    .await;
    if pending(&r) {
        return None;
    }
    assert_eq!(r["isError"], json!(false), "create_node {name}: {r}");
    Some(
        r["structuredContent"]["node"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("no id: {r}"))
            .to_string(),
    )
}

/// The root folder ID; `None` when the nodes area is not built yet.
async fn root_id(w: &World) -> Option<String> {
    let r = call(&w.app, &w.agent_token, "get_root", json!({})).await;
    if pending(&r) {
        return None;
    }
    assert_eq!(r["isError"], json!(false), "get_root: {r}");
    Some(r["structuredContent"]["id"].as_str().unwrap().to_string())
}

#[tokio::test]
async fn a_write_through_mcp_is_journaled_as_the_agent_for_its_user() {
    let w = world().await;
    let Some(root) = root_id(&w).await else {
        return;
    };
    let cursor = call(&w.app, &w.agent_token, "get_latest_cursor", json!({})).await;
    assert_eq!(cursor["isError"], json!(false), "{cursor}");
    let cursor = cursor["structuredContent"]["cursor"].clone();
    let Some(folder) = create_node(&w, &root, "Garden", "folder").await else {
        return;
    };
    let mut args = json!({"limit": 100});
    if cursor.is_string() {
        args["cursor"] = cursor;
    }
    let r = call(&w.app, &w.agent_token, "list_changes", args).await;
    assert_eq!(r["isError"], json!(false), "{r}");
    let items = r["structuredContent"]["items"].as_array().expect("items");
    let created = items
        .iter()
        .find(|e| e["type"] == "node.created" && e["node_id"] == folder.as_str())
        .unwrap_or_else(|| panic!("no node.created for {folder}: {r}"));
    assert_eq!(created["actor"]["kind"], "agent", "{created}");
    assert_eq!(created["actor"]["id"], w.agent_id.to_string(), "{created}");
    assert_eq!(created["on_behalf_of"], w.user.0.to_string(), "{created}");
}

#[tokio::test]
async fn an_agent_with_a_subtree_scope_sees_nothing_outside_it() {
    let w = world().await;
    let Some(root) = root_id(&w).await else {
        return;
    };
    let Some(inside) = create_node(&w, &root, "Inside", "folder").await else {
        return;
    };
    let outside = create_node(&w, &root, "Outside", "folder")
        .await
        .expect("create_node works");
    let token = scoped_agent(
        &w,
        "Subtree bot",
        scope(Some(inside.parse().unwrap()), Role::Edit),
    )
    .await;

    // Inside the subtree the agent works.
    let r = call(&w.app, &token, "get_node", json!({"node_id": inside})).await;
    assert_eq!(r["isError"], json!(false), "{r}");
    let r = call(
        &w.app,
        &token,
        "create_node",
        json!({"body": {"parent_id": inside, "name": "ok.md", "kind": "file"}}),
    )
    .await;
    assert_eq!(r["isError"], json!(false), "{r}");

    // Outside it: the same answer as for a node that does not exist.
    let r = call(&w.app, &token, "get_node", json!({"node_id": outside})).await;
    assert_eq!(error_code(&r), "not_found", "{r}");
    assert!(!r.to_string().contains("Outside"), "no data leaks: {r}");
    let r = call(
        &w.app,
        &token,
        "create_node",
        json!({"body": {"parent_id": outside, "name": "nope.md", "kind": "file"}}),
    )
    .await;
    assert_eq!(error_code(&r), "not_found", "{r}");
}

#[tokio::test]
async fn an_agent_with_max_role_view_cannot_write() {
    let w = world().await;
    let Some(root) = root_id(&w).await else {
        return;
    };
    let Some(file) = create_node(&w, &root, "plan.md", "file").await else {
        return;
    };
    let token = scoped_agent(&w, "Reader bot", scope(None, Role::View)).await;

    let r = call(&w.app, &token, "get_node", json!({"node_id": file})).await;
    assert_eq!(r["isError"], json!(false), "the agent can read: {r}");
    let r = call(
        &w.app,
        &token,
        "write_file",
        json!({"node_id": file, "content": "# changed", "encoding": "utf8"}),
    )
    .await;
    if pending(&r) {
        return;
    }
    // The user may write; the agent's scope leaves it out (`403 scope_exceeded`).
    assert!(
        ["permission_denied", "scope_exceeded"].contains(&error_code(&r)),
        "{r}"
    );
}
