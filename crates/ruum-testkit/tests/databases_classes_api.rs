//! Card K-1 over HTTP, on the backend of the suites (`RUUM_SUITE_BACKEND=memory|postgres`):
//! `listDatabaseColumns`, `suggestColumnClasses` and `confirmColumnClasses` on the fixture
//! database `<prefix>-garden` (table `plants`, wave 4 gate G-1), with a recording
//! `MaskingRules` in place of the lens rules of Q-2 and Q-3.
//!
//! The T-1 cases of these operations wait for `applyDatabaseMigration` (card D-2; wave 4 gate
//! G-3), so this file proves the rules of the card until then.

use axum::http::StatusCode;
use ruum_api::AppState;
use ruum_core::actor::{Actor, Context};
use ruum_core::agents::{AgentScope, Budget, NewAgent};
use ruum_core::authz::Role;
use ruum_core::databases::{self, DatabaseParts};
use ruum_core::privacy::{ClassesChanged, RecordingMaskingRules};
use ruum_testkit::client::{self, RequestSpec, Response};
use ruum_testkit::fixtures::{self, Backend, Credentials, Principal, TenantFixture, Who};
use serde_json::{Value, json};
use std::sync::Arc;

fn columns(t: &TenantFixture) -> RequestSpec {
    RequestSpec::get(format!("/v1/databases/{}/columns", t.database)).tenant(t.id)
}

fn suggest(t: &TenantFixture, column: &str, class: &str) -> RequestSpec {
    RequestSpec::post(format!("/v1/databases/{}/columns/suggestions", t.database))
        .tenant(t.id)
        .json(json!({ "suggestions": [{
            "schema": "main", "table": "plants", "column": column,
            "class": class, "reason": "botanical names, not about people",
        }] }))
}

fn confirm(t: &TenantFixture, column: &str, class: &str) -> RequestSpec {
    RequestSpec::post(format!("/v1/databases/{}/columns/confirm", t.database))
        .tenant(t.id)
        .json(json!({ "columns": [{
            "schema": "main", "table": "plants", "column": column, "class": class,
        }] }))
}

fn get_db(t: &TenantFixture) -> RequestSpec {
    RequestSpec::get(format!("/v1/databases/{}", t.database)).tenant(t.id)
}

fn body(r: &Response) -> Value {
    r.json().unwrap_or(Value::Null)
}

fn item<'a>(page: &'a Value, column: &str) -> &'a Value {
    page["items"]
        .as_array()
        .and_then(|a| a.iter().find(|c| c["column"] == column))
        .unwrap_or_else(|| panic!("no column {column} in {page}"))
}

