//! Tests of the query API: the role kinds, the agent rules of wave decision 1, the answers of
//! engine errors, the result cells, the service on the in-memory services (memory engine), the
//! Postgres query log (`#[sqlx::test]`), and a run on the Postgres engine (`RUUM_DBHOST_URL`;
//! skipped without it, except in CI).

use super::*;
use crate::actor::{PrincipalKind, PrincipalRef};
use crate::agents::{AgentScope, Budget, NewAgent};
use crate::databases::tests::{World, ctx, settled, world};
use crate::databases::{CreateDatabase, DatabaseParts, Engines, UpdateManifest, install};
use crate::ids::{PrincipalId, UserId};
use crate::nodes::OnConflict;
use crate::sharing::Grantee;
use crate::tenants::MemberRole;
use serde_json::json;

// ------------------------------------------------------------------------- pure functions

#[test]
fn the_role_kind_follows_the_caller() {
    use RoleKind::*;
    use StatementMode::{Read as R, Write as W};
    let cases = [
        (None, R, None),
        (Some(Role::Comment), R, Some(Read)),
        (Some(Role::View), R, Some(Read)),
        (Some(Role::View), W, None),
        (Some(Role::Comment), W, None),
        (Some(Role::Edit), R, Some(Read)),
        (Some(Role::Edit), W, Some(Write)),
        (Some(Role::Manage), R, Some(SecretRead)),
        (Some(Role::Manage), W, Some(Owner)),
    ];
    for (role, mode, kind) in cases {
        assert_eq!(person_kind(role, mode), kind, "{role:?} {mode:?}");
    }
}

/// Wave decision 1: an agent reads as `masked_read` and never writes before Q-2.
#[test]
fn agents_fail_closed() {
    assert_eq!(agent_kind(StatementMode::Read), Ok(RoleKind::MaskedRead));
    let p = agent_kind(StatementMode::Write).unwrap_err();
    assert_eq!((p.status, p.code), (403, "write_grant_required"));

    let mut columns = vec![QueryColumn {
        name: "email".into(),
        type_name: "text".into(),
        data_class: DataClass::Normal,
        source: None,
        masking: Masking::None,
    }];
    let mut rows = vec![
        vec![json!("alice@example.test")],
        vec![serde_json::Value::Null],
        vec![json!(42)],
    ];
    mark_agent_result(&mut columns, &mut rows);
    assert_eq!(columns[0].data_class, DataClass::Personal);
    assert_eq!(columns[0].masking, Masking::Hidden);
    let hidden = json!({ "$ruum": "hidden", "class": "personal" });
    assert_eq!(
        rows,
        vec![vec![hidden.clone()], vec![json!(null)], vec![hidden]]
    );
}

#[test]
fn log_visibility_follows_the_caller() {
    let user = UserId::new();
    let agent = Actor {
        actor: PrincipalRef {
            kind: PrincipalKind::Agent,
            id: PrincipalId(Uuid::now_v7()),
        },
        on_behalf_of: Some(user),
    };
    // An agent reads only its own rows, also with manage.
    for role in [Some(Role::View), Some(Role::Manage)] {
        assert_eq!(
            log_visibility(&agent, role).unwrap(),
            LogVisibility::OwnOnly(agent.actor.id)
        );
    }
    assert_eq!(
        log_visibility(&Actor::user(user), Some(Role::Manage)).unwrap(),
        LogVisibility::All
    );
    for role in [Some(Role::View), Some(Role::Edit)] {
        assert_eq!(
            log_visibility(&Actor::user(user), role).unwrap(),
            LogVisibility::OwnAndAgentsOf(user)
        );
    }
}

#[test]
fn engine_errors_are_sanitized_without_manage() {
    let problem = |e: EngineError, manage: bool| match engine_problem(e, manage) {
        QueryError::Problem(p) => p,
        QueryError::Core(e) => panic!("not a problem: {e}"),
    };
    // A value in the engine's primary text (22P02) never reaches a caller without manage.
    let e = || EngineError::Sql {
        code: "22P02".into(),
        message: "invalid input syntax for type integer: \"alice@example.test\"".into(),
    };
    let p = problem(e(), false);
    assert_eq!(
        (p.status, p.code, p.sqlstate.as_deref()),
        (400, "query_failed", Some("22P02"))
    );
    assert!(!p.message.contains("alice"), "{}", p.message);
    assert!(problem(e(), true).message.contains("alice"));
    // Object names are kept for the SQLSTATEs that name objects only.
    let p = problem(
        EngineError::Sql {
            code: "23505".into(),
            message: "duplicate key value violates unique constraint \"plants_pkey\"".into(),
        },
        false,
    );
    assert_eq!(p.message, "a unique constraint was violated: plants_pkey");
    // SQLite: the nearest SQLSTATE and the names after the colon.
    let p = problem(
        EngineError::Sql {
            code: "ConstraintViolation".into(),
            message: "UNIQUE constraint failed: plants.id".into(),
        },
        false,
    );
    assert_eq!(p.sqlstate.as_deref(), Some("23505"));
    assert_eq!(p.message, "a unique constraint was violated: plants.id");
    let p = problem(
        EngineError::Sql {
            code: "Unknown".into(),
            message: "no such table: nosuch".into(),
        },
        false,
    );
    assert_eq!(p.sqlstate.as_deref(), Some("42P01"));
    // A name that is not a plain identifier is left out.
    let p = problem(
        EngineError::Sql {
            code: "42703".into(),
            message: "column \"x y'z\" does not exist".into(),
        },
        false,
    );
    assert_eq!(p.message, "the column does not exist");
    // DDL by the write kind.
    let p = problem(EngineError::PermissionDenied("DDL by write".into()), false);
    assert_eq!(
        (p.sqlstate.as_deref(), p.message.as_str()),
        (Some("42501"), "permission denied")
    );
    assert_eq!(
        problem(EngineError::ReadOnly, false).code,
        "read_only_query"
    );
    assert_eq!(problem(EngineError::Timeout, false).status, 504);
    let p = problem(EngineError::NotReady, false);
    assert_eq!(
        (p.status, p.code, p.retry_after),
        (503, "database_not_ready", Some(RETRY_AFTER_SECONDS))
    );
    assert_eq!(p.hint.as_deref(), Some("retry"));
    assert_eq!(
        problem(EngineError::TransactionEnded, false)
            .hint
            .as_deref(),
        Some(rules::HINT_TRANSACTION_CONTROL)
    );
}

