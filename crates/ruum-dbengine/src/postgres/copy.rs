//! Copies with `CREATE DATABASE ... TEMPLATE`, the drain of a source, restore points and drop.
//!
//! **The drain** (decision 0004-27). PostgreSQL refuses a `TEMPLATE` copy while any session is
//! connected to the source. The engine stops handing out sessions (it takes the database's gate
//! exclusively, so new requests wait up to `request_wait`, then get `database_not_ready`),
//! closes the idle pooled connections of every kind, waits up to `drain_wait` for running
//! statements, then cancels them and ends the remaining sessions, sets `ALLOW_CONNECTIONS
//! false`, takes the copy, and opens the database again. `ruum_dbhost` may not signal the
//! sessions of the per-database roles (it is a member of none of them), so the engine cancels a
//! session from a new session of the same role (a role may signal its own backends).
//!
//! **The hand-over of a copy.** A `TEMPLATE` copy holds the source's objects with the
//! source's owners and grants. Before the copy is handed out, `ruum_dbhost` drops the objects of
//! the source's `mask` (as that `mask`), reassigns the objects of the source's `owner` to the
//! copy's `owner` (`REASSIGN OWNED`, which needs `ruum_dbhost` to hold `INHERIT` membership in
//! the old owner and `INHERIT` and `SET` in the new one; both are revoked right after that
//! step, also on failure), revokes every right of the source's roles in the copy, and builds the
//! copy's grants and functions. At rest `ruum_dbhost` holds membership in no LOGIN kind.
//!
//! A schema-only or masked copy changes tenant tables, and that can run tenant code: triggers,
//! rules, checks, policies. That step never runs on a `ruum_dbhost` connection (review E-2,
//! B-1): the copy's own `owner` runs it in a session of its own, after it disables the enabled
//! triggers and rules of its tables and lifts forced row-level security, and it puts each one
//! back in its old state afterwards. So tenant code runs at most with the rights of the copy's
//! `owner`, which a migration of the copy has anyway. Then `ruum_dbhost` installs the copy's key
//! and builds the lens views.
//!
//! **Restore points.** Point-in-time restore is card E-4. A restore point here is
//! `pg_create_restore_point` (or the current WAL position when `ruum_dbhost` may not call it)
//! plus a snapshot database: a `TEMPLATE` copy with `ALLOW_CONNECTIONS false`, no grant and
//! `ruum_meta.ident.state = 'snapshot'`, so no reference resolves to it. A restore copies the
//! snapshot back (into a new database, or in place by swapping names). Snapshots go with their
//! database.

use super::errors::{internal, sqlstate};
use super::lens::{self, ident};
use super::setup::{self, run};
use super::{DbState, Meta, PostgresEngine, neutral_name};
use crate::error::EngineError;
use crate::key::PseudonymKey;
use crate::model::*;
use crate::role::RoleKind;
use sqlx::{Connection, PgConnection, Row};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use time::OffsetDateTime;
use tokio::sync::RwLockWriteGuard;

/// Cancels (or ends) every session of the database's roles, from a session of each role.
async fn signal_sessions(
    engine: &PostgresEngine,
    state: &DbState,
    terminate: bool,
) -> Result<usize, EngineError> {
    let rows = {
        let mut admin = engine.admin().await?;
        sqlx::query(
            "select usename::text, pg_catalog.array_agg(pid) from pg_catalog.pg_stat_activity \
              where datname = $1 and pid <> pg_catalog.pg_backend_pid() group by usename",
        )
        .persistent(false)
        .bind(&state.name)
        .fetch_all(&mut *admin)
        .await
        .map_err(|e| internal("list the sessions of the database", e))?
    };
    let mut signalled = 0;
    let func = if terminate {
        "pg_catalog.pg_terminate_backend"
    } else {
        "pg_catalog.pg_cancel_backend"
    };
    for row in rows {
        let user: Option<String> = row.get(0);
        let pids: Vec<i32> = row.get(1);
        let Some(user) = user else { continue };
        let sql = format!("select {func}(p) from pg_catalog.unnest($1::int4[]) p");
        if user == "ruum_dbhost" {
            let mut admin = engine.admin().await?;
            let _ = sqlx::query(sqlx::AssertSqlSafe(sql))
                .persistent(false)
                .bind(&pids)
                .execute(&mut *admin)
                .await;
            signalled += pids.len();
            continue;
        }
        let Some(role) = state.meta.roles.iter().find(|r| r.role_name == user) else {
            continue;
        };
        if role.kind == RoleKind::Mask {
            continue;
        }
        let pool = engine.pool(state, role.kind);
        let Ok((mut conn, _)) = pool.open(engine).await else {
            continue;
        };
        let _ = sqlx::query(sqlx::AssertSqlSafe(sql))
            .persistent(false)
            .bind(&pids)
            .execute(&mut conn)
            .await;
        let _ = conn.close().await;
        signalled += pids.len();
    }
    Ok(signalled)
}

/// The number of sessions connected to a database.
async fn session_count(engine: &PostgresEngine, name: &str) -> Result<i64, EngineError> {
    let mut admin = engine.admin().await?;
    let row = sqlx::query(
        "select count(*) from pg_catalog.pg_stat_activity \
          where datname = $1 and pid <> pg_catalog.pg_backend_pid()",
    )
    .persistent(false)
    .bind(name)
    .fetch_one(&mut *admin)
    .await
    .map_err(|e| internal("count the sessions", e))?;
    Ok(row.get(0))
}

