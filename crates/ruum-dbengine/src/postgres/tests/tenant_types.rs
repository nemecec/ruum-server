//! Tenant-defined types against the lens and the hand-over (review-dbhost-2 item 2, decision
//! 0005 B-2, backlog S-1 and S-4, decision 0004-37).
//!
//! The tenant owns its types, so it may add casts, operators and functions for them. A personal
//! column of such a type must be masked like `secret` (the hidden constant, never a pseudonym),
//! and no tenant cast, operator or implicit conversion may run inside a `SECURITY DEFINER`
//! function of `ruum_mask` or on a `ruum_dbhost` connection. Every tenant function below calls
//! `public.mark`, which appends `<path>:<current_user>` to the session setting `ruum_test.hit`
//! and runs the role-grant probe of `hostile_copy`; the tests assert that nothing was marked
//! in the sessions they read and that no role gained a membership. Only the `owner` kind may run
//! `set_config` (review Q-1, S-1), so `mark` takes `current_user` where it runs and hands the
//! entry to `public.hit`, a `SECURITY DEFINER` function of the owner that writes the setting.

use super::hostile_copy::{escalation, taken};
use super::*;

/// The value columns of `t` and the path each one tries.
const VALUE_COLUMNS: &[&str] = &["e", "d", "dm", "c", "r", "a", "nc", "n", "s"];

/// The hidden constant of the lens.
const HIDDEN: &str = lens::SECRET_CONSTANT;

/// The migration of the tenant: types of every kind, with casts to `text`, an implicit cast
/// from a built-in type to a tenant type, an operator, a domain with a check, a view and
/// default privileges. (The view reads `id` only; a view over a retyped personal column is the
/// case of `hostile_copy::a_masked_copy_makes_views_over_masked_columns_again`, backlog S-8.)
fn tenant_types_up(grants: &str) -> String {
    format!(
        "CREATE FUNCTION public.hit(entry text) RETURNS void LANGUAGE sql SECURITY DEFINER AS $f$
           SELECT pg_catalog.set_config('ruum_test.hit',
             coalesce(pg_catalog.current_setting('ruum_test.hit', true), '') || entry, false);
         $f$;
         CREATE FUNCTION public.mark(path text) RETURNS text LANGUAGE plpgsql AS $f$
         BEGIN
           PERFORM public.hit(path || ':' || current_user || ',');
           {grants}
           RETURN path;
         END $f$;
         CREATE TYPE public.mood AS ENUM ('happy', 'sad');
         CREATE FUNCTION public.mood_text(public.mood) RETURNS text LANGUAGE plpgsql AS $f$
           BEGIN RETURN public.mark('enum-cast'); END $f$;
         CREATE CAST (public.mood AS text) WITH FUNCTION public.mood_text(public.mood) AS IMPLICIT;
         CREATE DOMAIN public.email AS text CHECK (public.mark('domain-check') IS NOT NULL);
         CREATE DOMAIN public.mood_d AS public.mood;
         CREATE TYPE public.pair AS (a text, b integer);
         CREATE FUNCTION public.pair_text(public.pair) RETURNS text LANGUAGE plpgsql AS $f$
           BEGIN RETURN public.mark('composite-cast'); END $f$;
         CREATE CAST (public.pair AS text) WITH FUNCTION public.pair_text(public.pair) AS ASSIGNMENT;
         CREATE TYPE public.span AS RANGE (subtype = integer);
         CREATE FUNCTION public.span_text(public.span) RETURNS text LANGUAGE plpgsql AS $f$
           BEGIN RETURN public.mark('range-cast'); END $f$;
         CREATE CAST (public.span AS text) WITH FUNCTION public.span_text(public.span);
         CREATE FUNCTION public.moods_text(public.mood[]) RETURNS text LANGUAGE plpgsql AS $f$
           BEGIN RETURN public.mark('array-cast'); END $f$;
         CREATE CAST (public.mood[] AS text) WITH FUNCTION public.moods_text(public.mood[]);
         CREATE FUNCTION public.int_mood(integer) RETURNS public.mood LANGUAGE plpgsql AS $f$
           BEGIN PERFORM public.mark('implicit-conversion'); RETURN 'happy'; END $f$;
         CREATE CAST (integer AS public.mood) WITH FUNCTION public.int_mood(integer) AS IMPLICIT;
         CREATE FUNCTION public.mood_eq(public.mood, text) RETURNS boolean LANGUAGE plpgsql AS $f$
           BEGIN PERFORM public.mark('operator'); RETURN true; END $f$;
         CREATE OPERATOR public.= (LEFTARG = public.mood, RIGHTARG = text,
           FUNCTION = public.mood_eq);
         CREATE TABLE t (id integer PRIMARY KEY, e public.mood, d public.email,
           dm public.mood_d, c public.pair, r public.span, a public.mood[], nc public.pair,
           n integer, s text);
         CREATE VIEW public.v AS SELECT id FROM t;
         ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL ON TABLES TO PUBLIC;
         ALTER DEFAULT PRIVILEGES GRANT ALL ON FUNCTIONS TO PUBLIC;
         ALTER DEFAULT PRIVILEGES GRANT ALL ON TYPES TO PUBLIC;"
    )
}

