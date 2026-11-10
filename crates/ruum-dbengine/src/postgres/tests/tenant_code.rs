//! Tenant code and masked sessions (decision 0004-38, design 8.5; review-Q-2-2 P-1,
//! review-Q-2-3 R-1 and N-2, the S-21 panel).
//!
//! - The catalog check finds each kind of tenant code on its own (one clause per kind), and
//!   never the functions of the allow-listed extensions.
//! - The engine's line: a `masked_write` or `write` session never writes where tenant code
//!   exists, before the statement and again after it (the race of a trigger made in between).
//! - A guard: the catalog places that can hold a function or an expression are the ones the
//!   check knows, so a Postgres upgrade that adds one fails here.

use super::*;
use crate::postgres::depends::{TenantCode, tenant_code};
use std::time::Duration;

/// `t (id, n)` and `audit (what)`, every column `normal`, made by a migration (so the lens has
/// them), and one row in `t`.
async fn plain_fixture<'e>(engine: &'e PostgresEngine, extensions: &[&str]) -> Fixture<'e> {
    let key = vector_key();
    let mut new = conformance::new_database(Uuid::now_v7());
    new.extensions = extensions.iter().map(|e| e.to_string()).collect();
    let created = engine
        .create_database(&new, &key)
        .await
        .expect("create_database");
    let f = Fixture {
        engine,
        created,
        key,
        key_bytes: std::array::from_fn(|i| i as u8),
    };
    let request = MigrationRequest {
        up: "CREATE TABLE t (id integer PRIMARY KEY, n integer, body text, tsv tsvector); \
             CREATE TABLE audit (what text); CREATE SEQUENCE side;"
            .to_string(),
        timeout: Duration::from_secs(10),
        dry_run: false,
    };
    engine
        .apply_migration(
            f.db(),
            &f.key,
            &request,
            &ClassSync {
                personal: vec![],
                secret: vec![],
            },
        )
        .await
        .expect("migration");
    f.run(
        RoleKind::Owner,
        StatementMode::Write,
        "INSERT INTO t (id, n) VALUES (1, 0)",
        vec![],
    )
    .await
    .expect("insert");
    f
}

async fn found(engine: &PostgresEngine, f: &Fixture<'_>) -> TenantCode {
    let mut conn = dbhost(engine, f.db()).await;
    let out = tenant_code(&mut conn, &role_name(&f.created, RoleKind::Owner))
        .await
        .expect("tenant code");
    close(conn).await;
    out
}

/// Runs a script of the tenant in an `owner` session.
async fn as_owner(engine: &PostgresEngine, f: &Fixture<'_>, sql: &str) {
    let mut conn = session(engine, f.db(), RoleKind::Owner).await;
    psql(&mut conn, sql).await.expect(sql);
    close(conn).await;
}

/// A label, the tenant's script, the script that drops it, and the clause it must set.
type Case = (
    &'static str,
    &'static str,
    &'static str,
    fn(TenantCode) -> bool,
);

