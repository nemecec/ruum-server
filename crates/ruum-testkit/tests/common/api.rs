//! Requests and set-up steps of the database cases. Each helper names, in its comment, the
//! operations it calls, so a case can name them in its `ops` list.

use super::{Ctx, Fail};
use crate::{check, fail};
use axum::http::Method;
use ruum_core::NodeId;
use ruum_testkit::client::RequestSpec;
use ruum_testkit::fixtures::{Credentials, Principal, Who};
use serde_json::{Value, json};
use std::time::Duration;

/// How long a case waits for a database to become `ready`.
pub const READY_WITHIN: Duration = Duration::from_secs(30);

/// The pattern of a pseudonym (`Pseudonym.id`): `p_` and 6 to 32 lower-case hex characters.
pub fn is_pseudonym(s: &str) -> bool {
    s.strip_prefix("p_").is_some_and(|h| {
        (6..=32).contains(&h.len())
            && h.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    })
}

/// The engine the cases use, and its names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Sqlite,
    Postgres,
}

impl Engine {
    pub fn name(self) -> &'static str {
        match self {
            Engine::Sqlite => "sqlite",
            Engine::Postgres => "postgres",
        }
    }

    /// The schema of the database's own tables: `main` (SQLite) or `public` (Postgres).
    pub fn schema(self) -> &'static str {
        match self {
            Engine::Sqlite => "main",
            Engine::Postgres => "public",
        }
    }

    /// A placeholder: `?n` (SQLite) or `$n` (Postgres).
    pub fn param(self, n: usize) -> String {
        match self {
            Engine::Sqlite => format!("?{n}"),
            Engine::Postgres => format!("${n}"),
        }
    }

    /// A valid manifest for this engine (`DatabaseManifest`).
    pub fn manifest(self) -> Value {
        match self {
            Engine::Sqlite => json!({ "engine": "sqlite", "version": "3", "plan": "shared-small" }),
            Engine::Postgres => {
                json!({ "engine": "postgres", "version": "17", "plan": "shared-small" })
            }
        }
    }

    /// The `data_classes` key of a column: `<schema>.<table>.<column>`.
    pub fn key(self, table: &str, column: &str) -> String {
        format!("{}.{table}.{column}", self.schema())
    }
}

/// The engine of the fixture database `<prefix>-garden` (seeded by card D-1). The cases make
/// their databases with the same engine, so they run on whatever engine the backend has.
/// Calls `getDatabase`.
pub async fn engine(cx: &Ctx) -> Result<Engine, Fail> {
    let db = cx
        .ok(Who::Owner, get_database(cx, cx.a().database), &[200])
        .await
        .map_err(|e| match e {
            Fail::Failed(m) => Fail::Failed(format!(
                "the fixture database {} (card D-1 seeds it): {m}",
                cx.a().database_name
            )),
            p => p,
        })?;
    match db["manifest"]["engine"].as_str() {
        Some("sqlite") => Ok(Engine::Sqlite),
        Some("postgres") => Ok(Engine::Postgres),
        other => fail!("the fixture database has engine {other:?}"),
    }
}

pub fn db_path(db: impl std::fmt::Display, rest: &str) -> String {
    format!("/v1/databases/{db}{rest}")
}

/// `createDatabase`.
pub fn create_database(cx: &Ctx, parent: NodeId, name: &str, e: Engine) -> RequestSpec {
    RequestSpec::post("/v1/databases")
        .tenant(cx.a().id)
        .json(json!({ "parent_id": parent, "name": name, "manifest": e.manifest() }))
}

/// `getDatabase`.
pub fn get_database(cx: &Ctx, db: impl std::fmt::Display) -> RequestSpec {
    RequestSpec::get(db_path(db, "")).tenant(cx.a().id)
}

/// `updateDatabaseManifest`.
pub fn update_manifest(cx: &Ctx, db: impl std::fmt::Display, manifest: Value) -> RequestSpec {
    RequestSpec::new(Method::PUT, db_path(db, "/manifest"))
        .tenant(cx.a().id)
        .json(json!({ "manifest": manifest }))
}

/// `queryDatabase` with a body of its own.
pub fn query_body(cx: &Ctx, db: impl std::fmt::Display, body: Value) -> RequestSpec {
    RequestSpec::post(db_path(db, "/query"))
        .tenant(cx.a().id)
        .json(body)
}

/// `queryDatabase`, `mode: read`.
pub fn read(cx: &Ctx, db: impl std::fmt::Display, sql: &str) -> RequestSpec {
    query_body(cx, db, json!({ "sql": sql, "mode": "read" }))
}