#[test]
fn cells_follow_the_contract() {
    // A JSON object with the key `$ruum` is wrapped, also inside an array.
    let marker_like = json!({ "$ruum": "hidden", "class": "secret" });
    assert_eq!(
        cell_value(&Value::Json(marker_like.clone())),
        json!({ "$ruum": "literal", "value": marker_like })
    );
    assert_eq!(
        cell_value(&Value::Json(
            json!([{ "$ruum": "pseudonym", "id": "p_abcdefabcdef" }])
        )),
        json!([{ "$ruum": "literal", "value": { "$ruum": "pseudonym", "id": "p_abcdefabcdef" } }])
    );
    assert_eq!(
        cell_value(&Value::Json(json!({ "a": 1 }))),
        json!({ "a": 1 })
    );
    assert_eq!(cell_value(&Value::Int(42)), json!(42));
    assert_eq!(
        cell_value(&Value::Int(i64::MAX)),
        json!(i64::MAX.to_string())
    );
    assert_eq!(cell_value(&Value::Float(f64::NAN)), json!("NaN"));
    assert_eq!(cell_value(&Value::Bytes(vec![1, 2, 3])), json!("AQID"));
    assert_eq!(cell_value(&Value::Numeric("1.50".into())), json!("1.50"));
    assert_eq!(param_value(&json!(7)), Value::Int(7));
    assert_eq!(param_value(&json!(1.5)), Value::Float(1.5));
    assert_eq!(param_value(&json!("x")), Value::Text("x".into()));
    assert_eq!(param_value(&json!(null)), Value::Null);
}

#[test]
fn class_keys_quote_dots_and_quotes() {
    assert_eq!(class_key("public", "users", "email"), "public.users.email");
    assert_eq!(class_key("main", "a.b", "c\"d"), "main.\"a.b\".\"c\"\"d\"");
    let classes = manifest_classes(&json!({ "data_classes": {
        "public.users.email": "personal",
        "public.users.token": "secret",
        "public.users.id": "normal",
        "public.users.bad": "unknown",
    }}));
    assert_eq!(classes.len(), 3);
    assert_eq!(
        class_of(&classes, "public", "users", "token"),
        (DataClass::Secret, true)
    );
    assert_eq!(
        class_of(&classes, "public", "users", "other"),
        (DataClass::Personal, false)
    );
}

// ------------------------------------------------------------------------- the service

/// The world of the query tests: Alice (manage), Bob (a guest with `edit` on the folder) and
/// Carol (a guest with `view`), and a SQLite database of Alice with the table `plants` and the
/// secret column `token` (by the manifest).
struct Q {
    w: World,
    carol: UserId,
    db: NodeId,
}

fn sql(text: &str) -> QueryRequest {
    QueryRequest {
        sql: text.into(),
        params: Vec::new(),
        mode: None,
        max_rows: None,
        timeout_ms: None,
        approval_id: None,
    }
}

fn write(text: &str) -> QueryRequest {
    QueryRequest {
        mode: Some(StatementMode::Write),
        ..sql(text)
    }
}

async fn agent(w: &World, user: UserId, max_role: Role) -> Context {
    let agent =
        w.s.agents
            .register(
                &ctx(w.team.id, user),
                NewAgent {
                    name: format!("agent {}", Uuid::now_v7().simple()),
                    description: None,
                    scope: AgentScope {
                        subtree: None,
                        max_role,
                        expires_at: None,
                        budget: Budget::default(),
                    },
                },
            )
            .await
            .unwrap();
    Context {
        tenant: w.team.id,
        actor: Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::Agent,
                id: agent.id.into(),
            },
            on_behalf_of: Some(user),
        },
        scope: Some(agent.scope.as_scope()),
        idempotency_key: None,
    }
}

