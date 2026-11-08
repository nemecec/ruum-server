//! The query API over HTTP (card Q-1), on the backend of the suites
//! (`RUUM_SUITE_BACKEND=memory|postgres`): `queryDatabase`, `getDatabaseSchema` and
//! `listDatabaseQueries` on the fixture database `<prefix>-garden` (SQLite 3 on the memory
//! engine, with the table `plants` of wave 4 gate G-1).
//!
//! The T-1 cases and the privacy suite of these operations need `applyDatabaseMigration` (card
//! D-2), so they do not run in this wave (wave 4 gate G-3). This file checks the rules of card
//! Q-1 directly: the statement rules (`422`), read-only mode, `503 database_not_ready` with
//! `Retry-After`, the query log (no literal, no parameter value, no pseudonym; who reads which
//! entries), sanitized errors, and the agent rules of wave decision 1 (hidden cells, `403
//! write_grant_required`).

use ruum_core::providers::{ProviderState, ProviderStatus};
use ruum_testkit::fixtures::{self, Backend, TenantFixture};
use ruum_testkit::{RequestSpec, Response, Who, client};
use serde_json::{Value, json};

struct T {
    app: axum::Router,
    state: ruum_api::AppState,
    a: TenantFixture,
}

impl T {
    async fn send(&self, who: Who, req: RequestSpec) -> Response {
        client::send(&self.app, &req, self.a.principal(who)).await
    }

    fn query(&self, body: Value) -> RequestSpec {
        RequestSpec::post(format!("/v1/databases/{}/query", self.a.database))
            .tenant(self.a.id)
            .json(body)
    }

    fn read(&self, sql: &str) -> RequestSpec {
        self.query(json!({ "sql": sql }))
    }

    fn write(&self, sql: &str) -> RequestSpec {
        self.query(json!({ "sql": sql, "mode": "write" }))
    }

    fn get(&self, rest: &str) -> RequestSpec {
        RequestSpec::get(format!("/v1/databases/{}{rest}", self.a.database)).tenant(self.a.id)
    }

    async fn ok(&self, who: Who, req: RequestSpec) -> Value {
        let label = req.label();
        let res = self.send(who, req).await;
        assert_eq!(
            res.status.as_u16(),
            200,
            "{label} as {}: {}",
            who.name(),
            res.text()
        );
        res.json().unwrap()
    }

    /// Expects `status` and `code`; returns the `error` object.
    async fn refused(&self, who: Who, req: RequestSpec, status: u16, code: &str) -> Value {
        let label = req.label();
        let res = self.send(who, req).await;
        assert_eq!(
            (res.status.as_u16(), res.error_code().as_deref()),
            (status, Some(code)),
            "{label} as {}: {}",
            who.name(),
            res.text()
        );
        res.json().unwrap()["error"].clone()
    }

    async fn log(&self, who: Who) -> Vec<Value> {
        self.ok(who, self.get("/queries?limit=1000")).await["items"]
            .as_array()
            .unwrap()
            .clone()
    }

