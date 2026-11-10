//! The function allow-list (decision 0004-38 (b) and (d), design 8.5; review-Q-2-s21-2 R-1 and
//! R-2, review-Q-2-harden H-1). One small test per protection:
//!
//! - a statement of a masked or read-only kind may not call a volatile built-in function outside
//!   the few named ones (`statements_may_call_only_the_allowed_functions`);
//! - a view or a policy that reaches anything outside the allow-list is not part of the masked
//!   lens: the kinds get the refusal `statement_not_allowed` (`tenant_code_on_live`), also with
//!   the attack of each review as the statement (`code_outside_the_list_is_not_in_the_masked_lens`);
//! - the same code counts as tenant code for writes, and only the code outside the list does
//!   (`code_outside_the_list_refuses_masked_writes`);
//! - the engine refuses a database where a wrong role may call a revoked function
//!   (`a_database_where_a_role_may_run_a_revoked_function_is_refused`);
//! - the pooled connection is reset after every use (`session_state_does_not_outlive_a_lease`).

use super::*;
use crate::postgres::setup;

/// The refusal that the documented hint code names.
fn refused(r: &Result<StatementOutcome, EngineError>) -> bool {
    matches!(r, Err(EngineError::StatementNotAllowed(m)) if m == "tenant_code_on_live")
}

/// Postgres refuses `EXECUTE` of the function.
fn denied(r: &Result<StatementOutcome, EngineError>) -> bool {
    match r {
        Err(EngineError::PermissionDenied(m)) => m.contains("permission denied for function"),
        Err(EngineError::Sql { code, message, .. }) => {
            code == "42501" && message.contains("permission denied for function")
        }
        _ => false,
    }
}

/// `t (id, n, body)` with two rows, a sequence `side`, `s (id serial, v)`, every column `normal`.
async fn fixture(engine: &PostgresEngine) -> Fixture<'_> {
    let f = vector_fixture(engine).await;
    f.migrate(
        "CREATE TABLE t (id integer PRIMARY KEY, n integer, body text); \
         CREATE SEQUENCE side; CREATE TABLE s (id serial PRIMARY KEY, v text);",
    )
    .await
    .expect("migration");
    f.run(
        RoleKind::Owner,
        StatementMode::Write,
        "INSERT INTO t VALUES (1, 7, 'hidden-value'), (2, 8, 'other')",
        vec![],
    )
    .await
    .expect("insert");
    f
}

async fn read(f: &Fixture<'_>, kind: RoleKind, sql: &str) -> Result<StatementOutcome, EngineError> {
    f.run(kind, StatementMode::Read, sql, vec![]).await
}

/// `n` of the first row, read by the owner kind (it passes a policy of the table).
async fn n_of_1(f: &Fixture<'_>) -> String {
    let r = read(f, RoleKind::Owner, "SELECT n FROM t WHERE id = 1")
        .await
        .unwrap();
    format!("{:?}", r.rows)
}

async fn write(
    f: &Fixture<'_>,
    kind: RoleKind,
    sql: &str,
) -> Result<StatementOutcome, EngineError> {
    f.run(kind, StatementMode::Write, sql, vec![]).await
}

/// The kinds whose lens hides a value or that only read: none may call a revoked function.
const NOT_OWNER: [RoleKind; 5] = [
    RoleKind::Read,
    RoleKind::Write,
    RoleKind::MaskedRead,
    RoleKind::MaskedWrite,
    RoleKind::SecretRead,
];

/// Volatile built-in functions that carry state: a seed, a lock, a large object, a session
/// setting, a sequence value, a notification, a query given as text.
const CHANNELS: &[&str] = &[
    "SELECT count(*) FROM (SELECT setseed(0.5)) s",
    "SELECT pg_advisory_lock(1)",
    "SELECT pg_try_advisory_xact_lock(1, 2)",
    "SELECT lo_from_bytea(0, 'a'::bytea)",
    "SELECT lo_create(0)",
    "SELECT set_config('ruum.x', 'y', false)",
    "SELECT count(*) FROM (SELECT pg_notify('c', 'p')) s",
    "SELECT query_to_xml('select 1', false, false, '')",
];

/// A statement of any kind but `owner` may call the functions of the allow-list and the few
/// named volatile ones, and no other volatile built-in function (review-Q-2-harden H-1: a list
/// of the bad ones is never complete, so every volatile function is revoked unless named).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn statements_may_call_only_the_allowed_functions() {
    let Some(engine) = engine().await else { return };
    let f = fixture(&engine).await;
    for kind in NOT_OWNER {
        for sql in CHANNELS {
            let r = read(&f, kind, sql).await;
            assert!(denied(&r), "{} {sql}: {r:?}", kind.as_str());
        }
        // What stays: plain, stable and immutable functions, and the named volatile ones.
        let r = read(
            &f,
            kind,
            "SELECT upper(body), n + 1, now() IS NOT NULL, current_setting('work_mem') IS NOT NULL, \
                    random() < 2, clock_timestamp() IS NOT NULL, gen_random_uuid() IS NOT NULL, \
                    (SELECT count(*) FROM (SELECT pg_sleep(0)) s) \
               FROM t WHERE id = 1",
        )
        .await;
        assert!(r.is_ok(), "{}: {r:?}", kind.as_str());
    }
    // The sequence of an insert's default stays usable by the kinds that write (`nextval` is one
    // of the named volatile functions).
    for kind in [RoleKind::Write, RoleKind::MaskedWrite] {
        let r = write(&f, kind, "INSERT INTO s (v) VALUES ('x') RETURNING id").await;
        assert!(r.is_ok(), "{}: {r:?}", kind.as_str());
    }
    // The owner kind keeps every function (migrations made with pg_dump call set_config).
    for sql in CHANNELS {
        let r = write(&f, RoleKind::Owner, sql).await;
        assert!(
            !denied(&r),
            "owner {sql}: {r:?} (the owner kind keeps the revoked functions)"
        );
    }
}