/// `CREATE` of `mask` on the lens schemas, as `ruum_mask|ruum_masked|ruum_nosecret`.
async fn mask_creates(engine: &PostgresEngine, created: &CreatedDatabase) -> String {
    let mut host = dbhost(engine, &created.database).await;
    let mask = lens::literal(&role_name(created, RoleKind::Mask));
    let out = psql(
        &mut host,
        &format!(
            "select has_schema_privilege({mask}, 'ruum_mask', 'create')::text || '|' || \
                    has_schema_privilege({mask}, 'ruum_masked', 'create')::text || '|' || \
                    has_schema_privilege({mask}, 'ruum_nosecret', 'create')::text"
        ),
    )
    .await
    .unwrap();
    close(host).await;
    out
}

fn all_personal() -> ClassSync {
    ClassSync {
        personal: VALUE_COLUMNS.to_vec(),
        secret: vec![],
    }
}

/// A database of a tenant with its own types, casts, operators, domains, a view and default
/// privileges, and two rows (one with values, one with `NULL`s).
async fn tenant_types_source<'e>(
    engine: &'e PostgresEngine,
    other: &CreatedDatabase,
) -> Fixture<'e> {
    let f = Fixture::new(engine).await;
    let request = MigrationRequest {
        up: tenant_types_up(&escalation(other, &f.created)),
        timeout: Duration::from_secs(10),
        dry_run: false,
    };
    engine
        .apply_migration(f.db(), &f.key, &request, &all_personal())
        .await
        .expect("the tenant's types, casts and operators");
    f.run(
        RoleKind::Owner,
        StatementMode::Write,
        "INSERT INTO t VALUES \
           (1, 'happy', 'a@example.com', 'sad', ROW('x', 1), '[1,5)', '{happy}', ROW(NULL, NULL), 42, 'alice'), \
           (2, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)",
        vec![],
    )
    .await
    .expect("rows");
    f
}

/// Reads `t` through the masked lens in a new session, which starts without the setting
/// `ruum_test.hit` (the masked reader may not run `set_config`, nor `current_setting`: it reads
/// the mark with `SHOW`, and defines it empty with `SET` before the read). Returns the rows and
/// the setting afterwards.
async fn masked_rows(engine: &PostgresEngine, db: &DatabaseRef) -> (Vec<String>, String) {
    let mut s = session(engine, db, RoleKind::MaskedRead).await;
    let fresh = psql(&mut s, "show ruum_test.hit")
        .await
        .expect_err("a new session starts with no mark");
    assert!(
        fresh.contains("unrecognized configuration parameter"),
        "a new session starts with no mark: {fresh}"
    );
    psql(&mut s, "set ruum_test.hit = ''").await.unwrap();
    let rows = psql(
        &mut s,
        &format!("select {} from t order by id", VALUE_COLUMNS.join(", ")),
    )
    .await
    .unwrap();
    let hit = psql(&mut s, "show ruum_test.hit").await.unwrap();
    close(s).await;
    (rows.lines().map(str::to_string).collect(), hit)
}