async fn set_allow_connections(
    engine: &PostgresEngine,
    name: &str,
    allow: bool,
) -> Result<(), EngineError> {
    let mut admin = engine.admin().await?;
    run(
        &mut admin,
        "allow or refuse connections",
        format!("alter database {} allow_connections {allow}", ident(name)),
    )
    .await
}

/// Drains a database (decision 0004-27). Holds its gate exclusively until the guard drops;
/// the caller opens the database again with [`resume`].
pub(crate) async fn drain<'a>(
    engine: &PostgresEngine,
    state: &'a DbState,
) -> Result<RwLockWriteGuard<'a, ()>, EngineError> {
    let waiting = state.gate.write();
    tokio::pin!(waiting);
    // New requests wait from now on (the write lock is queued). Close what is idle.
    let first = tokio::time::timeout(Duration::from_millis(1), &mut waiting).await;
    for pool in state.all_pools() {
        pool.close_all().await;
    }
    let guard = match first {
        Ok(g) => g,
        Err(_) => match tokio::time::timeout(engine.config.drain_wait, &mut waiting).await {
            Ok(g) => g,
            Err(_) => {
                // Cancel the running statements, then end the sessions that are left.
                signal_sessions(engine, state, false).await?;
                match tokio::time::timeout(Duration::from_secs(1), &mut waiting).await {
                    Ok(g) => g,
                    Err(_) => {
                        signal_sessions(engine, state, true).await?;
                        tokio::time::timeout(Duration::from_secs(30), &mut waiting)
                            .await
                            .map_err(|_| {
                                EngineError::Internal("the drain did not finish".to_string())
                            })?
                    }
                }
            }
        },
    };
    for pool in state.all_pools() {
        pool.close_all().await;
    }
    // Sessions of other engine processes, or ones that just ended: end them, then close.
    let started = Instant::now();
    loop {
        if session_count(engine, &state.name).await? == 0 {
            break;
        }
        if started.elapsed() > Duration::from_secs(10) {
            return Err(EngineError::Internal(
                "the database still has sessions after the drain".to_string(),
            ));
        }
        signal_sessions(engine, state, true).await?;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    set_allow_connections(engine, &state.name, false).await?;
    Ok(guard)
}

/// Opens a drained database again.
pub(crate) async fn resume(engine: &PostgresEngine, name: &str) -> Result<(), EngineError> {
    set_allow_connections(engine, name, true).await
}

/// Grants (or revokes) `ruum_dbhost` the memberships of a hand-over: `INHERIT` in the old
/// owner, `INHERIT` and `SET` in the new owner (`REASSIGN OWNED` gives a function, a type or
/// another object that is not a table to the new owner only with `SET` in it).
async fn membership(
    engine: &PostgresEngine,
    old_owner: &str,
    new_owner: &str,
    grant: bool,
) -> Result<(), EngineError> {
    let (old_owner, new_owner) = (ident(old_owner), ident(new_owner));
    let sql = if grant {
        format!(
            "grant {old_owner} to ruum_dbhost with inherit true, set false;\n\
             grant {new_owner} to ruum_dbhost with inherit true, set true"
        )
    } else {
        format!("revoke {old_owner}, {new_owner} from ruum_dbhost")
    };
    let mut admin = engine.admin().await?;
    run(&mut admin, "change the hand-over membership", sql).await
}

/// Revokes every right of the old roles in the copy (in the caller's transaction, with
/// `ruum_dbhost` holding the privileges of the new owner).
async fn revoke_old_rights(conn: &mut PgConnection, old: &[EngineRole]) -> Result<(), EngineError> {
    let names: Vec<String> = old.iter().map(|r| r.role_name.clone()).collect();
    let statements = sqlx::query(
        "with old as (select oid, rolname from pg_catalog.pg_roles where rolname = any($1::text[]))
         select pg_catalog.format('revoke all on table %s from %I cascade', c.oid::pg_catalog.regclass, o.rolname)
           from pg_catalog.pg_class c, pg_catalog.aclexplode(c.relacl) a, old o
          where a.grantee = o.oid and c.relkind in ('r', 'p', 'v', 'm', 'f')
         union
         select pg_catalog.format('revoke all on sequence %s from %I cascade', c.oid::pg_catalog.regclass, o.rolname)
           from pg_catalog.pg_class c, pg_catalog.aclexplode(c.relacl) a, old o
          where a.grantee = o.oid and c.relkind = 'S'
         union
         select pg_catalog.format('revoke all (%I) on table %s from %I cascade', t.attname, t.attrelid::pg_catalog.regclass, o.rolname)
           from pg_catalog.pg_attribute t, pg_catalog.aclexplode(t.attacl) a, old o
          where a.grantee = o.oid
         union
         select pg_catalog.format('revoke all on function %s from %I cascade', p.oid::pg_catalog.regprocedure, o.rolname)
           from pg_catalog.pg_proc p, pg_catalog.aclexplode(p.proacl) a, old o
          where a.grantee = o.oid and p.prokind <> 'p'
         union
         select pg_catalog.format('revoke all on procedure %s from %I cascade', p.oid::pg_catalog.regprocedure, o.rolname)
           from pg_catalog.pg_proc p, pg_catalog.aclexplode(p.proacl) a, old o
          where a.grantee = o.oid and p.prokind = 'p'
         union
         select pg_catalog.format('revoke all on schema %I from %I cascade', n.nspname, o.rolname)
           from pg_catalog.pg_namespace n, pg_catalog.aclexplode(n.nspacl) a, old o
          where a.grantee = o.oid
         union
         select pg_catalog.format('revoke all on type %s from %I cascade', t.oid::pg_catalog.regtype, o.rolname)
           from pg_catalog.pg_type t, pg_catalog.aclexplode(t.typacl) a, old o
          where a.grantee = o.oid
         union
         select pg_catalog.format('alter default privileges for role %I%s revoke all on %s from %I',
                  r.rolname,
                  case when d.defaclnamespace = 0 then '' else pg_catalog.format(' in schema %I', n.nspname) end,
                  case d.defaclobjtype when 'r' then 'tables' when 'S' then 'sequences'
                    when 'f' then 'functions' when 'T' then 'types' when 'n' then 'schemas' end,
                  case when a.grantee = 0 then 'public' else (select rolname from pg_catalog.pg_roles where oid = a.grantee) end)
           from pg_catalog.pg_default_acl d
           join pg_catalog.pg_roles r on r.oid = d.defaclrole
           left join pg_catalog.pg_namespace n on n.oid = d.defaclnamespace,
                pg_catalog.aclexplode(d.defaclacl) a
          where r.rolname = any($1::text[]) or a.grantee in (select oid from old)",
    )
    .persistent(false)
    .bind(&names)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| internal("list the rights of the source's roles", e))?;
    for row in statements {
        let sql: String = row.get(0);
        run(conn, "revoke a right of the source's roles", sql).await?;
    }
    Ok(())
}