/// One thing a person with `manage` can make: the SQL that makes it, the SQL that removes it,
/// and the relation that the lens then refuses.
struct Code {
    label: &'static str,
    make: &'static str,
    unmake: &'static str,
    relation: &'static str,
}

/// Code that runs on the real rows and reaches something outside the allow-list. The first four
/// are the attacks of review-Q-2-s21-2 R-1 and R-2 and of review-Q-2-harden H-1.
const OUTSIDE: &[Code] = &[
    Code {
        label: "a tenant function that writes a session setting (R-1)",
        make: "CREATE FUNCTION leak(t text) RETURNS text LANGUAGE plpgsql AS \
               $$ BEGIN EXECUTE format('set local ruum.v = %L', t); RETURN 'ok'; END $$; \
               CREATE VIEW bad AS SELECT id, leak(body) AS x FROM t",
        unmake: "DROP VIEW bad; DROP FUNCTION leak(text)",
        relation: "bad",
    },
    Code {
        label: "built-in advisory locks keyed by the value (R-2)",
        make: "CREATE VIEW bad AS SELECT id, pg_try_advisory_xact_lock(1001, n) AS x FROM t",
        unmake: "DROP VIEW bad",
        relation: "bad",
    },
    Code {
        label: "a built-in large object made from the value (R-2)",
        make: "CREATE VIEW bad AS SELECT id, lo_from_bytea(0, convert_to(body, 'UTF8')) AS x FROM t",
        unmake: "DROP VIEW bad",
        relation: "bad",
    },
    Code {
        label: "a built-in random seed set from the value (H-1)",
        make: "CREATE VIEW bad AS SELECT id, \
               (SELECT count(*) FROM (SELECT setseed(n / 1000.0)) q) AS x FROM t",
        unmake: "DROP VIEW bad",
        relation: "bad",
    },
    Code {
        label: "a sequence advanced by the value",
        make: "CREATE VIEW bad AS SELECT id, nextval('side') AS x FROM t",
        unmake: "DROP VIEW bad",
        relation: "bad",
    },
    Code {
        label: "a session setting set from the value",
        make: "CREATE VIEW bad AS SELECT id, set_config('ruum.v', body, false) AS x FROM t",
        unmake: "DROP VIEW bad",
        relation: "bad",
    },
    Code {
        label: "a function that reads session state",
        make: "CREATE VIEW bad AS SELECT id, current_setting('work_mem') AS x FROM t",
        unmake: "DROP VIEW bad",
        relation: "bad",
    },
    Code {
        label: "a function that runs a query given as text",
        make: "CREATE VIEW bad AS SELECT id, query_to_xml('select 1', false, false, '')::text AS x FROM t",
        unmake: "DROP VIEW bad",
        relation: "bad",
    },
    Code {
        label: "a view over a view outside the list",
        make: "CREATE VIEW bad0 AS SELECT id, nextval('side') AS x FROM t; \
               CREATE VIEW bad AS SELECT id FROM bad0",
        unmake: "DROP VIEW bad; DROP VIEW bad0",
        relation: "bad",
    },
    Code {
        label: "a policy that calls a function outside the list",
        make: "ALTER TABLE t ENABLE ROW LEVEL SECURITY; CREATE POLICY p ON t USING (random() < 2)",
        unmake: "DROP POLICY p ON t; ALTER TABLE t DISABLE ROW LEVEL SECURITY",
        relation: "t",
    },
];

/// The attack of review-Q-2-harden H-1 as a statement: force the view, then read `random()`.
fn attack(relation: &str) -> String {
    format!(
        "WITH a AS MATERIALIZED (SELECT count(*) AS c FROM {relation} WHERE id = 1) \
         SELECT (SELECT c FROM a) AS c, random() AS r, current_setting('ruum.v', true) AS s"
    )
}