/// A personal column of a tenant-defined type (enum, domain, domain over an enum, composite,
/// composite whose fields are all `NULL`, range, array of an enum) shows the hidden constant;
/// a built-in one shows a pseudonym. The tenant's casts, operator, implicit conversion and
/// domain check never run while a masked reader reads, and agents cannot group on such a
/// column.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_personal_column_of_a_tenant_type_is_hidden_and_runs_no_tenant_code() {
    let Some(engine) = engine().await else { return };
    let other = Fixture::new(&engine).await;
    let f = tenant_types_source(&engine, &other.created).await;
    let (rows, hit) = masked_rows(&engine, f.db()).await;
    assert_eq!(
        hit, "",
        "no tenant function ran while the masked reader read"
    );
    assert_eq!(rows.len(), 2, "{rows:?}");
    let cells: Vec<&str> = rows[0].split('|').collect();
    for (column, cell) in VALUE_COLUMNS.iter().zip(&cells) {
        match *column {
            "n" | "s" => assert!(cell.starts_with("p_"), "{column}: {cell}"),
            _ => assert_eq!(*cell, HIDDEN, "{column} is masked like secret"),
        }
    }
    assert_eq!(
        rows[1],
        "|".repeat(VALUE_COLUMNS.len() - 1),
        "NULL stays NULL"
    );
    // No pseudonym function is called on a tenant-typed column.
    let mut host = dbhost(&engine, f.db()).await;
    let view = psql(
        &mut host,
        "select pg_get_viewdef('ruum_masked.t'::regclass)",
    )
    .await
    .unwrap();
    close(host).await;
    for column in ["e", "d", "dm", "c", "r", "a", "nc"] {
        assert!(
            !view.contains(&format!("pseudonym_other({column})"))
                && !view.contains(&format!("pseudonym({column})")),
            "{column}: {view}"
        );
    }
    // One group only: the constant.
    let mut s = session(&engine, f.db(), RoleKind::MaskedRead).await;
    assert_eq!(
        psql(
            &mut s,
            "select count(distinct e) || '|' || count(distinct c) from t"
        )
        .await
        .unwrap(),
        "1|1"
    );
    close(s).await;
    assert_eq!(
        taken(&engine, &[&f.created, &other.created], &other.created).await,
        "",
        "the migration and the lens rebuild granted nothing"
    );
    assert_eq!(mask_creates(&engine, &f.created).await, "false|false|false");
}

/// A direct call of `pseudonym_other` with a value of a tenant-defined type (as tenant code in
/// step 2 of a masked copy could make, while the copy's owner holds `EXECUTE`) fails before
/// any conversion; a built-in base type still gets a pseudonym.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pseudonym_other_refuses_a_tenant_type_before_it_converts() {
    let Some(engine) = engine().await else { return };
    let other = Fixture::new(&engine).await;
    let f = tenant_types_source(&engine, &other.created).await;
    let owner = role_name(&f.created, RoleKind::Owner);
    let mask = role_name(&f.created, RoleKind::Mask);
    // A role that may call `pseudonym_other` directly (stricter than step 2 of a masked copy,
    // whose owner reaches it only through the wrappers of `ruum_copy`, copy.rs
    // `make_copy_entry_points`).
    let rights = |verb: &str, to: &str| {
        format!(
            "{verb} usage on schema ruum_mask {to} {o}; set role {m}; \
             {verb} execute on function ruum_mask.pseudonym_other(anyelement) {to} {o}; reset role",
            o = lens::ident(&owner),
            m = lens::ident(&mask)
        )
    };
    let mut host = dbhost(&engine, f.db()).await;
    psql(&mut host, &rights("grant", "to")).await.unwrap();
    let mut s = session(&engine, f.db(), RoleKind::Owner).await;
    psql(
        &mut s,
        "select pg_catalog.set_config('ruum_test.hit', '', false)",
    )
    .await
    .unwrap();
    for value in [
        "'happy'::public.mood",
        "'sad'::public.mood_d",
        "ROW('x', 1)::public.pair",
        "'[1,2)'::public.span",
        "ARRAY['happy']::public.mood[]",
        "'[1,2)'::int4range",
    ] {
        let err = psql(
            &mut s,
            &format!("select ruum_mask.pseudonym_other({value})"),
        )
        .await
        .expect_err(value);
        assert!(err.contains("not a built-in base type"), "{value}: {err}");
    }
    for value in ["interval '1 day'", "'2026-11-09'::date", "ARRAY[1, 2]"] {
        let out = psql(
            &mut s,
            &format!("select ruum_mask.pseudonym_other({value})"),
        )
        .await
        .unwrap();
        assert!(out.starts_with("p_"), "{value}: {out}");
    }
    assert_eq!(
        psql(
            &mut s,
            "select pg_catalog.current_setting('ruum_test.hit', true)"
        )
        .await
        .unwrap(),
        "",
        "no tenant cast ran inside pseudonym_other"
    );
    close(s).await;
    psql(&mut host, &rights("revoke", "from")).await.unwrap();
    close(host).await;
    assert_eq!(
        taken(&engine, &[&f.created, &other.created], &other.created).await,
        ""
    );
}

