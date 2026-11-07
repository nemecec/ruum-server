//! Copies of a database whose tables carry tenant code that fires on the hand-over's data
//! steps (review E-2, B-1): a `BEFORE TRUNCATE` trigger, an `ON UPDATE DO ALSO` rule and an
//! `ENABLE ALWAYS` row trigger that try to grant another database's role, an `ON UPDATE DO
//! INSTEAD NOTHING` rule and a forced row-level security policy that try to keep a secret.
//! The copy must succeed, nothing may be granted, every secret must be masked, and the tenant
//! code must be back in its old state afterwards.

use super::*;

/// The memberships of `roles` (as members), `member in role` joined with `, `.
async fn memberships(conn: &mut PgConnection, roles: &[String]) -> String {
    let list = roles
        .iter()
        .map(|r| lens::literal(r))
        .collect::<Vec<_>>()
        .join(", ");
    psql(
        conn,
        &format!(
            "select coalesce(string_agg(r.rolname || ' in ' || g.rolname, ', ' order by 1), '') \
               from pg_auth_members m \
               join pg_roles r on r.oid = m.member \
               join pg_roles g on g.oid = m.roleid \
              where r.rolname in ({list})"
        ),
    )
    .await
    .unwrap()
}

/// The state of the triggers, rules and forced row-level security of `public`.
async fn tenant_code_state(conn: &mut PgConnection) -> String {
    psql(
        conn,
        "select string_agg(x, ',' order by x) from (
           select 't:' || c.relname || '.' || t.tgname || '=' || t.tgenabled::text as x
             from pg_trigger t join pg_class c on c.oid = t.tgrelid
            where not t.tgisinternal and c.relnamespace = 'public'::regnamespace
           union all
           select 'r:' || c.relname || '.' || r.rulename || '=' || r.ev_enabled::text
             from pg_rewrite r join pg_class c on c.oid = r.ev_class
            where c.relnamespace = 'public'::regnamespace and r.rulename <> '_RETURN'
           union all
           select 'f:' || c.relname || '=' || c.relforcerowsecurity::text
             from pg_class c
            where c.relnamespace = 'public'::regnamespace and c.relkind = 'r') s",
    )
    .await
    .unwrap()
}

/// The PL/pgSQL statements of hostile tenant code: each one tries to give a role of `other` to
/// a role of `own` or to `ruum_dbhost`, and ignores its own failure. Only a session with
/// `ruum_dbhost`'s `ADMIN` on the roles of `other` can make one succeed.
fn escalation(other: &CreatedDatabase, own: &CreatedDatabase) -> String {
    let grant = |role: RoleKind, to: String| {
        format!(
            "BEGIN EXECUTE 'GRANT {} TO {}'; EXCEPTION WHEN OTHERS THEN NULL; END;",
            lens::ident(&role_name(other, role)),
            lens::ident(&to)
        )
    };
    [
        grant(RoleKind::Write, role_name(own, RoleKind::Owner)),
        grant(RoleKind::Owner, role_name(own, RoleKind::Read)),
        grant(RoleKind::SecretRead, "ruum_dbhost".to_string()),
    ]
    .join("\n")
}