/// A view or a policy that reaches anything outside the allow-list is not part of the masked
/// lens: a statement that reads it gets `statement_not_allowed` (`tenant_code_on_live`) from
/// `masked_read` and from `read`, also the attack statement, and nothing runs. A view that stays
/// on the list, and a policy that stays on the list, are read as before.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn code_outside_the_list_is_not_in_the_masked_lens() {
    let Some(engine) = engine().await else { return };
    let f = fixture(&engine).await;
    // Control: code on the list is in the lens.
    f.migrate(
        "CREATE VIEW good AS SELECT id, upper(body) AS b, n + 1 AS n1, coalesce(n, 0) AS n0, \
                body || '!' AS bx, count(*) OVER () AS c, to_char(now(), 'YYYY') AS y, \
                json_build_object('a', n)::text AS j, CASE WHEN n > 7 THEN 'x' ELSE 'y' END AS k \
           FROM t; \
         ALTER TABLE t ENABLE ROW LEVEL SECURITY; \
         CREATE POLICY p_ok ON t USING (n >= 0 AND lower(body) <> '')",
    )
    .await
    .expect("migration");
    for kind in [RoleKind::MaskedRead, RoleKind::Read] {
        let r = read(&f, kind, "SELECT b, n1, c FROM good WHERE id = 1")
            .await
            .unwrap_or_else(|e| panic!("{}: {e:?}", kind.as_str()));
        assert_eq!(r.rows.len(), 1, "{}", kind.as_str());
        let r = read(&f, kind, "SELECT id FROM t ORDER BY id")
            .await
            .unwrap();
        assert_eq!(
            r.rows.len(),
            2,
            "{}: a policy on the list lets rows through",
            kind.as_str()
        );
    }
    f.migrate("DROP VIEW good; DROP POLICY p_ok ON t; ALTER TABLE t DISABLE ROW LEVEL SECURITY")
        .await
        .expect("migration");

    for code in OUTSIDE {
        f.migrate(code.make)
            .await
            .unwrap_or_else(|e| panic!("{}: {e:?}", code.label));
        for kind in [RoleKind::MaskedRead, RoleKind::Read] {
            for sql in [
                format!("SELECT id FROM {}", code.relation),
                format!("SELECT count(*) FROM {}", code.relation),
                attack(code.relation),
            ] {
                let r = read(&f, kind, &sql).await;
                assert!(refused(&r), "{} {} {sql}: {r:?}", code.label, kind.as_str());
            }
        }
        f.migrate(code.unmake)
            .await
            .unwrap_or_else(|e| panic!("{} (remove): {e:?}", code.label));
        // Removed: the lens is whole again.
        for kind in [RoleKind::MaskedRead, RoleKind::Read] {
            let r = read(&f, kind, "SELECT id FROM t ORDER BY id").await;
            assert_eq!(r.unwrap().rows.len(), 2, "{} {}", code.label, kind.as_str());
        }
    }
}

/// The same code counts as tenant code for writes: `masked_write` and `write` get the refusal,
/// nothing changes, and the write runs again once the code is gone. Code on the list does not
/// count: a view on the list, a `CHECK`, an index expression and a generated column that use
/// only allowed functions leave the writes as they were.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn code_outside_the_list_refuses_masked_writes() {
    let Some(engine) = engine().await else { return };
    let f = fixture(&engine).await;
    let before = n_of_1(&f).await;
    // Control: code on the list.
    f.migrate(
        "CREATE VIEW good AS SELECT id, upper(body) AS b FROM t; \
         ALTER TABLE t ADD CONSTRAINT n_ok CHECK (n >= 0 AND length(body) < 100); \
         CREATE INDEX t_low ON t (lower(body)); \
         ALTER TABLE t ADD COLUMN g text GENERATED ALWAYS AS (upper(body)) STORED; \
         CREATE DOMAIN small AS integer CHECK (VALUE < 1000)",
    )
    .await
    .expect("migration");
    for kind in [RoleKind::MaskedWrite, RoleKind::Write] {
        let r = write(&f, kind, "UPDATE t SET n = 7 WHERE id = 1").await;
        assert!(r.is_ok(), "code on the list: {} {r:?}", kind.as_str());
    }
    f.migrate("DROP VIEW good; DROP INDEX t_low; ALTER TABLE t DROP COLUMN g; DROP DOMAIN small")
        .await
        .expect("migration");

    // The views and the policy of the attacks, and the other places that run on the real rows
    // (the engine reads the stored tree of each).
    let mut cases: Vec<(&str, &str, &str)> = OUTSIDE
        .iter()
        .map(|c| (c.label, c.make, c.unmake))
        .collect();
    cases.extend([
        (
            "a CHECK constraint that calls a function outside the list",
            "ALTER TABLE t ADD CONSTRAINT c_bad CHECK (random() < 2)",
            "ALTER TABLE t DROP CONSTRAINT c_bad",
        ),
        (
            "a domain whose CHECK calls a function outside the list",
            "CREATE DOMAIN dbad AS integer CHECK (random() < 2)",
            "DROP DOMAIN dbad",
        ),
        (
            "an index expression outside the list",
            "CREATE INDEX i_bad ON t ((array_to_tsvector(ARRAY['x' || coalesce(body, '')])))",
            "DROP INDEX i_bad",
        ),
        (
            "a partial index predicate outside the list",
            "CREATE INDEX i_pred ON t (id) WHERE array_to_tsvector(ARRAY['x' || coalesce(body, '')]) <> ''::tsvector",
            "DROP INDEX i_pred",
        ),
        (
            "a generated column outside the list",
            "ALTER TABLE t ADD COLUMN gbad tsvector GENERATED ALWAYS AS (array_to_tsvector(ARRAY['x' || coalesce(body, '')])) STORED",
            "ALTER TABLE t DROP COLUMN gbad",
        ),
    ]);
    for (label, make, unmake) in cases {
        f.migrate(make)
            .await
            .unwrap_or_else(|e| panic!("{label}: {e:?}"));
        for kind in [RoleKind::MaskedWrite, RoleKind::Write] {
            for sql in [
                "UPDATE t SET n = 99 WHERE id = 1",
                "INSERT INTO t (id, n, body) VALUES (3, 1, 'x')",
                "DELETE FROM t WHERE id = 2",
            ] {
                let r = write(&f, kind, sql).await;
                assert!(refused(&r), "{label} {} {sql}: {r:?}", kind.as_str());
            }
        }
        assert_eq!(n_of_1(&f).await, before, "{label}: nothing changed");
        f.migrate(unmake)
            .await
            .unwrap_or_else(|e| panic!("{label} (remove): {e:?}"));
        for kind in [RoleKind::MaskedWrite, RoleKind::Write] {
            let r = write(&f, kind, "UPDATE t SET n = 7 WHERE id = 1").await;
            assert!(r.is_ok(), "{label}: removed: {} {r:?}", kind.as_str());
        }
    }
}