async fn setup() -> Q {
    let w = world().await;
    let a = ctx(w.team.id, w.alice);
    let carol =
        w.s.identity
            .upsert_from_login(&crate::identity::VerifiedLogin {
                issuer: "https://test.ruum.localhost".into(),
                subject: format!("carol-{}", Uuid::now_v7()),
                email: Some(format!("carol-{}@example.test", Uuid::now_v7().simple())),
                email_verified: true,
                name: Some("carol".into()),
                id_token: None,
            })
            .await
            .unwrap();
    let (_, token) =
        w.s.tenants
            .invite(&a, carol.email.clone(), MemberRole::Guest, None)
            .await
            .unwrap();
    w.s.tenants
        .accept_invitation(&Actor::user(carol.id), &token)
        .await
        .unwrap();
    w.s.sharing
        .add_grant(&a, w.folder, Grantee::User { id: carol.id }, Role::View)
        .await
        .unwrap();
    let db =
        w.s.databases
            .create(
                &a,
                CreateDatabase {
                    parent_id: w.folder,
                    name: "Plants".into(),
                    manifest: json!({
                        "engine": "sqlite", "version": "3", "plan": "shared-small",
                        "data_classes": {
                            "main.plants.id": "normal",
                            "main.plants.name": "normal",
                            "main.plants.token": "secret",
                        },
                    }),
                    source: None,
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .unwrap()
            .value
            .node
            .id;
    settled(&w.s, &a, db).await;
    let q = Q {
        w,
        carol: carol.id,
        db,
    };
    for s in [
        "create table plants (id integer primary key, name text, token text)",
        "insert into plants (id, name, token) values (1, 'Rose', 'tok-rose-secret')",
        "insert into plants (id, name, token) values (2, 'Daisy', null)",
    ] {
        q.run(&a, write(s)).await.unwrap();
    }
    q
}

impl Q {
    fn alice(&self) -> Context {
        ctx(self.w.team.id, self.w.alice)
    }
    fn bob(&self) -> Context {
        ctx(self.w.team.id, self.w.bob)
    }
    fn carol(&self) -> Context {
        ctx(self.w.team.id, self.carol)
    }

    async fn run(
        &self,
        c: &Context,
        req: QueryRequest,
    ) -> std::result::Result<QueryResult, QueryError> {
        query(&self.w.s.databases, c, self.db, req).await
    }

    async fn log(&self, c: &Context) -> Vec<QueryLogEntry> {
        list_queries(
            &self.w.s.databases,
            c,
            self.db,
            ListQueries::default(),
            PageRequest::default(),
        )
        .await
        .unwrap()
        .items
    }
}

/// The status and code of a failed call.
fn failure(r: std::result::Result<QueryResult, QueryError>) -> (u16, String, Option<String>) {
    match r {
        Ok(v) => panic!("the call succeeded: {v:?}"),
        Err(QueryError::Problem(p)) => (p.status, p.code.to_string(), p.hint),
        Err(QueryError::Core(e)) => (e.http_status(), e.to_api().code.as_str().into(), None),
    }
}

#[tokio::test]
async fn people_read_and_write_by_their_role() {
    let q = setup().await;
    // A viewer reads; a secret value is the marker (wave decision 2), a NULL stays NULL.
    let r = q
        .run(
            &q.carol(),
            sql("select id, name, token from plants order by id"),
        )
        .await
        .unwrap();
    assert_eq!(r.content_trust, "untrusted");
    assert_eq!(r.row_count, 2);
    assert_eq!(r.rows[0][1], json!("Rose"));
    assert_eq!(
        r.rows[0][2],
        json!({ "$ruum": "hidden", "class": "secret" })
    );
    assert_eq!(r.rows[1][2], json!(null));
    assert_eq!(r.columns[2].data_class, DataClass::Secret);
    assert_eq!(r.columns[2].masking, Masking::Hidden);
    assert_eq!(r.columns[1].masking, Masking::None);
    assert_eq!(
        r.columns[0].source,
        Some(SourceColumn {
            schema: "main".into(),
            table: "plants".into(),
            column: "id".into()
        })
    );
    // A person with manage reads the secret.
    let r = q
        .run(&q.alice(), sql("select token from plants where id = 1"))
        .await
        .unwrap();
    assert_eq!(r.rows[0][0], json!("tok-rose-secret"));
    assert_eq!(r.columns[0].masking, Masking::None);
    // A viewer cannot write; an editor writes rows but no schema.
    assert_eq!(
        failure(
            q.run(&q.carol(), write("insert into plants (id) values (3)"))
                .await
        )
        .0,
        403
    );
    let r = q
        .run(
            &q.bob(),
            write("insert into plants (id, name) values (3, 'Iris')"),
        )
        .await
        .unwrap();
    assert_eq!(r.rows_affected, Some(1));
    let ddl = q
        .run(&q.bob(), write("alter table plants add column notes text"))
        .await;
    match ddl {
        Err(QueryError::Problem(p)) => {
            assert_eq!(
                (p.code, p.sqlstate.as_deref()),
                ("query_failed", Some("42501"))
            );
        }
        other => panic!("DDL by an editor: {other:?}"),
    }
}

/// Wave decision 3: a person's schema change runs through the migration path (the lens views
/// are dropped, `up` runs as owner, the catalog is synced and the lens rebuilt).
#[tokio::test]
async fn a_managers_schema_change_runs_through_the_migration_path() {
    let q = setup().await;
    let row =
        q.w.s
            .databases
            .deps()
            .store
            .get(q.w.team.id, q.db)
            .await
            .unwrap()
            .unwrap();
    q.run(
        &q.alice(),
        write("alter table plants add column notes text"),
    )
    .await
    .unwrap();
    let services_engine = q.w.s.database_engines.get(row.engine).unwrap().clone();
    let catalog = services_engine.catalog(&row.database_ref()).await.unwrap();
    assert!(
        catalog
            .table("main", "plants")
            .unwrap()
            .column("notes")
            .is_some()
    );
    let s = schema(&q.w.s.databases, &q.carol(), q.db).await.unwrap();
    let plants = s.tables.iter().find(|t| t.name == "plants").unwrap();
    let notes = plants.columns.iter().find(|c| c.name == "notes").unwrap();
    assert_eq!(
        (notes.data_class, notes.confirmed),
        (DataClass::Personal, false)
    );
    let token = plants.columns.iter().find(|c| c.name == "token").unwrap();
    assert_eq!(
        (token.data_class, token.confirmed),
        (DataClass::Secret, true)
    );
    assert!(s.tables.iter().all(|t| !t.name.starts_with("ruum_")));
    assert_eq!(s.engine, EngineKind::Sqlite);
}

/// The lens steps of the memory engine show the migration path of wave decision 3.
#[tokio::test]
async fn the_interim_sync_takes_the_manifest_classes() {
    let engine = ruum_dbengine::memory::MemoryEngine::new();
    let key = PseudonymKey::from_bytes([3; 32]);
    let created = engine
        .create_database(
            &ruum_dbengine::NewDatabase {
                tenant_id: Uuid::now_v7(),
                node_id: Uuid::now_v7(),
                engine_version: "3".into(),
                extensions: Vec::new(),
            },
            &key,
        )
        .await
        .unwrap();
    let manifest = json!({ "data_classes": { "main.t.a": "normal", "main.t.s": "secret" } });
    run_schema_change(
        &engine,
        &created.database,
        &key,
        EngineKind::Sqlite,
        &manifest,
        "create table t (a integer, s text, p text)",
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    let events = engine.lens_events(&created.database).await.unwrap();
    use ruum_dbengine::memory::LensEvent;
    assert_eq!(events[0], LensEvent::Dropped);
    assert_eq!(events[1], LensEvent::Up { ok: true });
    assert_eq!(events[2], LensEvent::Synced);
    let LensEvent::Rebuilt(spec) = &events[3] else {
        panic!("no rebuild: {events:?}");
    };
    let class = |c: &str| {
        spec.class_of(&ruum_dbengine::ColumnRef {
            schema: "main".into(),
            table: "t".into(),
            column: c.into(),
        })
    };
    assert_eq!(
        (class("a"), class("s"), class("p")),
        (DataClass::Normal, DataClass::Secret, DataClass::Personal)
    );
}

#[tokio::test]
async fn the_statement_rules_hold_for_people_and_agents() {
    let q = setup().await;
    let viewer_agent = agent(&q.w, q.w.bob, Role::View).await;
    for c in [q.alice(), q.carol(), viewer_agent.clone()] {
        let (status, code, hint) =
            failure(q.run(&c, sql("select count(*) from main.plants")).await);
        assert_eq!((status, code.as_str()), (422, "statement_not_allowed"));
        assert_eq!(hint.as_deref(), Some(rules::HINT_UNQUALIFIED_NAMES));
        let (status, code, _) = failure(q.run(&c, sql("select 1; select 2")).await);
        assert_eq!((status, code.as_str()), (422, "validation_failed"));
        let (status, code, hint) = failure(q.run(&c, sql("begin")).await);
        assert_eq!((status, code.as_str()), (422, "statement_not_allowed"));
        assert_eq!(hint.as_deref(), Some(rules::HINT_TRANSACTION_CONTROL));
        for s in [
            "set local statement_timeout = 0",
            "reset all",
            "select set_config('a', 'b', true)",
            "pragma writable_schema = 1",
            "attach database 'x' as x",
            "select load_extension('x')",
            "select * from ruum_roles",
        ] {
            let (status, code, _) = failure(q.run(&c, sql(s)).await);
            assert_eq!(
                (status, code.as_str()),
                (422, "statement_not_allowed"),
                "{s}"
            );
        }
        // The unqualified name works.
        q.run(&c, sql("select count(*) from plants")).await.unwrap();
    }
    // A write in read mode, found by the parser.
    let (status, code, _) = failure(
        q.run(&q.alice(), sql("delete from plants where id = 2"))
            .await,
    );
    assert_eq!((status, code.as_str()), (400, "read_only_query"));
    // A write that the parser takes for a query; only the engine finds it.
    let (status, code, _) = failure(
        q.run(
            &q.alice(),
            sql("with x as (select 9 as id) insert into plants (id, name) select id, 'x' from x"),
        )
        .await,
    );
    assert_eq!((status, code.as_str()), (400, "read_only_query"));
    // Nothing changed.
    let r = q
        .run(&q.alice(), sql("select count(*) from plants"))
        .await
        .unwrap();
    assert_eq!(r.rows[0][0], json!(2));
}

#[tokio::test]
async fn agents_read_hidden_values_and_never_write() {
    let q = setup().await;
    let narrow = agent(&q.w, q.w.bob, Role::View).await;
    let r = q
        .run(
            &narrow,
            sql("select id, name, token from plants order by id"),
        )
        .await
        .unwrap();
    let hidden = json!({ "$ruum": "hidden", "class": "personal" });
    assert_eq!(
        r.rows[0],
        vec![hidden.clone(), hidden.clone(), hidden.clone()]
    );
    assert_eq!(r.rows[1][2], json!(null));
    assert!(
        r.columns
            .iter()
            .all(|c| c.data_class == DataClass::Personal && c.masking == Masking::Hidden)
    );
    // The scope comes first, then the write grant.
    let (status, code, _) = failure(
        q.run(&narrow, write("insert into plants (id) values (5)"))
            .await,
    );
    assert_eq!((status, code.as_str()), (403, "scope_exceeded"));
    let wide = agent(&q.w, q.w.bob, Role::Edit).await;
    let (status, code, _) = failure(
        q.run(&wide, write("insert into plants (id) values (5)"))
            .await,
    );
    assert_eq!((status, code.as_str()), (403, "write_grant_required"));
    let managing = agent(&q.w, q.w.alice, Role::Manage).await;
    let (status, code, _) = failure(q.run(&managing, write("drop table plants")).await);
    assert_eq!((status, code.as_str()), (403, "write_grant_required"));
}

#[tokio::test]
async fn the_log_holds_no_value_and_follows_the_visibility() {
    let q = setup().await;
    let bobs_agent = agent(&q.w, q.w.bob, Role::View).await;
    let alices_agent = agent(&q.w, q.w.alice, Role::Manage).await;
    let mut req = sql("select id from plants where name = 'Rose' and id = ?1 and 7 = 7");
    req.params = vec![json!("p-secret-value"), json!(null)];
    // Two parameters for one placeholder: the engine fails, the log still has a row.
    let _ = q.run(&q.carol(), req.clone()).await;
    let mut req2 = sql("select id from plants where name = 'Rose' and id = ?1");
    req2.params = vec![json!(1)];
    let ok = q.run(&q.carol(), req2).await.unwrap();
    q.run(&bobs_agent, sql("select name from plants where id = 1"))
        .await
        .unwrap();
    q.run(&alices_agent, sql("select 'agent literal'"))
        .await
        .unwrap();
    let _ = q.run(&q.bob(), sql("select * from main.plants")).await;

    let all = q.log(&q.alice()).await;
    // Setup (3) + 5 calls here.
    assert_eq!(all.len(), 8, "{all:#?}");
    let text = serde_json::to_string(&all.iter().map(|e| e.view()).collect::<Vec<_>>()).unwrap();
    for value in [
        "Rose",
        "p-secret-value",
        "tok-rose-secret",
        "agent literal",
        "Daisy",
    ] {
        assert!(!text.contains(value), "the log holds {value:?}: {text}");
    }
    let entry = all.iter().find(|e| e.id == ok.query_id).unwrap();
    assert_eq!(
        entry.statement,
        "select id from plants where name = $L1 and id = ?1"
    );
    assert_eq!(
        entry.params,
        vec![json!({ "$ruum": "hidden", "class": "personal" })]
    );
    assert_eq!(entry.outcome, QueryOutcome::Ok);
    assert_eq!(entry.row_count, Some(1));
    assert_eq!(
        entry.statement_hash,
        *blake3::hash(b"select id from plants where name = 'Rose' and id = ?1").as_bytes()
    );
    let refused = all.iter().find(|e| e.by.actor.id.0 == q.w.bob.0).unwrap();
    assert_eq!(refused.outcome, QueryOutcome::Error);
    assert_eq!(refused.error_code.as_deref(), Some("statement_not_allowed"));
    let failed = all
        .iter()
        .find(|e| e.by.actor.id.0 == q.carol.0 && e.outcome == QueryOutcome::Error)
        .unwrap();
    assert_eq!(
        failed.params,
        vec![
            json!({ "$ruum": "hidden", "class": "personal" }),
            json!(null)
        ]
    );

    // Carol (view): her own rows only.
    let carols = q.log(&q.carol()).await;
    assert_eq!(carols.len(), 2);
    assert!(carols.iter().all(|e| e.by.actor.id.0 == q.carol.0));
    // Bob (edit, no manage): his own and his agent's.
    let bobs = q.log(&q.bob()).await;
    assert_eq!(bobs.len(), 2);
    assert!(
        bobs.iter()
            .any(|e| e.by.actor.id == bobs_agent.actor.actor.id)
    );
    // Agents: their own only, also with manage.
    let a = q.log(&bobs_agent).await;
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].by, bobs_agent.actor);
    let m = q.log(&alices_agent).await;
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].by, alices_agent.actor);
    // The actor filter only narrows.
    let narrowed = list_queries(
        &q.w.s.databases,
        &q.carol(),
        q.db,
        ListQueries {
            actor: Some(q.w.bob.0),
            mode: None,
        },
        PageRequest::default(),
    )
    .await
    .unwrap();
    assert!(narrowed.items.is_empty());
}

