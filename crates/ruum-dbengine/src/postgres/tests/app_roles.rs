//! The app roles of a binding on the data cluster (card D-5): sessions that LOG IN as the app
//! role with its short-lived password, as an app does. Each refusal is Postgres's own answer
//! (an SQLSTATE), never a timeout, and each shows its positive control in the same test first
//! (decision 0006 G).

use super::*;
use std::sync::atomic::Ordering::SeqCst;
use std::time::{Duration, Instant};

/// A table, a view, a sequence and rows of the database's own schema.
async fn seeded(engine: &PostgresEngine) -> Fixture<'_> {
    let f = Fixture::new(engine).await;
    f.migrate(
        "CREATE TABLE notes (id serial PRIMARY KEY, body text NOT NULL, token text); \
         CREATE VIEW note_bodies AS SELECT id, body FROM notes; \
         CREATE MATERIALIZED VIEW note_count AS SELECT count(*) AS n FROM notes;",
    )
    .await
    .expect("migration");
    f.run(
        RoleKind::Owner,
        StatementMode::Write,
        "INSERT INTO notes (body, token) VALUES ('first', 'tok_secret_1'), ('second', NULL)",
        vec![],
    )
    .await
    .expect("insert");
    f
}

async fn app_role(f: &Fixture<'_>, kind: RoleKind, binding: Uuid) -> AppRole {
    f.engine
        .create_app_role(f.db(), kind, binding)
        .await
        .expect("create_app_role")
}

async fn creds(engine: &PostgresEngine, role: &AppRole, valid_for: Duration) -> AppCredentials {
    engine
        .app_credentials(&role.role_ref().unwrap(), valid_for)
        .await
        .expect("app_credentials")
}

/// A session that logs in with `creds` (over TCP with SCRAM, as an app does) to `database`
/// (default: the role's own database).
async fn login(
    engine: &PostgresEngine,
    creds: &AppCredentials,
    database: Option<&str>,
) -> Result<PgConnection, sqlx::Error> {
    let options = engine
        .config
        .connect_options()
        .username(&creds.role_name)
        .password(creds.expose_password())
        .database(database.unwrap_or(&creds.database));
    PgConnection::connect_with(&options).await
}

/// A session with another role name than the one of `creds` (binding B's password on A's role).
async fn login_as(
    engine: &PostgresEngine,
    role_name: &str,
    password: &str,
    database: &str,
) -> Result<PgConnection, sqlx::Error> {
    let options = engine
        .config
        .connect_options()
        .username(role_name)
        .password(password)
        .database(database);
    PgConnection::connect_with(&options).await
}

fn code(e: &sqlx::Error) -> String {
    errors::sqlstate(e).unwrap_or_else(|| format!("no sqlstate: {e}"))
}

/// The SQLSTATE of a failed statement (the session stays usable for the next one).
async fn refused(conn: &mut PgConnection, sql: &str) -> String {
    match psql(conn, sql).await {
        Ok(out) => panic!("{sql} was not refused: {out}"),
        Err(e) => e
            .rsplit_once('(')
            .map(|(_, c)| c.trim_end_matches(')').to_string())
            .unwrap_or(e),
    }
}

async fn ok(conn: &mut PgConnection, sql: &str) -> String {
    psql(conn, sql)
        .await
        .unwrap_or_else(|e| panic!("{sql} failed: {e}"))
}

