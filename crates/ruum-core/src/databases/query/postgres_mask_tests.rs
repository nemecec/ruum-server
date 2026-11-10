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
        self.agent_ctx(self.agent, &self.scope)
    }

    /// The context of an agent of Alice.
    fn agent_ctx(&self, agent: AgentId, scope: &crate::authz::Scope) -> Context {
        Context {
            tenant: self.w.team.id,
            actor: Actor {
                actor: PrincipalRef {
                    kind: PrincipalKind::Agent,
                    id: agent.into(),
                },
                on_behalf_of: Some(self.w.alice),
            },
            scope: Some(scope.clone()),
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
        self.grant_to(self.agent, kind).await
    }

    async fn grant_to(&self, agent: AgentId, kind: GrantKind) {
        self.w
            .s
            .databases
            .create_grant(
                &self.alice(),
                &*self.w.s.agents,
                self.db,
                CreateGrant {
                    kind,
                    agent_id: agent,
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
/// view without one; the classes confirmed by Alice (`manage`). It holds no tenant code
/// (decision 0004-38), so masked sessions may write; [`definer_view`] adds some.
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

/// Adds `peek(i)`, a `SECURITY DEFINER` function of the tenant that reads `token`, and the view
/// `w` over it. This is tenant code: masked sessions no longer write to the database.
async fn definer_view(pg: &Pg) {
    for s in [
        "create function peek(i integer) returns text language sql stable security definer \
         set search_path = public as $$ select token from people where id = i $$",
        "create view w as select id, peek(id) as p from people",
    ] {
        pg.ok(&pg.alice(), write(s)).await;
    }
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
    definer_view(&pg).await;
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
    // owner's rights. A function of the tenant is outside the allow-list (decision 0004-38), so
    // `w` is not in the masked lens at all: the refusal, never a value or a marker.
    for who in [pg.bob(), pg.agent()] {
        let r = pg
            .refused(&who, sql("select id, p from w order by id"))
            .await;
        assert_eq!(
            r,
            (
                422,
                "statement_not_allowed".to_string(),
                Some("tenant_code_on_live".to_string())
            )
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
    // A manifest version that the reconciler applies (as after `restoreVersion`): the provider's
    // `applyManifest` rebuilds the lens too. The version takes `nick` out of `data_classes`, so
    // it reads as a pseudonym again (store `normal`, manifest none: `personal`).
    let typed = d.typed.get(pg.w.team.id, pg.db).await.unwrap().unwrap();
    let mut manifest = d
        .store
        .manifest(pg.w.team.id, typed.desired_version)
        .await
        .unwrap()
        .unwrap();
    let current = manifest.clone();
    manifest["data_classes"]
        .as_object_mut()
        .unwrap()
        .remove("public.people.nick");
    let manifest = crate::databases::manifest::check(&manifest, Some(&current), false).unwrap();
    d.store
        .put_manifest(
            &pg.alice(),
            pg.db,
            &manifest,
            Some("restored"),
            typed.desired_version,
            None,
        )
        .await
        .unwrap();
    let round = crate::providers::reconciler::Reconciler::new(
        pg.w.s.typed_nodes.clone(),
        pg.w.s.providers.clone(),
    )
    .settled(Duration::ZERO)
    .run_once()
    .await
    .unwrap();
    assert_eq!(round.applied, 1, "{round:?}");
    let r = pg
        .ok(&agent, sql("select nick from people where id = 1"))
        .await;
    assert!(is_pseudonym_marker(&r.rows[0][0]), "{}", r.rows[0][0]);
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
    // Control: the confirmed columns of the views on the allow-list show their values to the
    // agent and to Bob. `z` calls a function of the tenant: it is outside the lens (refusal).
    let refusal = (
        422,
        "statement_not_allowed".to_string(),
        Some("tenant_code_on_live".to_string()),
    );
    for who in [pg.agent(), pg.bob()] {
        for v in ["x", "y"] {
            let r = pg
                .ok(&who, sql(&format!("select n from {v} where id = 1")))
                .await;
            assert_eq!(r.rows[0][0], json!("nick-1"), "{v}");
        }
        let r = pg.refused(&who, sql("select n from z where id = 1")).await;
        assert_eq!(r, refusal, "z");
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
        let r = pg.refused(&who, sql("select n from z order by id")).await;
        assert_eq!(r, refusal, "z");
        for v in ["x"] {
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
    definer_view(&pg).await;
    let a = pg.alice();
    for s in [
        "create view j as select id, to_json(p)::text as doc from people p",
        "create view k as select id, p::text as doc from people p",
        "create function wrap(i integer) returns text language sql stable return peek(i)",
        "create view w2 as select id, wrap(id) as p from people",
        "alter table people add column tok2 text generated always as (upper(token)) stored",
        // Review-Q-2-1 F-4: a generated column reaches a definer function through a plain one.
        // A table is not a view, so the lens keeps it (no refusal); the walk must still find the
        // definer function and mask the column like a secret.
        "create function peekd(i integer) returns text language sql immutable security definer \
         set search_path = public as $$ select token from people where id = i $$",
        "create function wrapd(i integer) returns text language sql immutable return peekd(i)",
        "alter table orders add column leaked text generated always as (wrapd(id)) stored",
        // Control: a whole-row reference to a table without a secret column.
        "create view oj as select id, to_json(o)::text as doc from orders o",
    ] {
        pg.ok(&a, write(s)).await;
    }
    pg.confirm(&[("oj", "id", DataClass::Normal)]).await;
    pg.grant(GrantKind::ReadPersonal).await;
    for who in [pg.bob(), pg.agent()] {
        // `w2` reaches a definer function of the tenant: outside the allow-list, so outside the
        // lens (the refusal, no value and no marker).
        let r = pg.refused(&who, sql("select p from w2 order by id")).await;
        assert_eq!(
            r,
            (
                422,
                "statement_not_allowed".to_string(),
                Some("tenant_code_on_live".to_string())
            ),
            "w2"
        );
        for (v, col) in [("j", "doc"), ("k", "doc"), ("people", "tok2")] {
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
        let r = pg
            .ok(&who, sql("select leaked from orders order by id"))
            .await;
        let text = serde_json::to_string(&r.rows).unwrap();
        assert!(!text.contains("tok-"), "orders.leaked: {text}");
        assert_eq!(r.rows[0][0], hidden("secret"), "orders.leaked");
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

/// A kind of tenant code for [`masked_sessions_never_run_tenant_code_on_live`]: how Alice
/// makes it (`make`: query API; `owner`: an engine `owner` session, for statements the query API
/// cannot parse), the writes that would run it, and how it goes again.
struct CodeCase {
    label: &'static str,
    make: &'static [&'static str],
    owner: &'static [&'static str],
    writes: &'static [&'static str],
    drop: &'static [&'static str],
}

/// The writes of every case: the forms of the P-1 probe (review-Q-2-2).
const PLAIN_WRITES: &[&str] = &[
    "update orders set amount = 3 where id = 1",
    "insert into orders (id, amount) values (70, 1)",
    "with u as (update orders set amount = 4 where id = 2 returning id) select count(*) from u",
];

const CODE_CASES: &[CodeCase] = &[
    // Review-Q-2-2 P-1: a trigger copies a personal value into `audit`.
    CodeCase {
        label: "trigger",
        make: &[
            "create function log_it() returns trigger language plpgsql as $$ \
             begin insert into audit (what) values (new.person_email); return new; end $$",
            "create trigger orders_log after update on orders for each row execute function log_it()",
        ],
        owner: &[],
        writes: &[],
        drop: &[
            "drop trigger orders_log on orders",
            "drop function log_it()",
        ],
    },
    // Disabled code counts: `ENABLE TRIGGER` is DDL with the race of a new trigger.
    CodeCase {
        label: "disabled trigger with a built-in function",
        make: &["create table docs (id integer primary key, body text, tsv tsvector)"],
        owner: &[
            "create trigger docs_tsv before insert or update on docs for each row \
             execute function tsvector_update_trigger(tsv, 'pg_catalog.simple', body)",
            "alter table docs disable trigger docs_tsv",
        ],
        writes: &[],
        drop: &["drop table docs"],
    },
    CodeCase {
        label: "rule",
        make: &[],
        owner: &["create rule orders_copy as on update to orders do also \
             insert into audit (what) values (new.person_email)"],
        writes: &[],
        drop: &[],
    },
    CodeCase {
        label: "policy",
        make: &[],
        owner: &["create policy orders_p on orders using (amount >= 0)"],
        writes: &[],
        drop: &[],
    },
    // Review-Q-2-3 R-1: a plain (not definer) volatile function behind a view gets the real
    // secret as its argument; a row write runs it in a writable transaction.
    CodeCase {
        label: "function behind a view",
        make: &[
            "create function log_v(t text) returns text language plpgsql volatile as \
             $$ begin insert into audit (what) values (t); return 'ok'; end $$",
            "create view v2 as select id, log_v(token) as x from people",
        ],
        owner: &[],
        writes: &[
            "update orders set amount = (select count(*) from v2) where id = 1",
            "update orders set amount = 5 where id = 1 and exists (select 1 from v2 where id = 1)",
        ],
        drop: &["drop view v2", "drop function log_v(text)"],
    },
    // A STABLE function that calls a VOLATILE one writes too (S-21 review 2): volatility
    // tells nothing.
    CodeCase {
        label: "stable function over a volatile one behind a view",
        make: &[
            "create function log_s(t text) returns text language plpgsql volatile as \
             $$ begin insert into audit (what) values (t); return 'ok'; end $$",
            "create function quiet(t text) returns text language plpgsql stable as \
             $$ begin return log_s(t); end $$",
            "create view v3 as select id, quiet(token) as x from people",
        ],
        owner: &[],
        writes: &["update orders set amount = (select count(*) from v3) where id = 1"],
        drop: &[
            "drop view v3",
            "drop function quiet(text)",
            "drop function log_s(text)",
        ],
    },
    // A definer function behind a view writes with the owner's rights.
    CodeCase {
        label: "definer function behind a view",
        make: &[
            "create function stash_it2() returns integer language plpgsql volatile security \
             definer set search_path = public as $$ begin insert into audit (what) \
             select token from people where id = 1; return 1; end $$",
            "create view sv2 as select id, stash_it2() as s from people where id = 1",
        ],
        owner: &[],
        writes: &["update orders set amount = (select count(*) from sv2) where id = 1"],
        drop: &["drop view sv2", "drop function stash_it2()"],
    },
    // A volatile default runs on an insert.
    CodeCase {
        label: "volatile default",
        make: &[
            "create function stamp() returns text language plpgsql volatile as \
             $$ begin insert into audit (what) values ('ran'); return 'x'; end $$",
            "alter table orders alter column label set default stamp()",
        ],
        owner: &[],
        writes: &[],
        drop: &[
            "alter table orders alter column label drop default",
            "drop function stamp()",
        ],
    },
    // Any function counts, also one that runs only built-ins (an aggregate).
    CodeCase {
        label: "aggregate over a built-in function",
        make: &[],
        owner: &["create aggregate total(integer) (sfunc = int4pl, stype = integer)"],
        writes: &[],
        drop: &[],
    },
];

/// Decision 0004-38 (design 8.5 "Tenant code and masked sessions"; review-Q-2-2 P-1,
/// review-Q-2-3 R-1 and N-3, the S-21 panel): tenant code runs on the real rows of a write,
/// whatever lens the writer has. A row write of a masked session (an agent without and with
/// `read_personal`, a person with `edit` but not `manage`) on a live database that holds any
/// tenant code gets `501` with the hint `tenant_code_on_live`; nothing runs, nothing is copied.
/// One case per kind of tenant code, and a control after each.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn masked_sessions_never_run_tenant_code_on_live() {
    let Some(pg) = setup().await else { return };
    let a = pg.alice();
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
    let other = register_agent(&pg.w, pg.w.alice).await;
    pg.grant_to(other.id, GrantKind::WriteLive).await;
    pg.grant_to(other.id, GrantKind::ReadPersonal).await;
    let other_scope = other.scope.as_scope();
    let sessions = [
        ("agent (masked_write)", pg.agent()),
        (
            "agent with read_personal (write)",
            pg.agent_ctx(other.id, &other_scope),
        ),
        ("person with edit (write)", pg.bob()),
    ];
    let state = || async {
        let r = pg
            .ok(
                &pg.alice(),
                sql("select (select count(*) from audit), \
                     (select coalesce(sum(amount), 0) from orders), \
                     (select count(*) from orders)"),
            )
            .await;
        r.rows[0].clone()
    };
    // Control: without tenant code every session writes.
    for (label, c) in &sessions {
        let r = pg
            .run(c, write("update orders set amount = 2 where id = 1"))
            .await;
        assert!(r.is_ok(), "{label}: {r:?}");
    }
    let before = state().await;
    for case in CODE_CASES {
        for s in case.make {
            pg.ok(&a, write(s)).await;
        }
        for s in case.owner {
            as_owner(&pg, s).await;
        }
        // The views' `id` columns are confirmed `normal`, so the footprint lets the writes reach
        // the tenant-code check.
        for view in ["v2", "v3", "sv2"] {
            if case
                .make
                .iter()
                .any(|m| m.contains(&format!("create view {view} ")))
            {
                pg.confirm(&[(view, "id", DataClass::Normal)]).await;
            }
        }
        for (label, c) in &sessions {
            for text in PLAIN_WRITES.iter().chain(case.writes) {
                let (status, code, hint) = pg.refused(c, write(text)).await;
                assert_eq!(
                    (status, code.as_str(), hint.as_deref()),
                    (501, "not_implemented", Some("tenant_code_on_live")),
                    "{}: {label}: {text}",
                    case.label
                );
            }
        }
        assert_eq!(state().await, before, "{}: nothing ran", case.label);
        for s in case.drop {
            pg.ok(&a, write(s)).await;
        }
        for s in case.owner {
            let undo = if s.starts_with("create rule orders_copy") {
                "drop rule orders_copy on orders"
            } else if s.starts_with("create policy orders_p") {
                "drop policy orders_p on orders"
            } else if s.starts_with("create aggregate total") {
                "drop aggregate total(integer)"
            } else {
                continue;
            };
            as_owner(&pg, undo).await;
        }
        // Control: with the code gone, the agent writes again.
        pg.ok(
            &pg.agent(),
            write("update orders set amount = 2 where id = 1"),
        )
        .await;
    }
    // The engine's own line, for both masked write kinds: in the request's own transaction.
    pg.ok(&a, write(CODE_CASES[0].make[0])).await;
    pg.ok(&a, write(CODE_CASES[0].make[1])).await;
    let d = pg.w.s.databases.deps();
    let row = d.store.get(pg.w.team.id, pg.db).await.unwrap().unwrap();
    let key = d
        .queries
        .keys
        .engine_key(pg.w.team.id, row.key_node_id)
        .await
        .unwrap();
    for kind in [RoleKind::MaskedWrite, RoleKind::Write] {
        let role = RoleRef::new(row.database_ref(), kind).unwrap();
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
            matches!(&r, Err(EngineError::StatementNotAllowed(m)) if m == "tenant_code_on_live"),
            "{kind:?}: {r:?}"
        );
    }
    assert_eq!(state().await, before, "the engine ran nothing");
    // A person with manage writes: the trigger runs for them (they may see every value).
    pg.ok(&a, write("update orders set amount = 2 where id = 1"))
        .await;
    assert_eq!(
        pg.count(&a, "select count(*) from audit").await,
        json!(1),
        "the owner's write ran the trigger"
    );
    pg.drop().await;
}

/// Review-Q-2-2 P-2: tenant code (a function in a view, a check function, a trigger) can raise
/// any SQLSTATE with any text, with the values of the real row. A caller without `manage` gets
/// the fixed text, never a name from such an error. Since decision 0004-38 masked sessions reach
/// no tenant code: their row writes are refused where tenant code exists, and a view that calls a
/// function of the tenant is outside the masked lens. The marking of an error from inside a
/// routine (`in_routine`) is tested on the engine (`errors_from_inside_a_routine_are_marked`) and
/// in the unit tests of the mapping; here the manager still reads the engine's own text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn errors_raised_by_tenant_code_name_nothing() {
    let Some(pg) = setup().await else { return };
    let a = pg.alice();
    let message = |r: std::result::Result<QueryResult, QueryError>| match r {
        Err(QueryError::Problem(p)) => (p.sqlstate.clone(), p.message.clone()),
        other => panic!("{other:?}"),
    };
    // The engine's own errors still name their objects (no tenant code yet, so Bob writes).
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
    for s in [
        "create table notes (id integer primary key, code text, n integer)",
        "insert into notes values (1, 'tokcommon', 7)",
        "create function chk(c text, n integer) returns text language plpgsql stable as $$ \
         begin if n = 7 then raise exception using errcode = '23514', \
         message = format('bad \"%s\"', c); end if; return 'ok'; end $$",
        // The function is in the view's filter, so every read of the view runs it.
        "create view nv as select id, n from notes where chk(code, n) = 'ok'",
    ] {
        pg.ok(&a, write(s)).await;
    }
    pg.confirm(&[
        ("notes", "id", DataClass::Normal),
        ("notes", "code", DataClass::Secret),
        ("notes", "n", DataClass::Normal),
        ("nv", "id", DataClass::Normal),
        ("nv", "n", DataClass::Normal),
    ])
    .await;
    // The agent and Bob (edit, no manage) cannot reach the function at all: the view calls a
    // function of the tenant, so it is outside the lens (decision 0004-38). Without the refusal
    // the function would raise with the real secret in its text.
    for who in [pg.agent(), pg.bob()] {
        let r = pg.refused(&who, sql("select n from nv where id = 1")).await;
        assert_eq!(
            r,
            (
                422,
                "statement_not_allowed".to_string(),
                Some("tenant_code_on_live".to_string())
            )
        );
    }
    // Control: a person with manage reads the engine's text.
    let (_, text) = message(pg.run(&a, sql("select n from nv where id = 1")).await);
    assert!(text.contains("tokcommon"), "{text}");
    // A masked write that would run the function never reaches it.
    pg.grant(GrantKind::WriteLive).await;
    let (status, _, hint) = pg
        .refused(&pg.agent(), write("update notes set n = 7 where id = 1"))
        .await;
    assert_eq!(
        (status, hint.as_deref()),
        (501, Some("tenant_code_on_live"))
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

/// A masked session's statement that writes no row runs read-only, also in mode `write`
/// (found while closing review-Q-2-2 P-1): its transaction is read-only, and a row write of the
/// same session is not. The agent and Bob (edit, no manage) both.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_statements_that_write_no_row_run_read_only() {
    let Some(pg) = setup().await else { return };
    pg.grant(GrantKind::WriteLive).await;
    for (who, name) in [(pg.agent(), "agent"), (pg.bob(), "bob")] {
        let r = pg
            .ok(
                &who,
                write("select current_setting('transaction_read_only')"),
            )
            .await;
        assert_eq!(
            r.rows[0][0],
            json!("on"),
            "{name}: no row written: read-only"
        );
        let r = pg
            .ok(
                &who,
                write(
                    "update orders set amount = amount where id = 1 \
                     returning current_setting('transaction_read_only')",
                ),
            )
            .await;
        assert_eq!(
            r.rows[0][0],
            json!("off"),
            "{name}: a row write is writable"
        );
    }
    pg.drop().await;
}

/// A label, the statements that make the code, the statements that remove it, the view, and the
/// attack statement.
type AttackCase<'a> = (&'a str, &'a [&'a str], &'a [&'a str], &'a str, &'a str);

/// Review-Q-2-s21-2 R-1 and R-2 and review-Q-2-harden H-1 (decision 0004-38 (b) and (d)): code
/// that runs on the real rows may call only functions of the allow-list. A view that carries a
/// whole value out through a session setting (S1: a tenant function), advisory locks (U2: built-in
/// functions only), a large object (V) or a random seed (H1) is not part of the masked lens: the
/// agent and Bob (edit, no manage) get the refusal `statement_not_allowed`, hint
/// `tenant_code_on_live`, for the attack statement of each review, and a write that reads the
/// view is refused with the `501` of the write rule. Nothing leaks and nothing changes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn masked_reads_of_code_outside_the_allow_list_are_refused() {
    let Some(pg) = setup().await else { return };
    let a = pg.alice();
    pg.grant(GrantKind::WriteLive).await;
    // (label, statements that make it, statements that remove it, view, the attack statement)
    let cases: [AttackCase; 4] = [
        (
            "S1: a tenant function writes the value into a session setting",
            &[
                "create function leak(t text) returns text language plpgsql volatile as $$ \
                 begin execute pg_catalog.format('set local ruum_probe.v = %L', t); return 'ok'; end $$",
                "create view lv as select id, leak(token) as x from people",
            ],
            &["drop view lv", "drop function leak(text)"],
            "lv",
            "with a as materialized (select count(*) as c from lv where id = 1) \
             select c, current_setting('ruum_probe.v', true) as s from a",
        ),
        (
            "U2: built-in advisory locks keyed by the characters of the value",
            &["create view bv as select id, \
               pg_try_advisory_xact_lock(1001, ascii(substr(token, 1, 1))) as x, \
               pg_try_advisory_xact_lock(1002, ascii(substr(token, 2, 1))) as y from people"],
            &["drop view bv"],
            "bv",
            "with a as materialized (select count(*) as c from bv where id = 1) \
             select l.s from a, lateral (select string_agg(chr(objid::int), '' order by classid) as s \
             from pg_locks where locktype = 'advisory' and pid = pg_backend_pid() and a.c >= 0) l",
        ),
        (
            "V: a built-in large object made from the value",
            &["create view lov as select id, \
               lo_from_bytea(0, convert_to(coalesce(token, ''), 'UTF8')) as o from people"],
            &["drop view lov"],
            "lov",
            "with a as materialized (select count(*) as c from lov where id = 1) \
             select (select c from a) as c, \
             (select convert_from(lo_get(m.oid), 'UTF8') from pg_largeobject_metadata m limit 1) as v",
        ),
        (
            "H1: a built-in random seed set from the value",
            &["create view sv as select id, \
               (select 1 from (select setseed(ascii(substr(token, 1, 1)) / 1000.0)) q) as x from people"],
            &["drop view sv"],
            "sv",
            "with a as materialized (select count(*) as c from sv where id = 1) \
             select (select c from a) as c, random() as r",
        ),
    ];
    let refusal = (
        422,
        "statement_not_allowed".to_string(),
        Some("tenant_code_on_live".to_string()),
    );
    let write_refusal = (
        501,
        "not_implemented".to_string(),
        Some("tenant_code_on_live".to_string()),
    );
    for (label, make, unmake, view, attack) in cases {
        for s in make {
            pg.ok(&a, write(s)).await;
        }
        // The view's `id` is confirmed `normal`, so the class rule of the write path lets the
        // statement through to the rule under test.
        pg.confirm(&[(view, "id", DataClass::Normal)]).await;
        for (who, name) in [(pg.agent(), "agent"), (pg.bob(), "bob")] {
            for text in [attack.to_string(), format!("select id from {view}")] {
                let r = pg.refused(&who, sql(&text)).await;
                assert_eq!(r, refusal, "{label}: {name}: {text}");
            }
            let r = pg
                .refused(
                    &who,
                    write(&format!(
                        "update orders set amount = (select count(*) from {view} where id = 1) \
                         where id = 1"
                    )),
                )
                .await;
            assert_eq!(
                r, write_refusal,
                "{label}: {name}: a write that reads the view"
            );
        }
        assert_eq!(
            pg.count(&a, "select amount from orders where id = 1").await,
            json!(1),
            "{label}: nothing changed"
        );
        for s in unmake {
            pg.ok(&a, write(s)).await;
        }
        // Control: with the view gone, the lens is whole again.
        let r = pg.ok(&pg.agent(), sql("select count(*) from orders")).await;
        assert_eq!(r.rows[0][0], json!(PEOPLE), "{label}");
    }
    // An agent with `read_personal` reads the `nosecret` lens, and gets the same refusal.
    pg.grant(GrantKind::ReadPersonal).await;
    pg.ok(
        &a,
        write(
            "create view sv as select id, (select 1 from (select setseed(0.5)) q) as x from people",
        ),
    )
    .await;
    let r = pg.refused(&pg.agent(), sql("select id from sv")).await;
    assert_eq!(r, refusal, "an agent with read_personal");
    // The person with manage (the owner kind, in a writable statement) still reads the real view.
    let r = pg.ok(&a, write("select count(*) from sv")).await;
    assert_eq!(r.rows[0][0], json!(PEOPLE));
    pg.ok(&a, write("drop view sv")).await;
    pg.drop().await;
}