#[tokio::test]
async fn limits_come_from_the_plan_and_only_go_down() {
    let q = setup().await;
    let mut req = sql("select id from plants order by id");
    req.max_rows = Some(1);
    let r = q.run(&q.alice(), req).await.unwrap();
    assert_eq!(r.row_count, 1);
    assert!(r.truncated);
    assert_eq!(r.truncated_reason, Some("max_rows"));
    let mut req = sql("select id from plants");
    req.max_rows = Some(1_000_000_000);
    let r = q.run(&q.alice(), req).await.unwrap();
    assert_eq!(
        (r.row_count, r.truncated, r.truncated_reason),
        (2, false, None)
    );
    let mut req = sql("select 1");
    req.timeout_ms = Some(50);
    assert_eq!(failure(q.run(&q.alice(), req).await).1, "validation_failed");
    let mut req = sql("select 1");
    req.max_rows = Some(0);
    assert_eq!(failure(q.run(&q.alice(), req).await).1, "validation_failed");
    let long = format!("select '{}'", "x".repeat(MAX_SQL_CHARS));
    assert_eq!(
        failure(q.run(&q.alice(), sql(&long)).await).1,
        "validation_failed"
    );
}

#[tokio::test]
async fn errors_are_sanitized_for_people_without_manage() {
    let q = setup().await;
    let dup = write("insert into plants (id, name) values (1, 'dup-value')");
    match q.run(&q.bob(), dup.clone()).await {
        Err(QueryError::Problem(p)) => {
            assert_eq!(p.sqlstate.as_deref(), Some("23505"));
            assert_eq!(p.message, "a unique constraint was violated: plants.id");
        }
        other => panic!("{other:?}"),
    }
    match q.run(&q.alice(), dup).await {
        Err(QueryError::Problem(p)) => {
            assert!(
                p.message.contains("UNIQUE constraint failed"),
                "{}",
                p.message
            )
        }
        other => panic!("{other:?}"),
    }
}