/// Steps 1 and 3 of every hand-over (full, schema-only and masked copies, a restore), and
/// `sync_lens`, on a database with tenant types, casts, an operator, domains, a view and
/// default privileges (backlog S-4, review-dbhost item 1): nothing grants a role, `mask` holds
/// no `CREATE` afterwards, and a masked copy holds no value of a tenant-typed personal column.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hand_overs_of_a_database_with_tenant_types_grant_nothing() {
    let Some(engine) = engine().await else { return };
    let other = Fixture::new(&engine).await;
    let f = tenant_types_source(&engine, &other.created).await;
    engine
        .sync_lens(f.db(), &f.key, &all_personal())
        .await
        .expect("sync_lens");
    assert_eq!(
        taken(&engine, &[&f.created, &other.created], &other.created).await,
        "",
        "sync_lens granted nothing"
    );
    assert_eq!(mask_creates(&engine, &f.created).await, "false|false|false");
    let col = |c: &str| ColumnRef {
        schema: "public".into(),
        table: "t".into(),
        column: c.into(),
    };
    let plan = MaskPlan {
        columns: VALUE_COLUMNS
            .iter()
            .map(|c| (col(c), MaskAs::Pseudonym))
            .collect(),
        planned_from: None,
    };
    for data in [CopyData::Full, CopyData::SchemaOnly, CopyData::Masked(plan)] {
        let (key, _) = conformance::test_key();
        let copy = engine
            .copy_database(
                f.db(),
                &f.key,
                &conformance::new_database(f.db().tenant_id),
                &data,
                &key,
            )
            .await
            .unwrap_or_else(|e| panic!("{data:?}: the copy succeeds: {e:?}"));
        assert_eq!(
            taken(
                &engine,
                &[&f.created, &other.created, &copy],
                &other.created
            )
            .await,
            "",
            "{data:?}: the hand-over granted nothing"
        );
        assert_eq!(
            mask_creates(&engine, &copy).await,
            "false|false|false",
            "{data:?}: mask holds no CREATE in the copy"
        );
        let (rows, hit) = masked_rows(&engine, &copy.database).await;
        assert_eq!(
            hit, "",
            "{data:?}: no tenant function ran for the masked reader"
        );
        if let CopyData::Masked(_) = data {
            let mut owner = session(&engine, &copy.database, RoleKind::Owner).await;
            assert_eq!(
                psql(
                    &mut owner,
                    "select coalesce(e, '-') || coalesce(d, '-') || coalesce(dm, '-') || \
                            coalesce(c, '-') || coalesce(r, '-') || coalesce(a, '-') || \
                            coalesce(nc, '-') || '|' || left(n, 2) || left(s, 2) \
                       from t where id = 1",
                )
                .await
                .unwrap(),
                "-------|p_p_",
                "the tenant-typed personal columns hold no value; the built-in ones a pseudonym"
            );
            close(owner).await;
        } else if let CopyData::Full = data {
            assert!(rows[0].starts_with(HIDDEN), "{rows:?}");
        }
    }
    let point = engine
        .create_restore_point(f.db())
        .await
        .expect("restore point");
    let (key, _) = conformance::test_key();
    let restored = engine
        .restore_into_new(
            f.db(),
            &point,
            &conformance::new_database(f.db().tenant_id),
            &key,
        )
        .await
        .expect("restore into a new database");
    assert_eq!(
        taken(
            &engine,
            &[&f.created, &other.created, &restored],
            &other.created
        )
        .await,
        "",
        "the restore granted nothing"
    );
    assert_eq!(mask_creates(&engine, &restored).await, "false|false|false");
}

