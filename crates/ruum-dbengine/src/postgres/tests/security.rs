//! The security cases of card E-2 ("Done when").

use super::*;
use std::time::{Duration, Instant};

fn request(sql: &str, mode: StatementMode) -> StatementRequest {
    StatementRequest {
        sql: sql.to_string(),
        params: vec![],
        mode,
        timeout: Duration::from_secs(10),
        max_rows: 1000,
        max_bytes: None,
    }
}

async fn users_with_classes<'e>(engine: &'e PostgresEngine) -> Fixture<'e> {
    let f = vector_fixture(engine).await;
    let sync = ClassSync {
        personal: vec!["email", "name", "born"],
        secret: vec!["token"],
    };
    let request = MigrationRequest {
        up: "CREATE TABLE users (id integer PRIMARY KEY, email text UNIQUE, name text, \
             born date, token text, plan text NOT NULL DEFAULT 'free');"
            .to_string(),
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
        "INSERT INTO users (id, email, name, born, token) VALUES \
         (1, 'alice@example.com', 'Alice', '1990-04-01', 'tok_secret_alice'), \
         (2, 'bob@example.com', 'Bob', NULL, NULL)",
        vec![],
    )
    .await
    .expect("insert");
    f
}

/// ruum_dbhost never runs a caller's statement: the hook sees `session_user` = the role of the
/// request for every untrusted statement, in queries of every kind and in a migration's `up`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn untrusted_statements_run_only_as_the_role_of_the_request() {
    let Some((engine, seen)) = engine_with_hook().await else {
        return;
    };
    let f = Fixture::new(&engine).await;
    f.migrate("CREATE TABLE t (x integer); CREATE TABLE u (y text);")
        .await
        .unwrap();
    for kind in RoleKind::SESSION_KINDS {
        f.run(kind, StatementMode::Read, "SELECT count(*) FROM t", vec![])
            .await
            .unwrap();
    }
    let _ = f
        .run(RoleKind::Write, StatementMode::Write, "SELECT 1/0", vec![])
        .await;
    let all = seen.0.lock().unwrap().clone();
    assert_eq!(all.len(), 2 + RoleKind::SESSION_KINDS.len() + 1, "{all:#?}");
    for stmt in &all {
        assert_eq!(stmt.session_user, stmt.role_name, "{stmt:?}");
        assert_eq!(stmt.current_user, stmt.role_name, "{stmt:?}");
        assert_ne!(stmt.session_user, "ruum_dbhost");
        assert_eq!(stmt.role_name, role_name(&f.created, stmt.kind));
        assert!(conformance_is_neutral(&stmt.session_user));
    }
    let kinds: Vec<_> = all.iter().map(|s| s.kind).collect();
    assert_eq!(
        kinds[..2],
        [RoleKind::Owner, RoleKind::Owner],
        "up runs as owner"
    );
}

fn conformance_is_neutral(name: &str) -> bool {
    name.strip_prefix("r_")
        .is_some_and(|r| r.len() == 16 && r.bytes().all(|c| c.is_ascii_hexdigit()))
}

/// From a query, RESET ROLE, SET ROLE, set_config('role') and SET SESSION AUTHORIZATION cannot
/// reach another role, also in a DO block.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_query_reaches_another_role() {
    let Some(engine) = engine().await else { return };
    let a = Fixture::new(&engine).await;
    let b = Fixture::new(&engine).await;
    let mut targets = vec![
        role_name(&b.created, RoleKind::Owner),
        role_name(&a.created, RoleKind::Mask),
        "ruum_dbhost".to_string(),
        "postgres".to_string(),
    ];
    for kind in RoleKind::SESSION_KINDS {
        let other = if kind == RoleKind::Owner {
            RoleKind::Read
        } else {
            RoleKind::Owner
        };
        targets.push(role_name(&a.created, other));
        let mode = if kind.can_write() {
            StatementMode::Write
        } else {
            StatementMode::Read
        };
        let me = role_name(&a.created, kind);
        for target in &targets {
            for sql in [
                format!("SET ROLE {target}"),
                format!("SELECT set_config('role', '{target}', false)"),
                format!("SELECT set_config('role', '{target}', true)"),
                format!("DO $$ BEGIN EXECUTE 'SET ROLE {target}'; END $$"),
                format!("DO $$ BEGIN PERFORM set_config('role', '{target}', false); END $$"),
                format!("SET SESSION AUTHORIZATION {target}"),
            ] {
                let err = a.run(kind, mode, &sql, vec![]).await.unwrap_err();
                assert!(
                    matches!(&err, EngineError::PermissionDenied(_))
                        || matches!(&err, EngineError::Sql { code, .. } if code == "42501" || code == "22023"),
                    "{} {sql}: {err:?}",
                    kind.as_str()
                );
            }
        }
        targets.pop();
        // RESET ROLE runs, and current_user stays the role (also in a DO block).
        a.run(kind, mode, "RESET ROLE", vec![]).await.unwrap();
        let out = a
            .run(
                kind,
                mode,
                "DO $$ BEGIN RESET ROLE; IF current_user <> session_user THEN \
                 RAISE EXCEPTION 'reached %', current_user; END IF; END $$",
                vec![],
            )
            .await;
        assert!(out.is_ok(), "{out:?}");
        let who = a
            .run(
                kind,
                StatementMode::Read,
                "SELECT current_user::text, session_user::text",
                vec![],
            )
            .await
            .unwrap();
        assert_eq!(
            who.rows,
            [vec![Value::Text(me.clone()), Value::Text(me.clone())]]
        );
        // The same in one session that logs in as the role (several statements).
        let mut s = session(&engine, a.db(), kind).await;
        assert_eq!(
            psql(&mut s, "RESET ROLE; SELECT current_user")
                .await
                .unwrap(),
            me
        );
        close(s).await;
    }
}