/// Waits up to `limit` for a session to end (its next statement fails with a lost connection).
async fn ended_within(conn: &mut PgConnection, limit: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if psql(conn, "SELECT 1").await.is_err() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// The kinds table of `docs/STEP2.md`: lens `base` (real values and secrets), `app_read` reads,
/// `app_write` also writes rows; neither changes the schema or reaches another role.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn app_role_grants_follow_the_kinds_table() {
    let Some(engine) = engine().await else { return };
    let f = seeded(&engine).await;
    let binding = Uuid::now_v7();
    let read = app_role(&f, RoleKind::AppRead, binding).await;
    let write = app_role(&f, RoleKind::AppWrite, binding).await;
    let mut r = login(
        &engine,
        &creds(&engine, &read, Duration::from_secs(300)).await,
        None,
    )
    .await
    .expect("app_read logs in");
    let mut w = login(
        &engine,
        &creds(&engine, &write, Duration::from_secs(300)).await,
        None,
    )
    .await
    .expect("app_write logs in");

    // Lens base: the real values, secrets included, through the plain names.
    assert_eq!(ok(&mut r, "SHOW search_path").await, "public, extensions");
    assert_eq!(
        ok(&mut r, "SELECT body, token FROM notes ORDER BY id").await,
        "first|tok_secret_1\nsecond|"
    );
    assert_eq!(ok(&mut r, "SELECT count(*) FROM note_bodies").await, "2");
    assert_eq!(ok(&mut r, "SELECT count(*) FROM note_count").await, "1");
    assert_eq!(
        ok(&mut r, "SELECT session_user::text").await,
        read.role_name
    );
    // app_read cannot write.
    for sql in [
        "INSERT INTO notes (body) VALUES ('x')",
        "UPDATE notes SET body = 'x'",
        "DELETE FROM notes",
    ] {
        assert_eq!(refused(&mut r, sql).await, "42501", "{sql}");
    }
    // app_write writes rows (the sequence of the default too).
    ok(&mut w, "INSERT INTO notes (body) VALUES ('third')").await;
    ok(
        &mut w,
        "UPDATE notes SET body = 'THIRD' WHERE body = 'third'",
    )
    .await;
    ok(&mut w, "DELETE FROM notes WHERE body = 'THIRD'").await;
    // Neither changes the schema, truncates, makes temp tables or reaches server files.
    for conn in [&mut r, &mut w] {
        for (sql, expected) in [
            ("TRUNCATE notes", "42501"),
            ("ALTER TABLE notes ADD COLUMN x int", "42501"),
            ("DROP TABLE notes", "42501"),
            ("CREATE TABLE mine (x int)", "42501"),
            ("CREATE TEMP TABLE mine (x int)", "42501"),
            ("CREATE SCHEMA mine", "42501"),
            ("CREATE INDEX ON notes (body)", "42501"),
            (
                "CREATE FUNCTION f() RETURNS int LANGUAGE sql AS 'SELECT 1'",
                "42501",
            ),
            ("COPY notes TO PROGRAM 'id'", "42501"),
            ("COPY notes FROM '/etc/passwd'", "42501"),
            ("SELECT lo_import('/etc/passwd')", "42501"),
            ("SELECT pg_read_file('/etc/passwd')", "42501"),
            ("REFRESH MATERIALIZED VIEW note_count", "42501"),
        ] {
            assert_eq!(refused(conn, sql).await, expected, "{sql}");
        }
    }
    // No escalation: SET ROLE and SET SESSION AUTHORIZATION reach no other role.
    let owner = role_name(&f.created, RoleKind::Owner);
    let mask = role_name(&f.created, RoleKind::Mask);
    for target in [
        owner.as_str(),
        mask.as_str(),
        "ruum_dbhost",
        "postgres",
        &write.role_name,
    ] {
        assert_eq!(
            refused(&mut r, &format!("SET ROLE {}", lens::ident(target))).await,
            "42501",
            "SET ROLE {target}"
        );
        assert_eq!(
            refused(
                &mut r,
                &format!("SET SESSION AUTHORIZATION {}", lens::ident(target))
            )
            .await,
            "42501",
            "SET SESSION AUTHORIZATION {target}"
        );
    }
    // No pseudonym oracle, no key, no Ruum schema.
    for sql in [
        "SELECT ruum_mask.pseudonym('alice@example.com'::text)",
        "SELECT * FROM ruum_mask.key",
        "SELECT * FROM ruum_meta.ident",
        "SELECT * FROM ruum_meta.roles",
        "SELECT * FROM ruum_masked.notes",
        "SELECT * FROM ruum_nosecret.notes",
    ] {
        assert_eq!(refused(&mut r, sql).await, "42501", "{sql}");
        assert_eq!(refused(&mut w, sql).await, "42501", "{sql}");
    }
    // No right in a Ruum schema and no CREATE anywhere (not only a refused function call).
    for conn in [&mut r, &mut w] {
        assert_eq!(
            ok(
                conn,
                "SELECT has_schema_privilege('public', 'USAGE'), \
                 has_schema_privilege('extensions', 'USAGE'), \
                 has_schema_privilege('public', 'CREATE'), \
                 has_schema_privilege('ruum_mask', 'USAGE'), \
                 has_schema_privilege('ruum_meta', 'USAGE'), \
                 has_schema_privilege('ruum_masked', 'USAGE'), \
                 has_schema_privilege('ruum_nosecret', 'USAGE'), \
                 has_database_privilege(current_database(), 'CREATE'), \
                 has_database_privilege(current_database(), 'TEMPORARY')"
            )
            .await,
            "t|t|f|f|f|f|f|f|f"
        );
    }
    // pg_roles: no power, no membership, the connection limit and the role settings.
    for role in [&read, &write] {
        let mut admin = engine.admin().await.unwrap();
        let out = psql(
            &mut admin,
            &format!(
                "SELECT rolcanlogin, rolinherit, rolsuper, rolcreaterole, rolcreatedb, \
                 rolreplication, rolbypassrls, rolconnlimit, \
                 (SELECT count(*) FROM pg_auth_members m WHERE m.member = r.oid), \
                 (SELECT count(*) FROM pg_auth_members m WHERE m.roleid = r.oid \
                    AND m.member <> (SELECT oid FROM pg_roles WHERE rolname = 'ruum_dbhost')) \
                 FROM pg_roles r WHERE rolname = '{}'",
                role.role_name
            ),
        )
        .await
        .unwrap();
        assert_eq!(out, "t|f|f|f|f|f|f|5|0|0", "{}", role.role_name);
    }
    assert_eq!(ok(&mut r, "SHOW statement_timeout").await, "30s");
    assert_eq!(
        ok(&mut r, "SHOW idle_in_transaction_session_timeout").await,
        "1min"
    );
    close(r).await;
    close(w).await;
}

/// `CONNECT` on its own database only: another tenant's database, another database of the
/// same tenant, `postgres` and `template1` refuse the login with 42501.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn app_role_connects_only_to_its_own_database() {
    let Some(engine) = engine().await else { return };
    let a = seeded(&engine).await;
    let b = Fixture::new(&engine).await;
    let same_tenant = engine
        .create_database(&conformance::new_database(a.db().tenant_id), &a.key)
        .await
        .unwrap();
    let role = app_role(&a, RoleKind::AppWrite, Uuid::now_v7()).await;
    // The other databases have app roles of their own, so a CONNECT given to PUBLIC on them
    // would show here.
    app_role(&b, RoleKind::AppRead, Uuid::now_v7()).await;
    engine
        .create_app_role(&same_tenant.database, RoleKind::AppRead, Uuid::now_v7())
        .await
        .unwrap();
    let c = creds(&engine, &role, Duration::from_secs(300)).await;
    let mut own = login(&engine, &c, None).await.expect("own database");
    assert_eq!(ok(&mut own, "SELECT count(*) FROM notes").await, "2");
    close(own).await;
    for db in [
        b.db().engine_name.as_str(),
        same_tenant.database.engine_name.as_str(),
        "postgres",
        "template1",
    ] {
        match login(&engine, &c, Some(db)).await {
            Ok(_) => panic!("the app role logged in to {db}"),
            Err(e) => assert_eq!(code(&e), "42501", "{db}: {e}"),
        }
    }
    engine.drop_database(&same_tenant.database).await.unwrap();
}

/// The pseudonym function is out of reach on a schema-only branch too (D-3a).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_pseudonym_oracle_on_a_schema_only_branch() {
    let Some(engine) = engine().await else { return };
    let f = seeded(&engine).await;
    let branch = engine
        .copy_database(
            f.db(),
            &f.key,
            &conformance::new_database(f.db().tenant_id),
            &CopyData::SchemaOnly(MaskPlan::default()),
            &f.key,
        )
        .await
        .unwrap();
    let role = engine
        .create_app_role(&branch.database, RoleKind::AppRead, Uuid::now_v7())
        .await
        .unwrap();
    let mut s = login(
        &engine,
        &creds(&engine, &role, Duration::from_secs(300)).await,
        None,
    )
    .await
    .unwrap();
    assert_eq!(ok(&mut s, "SELECT count(*) FROM notes").await, "0");
    for sql in [
        "SELECT ruum_mask.pseudonym('x'::text)",
        "SELECT * FROM ruum_mask.key",
        "SELECT * FROM ruum_masked.notes",
    ] {
        assert_eq!(refused(&mut s, sql).await, "42501", "{sql}");
    }
    close(s).await;
    engine.drop_database(&branch.database).await.unwrap();
}