/// One case per clause of the check: the object alone sets its clause, and dropping it leaves
/// the database without tenant code. The functions of an allow-listed extension never count.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_kind_of_tenant_code_is_found_on_its_own() {
    let Some(engine) = engine().await else { return };
    let f = plain_fixture(&engine, &["citext", "pgcrypto"]).await;
    // Control: tables, views, an index, a sequence, the extensions, and expressions that call only
    // functions of the allow-list are no tenant code.
    as_owner(
        &engine,
        &f,
        "CREATE VIEW plain AS SELECT id, n FROM t; CREATE INDEX t_n ON t (n); \
         CREATE SEQUENCE s; CREATE TYPE mood AS ENUM ('a', 'b'); \
         ALTER TABLE t ADD CONSTRAINT n_ok CHECK (n >= 0); \
         CREATE OPERATOR ### (LEFTARG = text, RIGHTARG = text, FUNCTION = textcat); \
         CREATE VIEW listed AS SELECT id, upper(body) AS b, n + 1 AS n1, count(*) OVER () AS c FROM t; \
         CREATE INDEX t_low ON t (lower(body)); \
         ALTER TABLE t ADD COLUMN g text GENERATED ALWAYS AS (upper(body)) STORED; \
         CREATE DOMAIN small AS integer CHECK (VALUE < 1000)",
    )
    .await;
    let mut conn = dbhost(&engine, f.db()).await;
    let ext = psql(
        &mut conn,
        "SELECT count(*) FROM pg_proc WHERE pronamespace = 'extensions'::regnamespace",
    )
    .await
    .unwrap();
    close(conn).await;
    assert_ne!(ext, "0", "the extensions have functions");
    assert_eq!(found(&engine, &f).await, TenantCode::default());

    let cases: &[Case] = &[
        (
            "a plpgsql function",
            "CREATE FUNCTION f(x integer) RETURNS integer LANGUAGE plpgsql STABLE AS \
             $$ BEGIN RETURN x; END $$",
            "DROP FUNCTION f(integer)",
            |c| c.functions,
        ),
        (
            "a SQL function with a standard body",
            "CREATE FUNCTION g(x integer) RETURNS integer LANGUAGE sql IMMUTABLE RETURN x + 1",
            "DROP FUNCTION g(integer)",
            |c| c.functions,
        ),
        (
            "a procedure",
            "CREATE PROCEDURE p() LANGUAGE sql AS $$ SELECT 1 $$",
            "DROP PROCEDURE p()",
            |c| c.functions,
        ),
        (
            "an aggregate over a built-in function",
            "CREATE AGGREGATE total(integer) (SFUNC = int4pl, STYPE = integer)",
            "DROP AGGREGATE total(integer)",
            |c| c.functions,
        ),
        (
            "a range type (its constructors are functions)",
            "CREATE TYPE span AS RANGE (SUBTYPE = integer)",
            "DROP TYPE span",
            |c| c.functions,
        ),
        (
            "a trigger with a built-in function",
            "CREATE TRIGGER t_tsv BEFORE INSERT OR UPDATE ON t FOR EACH ROW \
             EXECUTE FUNCTION tsvector_update_trigger(tsv, 'pg_catalog.simple', body)",
            "DROP TRIGGER t_tsv ON t",
            |c| c.triggers && !c.functions,
        ),
        (
            "a disabled trigger",
            "CREATE TRIGGER t_off BEFORE UPDATE ON t FOR EACH ROW \
             EXECUTE FUNCTION tsvector_update_trigger(tsv, 'pg_catalog.simple', body); \
             ALTER TABLE t DISABLE TRIGGER t_off",
            "DROP TRIGGER t_off ON t",
            |c| c.triggers,
        ),
        (
            "a rule",
            "CREATE RULE t_copy AS ON UPDATE TO t DO ALSO INSERT INTO audit VALUES ('x')",
            "DROP RULE t_copy ON t",
            |c| c.rules && !c.functions && !c.triggers,
        ),
        (
            "a disabled rule",
            "CREATE RULE t_copy2 AS ON INSERT TO t DO ALSO INSERT INTO audit VALUES ('x'); \
             ALTER TABLE t DISABLE RULE t_copy2",
            "DROP RULE t_copy2 ON t",
            |c| c.rules,
        ),
        (
            "a policy",
            "CREATE POLICY t_p ON t USING (n >= 0)",
            "DROP POLICY t_p ON t",
            |c| c.policies && !c.functions,
        ),
        // Built-in functions outside the allow-list (decision 0004-38 (b), review-Q-2-harden
        // H-1): not tenant functions, but code that runs on the real rows.
        (
            "a view that calls a built-in function outside the allow-list",
            "CREATE VIEW sv AS SELECT id, setseed(n / 1000.0) IS NULL AS x FROM t",
            "DROP VIEW sv",
            |c| c.unlisted_functions && !c.functions,
        ),
        (
            "a CHECK constraint that calls a built-in function outside the allow-list",
            "ALTER TABLE t ADD CONSTRAINT c_rand CHECK (random() < 2)",
            "ALTER TABLE t DROP CONSTRAINT c_rand",
            |c| c.unlisted_functions && !c.functions,
        ),
        (
            "a domain whose CHECK calls a built-in function outside the allow-list",
            "CREATE DOMAIN d_rand AS integer CHECK (random() < 2)",
            "DROP DOMAIN d_rand",
            |c| c.unlisted_functions && !c.functions,
        ),
        (
            "an index expression outside the allow-list",
            "CREATE INDEX i_expr ON t ((array_to_tsvector(ARRAY['x' || coalesce(body, '')])))",
            "DROP INDEX i_expr",
            |c| c.unlisted_functions && !c.functions,
        ),
        (
            "a generated column outside the allow-list",
            "ALTER TABLE t ADD COLUMN gen tsvector GENERATED ALWAYS AS \
             (array_to_tsvector(ARRAY['x' || coalesce(body, '')])) STORED",
            "ALTER TABLE t DROP COLUMN gen",
            |c| c.unlisted_functions && !c.functions,
        ),
    ];
    for (label, make, drop, clause) in cases {
        as_owner(&engine, &f, make).await;
        let c = found(&engine, &f).await;
        assert!(clause(c) && c.any(), "{label}: {c:?}");
        as_owner(&engine, &f, drop).await;
        assert_eq!(
            found(&engine, &f).await,
            TenantCode::default(),
            "{label}: dropped"
        );
    }
}