/// A role of one database cannot connect to (so not read) another database.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_role_of_one_database_cannot_read_another() {
    let Some(engine) = engine().await else { return };
    let a = Fixture::new(&engine).await;
    let b = Fixture::new(&engine).await;
    b.seed_users().await;
    let state = engine.resolve(a.db()).await.unwrap();
    for kind in RoleKind::SESSION_KINDS {
        let pool = engine.pool(&state, kind);
        let password = pool.password(&engine, false).await.unwrap();
        for dbname in [b.db().engine_name.as_str(), "postgres", "template1"] {
            let options = pool.options(&engine, &password).database(dbname);
            let err = PgConnection::connect_with(&options).await.unwrap_err();
            assert_eq!(
                errors::sqlstate(&err).as_deref(),
                Some("42501"),
                "{} -> {dbname}: {err}",
                kind.as_str()
            );
        }
    }
}

/// A masked role cannot read a base table, `ruum_mask.key`, or call `ruum_mask.pseudonym` by
/// name; no lens kind reads the key.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn masked_roles_reach_neither_the_base_tables_nor_the_key() {
    let Some(engine) = engine().await else { return };
    let f = users_with_classes(&engine).await;
    for kind in [RoleKind::MaskedRead, RoleKind::MaskedWrite] {
        for sql in [
            "SELECT email FROM public.users",
            "SELECT * FROM ruum_mask.key",
            "SELECT ruum_mask.pseudonym('x'::text)",
            "SELECT ruum_mask.hmac12('x')",
            "SELECT ruum_mask.pseudonym_other(1::int)",
            "SELECT query_to_xml('select * from public.users', true, true, '')",
        ] {
            let err = f
                .run(kind, StatementMode::Read, sql, vec![])
                .await
                .unwrap_err();
            assert!(
                matches!(err, EngineError::PermissionDenied(_)),
                "{} {sql}: {err:?}",
                kind.as_str()
            );
        }
        // Through its lens it reads pseudonyms, never the value.
        let out = f
            .run(
                kind,
                StatementMode::Read,
                "SELECT email, token FROM users ORDER BY id",
                vec![],
            )
            .await
            .unwrap();
        assert_eq!(
            out.rows[0],
            [
                Value::Text("p_a59fc578d4cb".into()),
                Value::Text(lens::SECRET_CONSTANT.into())
            ]
        );
        assert_eq!(out.rows[1][1], Value::Null);
    }
    for kind in [
        RoleKind::Read,
        RoleKind::Write,
        RoleKind::Owner,
        RoleKind::SecretRead,
    ] {
        let err = f
            .run(
                kind,
                StatementMode::Read,
                "SELECT * FROM ruum_mask.key",
                vec![],
            )
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::PermissionDenied(_)), "{err:?}");
    }
    // nosecret: personal values, the secret masked; base (secret_read): everything.
    let out = f
        .run(
            RoleKind::Read,
            StatementMode::Read,
            "SELECT email, token FROM users WHERE id = 1",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(
        out.rows,
        [vec![
            Value::Text("alice@example.com".into()),
            Value::Text(lens::SECRET_CONSTANT.into())
        ]]
    );
    let out = f
        .run(
            RoleKind::SecretRead,
            StatementMode::Read,
            "SELECT token FROM users WHERE id = 1",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(out.rows, [vec![Value::Text("tok_secret_alice".into())]]);
}

/// The 29 test vectors of spike S-1 (key 0x00..0x1f; `panel/step2-spike-s1.md`, also Q-0's
/// unit test) and the overloads of review-Q-0 notes 1 to 3: type, literal, pseudonym. The
/// pseudonyms are fixed strings that the Rust side (`ruum_core::privacy`) computes.
const VECTORS: &[(&str, &str, &str)] = &[
    ("text", "alice@example.com", "p_a59fc578d4cb"),
    ("text", "Ünïcode ✓ 中文", "p_c9cff20b05f9"),
    ("text", "", "p_d38b42096d80"),
    ("int8", "-42", "p_2156a1be4067"),
    ("int8", "9223372036854775807", "p_250c40be2e1e"),
    ("numeric", "1.50", "p_254d5d122e4a"),
    ("numeric", "-0.000100", "p_076d10355f90"),
    ("numeric", "100", "p_59440f31cb67"),
    (
        "numeric",
        "123456789012345678901234567890.123400",
        "p_32e11e873e69",
    ),
    ("float8", "0.1", "p_c68a3e99cfa2"),
    ("float8", "1e21", "p_0e08c007c8e1"),
    ("float8", "1.5e-5", "p_8734ccb07ac8"),
    ("float8", "0.0001", "p_1f1dcdce1346"),
    ("float8", "-0", "p_7eb533b5b939"),
    ("float8", "3.141592653589793", "p_2a2183e6d5f9"),
    ("float8", "123456789012345.6", "p_648eabc346b5"),
    ("float8", "1234567890123456", "p_a6dbb62fb6fe"),
    ("float8", "1e-320", "p_5ee18f53bf52"),
    ("float8", "Infinity", "p_4d1a00207d32"),
    ("float8", "NaN", "p_7fb853b18b47"),
    (
        "timestamptz",
        "2026-11-07T12:34:56.789+02:00",
        "p_4f00da9579d9",
    ),
    ("timestamptz", "2026-01-01T00:00:00Z", "p_2d5175fd30b8"),
    (
        "timestamptz",
        "1999-12-31T23:59:59.000001-08:00",
        "p_94cbd3d335ea",
    ),
    ("date", "2026-11-07", "p_86cf4c04cf1c"),
    ("bytea", "\\xDEADbeef00", "p_86de3b507e52"),
    ("bytea", "\\x", "p_d38b42096d80"),
    ("bool", "true", "p_4476aeee13a6"),
    ("bool", "false", "p_a290a0a8027b"),
    (
        "uuid",
        "0190F0E2-7C4A-7B3E-9F00-0123456789AB",
        "p_8fc66ef9f72f",
    ),
    // review-Q-0 note 1: int2 and int4 have overloads of their own (canonical: decimal).
    ("int2", "-7", "p_54a07f6735f4"),
    ("int4", "2147483647", "p_9e62a09bcfd3"),
    // review-Q-0 note 2: real in the float4out layout, and timestamp (taken as UTC).
    ("float4", "0.1", "p_c68a3e99cfa2"),
    ("float4", "1e6", "p_9caf87bd675d"),
    ("float4", "123456", "p_8ba539f5f932"),
    ("timestamp", "2026-11-07 12:34:56.5", "p_0cb450a57215"),
    // varchar goes through the text overload; a zero numeric has no sign.
    ("varchar", "Bob", "p_4efbbf79f3f5"),
    ("numeric", "-0.000", "p_3a8b171143bc"),
    // review-Q-0 note 3: BC and infinite values take the type's output text (DateStyle ISO,
    // TimeZone UTC): `0044-03-15 BC`.
    ("date", "0044-03-15 BC", "p_3b990e68c480"),
    ("timestamptz", "0044-03-15 10:00:00+00 BC", "p_045de6a6b9b2"),
    ("date", "infinity", "p_ed12a57469b8"),
    // A type of the generic overload: its output text with fixed settings.
    ("interval", "1 day 02:03:04", "p_bc3f193b1d01"),
];

/// The Rust and the SQL pseudonym agree on the fixed vectors, under hostile session settings.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rust_and_sql_pseudonyms_agree_under_hostile_settings() {
    let Some(engine) = engine().await else { return };
    let f = vector_fixture(&engine).await;
    let mut types: Vec<&str> = VECTORS.iter().map(|v| v.0).collect();
    types.sort_unstable();
    types.dedup();
    let up: String = types
        .iter()
        .map(|t| format!("CREATE TABLE v_{t} (i integer PRIMARY KEY, v {t});\n"))
        .collect();
    let sync = ClassSync {
        personal: vec!["v"],
        secret: vec![],
    };
    let request = MigrationRequest {
        up,
        timeout: Duration::from_secs(10),
        dry_run: false,
    };
    engine
        .apply_migration(f.db(), &f.key, &request, &sync)
        .await
        .unwrap();
    // The values go in under hostile settings too (the literals are absolute).
    let mut owner = session(&engine, f.db(), RoleKind::Owner).await;
    for (i, (ty, lit, _)) in VECTORS.iter().enumerate() {
        let sql = format!(
            "INSERT INTO v_{ty} (i, v) VALUES ({i}, {}::{ty})",
            lens::literal(lit)
        );
        psql(&mut owner, &sql).await.unwrap();
    }
    close(owner).await;
    let mut s = session(&engine, f.db(), RoleKind::MaskedRead).await;
    psql(
        &mut s,
        "SET timezone = 'Pacific/Chatham'; SET datestyle = 'SQL, DMY'; \
         SET extra_float_digits = -3; SET bytea_output = 'escape'; \
         SET intervalstyle = 'sql_standard'; SET lc_monetary = 'C'",
    )
    .await
    .unwrap();
    for (i, (ty, lit, want)) in VECTORS.iter().enumerate() {
        let got = psql(&mut s, &format!("SELECT v FROM v_{ty} WHERE i = {i}"))
            .await
            .unwrap();
        assert_eq!(got, *want, "{ty} {lit:?}");
    }
    // The settings were really hostile.
    assert_eq!(
        psql(&mut s, "SHOW timezone").await.unwrap(),
        "Pacific/Chatham"
    );
    close(s).await;
}

