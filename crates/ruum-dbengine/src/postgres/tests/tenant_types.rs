//! Tenant-defined types against the lens and the hand-over (review-dbhost-2 item 2, decision
//! 0005 B-2, backlog S-1 and S-4, decision 0004-37).
//!
//! The tenant owns its types, so it may add casts, operators and functions for them. A personal
//! column of such a type must be masked like `secret` (the hidden constant, never a pseudonym),
//! and no tenant cast, operator or implicit conversion may run inside a `SECURITY DEFINER`
//! function of `ruum_mask` or on a `ruum_dbhost` connection. Every tenant function below calls
//! `public.mark`, which appends `<path>:<current_user>` to the session setting `ruum_test.hit`
//! and runs the role-grant probe of `hostile_copy`; the tests assert that nothing was marked
//! in the sessions they read and that no role gained a membership.

use super::hostile_copy::{escalation, taken};
use super::*;

/// The value columns of `t` and the path each one tries.
const VALUE_COLUMNS: &[&str] = &["e", "d", "dm", "c", "r", "a", "nc", "n", "s"];

/// The hidden constant of the lens.
const HIDDEN: &str = lens::SECRET_CONSTANT;

/// The migration of the tenant: types of every kind, with casts to `text`, an implicit cast
/// from a built-in type to a tenant type, an operator, a domain with a check, a view and
/// default privileges. (The view reads `id` only: a masked copy retypes a personal column, which
/// fails while a tenant view reads it; that is a separate gap.)
fn tenant_types_up(grants: &str) -> String {
    format!(
        "CREATE FUNCTION public.mark(path text) RETURNS text LANGUAGE plpgsql AS $f$
         BEGIN
           PERFORM pg_catalog.set_config('ruum_test.hit',
             coalesce(pg_catalog.current_setting('ruum_test.hit', true), '')
               || path || ':' || current_user || ',', false);
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

/// Reads `t` through the masked lens with the setting `ruum_test.hit` cleared first. Returns
/// the rows and the setting afterwards.
async fn masked_rows(engine: &PostgresEngine, db: &DatabaseRef) -> (Vec<String>, String) {
    let mut s = session(engine, db, RoleKind::MaskedRead).await;
    psql(
        &mut s,
        "select pg_catalog.set_config('ruum_test.hit', '', false)",
    )
    .await
    .unwrap();
    let rows = psql(
        &mut s,
        &format!("select {} from t order by id", VALUE_COLUMNS.join(", ")),
    )
    .await
    .unwrap();
    let hit = psql(
        &mut s,
        "select pg_catalog.current_setting('ruum_test.hit', true)",
    )
    .await
    .unwrap();
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
    // The rights of step 2 of a masked copy (copy.rs `pseudonyms_for_owner`).
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