fn update(sql: &str) -> StatementRequest {
    StatementRequest {
        sql: sql.to_string(),
        params: vec![],
        mode: StatementMode::Write,
        timeout: Duration::from_secs(10),
        max_rows: 100,
        max_bytes: None,
    }
}

async fn state_of(engine: &PostgresEngine, f: &Fixture<'_>) -> String {
    let mut conn = session(engine, f.db(), RoleKind::Owner).await;
    let out = psql(
        &mut conn,
        "SELECT (SELECT n FROM public.t WHERE id = 1)::text || '/' || \
                (SELECT count(*) FROM public.audit)::text || '/' || \
                (SELECT last_value::text || CASE WHEN is_called THEN '+' ELSE '' END \
                   FROM public.side)",
    )
    .await
    .unwrap();
    close(conn).await;
    out
}

/// The tenant's trigger: a `SECURITY DEFINER` function of the owner (so it can write `audit`
/// whatever the session's rights) that copies the row into `audit`, and takes a number of the
/// sequence `side`, which no rollback gives back: so the tests see whether the statement ran at
/// all (the check before it) or ran and was rolled back (the check after it).
const COPY_TRIGGER: &str = "CREATE FUNCTION public.log_it() RETURNS trigger LANGUAGE plpgsql \
     SECURITY DEFINER SET search_path = public AS \
     $$ BEGIN INSERT INTO public.audit VALUES (NEW.n::text); PERFORM nextval('public.side'); \
     RETURN NEW; END $$; \
     CREATE TRIGGER t_log AFTER UPDATE ON public.t FOR EACH ROW EXECUTE FUNCTION public.log_it()";

const DROP_TRIGGER: &str = "DROP TRIGGER t_log ON public.t; DROP FUNCTION public.log_it()";