/// Whether anything in the current database still depends on one of `roles`.
async fn leftover_dependencies(
    conn: &mut PgConnection,
    roles: &[EngineRole],
) -> Result<i64, EngineError> {
    let names: Vec<String> = roles.iter().map(|r| r.role_name.clone()).collect();
    let row = sqlx::query(
        "select count(*) from pg_catalog.pg_shdepend d
          where d.dbid = (select oid from pg_catalog.pg_database where datname = pg_catalog.current_database())
            and d.refclassid = 'pg_catalog.pg_authid'::pg_catalog.regclass
            and d.refobjid in (select oid from pg_catalog.pg_roles where rolname = any($1::text[]))",
    )
    .persistent(false)
    .bind(&names)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| internal("check the hand-over", e))?;
    Ok(row.get(0))
}

/// Drops the objects of the old `mask` in the copy (the key, the functions, the lens views).
async fn drop_old_mask_objects(conn: &mut PgConnection, old_mask: &str) -> Result<(), EngineError> {
    run(
        conn,
        "become the old mask",
        format!("set local role {}", ident(old_mask)),
    )
    .await?;
    lens::drop_views(conn).await?;
    run(
        conn,
        "drop the old key and functions",
        "drop function if exists ruum_mask.pseudonym_other(anyelement); \
         drop table if exists ruum_mask.key cascade"
            .to_string(),
    )
    .await?;
    let rows = sqlx::query(
        "select pg_catalog.format('drop function if exists %s cascade', p.oid::pg_catalog.regprocedure)
           from pg_catalog.pg_proc p where p.pronamespace = 'ruum_mask'::pg_catalog.regnamespace",
    )
    .persistent(false)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| internal("list the old functions", e))?;
    for row in rows {
        let sql: String = row.get(0);
        run(conn, "drop an old function", sql).await?;
    }
    run(conn, "leave the old mask", "reset role".to_string()).await
}

/// What a hand-over does with the data.
enum Data<'a> {
    Keep,
    SchemaOnly,
    Masked(&'a MaskPlan, &'a PseudonymKey),
}