/// While the database is not ready, every kind but owner waits, then gets `503` with
/// `Retry-After`; the owner kind runs.
#[tokio::test]
async fn a_database_that_is_not_ready_answers_503_except_to_the_owner_kind() {
    let q = setup().await;
    let mut deps = q.w.s.databases.deps().clone();
    deps.queries.not_ready_wait = Duration::from_millis(300);
    let svc = DatabaseService::new(deps);
    let row =
        q.w.handles
            .typed
            .get(q.w.team.id, q.db)
            .await
            .unwrap()
            .unwrap();
    let report = |state: ProviderState, generation: i64| crate::providers::ProviderStatus {
        node_id: q.db,
        state,
        generation,
        observed_version_id: row.observed_version,
        reason: None,
        message: None,
        detail: None,
        export: None,
        at: crate::now(),
    };
    use crate::providers::store::TypedNodeStore;
    q.w.handles
        .typed
        .report(
            q.w.team.id,
            &report(ProviderState::Updating, row.generation + 10),
        )
        .await
        .unwrap();
    for c in [q.carol(), q.alice()] {
        match query(&svc, &c, q.db, sql("select count(*) from plants")).await {
            Err(QueryError::Problem(p)) => {
                assert_eq!((p.status, p.code), (503, "database_not_ready"));
                assert_eq!(p.retry_after, Some(RETRY_AFTER_SECONDS));
                assert_eq!(p.hint.as_deref(), Some("retry"));
            }
            other => panic!("{other:?}"),
        }
    }
    match schema(&svc, &q.carol(), q.db).await {
        Err(QueryError::Problem(p)) => assert_eq!(p.code, "database_not_ready"),
        other => panic!("{other:?}"),
    }
    // The owner kind (a write by a person with manage) runs.
    query(
        &svc,
        &q.alice(),
        q.db,
        write("update plants set name = 'Rosa' where id = 1"),
    )
    .await
    .unwrap();
    q.w.handles
        .typed
        .report(
            q.w.team.id,
            &report(ProviderState::Ready, row.generation + 11),
        )
        .await
        .unwrap();
    query(&svc, &q.carol(), q.db, sql("select count(*) from plants"))
        .await
        .unwrap();
}