/// Two bindings, two roles: B's password does not log in as A's role; dropping A ends A's
/// sessions within 5 seconds and leaves B working.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bindings_are_separate_and_a_drop_ends_sessions_in_five_seconds() {
    let Some(engine) = engine().await else { return };
    let f = seeded(&engine).await;
    let a = app_role(&f, RoleKind::AppRead, Uuid::now_v7()).await;
    let b = app_role(&f, RoleKind::AppRead, Uuid::now_v7()).await;
    assert_ne!(a.role_name, b.role_name);
    let ca = creds(&engine, &a, Duration::from_secs(300)).await;
    let cb = creds(&engine, &b, Duration::from_secs(300)).await;
    let mut sa = login(&engine, &ca, None).await.unwrap();
    let mut sb = login(&engine, &cb, None).await.unwrap();
    assert_eq!(ok(&mut sa, "SELECT count(*) FROM notes").await, "2");
    match login_as(&engine, &a.role_name, cb.expose_password(), &ca.database).await {
        Ok(_) => panic!("binding B's password logged in as A's role"),
        Err(e) => assert_eq!(code(&e), "28P01", "{e}"),
    }
    let start = Instant::now();
    engine
        .drop_app_role(&a.role_ref().unwrap())
        .await
        .expect("drop A");
    assert!(
        ended_within(
            &mut sa,
            Duration::from_secs(5).saturating_sub(start.elapsed())
        )
        .await,
        "A's session ended within 5 s"
    );
    assert!(start.elapsed() < Duration::from_secs(5));
    match login(&engine, &ca, None).await {
        Ok(_) => panic!("a dropped role logged in"),
        Err(e) => assert_eq!(code(&e), "28P01", "{e}"),
    }
    let mut admin = engine.admin().await.unwrap();
    assert_eq!(
        psql(
            &mut admin,
            &format!(
                "SELECT count(*) FROM pg_roles WHERE rolname = '{}'",
                a.role_name
            )
        )
        .await
        .unwrap(),
        "0"
    );
    // B keeps working.
    assert_eq!(ok(&mut sb, "SELECT count(*) FROM notes").await, "2");
    close(sb).await;
    let mut sb = login(&engine, &cb, None).await.unwrap();
    assert_eq!(ok(&mut sb, "SELECT count(*) FROM notes").await, "2");
    close(sb).await;
    // A second drop is no error.
    engine.drop_app_role(&a.role_ref().unwrap()).await.unwrap();
}

/// A login after `VALID UNTIL` fails; a new password works and the old one stops.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn passwords_are_short_lived() {
    let Some(engine) = engine().await else { return };
    let f = seeded(&engine).await;
    let role = app_role(&f, RoleKind::AppRead, Uuid::now_v7()).await;
    let short = creds(&engine, &role, Duration::from_secs(2)).await;
    let s = login(&engine, &short, None)
        .await
        .expect("before VALID UNTIL");
    close(s).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    match login(&engine, &short, None).await {
        Ok(_) => panic!("a login after VALID UNTIL worked"),
        Err(e) => assert_eq!(code(&e), "28P01", "{e}"),
    }
    let first = creds(&engine, &role, Duration::from_secs(300)).await;
    let s = login(&engine, &first, None).await.expect("a new password");
    close(s).await;
    let second = creds(&engine, &role, Duration::from_secs(300)).await;
    assert_ne!(first.expose_password(), second.expose_password());
    match login(&engine, &first, None).await {
        Ok(_) => panic!("the old password still works"),
        Err(e) => assert_eq!(code(&e), "28P01", "{e}"),
    }
    let s = login(&engine, &second, None)
        .await
        .expect("the newest password");
    close(s).await;
    // The role has VALID UNTIL set, close to what was asked.
    let mut admin = engine.admin().await.unwrap();
    let left = psql(
        &mut admin,
        &format!(
            "SELECT extract(epoch FROM rolvaliduntil - now())::int FROM pg_roles \
             WHERE rolname = '{}'",
            role.role_name
        ),
    )
    .await
    .unwrap();
    let left: i64 = left.parse().unwrap();
    assert!((250..=301).contains(&left), "VALID UNTIL in {left} s");
}

/// Suspend ends the open sessions and refuses logins and new passwords; resume lets it in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suspend_ends_sessions_and_refuses_logins() {
    let Some(engine) = engine().await else { return };
    let f = seeded(&engine).await;
    let role = app_role(&f, RoleKind::AppWrite, Uuid::now_v7()).await;
    let c = creds(&engine, &role, Duration::from_secs(300)).await;
    let mut s = login(&engine, &c, None).await.unwrap();
    assert_eq!(ok(&mut s, "SELECT count(*) FROM notes").await, "2");
    let r = role.role_ref().unwrap();
    engine.suspend_app_role(&r).await.unwrap();
    assert!(ended_within(&mut s, Duration::from_secs(5)).await);
    match login(&engine, &c, None).await {
        Ok(_) => panic!("a suspended role logged in"),
        Err(e) => assert_eq!(code(&e), "28000", "{e}"),
    }
    assert!(matches!(
        engine.app_credentials(&r, Duration::from_secs(60)).await,
        Err(EngineError::PermissionDenied(_))
    ));
    engine.resume_app_role(&r).await.unwrap();
    let c = creds(&engine, &role, Duration::from_secs(300)).await;
    let mut s = login(&engine, &c, None).await.expect("resumed");
    assert_eq!(ok(&mut s, "SELECT count(*) FROM notes").await, "2");
    close(s).await;
}

/// Tables of a later schema change reach the app role through the default privileges (an
/// `owner` statement, no migration), and a migration that drops or revokes keeps the grants
/// (the engine grants again after it).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn later_tables_reach_the_app_role() {
    let Some(engine) = engine().await else { return };
    let f = seeded(&engine).await;
    let read = app_role(&f, RoleKind::AppRead, Uuid::now_v7()).await;
    let write = app_role(&f, RoleKind::AppWrite, Uuid::now_v7()).await;
    // Default privileges only: DDL in an owner session, which runs no re-grant.
    f.run(
        RoleKind::Owner,
        StatementMode::Write,
        "CREATE TABLE later (id serial PRIMARY KEY, x int)",
        vec![],
    )
    .await
    .unwrap();
    let mut r = login(
        &engine,
        &creds(&engine, &read, Duration::from_secs(300)).await,
        None,
    )
    .await
    .unwrap();
    let mut w = login(
        &engine,
        &creds(&engine, &write, Duration::from_secs(300)).await,
        None,
    )
    .await
    .unwrap();
    ok(&mut w, "INSERT INTO later (x) VALUES (1)").await;
    assert_eq!(ok(&mut r, "SELECT count(*) FROM later").await, "1");
    assert_eq!(
        refused(&mut r, "INSERT INTO later (x) VALUES (2)").await,
        "42501"
    );
    // A migration drops and makes a table again and revokes another: the grants follow.
    f.migrate(&format!(
        "DROP VIEW note_bodies; DROP MATERIALIZED VIEW note_count; DROP TABLE notes; \
         CREATE TABLE notes (id serial PRIMARY KEY, body text); \
         REVOKE ALL ON later FROM {r}, {w};",
        r = lens::ident(&read.role_name),
        w = lens::ident(&write.role_name),
    ))
    .await
    .unwrap();
    ok(&mut w, "INSERT INTO notes (body) VALUES ('again')").await;
    assert_eq!(ok(&mut r, "SELECT count(*) FROM notes").await, "1");
    assert_eq!(ok(&mut r, "SELECT count(*) FROM later").await, "1");
    ok(&mut w, "INSERT INTO later (x) VALUES (3)").await;
    close(r).await;
    close(w).await;
}