/// Hands a fresh copy (made from a template whose objects belong to `old`) over to `new_roles`,
/// masks or empties it, installs `key`, rebuilds the lens and opens it to its roles.
///
/// Three steps (review E-2, B-1): `ruum_dbhost` takes the objects over; then the copy's own
/// `owner` empties or masks the tables in a session of its own (the only step that runs tenant
/// code, such as checks, policies and index expressions); then `ruum_dbhost` installs the key and
/// builds the lens. No tenant code runs on a `ruum_dbhost` connection, so none runs with its
/// `CREATEROLE` and its `ADMIN` on the roles of other databases.
///
/// Steps 1 and 3 run no tenant code (re-review of E-2): they change ownership, rights and
/// `ruum_*` objects, and create views, which fires no trigger, rule or policy (event triggers
/// need a superuser); they write only tables of `ruum_*`, which no tenant role owns; and their
/// queries resolve names in `pg_catalog` only, since every `ruum_dbhost` session has the
/// `search_path` [`super::DBHOST_SEARCH_PATH`]. Before that fix, the `= 0` of
/// `revoke_old_rights` (step 1) and the `!~` of the catalog (step 3) picked a tenant's
/// operators in `public`. Step 1 also holds `INHERIT` in the old and the new owner, which
/// `REASSIGN OWNED` needs; the rights of the owners reach no other database.
async fn hand_over(
    engine: &PostgresEngine,
    name: &str,
    old: &[EngineRole],
    new: &NewDatabase,
    new_roles: &[EngineRole],
    data: Data<'_>,
    key: &PseudonymKey,
) -> Result<(), EngineError> {
    let role = |roles: &[EngineRole], kind: RoleKind| {
        roles
            .iter()
            .find(|r| r.kind == kind)
            .map(|r| r.role_name.clone())
            .expect("a role of every kind")
    };
    let old_owner = role(old, RoleKind::Owner);
    let new_owner = role(new_roles, RoleKind::Owner);
    let old_mask = role(old, RoleKind::Mask);
    let new_mask = role(new_roles, RoleKind::Mask);
    let masked = matches!(data, Data::Masked(..));
    // A masked copy is masked with the source's key; the copy's key comes in step 3
    // (decision 0004-25).
    let first_key = match &data {
        Data::Masked(_, source_key) => *source_key,
        _ => key,
    };
    setup::create_roles(engine, new_roles).await?;
    // 1. ruum_dbhost takes the objects over. The memberships last for this step only.
    membership(engine, &old_owner, &new_owner, true).await?;
    // `ruum_dbhost` needs SET on the old mask: it has it (decision 0004-34 a).
    let mut conn = engine.dbhost(name).await?;
    let result = async {
        run(&mut conn, "begin", "begin".to_string()).await?;
        drop_old_mask_objects(&mut conn, &old_mask).await?;
        // The schema rights first: a function or a type goes to a new owner only when that
        // owner may create in its schema.
        run(&mut conn, "grant the schemas", setup::grants_sql(new_roles)).await?;
        run(
            &mut conn,
            "reassign the tables",
            format!(
                "reassign owned by {} to {}",
                ident(&old_owner),
                ident(&new_owner)
            ),
        )
        .await?;
        revoke_old_rights(&mut conn, old).await?;
        let left = leftover_dependencies(&mut conn, old).await?;
        if left > 0 {
            return Err(EngineError::Internal(format!(
                "{left} objects of the copy still depend on the source's roles"
            )));
        }
        setup::write_meta(&mut conn, new.tenant_id, new.node_id, new_roles).await?;
        let mask = ident(&new_mask);
        run(
            &mut conn,
            "grant the tables",
            format!(
                "grant select, insert, update, delete on all tables in schema public to {mask};\n\
                 grant select on all tables in schema public to {sr};\n\
                 grant usage, select on all sequences in schema public to {mask}, {w}, {mw};\n{}",
                setup::default_privileges_sql(new_roles, Some(&new_owner)),
                sr = ident(&role(new_roles, RoleKind::SecretRead)),
                w = ident(&role(new_roles, RoleKind::Write)),
                mw = ident(&role(new_roles, RoleKind::MaskedWrite)),
            ),
        )
        .await?;
        setup::install_functions(&mut conn, new_roles, first_key).await?;
        if masked {
            pseudonyms_for_owner(&mut conn, &new_mask, &new_owner, true).await?;
        }
        run(&mut conn, "commit", "commit".to_string()).await
    }
    .await;
    let _ = conn.close().await;
    let revoked = membership(engine, &old_owner, &new_owner, false).await;
    result?;
    revoked?;
    // 2. The copy's owner empties or masks its tables, in a session of its own.
    if !matches!(data, Data::Keep) {
        let owner = new_roles
            .iter()
            .find(|r| r.kind == RoleKind::Owner)
            .cloned()
            .expect("an owner role");
        setup::grant_connect(engine, name, std::slice::from_ref(&owner)).await?;
        change_data(engine, name, new, new_roles, &data).await?;
    }
    // 3. ruum_dbhost makes the objects of mask again with the copy's key and builds the lens.
    // It refuses the copy when tenant code of step 2 left an object that depends on an object
    // of the lens schemas (review-masking-1 F2).
    let mut conn = engine.dbhost(name).await?;
    let result = async {
        run(&mut conn, "begin", "begin".to_string()).await?;
        if masked {
            pseudonyms_for_owner(&mut conn, &new_mask, &new_owner, false).await?;
        }
        setup::rebuild_lens(&mut conn, new_roles, key).await?;
        // The copy took the rights of its source; the old owner's grant went in step 1.
        setup::check_volatile_revoked(&mut conn, new_roles).await?;
        run(&mut conn, "commit", "commit".to_string()).await
    }
    .await;
    let _ = conn.close().await;
    result?;
    setup::grant_connect(engine, name, new_roles).await
}

/// Lets the copy's `owner` compute pseudonyms for the length of the masking (`USAGE` on
/// `ruum_mask`, `EXECUTE` on `pseudonym` and `pseudonym_other` only), or takes that back.
async fn pseudonyms_for_owner(
    conn: &mut PgConnection,
    mask: &str,
    owner: &str,
    grant: bool,
) -> Result<(), EngineError> {
    let owner = ident(owner);
    let (verb, to) = if grant {
        ("grant", "to")
    } else {
        ("revoke", "from")
    };
    run(
        conn,
        "change the owner's right on ruum_mask",
        format!("{verb} usage on schema ruum_mask {to} {owner}"),
    )
    .await?;
    run(
        conn,
        "become mask",
        format!("set local role {}", ident(mask)),
    )
    .await?;
    let rows = sqlx::query(
        "select p.oid::pg_catalog.regprocedure::text from pg_catalog.pg_proc p
          where p.pronamespace = 'ruum_mask'::pg_catalog.regnamespace
            and p.proname in ('pseudonym', 'pseudonym_other')",
    )
    .persistent(false)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| internal("list the pseudonym functions", e))?;
    for row in rows {
        let function: String = row.get(0);
        run(
            conn,
            "change the owner's right on a pseudonym function",
            format!("{verb} execute on function {function} {to} {owner}"),
        )
        .await?;
    }
    run(conn, "leave mask", "reset role".to_string()).await
}

