//! The app roles of a binding on the data cluster (card D-5): sessions that LOG IN as the app
//! role with its short-lived password, as an app does. Each refusal is Postgres's own answer
//! (an SQLSTATE), never a timeout, and each shows its positive control in the same test first
//! (decision 0006 G).

use super::*;
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
            &CopyData::SchemaOnly,
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
        .copy_database(f.db(), &f.key, &new(), &CopyData::SchemaOnly, &f.key)
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