/// A source database armed with hostile tenant code against the roles of `other`.
async fn hostile_source<'e>(engine: &'e PostgresEngine, other: &CreatedDatabase) -> Fixture<'e> {
    let f = Fixture::new(engine).await;
    f.migrate(
        "CREATE TABLE h (id integer PRIMARY KEY, secret text, personal text, n integer);
         CREATE TABLE kept (id integer PRIMARY KEY, secret text);
         CREATE TABLE locked (id integer PRIMARY KEY, secret text);",
    )
    .await
    .expect("tables");
    for insert in [
        "INSERT INTO h VALUES (1, 'tok_h', 'alice@example.com', 1), (2, NULL, NULL, 2)",
        "INSERT INTO kept VALUES (1, 'tok_kept')",
        "INSERT INTO locked VALUES (1, 'tok_locked')",
    ] {
        f.run(RoleKind::Owner, StatementMode::Write, insert, vec![])
            .await
            .expect("rows");
    }
    let up = format!(
        "CREATE FUNCTION public.evil() RETURNS integer LANGUAGE plpgsql AS $f$
         BEGIN
           {grants}
           RETURN 1;
         END $f$;
         CREATE FUNCTION public.evil_trigger() RETURNS trigger LANGUAGE plpgsql AS $f$
         BEGIN PERFORM public.evil(); RETURN NULL; END $f$;
         CREATE TRIGGER h_truncate BEFORE TRUNCATE ON h
           FOR EACH STATEMENT EXECUTE FUNCTION public.evil_trigger();
         CREATE TRIGGER h_update BEFORE UPDATE ON h
           FOR EACH ROW EXECUTE FUNCTION public.evil_trigger();
         ALTER TABLE h ENABLE ALWAYS TRIGGER h_update;
         CREATE TRIGGER h_off BEFORE INSERT ON h
           FOR EACH ROW EXECUTE FUNCTION public.evil_trigger();
         ALTER TABLE h DISABLE TRIGGER h_off;
         CREATE RULE h_rule AS ON UPDATE TO h DO ALSO SELECT public.evil();
         CREATE RULE h_insert AS ON INSERT TO h DO ALSO SELECT public.evil();
         ALTER TABLE h DISABLE RULE h_insert;
         CREATE RULE kept_rule AS ON UPDATE TO kept DO INSTEAD NOTHING;
         ALTER TABLE locked ENABLE ROW LEVEL SECURITY;
         ALTER TABLE locked FORCE ROW LEVEL SECURITY;
         CREATE POLICY locked_none ON locked USING (public.evil() < 0);",
        grants = escalation(other, &f.created),
    );
    f.migrate(&up).await.expect("hostile tenant code");
    f
}

fn plan() -> MaskPlan {
    let col = |t: &str, c: &str| ColumnRef {
        schema: "public".into(),
        table: t.into(),
        column: c.into(),
    };
    MaskPlan {
        columns: vec![
            (col("h", "secret"), MaskAs::Null),
            (col("h", "personal"), MaskAs::Pseudonym),
            (col("kept", "secret"), MaskAs::Null),
            (col("locked", "secret"), MaskAs::Null),
        ],
    }
}