/// Step 2 of a hand-over, in a session of the copy's `owner` (it owns every table and holds
/// none of `ruum_dbhost`'s rights): parks the tenant's triggers, rules and forced row-level
/// security, empties or masks the tables, and puts the tenant code back as it was.
async fn change_data(
    engine: &PostgresEngine,
    name: &str,
    new: &NewDatabase,
    new_roles: &[EngineRole],
    data: &Data<'_>,
) -> Result<(), EngineError> {
    let state = DbState::new(
        name.to_string(),
        Meta {
            tenant_id: new.tenant_id,
            node_id: new.node_id,
            roles: new_roles.to_vec(),
        },
    );
    let (mut conn, _) = engine.pool(&state, RoleKind::Owner).open(engine).await?;
    let result = async {
        run(&mut conn, "begin", "begin".to_string()).await?;
        let parked = park_tenant_code(&mut conn).await?;
        match data {
            Data::Keep => {}
            Data::SchemaOnly => empty_tables(&mut conn).await?,
            Data::Masked(plan, _) => mask_columns(&mut conn, plan).await?,
        }
        for sql in parked {
            run(&mut conn, "put the tenant code back", sql).await?;
        }
        run(&mut conn, "commit", "commit".to_string()).await
    }
    .await;
    let _ = conn.close().await;
    result
}

/// Disables every enabled trigger and rule of the current user's tables and lifts forced
/// row-level security, so the owner's data change reaches every row and no `DO INSTEAD` rule or
/// trigger skips one. Returns the statements that put each one back in its old state (`ENABLE`,
/// `ENABLE REPLICA` or `ENABLE ALWAYS`): the triggers of partitioned tables before the clones
/// on their partitions, so a clone keeps its own state.
async fn park_tenant_code(conn: &mut PgConnection) -> Result<Vec<String>, EngineError> {
    let rows = sqlx::query(
        "with mine as (
           select c.oid, c.relforcerowsecurity from pg_catalog.pg_class c
            where c.relowner = (select oid from pg_catalog.pg_roles where rolname = current_user)
              and c.relkind in ('r', 'p'))
         select case when t.tgparentid = 0 then 0 else 1 end as ord,
                pg_catalog.format('alter table %s disable trigger %I',
                  t.tgrelid::pg_catalog.regclass, t.tgname) as park,
                pg_catalog.format('alter table %s %s trigger %I', t.tgrelid::pg_catalog.regclass,
                  case t.tgenabled when 'R' then 'enable replica' when 'A' then 'enable always'
                    else 'enable' end, t.tgname) as back
           from pg_catalog.pg_trigger t join mine m on m.oid = t.tgrelid
          where not t.tgisinternal and t.tgenabled <> 'D'
         union all
         select 2,
                pg_catalog.format('alter table %s disable rule %I',
                  r.ev_class::pg_catalog.regclass, r.rulename),
                pg_catalog.format('alter table %s %s rule %I', r.ev_class::pg_catalog.regclass,
                  case r.ev_enabled when 'R' then 'enable replica' when 'A' then 'enable always'
                    else 'enable' end, r.rulename)
           from pg_catalog.pg_rewrite r join mine m on m.oid = r.ev_class
          where r.ev_enabled <> 'D' and r.rulename <> '_RETURN'
         union all
         select 3,
                pg_catalog.format('alter table %s no force row level security',
                  m.oid::pg_catalog.regclass),
                pg_catalog.format('alter table %s force row level security',
                  m.oid::pg_catalog.regclass)
           from mine m where m.relforcerowsecurity
         order by 1",
    )
    .persistent(false)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| internal("list the triggers and rules", e))?;
    let mut back = Vec::with_capacity(rows.len());
    for row in rows {
        run(conn, "park the tenant code", row.get("park")).await?;
        back.push(row.get("back"));
    }
    Ok(back)
}

/// Empties every table of `public` (a `schema_only` copy).
async fn empty_tables(conn: &mut PgConnection) -> Result<(), EngineError> {
    let row = sqlx::query(
        "select pg_catalog.string_agg(c.oid::pg_catalog.regclass::text, ', ')
           from pg_catalog.pg_class c
          where c.relnamespace = 'public'::pg_catalog.regnamespace and c.relkind in ('r', 'p')",
    )
    .persistent(false)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| internal("list the tables", e))?;
    let tables: Option<String> = row.get(0);
    if let Some(tables) = tables {
        run(
            conn,
            "empty the tables",
            format!("truncate {tables} restart identity cascade"),
        )
        .await?;
    }
    let matviews = sqlx::query(
        "select pg_catalog.format('refresh materialized view %s with no data', c.oid::pg_catalog.regclass)
           from pg_catalog.pg_class c
          where c.relnamespace = 'public'::pg_catalog.regnamespace and c.relkind = 'm'",
    )
    .persistent(false)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| internal("list the materialized views", e))?;
    for row in matviews {
        run(conn, "empty a materialized view", row.get(0)).await?;
    }
    Ok(())
}