/// `queryDatabase`, `mode: write`.
pub fn write(cx: &Ctx, db: impl std::fmt::Display, sql: &str) -> RequestSpec {
    query_body(cx, db, json!({ "sql": sql, "mode": "write" }))
}

/// `applyDatabaseMigration` with a body of its own.
pub fn migration_body(cx: &Ctx, db: impl std::fmt::Display, body: Value) -> RequestSpec {
    RequestSpec::post(db_path(db, "/migrations"))
        .tenant(cx.a().id)
        .json(body)
}

/// `applyDatabaseMigration`.
pub fn migration(cx: &Ctx, db: impl std::fmt::Display, name: &str, up: &str) -> RequestSpec {
    migration_body(cx, db, json!({ "name": name, "up": up }))
}

/// `createDatabaseBranch`.
pub fn branch(cx: &Ctx, db: impl std::fmt::Display, name: &str, data: &str) -> RequestSpec {
    RequestSpec::post(db_path(db, "/branches"))
        .tenant(cx.a().id)
        .json(json!({ "name": name, "data": data }))
}

/// `createDatabaseAccessGrant`.
pub fn grant(cx: &Ctx, db: impl std::fmt::Display, kind: &str, agent: Who) -> RequestSpec {
    RequestSpec::post(db_path(db, "/access-grants"))
        .tenant(cx.a().id)
        .json(json!({
            "kind": kind,
            "agent_id": cx.p(agent).id,
            "duration_minutes": 60,
            "reason": "acceptance test",
        }))
}

/// `decideDatabaseApproval`.
pub fn decide(
    cx: &Ctx,
    db: impl std::fmt::Display,
    approval: &str,
    decision: &str,
    classes: Option<Value>,
) -> RequestSpec {
    let mut body = json!({ "decision": decision });
    if let Some(c) = classes {
        body["classes"] = c;
    }
    RequestSpec::post(db_path(db, &format!("/approvals/{approval}/decision")))
        .tenant(cx.a().id)
        .json(body)
}

/// A `GET` of a path of the database (`/migrations`, `/approvals`, ...).
pub fn list(cx: &Ctx, db: impl std::fmt::Display, rest: &str) -> RequestSpec {
    RequestSpec::get(db_path(db, rest)).tenant(cx.a().id)
}

/// `restoreDatabase`.
pub fn restore(cx: &Ctx, db: impl std::fmt::Display, body: Value) -> RequestSpec {
    RequestSpec::post(db_path(db, "/restore"))
        .tenant(cx.a().id)
        .json(body)
}

/// `createDatabaseRestorePoint`.
pub fn restore_point(cx: &Ctx, db: impl std::fmt::Display, label: &str) -> RequestSpec {
    RequestSpec::post(db_path(db, "/restore-points"))
        .tenant(cx.a().id)
        .json(json!({ "label": label }))
}

/// `copyNode`, with `on_conflict: rename` (a copy next to its source needs a new name).
pub fn copy_node(
    cx: &Ctx,
    node: impl std::fmt::Display,
    parent: impl std::fmt::Display,
) -> RequestSpec {
    RequestSpec::post(format!("/v1/nodes/{node}/copy"))
        .tenant(cx.a().id)
        .json(json!({ "parent_id": parent.to_string(), "on_conflict": "rename" }))
}

/// `moveNode`.
pub fn move_node(
    cx: &Ctx,
    node: impl std::fmt::Display,
    parent: impl std::fmt::Display,
) -> RequestSpec {
    RequestSpec::post(format!("/v1/nodes/{node}/move"))
        .tenant(cx.a().id)
        .json(json!({ "parent_id": parent.to_string() }))
}

/// `getNode`.
pub fn get_node(cx: &Ctx, node: impl std::fmt::Display) -> RequestSpec {
    RequestSpec::get(format!("/v1/nodes/{node}")).tenant(cx.a().id)
}

/// The `items` of a page.
pub fn items(page: &Value) -> Vec<Value> {
    page["items"].as_array().cloned().unwrap_or_default()
}

/// The ID of the database of a `DatabaseWrite`.
pub fn written_id(write: &Value) -> Result<String, Fail> {
    match write["database"]["node"]["id"].as_str() {
        Some(id) => Ok(id.to_string()),
        None => fail!("the answer has no database.node.id: {write}"),
    }
}