/// The attack of review-Q-2-harden-2 F-1 as a statement: force the cast in the view with
/// `count(x)` (a plain `count(*)` evaluates nothing), then read the setting that the check
/// wrote.
fn domain_attack(relation: &str) -> String {
    format!(
        "WITH a AS MATERIALIZED (SELECT count(x) AS c FROM {relation} WHERE id = 1) \
         SELECT c, current_setting('ruum.v', true) AS s FROM a"
    )
}

/// The tenant function that a domain check calls to carry a whole value out through a session
/// setting (review-Q-2-harden-2 F-1), and the domain with that check.
const LEAK_DOMAIN: &str = "CREATE FUNCTION leakd(t text) RETURNS text LANGUAGE plpgsql AS \
     $$ BEGIN EXECUTE format('set local ruum.v = %L', t); RETURN 'ok'; END $$; \
     CREATE DOMAIN leaked AS text CHECK (leakd(VALUE) = 'ok')";
const DROP_LEAK_DOMAIN: &str = "DROP DOMAIN leaked CASCADE; DROP FUNCTION leakd(text)";

/// One domain coercion that reaches a check outside the allow-list: what it makes, what removes
/// it, the relation that the lens must refuse, and whether the statement of
/// [`domain_attack`] carries the value out when nothing guards it (the owner kind shows it).
struct DomainCase {
    label: &'static str,
    make: String,
    unmake: String,
    relation: &'static str,
    carries: bool,
}

fn domain_cases() -> Vec<DomainCase> {
    let leak = |label, view: &str, extra: &str, extra_drop: &str| DomainCase {
        label,
        make: format!("{LEAK_DOMAIN}; {extra} CREATE VIEW bad AS {view}"),
        unmake: format!("DROP VIEW bad; {extra_drop} {DROP_LEAK_DOMAIN}"),
        relation: "bad",
        carries: true,
    };
    vec![
        leak(
            "a cast to a domain whose check calls a tenant function (F-1)",
            "SELECT id, body::leaked AS x FROM t",
            "",
            "",
        ),
        leak(
            "a cast to a domain over that domain (nested)",
            "SELECT id, body::outer_d AS x FROM t",
            "CREATE DOMAIN outer_d AS leaked CHECK (VALUE <> '');",
            "DROP DOMAIN outer_d;",
        ),
        leak(
            "a cast to a domain over a domain over that domain",
            "SELECT id, body::top_d AS x FROM t",
            "CREATE DOMAIN mid_d AS leaked; CREATE DOMAIN top_d AS mid_d CHECK (VALUE <> '');",
            "DROP DOMAIN top_d; DROP DOMAIN mid_d;",
        ),
        leak(
            "a cast to a domain whose own check casts to that domain",
            "SELECT id, body::wraps AS x FROM t",
            "CREATE DOMAIN wraps AS text CHECK (VALUE::leaked IS NOT NULL);",
            "DROP DOMAIN wraps;",
        ),
        leak(
            "a cast to an array of that domain",
            "SELECT id, ARRAY[body]::leaked[] AS x FROM t",
            "",
            "",
        ),
        leak(
            "a cast to a domain over an array of that domain",
            "SELECT id, ARRAY[body]::leaked[]::arr_d AS x FROM t",
            "CREATE DOMAIN arr_d AS leaked[] CHECK (VALUE IS NOT NULL);",
            "DROP DOMAIN arr_d;",
        ),
        leak(
            "a row of a composite type with a field of that domain",
            "SELECT id, ROW(body)::pair AS x FROM t",
            "CREATE TYPE pair AS (a leaked);",
            "DROP TYPE pair;",
        ),
        leak(
            "a cast to that domain through a view over a view",
            "SELECT id, x FROM bad0",
            "CREATE VIEW bad0 AS SELECT id, body::leaked AS x FROM t;",
            "DROP VIEW bad0;",
        ),
        DomainCase {
            label: "a domain whose check takes advisory locks (built-in functions only)",
            make:
                "CREATE DOMAIN locked AS text CHECK (pg_try_advisory_xact_lock(7, length(VALUE))); \
                   CREATE VIEW bad AS SELECT id, body::locked AS x FROM t"
                    .to_string(),
            unmake: "DROP VIEW bad; DROP DOMAIN locked".to_string(),
            relation: "bad",
            carries: false,
        },
        DomainCase {
            label: "a domain whose check advances a sequence (built-in functions only)",
            make: "CREATE DOMAIN counted AS integer CHECK (nextval('side') > 0); \
                   CREATE VIEW bad AS SELECT id, n::counted AS x FROM t"
                .to_string(),
            unmake: "DROP VIEW bad; DROP DOMAIN counted".to_string(),
            relation: "bad",
            carries: false,
        },
        DomainCase {
            label: "a policy that casts to that domain",
            make: format!(
                "{LEAK_DOMAIN}; ALTER TABLE t ENABLE ROW LEVEL SECURITY; \
                 CREATE POLICY p_dom ON t USING (body::leaked IS NOT NULL)"
            ),
            unmake: format!(
                "DROP POLICY p_dom ON t; ALTER TABLE t DISABLE ROW LEVEL SECURITY; {DROP_LEAK_DOMAIN}"
            ),
            relation: "t",
            carries: false,
        },
    ]
}