/// The connection limit of a role holds, and the cluster budget refuses a new app role before
/// it exists (no role, no row).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_limits_and_the_budget() {
    let Some(engine) = engine().await else { return };
    let f = seeded(&engine).await;
    let role = app_role(&f, RoleKind::AppRead, Uuid::now_v7()).await;
    let c = creds(&engine, &role, Duration::from_secs(300)).await;
    let mut open = Vec::new();
    for _ in 0..engine.config.app_connection_limit {
        open.push(login(&engine, &c, None).await.expect("within the limit"));
    }
    match login(&engine, &c, None).await {
        Ok(_) => panic!("a login over the role's connection limit worked"),
        Err(e) => assert_eq!(code(&e), "53300", "{e}"),
    }
    for s in open {
        close(s).await;
    }
    // A budget of 0: every new app role is refused, before the role or its row exists.
    let Some(tight) = engine_with(|c| c.app_connection_budget = Some(0)).await else {
        return;
    };
    let before = tight.app_roles(f.db()).await.unwrap().len();
    match tight
        .create_app_role(f.db(), RoleKind::AppWrite, Uuid::now_v7())
        .await
    {
        Err(EngineError::PermissionDenied(m)) => assert_eq!(m, "app_connection_budget"),
        other => panic!("over the budget: {other:?}"),
    }
    assert_eq!(tight.app_roles(f.db()).await.unwrap().len(), before);
    // An existing role is no new connection: asking for it again works.
    let again = tight
        .create_app_role(f.db(), RoleKind::AppRead, role.binding)
        .await
        .unwrap();
    assert_eq!(again.role_name, role.role_name);
}

/// `temp_file_limit` is a role setting the app cannot raise (when the cluster set-up lets
/// ruum_dbhost set it; the test grants that as the superuser).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn temp_file_limit_is_set_and_the_app_cannot_raise_it() {
    let Some(engine) = engine().await else { return };
    if superuser(
        "postgres",
        "GRANT SET ON PARAMETER temp_file_limit TO ruum_dbhost",
    )
    .is_none()
    {
        return;
    }
    let f = seeded(&engine).await;
    let role = app_role(&f, RoleKind::AppWrite, Uuid::now_v7()).await;
    let mut s = login(
        &engine,
        &creds(&engine, &role, Duration::from_secs(300)).await,
        None,
    )
    .await
    .unwrap();
    assert_eq!(ok(&mut s, "SHOW temp_file_limit").await, "1GB");
    assert_eq!(refused(&mut s, "SET temp_file_limit = -1").await, "42501");
    assert_eq!(
        refused(
            &mut s,
            &format!(
                "ALTER ROLE {} SET temp_file_limit = -1",
                lens::ident(&role.role_name)
            )
        )
        .await,
        "42501"
    );
    close(s).await;
}

/// The ACL entries of a database that name `role`: tables, columns, functions, schemas, types,
/// default privileges (as grantee or owner) and policies.
async fn mentions(engine: &PostgresEngine, db: &DatabaseRef, role: &str) -> String {
    let mut conn = dbhost(engine, db).await;
    let out = psql(
        &mut conn,
        &format!(
            "WITH r AS (SELECT oid FROM pg_roles WHERE rolname = '{role}')
             SELECT (SELECT count(*) FROM pg_class c, aclexplode(c.relacl) a, r WHERE a.grantee = r.oid)
                  + (SELECT count(*) FROM pg_attribute t, aclexplode(t.attacl) a, r WHERE a.grantee = r.oid)
                  + (SELECT count(*) FROM pg_proc p, aclexplode(p.proacl) a, r WHERE a.grantee = r.oid)
                  + (SELECT count(*) FROM pg_namespace n, aclexplode(n.nspacl) a, r WHERE a.grantee = r.oid)
                  + (SELECT count(*) FROM pg_type t, aclexplode(t.typacl) a, r WHERE a.grantee = r.oid)
                  + (SELECT count(*) FROM pg_default_acl d, aclexplode(d.defaclacl) a, r WHERE a.grantee = r.oid)
                  + (SELECT count(*) FROM pg_default_acl d, r WHERE d.defaclrole = r.oid)
                  + (SELECT count(*) FROM pg_policy p, r WHERE r.oid = ANY (p.polroles))
                  + (SELECT count(*) FROM pg_shdepend d, r WHERE d.refobjid = r.oid
                       AND d.dbid = (SELECT oid FROM pg_database WHERE datname = current_database()))"
        ),
    )
    .await
    .unwrap();
    close(conn).await;
    out
}