/// A full, a schema-only and a masked copy of a database with hostile triggers and rules: no
/// tenant code runs with the rights of `ruum_dbhost`, no role gains a membership, the secrets
/// are masked, and the triggers, rules and forced row-level security are as they were.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_copy_runs_no_tenant_trigger_or_rule_with_platform_rights() {
    let Some(engine) = engine().await else { return };
    let other = Fixture::new(&engine).await;
    let f = hostile_source(&engine, &other.created).await;
    let mut source_host = dbhost(&engine, f.db()).await;
    let source_state = tenant_code_state(&mut source_host).await;
    close(source_host).await;
    assert_eq!(
        source_state,
        "f:h=false,f:kept=false,f:locked=true,r:h.h_insert=D,r:h.h_rule=O,r:kept.kept_rule=O,\
         t:h.h_off=D,t:h.h_truncate=O,t:h.h_update=A"
    );
    let logins = |c: &CreatedDatabase| -> Vec<String> {
        c.roles
            .iter()
            .filter(|r| r.kind != RoleKind::Mask)
            .map(|r| r.role_name.clone())
            .collect()
    };
    for data in [
        CopyData::Full,
        CopyData::SchemaOnly,
        CopyData::Masked(plan()),
    ] {
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
        let mut host = dbhost(&engine, &copy.database).await;
        let mut roles = logins(&f.created);
        roles.extend(logins(&other.created));
        roles.extend(logins(&copy));
        assert_eq!(
            memberships(&mut host, &roles).await,
            "",
            "{data:?}: no LOGIN role became a member of anything"
        );
        // ruum_dbhost holds SET in every `mask` by design; in no LOGIN kind.
        let list = logins(&other.created)
            .iter()
            .map(|r| lens::literal(r))
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(
            psql(
                &mut host,
                &format!(
                    "select count(*) from pg_auth_members m \
                      where m.member = 'ruum_dbhost'::regrole \
                        and m.roleid in (select oid from pg_roles where rolname in ({list})) \
                        and (m.inherit_option or m.set_option)"
                ),
            )
            .await
            .unwrap(),
            "0",
            "{data:?}: ruum_dbhost gained no membership in the other database's LOGIN roles"
        );
        assert_eq!(
            tenant_code_state(&mut host).await,
            source_state,
            "{data:?}: the triggers, rules and forced row-level security are as they were"
        );
        let owner = role_name(&copy, RoleKind::Owner);
        assert_eq!(
            psql(
                &mut host,
                &format!(
                    "select has_schema_privilege({o}, 'ruum_mask', 'usage')::text || '|' || \
                            has_function_privilege({o}, 'ruum_mask.pseudonym(text)', 'execute')::text",
                    o = lens::literal(&owner)
                ),
            )
            .await
            .unwrap(),
            "false|false",
            "{data:?}: the copy's owner holds no right on ruum_mask after the hand-over"
        );
        close(host).await;
        // The data, read by the copy's owner; `locked` forces row-level security on its owner,
        // so the owner lifts that first.
        let mut owner_session = session(&engine, &copy.database, RoleKind::Owner).await;
        let rows = psql(
            &mut owner_session,
            "alter table locked no force row level security;
             select (select string_agg(coalesce(secret, '-') || ':' || \
                            coalesce(left(personal, 2), '-'), ',' order by id) from h) || '/' || \
                    (select string_agg(coalesce(secret, '-'), ',' order by id) from kept) || '/' || \
                    (select string_agg(coalesce(secret, '-'), ',' order by id) from locked)",
        )
        .await
        .unwrap();
        let expected = match data {
            CopyData::Full => "tok_h:al,-:-/tok_kept/tok_locked",
            CopyData::SchemaOnly => "",
            CopyData::Masked(_) => "-:p_,-:-/-/-",
        };
        assert_eq!(rows, expected, "{data:?}: the data of the copy");
        close(owner_session).await;
        // The copy works for its roles.
        f.run_on(
            &copy.database,
            RoleKind::Owner,
            StatementMode::Read,
            "SELECT count(*) FROM kept",
            vec![],
        )
        .await
        .unwrap();
    }
}

/// A masked copy builds a foreign key on masked columns again (both sides hold the pseudonym of
/// the same value) and leaves out one that cannot be built again (one side masked, the other
/// not), without failing the copy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_masked_copy_builds_foreign_keys_again_or_leaves_them_out() {
    let Some(engine) = engine().await else { return };
    let f = Fixture::new(&engine).await;
    f.migrate(
        "CREATE TABLE users (id integer PRIMARY KEY, email text UNIQUE);
         CREATE TABLE orders (id integer PRIMARY KEY,
           user_email text CONSTRAINT orders_user_fk REFERENCES users (email),
           note_email text CONSTRAINT orders_note_fk REFERENCES users (email));",
    )
    .await
    .unwrap();
    for insert in [
        "INSERT INTO users VALUES (1, 'alice@example.com'), (2, 'bob@example.com')",
        "INSERT INTO orders VALUES (1, 'alice@example.com', 'bob@example.com')",
    ] {
        f.run(RoleKind::Owner, StatementMode::Write, insert, vec![])
            .await
            .unwrap();
    }
    let col = |t: &str, c: &str| ColumnRef {
        schema: "public".into(),
        table: t.into(),
        column: c.into(),
    };
    let plan = MaskPlan {
        columns: vec![
            (col("users", "email"), MaskAs::Pseudonym),
            (col("orders", "user_email"), MaskAs::Pseudonym),
        ],
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
        .expect("the masked copy succeeds");
    let mut host = dbhost(&engine, &copy.database).await;
    assert_eq!(
        psql(
            &mut host,
            "select string_agg(conname, ',' order by conname) from pg_constraint \
              where contype = 'f' and conrelid = 'public.orders'::regclass",
        )
        .await
        .unwrap(),
        "orders_user_fk",
        "the key on two masked columns is built again; the other one is left out"
    );
    close(host).await;
    let mut owner_session = session(&engine, &copy.database, RoleKind::Owner).await;
    assert_eq!(
        psql(
            &mut owner_session,
            "select (o.user_email = u.email)::text || '|' || left(o.user_email, 2) || '|' || \
                    o.note_email \
               from orders o join users u on u.id = 1",
        )
        .await
        .unwrap(),
        "true|p_|bob@example.com"
    );
    close(owner_session).await;
}

