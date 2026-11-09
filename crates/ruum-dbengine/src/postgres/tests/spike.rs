//! "S-1's script passes against the engine": the role, lens and pseudonym sections of
//! `spikes/step2-roles/spike.sh` (1 to 4, 6 and 8), run against a database that the engine made
//! (its roles, its lens, its key), each statement in a session that logs in as the role.
//! Section 5 (pooled sessions) and section 7 (extensions) are covered by the cases in
//! `security.rs`; section 6 by `rust_and_sql_pseudonyms_agree_under_hostile_settings`.

use super::*;
use std::time::Duration;

const RAW_PERSONAL: &[&str] = &[
    "alice@example.com",
    "bob@example.com",
    "carol@example.com",
    "Alice Liddell",
    "Bob Builder",
    "1990-04-01",
    "1985-02-03",
    "2026-11-01",
];
const RAW_SECRET: &[&str] = &["tok_secret_alice", "tok_secret_bob", "tok_secret_carol"];
const KEY_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
/// The pseudonyms of alice's and bob's addresses under the key 0x00..0x1f.
const ALICE: &str = "p_a59fc578d4cb";

struct Checks {
    failed: Vec<String>,
    passed: usize,
}

impl Checks {
    fn check(&mut self, name: &str, ok: bool, out: &str) {
        if ok {
            self.passed += 1;
        } else {
            self.failed.push(format!("{name}: {out}"));
        }
    }
}

struct Spike<'e> {
    engine: &'e PostgresEngine,
    db: DatabaseRef,
    created: CreatedDatabase,
    checks: Checks,
}