/// Review-Q-2-harden-2 F-1: a cast to a domain runs the checks of the domain on the real value,
/// also inside a view or a policy that calls nothing else. A view or policy whose coercion
/// reaches a check outside the allow-list (also nested in another domain, an array, a composite
/// type or the check of another domain; also when the check calls built-in functions only) is not
/// part of the masked lens: `masked_read` and `read` get the refusal, the attack statement
/// included. The owner kind shows that the same statement carries the value out when nothing
/// guards it. Domains whose checks are on the list stay in the lens.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn domain_checks_outside_the_list_are_not_in_the_masked_lens() {
    let Some(engine) = engine().await else { return };
    let f = fixture(&engine).await;
    // Control: domains whose checks are on the list (also nested, in an array, in a policy, and
    // a built-in domain) are in the lens.
    f.migrate(
        "CREATE DOMAIN shorty AS text CHECK (length(VALUE) < 100); \
         CREATE DOMAIN shorter AS shorty CHECK (VALUE <> ''); \
         CREATE DOMAIN nonneg AS integer CHECK (VALUE >= 0 AND abs(VALUE) < 1000000); \
         CREATE VIEW good AS SELECT id, body::shorter AS b, ARRAY[body]::shorty[] AS arr, \
                n::nonneg AS nn, n::information_schema.cardinal_number AS cn FROM t; \
         ALTER TABLE t ENABLE ROW LEVEL SECURITY; \
         CREATE POLICY p_ok ON t USING (body::shorty <> '' AND n::nonneg >= 0)",
    )
    .await
    .expect("migration");
    for kind in [RoleKind::MaskedRead, RoleKind::Read] {
        let r = read(&f, kind, "SELECT b, nn, cn FROM good WHERE id = 1")
            .await
            .unwrap_or_else(|e| panic!("{}: {e:?}", kind.as_str()));
        assert_eq!(r.rows.len(), 1, "{}", kind.as_str());
        let r = read(&f, kind, "SELECT id FROM t ORDER BY id")
            .await
            .unwrap();
        assert_eq!(
            r.rows.len(),
            2,
            "{}: the policy lets rows through",
            kind.as_str()
        );
    }
    f.migrate(
        "DROP VIEW good; DROP POLICY p_ok ON t; ALTER TABLE t DISABLE ROW LEVEL SECURITY; \
         DROP DOMAIN shorter; DROP DOMAIN shorty; DROP DOMAIN nonneg",
    )
    .await
    .expect("migration");

    for case in domain_cases() {
        f.migrate(&case.make)
            .await
            .unwrap_or_else(|e| panic!("{}: {e:?}", case.label));
        if case.carries {
            // Without the guard the statement discloses the value: the owner kind reads it.
            let r = read(&f, RoleKind::Owner, &domain_attack(case.relation))
                .await
                .unwrap_or_else(|e| panic!("{} (owner): {e:?}", case.label));
            assert!(
                format!("{:?}", r.rows).contains("hidden-value"),
                "{}: the attack discloses the value when nothing guards it: {r:?}",
                case.label
            );
        }
        for kind in [RoleKind::MaskedRead, RoleKind::Read] {
            let mut statements = vec![
                format!("SELECT id FROM {}", case.relation),
                format!("SELECT count(*) FROM {}", case.relation),
                attack(case.relation),
            ];
            if case.relation == "bad" {
                statements.push(domain_attack(case.relation));
            }
            for sql in statements {
                let r = read(&f, kind, &sql).await;
                assert!(refused(&r), "{} {} {sql}: {r:?}", case.label, kind.as_str());
            }
        }
        f.migrate(&case.unmake)
            .await
            .unwrap_or_else(|e| panic!("{} (remove): {e:?}", case.label));
        // Removed: the lens is whole again.
        for kind in [RoleKind::MaskedRead, RoleKind::Read] {
            let r = read(&f, kind, "SELECT id FROM t ORDER BY id").await;
            assert_eq!(r.unwrap().rows.len(), 2, "{} {}", case.label, kind.as_str());
        }
    }
}