#[tokio::test]
async fn share_links_and_strangers_get_nothing() {
    let q = setup().await;
    let stranger = ctx(crate::ids::TenantId::new(), q.w.alice);
    assert_eq!(failure(q.run(&stranger, sql("select 1")).await).0, 404);
    let link = Context {
        tenant: q.w.team.id,
        actor: Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::ShareLink,
                id: PrincipalId(Uuid::now_v7()),
            },
            on_behalf_of: None,
        },
        scope: None,
        idempotency_key: None,
    };
    assert_eq!(
        failure(q.run(&link, sql("select 1")).await).1,
        "user_required"
    );
}

// --------------------------------------------------------------- the Postgres query log

/// A tenant with a user and `n` database nodes with their `databases` rows, written as the
/// system. Returns (tenant, user, nodes).
async fn seed_pg(pool: &sqlx::PgPool, n: usize) -> (TenantId, UserId, Vec<NodeId>) {
    let (t, u, root) = (TenantId::new(), UserId::new(), NodeId::new());
    let mut tx = pool.begin().await.unwrap();
    for s in [
        "set local ruum.system = 'on'",
        "set constraints all deferred",
    ] {
        sqlx::query(s).execute(&mut *tx).await.unwrap();
    }
    sqlx::query(
        "insert into tenants (id, name, kind, plan, root_node_id, created_kind, created_by) \
         values ($1, 't', 'team', 'team', $2, 'user', $3)",
    )
    .bind(t)
    .bind(root)
    .bind(u.0)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "insert into users (id, tenant_id, email, email_verified, display_name) \
         values ($1, $2, $3, true, 'x')",
    )
    .bind(u.0)
    .bind(t)
    .bind(format!("{}@example.test", u.0))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "insert into nodes (id, tenant_id, parent_id, name, kind, owner_id, created_kind, \
         created_by, updated_kind, updated_by) values ($1, $2, null, 'root', 'folder', $3, \
         'user', $3, 'user', $3)",
    )
    .bind(root)
    .bind(t)
    .bind(u.0)
    .execute(&mut *tx)
    .await
    .unwrap();
    let mut nodes = Vec::new();
    for _ in 0..n {
        let (node, v) = (NodeId::new(), crate::ids::VersionId::new());
        let manifest = json!({ "engine": "sqlite", "version": "3", "plan": "shared-small" });
        let (bytes, hash) = crate::nodes::typed_manifest_bytes(&manifest);
        sqlx::query(
            "insert into nodes (id, tenant_id, parent_id, name, kind, type, head_version, \
             owner_id, created_kind, created_by, updated_kind, updated_by) \
             values ($1, $2, $3, $4, 'typed', 'database', $5, $6, 'user', $6, 'user', $6)",
        )
        .bind(node)
        .bind(t)
        .bind(root)
        .bind(node.to_string())
        .bind(v)
        .bind(u.0)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query(
            "insert into versions (id, tenant_id, node_id, manifest, encoding, size_bytes, \
             content_hash, actor_kind, actor) values ($1, $2, $3, $4, 'typed_manifest_v1', $5, \
             $6, 'user', $7)",
        )
        .bind(v)
        .bind(t)
        .bind(node)
        .bind(&bytes)
        .bind(bytes.len() as i64)
        .bind(hash.to_vec())
        .bind(u.0)
        .execute(&mut *tx)
        .await
        .unwrap();
        crate::providers::postgres::insert_typed_node(
            &mut tx,
            &crate::providers::store::NewTypedNode {
                tenant_id: t,
                node_id: node,
                provider: crate::databases::PROVIDER_ID.into(),
                type_name: crate::databases::NODE_TYPE.into(),
                desired_version: v,
            },
        )
        .await
        .unwrap();
        nodes.push(node);
    }
    tx.commit().await.unwrap();
    let store = crate::databases::PgDatabaseStore::new(pool.clone());
    use crate::databases::DatabaseStore;
    for node in &nodes {
        store
            .insert(
                &DatabaseRow {
                    tenant_id: t,
                    node_id: *node,
                    engine: EngineKind::Sqlite,
                    engine_version: "3".into(),
                    cluster: "memory".into(),
                    engine_name: format!("d_{}", &Uuid::now_v7().simple().to_string()[16..]),
                    branch_of: None,
                    branch_data: None,
                    key_node_id: None,
                    extensions: Vec::new(),
                    size_bytes: 0,
                },
                &[],
            )
            .await
            .unwrap();
    }
    (t, u, nodes)
}

