//! Hostile tests of the privacy filter on the Postgres engine (card Q-2; `RUUM_DBHOST_URL`;
//! skipped without it, except in CI or with `RUUM_DBENGINE_REQUIRE_POSTGRES`). Each test makes
//! its own database on the data cluster and drops it at the end.
//!
//! The leak paths: every clause of an agent's read (decision 0004-3), expressions, aggregates and
//! joins; the hidden-value marker of `secret` columns and of `personal` columns of a
//! tenant-defined type (0005 B-2, 0004-37 a); a tenant view over a `secret` column (review-Q-1
//! N-9); the `read_personal` grant; agent writes (`write_live`, `class_read_required`, the
//! destructive statements, the engine's own line); the lens after a change of classes and after
//! a drift of the manifest (review-K-1 C-1); dynamic SQL in a string (`query_to_xml`, review-Q-1
//! S-1); `pg_stats` and `EXPLAIN`.

use super::super::*;
use crate::actor::{PrincipalKind, PrincipalRef};
use crate::agents::{AgentScope, Budget, NewAgent};
use crate::databases::access::grants::{CreateGrant, GrantKind};
use crate::databases::classes::{ConfirmItem, ConfirmRequest};
use crate::databases::tests::{World, ctx, world};
use crate::databases::{CreateDatabase, DatabaseParts, Engines, UpdateManifest, install};
use crate::ids::{AgentId, UserId};
use crate::nodes::OnConflict;
use crate::providers::ProviderState;
use serde_json::{Value, json};

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

/// The Postgres engine of the data cluster, or `None` (the test is skipped).
async fn engine() -> Option<Arc<dyn DatabaseEngine>> {
    let required = std::env::var_os("CI").is_some()
        || std::env::var_os("RUUM_DBENGINE_REQUIRE_POSTGRES").is_some();
    let Some(config) = ruum_dbengine::postgres::PostgresConfig::from_env() else {
        assert!(
            !required,
            "RUUM_DBHOST_URL is not set: this test needs the data cluster"
        );
        eprintln!("skipped: RUUM_DBHOST_URL is not set");
        return None;
    };
    match ruum_dbengine::postgres::PostgresEngine::connect(config).await {
        Ok(e) => Some(Arc::new(e) as Arc<dyn DatabaseEngine>),
        Err(e) => {
            assert!(!required, "the data cluster: {e}");
            eprintln!("skipped: the data cluster does not answer: {e}");
            None
        }
    }
}

struct Pg {
    w: World,
    db: NodeId,
    agent: AgentId,
    scope: crate::authz::Scope,
}

const PEOPLE: usize = 4;

impl Pg {
    fn alice(&self) -> Context {
        ctx(self.w.team.id, self.w.alice)
    }

    fn bob(&self) -> Context {
        ctx(self.w.team.id, self.w.bob)
    }

    fn agent(&self) -> Context {
        Context {
            tenant: self.w.team.id,
            actor: Actor {
                actor: PrincipalRef {
                    kind: PrincipalKind::Agent,
                    id: self.agent.into(),
                },
                on_behalf_of: Some(self.w.alice),
            },
            scope: Some(self.scope.clone()),
            idempotency_key: None,
        }
    }

    async fn run(
        &self,
        c: &Context,
        r: QueryRequest,
    ) -> std::result::Result<QueryResult, QueryError> {
        query_at(&self.w.s.databases, c, self.db, r, "https://ruum.test").await
    }

    async fn ok(&self, c: &Context, r: QueryRequest) -> QueryResult {
        let text = r.sql.clone();
        match self.run(c, r).await {
            Ok(v) => v,
            Err(e) => panic!("{text}: {e:?}"),
        }
    }

    /// The status, code and hint of a call that must fail.
    async fn refused(&self, c: &Context, r: QueryRequest) -> (u16, String, Option<String>) {
        let text = r.sql.clone();
        match self.run(c, r).await {
            Ok(v) => panic!("{text} succeeded: {:?}", v.rows),
            Err(QueryError::Problem(p)) => {
                (p.code.http_status(), p.code.as_str().to_string(), p.hint)
            }
            Err(QueryError::Core(e)) => (e.http_status(), e.to_api().code.as_str().into(), None),
        }
    }

    async fn count(&self, c: &Context, text: &str) -> Value {
        self.ok(c, sql(text)).await.rows[0][0].clone()
    }

    async fn grant(&self, kind: GrantKind) {
        self.w
            .s
            .databases
            .create_grant(
                &self.alice(),
                &*self.w.s.agents,
                self.db,
                CreateGrant {
                    kind,
                    agent_id: self.agent,
                    duration_minutes: 60,
                    reason: Some("privacy test".into()),
                },
            )
            .await
            .unwrap();
    }

    async fn confirm(&self, items: &[(&str, &str, DataClass)]) {
        self.w
            .s
            .databases
            .confirm_columns(
                &self.alice(),
                "https://ruum.test",
                self.db,
                ConfirmRequest {
                    columns: items
                        .iter()
                        .map(|(t, c, k)| ConfirmItem {
                            schema: "public".into(),
                            table: t.to_string(),
                            column: c.to_string(),
                            class: *k,
                        })
                        .collect(),
                },
                None,
            )
            .await
            .unwrap();
    }

    async fn drop(self) {
        let d = self.w.s.databases.deps();
        let row = d.store.get(self.w.team.id, self.db).await.unwrap().unwrap();
        d.engines
            .postgres
            .as_ref()
            .unwrap()
            .drop_database(&row.database_ref())
            .await
            .unwrap();
    }
}

async fn register_agent(w: &World, user: UserId) -> crate::agents::Agent {
    w.s.agents
        .register(
            &ctx(w.team.id, user),
            NewAgent {
                name: format!("agent {}", Uuid::now_v7().simple()),
                description: None,
                scope: AgentScope {
                    subtree: None,
                    max_role: Role::Manage,
                    expires_at: None,
                    budget: Budget::default(),
                },
            },
        )
        .await
        .unwrap()
}