/// A pooled admin connection keeps `search_path = pg_catalog, pg_temp` after `SET` followed by
/// `RESET ALL` or `DISCARD ALL`, also when the pool hands the same connection out again
/// (review-dbhost item 3).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reused_admin_connection_keeps_its_search_path() {
    let Some(engine) = engine().await else { return };
    let mut admin = engine.admin().await.expect("admin");
    let pid = psql(&mut admin, "select pg_backend_pid()").await.unwrap();
    for reset in ["RESET ALL", "DISCARD ALL", "RESET search_path"] {
        // One statement per call: DISCARD ALL cannot run in the implicit transaction of a
        // multi-statement query.
        psql(&mut admin, "SET search_path = public").await.unwrap();
        assert_eq!(
            psql(&mut admin, "show search_path").await.unwrap(),
            "public"
        );
        psql(&mut admin, reset).await.unwrap();
        assert_eq!(
            psql(&mut admin, "show search_path").await.unwrap(),
            "pg_catalog, pg_temp",
            "after {reset}"
        );
    }
    drop(admin);
    // The pool hands an idle connection out again; look for the same backend.
    let mut held = Vec::new();
    let mut found = false;
    for _ in 0..3 {
        // the admin pool holds at most 4 connections
        let mut c = engine.admin().await.expect("admin");
        let again = psql(&mut c, "select pg_backend_pid()").await.unwrap();
        assert_eq!(
            psql(&mut c, "show search_path").await.unwrap(),
            "pg_catalog, pg_temp",
            "a connection taken from the pool again"
        );
        held.push(c);
        if again == pid {
            found = true;
            break;
        }
    }
    assert!(found, "the pool handed the reset connection out again");
}

/// What every build of the lens leaves, whatever `mask` owned before (review-masking-2 F-1,
/// review-masking-1 F1): `mask` holds no `CREATE`, `pseudonym_other` checks the type first,
/// `hmac12` is `SECURITY DEFINER`, the key has no grant to `PUBLIC`, `mask` gives no default
/// privilege to another role, the masked reader sees what it saw before the changes (the same
/// key) and runs no tenant code, and the reader sees the clear value of `s`.
async fn assert_mask_objects_fresh(
    engine: &PostgresEngine,
    f: &Fixture<'_>,
    other: &CreatedDatabase,
    before: &[String],
    when: &str,
) {
    assert_eq!(
        mask_creates(engine, &f.created).await,
        "false|false|false",
        "{when}: mask holds no CREATE"
    );
    let mask = lens::literal(&role_name(&f.created, RoleKind::Mask));
    let mut host = dbhost(engine, f.db()).await;
    let state = psql(
        &mut host,
        &format!(
            "select (select (p.prosrc like '%not a built-in base type%')::text from pg_proc p \
                      where p.oid = 'ruum_mask.pseudonym_other(anyelement)'::regprocedure) \
                 || '|' || (select p.prosecdef::text from pg_proc p \
                      where p.oid = 'ruum_mask.hmac12(text)'::regprocedure) \
                 || '|' || (select count(*)::text from pg_class c, aclexplode(c.relacl) a \
                      where c.oid = 'ruum_mask.key'::regclass and a.grantee = 0) \
                 || '|' || (select count(*)::text from pg_default_acl d, aclexplode(d.defaclacl) a \
                      where d.defaclrole = {mask}::regrole and a.grantee <> d.defaclrole)"
        ),
    )
    .await
    .unwrap();
    close(host).await;
    assert_eq!(
        state, "true|true|0|0",
        "{when}: type check | definer | PUBLIC grants on the key | default privileges of mask"
    );
    let (rows, hit) = masked_rows(engine, f.db()).await;
    assert_eq!(
        hit, "",
        "{when}: no tenant function ran for the masked reader"
    );
    assert_eq!(
        rows, before,
        "{when}: the masked lens as before, with the same key"
    );
    let mut reader = session(engine, f.db(), RoleKind::Read).await;
    assert_eq!(
        psql(&mut reader, "select s from t where id = 1")
            .await
            .unwrap(),
        "alice",
        "{when}: the _RETURN rule that mask replaced is gone"
    );
    close(reader).await;
    assert_eq!(
        taken(engine, &[&f.created, other], other).await,
        "",
        "{when}: nothing granted a role"
    );
}