/// Polls `getDatabase` (as the owner) until the state is `ready`, at most [`READY_WITHIN`].
/// Fails on `failed`. Returns the database. Calls `getDatabase`.
pub async fn wait_ready(cx: &Ctx, db: &str) -> Result<Value, Fail> {
    let start = std::time::Instant::now();
    loop {
        let d = cx.ok(Who::Owner, get_database(cx, db), &[200]).await?;
        match d["status"]["state"].as_str() {
            Some("ready") => return Ok(d),
            Some("failed") => fail!("database {db} failed: {}", d["status"]),
            Some(_) => {}
            None => fail!("getDatabase {db} has no status.state: {d}"),
        }
        check!(
            start.elapsed() < READY_WITHIN,
            "database {db} is not ready after {READY_WITHIN:?}: {}",
            d["status"]
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Creates a database as `who` in `parent` and waits until it is ready. Calls `createDatabase`
/// and `getDatabase`.
pub async fn new_database(
    cx: &Ctx,
    who: Who,
    parent: NodeId,
    name: &str,
    e: Engine,
) -> Result<String, Fail> {
    let made = cx
        .ok(who, create_database(cx, parent, name, e), &[202])
        .await?;
    let id = written_id(&made)?;
    wait_ready(cx, &id).await?;
    Ok(id)
}

/// The database of most cases: made by `member` (content role `manage`) in `Projects`, with
/// the migration `0001_garden` by `member` (its classes count as confirmed) and some rows.
#[derive(Clone, Debug)]
pub struct Garden {
    pub id: String,
    pub engine: Engine,
    /// The real value in `plants.email` of row 1 (a `personal` column).
    pub email: String,
}

/// The `up` of the garden: `plants (id, name, email)` and three small tables `t1`, `t2`,
/// `t3` (only an `id`, class `normal`) for destructive statements.
pub const GARDEN_UP: &str = "create table plants (id integer primary key, name text, email text);\n\
     create table t1 (id integer primary key);\n\
     create table t2 (id integer primary key);\n\
     create table t3 (id integer primary key);";

/// Makes the garden (see [`Garden`]). Calls `getDatabase` (the engine of the fixture database),
/// `createDatabase`, `applyDatabaseMigration` and `queryDatabase`.
pub async fn garden(cx: &Ctx, name: &str) -> Result<Garden, Fail> {
    let e = engine(cx).await?;
    let id = new_database(cx, Who::Member, cx.a().projects, name, e).await?;
    let mut classes = serde_json::Map::new();
    for (t, c, class) in [
        ("plants", "id", "normal"),
        ("plants", "name", "normal"),
        ("plants", "email", "personal"),
        ("t1", "id", "normal"),
        ("t2", "id", "normal"),
        ("t3", "id", "normal"),
    ] {
        classes.insert(e.key(t, c), json!(class));
    }
    cx.ok(
        Who::Member,
        migration_body(
            cx,
            &id,
            json!({ "name": "0001_garden", "up": GARDEN_UP, "data_classes": classes }),
        ),
        &[200],
    )
    .await?;
    let email = format!("{}-rose@example.test", cx.a().prefix);
    for sql in [
        format!("insert into plants (id, name, email) values (1, 'Rose', '{email}')"),
        "insert into t1 (id) values (1), (2)".to_string(),
        "insert into t2 (id) values (1), (2)".to_string(),
        "insert into t3 (id) values (1), (2)".to_string(),
    ] {
        cx.ok(Who::Member, write(cx, &id, &sql), &[200]).await?;
    }
    Ok(Garden {
        id,
        engine: e,
        email,
    })
}

/// Sets the scope of a fixture agent through its user (`updateAgent`).
pub async fn set_scope(
    cx: &Ctx,
    agent: Who,
    subtree: Option<NodeId>,
    max_role: &str,
) -> Result<(), Fail> {
    let user = agent.acts_for().expect("an agent");
    cx.ok(
        user,
        RequestSpec::new(Method::PATCH, format!("/v1/agents/{}", cx.p(agent).id))
            .tenant(cx.a().id)
            .json(json!({ "scope": { "subtree": subtree, "max_role": max_role } })),
        &[200],
    )
    .await?;
    Ok(())
}

/// Gives `agent` a grant of `kind` on `db`, by the user it acts for
/// (`createDatabaseAccessGrant`). Returns the grant ID.
pub async fn give_grant(cx: &Ctx, db: &str, kind: &str, agent: Who) -> Result<String, Fail> {
    let user = agent.acts_for().expect("an agent");
    let g = cx.ok(user, grant(cx, db, kind, agent), &[201]).await?;
    match g["grant"]["id"].as_str() {
        Some(id) => Ok(id.to_string()),
        None => fail!("createDatabaseAccessGrant gave no grant.id: {g}"),
    }
}

/// The approval of a `202` (`ApprovalRequired`): its ID, after checking that it is pending.
pub fn approval_of(body: &Value) -> Result<String, Fail> {
    let a = &body["approval"];
    check!(
        a["status"] == "pending",
        "the approval is not pending: {body}"
    );
    match a["id"].as_str() {
        Some(id) => Ok(id.to_string()),
        None => fail!("the 202 has no approval.id: {body}"),
    }
}

/// Sets the tenant policy `guests_create_resources` (`updateTenant`, by the owner).
pub async fn guests_create_resources(cx: &Ctx, on: bool) -> Result<(), Fail> {
    cx.ok(
        Who::Owner,
        RequestSpec::new(Method::PATCH, format!("/v1/tenants/{}", cx.a().id))
            .json(json!({ "policy": { "guests_create_resources": on } })),
        &[200],
    )
    .await?;
    Ok(())
}

/// Gives `who` a direct grant of `role` on `node`, by the owner (`addGrant`).
pub async fn add_grant(
    cx: &Ctx,
    node: impl std::fmt::Display,
    who: Who,
    role: &str,
) -> Result<(), Fail> {
    cx.ok(
        Who::Owner,
        RequestSpec::post(format!("/v1/nodes/{node}/grants"))
            .tenant(cx.a().id)
            .json(json!({
                "grantee": { "type": "user", "id": cx.p(who).id },
                "role": role,
                "notify": false,
            })),
        &[200, 201],
    )
    .await?;
    Ok(())
}

/// Makes a folder as `who` (`createNode`) and returns its ID.
pub async fn folder(cx: &Ctx, who: Who, parent: NodeId, name: &str) -> Result<String, Fail> {
    let made = cx
        .ok(
            who,
            RequestSpec::post("/v1/nodes")
                .tenant(cx.a().id)
                .json(json!({ "parent_id": parent, "name": name, "kind": "folder" })),
            &[201],
        )
        .await?;
    match made["node"]["id"].as_str() {
        Some(id) => Ok(id.to_string()),
        None => fail!("createNode gave no node.id: {made}"),
    }
}

/// An `edit` share link on `Projects`: the owner allows edit links (`updateTenant`), makes the
/// link (`createShareLink`) and resolves it (`resolveShareLink`). Returns its bearer.
pub async fn edit_link(cx: &Ctx) -> Result<Principal, Fail> {
    cx.ok(
        Who::Owner,
        RequestSpec::new(Method::PATCH, format!("/v1/tenants/{}", cx.a().id))
            .json(json!({ "policy": { "allow_edit_links": true } })),
        &[200],
    )
    .await?;
    let link = cx
        .ok(
            Who::Owner,
            RequestSpec::post(format!("/v1/nodes/{}/share-links", cx.a().projects))
                .tenant(cx.a().id)
                .json(json!({ "role": "edit" })),
            &[201],
        )
        .await?;
    let Some(token) = link["share_link"]["token"].as_str() else {
        fail!("createShareLink gave no token: {link}");
    };
    let resolved = cx
        .ok(
            Who::Anonymous,
            RequestSpec::post("/v1/share-links/resolve").json(json!({ "token": token })),
            &[200],
        )
        .await?;
    let Some(access) = resolved["access_token"].as_str() else {
        fail!("resolveShareLink gave no access_token");
    };
    Ok(Principal {
        who: Who::ShareLink,
        id: uuid::Uuid::nil(),
        email: None,
        credentials: Credentials::Bearer(access.to_string()),
    })
}

/// The `hint` of an error object contains `want`.
pub fn hint_has(err: &Value, want: &str) -> Result<(), Fail> {
    let hint = err["hint"].as_str().unwrap_or_default();
    check!(
        hint.contains(want),
        "the hint {hint:?} does not name {want:?} ({err})"
    );
    Ok(())
}

/// The value of the first cell of the first row of a `QueryResult`.
pub fn first_cell(result: &Value) -> Value {
    result["rows"][0][0].clone()
}

/// The newest cursor of the changes feed (`getLatestCursor`), to show that nothing was written.
pub async fn latest_cursor(cx: &Ctx) -> Result<Value, Fail> {
    cx.ok(
        Who::Owner,
        RequestSpec::get("/v1/changes/latest").tenant(cx.a().id),
        &[200],
    )
    .await
}