    /// Reports a state of the fixture database as its provider (`reportStatus`).
    async fn report(&self, state: ProviderState) {
        let typed = &self.state.services.typed_nodes;
        let row = typed
            .get(self.a.id, self.a.database)
            .await
            .unwrap()
            .unwrap();
        typed
            .report(
                self.a.id,
                &ProviderStatus {
                    node_id: self.a.database,
                    state,
                    generation: row.generation + 1,
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
    }
}

async fn world() -> T {
    let backend = Backend::start().await;
    let (state, _db) = backend.world().await;
    let w = fixtures::seed(&state).await;
    assert!(w.seeded, "the fixture world must seed");
    T {
        app: client::app(state.clone()),
        state,
        a: w.a,
    }
}

/// Whether a text holds a pseudonym (`p_` and 12 lower-case hex characters).
fn has_pseudonym(text: &str) -> bool {
    text.match_indices("p_").any(|(i, _)| {
        let rest = &text[i + 2..];
        rest.len() >= 12
            && rest[..12]
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    })
}

#[tokio::test]
async fn people_query_the_fixture_database() {
    let t = world().await;
    // The standard request of the suites.
    let r = t
        .ok(Who::MemberView, t.read("select count(*) from plants"))
        .await;
    assert_eq!(r["rows"], json!([[2]]));
    assert_eq!(r["row_count"], 1);
    assert_eq!(r["content_trust"], "untrusted");
    assert_eq!(r["mode"], "read");
    assert_eq!(r["truncated"], false);
    assert!(r["truncated_reason"].is_null());
    assert!(r["rows_affected"].is_null());
    assert!(r["query_id"].is_string());
    let r = t
        .ok(
            Who::MemberView,
            t.read("select id, latin_name from plants order by id"),
        )
        .await;
    assert_eq!(
        r["rows"],
        json!([[1, "Rosa canina"], [2, "Bellis perennis"]])
    );
    assert_eq!(
        r["columns"][1],
        json!({
            "name": "latin_name", "type": "text", "data_class": "personal",
            "source": { "schema": "main", "table": "plants", "column": "latin_name" },
            "masking": "none",
        })
    );
    // max_rows only lowers the plan's cap.
    let r = t
        .ok(
            Who::MemberView,
            t.query(json!({ "sql": "select id from plants", "max_rows": 1 })),
        )
        .await;
    assert_eq!(
        (&r["row_count"], &r["truncated"], &r["truncated_reason"]),
        (&json!(1), &json!(true), &json!("max_rows"))
    );
    // A write by an editor.
    let r = t
        .ok(
            Who::MemberEdit,
            t.write("update plants set common_name = 'wild rose' where id = 1"),
        )
        .await;
    assert_eq!(r["rows_affected"], 1);
    // The schema: the database's own tables, names and types only.
    let s = t.ok(Who::MemberView, t.get("/schema")).await;
    assert_eq!(s["engine"], "sqlite");
    let tables = s["tables"].as_array().unwrap();
    assert_eq!(tables.len(), 1, "{s}");
    assert_eq!(tables[0]["name"], "plants");
    assert_eq!(tables[0]["schema"], "main");
    assert_eq!(tables[0]["primary_key"], json!(["id"]));
    assert_eq!(
        tables[0]["columns"][1],
        json!({ "name": "latin_name", "type": "text", "nullable": false,
                "data_class": "personal", "confirmed": false })
    );
    assert!(!s.to_string().contains("Rosa"), "the schema holds a value");
}

#[tokio::test]
async fn the_statement_rules_answer_422() {
    let t = world().await;
    for who in [Who::Member, Who::MemberView, Who::Agent] {
        let e = t
            .refused(
                who,
                t.read("select count(*) from main.plants"),
                422,
                "statement_not_allowed",
            )
            .await;
        assert_eq!(e["hint"], "unqualified_names");
        t.refused(who, t.read("select 1; select 2"), 422, "validation_failed")
            .await;
        for sql in [
            "set local statement_timeout = 0",
            "reset all",
            "select set_config('x', 'y', true)",
            "pragma writable_schema = 1",
            "attach database 'other' as other",
            "select * from ruum_roles",
        ] {
            t.refused(who, t.read(sql), 422, "statement_not_allowed")
                .await;
        }
        let e = t
            .refused(who, t.read("begin"), 422, "statement_not_allowed")
            .await;
        assert_eq!(e["hint"], "remove_transaction_control");
        t.ok(who, t.read("select count(*) from plants")).await;
    }
    let e = t
        .refused(Who::Member, t.write("commit"), 422, "statement_not_allowed")
        .await;
    assert_eq!(e["hint"], "remove_transaction_control");
    // A write in read mode: by the parser, and by the engine alone.
    t.refused(
        Who::Member,
        t.read("delete from plants"),
        400,
        "read_only_query",
    )
    .await;
    t.refused(
        Who::Member,
        t.read(
            "with x as (select 9 as id) insert into plants (id, latin_name) select id, 'x' from x",
        ),
        400,
        "read_only_query",
    )
    .await;
    // A viewer cannot write; the request limits.
    t.refused(
        Who::MemberView,
        t.write("delete from plants"),
        403,
        "permission_denied",
    )
    .await;
    t.refused(
        Who::MemberView,
        t.query(json!({ "sql": "select 1", "timeout_ms": 10 })),
        422,
        "validation_failed",
    )
    .await;
    let r = t
        .ok(Who::Member, t.read("select count(*) from plants"))
        .await;
    assert_eq!(r["rows"], json!([[2]]), "nothing changed");
}

#[tokio::test]
async fn errors_are_sanitized_for_people_without_manage() {
    let t = world().await;
    let dup = "insert into plants (id, latin_name) values (1, 'dup-value-xyz')";
    let e = t
        .refused(Who::MemberEdit, t.write(dup), 400, "query_failed")
        .await;
    assert_eq!(e["sqlstate"], "23505");
    assert_eq!(e["message"], "a unique constraint was violated: plants.id");
    assert!(e.get("hint").is_none());
    let e = t
        .refused(Who::Member, t.write(dup), 400, "query_failed")
        .await;
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .contains("UNIQUE constraint failed"),
        "{e}"
    );
    // DDL by an editor fails in the engine (the write kind makes no objects).
    let e = t
        .refused(
            Who::MemberEdit,
            t.write("alter table plants add column notes text"),
            400,
            "query_failed",
        )
        .await;
    assert_eq!(e["sqlstate"], "42501");
}

/// Wave decision 1: an agent reads only hidden values and never writes before card Q-2.
#[tokio::test]
async fn agents_read_hidden_values_and_never_write() {
    let t = world().await;
    let r = t
        .ok(
            Who::Agent,
            t.read("select id, latin_name, common_name from plants order by id"),
        )
        .await;
    let hidden = json!({ "$ruum": "hidden", "class": "personal" });
    assert_eq!(r["rows"][0], json!([hidden, hidden, hidden]));
    assert!(!r.to_string().contains("Rosa"), "{r}");
    for c in r["columns"].as_array().unwrap() {
        assert_eq!(
            (&c["data_class"], &c["masking"]),
            (&json!("personal"), &json!("hidden"))
        );
    }
    // The agent's scope (view) first; then, with edit, the write grant.
    t.refused(
        Who::Agent,
        t.write("delete from plants"),
        403,
        "scope_exceeded",
    )
    .await;
    t.ok(
        Who::Member,
        RequestSpec::new(
            axum::http::Method::PATCH,
            format!("/v1/agents/{}", t.a.principal(Who::Agent).id),
        )
        .tenant(t.a.id)
        .json(json!({ "scope": { "subtree": t.a.projects, "max_role": "edit" } })),
    )
    .await;
    t.refused(
        Who::Agent,
        t.write("delete from plants"),
        403,
        "write_grant_required",
    )
    .await;
    let r = t
        .ok(Who::Member, t.read("select count(*) from plants"))
        .await;
    assert_eq!(r["rows"], json!([[2]]));
}

#[tokio::test]
async fn the_log_holds_no_value_and_follows_the_visibility() {
    let t = world().await;
    let literal = "Rosa canina";
    let param = "param-value-4711";
    t.ok(
        Who::MemberView,
        t.query(json!({
            "sql": format!("select common_name from plants where latin_name = '{literal}' or latin_name = ?1"),
            "params": [param],
        })),
    )
    .await;
    t.refused(
        Who::MemberView,
        t.query(json!({
            "sql": "select * from main.plants where latin_name = 'Bellis perennis'",
            "params": [param, null],
        })),
        422,
        "statement_not_allowed",
    )
    .await;
    t.ok(
        Who::Agent,
        t.read("select latin_name from plants where id = 2"),
    )
    .await;
    t.ok(
        Who::AgentOfGuestView,
        t.read("select 'agent-literal-77' from plants"),
    )
    .await;
    t.ok(Who::GuestView, t.read("select 1 from plants")).await;
    t.ok(Who::MemberEdit, t.read("select 2 from plants")).await;

    // Manage: every entry; no literal, no parameter value, no pseudonym, no hash.
    let all = t.log(Who::Member).await;
    assert_eq!(all.len(), 6, "{all:#?}");
    let text = serde_json::to_string(&all).unwrap();
    for value in [literal, param, "Bellis", "agent-literal-77", "Rosa"] {
        assert!(!text.contains(value), "the log holds {value:?}: {text}");
    }
    assert!(!has_pseudonym(&text), "the log holds a pseudonym: {text}");
    assert!(!text.contains("statement_hash") && !text.contains("sqlstate"));
    let first = all
        .iter()
        .find(|e| {
            e["outcome"] == "ok"
                && e["by"]["actor"]["id"] == json!(t.a.principal(Who::MemberView).id)
        })
        .unwrap();
    assert_eq!(
        first["statement"],
        "select common_name from plants where latin_name = $L1 or latin_name = ?1"
    );
    assert_eq!(
        first["params"],
        json!([{ "$ruum": "hidden", "class": "personal" }])
    );
    assert_eq!(first["mode"], "read");
    assert_eq!(first["row_count"], 1);
    let refused = all.iter().find(|e| e["outcome"] == "error").unwrap();
    assert_eq!(refused["error_code"], "statement_not_allowed");
    assert_eq!(
        refused["params"],
        json!([{ "$ruum": "hidden", "class": "personal" }, null])
    );
    // A person with view: own entries only (member_view has no agent).
    let own = t.log(Who::MemberView).await;
    assert_eq!(own.len(), 2);
    // A person with view whose agent ran a query: own entries and the agent's.
    let guest = t.log(Who::GuestView).await;
    assert_eq!(guest.len(), 2, "{guest:#?}");
    assert!(guest.iter().any(|e| e["by"]["actor"]["kind"] == "agent"));
    // An editor without manage: own entries only.
    assert_eq!(t.log(Who::MemberEdit).await.len(), 1);
    // An agent: own entries only.
    let agent = t.log(Who::Agent).await;
    assert_eq!(agent.len(), 1);
    assert_eq!(
        agent[0]["by"]["actor"]["id"],
        json!(t.a.principal(Who::Agent).id)
    );
    // The actor filter only narrows.
    let other = t.a.principal(Who::Member).id;
    let narrowed = t
        .ok(Who::MemberView, t.get(&format!("/queries?actor={other}")))
        .await;
    assert_eq!(narrowed["items"], json!([]));
    let writes = t.ok(Who::Member, t.get("/queries?mode=write")).await;
    assert_eq!(writes["items"], json!([]));
    // Paging.
    let p1 = t.ok(Who::Member, t.get("/queries?limit=4")).await;
    assert_eq!(p1["items"].as_array().unwrap().len(), 4);
    let cursor = p1["next_cursor"].as_str().unwrap().to_string();
    let p2 = t
        .ok(
            Who::Member,
            t.get(&format!("/queries?limit=4&cursor={cursor}")),
        )
        .await;
    assert_eq!(p2["items"].as_array().unwrap().len(), 2);
    assert!(p2["next_cursor"].is_null());
}

/// While the database is not ready, every kind but owner waits 10 seconds, then gets `503
/// database_not_ready` with `Retry-After`; a write by a person with manage (owner) runs.
#[tokio::test]
async fn a_database_that_is_not_ready_answers_503() {
    let t = world().await;
    t.report(ProviderState::Updating).await;
    let res = t
        .send(Who::MemberView, t.read("select count(*) from plants"))
        .await;
    assert_eq!(res.status.as_u16(), 503, "{}", res.text());
    assert_eq!(res.error_code().as_deref(), Some("database_not_ready"));
    assert!(res.headers.contains_key("retry-after"));
    assert_eq!(res.json().unwrap()["error"]["hint"], "retry");
    let res = t.send(Who::MemberView, t.get("/schema")).await;
    assert_eq!(res.status.as_u16(), 503, "{}", res.text());
    assert!(res.headers.contains_key("retry-after"));
    t.ok(
        Who::Member,
        t.write("update plants set common_name = 'rose' where id = 1"),
    )
    .await;
    t.report(ProviderState::Ready).await;
    t.ok(Who::MemberView, t.read("select count(*) from plants"))
        .await;
}