/// A statement over the timeout is cancelled (also a DO block that lifts its own timeout).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_statement_over_the_timeout_is_cancelled() {
    let Some(engine) = engine().await else { return };
    let f = Fixture::new(&engine).await;
    for (kind, mode, sql) in [
        (RoleKind::Read, StatementMode::Read, "SELECT pg_sleep(5)"),
        (
            RoleKind::Owner,
            StatementMode::Write,
            "DO $$ BEGIN PERFORM set_config('statement_timeout', '0', true); PERFORM pg_sleep(5); END $$",
        ),
    ] {
        let started = Instant::now();
        let mut r = request(sql, mode);
        r.timeout = Duration::from_millis(300);
        let err = engine.execute(&f.role(kind), &f.key, &r).await.unwrap_err();
        assert!(matches!(err, EngineError::Timeout), "{sql}: {err:?}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}

/// After a request, a pooled connection holds no session option: `SHOW` of statement_timeout,
/// role, search_path (and TimeZone) equal a fresh session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pooled_connection_holds_no_session_option() {
    let Some((engine, seen)) = engine_with_hook().await else {
        return;
    };
    let f = Fixture::new(&engine).await;
    f.seed_users().await;
    const SHOW: &str = "SELECT current_setting('statement_timeout') || '|' || current_user || '|' \
        || current_setting('search_path') || '|' || current_setting('TimeZone') || '|' \
        || current_setting('transaction_read_only')";
    for kind in [RoleKind::Write, RoleKind::MaskedWrite, RoleKind::Owner] {
        // A committed write that sets session options (set_config(..., false) inside the
        // transaction outlives it without DISCARD ALL).
        f.run(
            kind,
            StatementMode::Write,
            "SELECT set_config('statement_timeout', '123s', false), \
                    set_config('search_path', 'pg_catalog', false), \
                    set_config('TimeZone', 'Pacific/Chatham', false), \
                    set_config('default_transaction_read_only', 'on', false)",
            vec![],
        )
        .await
        .unwrap();
        let pid = seen.last_pid();
        // The next request gets the same connection, with nothing left.
        let state = engine.resolve(f.db()).await.unwrap();
        let pool = engine.pool(&state, kind);
        assert_eq!(pool.idle_count(), 1);
        let mut lease = pool.acquire(&engine).await.unwrap();
        assert_eq!(
            lease.backend_pid, pid,
            "the connection went back to the pool"
        );
        let pooled = psql(lease.conn(), SHOW).await.unwrap();
        lease.release(&engine).await;
        let mut fresh = session(&engine, f.db(), kind).await;
        let clean = psql(&mut fresh, SHOW).await.unwrap();
        close(fresh).await;
        assert_eq!(pooled, clean, "{}", kind.as_str());
        assert!(!pooled.contains("Chatham") && !pooled.contains("123s"));
        assert!(pooled.contains(&role_name(&f.created, kind)));
    }
}

/// A connection whose transaction failed (an error, a timeout, a refused write) is closed; a
/// clean one is reused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connection_whose_transaction_failed_is_closed() {
    let Some((engine, seen)) = engine_with_hook().await else {
        return;
    };
    let f = Fixture::new(&engine).await;
    f.seed_users().await;
    let ok = || {
        f.run(
            RoleKind::Write,
            StatementMode::Write,
            "UPDATE users SET name = name",
            vec![],
        )
    };
    ok().await.unwrap();
    let first = seen.last_pid();
    ok().await.unwrap();
    assert_eq!(seen.last_pid(), first, "a clean connection is reused");
    for bad in [
        "SELECT 1/0",
        "INSERT INTO users (id, name) VALUES (1, 'dup')",
        "SELECT * FROM nope",
    ] {
        let before = seen.last_pid();
        assert!(
            f.run(RoleKind::Write, StatementMode::Write, bad, vec![])
                .await
                .is_err()
        );
        assert_eq!(
            seen.last_pid(),
            before,
            "the failing statement ran on the pooled connection"
        );
        ok().await.unwrap();
        assert_ne!(
            seen.last_pid(),
            before,
            "{bad}: the failed connection was closed"
        );
    }
}