/// A Postgres database with `people`, `orders`, a tenant enum, a view over a secret column and a
/// view without one; the classes confirmed by Alice (`manage`).
async fn setup() -> Option<Pg> {
    let engine = engine().await?;
    let mut w = world().await;
    let parts = DatabaseParts {
        engines: Engines {
            postgres: Some(engine),
            sqlite: None,
        },
        keys: w.s.tenant_keys.clone(),
        store: w.handles.databases.clone(),
        typed: w.handles.typed.clone(),
        columns: w.s.databases.deps().columns.clone(),
        masking: Arc::new(crate::privacy::RecordingMaskingRules::new()),
        grants: w.s.databases.deps().grants.clone(),
        queries: Arc::new(MemoryQueryLog::new()),
    };
    install(&mut w.s, parts).unwrap();
    let a = ctx(w.team.id, w.alice);
    let db = w
        .s
        .databases
        .create(
            &a,
            CreateDatabase {
                parent_id: w.folder,
                name: format!("People {}", Uuid::now_v7().simple()),
                manifest: json!({ "engine": "postgres", "version": "17", "plan": "shared-small" }),
                source: None,
                on_conflict: OnConflict::Fail,
            },
        )
        .await
        .unwrap()
        .value
        .node
        .id;
    // The data cluster can be slow while other suites run: wait up to 60 s.
    for i in 0.. {
        let d = w.s.databases.get(&a, db).await.unwrap();
        if d.status.state != ProviderState::Provisioning {
            break;
        }
        assert!(i < 600, "database {db} is still provisioning after 60 s");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let agent = register_agent(&w, w.alice).await;
    let pg = Pg {
        db,
        agent: agent.id,
        scope: agent.scope.as_scope(),
        w,
    };
    for s in [
        "create type mood as enum ('happy', 'sad')",
        "create table people (id integer primary key, email text not null unique, name text, \
         token text, nick text, score integer not null check (score >= 0), feeling mood)",
        "create table orders (id integer primary key, person_email text, name_copy text, \
         amount integer, label text)",
        "create view v as select id, upper(token) as t, name from people",
        "create view safe as select id, name from people",
        "create function peek(i integer) returns text language sql stable security definer \
         set search_path = public as $$ select token from people where id = i $$",
        "create view w as select id, peek(id) as p from people",
    ] {
        pg.ok(&a, write(s)).await;
    }
    for i in 1..=PEOPLE {
        let token = if i == PEOPLE {
            "null".to_string()
        } else {
            format!("'tok-{i}-secret'")
        };
        pg.ok(
            &a,
            write(&format!(
                "insert into people values ({i}, 'a{i}@example.test', 'Name {i}', {token}, \
                 'nick-{i}', {score}, 'happy')",
                score = 10 * i
            )),
        )
        .await;
        pg.ok(
            &a,
            write(&format!(
                "insert into orders values ({i}, 'a{i}@example.test', 'Name {i}', {i}, 'order-{i}')"
            )),
        )
        .await;
    }
    use DataClass::{Normal as N, Personal as P, Secret as S};
    pg.confirm(&[
        ("people", "id", N),
        ("people", "email", P),
        ("people", "name", P),
        ("people", "token", S),
        ("people", "nick", N),
        ("people", "score", N),
        ("people", "feeling", P),
        ("orders", "id", N),
        ("orders", "person_email", P),
        ("orders", "name_copy", N),
        ("orders", "amount", N),
        ("orders", "label", N),
        ("v", "id", N),
        ("safe", "id", N),
    ])
    .await;
    Some(pg)
}

fn hidden(class: &str) -> Value {
    json!({ "$ruum": "hidden", "class": class })
}

fn is_pseudonym_marker(v: &Value) -> bool {
    v["$ruum"] == "pseudonym"
        && v["id"].as_str().is_some_and(postgres_mask::is_pseudonym)
        && v.as_object().is_some_and(|m| m.len() == 2)
}

/// No raw value of `people` in an answer.
fn no_raw(r: &QueryResult) {
    let text = serde_json::to_string(&r.rows).unwrap();
    for raw in ["@example.test", "tok-", "TOK-", "Name ", "happy"] {
        assert!(!text.contains(raw), "{raw} in {text}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agents_read_markers_in_every_clause_and_never_a_value() {
    let Some(pg) = setup().await else { return };
    let agent = pg.agent();
    // Plain columns: a pseudonym, the secret marker, the personal marker of a tenant type.
    let r = pg
        .ok(
            &agent,
            sql("select id, email, token, feeling, score, nick from people order by id"),
        )
        .await;
    no_raw(&r);
    let class = |i: usize| (r.columns[i].data_class, r.columns[i].masking);
    assert_eq!(class(0), (DataClass::Normal, Masking::None));
    assert_eq!(class(1), (DataClass::Personal, Masking::Pseudonym));
    assert_eq!(class(2), (DataClass::Secret, Masking::Hidden));
    assert_eq!(class(3), (DataClass::Personal, Masking::Hidden));
    assert_eq!(r.columns[1].type_name, "text");
    for (i, row) in r.rows.iter().enumerate() {
        assert_eq!(row[0], json!(i + 1));
        assert!(is_pseudonym_marker(&row[1]), "{}", row[1]);
        if i + 1 == PEOPLE {
            assert_eq!(row[2], json!(null), "a NULL secret stays NULL");
        } else {
            assert_eq!(row[2], hidden("secret"));
        }
        assert_eq!(
            row[3],
            hidden("personal"),
            "a tenant type is never a pseudonym"
        );
        assert_eq!(row[4], json!(10 * (i + 1)));
        assert_eq!(row[5], json!(format!("nick-{}", i + 1)));
    }
    // Expressions take the strictest class they read; constants and parameters stay as sent.
    let mut req = sql(
        "select upper(token), lower(email), length(email), coalesce(name, 'x'), 'guess', $1, \
         count(*) over () from people where id = 1",
    );
    req.params = vec![json!("guess-2")];
    let r = pg.ok(&agent, req).await;
    no_raw(&r);
    assert_eq!(r.rows[0][0], hidden("secret"));
    assert!(is_pseudonym_marker(&r.rows[0][1]), "{}", r.rows[0][1]);
    assert_eq!(r.rows[0][2], json!(14), "the length of a pseudonym");
    assert!(is_pseudonym_marker(&r.rows[0][3]));
    assert_eq!(r.rows[0][4], json!("guess"));
    assert_eq!(r.rows[0][5], json!("guess-2"));
    assert_eq!(r.rows[0][6], json!(1));
    let classes: Vec<_> = r.columns.iter().map(|c| c.data_class).collect();
    use DataClass::{Normal as N, Personal as P, Secret as S};
    assert_eq!(classes, vec![S, P, P, P, N, N, N]);
    assert_eq!(r.columns[4].masking, Masking::None);
    // Every clause compares pseudonyms or the constant (the person's control first).
    let all = json!(PEOPLE);
    for (text, person, agent_n) in [
        (
            "select count(*) from people where email like 'a%'",
            all.clone(),
            json!(0),
        ),
        (
            "select count(*) from people where email like 'p\\_%'",
            json!(0),
            all.clone(),
        ),
        (
            "select count(*) from people where token = 'tok-1-secret'",
            json!(1),
            json!(0),
        ),
        (
            "select count(*) from people where token like 'tok%'",
            json!(3),
            json!(0),
        ),
        (
            "select count(*) from people where feeling = 'happy'",
            all.clone(),
            json!(0),
        ),
        (
            "select count(*) from people p join orders o on o.name_copy = p.name",
            all.clone(),
            json!(0),
        ),
        (
            "select count(*) from people p where exists (select 1 from orders o where o.name_copy = p.name)",
            all.clone(),
            json!(0),
        ),
        (
            "select count(*) from orders o join people p on p.email = o.person_email",
            all.clone(),
            all.clone(),
        ),
        (
            "select count(*) from (select min(email) m from people) s where m like 'a%'",
            json!(1),
            json!(0),
        ),
    ] {
        assert_eq!(pg.count(&pg.alice(), text).await, person, "alice: {text}");
        assert_eq!(pg.count(&agent, text).await, agent_n, "agent: {text}");
    }
    // A parameter with the real value finds nothing.
    let mut req = sql("select count(*) from people where email = $1");
    req.params = vec![json!("a1@example.test")];
    assert_eq!(pg.ok(&agent, req).await.rows[0][0], json!(0));
    // ORDER BY and GROUP BY see pseudonyms.
    let r = pg
        .ok(
            &agent,
            sql("select email, count(*) from people group by email order by email"),
        )
        .await;
    no_raw(&r);
    let ids: Vec<String> = r
        .rows
        .iter()
        .map(|row| row[0]["id"].as_str().unwrap().to_string())
        .collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);
    // Dynamic SQL in a string reaches neither the base tables nor the key (review-Q-1 S-1).
    for text in [
        "select query_to_xml('select email from public.people', false, false, '')",
        "select query_to_xml('select * from ruum_mask.key', false, false, '')",
        "select query_to_xml('select ruum_mask.pseudonym(''x''::text)', false, false, '')",
    ] {
        let (status, code, _) = pg.refused(&agent, sql(text)).await;
        assert_eq!((status, code.as_str()), (400, "query_failed"), "{text}");
    }
    // A direct call by name: refused by the rules, or no such function.
    for text in [
        "select pseudonym('x'::text)",
        "select ruum_mask.pseudonym('x'::text)",
    ] {
        let (status, _, _) = pg.refused(&agent, sql(text)).await;
        assert!(status == 400 || status == 422, "{text}: {status}");
    }
    // Side channels.
    // Statistics, made in an owner session of the engine (the query API refuses ANALYZE).
    {
        let d = pg.w.s.databases.deps();
        let row = d.store.get(pg.w.team.id, pg.db).await.unwrap().unwrap();
        let key = d
            .queries
            .keys
            .engine_key(pg.w.team.id, row.key_node_id)
            .await
            .unwrap();
        let owner = RoleRef::new(row.database_ref(), RoleKind::Owner).unwrap();
        d.engines
            .postgres
            .as_ref()
            .unwrap()
            .execute(
                &owner,
                &key,
                &StatementRequest {
                    sql: "analyze people".into(),
                    params: vec![],
                    mode: StatementMode::Write,
                    timeout: Duration::from_secs(10),
                    max_rows: 10,
                    max_bytes: None,
                },
            )
            .await
            .unwrap();
    }
    assert_ne!(
        pg.count(
            &pg.alice(),
            "select count(*) from pg_stats where tablename = 'people'"
        )
        .await,
        json!(0),
        "control: the owner sees the statistics"
    );
    assert_eq!(
        pg.count(
            &agent,
            "select count(*) from pg_stats where tablename = 'people'"
        )
        .await,
        json!(0)
    );
    let r = pg
        .ok(
            &agent,
            sql("explain (verbose, costs off) select * from people p join orders o on o.name_copy = p.name"),
        )
        .await;
    no_raw(&r);
    assert_eq!(r.columns[0].data_class, DataClass::Normal);
    pg.drop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tenant_view_over_a_secret_hides_its_columns_from_viewers_and_agents() {
    let Some(pg) = setup().await else { return };
    // `v` reads `token` (secret): its unconfirmed columns are masked like secret, for Bob
    // (edit, no manage: the nosecret lens) and for the agent; `v.id` stays normal.
    for who in [pg.bob(), pg.agent()] {
        let r = pg
            .ok(&who, sql("select id, t, name from v order by id"))
            .await;
        no_raw(&r);
        assert_eq!(r.rows[0][0], json!(1));
        // The masked lens marks a personal column that it hides as `personal` (it cannot tell a
        // view over a secret from a tenant type); the nosecret lens hides only secrets.
        for i in [1, 2] {
            let c = &r.rows[0][i];
            assert!(c == &hidden("secret") || c == &hidden("personal"), "{c}");
        }
        assert_eq!(r.rows[PEOPLE - 1][1], json!(null));
        assert_eq!(r.columns[1].masking, Masking::Hidden);
        assert_eq!(
            pg.count(&who, "select count(*) from v where t = 'TOK-1-SECRET'")
                .await,
            json!(0)
        );
        assert_eq!(
            pg.count(&who, "select count(*) from v where t like 'TOK%'")
                .await,
            json!(0)
        );
    }
    assert_eq!(
        pg.count(
            &pg.alice(),
            "select count(*) from v where t = 'TOK-1-SECRET'"
        )
        .await,
        json!(1)
    );
    // `w` calls a SECURITY DEFINER function of the tenant, which reads `token` with the
    // owner's rights: pg_depend cannot tell what it reads, so `w.p` is masked like secret.
    for who in [pg.bob(), pg.agent()] {
        let r = pg.ok(&who, sql("select id, p from w order by id")).await;
        no_raw(&r);
        let c = &r.rows[0][1];
        assert!(c == &hidden("secret") || c == &hidden("personal"), "{c}");
        assert_eq!(
            pg.count(&who, "select count(*) from w where p like 'tok%'")
                .await,
            json!(0)
        );
    }
    assert_eq!(
        pg.count(&pg.alice(), "select count(*) from w where p like 'tok%'")
            .await,
        json!(3)
    );
    // `safe` reads no secret: a pseudonym for the agent, the value for Bob.
    let r = pg
        .ok(&pg.agent(), sql("select name from safe where id = 1"))
        .await;
    assert!(is_pseudonym_marker(&r.rows[0][0]), "{}", r.rows[0][0]);
    let r = pg
        .ok(&pg.bob(), sql("select name from safe where id = 1"))
        .await;
    assert_eq!(r.rows[0][0], json!("Name 1"));
    // Bob never reads a secret, also not through an expression.
    let r = pg
        .ok(
            &pg.bob(),
            sql("select upper(token), length(token), email from people where id = 1"),
        )
        .await;
    assert_eq!(r.rows[0][0], hidden("secret"));
    assert_eq!(r.rows[0][1], hidden("secret"));
    assert_eq!(r.rows[0][2], json!("a1@example.test"));
    pg.drop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_writes_need_a_grant_and_never_touch_a_restricted_column() {
    let Some(pg) = setup().await else { return };
    let agent = pg.agent();
    // No write_live grant: 403 with the grant prompt of the agent's user (IDs only).
    let (status, code, hint) = pg
        .refused(&agent, write("update people set score = 1 where id = 1"))
        .await;
    assert_eq!((status, code.as_str()), (403, "write_grant_required"));
    let hint = hint.unwrap();
    assert!(
        hint.starts_with(&format!(
            "https://ruum.test/t/{}/item/{}?give=write_live&agent={}",
            pg.w.team.id, pg.db, pg.agent
        )),
        "{hint}"
    );
    pg.grant(GrantKind::WriteLive).await;
    // Plain columns: allowed (masked_write).
    let r = pg
        .ok(
            &agent,
            write("update people set score = score + 1 where id = 1"),
        )
        .await;
    assert_eq!(r.rows_affected, Some(1));
    pg.ok(
        &agent,
        write("insert into orders (id, label, amount) values (50, 'ok', 1)"),
    )
    .await;
    // Reads or writes of a personal or secret column: 403 class_read_required.
    for text in [
        "update people set nick = email where id = 1",
        "update people set score = 1 where id = 1 returning email",
        "update people set score = 1 where email = 'a1@example.test'",
        "update people set score = 1 where token is not null and id = 1",
        "insert into orders (id, label) select id + 100, email from people",
        "insert into orders values (60, 'x', 'y', 1, 'z')",
        "insert into orders (id, person_email) values (61, 'x')",
        "delete from people where name = 'Name 1'",
        "with x as (select email from people) update orders set label = (select min(email) from x) where id = 1",
        // Review-Q-2-2 P-3: a whole row and functional notation read columns too.
        "update orders set label = (select p::text from people p where p.id = 1) where id = 1",
        "update orders set label = email(p) from people p where p.id = orders.id and orders.id = 2",
        "update orders set label = token(p) from people p where p.id = orders.id and orders.id = 3",
        "update orders set label = upper(score) from people p(id, score, x, y, z, w) where p.id = orders.id and orders.id = 4",
    ] {
        let (status, code, hint) = pg.refused(&agent, write(text)).await;
        assert_eq!(
            (status, code.as_str()),
            (403, "class_read_required"),
            "{text}"
        );
        assert!(hint.is_some(), "{text}");
    }
    let (_, _, hint) = pg
        .refused(&agent, write("update people set nick = email where id = 1"))
        .await;
    assert!(hint.unwrap().contains("give=read_personal"));
    // Destructive statements never run at once (approvals are card A-2).
    for text in [
        "delete from orders",
        "update orders set amount = 0",
        "truncate orders",
        "drop table orders",
        "alter table orders add column extra text",
    ] {
        let (status, code, _) = pg.refused(&agent, write(text)).await;
        assert_eq!((status, code.as_str()), (501, "not_implemented"), "{text}");
    }
    assert_eq!(
        pg.count(&pg.alice(), "select count(*) from orders").await,
        json!(PEOPLE + 1)
    );
    // The values did not move.
    assert_eq!(
        pg.count(
            &pg.alice(),
            "select count(*) from people where nick like 'nick-%'"
        )
        .await,
        json!(PEOPLE)
    );
    assert_eq!(
        pg.count(
            &pg.alice(),
            "select count(*) from orders where label like 'order-%' and id <= 4"
        )
        .await,
        json!(PEOPLE),
        "no pseudonym or lens constant was stored as a normal value"
    );
    // An engine error is sanitized: no row value.
    let r = pg
        .run(&agent, write("update people set score = -1 where id = 1"))
        .await;
    let Err(QueryError::Problem(p)) = r else {
        panic!("{r:?}")
    };
    assert_eq!(
        (p.code.as_str(), p.sqlstate.as_deref()),
        ("query_failed", Some("23514"))
    );
    assert!(
        !p.message.contains("Failing row") && !p.message.contains("@example"),
        "{}",
        p.message
    );
    // The engine holds the line by itself: a masked_write session cannot write a masked column.
    let d = pg.w.s.databases.deps();
    let row = d.store.get(pg.w.team.id, pg.db).await.unwrap().unwrap();
    let key = d
        .queries
        .keys
        .engine_key(pg.w.team.id, row.key_node_id)
        .await
        .unwrap();
    let engine = d.engines.postgres.as_ref().unwrap();
    for text in [
        "update people set email = 'x@y' where id = 1",
        "update people set token = 'x' where id = 1",
        "update people set feeling = 'sad' where id = 1",
    ] {
        let role = RoleRef::new(row.database_ref(), RoleKind::MaskedWrite).unwrap();
        let r = engine
            .execute(
                &role,
                &key,
                &StatementRequest {
                    sql: text.into(),
                    params: vec![],
                    mode: StatementMode::Write,
                    timeout: Duration::from_secs(5),
                    max_rows: 10,
                    max_bytes: None,
                },
            )
            .await;
        assert!(r.is_err(), "the engine let {text} through");
    }
    // With read_personal too: personal columns may be written, secret ones never.
    pg.grant(GrantKind::ReadPersonal).await;
    pg.ok(&agent, write("update people set nick = name where id = 2"))
        .await;
    let (status, code, hint) = pg
        .refused(&agent, write("update people set nick = token where id = 2"))
        .await;
    assert_eq!((status, code.as_str()), (403, "class_read_required"));
    assert!(!hint.unwrap().contains("give="), "no grant shows a secret");
    // The log names the grant of each agent write.
    let log = list_queries(
        &pg.w.s.databases,
        &agent,
        pg.db,
        ListQueries::default(),
        PageRequest::default(),
    )
    .await
    .unwrap()
    .items;
    assert!(
        log.iter()
            .any(|e| e.grant_id.is_some() && e.outcome == QueryOutcome::Ok)
    );
    // A person without manage never writes a secret either.
    let (status, code, _) = pg
        .refused(
            &pg.bob(),
            write("update people set token = 'x' where id = 1"),
        )
        .await;
    assert_eq!((status, code.as_str()), (403, "class_read_required"));
    pg.drop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_personal_shows_personal_values_and_never_a_secret() {
    let Some(pg) = setup().await else { return };
    let agent = pg.agent();
    let perms = || async {
        pg.w.s
            .databases
            .get(&agent, pg.db)
            .await
            .unwrap()
            .my_permissions
            .unwrap()
    };
    let p = perms().await;
    assert!(!p.read_personal && !p.write_live && !p.query_write && !p.read_secret);
    pg.grant(GrantKind::ReadPersonal).await;
    // getDatabase counts the grants of an agent (review-A-1 N-2).
    let p = perms().await;
    assert!(p.read_personal && !p.write_live && !p.read_secret);
    pg.grant(GrantKind::WriteLive).await;
    let p = perms().await;
    assert!(p.write_live && p.query_write && !p.read_secret);
    let r = pg
        .ok(
            &agent,
            sql("select email, token, feeling from people where id = 1"),
        )
        .await;
    assert_eq!(r.rows[0][0], json!("a1@example.test"));
    assert_eq!(r.rows[0][1], hidden("secret"));
    assert_eq!(r.rows[0][2], json!("happy"));
    assert_eq!(r.columns[0].masking, Masking::None);
    assert_eq!(
        pg.count(
            &agent,
            "select count(*) from people where token like 'tok%'"
        )
        .await,
        json!(0)
    );
    assert_eq!(
        pg.count(&agent, "select count(*) from people where email like 'a%'")
            .await,
        json!(PEOPLE)
    );
    pg.drop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_lens_follows_confirmations_and_the_stricter_of_store_and_manifest() {
    let Some(pg) = setup().await else { return };
    let agent = pg.agent();
    let nick = || async {
        pg.ok(&agent, sql("select nick from people where id = 1"))
            .await
            .rows[0][0]
            .clone()
    };
    assert_eq!(nick().await, json!("nick-1"));
    // A confirmation rebuilds the lens (MaskingRules::classes_changed).
    pg.confirm(&[("people", "nick", DataClass::Secret)]).await;
    assert_eq!(nick().await, hidden("secret"));
    assert_eq!(
        pg.count(
            &agent,
            "select count(*) from people where nick like 'nick%'"
        )
        .await,
        json!(0)
    );
    pg.confirm(&[("people", "nick", DataClass::Normal)]).await;
    assert_eq!(nick().await, json!("nick-1"));
    // The manifest takes `score` back without the store (review-K-1 C-1): the stricter of the
    // two counts, in the lens and in the markers.
    let d = pg.w.s.databases.deps();
    let typed = d.typed.get(pg.w.team.id, pg.db).await.unwrap().unwrap();
    let mut manifest = d
        .store
        .manifest(pg.w.team.id, typed.desired_version)
        .await
        .unwrap()
        .unwrap();
    manifest["data_classes"]
        .as_object_mut()
        .unwrap()
        .remove("public.people.score");
    // Review-Q-2-1 F-2: and makes `nick` secret. No manual rebuild: the manifest write itself
    // rebuilds the lens (`applyManifest` -> `classes_changed`).
    manifest["data_classes"]
        .as_object_mut()
        .unwrap()
        .insert("public.people.nick".into(), json!("secret"));
    pg.w.s
        .databases
        .update_manifest(
            &pg.alice(),
            pg.db,
            UpdateManifest {
                manifest,
                label: None,
            },
            None,
        )
        .await
        .unwrap();
    let r = pg
        .ok(&agent, sql("select score from people where id = 1"))
        .await;
    assert_eq!(r.columns[0].data_class, DataClass::Personal);
    assert!(is_pseudonym_marker(&r.rows[0][0]), "{}", r.rows[0][0]);
    // The filters see the new lens at once: a raw value of a column taken back finds nothing,
    // and a column made secret is the constant, for the agent and for Bob (no manage).
    for who in [pg.agent(), pg.bob()] {
        assert_eq!(
            pg.count(&who, "select count(*) from people where score::text = '10'")
                .await,
            json!(if who.actor.is_agent() { 0 } else { 1 }),
        );
        assert_eq!(
            pg.count(&who, "select count(*) from people where nick = 'nick-1'")
                .await,
            json!(0)
        );
        let r = pg
            .ok(
                &who,
                sql("select nick, nick || '' from people where id = 1"),
            )
            .await;
        assert_eq!(r.rows[0][0], hidden("secret"), "{:?}", r.rows);
        assert_eq!(r.rows[0][1], hidden("secret"), "{:?}", r.rows);
    }
    pg.drop().await;
}

/// Runs a statement in an `owner` session of the engine (statements the query API refuses, such
/// as `ANALYZE`).
async fn as_owner(pg: &Pg, text: &str) {
    let d = pg.w.s.databases.deps();
    let row = d.store.get(pg.w.team.id, pg.db).await.unwrap().unwrap();
    let key = d
        .queries
        .keys
        .engine_key(pg.w.team.id, row.key_node_id)
        .await
        .unwrap();
    let owner = RoleRef::new(row.database_ref(), RoleKind::Owner).unwrap();
    d.engines
        .postgres
        .as_ref()
        .unwrap()
        .execute(
            &owner,
            &key,
            &StatementRequest {
                sql: text.into(),
                params: vec![],
                mode: StatementMode::Write,
                timeout: Duration::from_secs(10),
                max_rows: 10,
                max_bytes: None,
            },
        )
        .await
        .unwrap();
}

/// The class store's row of a column.
async fn column_row(pg: &Pg, table: &str, column: &str) -> crate::databases::classes::ColumnRow {
    pg.w.s
        .databases
        .deps()
        .columns
        .list(pg.w.team.id, pg.db)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.is_live() && r.column.table == table && r.column.column == column)
        .unwrap()
}

/// Review-Q-2-1 F-1: a view replaced in place, or a function replaced in place that a view
/// calls, keeps the view's OID and `attnum`s; the confirmation of its columns is cleared
/// (`redefined`), so a column confirmed `normal` never shows what it computes now.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_view_or_function_replaced_in_place_loses_its_confirmation() {
    let Some(pg) = setup().await else { return };
    let a = pg.alice();
    for s in [
        "create view x as select id, nick as n from people",
        "create view y as select id, nick as n from people",
        "create function nick_of(i integer) returns text language sql stable security definer \
         set search_path = public as $$ select nick from people where id = i $$",
        "create view z as select id, nick_of(id) as n from people",
    ] {
        pg.ok(&a, write(s)).await;
    }
    use DataClass::Normal as N;
    pg.confirm(&[
        ("x", "id", N),
        ("x", "n", N),
        ("y", "id", N),
        ("y", "n", N),
        ("z", "id", N),
        ("z", "n", N),
    ])
    .await;
    // Control: the confirmed columns show their values to the agent and to Bob.
    for who in [pg.agent(), pg.bob()] {
        for v in ["x", "y", "z"] {
            let r = pg
                .ok(&who, sql(&format!("select n from {v} where id = 1")))
                .await;
            assert_eq!(r.rows[0][0], json!("nick-1"), "{v}");
        }
    }
    for s in [
        "create or replace view x as select id, token as n from people",
        "create or replace view y as select id, email as n from people",
        "create or replace function nick_of(i integer) returns text language sql stable \
         security definer set search_path = public as $$ select token from people where id = i $$",
    ] {
        pg.ok(&a, write(s)).await;
    }
    for (v, col) in [("x", "n"), ("y", "n"), ("z", "n")] {
        let row = column_row(&pg, v, col).await;
        assert!(row.confirmed.is_none(), "{v}.{col}: {row:?}");
        assert_eq!(
            row.unconfirmed_reason,
            Some(crate::databases::classes::UnconfirmedReason::Redefined),
            "{v}.{col}"
        );
    }
    assert!(
        column_row(&pg, "people", "nick").await.confirmed.is_some(),
        "a table column keeps its class"
    );
    for who in [pg.agent(), pg.bob()] {
        for v in ["x", "z"] {
            let r = pg
                .ok(&who, sql(&format!("select n from {v} order by id")))
                .await;
            no_raw(&r);
            let c = &r.rows[0][0];
            assert!(
                c == &hidden("secret") || c == &hidden("personal"),
                "{v}: {c}"
            );
            assert_eq!(
                pg.count(
                    &who,
                    &format!("select count(*) from {v} where n = 'tok-1-secret'")
                )
                .await,
                json!(0),
                "{v}"
            );
        }
    }
    // y now reads a personal column: a pseudonym for the agent, never the address.
    let r = pg.ok(&pg.agent(), sql("select n from y")).await;
    no_raw(&r);
    assert!(is_pseudonym_marker(&r.rows[0][0]), "{}", r.rows[0][0]);
    pg.drop().await;
}

/// Review-Q-2-1 F-3, F-4, F-5: a whole-row reference in a view, a definer function reached
/// through a plain SQL-standard function, and a generated column over a secret never show the
/// secret to a reader of the `nosecret` lens (Bob, an agent with `read_personal`) or the masked
/// lens.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn computed_columns_over_a_secret_never_show_it() {
    let Some(pg) = setup().await else { return };
    let a = pg.alice();
    for s in [
        "create view j as select id, to_json(p)::text as doc from people p",
        "create view k as select id, p::text as doc from people p",
        "create function wrap(i integer) returns text language sql stable return peek(i)",
        "create view w2 as select id, wrap(id) as p from people",
        "alter table people add column tok2 text generated always as (upper(token)) stored",
        // Control: a whole-row reference to a table without a secret column.
        "create view oj as select id, to_json(o)::text as doc from orders o",
    ] {
        pg.ok(&a, write(s)).await;
    }
    pg.confirm(&[("oj", "id", DataClass::Normal)]).await;
    pg.grant(GrantKind::ReadPersonal).await;
    for who in [pg.bob(), pg.agent()] {
        for (v, col) in [("j", "doc"), ("k", "doc"), ("w2", "p"), ("people", "tok2")] {
            let r = pg
                .ok(&who, sql(&format!("select {col} from {v} order by id")))
                .await;
            let text = serde_json::to_string(&r.rows).unwrap();
            assert!(
                !text.contains("tok-") && !text.contains("TOK-"),
                "{v}.{col}: {text}"
            );
            assert_eq!(r.rows[0][0], hidden("secret"), "{v}.{col}");
        }
        assert_eq!(
            pg.count(
                &who,
                "select count(*) from people where tok2 = 'TOK-1-SECRET'"
            )
            .await,
            json!(0)
        );
        assert_eq!(
            pg.count(&who, "select count(*) from j where doc like '%tok-1%'")
                .await,
            json!(0)
        );
        // The other columns keep their lens: a personal value for these readers.
        let r = pg
            .ok(
                &who,
                sql("select email, doc from people, oj where people.id = 1 and oj.id = 1"),
            )
            .await;
        assert_eq!(r.rows[0][0], json!("a1@example.test"));
        assert!(
            r.rows[0][1].as_str().is_some_and(|d| d.contains("order-1")),
            "{}",
            r.rows[0][1]
        );
    }
    // The owner's control finds the values.
    assert_eq!(
        pg.count(
            &a,
            "select count(*) from people where tok2 = 'TOK-1-SECRET'"
        )
        .await,
        json!(1)
    );
    assert_eq!(
        pg.count(&a, "select count(*) from w2 where p like 'tok%'")
            .await,
        json!(3)
    );
    pg.drop().await;
}

/// Review-Q-2-2 P-1: a tenant trigger or rule runs on the real row of a write, whatever lens the
/// writer has. An agent's write in a database with one needs a person's approval (501 until card
/// A-2); nothing runs, and the engine refuses a `masked_write` there by itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_writes_never_run_tenant_triggers_or_rules() {
    let Some(pg) = setup().await else { return };
    let a = pg.alice();
    let agent = pg.agent();
    pg.ok(
        &a,
        write("create table audit (id serial primary key, what text)"),
    )
    .await;
    pg.confirm(&[
        ("audit", "id", DataClass::Normal),
        ("audit", "what", DataClass::Normal),
    ])
    .await;
    pg.grant(GrantKind::WriteLive).await;
    // Control: without a trigger the agent's write runs.
    pg.ok(&agent, write("update orders set amount = 2 where id = 1"))
        .await;
    for s in [
        "create function log_it() returns trigger language plpgsql as $$ \
         begin insert into audit (what) values (new.person_email); return new; end $$",
        "create trigger orders_log after update on orders for each row execute function log_it()",
    ] {
        pg.ok(&a, write(s)).await;
    }
    let refuse = |label: &'static str| {
        let pg = &pg;
        let agent = agent.clone();
        async move {
            for text in [
                "update orders set amount = 3 where id = 1",
                "insert into orders (id, amount) values (70, 1)",
                "with u as (update orders set amount = 4 where id = 2 returning id) select count(*) from u",
            ] {
                let (status, code, hint) = pg.refused(&agent, write(text)).await;
                assert_eq!(
                    (status, code.as_str()),
                    (501, "not_implemented"),
                    "{label}: {text}"
                );
                assert!(hint.is_some());
            }
        }
    };
    refuse("masked_write").await;
    let audit = || async { pg.count(&pg.alice(), "select count(*) from audit").await };
    assert_eq!(audit().await, json!(0), "nothing ran");
    let r = pg.ok(&agent, sql("select what from audit")).await;
    assert!(r.rows.is_empty());
    // The engine's own line: a masked_write session refuses the write in its transaction.
    let d = pg.w.s.databases.deps();
    let row = d.store.get(pg.w.team.id, pg.db).await.unwrap().unwrap();
    let key = d
        .queries
        .keys
        .engine_key(pg.w.team.id, row.key_node_id)
        .await
        .unwrap();
    let role = RoleRef::new(row.database_ref(), RoleKind::MaskedWrite).unwrap();
    let r = d
        .engines
        .postgres
        .as_ref()
        .unwrap()
        .execute(
            &role,
            &key,
            &StatementRequest {
                sql: "update orders set amount = 5 where id = 1".into(),
                params: vec![],
                mode: StatementMode::Write,
                timeout: Duration::from_secs(5),
                max_rows: 10,
                max_bytes: None,
            },
        )
        .await;
    assert!(
        matches!(&r, Err(EngineError::StatementNotAllowed(m)) if m == "tenant_triggers_or_rules"),
        "{r:?}"
    );
    assert_eq!(audit().await, json!(0), "nothing ran");
    // With read_personal (the `write` kind) too.
    pg.grant(GrantKind::ReadPersonal).await;
    refuse("write").await;
    assert_eq!(audit().await, json!(0));
    // A rule does the same as a trigger.
    pg.ok(&a, write("drop trigger orders_log on orders")).await;
    pg.ok(&agent, write("update orders set amount = 6 where id = 1"))
        .await;
    // The query API cannot parse `CREATE RULE`; a rule can still come with a restored or copied
    // database, so the owner makes it in the engine.
    as_owner(
        &pg,
        "create rule orders_copy as on update to orders do also \
         insert into audit (what) values (new.person_email)",
    )
    .await;
    refuse("rule").await;
    assert_eq!(audit().await, json!(0));
    pg.drop().await;
}

/// Review-Q-2-2 P-2: tenant code (a trigger, a function in a check constraint) can raise any
/// SQLSTATE with any text, with the values of the real row. A caller without `manage` gets the
/// fixed text, never a name from such an error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn errors_raised_by_tenant_code_name_nothing() {
    let Some(pg) = setup().await else { return };
    let a = pg.alice();
    for s in [
        "create table notes (id integer primary key, code text, n integer)",
        "insert into notes values (1, 'tokcommon', 1)",
        "create function chk(c text, n integer) returns boolean language plpgsql immutable as $$ \
         begin if n = 7 then raise exception using errcode = '23514', \
         message = format('bad \"%s\"', c); end if; return true; end $$",
        "alter table notes add constraint notes_chk check (chk(code, n))",
    ] {
        pg.ok(&a, write(s)).await;
    }
    pg.confirm(&[
        ("notes", "id", DataClass::Normal),
        ("notes", "code", DataClass::Secret),
        ("notes", "n", DataClass::Normal),
    ])
    .await;
    pg.grant(GrantKind::WriteLive).await;
    let message = |r: std::result::Result<QueryResult, QueryError>| match r {
        Err(QueryError::Problem(p)) => (p.sqlstate.clone(), p.message.clone()),
        other => panic!("{other:?}"),
    };
    // The agent (no trigger in the database, so its write reaches the engine).
    let (state, text) = message(
        pg.run(&pg.agent(), write("update notes set n = 7 where id = 1"))
            .await,
    );
    assert_eq!(state.as_deref(), Some("23514"));
    assert_eq!(text, "a check constraint was violated");
    // A trigger that names the real secret, for Bob (edit, no manage).
    for s in [
        "create function shout() returns trigger language plpgsql as $$ begin \
         if new.score = 99 then raise exception using errcode = '23505', \
         message = format('dup \"%s\"', replace(new.token, '-', '_')); end if; return new; end $$",
        "create trigger people_shout before update on people for each row execute function shout()",
    ] {
        pg.ok(&a, write(s)).await;
    }
    let (state, text) = message(
        pg.run(
            &pg.bob(),
            write("update people set score = 99 where id = 1"),
        )
        .await,
    );
    assert_eq!(state.as_deref(), Some("23505"));
    assert_eq!(text, "a unique constraint was violated");
    // Control: a person with manage reads the engine's text.
    let (_, text) = message(
        pg.run(&a, write("update people set score = 99 where id = 1"))
            .await,
    );
    assert!(text.contains("tok_1_secret"), "{text}");
    // The engine's own errors still name their objects.
    let (_, text) = message(
        pg.run(
            &pg.bob(),
            write("update people set score = -1 where id = 2"),
        )
        .await,
    );
    assert!(
        text.starts_with("a check constraint was violated: ")
            && text.contains("people_score_check"),
        "{text}"
    );
    pg.drop().await;
}