/// The same domains count as tenant code for writes: a view, a `CHECK`, an index expression or a
/// generated column that casts to a domain with a check outside the list refuses the writes of
/// `masked_write` and `write`. A domain on the list does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn casts_to_domains_outside_the_list_refuse_masked_writes() {
    let Some(engine) = engine().await else { return };
    let f = fixture(&engine).await;
    f.migrate(
        "CREATE DOMAIN shorty AS text CHECK (length(VALUE) < 100); \
         CREATE VIEW good AS SELECT id, body::shorty AS b FROM t; \
         ALTER TABLE t ADD CONSTRAINT c_ok CHECK (body::shorty IS NOT NULL); \
         CREATE INDEX t_dom ON t ((body::shorty)); \
         ALTER TABLE t ADD COLUMN g text GENERATED ALWAYS AS (body::shorty) STORED",
    )
    .await
    .expect("migration");
    for kind in [RoleKind::MaskedWrite, RoleKind::Write] {
        let r = write(&f, kind, "UPDATE t SET n = 7 WHERE id = 1").await;
        assert!(r.is_ok(), "a domain on the list: {} {r:?}", kind.as_str());
    }
    f.migrate(
        "DROP VIEW good; ALTER TABLE t DROP CONSTRAINT c_ok; DROP INDEX t_dom; \
         ALTER TABLE t DROP COLUMN g; DROP DOMAIN shorty",
    )
    .await
    .expect("migration");
    let before = n_of_1(&f).await;
    for case in domain_cases() {
        f.migrate(&case.make)
            .await
            .unwrap_or_else(|e| panic!("{}: {e:?}", case.label));
        for kind in [RoleKind::MaskedWrite, RoleKind::Write] {
            let r = write(&f, kind, "UPDATE t SET n = 99 WHERE id = 1").await;
            assert!(refused(&r), "{} {}: {r:?}", case.label, kind.as_str());
        }
        assert_eq!(n_of_1(&f).await, before, "{}: nothing changed", case.label);
        f.migrate(&case.unmake)
            .await
            .unwrap_or_else(|e| panic!("{} (remove): {e:?}", case.label));
        for kind in [RoleKind::MaskedWrite, RoleKind::Write] {
            let r = write(&f, kind, "UPDATE t SET n = 7 WHERE id = 1").await;
            assert!(
                r.is_ok(),
                "{}: removed: {} {r:?}",
                case.label,
                kind.as_str()
            );
        }
    }
}

/// A fixture with extensions: `ci (id, c citext)`, `trg (id, s text)` (for a trigram index), `plain
/// (id, n)`, one row each, every column `normal`.
async fn extension_fixture(engine: &PostgresEngine) -> Fixture<'_> {
    let key = vector_key();
    let mut new = conformance::new_database(Uuid::now_v7());
    new.extensions = vec!["citext".to_string(), "pg_trgm".to_string()];
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
    f.migrate(
        "CREATE TABLE ci (id integer PRIMARY KEY, c citext); \
         CREATE TABLE trg (id integer PRIMARY KEY, s text); \
         CREATE TABLE plain (id integer PRIMARY KEY, n integer)",
    )
    .await
    .expect("migration");
    f.migrate(
        "INSERT INTO ci VALUES (1, 'hidden-value'); INSERT INTO trg VALUES (1, 'hidden-value'); \
         INSERT INTO plain VALUES (1, 7)",
    )
    .await
    .expect("insert");
    f
}

/// One index whose operator class is outside the list: what makes it, what removes it, the
/// relations that the lens must refuse, and a statement of a hostile read of the relation.
struct IndexCase {
    label: &'static str,
    make: &'static str,
    unmake: &'static str,
    relations: &'static [&'static str],
    read: &'static str,
    write: &'static str,
}

const INDEX_CASES: &[IndexCase] = &[
    IndexCase {
        label: "a B-tree index with the operator class of an extension (citext_ops)",
        make: "CREATE INDEX ci_c ON ci (c)",
        unmake: "DROP INDEX ci_c",
        relations: &["ci"],
        read: "SELECT id FROM ci WHERE c = 'hidden-value'",
        write: "UPDATE ci SET id = 2 WHERE id = 1",
    },
    IndexCase {
        label: "a GIN index with the operator class of an extension (gin_trgm_ops)",
        make: "CREATE INDEX trg_s ON trg USING gin (s gin_trgm_ops)",
        unmake: "DROP INDEX trg_s",
        relations: &["trg"],
        read: "SELECT id FROM trg WHERE s LIKE '%hidden%'",
        write: "UPDATE trg SET id = 2 WHERE id = 1",
    },
    IndexCase {
        label: "a view over a table with such an index",
        make: "CREATE INDEX ci_c ON ci (c); CREATE VIEW civ AS SELECT id FROM ci",
        unmake: "DROP VIEW civ; DROP INDEX ci_c",
        relations: &["ci", "civ"],
        read: "SELECT id FROM civ",
        write: "UPDATE ci SET id = 2 WHERE id = 1",
    },
    IndexCase {
        label: "a partitioned table whose partition has such an index",
        make: "CREATE TABLE pt (id integer, c citext) PARTITION BY RANGE (id); \
               CREATE TABLE pt1 PARTITION OF pt FOR VALUES FROM (0) TO (10); \
               CREATE INDEX pt1_c ON pt1 (c)",
        unmake: "DROP TABLE pt",
        relations: &["pt", "pt1"],
        read: "SELECT id FROM pt WHERE c = 'x'",
        write: "UPDATE ci SET id = 2 WHERE id = 1",
    },
];