/// Masks the columns of the plan in a copy (decision 0004-25, 0004-17): a `personal` column
/// becomes `text` holding its pseudonym under the key installed now (the source's), a `secret`
/// column keeps its type and holds `NULL` for a non-empty value (its `NOT NULL` dropped).
/// Checks that read a masked column are dropped; foreign keys on masked columns are dropped and
/// built again, and one that cannot be built again (its other side is not masked the same way)
/// is left out. Runs in the `owner` session of [`change_data`], with the tenant's triggers and
/// rules parked; a secret column is checked to hold no value afterwards.
async fn mask_columns(conn: &mut PgConnection, plan: &MaskPlan) -> Result<(), EngineError> {
    if plan.columns.is_empty() {
        return Ok(());
    }
    let types = lens::pseudonym_types(conn).await?;
    let mut fks: Vec<(String, String, String)> = Vec::new();
    for (column, how) in &plan.columns {
        if column.schema != "public" {
            continue;
        }
        let table = format!("public.{}", ident(&column.table));
        let col = ident(&column.column);
        let row = sqlx::query(
            "select a.attrelid::int8, a.attnum::int2 from pg_catalog.pg_attribute a
              where a.attrelid = pg_catalog.to_regclass($1) and a.attname = $2
                and a.attnum > 0 and not a.attisdropped",
        )
        .persistent(false)
        .bind(&table)
        .bind(&column.column)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| internal("find a masked column", e))?;
        let Some(row) = row else { continue };
        let rel: i64 = row.get(0);
        let attnum: i16 = row.get(1);
        // Foreign keys from or to the column: drop now, build again after.
        for fk in sqlx::query(
            "select c.conname::text, c.conrelid::pg_catalog.regclass::text,
                    pg_catalog.pg_get_constraintdef(c.oid)
               from pg_catalog.pg_constraint c
              where c.contype = 'f'
                and ((c.conrelid = $1::int8::pg_catalog.oid and $2::int2 = any(c.conkey))
                  or (c.confrelid = $1::int8::pg_catalog.oid and $2::int2 = any(c.confkey)))",
        )
        .persistent(false)
        .bind(rel)
        .bind(attnum)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| internal("list the foreign keys", e))?
        {
            let (name, on, def): (String, String, String) = (fk.get(0), fk.get(1), fk.get(2));
            run(
                conn,
                "drop a foreign key",
                format!(
                    "alter table {on} drop constraint if exists {}",
                    ident(&name)
                ),
            )
            .await?;
            if !fks.iter().any(|(n, o, _)| *n == name && *o == on) {
                fks.push((name, on, def));
            }
        }
        // Checks that read the column.
        for chk in sqlx::query(
            "select c.conname::text from pg_catalog.pg_constraint c
              where c.contype = 'c' and c.conrelid = $1::int8::pg_catalog.oid and $2::int2 = any(c.conkey)",
        )
        .persistent(false)
        .bind(rel)
        .bind(attnum)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| internal("list the checks", e))?
        {
            let name: String = chk.get(0);
            run(
                conn,
                "drop a check",
                format!("alter table {table} drop constraint {}", ident(&name)),
            )
            .await?;
        }
        match how {
            MaskAs::Pseudonym => {
                // A personal column of a type that is not a built-in base type (decision 0005
                // B-2) holds no value in the copy: it becomes `text` and `NULL`, and no
                // conversion of the tenant's type runs inside a pseudonym function.
                let Some(base) = types.get(&(rel as u32, attnum)).copied() else {
                    run(
                        conn,
                        "mask a personal column of a tenant-defined type",
                        format!(
                            "alter table {table} alter column {col} drop default;\n\
                             alter table {table} alter column {col} drop not null;\n\
                             alter table {table} alter column {col} type text using null::text"
                        ),
                    )
                    .await?;
                    continue;
                };
                run(
                    conn,
                    "mask a personal column",
                    format!(
                        "alter table {table} alter column {col} drop default;\n\
                         alter table {table} alter column {col} type text using {}",
                        lens::pseudonym_call(&col, base)
                    ),
                )
                .await?;
            }
            MaskAs::Null => {
                run(
                    conn,
                    "mask a secret column",
                    format!(
                        "alter table {table} alter column {col} drop not null;\n\
                         update {table} set {col} = null where {col} is not null and {col}::text <> ''"
                    ),
                )
                .await?;
                let left: bool = sqlx::query(sqlx::AssertSqlSafe(format!(
                    "select exists (select 1 from {table} \
                      where {col} is not null and {col}::text <> '')"
                )))
                .persistent(false)
                .fetch_one(&mut *conn)
                .await
                .map_err(|e| internal("check a secret column", e))?
                .get(0);
                if left {
                    return Err(EngineError::Internal(format!(
                        "the secret column {}.{} still holds values after the masking",
                        column.table, column.column
                    )));
                }
            }
        }
    }
    for (name, on, def) in fks {
        run(conn, "savepoint", "savepoint ruum_fk".to_string()).await?;
        let added = run(
            conn,
            "build a foreign key again",
            format!("alter table {on} add constraint {} {def}", ident(&name)),
        )
        .await;
        let end = if added.is_ok() {
            "release savepoint ruum_fk"
        } else {
            "rollback to savepoint ruum_fk"
        };
        run(conn, "end the savepoint", end.to_string()).await?;
    }
    Ok(())
}

/// `copy_database`: a `TEMPLATE` copy of `source` after its drain.
pub(crate) async fn copy_database(
    engine: &PostgresEngine,
    source: &DatabaseRef,
    source_key: &PseudonymKey,
    new: &NewDatabase,
    data: &CopyData,
    key: &PseudonymKey,
) -> Result<CreatedDatabase, EngineError> {
    let state = engine.resolve(source).await?;
    let name = neutral_name("d_");
    let new_roles = setup::new_roles();
    {
        let _ops = state.ops.lock().await;
        let guard = drain(engine, &state).await?;
        let copied = setup::create_closed_database(engine, &name, Some(&state.name), true).await;
        let resumed = resume(engine, &state.name).await;
        drop(guard);
        copied?;
        resumed?;
    }
    let how = match data {
        CopyData::Full => Data::Keep,
        CopyData::SchemaOnly => Data::SchemaOnly,
        CopyData::Masked(plan) => Data::Masked(plan, source_key),
    };
    if let Err(e) = hand_over(engine, &name, &state.meta.roles, new, &new_roles, how, key).await {
        setup::discard(engine, &name, &new_roles).await;
        return Err(e);
    }
    finish(engine, name, new, new_roles)
}