/// An agent of `member` with `max_role` on `Projects`, with a bearer token.
async fn agent(state: &AppState, t: &TenantFixture, max_role: Role) -> Principal {
    let c = Context {
        tenant: t.id,
        actor: Actor::user(t.user_id(Who::Member)),
        scope: None,
        idempotency_key: None,
    };
    let s = &state.services;
    let a = s
        .agents
        .register(
            &c,
            NewAgent {
                name: format!("{}-classes-{max_role:?}", t.prefix),
                description: None,
                scope: AgentScope {
                    subtree: Some(t.projects),
                    max_role,
                    expires_at: None,
                    budget: Budget::default(),
                },
            },
        )
        .await
        .expect("register an agent");
    let (_, secret) = s
        .agents
        .create_key(&c, a.id, Some("classes test".into()), None)
        .await
        .expect("an agent key");
    let token = s.agents.exchange_key(&secret).await.expect("a token");
    Principal {
        who: Who::Agent,
        id: a.id.0,
        email: None,
        credentials: Credentials::Bearer(token.access_token),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn databases_classes_api() {
    let backend = Backend::start().await;
    let (state, db) = backend.world().await;
    // A recording MaskingRules, so the test sees every classes_changed call.
    let rec = Arc::new(RecordingMaskingRules::new());
    let mut s = state.services.clone();
    let d = s.databases.deps().clone();
    let keys = s.tenant_keys.clone();
    databases::install(
        &mut s,
        DatabaseParts {
            engines: d.engines.clone(),
            keys,
            store: d.store.clone(),
            typed: d.typed.clone(),
            columns: d.columns.clone(),
            masking: rec.clone(),
            grants: d.grants.clone(),
        },
    )
    .expect("install the database area");
    let state = AppState::new(s, state.config.clone());
    let w = fixtures::seed(&state).await;
    assert!(w.seeded, "the fixtures are seeded");
    let app = client::app(state.clone());
    let a = &w.a;
    let p = |who: Who| a.principal(who).clone();
    let calls_on_a = |calls: Vec<ClassesChanged>| {
        calls
            .into_iter()
            .filter(|c| c.tenant == a.id && c.node.0 == a.database.0)
            .count()
    };
    rec.take();

    // listDatabaseColumns: view; the columns of `plants`, all personal and unconfirmed.
    let r = client::send(&app, &columns(a), &p(Who::Owner)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let page = body(&r);
    let names: Vec<&str> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["column"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["common_name", "id", "latin_name"]);
    for c in page["items"].as_array().unwrap() {
        assert_eq!(c["schema"], "main");
        assert_eq!(c["table"], "plants");
        assert_eq!(
            c["data_class"], "personal",
            "unconfirmed counts as personal"
        );
        assert_eq!(c["confirmed_class"], Value::Null);
        assert_eq!(c["confirmed_by"], Value::Null);
    }
    assert_eq!(page["next_cursor"], Value::Null);
    assert_eq!(
        calls_on_a(rec.take()),
        1,
        "the first sync found new columns"
    );
    let r = client::send(&app, &get_db(a), &p(Who::Owner)).await;
    assert_eq!(body(&r)["unconfirmed_columns"], 3);
    // The narrow agent (view) lists; a person of tenant B does not see A's database.
    let r = client::send(&app, &columns(a), &p(Who::Agent)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = client::send(&app, &columns(a).tenant(w.b.id), w.b.principal(Who::Owner)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // suggestColumnClasses: edit, people and agents; never the effective class.
    let r = client::send(
        &app,
        &suggest(a, "latin_name", "secret"),
        &p(Who::MemberEdit),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let s0 = &body(&r)["items"][0];
    assert_eq!(s0["suggestion"]["class"], "secret");
    assert_eq!(s0["suggestion"]["by"]["actor"]["kind"], "user");
    assert_eq!(s0["data_class"], "personal");
    let editor_agent = agent(&state, a, Role::Edit).await;
    let r = client::send(&app, &suggest(a, "latin_name", "normal"), &editor_agent).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let s1 = &body(&r)["items"][0];
    assert_eq!(s1["suggestion"]["class"], "normal");
    assert_eq!(s1["suggestion"]["by"]["actor"]["kind"], "agent");
    assert_eq!(
        s1["suggestion"]["by"]["actor"]["id"],
        json!(editor_agent.id),
        "the actor"
    );
    assert_eq!(
        s1["suggestion"]["by"]["on_behalf_of"],
        json!(a.user_id(Who::Member)),
        "on_behalf_of"
    );
    assert_eq!(s1["data_class"], "personal", "a suggestion is not a class");
    let r = client::send(&app, &suggest(a, "latin_name", "normal"), &p(Who::Agent)).await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "the view agent may not suggest"
    );
    let r = client::send(&app, &suggest(a, "no_such", "normal"), &p(Who::Owner)).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    assert_eq!(r.error_code().as_deref(), Some("validation_failed"));
    assert_eq!(calls_on_a(rec.take()), 0, "suggestions change no class");

    // confirmColumnClasses: users only, manage.
    let columns_url = format!(
        "{}/t/{}/databases/{}/columns",
        state.config.public_url.trim_end_matches('/'),
        a.id,
        a.database
    );
    for agent in [
        p(Who::Agent),
        editor_agent.clone(),
        agent(&state, a, Role::Manage).await,
    ] {
        let r = client::send(&app, &confirm(a, "latin_name", "normal"), &agent).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
        assert_eq!(r.error_code().as_deref(), Some("user_required"));
        assert_eq!(
            body(&r)["error"]["hint"],
            json!(columns_url),
            "the hint is the web URL of the Columns tab, IDs only"
        );
    }
    let r = client::send(
        &app,
        &confirm(a, "latin_name", "normal"),
        &p(Who::MemberEdit),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "an editor may not confirm");
    assert_eq!(r.error_code().as_deref(), Some("permission_denied"));
    let r = client::send(&app, &confirm(a, "no_such", "normal"), &p(Who::Owner)).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    assert_eq!(
        calls_on_a(rec.take()),
        0,
        "a refused confirmation changes nothing"
    );
    let before = body(&client::send(&app, &get_db(a), &p(Who::Owner)).await);
    let r = client::send(
        &app,
        &confirm(a, "latin_name", "normal").header("If-Match", "\"9999\""),
        &p(Who::Owner),
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED, "{}", r.text());
    let r = client::send(&app, &confirm(a, "latin_name", "normal"), &p(Who::Owner)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let wr = body(&r);
    assert_ne!(
        wr["database"]["manifest_version_id"], before["manifest_version_id"],
        "a confirmation is a new manifest version"
    );
    assert_eq!(
        wr["database"]["manifest"]["data_classes"],
        json!({ "main.plants.latin_name": "normal" }),
        "the head manifest holds the confirmed classes (wave decision 2)"
    );
    assert_eq!(wr["database"]["unconfirmed_columns"], 2);
    assert_eq!(wr["database"]["status"]["state"], "ready");
    assert!(wr["change"]["seq"].is_number());
    assert_eq!(
        calls_on_a(rec.take()),
        1,
        "a confirmation calls classes_changed"
    );
    let r = client::send(&app, &columns(a), &p(Who::Owner)).await;
    let page = body(&r);
    let latin = item(&page, "latin_name");
    assert_eq!(latin["data_class"], "normal");
    assert_eq!(latin["confirmed_class"], "normal");
    assert_eq!(latin["confirmed_by"], json!(a.user_id(Who::Owner)));
    assert!(latin["confirmed_by_display_name"].is_string(), "{latin}");
    assert!(latin["confirmed_at"].is_string());
    assert_eq!(
        latin["suggestion"]["class"], "normal",
        "the newest suggestion stays"
    );
    assert_eq!(item(&page, "id")["data_class"], "personal");
    let r = client::send(
        &app,
        &RequestSpec::get(format!(
            "/v1/databases/{}/columns?unconfirmed=true&limit=1",
            a.database
        ))
        .tenant(a.id),
        &p(Who::Owner),
    )
    .await;
    let first = body(&r);
    assert_eq!(first["items"].as_array().unwrap().len(), 1);
    let cursor = first["next_cursor"]
        .as_str()
        .expect("a second page")
        .to_owned();
    let r = client::send(
        &app,
        &RequestSpec::get(format!(
            "/v1/databases/{}/columns?unconfirmed=true&limit=1&cursor={cursor}",
            a.database
        ))
        .tenant(a.id),
        &p(Who::Owner),
    )
    .await;
    let second = body(&r);
    assert_eq!(second["items"].as_array().unwrap().len(), 1);
    assert_eq!(second["next_cursor"], Value::Null);
    assert!(
        [&first, &second]
            .iter()
            .all(|pg| pg["items"][0]["column"] != "latin_name")
    );
    // The new version is in the node's versions; the confirmation is in the changes feed.
    let r = client::send(
        &app,
        &RequestSpec::get(format!("/v1/nodes/{}/versions", a.database)).tenant(a.id),
        &p(Who::Owner),
    )
    .await;
    assert_eq!(
        body(&r)["items"][0]["id"],
        wr["database"]["manifest_version_id"]
    );
    let r = client::send(
        &app,
        &RequestSpec::get("/v1/changes").tenant(a.id),
        &p(Who::Owner),
    )
    .await;
    let feed = body(&r);
    let entry = feed["items"]
        .as_array()
        .and_then(|i| i.iter().find(|e| e["type"] == "database.columns_confirmed"))
        .unwrap_or_else(|| panic!("database.columns_confirmed in the feed: {feed}"));
    assert_eq!(entry["data"]["columns"][0]["column"], "latin_name");
    assert_eq!(entry["data"]["columns"][0]["old_class"], Value::Null);
    assert_eq!(entry["actor"]["kind"], "user");

    if let Some(db) = db {
        db.finish().await;
    }
    backend.finish().await;
}