/// The state of a database from before the fix, and changes that need only the ownership of
/// `mask`'s objects, do not survive a lens build (`sync_lens`) or an in-place restore of a
/// restore point taken in that state (review-masking-2 F-1 (a) to (c); review-masking-1 F1).
/// Without `CREATE`, `mask` cannot `CREATE OR REPLACE` a view, but it can still replace the
/// view's `_RETURN` rule, alter a function, grant on the key and set default privileges.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_state_from_before_the_fix_does_not_survive_a_build_or_a_restore() {
    let Some(engine) = engine().await else { return };
    let other = Fixture::new(&engine).await;
    let f = tenant_types_source(&engine, &other.created).await;
    let (before, _) = masked_rows(&engine, f.db()).await;
    let m = lens::ident(&role_name(&f.created, RoleKind::Mask));
    let mut host = dbhost(&engine, f.db()).await;
    // Without CREATE: CREATE OR REPLACE fails ...
    let err = psql(
        &mut host,
        &format!("set role {m}; create or replace view ruum_masked.t as select 1 as id"),
    )
    .await
    .expect_err("mask has no CREATE on ruum_masked");
    assert!(err.contains("permission denied"), "{err}");
    // ... but an owner needs no CREATE for these.
    psql(
        &mut host,
        &format!(
            "reset role; set role {m};
             create or replace rule \"_RETURN\" as on select to ruum_nosecret.t do instead
               select id, e, d, dm, c, r, a, nc, n, 'tampered'::text as s from public.t;
             alter function ruum_mask.hmac12(text) security invoker;
             grant select on ruum_mask.key to public;
             alter default privileges grant select on tables to public;
             reset role"
        ),
    )
    .await
    .expect("changes that need only ownership");
    // The state of main before the fix: mask holds CREATE, pseudonym_other converts without a
    // type check, and the masked view calls it on the enum column.
    psql(
        &mut host,
        &format!(
            "grant create on schema ruum_mask, ruum_masked, ruum_nosecret to {m};
             set role {m};
             create or replace function ruum_mask.pseudonym_other(v anyelement) returns text
               language sql stable strict security definer set search_path = pg_catalog, pg_temp
               as $f$ select ruum_mask.hmac12(v::text) $f$;
             create or replace view ruum_masked.t as select id, ruum_mask.pseudonym_other(e) as e,
               null::text as d, null::text as dm, null::text as c, null::text as r,
               null::text as a, null::text as nc, null::text as n, null::text as s from public.t;
             reset role"
        ),
    )
    .await
    .expect("the state from before the fix");
    close(host).await;
    assert_eq!(mask_creates(&engine, &f.created).await, "true|true|true");
    let mut reader = session(&engine, f.db(), RoleKind::Read).await;
    assert_eq!(
        psql(&mut reader, "select s from t where id = 1")
            .await
            .unwrap(),
        "tampered"
    );
    close(reader).await;
    let point = engine
        .create_restore_point(f.db())
        .await
        .expect("a restore point in that state");
    engine
        .sync_lens(f.db(), &f.key, &all_personal())
        .await
        .expect("sync_lens");
    assert_mask_objects_fresh(&engine, &f, &other.created, &before, "after sync_lens").await;
    engine
        .restore_in_place(f.db(), &point, &f.key)
        .await
        .expect("restore in place");
    assert_mask_objects_fresh(
        &engine,
        &f,
        &other.created,
        &before,
        "after restore_in_place",
    )
    .await;
}