/// Copies never carry app grants: a full copy, a schema-only copy and a restore into a new
/// database name no app role of the source (also not one that made default
/// privileges of its own, nor a policy that names it), and the source's role, given `CONNECT`
/// on the copy by the superuser, reads nothing there. The source's role keeps working, and is
/// dropped while the restore point exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copies_carry_no_app_grants() {
    let Some(engine) = engine().await else { return };
    let f = seeded(&engine).await;
    let read = app_role(&f, RoleKind::AppRead, Uuid::now_v7()).await;
    let write = app_role(&f, RoleKind::AppWrite, Uuid::now_v7()).await;
    // A policy that names the app role (a tenant migration may write one).
    f.migrate(&format!(
        "ALTER TABLE notes ENABLE ROW LEVEL SECURITY; \
         CREATE POLICY app_reads ON notes FOR SELECT TO {} USING (true); \
         CREATE POLICY everyone ON notes FOR ALL TO {}, public USING (true);",
        lens::ident(&read.role_name),
        lens::ident(&read.role_name),
    ))
    .await
    .unwrap();
    let cr = creds(&engine, &read, Duration::from_secs(300)).await;
    let cw = creds(&engine, &write, Duration::from_secs(300)).await;
    // Default privileges the app role makes for itself (any role may).
    let mut w = login(&engine, &cw, None).await.unwrap();
    ok(
        &mut w,
        "ALTER DEFAULT PRIVILEGES GRANT SELECT ON TABLES TO PUBLIC; \
         ALTER DEFAULT PRIVILEGES REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC; \
         ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT ON TABLES TO PUBLIC",
    )
    .await;
    close(w).await;
    for role in [&read, &write] {
        assert_ne!(mentions(&engine, f.db(), &role.role_name).await, "0");
    }
    let point = engine.create_restore_point(f.db()).await.unwrap();
    let new = || conformance::new_database(f.db().tenant_id);
    let full = engine
        .copy_database(f.db(), &f.key, &new(), &CopyData::Full, &f.key)
        .await
        .unwrap();
    let schema_only = engine
        .copy_database(
            f.db(),
            &f.key,
            &new(),
            &CopyData::SchemaOnly(MaskPlan::default()),
            &f.key,
        )
        .await
        .unwrap();
    let restored = engine
        .restore_into_new(f.db(), &point, &new(), &f.key)
        .await
        .unwrap();
    for copy in [&full, &schema_only, &restored] {
        assert!(engine.app_roles(&copy.database).await.unwrap().is_empty());
        for role in [&read, &write] {
            assert_eq!(
                mentions(&engine, &copy.database, &role.role_name).await,
                "0",
                "the copy names the source's app role {}",
                role.role_name
            );
        }
        // The policy that named only the app role is gone; the other one keeps its other role.
        let mut conn = dbhost(&engine, &copy.database).await;
        assert_eq!(
            psql(
                &mut conn,
                "SELECT string_agg(polname, ',' ORDER BY polname) FROM pg_policy"
            )
            .await
            .unwrap(),
            "everyone"
        );
        close(conn).await;
    }
    // The superuser gives the source's role CONNECT on the full copy: it reads nothing.
    if superuser(
        "postgres",
        &format!(
            "GRANT CONNECT ON DATABASE {} TO {}",
            lens::ident(&full.database.engine_name),
            lens::ident(&read.role_name)
        ),
    )
    .is_some()
    {
        let mut s = login(&engine, &cr, Some(&full.database.engine_name))
            .await
            .expect("CONNECT given by the superuser");
        // No USAGE on its schema: the name does not resolve, the qualified name is refused.
        for (sql, expected) in [
            ("SELECT count(*) FROM notes", "42P01"),
            ("SELECT count(*) FROM public.notes", "42501"),
            ("SELECT count(*) FROM public.note_bodies", "42501"),
        ] {
            assert_eq!(refused(&mut s, sql).await, expected, "{sql}");
        }
        close(s).await;
    }
    // The source's roles keep working.
    let mut s = login(&engine, &cr, None).await.unwrap();
    assert_eq!(ok(&mut s, "SELECT count(*) FROM notes").await, "2");
    close(s).await;
    // The copies go first (the superuser's CONNECT on one of them names the role).
    for copy in [full, schema_only, restored] {
        engine.drop_database(&copy.database).await.unwrap();
    }
    // The roles drop while the restore point exists (they leave it too), with their default
    // privileges and policies.
    for role in [&read, &write] {
        engine
            .drop_app_role(&role.role_ref().unwrap())
            .await
            .expect("drop with a restore point");
        assert_eq!(mentions(&engine, f.db(), &role.role_name).await, "0");
    }
    let mut conn = dbhost(&engine, f.db()).await;
    assert_eq!(
        psql(
            &mut conn,
            "SELECT string_agg(polname, ',' ORDER BY polname) FROM pg_policy"
        )
        .await
        .unwrap(),
        "everyone"
    );
    close(conn).await;
}

/// A restore in place keeps the app roles of now (not those of the restore point), with their
/// grants; a role made after the restore point reads the restored tables.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restore_in_place_keeps_the_app_roles_of_now() {
    let Some(engine) = engine().await else { return };
    let f = seeded(&engine).await;
    let early = app_role(&f, RoleKind::AppRead, Uuid::now_v7()).await;
    let point = engine.create_restore_point(f.db()).await.unwrap();
    let late = app_role(&f, RoleKind::AppWrite, Uuid::now_v7()).await;
    f.run(
        RoleKind::Owner,
        StatementMode::Write,
        "INSERT INTO notes (body) VALUES ('after the point')",
        vec![],
    )
    .await
    .unwrap();
    engine
        .restore_in_place(f.db(), &point, &f.key)
        .await
        .unwrap();
    let mut names: Vec<_> = engine
        .app_roles(f.db())
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.role_name)
        .collect();
    names.sort();
    let mut expected = vec![early.role_name.clone(), late.role_name.clone()];
    expected.sort();
    assert_eq!(names, expected);
    for role in [&early, &late] {
        let mut s = login(
            &engine,
            &creds(&engine, role, Duration::from_secs(300)).await,
            None,
        )
        .await
        .unwrap();
        assert_eq!(ok(&mut s, "SELECT count(*) FROM notes").await, "2");
        close(s).await;
    }
}

/// `drop_database` leaves no app role of the database in the cluster; the seven roles of step 2
/// are made as before, and the platform never runs a statement as an app role.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drop_database_leaves_no_app_role() {
    let Some(engine) = engine().await else { return };
    let f = seeded(&engine).await;
    let kinds: Vec<_> = f.created.roles.iter().map(|r| r.kind).collect();
    assert_eq!(kinds, RoleKind::ALL);
    let role = app_role(&f, RoleKind::AppWrite, Uuid::now_v7()).await;
    let c = creds(&engine, &role, Duration::from_secs(300)).await;
    let mut s = login(&engine, &c, None).await.unwrap();
    assert!(matches!(
        f.engine
            .execute(
                &role.role_ref().unwrap(),
                &f.key,
                &StatementRequest {
                    sql: "SELECT 1".into(),
                    params: vec![],
                    mode: StatementMode::Read,
                    timeout: Duration::from_secs(5),
                    max_rows: 1,
                    max_bytes: None,
                },
            )
            .await,
        Err(EngineError::InvalidRole(_))
    ));
    engine.drop_database(f.db()).await.unwrap();
    assert!(ended_within(&mut s, Duration::from_secs(5)).await);
    let mut admin = engine.admin().await.unwrap();
    assert_eq!(
        psql(
            &mut admin,
            &format!(
                "SELECT count(*) FROM pg_roles WHERE rolname = '{}'",
                role.role_name
            )
        )
        .await
        .unwrap(),
        "0"
    );
}