/// Before the pool opens connections for a role, ruum_dbhost resets its settings and password;
/// a connection whose role has other settings is closed (decision 0004-34 c).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_role_that_changed_its_settings_or_password_is_reset() {
    let Some(engine) = engine().await else { return };
    let f = Fixture::new(&engine).await;
    f.seed_users().await;
    let state = engine.resolve(f.db()).await.unwrap();
    for kind in [RoleKind::MaskedWrite, RoleKind::Write, RoleKind::Owner] {
        f.run(
            kind,
            StatementMode::Write,
            "DO $$ BEGIN EXECUTE 'ALTER ROLE ' || current_user || ' SET timezone = ''Pacific/Chatham'''; \
             EXECUTE 'ALTER ROLE ' || current_user || ' SET statement_timeout = 0'; END $$",
            vec![],
        )
        .await
        .unwrap();
        // The setting reaches a new connection of the role.
        let pool = engine.pool(&state, kind);
        pool.close_all().await;
        let out = f
            .run(
                kind,
                StatementMode::Read,
                "SELECT current_setting('TimeZone')",
                vec![],
            )
            .await
            .unwrap();
        assert_ne!(
            out.rows,
            [vec![Value::Text("Pacific/Chatham".into())]],
            "{}",
            kind.as_str()
        );
        let mut host = dbhost(&engine, f.db()).await;
        let config = psql(
            &mut host,
            &format!(
                "select array_to_string(setconfig, ',') from pg_db_role_setting \
                  where setrole = '{}'::regrole",
                role_name(&f.created, kind)
            ),
        )
        .await
        .unwrap();
        close(host).await;
        assert_eq!(config, format!("search_path={}", pool::search_path(kind)));

        // A password the role chose itself stops working at the next reset.
        f.run(
            kind,
            StatementMode::Write,
            "DO $$ BEGIN EXECUTE 'ALTER ROLE ' || current_user || ' PASSWORD ''known'''; END $$",
            vec![],
        )
        .await
        .unwrap();
        pool.close_all().await;
        f.run(kind, StatementMode::Read, "SELECT 1", vec![])
            .await
            .unwrap();
        let options = pool.options(&engine, "known");
        assert!(PgConnection::connect_with(&options).await.is_err());
    }
}

/// Transaction control is refused before it runs (first line); with the first line off, the
/// check after each statement fails the request and closes the connection (second line).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transaction_control_is_caught_before_and_after() {
    let Some((engine, seen)) = engine_with_hook().await else {
        return;
    };
    let f = Fixture::new(&engine).await;
    f.seed_users().await;
    let err = f
        .run(RoleKind::Owner, StatementMode::Write, "COMMIT", vec![])
        .await
        .unwrap_err();
    assert!(
        matches!(err, EngineError::StatementNotAllowed(_)),
        "{err:?}"
    );
    let before = seen.0.lock().unwrap().len();
    let err = f
        .migrate("CREATE TABLE a1 (x integer); COMMIT; CREATE TABLE b1 (x integer);")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, EngineError::MigrationFailed(e) if matches!(**e, EngineError::StatementNotAllowed(_))),
        "{err:?}"
    );
    assert_eq!(
        seen.0.lock().unwrap().len(),
        before,
        "nothing of the `up` ran"
    );

    engine
        .allow_transaction_control
        .store(true, Ordering::SeqCst);
    for (sql, mode) in [
        ("COMMIT", StatementMode::Write),
        ("ROLLBACK", StatementMode::Read),
    ] {
        f.run(RoleKind::Owner, StatementMode::Read, "SELECT 1", vec![])
            .await
            .unwrap();
        let pid = seen.last_pid();
        let err = f.run(RoleKind::Owner, mode, sql, vec![]).await.unwrap_err();
        assert!(
            matches!(err, EngineError::TransactionEnded),
            "{sql}: {err:?}"
        );
        f.run(RoleKind::Owner, StatementMode::Read, "SELECT 1", vec![])
            .await
            .unwrap();
        assert_ne!(seen.last_pid(), pid, "{sql}: the connection was closed");
    }
    let err = f
        .migrate("CREATE TABLE a1 (x integer); COMMIT; CREATE TABLE b1 (x integer);")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, EngineError::MigrationFailed(e) if matches!(**e, EngineError::TransactionEnded)),
        "{err:?}"
    );
    let catalog = engine.catalog(f.db()).await.unwrap();
    assert!(
        catalog.table("public", "b1").is_none(),
        "nothing after the COMMIT ran"
    );
    engine
        .allow_transaction_control
        .store(false, Ordering::SeqCst);
    // The timeout still holds afterwards.
    let mut r = request("SELECT pg_sleep(3)", StatementMode::Read);
    r.timeout = Duration::from_millis(200);
    let err = engine
        .execute(&f.role(RoleKind::Read), &f.key, &r)
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::Timeout), "{err:?}");
}