async fn pg_conformance(pool: sqlx::PgPool) {
    let (a, user, an) = seed_pg(&pool, 2).await;
    let (b, _, bn) = seed_pg(&pool, 1).await;
    let people = log::conformance::People {
        user,
        agent: PrincipalId(Uuid::now_v7()),
        other: UserId::new(),
    };
    log::conformance::run(&PgQueryLog::new(pool), a, an[0], an[1], b, bn[0], &people).await;
}

/// The conformance suite on Postgres, with row-level security.
#[sqlx::test(migrations = "../../migrations")]
async fn postgres_log_conformance(pool: sqlx::PgPool) {
    pg_conformance(pool).await;
}

/// The same without forced row-level security on `database_queries`: only the tenant filter
/// of the statements keeps the tenants apart.
#[sqlx::test(migrations = "../../migrations")]
async fn postgres_log_filters_by_tenant_without_rls(pool: sqlx::PgPool) {
    sqlx::query("alter table database_queries no force row level security")
        .execute(&pool)
        .await
        .unwrap();
    pg_conformance(pool).await;
}

/// Row-level security: a row of tenant A cannot be written in a transaction of tenant B.
#[sqlx::test(migrations = "../../migrations")]
async fn postgres_log_rls_refuses_another_tenants_row(pool: sqlx::PgPool) {
    let (a, user, an) = seed_pg(&pool, 1).await;
    let (b, _, _) = seed_pg(&pool, 1).await;
    let e = log::conformance::entry(
        a,
        an[0],
        Actor::user(user),
        StatementMode::Read,
        crate::now(),
    );
    let mut tx = crate::journal::postgres::begin_tenant(&pool, b)
        .await
        .unwrap();
    let r = sqlx::query(
        "insert into database_queries (id, tenant_id, node_id, mode, statement, statement_hash, \
         outcome, actor_kind, actor) values ($1, $2, $3, 'read', 'x', $4, 'ok', 'user', $5)",
    )
    .bind(e.id)
    .bind(a)
    .bind(an[0])
    .bind(e.statement_hash.to_vec())
    .bind(user.0)
    .execute(&mut *tx)
    .await;
    assert!(r.is_err(), "RLS let tenant B write a row of tenant A");
}

// ------------------------------------------------------------- the Postgres engine