fn finish(
    engine: &PostgresEngine,
    name: String,
    new: &NewDatabase,
    roles: Vec<EngineRole>,
) -> Result<CreatedDatabase, EngineError> {
    let meta = Meta {
        tenant_id: new.tenant_id,
        node_id: new.node_id,
        roles: roles.clone(),
    };
    engine.remember(Arc::new(DbState::new(name.clone(), meta)));
    Ok(CreatedDatabase {
        database: DatabaseRef {
            tenant_id: new.tenant_id,
            node_id: new.node_id,
            cluster: engine.config.cluster.clone(),
            engine_name: name,
        },
        roles,
    })
}

/// The restore points of a database: (at, lsn, snapshot).
async fn restore_points(
    engine: &PostgresEngine,
    name: &str,
) -> Result<Vec<(OffsetDateTime, Option<String>, String)>, EngineError> {
    let mut conn = engine.dbhost(name).await?;
    let rows = sqlx::query("select at, lsn, snapshot from ruum_meta.restore_points order by at")
        .persistent(false)
        .fetch_all(&mut conn)
        .await;
    let _ = conn.close().await;
    Ok(rows
        .map_err(|e| internal("read the restore points", e))?
        .into_iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect())
}

fn find_point(
    points: &[(OffsetDateTime, Option<String>, String)],
    at: &RestorePointMark,
) -> Result<String, EngineError> {
    points
        .iter()
        .find(|(t, lsn, _)| *t == at.at && (at.wal_lsn.is_none() || at.wal_lsn == *lsn))
        .map(|(_, _, s)| s.clone())
        .ok_or(EngineError::RestorePointNotFound)
}

/// `create_restore_point`: the WAL position (`pg_create_restore_point` when `ruum_dbhost` may
/// call it, else `pg_current_wal_lsn`) and a snapshot database.
pub(crate) async fn create_restore_point(
    engine: &PostgresEngine,
    db: &DatabaseRef,
) -> Result<RestorePointMark, EngineError> {
    let state = engine.resolve(db).await?;
    let _ops = state.ops.lock().await;
    let snapshot = neutral_name("d_");
    let last = restore_points(engine, &state.name)
        .await?
        .last()
        .map(|p| p.0);
    let (mut at, lsn) = {
        let guard = drain(engine, &state).await?;
        let marked = async {
            let mut admin = engine.admin().await?;
            let label = format!("ruum {snapshot}");
            let lsn = match sqlx::query("select pg_catalog.pg_create_restore_point($1)::text")
                .persistent(false)
                .bind(&label)
                .fetch_one(&mut *admin)
                .await
            {
                Ok(row) => row.get::<String, _>(0),
                Err(e) if sqlstate(&e).as_deref() == Some("42501") => {
                    sqlx::query("select pg_catalog.pg_current_wal_lsn()::text")
                        .persistent(false)
                        .fetch_one(&mut *admin)
                        .await
                        .map_err(|e| internal("read the WAL position", e))?
                        .get::<String, _>(0)
                }
                Err(e) => return Err(internal("create the restore point", e)),
            };
            let at: OffsetDateTime = sqlx::query("select pg_catalog.clock_timestamp()")
                .persistent(false)
                .fetch_one(&mut *admin)
                .await
                .map_err(|e| internal("read the time", e))?
                .get(0);
            drop(admin);
            setup::create_closed_database(engine, &snapshot, Some(&state.name), false).await?;
            Ok::<_, EngineError>((at, lsn))
        }
        .await;
        let resumed = resume(engine, &state.name).await;
        drop(guard);
        let marked = marked?;
        resumed?;
        marked
    };
    if let Some(last) = last
        && at <= last
    {
        at = last + time::Duration::microseconds(1);
    }
    // Mark the snapshot, so no reference ever resolves to it, and close it.
    let marked = async {
        set_allow_connections(engine, &snapshot, true).await?;
        let mut conn = engine.dbhost(&snapshot).await?;
        let r = run(
            &mut conn,
            "mark the snapshot",
            "update ruum_meta.ident set state = 'snapshot'; delete from ruum_meta.restore_points"
                .to_string(),
        )
        .await;
        let _ = conn.close().await;
        r?;
        set_allow_connections(engine, &snapshot, false).await
    }
    .await;
    if let Err(e) = marked {
        drop_snapshot(engine, &snapshot).await;
        return Err(e);
    }
    let mut conn = engine.dbhost(&state.name).await?;
    let recorded =
        sqlx::query("insert into ruum_meta.restore_points (at, lsn, snapshot) values ($1, $2, $3)")
            .persistent(false)
            .bind(at)
            .bind(&lsn)
            .bind(&snapshot)
            .execute(&mut conn)
            .await;
    let _ = conn.close().await;
    if let Err(e) = recorded {
        drop_snapshot(engine, &snapshot).await;
        return Err(internal("record the restore point", e));
    }
    Ok(RestorePointMark {
        at,
        wal_lsn: Some(lsn),
    })
}

async fn drop_snapshot(engine: &PostgresEngine, name: &str) {
    if let Ok(mut admin) = engine.admin().await {
        let _ = run(
            &mut admin,
            "drop a snapshot",
            format!("drop database if exists {}", ident(name)),
        )
        .await;
    }
}