/// The source of the two tests below: an enum `mood` with an implicit cast to `text` whose
/// function runs `plant` (the tenant's code) and records what it saw in `seen`; a table `t` with
/// the secret column `sec` of that type and one row; a materialized view `mv` over `plant(sec)`,
/// unpopulated in the source. Step 2 of a masked copy makes every materialized view again after
/// the masking, with data, so `plant` runs as the copy's owner while the entry points of
/// `ruum_copy` exist (review-masking-1 F2). (Before FU1-R10-1 of the Kimi review of run 10,
/// step 2 ran `sec::text <> ''`, so the cast ran `plant` on the rows in clear; it no longer casts
/// a tenant type, see `a_masked_copy_runs_no_tenant_code_on_values_in_clear`.)
async fn planting_source<'e>(engine: &'e PostgresEngine, plant: &str) -> Fixture<'e> {
    let f = Fixture::new(engine).await;
    let sync = ClassSync {
        personal: vec![],
        secret: vec!["sec"],
    };
    let request = MigrationRequest {
        up: format!(
            "CREATE TYPE public.mood AS ENUM ('happy', 'sad');
             CREATE TABLE public.seen (what text);
             CREATE FUNCTION public.plant(public.mood) RETURNS text LANGUAGE plpgsql AS $f$
             DECLARE planted text := 'planted';
             BEGIN
               BEGIN
                 EXECUTE {plant};
               EXCEPTION WHEN OTHERS THEN planted := 'denied';
               END;
               INSERT INTO public.seen VALUES (
                 pg_catalog.has_schema_privilege('ruum_mask', 'USAGE')::text || '|' || planted);
               RETURN 'planted';
             END $f$;
             CREATE CAST (public.mood AS text) WITH FUNCTION public.plant(public.mood) AS IMPLICIT;
             CREATE TABLE t (id integer PRIMARY KEY, sec public.mood);
             CREATE MATERIALIZED VIEW public.mv AS SELECT public.plant(t.sec) AS p FROM public.t
               WITH NO DATA;",
            plant = lens::literal(plant)
        ),
        timeout: Duration::from_secs(10),
        dry_run: false,
    };
    engine
        .apply_migration(f.db(), &f.key, &request, &sync)
        .await
        .expect("migration");
    f.run(
        RoleKind::Owner,
        StatementMode::Write,
        "INSERT INTO t VALUES (1, 'happy')",
        vec![],
    )
    .await
    .expect("a row");
    f
}

/// A masked copy of [`planting_source`] with `sec` as `NULL`.
async fn masked_copy_of_planting(
    engine: &PostgresEngine,
    f: &Fixture<'_>,
) -> Result<CreatedDatabase, EngineError> {
    let plan = MaskPlan {
        columns: vec![(
            ColumnRef {
                schema: "public".into(),
                table: "t".into(),
                column: "sec".into(),
            },
            MaskAs::Null,
        )],
        planned_from: None,
    };
    let (key, _) = conformance::test_key();
    engine
        .copy_database(
            f.db(),
            &f.key,
            &conformance::new_database(f.db().tenant_id),
            &CopyData::Masked(plan),
            &key,
        )
        .await
}

/// Tenant code in step 2 of a masked copy runs as the copy's owner, which holds no right on
/// `ruum_mask` (backlog 2026-11-09 S-9, review-masking-1 F2): `has_schema_privilege` is false
/// while the code runs, and a view over `ruum_mask.key` cannot be made. The copy goes through,
/// with no such view.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step_2_of_a_masked_copy_has_no_right_on_ruum_mask() {
    let Some(engine) = engine().await else { return };
    let f = planting_source(
        &engine,
        "CREATE VIEW public.k AS SELECT id FROM ruum_mask.key",
    )
    .await;
    let copy = masked_copy_of_planting(&engine, &f)
        .await
        .expect("the copy goes through");
    let mut owner = session(&engine, &copy.database, RoleKind::Owner).await;
    assert_eq!(
        psql(
            &mut owner,
            "select string_agg(distinct what, ',') || '/' || (to_regclass('public.k') is null)::text \
               from public.seen",
        )
        .await
        .unwrap(),
        "false|denied/true",
        "the owner had no USAGE on ruum_mask in step 2, and no view over the key was made"
    );
    close(owner).await;
    let mut host = dbhost(&engine, &copy.database).await;
    assert_eq!(
        psql(
            &mut host,
            "select (to_regnamespace('ruum_copy') is null)::text",
        )
        .await
        .unwrap(),
        "true",
        "the entry points of step 2 are gone"
    );
    close(host).await;
}