/// The engine's line by itself: `masked_write` and `write` never write where tenant code
/// exists; `owner` does; a read of `write` runs. Then the race: a trigger made after the first
/// check and before the statement fires in the statement, and the check after it rolls the whole
/// transaction back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn masked_write_kinds_refuse_tenant_code_before_and_after_the_statement() {
    let Some(engine) = engine().await else { return };
    let f = plain_fixture(&engine, &[]).await;
    let refused = |r: Result<StatementOutcome, EngineError>| matches!(&r, Err(EngineError::StatementNotAllowed(m)) if m == "tenant_code_on_live");
    // Control: without tenant code both kinds write.
    for kind in [RoleKind::MaskedWrite, RoleKind::Write] {
        engine
            .execute(
                &f.role(kind),
                &f.key,
                &update("UPDATE t SET n = 1 WHERE id = 1"),
            )
            .await
            .expect("a write without tenant code runs");
    }
    assert_eq!(state_of(&engine, &f).await, "1/0/1");
    as_owner(&engine, &f, COPY_TRIGGER).await;
    for kind in [RoleKind::MaskedWrite, RoleKind::Write] {
        let r = engine
            .execute(
                &f.role(kind),
                &f.key,
                &update("UPDATE t SET n = 2 WHERE id = 1"),
            )
            .await;
        assert!(refused(r), "{kind:?}");
        // A read in mode write is refused too: the transaction is writable, and tenant code in
        // a view could write. The query layer sends reads of masked sessions read-only.
        let r = engine
            .execute(&f.role(kind), &f.key, &update("SELECT n FROM t"))
            .await;
        assert!(refused(r), "{kind:?}: a read in a writable transaction");
        let mut read = update("SELECT n FROM t");
        read.mode = StatementMode::Read;
        engine
            .execute(&f.role(kind), &f.key, &read)
            .await
            .expect("a read-only read runs");
    }
    assert_eq!(
        state_of(&engine, &f).await,
        "1/0/1",
        "nothing ran: the statement never started"
    );
    engine
        .execute(
            &f.role(RoleKind::Owner),
            &f.key,
            &update("UPDATE t SET n = 3 WHERE id = 1"),
        )
        .await
        .expect("the owner writes");
    assert_eq!(
        state_of(&engine, &f).await,
        "3/1/1+",
        "the trigger ran for the owner"
    );
    as_owner(
        &engine,
        &f,
        &format!("{DROP_TRIGGER}; DELETE FROM public.audit"),
    )
    .await;

    // The race: the trigger comes between the first check and the statement.
    for (kind, seq) in [(RoleKind::MaskedWrite, "2+"), (RoleKind::Write, "3+")] {
        *engine.between_checks.lock() = Some(COPY_TRIGGER.to_string());
        let r = engine
            .execute(
                &f.role(kind),
                &f.key,
                &update("UPDATE t SET n = 4 WHERE id = 1"),
            )
            .await;
        assert!(refused(r), "{kind:?}: the check after the statement");
        assert!(
            engine.between_checks.lock().is_none(),
            "{kind:?}: the script ran"
        );
        // The trigger ran (the sequence moved on, which no rollback undoes), and its copy and
        // the statement were rolled back.
        assert_eq!(
            state_of(&engine, &f).await,
            format!("3/0/{seq}"),
            "{kind:?}: the statement and the trigger's copy were rolled back"
        );
        as_owner(&engine, &f, DROP_TRIGGER).await;
    }
    // Control: with nothing made in between, the write runs.
    engine
        .execute(
            &f.role(RoleKind::MaskedWrite),
            &f.key,
            &update("UPDATE t SET n = 5 WHERE id = 1"),
        )
        .await
        .expect("the write runs");
    assert_eq!(state_of(&engine, &f).await, "5/0/3+");
}

/// Every catalog column that points to a function (`pg_get_catalog_foreign_keys`) and every
/// catalog column that holds an expression (`pg_node_tree`). Each place is covered by a clause of
/// the check, or cannot hold tenant code (see the comment of each line). A Postgres upgrade that
/// adds a place fails this test: then decide its clause and add it here.
const FUNCTION_PLACES: &[&str] = &[
    // The functions clause: a tenant function is a row of pg_proc, whatever points to it.
    "pg_aggregate.aggcombinefn",
    "pg_aggregate.aggdeserialfn",
    "pg_aggregate.aggfinalfn",
    "pg_aggregate.aggfnoid",
    "pg_aggregate.aggminvtransfn",
    "pg_aggregate.aggmtransfn",
    "pg_aggregate.aggmfinalfn",
    "pg_aggregate.aggserialfn",
    "pg_aggregate.aggtransfn",
    // Access methods, languages, FDWs, conversions, transforms, text search parsers and
    // templates: superuser only, or C functions (superuser).
    "pg_am.amhandler",
    "pg_amproc.amproc",
    "pg_cast.castfunc",
    "pg_conversion.conproc",
    // Event triggers: the event_triggers clause.
    "pg_event_trigger.evtfoid",
    "pg_foreign_data_wrapper.fdwhandler",
    "pg_foreign_data_wrapper.fdwvalidator",
    "pg_language.laninline",
    "pg_language.lanplcallfoid",
    "pg_language.lanvalidator",
    "pg_operator.oprcode",
    "pg_operator.oprjoin",
    "pg_operator.oprrest",
    "pg_proc.prosupport",
    "pg_range.rngcanonical",
    "pg_range.rngsubdiff",
    "pg_transform.trffromsql",
    "pg_transform.trftosql",
    // The triggers clause (a trigger can call a built-in function).
    "pg_trigger.tgfoid",
    "pg_ts_parser.prsend",
    "pg_ts_parser.prsheadline",
    "pg_ts_parser.prslextype",
    "pg_ts_parser.prsstart",
    "pg_ts_parser.prstoken",
    "pg_ts_template.tmplinit",
    "pg_ts_template.tmpllexize",
    "pg_type.typanalyze",
    "pg_type.typinput",
    "pg_type.typmodin",
    "pg_type.typmodout",
    "pg_type.typoutput",
    "pg_type.typreceive",
    "pg_type.typsend",
    "pg_type.typsubscript",
];