/// The password is in no log line: the tracing output of the engine while it makes a role,
/// issues passwords, logs in, suspends and drops never holds one, nor does an error or `Debug`.
#[tokio::test(flavor = "current_thread")]
async fn passwords_are_never_logged() {
    use std::io::Write;
    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let Some(engine) = engine().await else { return };
    let f = seeded(&engine).await;
    let role = app_role(&f, RoleKind::AppWrite, Uuid::now_v7()).await;
    let mut passwords = Vec::new();
    for _ in 0..3 {
        let c = creds(&engine, &role, Duration::from_secs(300)).await;
        tracing::info!(credentials = ?c, shown = %c, "issued");
        let s = login(&engine, &c, None).await.unwrap();
        close(s).await;
        passwords.push(c.expose_password().to_string());
    }
    let r = role.role_ref().unwrap();
    engine.suspend_app_role(&r).await.unwrap();
    let refused = engine.app_credentials(&r, Duration::from_secs(60)).await;
    tracing::info!(error = ?refused, "refused while suspended");
    engine.resume_app_role(&r).await.unwrap();
    engine.drop_app_role(&r).await.unwrap();
    let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(out.contains("issued"), "the capture works: {out}");
    for pw in &passwords {
        assert!(!out.contains(pw.as_str()), "a password is in the log");
    }
}

/// How many roles named `name` the cluster has.
async fn role_count(engine: &PostgresEngine, name: &str) -> String {
    let mut admin = engine.admin().await.unwrap();
    psql(
        &mut admin,
        &format!("SELECT count(*) FROM pg_roles WHERE rolname = '{name}'"),
    )
    .await
    .unwrap()
}

/// The owner kind's role of a fixture.
fn owner_of<'a>(f: &'a Fixture<'_>) -> &'a str {
    &f.created
        .roles
        .iter()
        .find(|r| r.kind == RoleKind::Owner)
        .unwrap()
        .role_name
}

/// Large objects (review D-5, 1): an app role cannot make one (the cluster set-up revokes the
/// volatile `lo_*` functions from `PUBLIC`). One that the superuser gives it blocks neither a
/// copy nor the drop of the role: a copy gives it to its owner, a drop to the database's
/// owner, also in the restore point; a large-object grant to another app role leaves with that
/// role.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn large_objects_of_an_app_role_block_neither_copies_nor_the_drop() {
    let Some(engine) = engine().await else { return };
    let f = seeded(&engine).await;
    let read = app_role(&f, RoleKind::AppRead, Uuid::now_v7()).await;
    let other = app_role(&f, RoleKind::AppRead, Uuid::now_v7()).await;
    let mut s = login(
        &engine,
        &creds(&engine, &read, Duration::from_secs(300)).await,
        None,
    )
    .await
    .unwrap();
    assert_eq!(ok(&mut s, "SELECT count(*) FROM notes").await, "2");
    assert_eq!(refused(&mut s, "SELECT lo_create(0)").await, "42501");
    assert_eq!(
        refused(&mut s, "SELECT lo_from_bytea(0, decode('6869', 'hex'))").await,
        "42501"
    );
    close(s).await;
    let Some(made) = superuser(
        &f.db().engine_name,
        &format!(
            "SELECT lo_create(0) \\gset\n\
             SELECT lo_from_bytea(0, decode('6869', 'hex')) AS second \\gset\n\
             ALTER LARGE OBJECT :lo_create OWNER TO {r};\n\
             ALTER LARGE OBJECT :second OWNER TO {r};\n\
             GRANT SELECT ON LARGE OBJECT :lo_create TO {o};\n\
             SELECT count(*) FROM pg_largeobject_metadata;",
            r = lens::ident(&read.role_name),
            o = lens::ident(&other.role_name),
        ),
    ) else {
        return;
    };
    assert_eq!(made.trim(), "2", "{made}");
    assert_ne!(mentions(&engine, f.db(), &read.role_name).await, "0");
    assert_ne!(mentions(&engine, f.db(), &other.role_name).await, "0");
    let point = engine.create_restore_point(f.db()).await.unwrap();
    let new = || conformance::new_database(f.db().tenant_id);
    let full = engine
        .copy_database(f.db(), &f.key, &new(), &CopyData::Full, &f.key)
        .await
        .expect("a full copy");
    let schema_only = engine
        .copy_database(
            f.db(),
            &f.key,
            &new(),
            &CopyData::SchemaOnly(MaskPlan::default()),
            &f.key,
        )
        .await
        .expect("a schema-only copy");
    let restored = engine
        .restore_into_new(f.db(), &point, &new(), &f.key)
        .await
        .expect("a restore into a new database");
    for copy in [&full, &schema_only, &restored] {
        for role in [&read, &other] {
            assert_eq!(
                mentions(&engine, &copy.database, &role.role_name).await,
                "0"
            );
        }
    }
    for copy in [full, schema_only, restored] {
        engine.drop_database(&copy.database).await.unwrap();
    }
    engine
        .drop_app_role(&read.role_ref().unwrap())
        .await
        .expect("drop with large objects");
    assert_eq!(role_count(&engine, &read.role_name).await, "0");
    // The large objects stay, with the owner; the grant to the other role stays until it goes.
    let mut conn = dbhost(&engine, f.db()).await;
    assert_eq!(
        psql(
            &mut conn,
            &format!(
                "SELECT count(*) FROM pg_largeobject_metadata \
                 WHERE lomowner = (SELECT oid FROM pg_roles WHERE rolname = '{}')",
                owner_of(&f)
            )
        )
        .await
        .unwrap(),
        "2"
    );
    close(conn).await;
    assert_ne!(mentions(&engine, f.db(), &other.role_name).await, "0");
    engine
        .drop_app_role(&other.role_ref().unwrap())
        .await
        .expect("drop with a large-object grant");
    assert_eq!(role_count(&engine, &other.role_name).await, "0");
}