impl Spike<'_> {
    fn name(&self, kind: RoleKind) -> String {
        role_name(&self.created, kind)
    }

    async fn run(&self, kind: RoleKind, sql: &str) -> Result<String, String> {
        let mut s = session(self.engine, &self.db, kind).await;
        let out = psql(&mut s, sql).await;
        close(s).await;
        out
    }

    async fn ok(&mut self, name: &str, kind: RoleKind, sql: &str, want: Option<&str>) {
        let out = self.run(kind, sql).await;
        let good = match (&out, want) {
            (Ok(o), Some(w)) => o == w,
            (Ok(_), None) => true,
            _ => false,
        };
        self.checks.check(name, good, &format!("{out:?}"));
    }

    async fn err(&mut self, name: &str, kind: RoleKind, sql: &str, any_of: &[&str]) {
        let out = self.run(kind, sql).await;
        let good = matches!(&out, Err(e) if any_of.iter().any(|w| e.contains(w)));
        self.checks.check(name, good, &format!("{out:?}"));
    }

    async fn noleak(&mut self, name: &str, kind: RoleKind, sql: &str) {
        let out = self.run(kind, sql).await;
        let good = match &out {
            Ok(o) => {
                let lower = o.to_lowercase();
                RAW_PERSONAL
                    .iter()
                    .chain(RAW_SECRET)
                    .all(|v| !lower.contains(&v.to_lowercase()))
                    && !o.contains(&KEY_HEX[..12])
                    && !o.contains(&KEY_HEX[12..24])
            }
            Err(_) => false,
        };
        self.checks.check(name, good, &format!("{out:?}"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s1_script_sections_pass_against_an_engine_database() {
    let Some(engine) = engine().await else { return };
    let key = vector_key();
    let created = engine
        .create_database(&conformance::new_database(Uuid::now_v7()), &key)
        .await
        .unwrap();
    let other = engine
        .create_database(&conformance::new_database(Uuid::now_v7()), &key)
        .await
        .unwrap();
    let db = created.database.clone();
    let sync = ClassSync {
        personal: vec!["email", "name", "born", "last_login", "user_email"],
        secret: vec!["api_token"],
    };
    let up = MigrationRequest {
        up: "create table users (
               id int primary key, email text not null, name text, plan text not null default 'free',
               age int check (age >= 0), code text, api_token text, born date, last_login timestamptz,
               unique (code, email));
             create table orders (id int primary key, user_email text, amount int not null default 0);
             insert into users values
               (1, 'alice@example.com', 'Alice Liddell', 'free', 30, 'c1', 'tok_secret_alice', '1990-04-01', '2026-11-01 10:00+00'),
               (2, 'bob@example.com', 'Bob Builder', 'pro', 41, 'c2', 'tok_secret_bob', '1985-02-03', '2026-11-02 11:00+00'),
               (3, 'carol@example.com', 'Carol', 'free', 25, 'c3', 'tok_secret_carol', '2000-12-24', null),
               (4, 'carol@example.com', 'Carol', 'free', 26, 'c4', null, null, null);
             insert into orders values (1, 'alice@example.com', 10), (2, 'bob@example.com', 20), (3, 'alice@example.com', 5);"
            .to_string(),
        timeout: Duration::from_secs(10),
        dry_run: false,
    };
    engine.apply_migration(&db, &key, &up, &sync).await.unwrap();
    {
        // The ANALYZE of the spike (there by the superuser): the owner analyzes its tables.
        let mut owner = session(&engine, &db, RoleKind::Owner).await;
        psql(&mut owner, "analyze users; analyze orders")
            .await
            .unwrap();
        close(owner).await;
    }
    let mut s = Spike {
        engine: &engine,
        db: db.clone(),
        created: created.clone(),
        checks: Checks {
            failed: Vec::new(),
            passed: 0,
        },
    };
    use RoleKind::*;
    let mask = s.name(Mask);
    let other_owner = role_name(&other, Owner);

    // == (1) no role reaches another role
    for kind in RoleKind::SESSION_KINDS {
        let me = s.name(kind);
        let k = kind.as_str();
        let peer = s.name(if kind == Read { Owner } else { Read });
        s.ok(
            &format!("({k}) session_user = current_user = itself"),
            kind,
            &format!("select session_user = '{me}' and current_user = '{me}'"),
            Some("t"),
        )
        .await;
        s.ok(
            &format!("({k}) RESET ROLE stays itself"),
            kind,
            &format!("reset role; select current_user = '{me}'"),
            Some("t"),
        )
        .await;
        for target in [
            peer.as_str(),
            "ruum_dbhost",
            mask.as_str(),
            other_owner.as_str(),
        ] {
            s.err(
                &format!("({k}) SET ROLE {target}"),
                kind,
                &format!("set role {target}"),
                &["permission denied to set role"],
            )
            .await;
            // Only the owner kind may run set_config at all (review Q-1, S-1); for it, the
            // role change itself is refused.
            s.err(
                &format!("({k}) set_config role {target}"),
                kind,
                &format!("select set_config('role', '{target}', false)"),
                &[if kind == Owner {
                    "permission denied to set role"
                } else {
                    "permission denied for function set_config"
                }],
            )
            .await;
            s.err(
                &format!("({k}) DO SET ROLE {target}"),
                kind,
                &format!("do $$ begin execute 'set role {target}'; end $$"),
                &["permission denied to set role"],
            )
            .await;
        }
        s.err(
            &format!("({k}) SET ROLE postgres"),
            kind,
            "set role postgres",
            &["permission denied to set role"],
        )
        .await;
        s.err(
            &format!("({k}) SET SESSION AUTHORIZATION"),
            kind,
            &format!("set session authorization {peer}"),
            &["permission denied to set session authorization"],
        )
        .await;
        s.err(
            &format!("({k}) TEMP table"),
            kind,
            "create temp table x (i int)",
            &["permission denied to create temporary tables"],
        )
        .await;
        s.err(
            &format!("({k}) ruum_mask.key"),
            kind,
            "select * from ruum_mask.key",
            &["permission denied for schema ruum_mask"],
        )
        .await;
        s.err(
            &format!("({k}) ruum_mask.pseudonym by name"),
            kind,
            "select ruum_mask.pseudonym('x'::text)",
            &["permission denied for schema ruum_mask"],
        )
        .await;
        s.err(
            &format!("({k}) create schema"),
            kind,
            "create schema evil",
            &["permission denied for database"],
        )
        .await;
        s.err(
            &format!("({k}) prepare transaction"),
            kind,
            "begin; prepare transaction 'p'",
            &["prepared transactions are disabled"],
        )
        .await;
    }
    // Connecting to another database: in `a_role_of_one_database_cannot_read_another`.
    s.ok(
        "(owner) a SECURITY DEFINER function of the owner",
        Owner,
        &format!(
            "create function public.sw() returns text language plpgsql security definer \
           as $f$ begin perform set_config('role', '{}', false); return current_user; end $f$; \
         create function public.sw2() returns text language plpgsql \
           as $f$ begin execute 'set role ruum_dbhost'; return current_user; end $f$;",
            s.name(Read)
        ),
        None,
    )
    .await;
    s.err(
        "(owner) set_config('role') in a SECURITY DEFINER function",
        Owner,
        "select public.sw()",
        &[
            "cannot set parameter \"role\" within security-definer function",
            "permission denied to set role",
        ],
    )
    .await;
    s.err(
        "(owner) SET ROLE ruum_dbhost in a function",
        Owner,
        "select public.sw2()",
        &["permission denied to set role"],
    )
    .await;

    // == (2) masked reads: pseudonyms in every clause; no base table, no key, no stats
    s.ok(
        "masked_read: 'users' is the lens view",
        MaskedRead,
        "select 'users'::regclass::text",
        Some("users"),
    )
    .await;
    s.ok(
        "masked_read: search_path",
        MaskedRead,
        "show search_path",
        Some("ruum_masked, extensions"),
    )
    .await;
    s.ok(
        "masked_read: email of row 1 is the Rust pseudonym",
        MaskedRead,
        "select email from users where id = 1",
        Some(ALICE),
    )
    .await;
    s.noleak(
        "masked_read: select *",
        MaskedRead,
        "select * from users order by id",
    )
    .await;
    s.ok(
        "masked_read: the secret is the constant or NULL",
        MaskedRead,
        "select coalesce(api_token, 'NULL') from users order by id",
        Some("ruum:hidden\nruum:hidden\nruum:hidden\nNULL"),
    )
    .await;
    s.ok("masked_read: date and timestamptz are text pseudonyms", MaskedRead, "select born like 'p\\_%' and last_login like 'p\\_%' and pg_typeof(born)::text = 'text' from users where id = 1", Some("t")).await;
    s.ok(
        "masked_read: WHERE LIKE 'a%' sees pseudonyms",
        MaskedRead,
        "select count(*) from users where email like 'a%'",
        Some("0"),
    )
    .await;
    s.ok(
        "masked_read: WHERE = real value",
        MaskedRead,
        "select count(*) from users where email = 'alice@example.com'",
        Some("0"),
    )
    .await;
    s.ok(
        "masked_read: WHERE = pseudonym",
        MaskedRead,
        &format!("select count(*) from users where email = '{ALICE}'"),
        Some("1"),
    )
    .await;
    s.noleak(
        "masked_read: ORDER BY",
        MaskedRead,
        "select email from users order by email",
    )
    .await;
    s.noleak(
        "masked_read: GROUP BY, HAVING",
        MaskedRead,
        "select email, count(*) from users group by email having count(*) > 0 order by 2 desc, 1",
    )
    .await;
    s.ok(
        "masked_read: GROUP BY keeps equal values equal",
        MaskedRead,
        "select max(c) from (select count(*) c from users group by email) s",
        Some("2"),
    )
    .await;
    s.noleak(
        "masked_read: min, max, string_agg",
        MaskedRead,
        "select min(email), max(name), string_agg(email, ',') from users",
    )
    .await;
    s.ok(
        "masked_read: length is the pseudonym's",
        MaskedRead,
        "select string_agg(distinct length(email)::text, ',') from users",
        Some("14"),
    )
    .await;
    s.ok("masked_read: JOIN on personal columns", MaskedRead, "select string_agg(u.id || ':' || o.amount, ',' order by u.id, o.amount) from users u join orders o on o.user_email = u.email", Some("1:5,1:10,2:20")).await;
    s.ok(
        "masked_read: JOIN personal = normal",
        MaskedRead,
        "select count(*) from users u join orders o on u.code = o.user_email",
        Some("0"),
    )
    .await;
    s.ok("masked_read: subquery and CTE", MaskedRead, "with x as (select email from users) select count(*) from x where email like '%@%' or email in (select 'alice@example.com')", Some("0")).await;
    s.noleak(
        "masked_read: EXPLAIN",
        MaskedRead,
        "explain select * from users where email = 'zzz'",
    )
    .await;
    s.noleak(
        "masked_read: view definitions",
        MaskedRead,
        "select pg_get_viewdef('users'::regclass) || pg_get_viewdef('orders'::regclass)",
    )
    .await;
    s.noleak("masked_read: function bodies", MaskedRead, "select string_agg(prosrc, ' ') from pg_proc where pronamespace = 'ruum_mask'::regnamespace").await;
    s.err(
        "masked_read: public.users",
        MaskedRead,
        "select * from public.users",
        &["permission denied for schema public"],
    )
    .await;
    s.err(
        "masked_read: query_to_xml",
        MaskedRead,
        "select query_to_xml('select * from public.users', true, true, '')",
        &["permission denied"],
    )
    .await;
    s.ok(
        "masked_read: pg_stats shows nothing",
        MaskedRead,
        "select count(*) from pg_stats where tablename in ('users', 'orders')",
        Some("0"),
    )
    .await;
    s.err(
        "masked_read: pg_statistic",
        MaskedRead,
        "select * from pg_statistic",
        &["permission denied"],
    )
    .await;
    s.noleak("masked_read: settings and seclabels", MaskedRead, "select (select count(*) from pg_seclabels)::text || string_agg(setting, ',') from pg_settings").await;
    s.err(
        "masked_read: create a function",
        MaskedRead,
        "create function f() returns int language sql as 'select 1'",
        &["no schema has been selected", "permission denied"],
    )
    .await;
    s.err(
        "masked_read: create in ruum_masked",
        MaskedRead,
        "create view ruum_masked.v as select 1",
        &["permission denied for schema ruum_masked"],
    )
    .await;
    s.ok(
        "read (nosecret): personal visible, secret masked",
        Read,
        "select email || '|' || coalesce(api_token, '') from users where id = 1",
        Some("alice@example.com|ruum:hidden"),
    )
    .await;
    s.ok(
        "read (nosecret): WHERE on the secret",
        Read,
        "select count(*) from users where api_token like 'tok%'",
        Some("0"),
    )
    .await;
    s.ok(
        "secret_read (base): sees the secret",
        SecretRead,
        "select api_token from users where id = 1",
        Some("tok_secret_alice"),
    )
    .await;

    // == (3) the pseudonym function runs from a view only
    s.ok(
        "masked_read: the view calls pseudonym()",
        MaskedRead,
        "select count(*) from users where email like 'p\\_%'",
        Some("4"),
    )
    .await;
    s.err(
        "masked_read: hmac12",
        MaskedRead,
        "select ruum_mask.hmac12('x')",
        &["permission denied"],
    )
    .await;
    s.ok(
        "masked_read: a constant comes back as it is",
        MaskedRead,
        "select 'alice@example.com'",
        Some("alice@example.com"),
    )
    .await;

    // == (4) masked_write: plain columns only; WHERE compares pseudonyms
    s.ok(
        "masked_write: UPDATE a plain column",
        MaskedWrite,
        "update users set plan = 'pro' where id = 1 returning plan",
        Some("pro"),
    )
    .await;
    s.ok(
        "masked_write: INSERT plain columns",
        MaskedWrite,
        "insert into orders (id, amount) values (10, 99) returning amount",
        Some("99"),
    )
    .await;
    s.err(
        "masked_write: UPDATE a personal column",
        MaskedWrite,
        "update users set email = 'x@y' where id = 1",
        &["cannot update column \"email\""],
    )
    .await;
    s.err(
        "masked_write: UPDATE the secret",
        MaskedWrite,
        "update users set api_token = 'x' where id = 1",
        &["cannot update column \"api_token\""],
    )
    .await;
    s.err(
        "masked_write: INSERT a masked column",
        MaskedWrite,
        "insert into orders (id, user_email) values (11, 'x@y')",
        &["cannot insert into column \"user_email\""],
    )
    .await;
    s.ok("masked_write: WHERE = real value", MaskedWrite, "with u as (update users set plan = 'x' where email = 'alice@example.com' returning 1) select count(*) from u", Some("0")).await;
    s.ok("masked_write: WHERE = pseudonym", MaskedWrite, &format!("with u as (update users set plan = 'y' where email = '{ALICE}' returning 1) select count(*) from u"), Some("1")).await;
    s.noleak(
        "masked_write: RETURNING a personal column",
        MaskedWrite,
        "update users set plan = 'z' where id = 2 returning email, name, api_token",
    )
    .await;
    let out = s
        .run(MaskedWrite, "update users set age = -1 where id = 1")
        .await;
    s.checks.check(
        "masked_write: a CHECK violation shows no value",
        matches!(&out, Err(e) if e.contains("users_age_check") && !e.contains("alice")),
        &format!("{out:?}"),
    );
    let out = s
        .run(Write, "update users set code = 'c3' where id = 4")
        .await;
    s.checks.check("write: a UNIQUE violation shows no key value", matches!(&out, Err(e) if e.contains("users_code_email_key") && !e.contains("carol@example.com")), &format!("{out:?}"));
    s.ok(
        "write (nosecret): UPDATE a personal column",
        Write,
        "update users set name = 'Carol C' where id = 4 returning name",
        Some("Carol C"),
    )
    .await;
    s.err(
        "write (nosecret): UPDATE the secret",
        Write,
        "update users set api_token = 'x' where id = 1",
        &["cannot update column \"api_token\""],
    )
    .await;
    for kind in [Read, MaskedRead, SecretRead] {
        s.err(
            &format!("{}: no writes", kind.as_str()),
            kind,
            "update users set plan = 'x' where id = 1",
            &["permission denied"],
        )
        .await;
    }

    // == (8) the owner kind (untrusted migrations) cannot widen a lens
    let mw = s.name(MaskedWrite);
    s.ok(
        "owner: GRANT USAGE on public grants nothing",
        Owner,
        &format!("grant usage on schema public to {mw}"),
        None,
    )
    .await;
    s.err(
        "masked_write: still no USAGE on public",
        MaskedWrite,
        "select email from public.users",
        &["permission denied for schema public"],
    )
    .await;
    s.ok(
        "owner: may GRANT SELECT on its table",
        Owner,
        &format!("grant select on users to {mw}"),
        None,
    )
    .await;
    s.err(
        "masked_write: that grant is useless",
        MaskedWrite,
        "select email from public.users",
        &["permission denied for schema public"],
    )
    .await;
    s.err(
        "owner: replace a lens view",
        Owner,
        "create or replace view ruum_masked.users as select * from public.users",
        &["permission denied for schema ruum_masked", "must be owner"],
    )
    .await;
    s.err(
        "owner: create an extension",
        Owner,
        "create extension pgcrypto",
        &["permission denied"],
    )
    .await;
    s.err(
        "owner: default privileges of mask",
        Owner,
        &format!("alter default privileges for role {mask} grant select on tables to {mw}"),
        &["permission denied", "must be member"],
    )
    .await;
    s.err(
        "owner: grant mask to itself",
        Owner,
        &format!("grant {mask} to {}", s.name(Owner)),
        &["permission denied", "must have admin option"],
    )
    .await;
    // Finding (f): with the lens views in place a retype fails; through the engine it works.
    s.err(
        "finding (f): ALTER COLUMN TYPE with the views in place",
        Owner,
        "alter table users alter column age type bigint",
        &["cannot alter type of a column used by a view or rule"],
    )
    .await;

    let failed = s.checks.failed.clone();
    assert!(
        failed.is_empty(),
        "{} passed, {} failed:\n{}",
        s.checks.passed,
        failed.len(),
        failed.join("\n")
    );
    assert!(s.checks.passed > 150, "{} checks", s.checks.passed);
}