/// Review-Q-2-harden-2 F-2: an index calls the support functions and operators of its operator
/// class on the raw keys. A relation with an index whose class holds a function outside the
/// allow-list (here the classes of extensions; a tenant cannot make a class itself, but a class
/// of an extension, or one that a superuser made from a function of the tenant, calls code of
/// its own) is not part of the masked lens: `masked_read` and `read` get the refusal, also for
/// the read that would use the index, and so does every view over it. The owner kind reads the
/// same relation. Built-in classes stay in the lens.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn index_support_outside_the_list_is_not_in_the_masked_lens() {
    let Some(engine) = engine().await else { return };
    let f = extension_fixture(&engine).await;
    // Control: indexes of the built-in classes are in the lens (B-tree of several types, with
    // pattern ops, hash, GIN, GiST, SP-GiST, BRIN, expression and partial indexes).
    f.migrate(
        "CREATE TABLE b (id integer PRIMARY KEY, s text, u uuid, ts timestamptz, j jsonb, \
                         a integer[], r int4range, tv tsvector, ip inet, bx box); \
         CREATE INDEX b_s ON b (s); CREATE INDEX b_sp ON b (s text_pattern_ops); \
         CREATE INDEX b_u ON b (u); CREATE INDEX b_ts ON b (ts); \
         CREATE INDEX b_h ON b USING hash (s); CREATE INDEX b_j ON b USING gin (j); \
         CREATE INDEX b_a ON b USING gin (a); CREATE INDEX b_tv ON b USING gin (tv); \
         CREATE INDEX b_r ON b USING gist (r); CREATE INDEX b_bx ON b USING gist (bx); \
         CREATE INDEX b_ip ON b USING spgist (ip); CREATE INDEX b_sps ON b USING spgist (s); \
         CREATE INDEX b_br ON b USING brin (ts); CREATE INDEX b_x ON b (lower(s)); \
         CREATE INDEX b_pp ON b (id) WHERE s IS NOT NULL; \
         CREATE VIEW bv AS SELECT id, s FROM b",
    )
    .await
    .expect("migration");
    for kind in [RoleKind::MaskedRead, RoleKind::Read] {
        for rel in ["b", "bv", "plain"] {
            let r = read(&f, kind, &format!("SELECT id FROM {rel}")).await;
            assert!(
                r.is_ok(),
                "built-in classes: {rel} {}: {r:?}",
                kind.as_str()
            );
        }
    }
    for kind in [RoleKind::MaskedWrite, RoleKind::Write] {
        let r = write(&f, kind, "UPDATE plain SET n = 8 WHERE id = 1").await;
        assert!(r.is_ok(), "built-in classes: {} {r:?}", kind.as_str());
    }
    f.migrate("DROP VIEW bv; DROP TABLE b")
        .await
        .expect("migration");

    for case in INDEX_CASES {
        f.migrate(case.make)
            .await
            .unwrap_or_else(|e| panic!("{}: {e:?}", case.label));
        // The owner kind reads the relation (the code is not broken, only not offered).
        let r = read(&f, RoleKind::Owner, case.read).await;
        assert!(r.is_ok(), "{} (owner): {r:?}", case.label);
        for kind in [RoleKind::MaskedRead, RoleKind::Read] {
            for rel in case.relations {
                for sql in [
                    format!("SELECT id FROM {rel}"),
                    format!("SELECT count(*) FROM {rel}"),
                ] {
                    let r = read(&f, kind, &sql).await;
                    assert!(refused(&r), "{} {} {sql}: {r:?}", case.label, kind.as_str());
                }
            }
            let r = read(&f, kind, case.read).await;
            assert!(
                refused(&r),
                "{} {}: {}: {r:?}",
                case.label,
                kind.as_str(),
                case.read
            );
            // What has no such index stays in the lens.
            let r = read(&f, kind, "SELECT id FROM plain").await;
            assert!(r.is_ok(), "{} {}: plain: {r:?}", case.label, kind.as_str());
        }
        f.migrate(case.unmake)
            .await
            .unwrap_or_else(|e| panic!("{} (remove): {e:?}", case.label));
        for kind in [RoleKind::MaskedRead, RoleKind::Read] {
            for rel in ["ci", "trg"] {
                let r = read(&f, kind, &format!("SELECT id FROM {rel}")).await;
                assert!(
                    r.is_ok(),
                    "{} removed: {rel} {}: {r:?}",
                    case.label,
                    kind.as_str()
                );
            }
        }
    }
}

/// An index whose operator class is outside the allow-list counts as tenant code for writes:
/// `masked_write` and `write` get the refusal, nothing changes, and the write runs again once the
/// index is gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn index_support_outside_the_list_refuses_masked_writes() {
    let Some(engine) = engine().await else { return };
    let f = extension_fixture(&engine).await;
    for case in INDEX_CASES {
        f.migrate(case.make)
            .await
            .unwrap_or_else(|e| panic!("{}: {e:?}", case.label));
        for kind in [RoleKind::MaskedWrite, RoleKind::Write] {
            let r = write(&f, kind, "UPDATE plain SET n = 99 WHERE id = 1").await;
            assert!(refused(&r), "{} {}: {r:?}", case.label, kind.as_str());
            let r = write(&f, kind, case.write).await;
            assert!(refused(&r), "{} {}: {r:?}", case.label, kind.as_str());
        }
        let r = read(&f, RoleKind::Owner, "SELECT n FROM plain WHERE id = 1")
            .await
            .unwrap();
        assert!(
            format!("{:?}", r.rows).contains('7'),
            "{}: nothing changed: {r:?}",
            case.label
        );
        f.migrate(case.unmake)
            .await
            .unwrap_or_else(|e| panic!("{} (remove): {e:?}", case.label));
        for kind in [RoleKind::MaskedWrite, RoleKind::Write] {
            let r = write(&f, kind, "UPDATE plain SET n = 7 WHERE id = 1").await;
            assert!(
                r.is_ok(),
                "{}: removed: {} {r:?}",
                case.label,
                kind.as_str()
            );
        }
    }
}