/// Another database's owner cannot block the drop of an app role (review D-5, 2): a grant and
/// policies of database B that name the role, also in a restore point of B, leave with it; B
/// keeps working and its restore point too. `drop_database` does the same for its app roles.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_database_cannot_block_the_drop() {
    let Some(engine) = engine().await else { return };
    let f = seeded(&engine).await;
    let b = seeded(&engine).await;
    let doomed_db = seeded(&engine).await;
    let a = app_role(&f, RoleKind::AppRead, Uuid::now_v7()).await;
    let doomed = app_role(&doomed_db, RoleKind::AppWrite, Uuid::now_v7()).await;
    b.migrate(&format!(
        "GRANT SELECT ON notes TO {a}, {d}; \
         ALTER TABLE notes ENABLE ROW LEVEL SECURITY; \
         CREATE POLICY only_a ON notes FOR SELECT TO {a} USING (true); \
         CREATE POLICY a_and_public ON notes FOR SELECT TO {a}, {d}, public USING (true);",
        a = lens::ident(&a.role_name),
        d = lens::ident(&doomed.role_name),
    ))
    .await
    .expect("B names other databases' app roles");
    let point = engine.create_restore_point(b.db()).await.unwrap();
    assert_ne!(mentions(&engine, b.db(), &a.role_name).await, "0");
    engine
        .drop_app_role(&a.role_ref().unwrap())
        .await
        .expect("drop although B names the role");
    assert_eq!(role_count(&engine, &a.role_name).await, "0");
    let mut conn = dbhost(&engine, b.db()).await;
    assert_eq!(
        psql(
            &mut conn,
            "SELECT string_agg(polname, ',' ORDER BY polname) FROM pg_policy"
        )
        .await
        .unwrap(),
        "a_and_public"
    );
    close(conn).await;
    assert_eq!(
        b.run(
            RoleKind::Owner,
            StatementMode::Read,
            "SELECT count(*) FROM notes",
            vec![]
        )
        .await
        .expect("B works")
        .rows
        .len(),
        1
    );
    // The database goes with its app role, although B (and B's restore point) name it.
    engine.drop_database(doomed_db.db()).await.unwrap();
    assert_eq!(role_count(&engine, &doomed.role_name).await, "0");
    // B's restore point still restores.
    engine
        .restore_in_place(b.db(), &point, &b.key)
        .await
        .expect("restore B");
}

/// The connection limit of a role (empty when it is gone).
async fn conn_limit(engine: &PostgresEngine, name: &str) -> String {
    let mut admin = engine.admin().await.unwrap();
    psql(
        &mut admin,
        &format!("SELECT rolconnlimit FROM pg_roles WHERE rolname = '{name}'"),
    )
    .await
    .unwrap()
}

/// A grant made again between the strip and `DROP ROLE` (Kimi run 15 on D-5, 1): another
/// database's owner names the app role again right before the drop, twice; the drop strips and
/// tries again, and the role goes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_grant_made_again_before_drop_role_does_not_block_it() {
    let Some(engine) = engine().await else { return };
    let f = seeded(&engine).await;
    let b = seeded(&engine).await;
    let a = app_role(&f, RoleKind::AppRead, Uuid::now_v7()).await;
    let grant = format!("GRANT SELECT ON notes TO {}", lens::ident(&a.role_name));
    b.migrate(&grant).await.expect("B names A's app role");
    *engine.before_role_drop.lock() = Some((b.db().clone(), grant, 2));
    engine
        .drop_app_role(&a.role_ref().unwrap())
        .await
        .expect("the drop strips again after a grant made again");
    assert_eq!(engine.before_role_drop.lock().as_ref().unwrap().2, 0);
    assert_eq!(role_count(&engine, &a.role_name).await, "0");
    assert_eq!(mentions(&engine, b.db(), &a.role_name).await, "0");
}

/// A purge whose app role another database keeps naming (Kimi run 15 on D-5, 1): the first
/// `drop_database` drops the database but not the role; the role is out of the app budget
/// (connection limit 0) and the retry, which finds no database, drops it before it answers
/// `NotFound`, also in an engine of another process.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_purge_is_finished_by_its_retry() {
    let Some(engine) = engine().await else { return };
    let doomed_db = seeded(&engine).await;
    let b = seeded(&engine).await;
    let doomed = app_role(&doomed_db, RoleKind::AppWrite, Uuid::now_v7()).await;
    let kept = app_role(&doomed_db, RoleKind::AppRead, Uuid::now_v7()).await;
    assert_ne!(conn_limit(&engine, &doomed.role_name).await, "0");
    let grant = format!(
        "GRANT SELECT ON notes TO {}",
        lens::ident(&doomed.role_name)
    );
    b.migrate(&grant).await.expect("B names the app role");
    let attempts = crate::postgres::app_roles::DROP_ATTEMPTS;
    *engine.before_role_drop.lock() = Some((b.db().clone(), grant, 2 + attempts));
    // The other app role goes first or second; each of its drops also runs the script.
    // An engine of another process that resolved the database before the purge.
    let stale = engine_with(|_| {}).await.expect("a second engine");
    stale.resolve(doomed_db.db()).await.expect("resolve");
    let first = engine.drop_database(doomed_db.db()).await;
    assert!(
        matches!(first, Err(EngineError::Internal(ref m)) if m.contains("2BP01")),
        "{first:?}"
    );
    assert_eq!(role_count(&engine, &doomed.role_name).await, "1");
    assert_eq!(conn_limit(&engine, &doomed.role_name).await, "0");
    *engine.before_role_drop.lock() = None;
    // A resume of another replica that read the role before the purge lets it in again late
    // (Kimi run 15, third round): the retry still takes it.
    let mut admin = engine.admin().await.unwrap();
    ok(
        &mut admin,
        &format!("ALTER ROLE {} LOGIN", lens::ident(&doomed.role_name)),
    )
    .await;
    drop(admin);
    // The retry in another process with the state of before (it finds the database gone at
    // its first read), then in a fresh one and in this one.
    assert!(matches!(
        stale.drop_database(doomed_db.db()).await,
        Err(EngineError::NotFound)
    ));
    assert_eq!(role_count(&engine, &doomed.role_name).await, "0");
    let other = engine_with(|_| {}).await.expect("a third engine");
    assert!(matches!(
        other.drop_database(doomed_db.db()).await,
        Err(EngineError::NotFound)
    ));
    assert_eq!(role_count(&engine, &doomed.role_name).await, "0");
    assert_eq!(role_count(&engine, &kept.role_name).await, "0");
    assert_eq!(mentions(&engine, b.db(), &doomed.role_name).await, "0");
    assert!(matches!(
        engine.drop_database(doomed_db.db()).await,
        Err(EngineError::NotFound)
    ));
}

/// A role made by another server replica while a purge runs never escapes it (Kimi run 15 on
/// D-5, third round): the create holds the database's lifecycle lock, so the purge of another
/// engine waits for it and then drops the new role with the database.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_role_made_during_a_purge_goes_with_it() {
    let Some(creator) = engine().await else {
        return;
    };
    let creator = Arc::new(creator);
    let purger = Arc::new(engine_with(|_| {}).await.expect("a second engine"));
    let f = seeded(&creator).await;
    let before = app_role(&f, RoleKind::AppRead, Uuid::now_v7()).await;
    let reached = Arc::new(tokio::sync::Notify::new());
    let go = Arc::new(tokio::sync::Notify::new());
    *creator.create_pause.lock() = Some((reached.clone(), go.clone()));
    let db = f.db().clone();
    let create = {
        let (engine, db) = (creator.clone(), db.clone());
        tokio::spawn(async move {
            engine
                .create_app_role(&db, RoleKind::AppWrite, Uuid::now_v7())
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(30), reached.notified())
        .await
        .expect("the create holds the lock");
    let purge = {
        let (engine, db) = (purger.clone(), db.clone());
        tokio::spawn(async move { engine.drop_database(&db).await })
    };
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!purge.is_finished(), "the purge waits for the create");
    go.notify_one();
    let made = create.await.unwrap().expect("the create");
    purge.await.unwrap().expect("the purge");
    assert_eq!(role_count(&creator, &before.role_name).await, "0");
    assert_eq!(role_count(&creator, &made.role_name).await, "0");
}