/// Review-Q-2-2 P-4, side channels and the survivor `other-schema-normal`: whole rows and
/// renamed columns are never labelled `normal`; a column of another schema is `personal`;
/// `EXPLAIN` with costs gives the agent no estimate that depends on a hidden value.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn labels_of_whole_rows_and_estimates_of_plans() {
    let Some(pg) = setup().await else { return };
    let agent = pg.agent();
    for text in [
        "select row_to_json(p) from people p where id = 1",
        "select upper(score) from people p(id, score, x, y, z, w) where id = 1",
    ] {
        let r = pg.ok(&agent, sql(text)).await;
        no_raw(&r);
        assert_ne!(r.columns[0].data_class, DataClass::Normal, "{text}");
        assert_ne!(r.columns[0].masking, Masking::None, "{text}");
        let cell = serde_json::to_string(&r.rows[0][0]).unwrap();
        assert!(!cell.contains("ruum:hidden"), "{text}: {cell}");
    }
    let r = pg
        .ok(
            &agent,
            sql("select relname from pg_class where relname = 'people'"),
        )
        .await;
    assert_eq!(r.columns[0].data_class, DataClass::Personal);
    // EXPLAIN with costs after ANALYZE of a skewed table.
    let a = pg.alice();
    for s in [
        "create table big (id integer primary key, email text, token text)",
        "insert into big select g, case when g <= 900 then 'common@x.test' else 'rare' || g || '@x.test' end, \
         case when g <= 900 then 'tokcommon' else 'tok' || g end from generate_series(1, 1000) g",
    ] {
        pg.ok(&a, write(s)).await;
    }
    pg.confirm(&[
        ("big", "id", DataClass::Normal),
        ("big", "email", DataClass::Personal),
        ("big", "token", DataClass::Secret),
    ])
    .await;
    as_owner(&pg, "analyze big").await;
    let rows_estimate = |plan: &QueryResult| -> String {
        let text = plan.rows[0][0].as_str().unwrap_or_default().to_string();
        let at = text.find("rows=").expect("an estimate");
        text[at..].split_whitespace().next().unwrap().to_string()
    };
    for (common, rare) in [
        (
            "explain select * from big where email = 'common@x.test'",
            "explain select * from big where email = 'rare950@x.test'",
        ),
        (
            "explain select * from big where token = 'tokcommon'",
            "explain select * from big where token = 'tok950'",
        ),
    ] {
        let owner = (
            rows_estimate(&pg.ok(&a, sql(common)).await),
            rows_estimate(&pg.ok(&a, sql(rare)).await),
        );
        assert_ne!(owner.0, owner.1, "control: the owner's estimates differ");
        let seen = (
            rows_estimate(&pg.ok(&agent, sql(common)).await),
            rows_estimate(&pg.ok(&agent, sql(rare)).await),
        );
        assert_eq!(seen.0, seen.1, "{common}");
    }
    pg.drop().await;
}