/// A branch succeeds while pooled connections of several kinds are open and a long statement
/// runs: the statement is cancelled after the bound, requests during the pause wait and then get
/// `database_not_ready`, and every kind works again afterwards (decision 0004-27).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_branch_drains_a_source_in_use() {
    let Some(engine) = engine_with(|c| {
        c.drain_wait = Duration::from_secs(2);
        c.request_wait = Duration::from_millis(700);
    })
    .await
    else {
        return;
    };
    let engine = Arc::new(engine);
    let f = Fixture::new(&*engine).await;
    f.seed_users().await;
    for kind in RoleKind::SESSION_KINDS {
        f.run(
            kind,
            StatementMode::Read,
            "SELECT count(*) FROM users",
            vec![],
        )
        .await
        .unwrap();
    }
    let role = f.role(RoleKind::Read);
    let key = f.key.clone();
    let e2 = engine.clone();
    let long = tokio::spawn(async move {
        let mut r = request("SELECT pg_sleep(30)", StatementMode::Read);
        r.timeout = Duration::from_secs(60);
        let started = Instant::now();
        let out = e2.execute(&role, &key, &r).await;
        (out, started.elapsed())
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let e3 = engine.clone();
    let source = f.db().clone();
    let source_key = f.key.clone();
    let tenant = f.db().tenant_id;
    let copy = tokio::spawn(async move {
        let started = Instant::now();
        let (branch_key, _) = conformance::test_key();
        let out = e3
            .copy_database(
                &source,
                &source_key,
                &conformance::new_database(tenant),
                &CopyData::Full,
                &branch_key,
            )
            .await;
        (out, started.elapsed())
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    // A request during the pause waits, then gets database_not_ready.
    let started = Instant::now();
    let err = f
        .run(RoleKind::Write, StatementMode::Read, "SELECT 1", vec![])
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::NotReady), "{err:?}");
    assert!(err.is_retryable());
    assert!(started.elapsed() >= Duration::from_millis(600), "it waited");

    let (out, took) = long.await.unwrap();
    assert!(
        matches!(out, Err(EngineError::NotReady)),
        "the long statement was cancelled: {out:?}"
    );
    assert!(
        took < Duration::from_secs(10),
        "cancelled after the bound: {took:?}"
    );
    let (copied, took) = copy.await.unwrap();
    let copied = copied.expect("the branch succeeds");
    assert!(
        took >= Duration::from_secs(2),
        "it waited for the bound: {took:?}"
    );
    assert_eq!(f.user_names(&copied.database).await, ["Ann", "Bob", "Cid"]);
    for kind in RoleKind::SESSION_KINDS {
        f.run(
            kind,
            StatementMode::Read,
            "SELECT count(*) FROM users",
            vec![],
        )
        .await
        .unwrap();
    }
    f.run(
        RoleKind::Write,
        StatementMode::Write,
        "INSERT INTO users (id, name) VALUES (4, 'Dan')",
        vec![],
    )
    .await
    .unwrap();
}

/// A masked branch masks with the source's key and holds the branch's key in `ruum_mask.key`
/// before it is returned (decision 0004-25); non-text personal columns become text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_masked_branch_masks_with_the_source_key_and_holds_its_own() {
    let Some(engine) = engine().await else { return };
    let f = users_with_classes(&engine).await;
    // The source key is random; the branch key is the vector key, so its pseudonyms are known.
    let (source_key, _) = conformance::test_key();
    let source = engine
        .copy_database(
            f.db(),
            &f.key,
            &conformance::new_database(f.db().tenant_id),
            &CopyData::Full,
            &source_key,
        )
        .await
        .unwrap();
    let src = |kind| RoleRef::new(source.database.clone(), kind).unwrap();
    // Re-key the source: a full copy of the vector-key database with its own random key.
    let source_email = engine
        .execute(
            &src(RoleKind::MaskedRead),
            &source_key,
            &request(
                "SELECT email, born FROM users WHERE id = 1",
                StatementMode::Read,
            ),
        )
        .await
        .unwrap()
        .rows[0]
        .clone();
    assert_ne!(
        source_email[0],
        Value::Text("p_a59fc578d4cb".into()),
        "the copy has its own key"
    );
    let col = |c: &str| ColumnRef {
        schema: "public".into(),
        table: "users".into(),
        column: c.into(),
    };
    let plan = MaskPlan {
        columns: vec![
            (col("email"), MaskAs::Pseudonym),
            (col("name"), MaskAs::Pseudonym),
            (col("born"), MaskAs::Pseudonym),
            (col("token"), MaskAs::Null),
        ],
    };
    let branch = engine
        .copy_database(
            &source.database,
            &source_key,
            &conformance::new_database(f.db().tenant_id),
            &CopyData::Masked(plan),
            &vector_key(),
        )
        .await
        .unwrap();
    let br = |kind| RoleRef::new(branch.database.clone(), kind).unwrap();
    // The base table holds what the source's masked lens shows (the source's key).
    let base = engine
        .execute(
            &br(RoleKind::SecretRead),
            &vector_key(),
            &request(
                "SELECT email, born, token FROM users WHERE id = 1",
                StatementMode::Read,
            ),
        )
        .await
        .unwrap();
    assert_eq!(base.rows[0][0], source_email[0]);
    assert_eq!(
        base.rows[0][1], source_email[1],
        "a date column became its text pseudonym"
    );
    assert_eq!(base.rows[0][2], Value::Null, "a secret became NULL");
    assert_eq!(base.columns[1].type_name, "text");
    let catalog = engine.catalog(&branch.database).await.unwrap();
    assert_eq!(
        catalog
            .table("public", "users")
            .unwrap()
            .column("born")
            .unwrap()
            .data_type,
        "text"
    );
    // The branch's own key is installed: a new value gets the vector-key pseudonym.
    engine
        .execute(
            &br(RoleKind::Owner),
            &vector_key(),
            &request(
                "INSERT INTO users (id, email) VALUES (9, 'alice@example.com')",
                StatementMode::Write,
            ),
        )
        .await
        .unwrap();
    let out = engine
        .execute(
            &br(RoleKind::MaskedRead),
            &vector_key(),
            &request("SELECT email FROM users WHERE id = 9", StatementMode::Read),
        )
        .await
        .unwrap();
    assert_eq!(out.rows, [vec![Value::Text("p_a59fc578d4cb".into())]]);
    // The source still works with its own key.
    let again = engine
        .execute(
            &src(RoleKind::MaskedRead),
            &source_key,
            &request(
                "SELECT email, born FROM users WHERE id = 1",
                StatementMode::Read,
            ),
        )
        .await
        .unwrap();
    assert_eq!(again.rows[0], source_email);
}