/// Every operator class that Postgres ships is on the list: its family holds only non-volatile
/// functions of `pg_catalog` and operators of `pg_catalog` that are allowed. If a Postgres
/// upgrade adds a class that is not, every relation with an index of that class would be refused:
/// this test names it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_built_in_operator_classes_are_on_the_list() {
    let Some(engine) = engine().await else { return };
    let f = fixture(&engine).await;
    let mut conn = dbhost(&engine, f.db()).await;
    let ctes = crate::postgres::allow::ctes(1);
    let unlisted = crate::postgres::allow::unlisted_index_support("i");
    let sql = format!(
        "WITH RECURSIVE {ctes} \
         SELECT count(*), coalesce(string_agg(oc.opcname, ','), '') \
           FROM (SELECT oid, ARRAY[oid] AS indclass FROM pg_catalog.pg_opclass WHERE oid < 16384) i \
           JOIN pg_catalog.pg_opclass oc ON oc.oid = i.oid WHERE {unlisted}"
    );
    use sqlx::Row;
    let row = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(crate::postgres::allow::row_functions())
        .fetch_one(&mut conn)
        .await
        .expect("query");
    let (n, names): (i64, String) = (row.get(0), row.get(1));
    close(conn).await;
    assert_eq!(n, 0, "built-in operator classes outside the list: {names}");
}

/// The engine refuses a database where a role other than the owner kind, or `PUBLIC`, may call a
/// revoked function (a database of an older cluster, or something granted it), and accepts it
/// again once the grant is gone. The check names the function.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_database_where_a_role_may_run_a_revoked_function_is_refused() {
    let Some(engine) = engine().await else { return };
    let f = fixture(&engine).await;
    let mut conn = dbhost(&engine, f.db()).await;
    let roles = setup::read_roles(&mut conn).await.unwrap();
    setup::check_volatile_revoked(&mut conn, &roles)
        .await
        .expect("a new database passes");
    const FUNCTION: &str = "pg_catalog.setseed(double precision)";
    for kind in [RoleKind::MaskedRead, RoleKind::Write, RoleKind::Mask] {
        let role = role_name(&f.created, kind);
        psql(
            &mut conn,
            &format!("GRANT EXECUTE ON FUNCTION {FUNCTION} TO \"{role}\""),
        )
        .await
        .unwrap();
        let err = setup::check_volatile_revoked(&mut conn, &roles)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, EngineError::Internal(m) if m.contains("setseed")),
            "{}: {err:?}",
            kind.as_str()
        );
        psql(
            &mut conn,
            &format!("REVOKE EXECUTE ON FUNCTION {FUNCTION} FROM \"{role}\""),
        )
        .await
        .unwrap();
        setup::check_volatile_revoked(&mut conn, &roles)
            .await
            .expect("passes again");
    }
    psql(
        &mut conn,
        &format!("GRANT EXECUTE ON FUNCTION {FUNCTION} TO PUBLIC"),
    )
    .await
    .unwrap();
    let err = setup::check_volatile_revoked(&mut conn, &roles)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, EngineError::Internal(m) if m.contains("PUBLIC") && m.contains("setseed")),
        "{err:?}"
    );
    psql(
        &mut conn,
        &format!("REVOKE EXECUTE ON FUNCTION {FUNCTION} FROM PUBLIC"),
    )
    .await
    .unwrap();
    setup::check_volatile_revoked(&mut conn, &roles)
        .await
        .expect("passes again");
    close(conn).await;
}

/// The pooled connection is reset (`DISCARD ALL`) after every use: the sequence value that one
/// request took (`lastval` is state of the session) is gone for the next request on the same
/// connection, for the kinds that write and cannot set options.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_state_does_not_outlive_a_lease() {
    let Some((engine, seen)) = engine_with_hook().await else {
        return;
    };
    let f = fixture(&engine).await;
    for kind in [RoleKind::Write, RoleKind::MaskedWrite] {
        write(&f, kind, "INSERT INTO s (v) VALUES ('x') RETURNING id")
            .await
            .unwrap();
        let pid = seen.last_pid();
        let state = engine.resolve(f.db()).await.unwrap();
        let pool = engine.pool(&state, kind);
        assert_eq!(pool.idle_count(), 1, "{}", kind.as_str());
        let mut lease = pool.acquire(&engine).await.unwrap();
        assert_eq!(
            lease.backend_pid, pid,
            "the connection went back to the pool"
        );
        let r = psql(lease.conn(), "SELECT lastval()").await;
        lease.release(&engine).await;
        let err = r.expect_err("the sequence value of the earlier use is gone");
        assert!(err.contains("not yet defined in this session"), "{err}");
    }
}