/// The memberships that a hostile tenant could have taken: those of the LOGIN roles of
/// `databases`, and those of `ruum_dbhost` in the LOGIN roles of `other` (it holds `SET` in
/// every `mask` by design). Empty when nothing was taken.
async fn taken(
    engine: &PostgresEngine,
    databases: &[&CreatedDatabase],
    other: &CreatedDatabase,
) -> String {
    let logins = |c: &CreatedDatabase| -> Vec<String> {
        c.roles
            .iter()
            .filter(|r| r.kind != RoleKind::Mask)
            .map(|r| r.role_name.clone())
            .collect()
    };
    let roles: Vec<String> = databases.iter().flat_map(|c| logins(c)).collect();
    let mut host = dbhost(engine, &other.database).await;
    let mut out = memberships(&mut host, &roles).await;
    let list = logins(other)
        .iter()
        .map(|r| lens::literal(r))
        .collect::<Vec<_>>()
        .join(", ");
    let dbhost_in = psql(
        &mut host,
        &format!(
            "select coalesce(string_agg('ruum_dbhost in ' || g.rolname, ', ' order by 1), '') \
               from pg_auth_members m join pg_roles g on g.oid = m.roleid \
              where m.member = 'ruum_dbhost'::regrole and g.rolname in ({list}) \
                and (m.inherit_option or m.set_option)"
        ),
    )
    .await
    .unwrap();
    close(host).await;
    if !dbhost_in.is_empty() {
        if !out.is_empty() {
            out.push_str(", ");
        }
        out.push_str(&dbhost_in);
    }
    out
}