const EXPRESSION_PLACES: &[&str] = &[
    // Defaults and generated columns: built-ins only unless they call a tenant function (the
    // functions clause); a built-in default is no tenant code (design 8.5, accepted).
    "pg_attrdef.adbin",
    // Partition bounds: constants.
    "pg_class.relpartbound",
    // Check constraints and domain checks: as defaults (accepted one-bit answer, design 8.5).
    "pg_constraint.conbin",
    // Index expressions and predicates: as defaults.
    "pg_index.indexprs",
    "pg_index.indpred",
    "pg_partitioned_table.partexprs",
    // The policies clause.
    "pg_policy.polqual",
    "pg_policy.polwithcheck",
    // Function bodies and argument defaults: the functions clause.
    "pg_proc.proargdefaults",
    "pg_proc.prosqlbody",
    // Row filters of publications: logical decoding evaluates them, never a write session; a
    // publication needs CREATE on the database, which the tenant does not have.
    "pg_publication_rel.prqual",
    // Rules (a view's own `_RETURN` is its query): the rules clause.
    "pg_rewrite.ev_action",
    "pg_rewrite.ev_qual",
    // Extended statistics: evaluated by ANALYZE (owner only), never in a write.
    "pg_statistic_ext.stxexprs",
    // Trigger conditions: the triggers clause.
    "pg_trigger.tgqual",
    // Domain defaults: as defaults.
    "pg_type.typdefaultbin",
];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_catalog_places_that_hold_code_are_known() {
    let Some(engine) = engine().await else { return };
    let f = plain_fixture(&engine, &[]).await;
    let mut conn = dbhost(&engine, f.db()).await;
    let functions = psql(
        &mut conn,
        "SELECT DISTINCT k.fktable::text || '.' || c FROM pg_get_catalog_foreign_keys() k, \
         unnest(k.fkcols) c WHERE k.pktable = 'pg_proc'::regclass ORDER BY 1",
    )
    .await
    .unwrap();
    let expressions = psql(
        &mut conn,
        "SELECT c.relname || '.' || a.attname FROM pg_attribute a \
         JOIN pg_class c ON c.oid = a.attrelid \
         WHERE c.relnamespace = 'pg_catalog'::regnamespace AND c.relkind = 'r' \
           AND a.atttypid = 'pg_node_tree'::regtype AND a.attnum > 0 AND NOT a.attisdropped \
         ORDER BY 1",
    )
    .await
    .unwrap();
    close(conn).await;
    let sorted = |list: &[&str]| {
        let mut v: Vec<String> = list.iter().map(|s| s.to_string()).collect();
        v.sort();
        v.join("\n")
    };
    assert_eq!(
        functions,
        sorted(FUNCTION_PLACES),
        "a catalog place of functions is new or gone: decide its clause of the tenant-code check"
    );
    assert_eq!(
        expressions,
        sorted(EXPRESSION_PLACES),
        "a catalog place of expressions is new or gone: decide its clause of the tenant-code check"
    );
}