/// A pg_dump export holds no `ruum_mask`, `ruum_masked` or `ruum_nosecret` object, no role and
/// no grant, and an import of it into a new database installs the key handed in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_export_holds_no_ruum_object_and_imports_with_the_key_handed_in() {
    let Some(engine) = engine().await else { return };
    let (key, _) = conformance::test_key();
    let f = Fixture {
        created: engine
            .create_database(&conformance::new_database(Uuid::now_v7()), &key)
            .await
            .unwrap(),
        engine: &engine,
        key: key.clone(),
        key_bytes: [0; 32],
    };
    f.migrate(
        "CREATE TABLE users (id serial PRIMARY KEY, email text NOT NULL, note text);
         CREATE VIEW emails AS SELECT email FROM users;
         CREATE FUNCTION public.shout(t text) RETURNS text LANGUAGE sql AS $$ SELECT upper(t) $$;",
    )
    .await
    .unwrap();
    f.run(
        RoleKind::Owner,
        StatementMode::Write,
        "INSERT INTO users (email, note) VALUES ('alice@example.com', 'a; b'), ('bob@example.com', E'tab\\there')",
        vec![],
    )
    .await
    .unwrap();
    let text = String::from_utf8(
        engine
            .export(f.db(), ExportFormat::Sql)
            .await
            .unwrap()
            .into_bytes()
            .await
            .unwrap(),
    )
    .unwrap();
    let lower = text.to_lowercase();
    for word in [
        "ruum_mask",
        "ruum_masked",
        "ruum_nosecret",
        "ruum_meta",
        "create role",
        "grant ",
        "revoke ",
        "owner to",
    ] {
        assert!(!lower.contains(word), "the export holds {word:?}");
    }
    for role in &f.created.roles {
        assert!(!text.contains(&role.role_name));
    }
    assert!(text.contains("alice@example.com") && text.contains("CREATE VIEW public.emails"));

    let imported = engine
        .import_sql(
            &conformance::new_database(f.db().tenant_id),
            &vector_key(),
            &text,
            Duration::from_secs(30),
            &ClassSync {
                personal: vec!["email"],
                secret: vec![],
            },
        )
        .await
        .unwrap();
    let role = |kind| RoleRef::new(imported.database.clone(), kind).unwrap();
    let out = engine
        .execute(
            &role(RoleKind::MaskedRead),
            &vector_key(),
            &request(
                "SELECT email, note FROM users ORDER BY id",
                StatementMode::Read,
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        out.rows[0],
        [
            Value::Text("p_a59fc578d4cb".into()),
            Value::Text("a; b".into())
        ]
    );
    assert_eq!(out.rows[1][1], Value::Text("tab\there".into()));
    // The sequence of the serial column came along.
    engine
        .execute(
            &role(RoleKind::Write),
            &vector_key(),
            &request(
                "INSERT INTO users (email) VALUES ('c@example.com')",
                StatementMode::Write,
            ),
        )
        .await
        .unwrap();
    let out = engine
        .execute(
            &role(RoleKind::Owner),
            &vector_key(),
            &request(
                "SELECT max(id), public.shout('x') FROM users",
                StatementMode::Read,
            ),
        )
        .await
        .unwrap();
    assert_eq!(out.rows, [vec![Value::Int(3), Value::Text("X".into())]]);
}