/// A tenant puts operators into `public` that a catalog query of the platform would pick when
/// `public` is on its `search_path` (`oid = integer`, `name !~ name`; the CVE-2018-1058 kind),
/// and tries to grant other roles from them. Steps 1 and 3 of every hand-over (review E-2,
/// re-review), the lens rebuild after a migration, `sync_lens` and a restore run on
/// `ruum_dbhost` connections; none of them may resolve a name in a tenant schema, so the
/// operators never run with `ruum_dbhost`'s rights and nothing is granted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn platform_steps_run_no_tenant_operator() {
    let Some(engine) = engine().await else { return };
    let other = Fixture::new(&engine).await;
    let f = Fixture::new(&engine).await;
    f.migrate("CREATE TABLE t (id integer PRIMARY KEY, secret text, personal text)")
        .await
        .expect("table");
    f.run(
        RoleKind::Owner,
        StatementMode::Write,
        "INSERT INTO t VALUES (1, 'tok_t', 'alice@example.com')",
        vec![],
    )
    .await
    .expect("row");
    let grants = escalation(&other.created, &f.created);
    let up = format!(
        "CREATE FUNCTION public.evil_oid_eq(oid, integer) RETURNS boolean LANGUAGE plpgsql AS $f$
         BEGIN
           PERFORM pg_catalog.set_config('ruum_test.hit', 'yes', false);
           {grants}
           RETURN $1 OPERATOR(pg_catalog.=) $2::pg_catalog.oid;
         END $f$;
         CREATE OPERATOR public.= (LEFTARG = oid, RIGHTARG = integer,
           FUNCTION = public.evil_oid_eq);
         CREATE FUNCTION public.evil_name_nomatch(name, name) RETURNS boolean LANGUAGE plpgsql AS $f$
         BEGIN
           PERFORM pg_catalog.set_config('ruum_test.hit', 'yes', false);
           {grants}
           RETURN $1::pg_catalog.text OPERATOR(pg_catalog.!~) $2::pg_catalog.text;
         END $f$;
         CREATE OPERATOR public.!~ (LEFTARG = name, RIGHTARG = name,
           FUNCTION = public.evil_name_nomatch);"
    );
    f.migrate(&up).await.expect("hostile operators");
    // The control: where `public` is on the search_path (here the owner's), the queries of the
    // platform pick the tenant's operators.
    let mut owner = session(&engine, f.db(), RoleKind::Owner).await;
    assert_eq!(
        psql(
            &mut owner,
            "select pg_catalog.set_config('ruum_test.hit', 'no', false);
             select count(*) from pg_catalog.pg_namespace n where n.oid = 0;
             select pg_catalog.set_config('ruum_test.eq', pg_catalog.current_setting('ruum_test.hit'), false),
                    pg_catalog.set_config('ruum_test.hit', 'no', false);
             select count(*) from pg_catalog.pg_namespace n where n.nspname !~ '^pg_';
             select pg_catalog.current_setting('ruum_test.eq') || '|' ||
                    pg_catalog.current_setting('ruum_test.hit')",
        )
        .await
        .unwrap(),
        "yes|yes",
        "the tenant's operators are picked where public is on the search_path"
    );
    close(owner).await;
    assert_eq!(
        taken(&engine, &[&f.created, &other.created], &other.created).await,
        "",
        "the lens rebuild after the migration ran no tenant operator as ruum_dbhost"
    );
    let sync = ClassSync {
        personal: vec!["personal"],
        secret: vec!["secret"],
    };
    engine
        .sync_lens(f.db(), &f.key, &sync)
        .await
        .expect("sync_lens");
    assert_eq!(
        taken(&engine, &[&f.created, &other.created], &other.created).await,
        "",
        "sync_lens ran no tenant operator as ruum_dbhost"
    );
    let col = |c: &str| ColumnRef {
        schema: "public".into(),
        table: "t".into(),
        column: c.into(),
    };
    let masked = MaskPlan {
        columns: vec![
            (col("secret"), MaskAs::Null),
            (col("personal"), MaskAs::Pseudonym),
        ],
    };
    for data in [
        CopyData::Full,
        CopyData::SchemaOnly,
        CopyData::Masked(masked),
    ] {
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
            "{data:?}: the hand-over ran no tenant operator as ruum_dbhost"
        );
        f.run_on(
            &copy.database,
            RoleKind::Owner,
            StatementMode::Read,
            "SELECT count(*) FROM t",
            vec![],
        )
        .await
        .unwrap_or_else(|e| panic!("{data:?}: the copy works: {e:?}"));
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
        "the restore ran no tenant operator as ruum_dbhost"
    );
}

/// Every `ruum_dbhost` session, to a database and to the admin database, resolves names in
/// `pg_catalog` (and its own `pg_temp`) only, whatever the database or the role defaults say.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dbhost_sessions_search_pg_catalog_only() {
    let Some(engine) = engine().await else { return };
    let f = Fixture::new(&engine).await;
    let mut host = dbhost(&engine, f.db()).await;
    assert_eq!(
        psql(&mut host, "show search_path").await.unwrap(),
        "pg_catalog, pg_temp",
        "a ruum_dbhost session to a database"
    );
    close(host).await;
    let mut admin = engine.admin().await.expect("admin");
    assert_eq!(
        psql(&mut admin, "show search_path").await.unwrap(),
        "pg_catalog, pg_temp",
        "a ruum_dbhost session to the admin database"
    );
}