/// A read of the app roles that fails stops the purge (Kimi run 15 on D-5, second review): no
/// drop with a role left out, unmarked and inside the budget; the next try drops both.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_read_of_the_app_roles_stops_the_purge() {
    let Some(engine) = engine().await else { return };
    let f = seeded(&engine).await;
    let role = app_role(&f, RoleKind::AppWrite, Uuid::now_v7()).await;
    engine.fail_app_names.store(true, SeqCst);
    let first = engine.drop_database(f.db()).await;
    engine.fail_app_names.store(false, SeqCst);
    assert!(matches!(first, Err(EngineError::Internal(_))), "{first:?}");
    assert_eq!(role_count(&engine, &role.role_name).await, "1");
    assert_ne!(conn_limit(&engine, &role.role_name).await, "0");
    assert_eq!(
        f.run(
            RoleKind::Owner,
            StatementMode::Read,
            "SELECT count(*) FROM notes",
            vec![]
        )
        .await
        .expect("the database stays")
        .rows
        .len(),
        1
    );
    engine.drop_database(f.db()).await.expect("the next try");
    assert_eq!(role_count(&engine, &role.role_name).await, "0");
}

/// The mark of a purge is inert while its database exists (a drop that failed leaves it): the
/// sweep of a retry takes no role of a live database, and the role works again after a restore
/// from the trash.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mark_on_a_live_database_takes_nothing() {
    let Some(engine) = engine().await else { return };
    let f = seeded(&engine).await;
    let role = app_role(&f, RoleKind::AppRead, Uuid::now_v7()).await;
    let name = f.db().engine_name.clone();
    crate::postgres::app_roles::mark_for_drop(
        &engine,
        &name,
        std::slice::from_ref(&role.role_name),
    )
    .await
    .expect("mark");
    engine.suspend_app_access(f.db()).await.unwrap();
    crate::postgres::app_roles::drop_marked(&engine, &name)
        .await
        .expect("the sweep");
    assert_eq!(role_count(&engine, &role.role_name).await, "1");
    assert_ne!(conn_limit(&engine, &role.role_name).await, "0");
    engine.resume_app_access(f.db()).await.unwrap();
    let mut s = login(
        &engine,
        &creds(&engine, &role, Duration::from_secs(300)).await,
        None,
    )
    .await
    .expect("the role works");
    assert_eq!(ok(&mut s, "SELECT count(*) FROM notes").await, "2");
    close(s).await;
}

/// A database in the trash (review D-5, 3 and 4): every app role is suspended and its
/// sessions end; a role made afterwards is made `NOLOGIN` and gets no password; the restore
/// lets them all in again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_database_in_the_trash_suspends_its_apps() {
    let Some(engine) = engine().await else { return };
    let f = seeded(&engine).await;
    let early = app_role(&f, RoleKind::AppWrite, Uuid::now_v7()).await;
    let c = creds(&engine, &early, Duration::from_secs(300)).await;
    let mut s = login(&engine, &c, None).await.unwrap();
    assert_eq!(ok(&mut s, "SELECT count(*) FROM notes").await, "2");
    engine.suspend_app_access(f.db()).await.unwrap();
    assert!(ended_within(&mut s, Duration::from_secs(1)).await);
    match login(&engine, &c, None).await {
        Ok(_) => panic!("an app of a database in the trash logged in"),
        Err(e) => assert_eq!(code(&e), "28000", "{e}"),
    }
    let late = app_role(&f, RoleKind::AppRead, Uuid::now_v7()).await;
    assert!(late.suspended);
    let mut admin = engine.admin().await.unwrap();
    assert_eq!(
        psql(
            &mut admin,
            &format!(
                "SELECT rolcanlogin::text FROM pg_roles WHERE rolname = '{}'",
                late.role_name
            )
        )
        .await
        .unwrap(),
        "false"
    );
    drop(admin);
    for role in [&early, &late] {
        assert!(matches!(
            engine
                .app_credentials(&role.role_ref().unwrap(), Duration::from_secs(60))
                .await,
            Err(EngineError::PermissionDenied(m)) if m == "app_role_suspended"
        ));
    }
    engine.resume_app_access(f.db()).await.unwrap();
    for role in [&early, &late] {
        let mut s = login(
            &engine,
            &creds(&engine, role, Duration::from_secs(300)).await,
            None,
        )
        .await
        .expect("restored");
        assert_eq!(ok(&mut s, "SELECT count(*) FROM notes").await, "2");
        close(s).await;
    }
}

/// While `ruum_dbhost` may not set `temp_file_limit`, no password is issued unless the
/// configuration accepts it (review D-5, 6; backlog D5-3).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn credentials_need_the_temp_file_limit() {
    let Some(strict) = engine_with(|c| c.app_require_temp_file_limit = true).await else {
        return;
    };
    let f = seeded(&strict).await;
    let role = app_role(&f, RoleKind::AppRead, Uuid::now_v7()).await;
    let may = || async {
        let mut admin = strict.admin().await.unwrap();
        let out = psql(
            &mut admin,
            "SELECT has_parameter_privilege('temp_file_limit', 'SET')::text",
        )
        .await
        .unwrap();
        drop(admin);
        out
    };
    let before = may().await;
    let got = strict
        .app_credentials(&role.role_ref().unwrap(), Duration::from_secs(60))
        .await;
    if may().await != before {
        // Another test granted the right meanwhile.
        return;
    }
    match (before.as_str(), got) {
        ("false", Err(EngineError::PermissionDenied(m))) => {
            assert_eq!(m, "temp_file_limit_not_enforced")
        }
        ("true", Ok(c)) => {
            let mut s = login(&strict, &c, None).await.unwrap();
            assert_eq!(ok(&mut s, "SHOW temp_file_limit").await, "1GB");
            close(s).await;
        }
        (may, other) => panic!("SET on temp_file_limit: {may}; {:?}", other.map(|_| ())),
    }
}