/// On the Postgres engine (`RUUM_DBHOST_URL`): a viewer's filter on a `secret` column compares
/// the masked value (the `nosecret` lens), a manager's filter the real one; a manager's `ALTER
/// COLUMN ... TYPE` through `queryDatabase` works and the lens is rebuilt (wave decision 3).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_engine_masks_secrets_for_viewers_and_takes_a_managers_retype() {
    let required = std::env::var_os("CI").is_some()
        || std::env::var_os("RUUM_DBENGINE_REQUIRE_POSTGRES").is_some();
    let Some(config) = ruum_dbengine::postgres::PostgresConfig::from_env() else {
        assert!(
            !required,
            "RUUM_DBHOST_URL is not set: this test needs the data cluster"
        );
        eprintln!("skipped: RUUM_DBHOST_URL is not set");
        return;
    };
    let engine = match ruum_dbengine::postgres::PostgresEngine::connect(config).await {
        Ok(e) => Arc::new(e) as Arc<dyn DatabaseEngine>,
        Err(e) => {
            assert!(!required, "the data cluster: {e}");
            eprintln!("skipped: the data cluster does not answer: {e}");
            return;
        }
    };
    let mut w = world().await;
    let parts = DatabaseParts {
        engines: Engines {
            postgres: Some(engine.clone()),
            sqlite: None,
        },
        keys: w.s.tenant_keys.clone(),
        store: w.handles.databases.clone(),
        typed: w.handles.typed.clone(),
        columns: w.s.databases.deps().columns.clone(),
        masking: w.s.databases.deps().masking.clone(),
        grants: w.s.databases.deps().grants.clone(),
        queries: Arc::new(MemoryQueryLog::new()),
    };
    install(&mut w.s, parts).unwrap();
    let a = ctx(w.team.id, w.alice);
    let b = ctx(w.team.id, w.bob);
    let base = json!({ "engine": "postgres", "version": "17", "plan": "shared-small" });
    let db =
        w.s.databases
            .create(
                &a,
                CreateDatabase {
                    parent_id: w.folder,
                    name: "Accounts".into(),
                    manifest: base.clone(),
                    source: None,
                    on_conflict: OnConflict::Fail,
                },
            )
            .await
            .unwrap()
            .value
            .node
            .id;
    settled(&w.s, &a, db).await;
    // The classes first, so the lens of the schema change masks the token.
    let mut manifest = base.clone();
    manifest["data_classes"] = json!({
        "public.accounts.id": "normal",
        "public.accounts.token": "secret",
    });
    w.s.databases
        .update_manifest(
            &a,
            db,
            UpdateManifest {
                manifest,
                label: None,
            },
            None,
        )
        .await
        .unwrap();
    let svc = &w.s.databases;
    let run = |c: Context, r: QueryRequest| async move { query(svc, &c, db, r).await };
    run(
        a.clone(),
        write("create table accounts (id integer primary key, token text)"),
    )
    .await
    .unwrap();
    run(
        a.clone(),
        write("insert into accounts (id, token) values (1, 'tok-real-secret')"),
    )
    .await
    .unwrap();
    let count = |r: QueryResult| r.rows[0][0].clone();
    let filter_real = || {
        let mut r = sql("select count(*) from accounts where token = $1");
        r.params = vec![json!("tok-real-secret")];
        r
    };
    let filter_masked = || {
        let mut r = sql("select count(*) from accounts where token = $1");
        r.params = vec![json!(ruum_dbengine::postgres::lens::SECRET_CONSTANT)];
        r
    };
    // Bob (edit, no manage) reads through the nosecret lens: his filter sees the constant.
    assert_eq!(
        count(run(b.clone(), filter_real()).await.unwrap()),
        json!(0)
    );
    assert_eq!(
        count(run(b.clone(), filter_masked()).await.unwrap()),
        json!(1)
    );
    let r = run(b.clone(), sql("select id, token from accounts"))
        .await
        .unwrap();
    assert_eq!(
        r.rows[0][1],
        json!({ "$ruum": "hidden", "class": "secret" })
    );
    // Alice (manage) reads the real value.
    assert_eq!(
        count(run(a.clone(), filter_real()).await.unwrap()),
        json!(1)
    );
    // A retype while the lens views exist: the migration path drops and rebuilds them.
    run(
        a.clone(),
        write("alter table accounts alter column id type bigint"),
    )
    .await
    .unwrap();
    let r = run(b.clone(), sql("select id, token from accounts"))
        .await
        .unwrap();
    assert_eq!(r.columns[0].type_name, "bigint");
    assert_eq!(
        r.rows[0][1],
        json!({ "$ruum": "hidden", "class": "secret" })
    );
    assert_eq!(
        count(run(b.clone(), filter_real()).await.unwrap()),
        json!(0)
    );
    // A qualified name is refused also where the manager could read it.
    let (status, code, hint) =
        failure(run(a.clone(), sql("select count(*) from public.accounts")).await);
    assert_eq!((status, code.as_str()), (422, "statement_not_allowed"));
    assert_eq!(hint.as_deref(), Some(rules::HINT_UNQUALIFIED_NAMES));
    // set_config through a function that runs a query given as text (review Q-1, S-1): the
    // statement rules let it through (the text is a string literal), the engine refuses it for
    // every kind but owner, and the answer is the fixed text. An agent reads as masked_read.
    let bot = agent(&w, w.bob, Role::Edit).await;
    for text in [
        "select query_to_xml('select set_config(''work_mem'', ''7MB'', true)', false, false, '')",
        "select query_to_xml(U&'select set\\005fconfig(''statement_timeout'', ''0'', true)', \
         false, false, '')",
        "select * from ts_stat('select to_tsvector(set_config(''work_mem'', ''7MB'', true))')",
    ] {
        assert!(
            rules::analyze(EngineKind::Postgres, text).verdict.is_ok(),
            "{text}"
        );
        for (who, req) in [
            (b.clone(), sql(text)),
            (b.clone(), write(text)),
            (bot.clone(), sql(text)),
        ] {
            let r = run(who, req).await;
            let Err(QueryError::Problem(p)) = r else {
                panic!("{text}: {r:?}");
            };
            assert_eq!((p.status, p.code), (400, "query_failed"), "{text}");
            assert_eq!(p.sqlstate.as_deref(), Some("42501"), "{text}");
        }
    }
    let s = schema(svc, &b, db).await.unwrap();
    assert_eq!(s.engine, EngineKind::Postgres);
    assert!(s.tables.iter().all(|t| t.schema == "public"));
    let accounts = s.tables.iter().find(|t| t.name == "accounts").unwrap();
    assert_eq!(accounts.columns[0].type_name, "bigint");
    w.s.databases
        .deps()
        .engines
        .postgres
        .as_ref()
        .unwrap()
        .drop_database(
            &w.s.databases
                .deps()
                .store
                .get(w.team.id, db)
                .await
                .unwrap()
                .unwrap()
                .database_ref(),
        )
        .await
        .unwrap();
}