/// The facts of a new database (and of a copy): the pseudonym functions, TEMPORARY, the
/// memberships of ruum_dbhost, the owners of the schemas and tables (decision 0004-33, 0004-34).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_roles_schemas_and_functions_are_as_specified() {
    let Some(engine) = engine().await else { return };
    let f = users_with_classes(&engine).await;
    let (key, _) = conformance::test_key();
    let copy = engine
        .copy_database(
            f.db(),
            &f.key,
            &conformance::new_database(f.db().tenant_id),
            &CopyData::Full,
            &key,
        )
        .await
        .unwrap();
    for created in [&f.created, &copy] {
        let db = &created.database;
        let mut host = dbhost(&engine, db).await;
        let r = |k| role_name(created, k);
        let fns = psql(
            &mut host,
            "select string_agg(distinct format('%s|%s|%s', p.prosecdef, pg_get_userbyid(p.proowner), \
                    array_to_string(p.proconfig, ',')), ';') \
               from pg_proc p where p.pronamespace = 'ruum_mask'::regnamespace and p.proname = 'pseudonym'",
        )
        .await
        .unwrap();
        assert_eq!(
            fns,
            format!("t|{}|search_path=pg_catalog, pg_temp", r(RoleKind::Mask))
        );
        let others = psql(
            &mut host,
            "select count(*) from pg_proc p where p.pronamespace = 'ruum_mask'::regnamespace \
              and not (array_to_string(p.proconfig, ',') like 'search_path=pg_catalog, pg_temp%')",
        )
        .await
        .unwrap();
        assert_eq!(
            others, "0",
            "every function of ruum_mask fixes its search_path"
        );
        // Only the masked kinds may execute pseudonym, nobody hmac12 or canonical.
        let execs = psql(
            &mut host,
            &format!(
                "select count(*) from pg_proc p, pg_roles r \
                  where p.pronamespace = 'ruum_mask'::regnamespace and r.rolname in ({}) \
                    and has_function_privilege(r.oid, p.oid, 'execute') \
                    and (r.rolname not in ('{}', '{}') or p.proname in ('hmac12', 'canonical'))",
                RoleKind::ALL
                    .iter()
                    .filter(|k| **k != RoleKind::Mask)
                    .map(|k| format!("'{}'", r(*k)))
                    .collect::<Vec<_>>()
                    .join(", "),
                r(RoleKind::MaskedRead),
                r(RoleKind::MaskedWrite)
            ),
        )
        .await
        .unwrap();
        assert_eq!(execs, "0");
        for kind in RoleKind::ALL {
            let temp = psql(
                &mut host,
                &format!(
                    "select has_database_privilege('{}', current_database(), 'temporary')",
                    r(kind)
                ),
            )
            .await
            .unwrap();
            assert_eq!(temp, "f", "{} has no TEMPORARY", kind.as_str());
        }
        let members = psql(
            &mut host,
            "select coalesce(string_agg(format('%s:%s/%s/%s', g.rolname, m.admin_option, m.inherit_option, m.set_option), ',' order by g.rolname), '') \
               from pg_auth_members m join pg_roles g on g.oid = m.roleid \
              where m.member = 'ruum_dbhost'::regrole and m.grantor = 'ruum_dbhost'::regrole",
        )
        .await
        .unwrap();
        let mine: Vec<&str> = members
            .split(',')
            .filter(|m| {
                created
                    .roles
                    .iter()
                    .any(|r| m.starts_with(&format!("{}:", r.role_name)))
            })
            .collect();
        assert_eq!(
            mine,
            [format!("{}:f/f/t", r(RoleKind::Mask))],
            "SET-only membership in mask, none in a LOGIN kind"
        );
        let schemas = psql(
            &mut host,
            "select string_agg(distinct pg_get_userbyid(nspowner), ',') from pg_namespace \
              where nspname in ('public', 'ruum_meta', 'ruum_mask', 'ruum_masked', 'ruum_nosecret', 'extensions')",
        )
        .await
        .unwrap();
        assert_eq!(schemas, "ruum_dbhost", "ruum_dbhost owns every schema");
        let owned = psql(
            &mut host,
            &format!(
                "select (select count(*) from pg_namespace where nspowner = '{o}'::regrole) || '|' || \
                        (select count(*) from pg_class where relowner = '{o}'::regrole \
                            and relnamespace not in ('public'::regnamespace, 'pg_toast'::regnamespace)) || '|' || \
                        (select count(*) from pg_class where relowner = '{o}'::regrole and relkind = 'r')",
                o = r(RoleKind::Owner)
            ),
        )
        .await
        .unwrap();
        assert_eq!(
            owned, "0|0|1",
            "the owner kind owns the tables of public only"
        );
        let key_owner = psql(
            &mut host,
            "select pg_get_userbyid(relowner) from pg_class where oid = 'ruum_mask.key'::regclass",
        )
        .await
        .unwrap();
        assert_eq!(key_owner, r(RoleKind::Mask));
        let grants = psql(
            &mut host,
            "select count(*) from pg_class c, aclexplode(c.relacl) a \
              where c.oid = 'ruum_mask.key'::regclass and a.grantee <> c.relowner",
        )
        .await
        .unwrap();
        assert_eq!(grants, "0", "nobody but mask has a right on the key");
        close(host).await;
    }
}

/// The lens views are dropped before a migration and rebuilt with the catalog sync after it,
/// also after a failed one: a migration may retype or drop a column that a lens view reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_migration_may_retype_a_column_of_the_lens() {
    let Some(engine) = engine().await else { return };
    let f = users_with_classes(&engine).await;
    let sync = ClassSync {
        personal: vec!["email", "name", "born"],
        secret: vec!["token"],
    };
    let migrate = |up: &str| MigrationRequest {
        up: up.to_string(),
        timeout: Duration::from_secs(10),
        dry_run: false,
    };
    engine
        .apply_migration(f.db(), &f.key, &migrate("ALTER TABLE users ALTER COLUMN born TYPE timestamptz; ALTER TABLE users DROP COLUMN plan;"), &sync)
        .await
        .unwrap();
    let err = engine
        .apply_migration(
            f.db(),
            &f.key,
            &migrate("ALTER TABLE users DROP COLUMN name; SELECT 1/0;"),
            &sync,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::MigrationFailed(_)));
    for kind in [RoleKind::Read, RoleKind::MaskedRead] {
        let out = f
            .run(
                kind,
                StatementMode::Read,
                "SELECT name, born FROM users WHERE id = 1",
                vec![],
            )
            .await
            .unwrap();
        assert_eq!(out.rows.len(), 1, "{}", kind.as_str());
    }
    let out = f
        .run(
            RoleKind::MaskedRead,
            StatementMode::Read,
            "SELECT born FROM users WHERE id = 1",
            vec![],
        )
        .await
        .unwrap();
    assert!(matches!(&out.rows[0][0], Value::Text(p) if p.starts_with("p_")));
    // A failing sync leaves the lens dropped: the lens kinds fail closed, then a sync repairs it.
    struct Failing;
    #[async_trait]
    impl CatalogSync for Failing {
        async fn sync(&self, _: &Catalog) -> Result<LensSpec, EngineError> {
            Err(EngineError::Internal("platform down".into()))
        }
    }
    let err = engine
        .apply_migration(
            f.db(),
            &f.key,
            &migrate("ALTER TABLE users ADD COLUMN extra text;"),
            &Failing,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::CatalogSync(_)), "{err:?}");
    let err = f
        .run(
            RoleKind::MaskedRead,
            StatementMode::Read,
            "SELECT 1",
            vec![],
        )
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::NotReady), "{err:?}");
    f.run(
        RoleKind::Owner,
        StatementMode::Read,
        "SELECT extra FROM users",
        vec![],
    )
    .await
    .unwrap();
    engine.sync_lens(f.db(), &f.key, &sync).await.unwrap();
    let out = f
        .run(
            RoleKind::MaskedRead,
            StatementMode::Read,
            "SELECT extra FROM users WHERE id = 1",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(out.rows, [vec![Value::Null]]);
}