/// `restore_into_new`: a copy of the snapshot of the restore point, handed over to new roles.
pub(crate) async fn restore_into_new(
    engine: &PostgresEngine,
    source: &DatabaseRef,
    at: &RestorePointMark,
    new: &NewDatabase,
    key: &PseudonymKey,
) -> Result<CreatedDatabase, EngineError> {
    let state = engine.resolve(source).await?;
    let snapshot = {
        let _gate = engine.enter(&state).await?;
        find_point(&restore_points(engine, &state.name).await?, at)?
    };
    let name = neutral_name("d_");
    let new_roles = setup::new_roles();
    setup::create_closed_database(engine, &name, Some(&snapshot), true).await?;
    let made = async {
        // The snapshot is marked; make the copy live before the hand-over.
        let mut conn = engine.dbhost(&name).await?;
        let r = run(
            &mut conn,
            "make the copy live",
            "update ruum_meta.ident set state = 'live'".to_string(),
        )
        .await;
        let _ = conn.close().await;
        r?;
        hand_over(
            engine,
            &name,
            &state.meta.roles,
            new,
            &new_roles,
            Data::Keep,
            key,
        )
        .await
    }
    .await;
    if let Err(e) = made {
        setup::discard(engine, &name, &new_roles).await;
        return Err(e);
    }
    finish(engine, name, new, new_roles)
}

/// `restore_in_place`: the database becomes a copy of the snapshot of the restore point. The
/// new copy is made first; then the names are swapped and the old database dropped, so a failure
/// leaves the database as it was. The roles stay; the restore points stay; the objects of `mask`
/// are made again with `key`, and the lens is built again (`mask` holds no `CREATE` afterwards).
pub(crate) async fn restore_in_place(
    engine: &PostgresEngine,
    db: &DatabaseRef,
    at: &RestorePointMark,
    key: &PseudonymKey,
) -> Result<(), EngineError> {
    let state = engine.resolve(db).await?;
    let _ops = state.ops.lock().await;
    let points = restore_points(engine, &state.name).await?;
    let snapshot = find_point(&points, at)?;
    let fresh = neutral_name("d_");
    setup::create_closed_database(engine, &fresh, Some(&snapshot), true).await?;
    let prepared = async {
        let mut conn = engine.dbhost(&fresh).await?;
        let r = async {
            run(&mut conn, "begin", "begin".to_string()).await?;
            run(
                &mut conn,
                "make the copy live",
                "update ruum_meta.ident set state = 'live'; delete from ruum_meta.restore_points"
                    .to_string(),
            )
            .await?;
            for (t, lsn, snap) in &points {
                sqlx::query(
                    "insert into ruum_meta.restore_points (at, lsn, snapshot) values ($1, $2, $3)",
                )
                .persistent(false)
                .bind(t)
                .bind(lsn)
                .bind(snap)
                .execute(&mut conn)
                .await
                .map_err(|e| internal("keep the restore points", e))?;
            }
            // The snapshot holds the objects of mask as they were then (also from before a
            // fix, review-masking-2 F-1): make them again, with the key, and build the lens.
            setup::rebuild_lens(&mut conn, &state.meta.roles, key).await?;
            run(&mut conn, "commit", "commit".to_string()).await
        }
        .await;
        let _ = conn.close().await;
        r?;
        set_allow_connections(engine, &fresh, false).await
    }
    .await;
    if let Err(e) = prepared {
        drop_snapshot(engine, &fresh).await;
        return Err(e);
    }
    let guard = drain(engine, &state).await?;
    let old = neutral_name("d_");
    let swapped = async {
        let mut admin = engine.admin().await?;
        run(
            &mut admin,
            "swap the databases",
            format!(
                "alter database {cur} rename to {old};\n\
                 alter database {fresh} rename to {cur};",
                cur = ident(&state.name),
                old = ident(&old),
                fresh = ident(&fresh),
            ),
        )
        .await
    }
    .await;
    if let Err(e) = swapped {
        let _ = resume(engine, &state.name).await;
        drop(guard);
        drop_snapshot(engine, &fresh).await;
        return Err(e);
    }
    let opened = async {
        setup::grant_connect(engine, &state.name, &state.meta.roles).await?;
        resume(engine, &state.name).await
    }
    .await;
    drop(guard);
    drop_snapshot(engine, &old).await;
    opened
}

/// `drop_database`: the drain, the snapshots, the database, its roles.
pub(crate) async fn drop_database(
    engine: &PostgresEngine,
    db: &DatabaseRef,
) -> Result<(), EngineError> {
    let state = engine.resolve(db).await?;
    let _ops = state.ops.lock().await;
    if state.dropped.load(Ordering::SeqCst) {
        return Err(EngineError::NotFound);
    }
    let points = restore_points(engine, &state.name).await?;
    let guard = drain(engine, &state).await?;
    state.dropped.store(true, Ordering::SeqCst);
    engine.forget(&state.name);
    let dropped = async {
        let mut admin = engine.admin().await?;
        run(
            &mut admin,
            "drop the database",
            format!("drop database {}", ident(&state.name)),
        )
        .await
    }
    .await;
    if let Err(e) = dropped {
        state.dropped.store(false, Ordering::SeqCst);
        let _ = resume(engine, &state.name).await;
        drop(guard);
        return Err(e);
    }
    drop(guard);
    for (_, _, snapshot) in points {
        drop_snapshot(engine, &snapshot).await;
    }
    setup::drop_roles(engine, &state.meta.roles).await
}