/// Tenant code in step 2 of a masked copy may call the pseudonym functions through the entry
/// points of `ruum_copy`. An object it makes there that points at an entry point would keep a
/// reference into the platform's schemas, so step 3 refuses the copy instead (review-masking-1
/// F2).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_masked_copy_refuses_an_object_that_points_into_the_lens_schemas() {
    let Some(engine) = engine().await else { return };
    let f = planting_source(
        &engine,
        "CREATE VIEW public.k AS SELECT ruum_copy.pseudonym('x'::text) AS p",
    )
    .await;
    let err = masked_copy_of_planting(&engine, &f)
        .await
        .expect_err("the copy is refused");
    let err = format!("{err:?}");
    assert!(err.contains("depend on the lens schemas"), "{err}");
    assert!(err.contains("_RETURN"), "{err}");
    let mut host = dbhost(&engine, f.db()).await;
    assert_eq!(
        psql(&mut host, "select (to_regclass('public.k') is null)::text")
            .await
            .unwrap(),
        "true",
        "the source has no such view"
    );
    close(host).await;
}

/// A secret column of a composite type whose fields are all `NULL` shows the constant, in the
/// masked lens and in the lens of the readers (review-masking-2 F-3).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_secret_composite_with_null_fields_shows_the_constant() {
    let Some(engine) = engine().await else { return };
    let other = Fixture::new(&engine).await;
    let f = tenant_types_source(&engine, &other.created).await;
    let sync = ClassSync {
        personal: vec![],
        secret: vec!["nc"],
    };
    engine
        .sync_lens(f.db(), &f.key, &sync)
        .await
        .expect("sync_lens");
    for kind in [RoleKind::MaskedRead, RoleKind::Read] {
        let mut s = session(&engine, f.db(), kind).await;
        assert_eq!(
            psql(&mut s, "select coalesce(nc::text, '-') from t order by id")
                .await
                .unwrap(),
            format!("{HIDDEN}\n-"),
            "{kind:?}"
        );
        close(s).await;
    }
}

/// A personal column of a domain over `text` is hidden by the guard of the lens and emptied by
/// the guard of the masked copy on their own: the type check of `pseudonym_other` lets a domain
/// over a built-in type through (it arrives as its base type), so only the guards stop a
/// pseudonym here (review-masking-2 F-3, the kills of the two guard mutations).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_guards_hide_a_domain_over_a_built_in_type_on_their_own() {
    let Some(engine) = engine().await else { return };
    let f = Fixture::new(&engine).await;
    let sync = ClassSync {
        personal: vec!["p"],
        secret: vec![],
    };
    let request = MigrationRequest {
        up: "CREATE DOMAIN public.plain AS text;
             CREATE TABLE u (id integer PRIMARY KEY, p public.plain);"
            .into(),
        timeout: Duration::from_secs(10),
        dry_run: false,
    };
    engine
        .apply_migration(f.db(), &f.key, &request, &sync)
        .await
        .expect("migration");
    f.run(
        RoleKind::Owner,
        StatementMode::Write,
        "INSERT INTO u VALUES (1, 'bob'), (2, NULL)",
        vec![],
    )
    .await
    .expect("rows");
    let mut s = session(&engine, f.db(), RoleKind::MaskedRead).await;
    assert_eq!(
        psql(&mut s, "select coalesce(p, '-') from u order by id")
            .await
            .unwrap(),
        format!("{HIDDEN}\n-"),
        "the masked lens hides the domain column"
    );
    close(s).await;
    let plan = MaskPlan {
        columns: vec![(
            ColumnRef {
                schema: "public".into(),
                table: "u".into(),
                column: "p".into(),
            },
            MaskAs::Pseudonym,
        )],
        planned_from: None,
    };
    let (key, _) = conformance::test_key();
    let copy = engine
        .copy_database(
            f.db(),
            &f.key,
            &conformance::new_database(f.db().tenant_id),
            &CopyData::Masked(plan),
            &key,
        )
        .await
        .expect("a masked copy");
    let mut owner = session(&engine, &copy.database, RoleKind::Owner).await;
    assert_eq!(
        psql(&mut owner, "select coalesce(p, '-') from u order by id")
            .await
            .unwrap(),
        "-\n-",
        "the masked copy holds no value of the domain column"
    );
    close(owner).await;
}