/// Extensions live in the schema `extensions`, reachable by every kind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extensions_are_reachable_by_every_kind() {
    let Some(engine) = engine().await else { return };
    let mut new = conformance::new_database(Uuid::now_v7());
    new.extensions = vec!["pg_trgm".into(), "citext".into()];
    let (key, key_bytes) = conformance::test_key();
    let created = engine.create_database(&new, &key).await.unwrap();
    let f = Fixture {
        engine: &engine,
        created,
        key,
        key_bytes,
    };
    f.migrate("CREATE TABLE t (name citext);").await.unwrap();
    for kind in RoleKind::SESSION_KINDS {
        let out = f
            .run(
                kind,
                StatementMode::Read,
                "SELECT similarity('abc', 'abd') > 0",
                vec![],
            )
            .await
            .unwrap();
        assert_eq!(out.rows, [vec![Value::Bool(true)]], "{}", kind.as_str());
    }
    let mut host = dbhost(&engine, f.db()).await;
    let where_ = psql(&mut host, "select string_agg(distinct extnamespace::regnamespace::text, ',') from pg_extension where extname <> 'plpgsql'").await.unwrap();
    close(host).await;
    assert_eq!(where_, "extensions");
    assert!(
        engine
            .catalog(f.db())
            .await
            .unwrap()
            .tables
            .iter()
            .all(|t| t.schema == "public")
    );
}

/// Errors carry the SQLSTATE and the primary message only: no row value of the DETAIL.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn errors_hold_no_row_values() {
    let Some(engine) = engine().await else { return };
    let f = users_with_classes(&engine).await;
    for kind in [RoleKind::Owner, RoleKind::Write] {
        let err = f
            .run(
                kind,
                StatementMode::Write,
                "UPDATE users SET email = 'alice@example.com' WHERE id = 2",
                vec![],
            )
            .await
            .unwrap_err();
        let EngineError::Sql { code, message } = &err else {
            panic!("{err:?}")
        };
        assert_eq!(code, "23505");
        assert!(message.contains("users_email_key"), "{message}");
        assert!(
            !message.contains("alice@example.com") && !message.contains("Key ("),
            "{message}"
        );
    }
}

/// Results: types, sources through the lens, numbers, arrays, a type to cast.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn result_values_and_sources() {
    let Some(engine) = engine().await else { return };
    let f = users_with_classes(&engine).await;
    let out = f
        .run(
            RoleKind::Read,
            StatementMode::Read,
            "SELECT id, email, 12.50::numeric AS n, ARRAY[1, NULL, 3] AS a, '{\"k\": 1}'::jsonb AS j, \
                    born, now() > '2000-01-01' AS t, '00000000-0000-0000-0000-000000000001'::uuid AS u \
               FROM users WHERE id = $1 AND email = $2 AND $3::numeric > 1 AND $4::text IS NULL",
            vec![
                Value::Int(1),
                Value::Text("alice@example.com".into()),
                Value::Numeric("1.5".into()),
                Value::Null,
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        out.rows,
        [vec![
            Value::Int(1),
            Value::Text("alice@example.com".into()),
            Value::Numeric("12.50".into()),
            Value::Array(vec![Value::Int(1), Value::Null, Value::Int(3)]),
            Value::Json(serde_json::json!({"k": 1})),
            Value::Date(time::macros::date!(1990 - 04 - 01)),
            Value::Bool(true),
            Value::Uuid(Uuid::from_u128(1)),
        ]]
    );
    let types: Vec<_> = out.columns.iter().map(|c| c.type_name.as_str()).collect();
    assert_eq!(
        types,
        [
            "integer",
            "text",
            "numeric",
            "integer[]",
            "jsonb",
            "date",
            "boolean",
            "uuid"
        ]
    );
    assert_eq!(
        out.columns[1].source,
        Some(ColumnRef {
            schema: "public".into(),
            table: "users".into(),
            column: "email".into()
        }),
        "a column of a lens view comes from the table"
    );
    // A zero-row result still describes its columns.
    let out = f
        .run(
            RoleKind::MaskedRead,
            StatementMode::Read,
            "SELECT email, born FROM users WHERE false",
            vec![],
        )
        .await
        .unwrap();
    assert!(out.rows.is_empty());
    assert_eq!(
        out.columns
            .iter()
            .map(|c| c.type_name.as_str())
            .collect::<Vec<_>>(),
        ["text", "text"]
    );
    let err = f
        .run(
            RoleKind::Read,
            StatementMode::Read,
            "SELECT interval '1 day'",
            vec![],
        )
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::Unsupported(_)), "{err:?}");
}
