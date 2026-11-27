//! The roles of app bindings on the data cluster (card D-5; decisions 0004-14, 0004-32 and
//! 0006-3; `docs/STEP3.md` "How an app runs" rule 2, the role half).
//!
//! **The role.** One LOGIN role per (database, kind, binding), `r_` + 16 random hex like every
//! role, made by `ruum_dbhost`: `NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION
//! NOBYPASSRLS CONNECTION LIMIT <n>`, member of no role, no role granted to it. It has `CONNECT`
//! on its own database only (`create_closed_database` revokes `CONNECT` and `TEMPORARY` from
//! `PUBLIC` on every database, the cluster set-up closes `postgres` and `template1`). In its
//! database it has `USAGE` on `public` and `extensions` and nothing in the Ruum schemas
//! (`ruum_meta`, `ruum_mask`, `ruum_masked`, `ruum_nosecret`: no pseudonym oracle, no key). Lens
//! `base`: `search_path = public, extensions`, a role setting.
//!
//! **Its grants.** `app_read`: `SELECT` on every table, view and materialized view of `public`,
//! `USAGE, SELECT` on its sequences. `app_write`: also `INSERT, UPDATE, DELETE`, and `UPDATE` on
//! sequences. Never `TRUNCATE`, `REFERENCES`, `TRIGGER`, ownership or DDL. This is the minimum
//! the engine grants: the tenant's own `owner` may give its app roles more in its own database
//! (backlog D5-11). Large objects: plain Postgres lets any role make them, but the cluster
//! set-up revokes the volatile `lo_*` functions from `PUBLIC` (backlog D5-12); should an app
//! role own one anyway, it goes to the owner when the role leaves a database. Tables of a later
//! migration get the same rights through `ALTER DEFAULT PRIVILEGES` of the `owner` kind, and the
//! engine grants again after every `apply_migration` and `sync_lens` ([`regrant_all`]). The
//! grants on tables run in an `owner` session (the owner owns the tables); the schema grants run
//! as `ruum_dbhost` (it owns the schemas and the database).
//!
//! **Limits.** Role settings `statement_timeout` and `idle_in_transaction_session_timeout` (the
//! app may lower or raise them in its own session; they are defaults), `temp_file_limit` (a
//! superuser setting: the app cannot raise it; set when `ruum_dbhost` holds `SET` on it, else a
//! warning, backlog D5-3, and [`credentials`] refuses unless
//! [`super::PostgresConfig::app_require_temp_file_limit`] is off), and a connection limit per
//! role. The sum of the connection limits
//! of every app role of the cluster stays under a budget (half of `max_connections` by default,
//! [`super::PostgresConfig::app_connection_budget`]), checked under an advisory lock when a role
//! is made: app roles are refused first, the platform's roles keep the rest.
//!
//! **Passwords.** [`credentials`] sets a new random password (32 bytes of the OS RNG, as hex)
//! with `VALID UNTIL`. The engine sends a SCRAM-SHA-256 verifier, never the password, so the
//! server's statement log cannot hold it either. The password lives only in the returned
//! [`AppCredentials`] (whose `Debug` and `Display` hide it).
//!
//! **The engine's list.** The app roles are rows of `ruum_meta.roles` with a binding. The meta
//! data of a database ([`super::Meta`]) holds the seven roles of step 2 only; the app roles are
//! read fresh for each use. A role is recorded before it is made, so a crash leaves a row
//! without a role (made on the next `create_app_role`) and never a role that no list names.
//!
//! **Ending a role.** Suspend: `NOLOGIN` and its sessions end; the call fails while one still
//! runs ([`end_sessions`]). A database in the trash suspends all its app roles and keeps a flag
//! (`ruum_meta.app_access`, [`set_access`]): a role made while it is set is made `NOLOGIN`, and
//! no password is issued. Drop: `NOLOGIN`, sessions end, what it owns goes to the owner and
//! every right and default privilege it holds or owns in the database goes ([`strip_in_tx`]),
//! then in the database's restore points, then in any other database of the engine whose owner
//! named it in a grant or a policy ([`strip_elsewhere`]), `DROP ROLE`, then the row. A grant
//! made again between the strip and `DROP ROLE` (2BP01) makes the step strip and try again, up
//! to [`DROP_ATTEMPTS`] times. `drop_database` first marks its app roles with a comment that
//! names the database ([`mark_for_drop`]) and, once the database is gone, sets their connection
//! limit to 0 (out of the budget); a retried `drop_database` of the gone database drops the
//! marked roles that the first try left ([`drop_marked`]) before it answers `NotFound`. The
//! engine's locks of a database are per process; `create_app_role`, `suspend_app_access` /
//! `resume_app_access` and `drop_database` also take a lock of the whole cluster for the
//! database ([`lifecycle_lock`]), so a role made or resumed by another server replica never
//! escapes a purge.
//! `ruum_dbhost` may only signal or revoke with the privileges of the role or the owner, so it
//! takes membership in them inside the one transaction of the step and gives it back before
//! the commit (a failure rolls the grant back too).
//!
//! **Copies never carry app grants.** A `TEMPLATE` copy holds the source's ACLs. The hand-over
//! of a copy (a branch, `copyNode`, a template) and of a restore into a new node strips every
//! role that the copy depends on and that is not one of its own roles ([`foreign_roles`]),
//! before the copy is handed out (`copy.rs`). A restore point keeps the grants and policies of
//! the app roles of its time, so a restore in place gives them back exactly; when an app role
//! is dropped, it leaves the restore points of its database too ([`drop_role`]), so it never
//! blocks `DROP ROLE` and never comes back.

use super::copy::{
    leftover_dependencies, restore_points, revoke_old_rights, set_allow_connections,
};
use super::errors::{internal, sqlstate};
use super::lens::{ident, literal};
use super::setup::{self, run};
use super::{DbState, PostgresEngine, neutral_name};
use crate::error::EngineError;
use crate::model::*;
use crate::role::{RoleKind, RoleRef};
use rand::TryRng;
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection, Row};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use time::OffsetDateTime;
use uuid::Uuid;

/// The `search_path` of an app role: lens `base` (decision 0004-34 g).
pub(crate) const APP_SEARCH_PATH: &str = "public, extensions";

/// The key of the advisory lock that serializes the budget check and `CREATE ROLE`.
const BUDGET_LOCK: i64 = 0x5275_756d_4435; // "RuumD5"

/// How often the end of an app role strips and tries `DROP ROLE` again when a dependency came
/// back after the strip (2BP01: another database's owner granted the role a right again).
pub(crate) const DROP_ATTEMPTS: usize = 5;

/// The comment on an app role whose database `drop_database` drops: this text and the name of
/// the database. It is in the shared catalog, so it outlives the database and its meta data.
const DROPPED_WITH: &str = "ruum: dropped with ";

/// The text before the database name in the key of [`lifecycle_lock`].
const LIFECYCLE_KEY: &str = "ruum lifecycle ";

/// A warning once per process when `ruum_dbhost` may not set `temp_file_limit`.
static TEMP_FILE_WARNED: AtomicBool = AtomicBool::new(false);

/// The kind and binding of an app role reference, or `InvalidRole`.
fn require_app(role: &RoleRef) -> Result<(RoleKind, Uuid), EngineError> {
    match role.binding() {
        Some(b) if role.kind().is_app() => Ok((role.kind(), b)),
        _ => Err(EngineError::InvalidRole(format!(
            "{} is not an app role",
            role.kind().as_str()
        ))),
    }
}

/// One row of `ruum_meta.roles` with a binding.
#[derive(Debug, Clone)]
pub(crate) struct AppRow {
    pub kind: RoleKind,
    pub binding: Uuid,
    pub role_name: String,
}

/// The app rows of the database of `conn` (a `ruum_dbhost` connection).
pub(crate) async fn read_rows(conn: &mut PgConnection) -> Result<Vec<AppRow>, EngineError> {
    Ok(setup::read_roles(conn)
        .await?
        .into_iter()
        .filter_map(|r| match (r.kind.is_app(), r.binding) {
            (true, Some(binding)) => Some(AppRow {
                kind: r.kind,
                binding,
                role_name: r.role_name,
            }),
            _ => None,
        })
        .collect())
}

/// The names of the app roles of the database `name`. A read that fails is an error, never an
/// empty list: `drop_database` and the drain act on every app role of the list, and a role
/// left out would outlive its database with no mark (Kimi run 15 on D-5).
pub(crate) async fn names(engine: &PostgresEngine, name: &str) -> Result<Vec<String>, EngineError> {
    #[cfg(test)]
    if engine.fail_app_names.load(Ordering::SeqCst) {
        return Err(EngineError::Internal(
            "read the app roles: failed by the test".to_string(),
        ));
    }
    let mut conn = engine.dbhost(name).await?;
    let rows = read_rows(&mut conn).await;
    let _ = conn.close().await;
    Ok(rows?.into_iter().map(|r| r.role_name).collect())
}

/// `rolcanlogin` of a role, `None` when it does not exist.
async fn can_login(engine: &PostgresEngine, name: &str) -> Result<Option<bool>, EngineError> {
    let mut admin = engine.admin().await?;
    let row = sqlx::query("select rolcanlogin from pg_catalog.pg_roles where rolname = $1")
        .persistent(false)
        .bind(name)
        .fetch_optional(&mut *admin)
        .await
        .map_err(|e| internal("read an app role", e))?;
    Ok(row.map(|r| r.get(0)))
}

/// The role settings of an app role (after `RESET ALL`), with `temp_file_limit` when
/// `ruum_dbhost` may set it; and whether it may.
async fn settings_sql(
    engine: &PostgresEngine,
    conn: &mut PgConnection,
    name: &str,
) -> Result<(String, bool), EngineError> {
    let c = &engine.config;
    let role = ident(name);
    let mut sql = format!(
        "alter role {role} set search_path = {APP_SEARCH_PATH};\n\
         alter role {role} set statement_timeout = {};\n\
         alter role {role} set idle_in_transaction_session_timeout = {};\n",
        c.app_statement_timeout
            .as_millis()
            .clamp(1, i32::MAX as u128),
        c.app_idle_in_transaction_timeout
            .as_millis()
            .clamp(1, i32::MAX as u128),
    );
    let may: bool =
        sqlx::query("select pg_catalog.has_parameter_privilege('temp_file_limit', 'SET')")
            .persistent(false)
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| internal("check the right on temp_file_limit", e))?
            .get(0);
    if may {
        sql.push_str(&format!(
            "alter role {role} set temp_file_limit = {};\n",
            c.app_temp_file_limit_kb
        ));
    } else if !TEMP_FILE_WARNED.swap(true, Ordering::SeqCst) {
        tracing::warn!(
            "ruum_dbhost may not set temp_file_limit: app roles get no temp file limit until the \
             data cluster set-up grants SET on it (backlog D5-3)"
        );
    }
    Ok((sql, may))
}

/// The comment on an app role that names its tenant, for the tenant's share of the app budget
/// (backlog D5-10, card B-1d): this text and [`tenant_tag`]. A comment is readable by every role
/// of the cluster, so it holds a hash of the tenant ID, never the ID.
const TENANT_TAG: &str = "ruum: app of ";

/// The neutral tag of a tenant in [`TENANT_TAG`] comments: 16 hex characters of a SHA-256 of
/// the tenant ID. It links the app roles of one tenant to each other (so the engine can sum
/// them) and to nothing else.
pub(crate) fn tenant_tag(tenant: Uuid) -> String {
    let digest = Sha256::digest(format!("ruum tenant {tenant}").as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("{TENANT_TAG}{}", &hex[..16])
}

/// The sum of the connection limits of the app roles of one tenant (by its [`tenant_tag`]).
async fn tenant_used(conn: &mut PgConnection, tag: &str) -> Result<i64, EngineError> {
    let row = sqlx::query(
        "select coalesce(sum(rolconnlimit), 0)::int8 from pg_catalog.pg_roles
          where rolname ~ '^r_[0-9a-f]{16}$' and rolconnlimit > 0
            and pg_catalog.shobj_description(oid, 'pg_authid') = $1",
    )
    .persistent(false)
    .bind(tag)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| internal("read the tenant's share of the app budget", e))?;
    Ok(row.get(0))
}

/// The share of the app budget that one tenant's app roles may hold: the setting, or a tenth of
/// the budget.
fn tenant_share(engine: &PostgresEngine, budget: i64) -> i64 {
    engine
        .config
        .app_tenant_share
        .map(i64::from)
        .unwrap_or(budget / 10)
}

/// The sum of the connection limits of every app role of the cluster, and the budget.
async fn budget(
    engine: &PostgresEngine,
    conn: &mut PgConnection,
) -> Result<(i64, i64), EngineError> {
    let row = sqlx::query(
        "select coalesce((select sum(rolconnlimit) from pg_catalog.pg_roles
                           where rolname ~ '^r_[0-9a-f]{16}$' and rolconnlimit > 0), 0)::int8,
                (select setting::int8 from pg_catalog.pg_settings where name = 'max_connections')",
    )
    .persistent(false)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| internal("read the app connection budget", e))?;
    let used: i64 = row.get(0);
    let max: i64 = row.get(1);
    let budget = engine
        .config
        .app_connection_budget
        .map(i64::from)
        .unwrap_or(max / 2);
    Ok((used, budget))
}

/// Makes the cluster role of an app (or nothing when it exists), with its settings and
/// `CONNECT` on its own database, in one transaction under the budget lock. While the apps of
/// its database are suspended (`login` false: the database is in the trash), the role is made,
/// or made again, `NOLOGIN`. A new role must fit the cluster's app budget and the share of
/// `tenant` (the tenant of the database's `ruum_meta.ident`; backlog D5-10); its comment names
/// the tenant by [`tenant_tag`].
async fn make_cluster_role(
    engine: &PostgresEngine,
    database: &str,
    name: &str,
    login: bool,
    tenant: Uuid,
) -> Result<(), EngineError> {
    let mut admin = engine.admin().await?;
    let result = async {
        run(&mut admin, "begin", "begin".to_string()).await?;
        sqlx::query("select pg_catalog.pg_advisory_xact_lock($1)")
            .persistent(false)
            .bind(BUDGET_LOCK)
            .execute(&mut *admin)
            .await
            .map_err(|e| internal("lock the app connection budget", e))?;
        let exists: bool =
            sqlx::query("select exists (select 1 from pg_catalog.pg_roles where rolname = $1)")
                .persistent(false)
                .bind(name)
                .fetch_one(&mut *admin)
                .await
                .map_err(|e| internal("look for the app role", e))?
                .get(0);
        if !exists {
            let limit = i64::from(engine.config.app_connection_limit);
            let (used, budget) = budget(engine, &mut admin).await?;
            if used + limit > budget {
                return Err(EngineError::PermissionDenied(
                    "app_connection_budget".to_string(),
                ));
            }
            // The tenant's share of the budget (backlog D5-10): one tenant never takes the
            // room of all the others. Checked under the same lock as the budget.
            let tag = tenant_tag(tenant);
            if tenant_used(&mut admin, &tag).await? + limit > tenant_share(engine, budget) {
                return Err(EngineError::PermissionDenied(
                    "app_connection_budget".to_string(),
                ));
            }
            let role = ident(name);
            let mut sql = format!(
                "create role {role} login noinherit nosuperuser nocreatedb nocreaterole \
                   noreplication nobypassrls connection limit {limit} password null;\n\
                 comment on role {role} is {};\n",
                literal(&tag)
            );
            if !login {
                sql.push_str(&format!("alter role {role} nologin;\n"));
            }
            sql.push_str(&settings_sql(engine, &mut admin, name).await?.0);
            run(&mut admin, "create the app role", sql).await?;
        } else if !login {
            run(
                &mut admin,
                "suspend the app role",
                format!("alter role {} nologin", ident(name)),
            )
            .await?;
        }
        run(
            &mut admin,
            "grant connect to the app role",
            format!(
                "grant connect on database {} to {}",
                ident(database),
                ident(name)
            ),
        )
        .await?;
        run(&mut admin, "commit", "commit".to_string()).await
    }
    .await;
    if result.is_err() {
        let _ = run(&mut admin, "rollback", "rollback".to_string()).await;
    }
    result
}

/// The table grants of an app role (in an `owner` session: the owner owns the tables).
fn table_grants_sql(kind: RoleKind, name: &str) -> String {
    let role = ident(name);
    let mut sql = format!(
        "grant select on all tables in schema public to {role};\n\
         grant usage, select on all sequences in schema public to {role};\n\
         alter default privileges in schema public grant select on tables to {role};\n\
         alter default privileges in schema public grant usage, select on sequences to {role};\n"
    );
    if kind == RoleKind::AppWrite {
        sql.push_str(&format!(
            "grant insert, update, delete on all tables in schema public to {role};\n\
             grant update on all sequences in schema public to {role};\n\
             alter default privileges in schema public grant insert, update, delete on tables to {role};\n\
             alter default privileges in schema public grant update on sequences to {role};\n"
        ));
    }
    sql
}

/// Grants the schemas (as `ruum_dbhost`, in `conn`) and the tables (in an `owner` session) to
/// the app roles `rows`.
async fn grant(
    engine: &PostgresEngine,
    state: &DbState,
    conn: &mut PgConnection,
    rows: &[AppRow],
) -> Result<(), EngineError> {
    if rows.is_empty() {
        return Ok(());
    }
    let names: Vec<String> = rows.iter().map(|r| ident(&r.role_name)).collect();
    run(
        conn,
        "grant the schemas to the app roles",
        format!(
            "grant usage on schema public, extensions to {}",
            names.join(", ")
        ),
    )
    .await?;
    let sql: String = rows
        .iter()
        .map(|r| table_grants_sql(r.kind, &r.role_name))
        .collect();
    let owner = engine.pool(state, RoleKind::Owner);
    let mut lease = owner.acquire(engine).await?;
    match run(lease.conn(), "grant the tables to the app roles", sql).await {
        Ok(()) => {
            lease.release(engine).await;
            Ok(())
        }
        Err(e) => {
            lease.close().await;
            Err(e)
        }
    }
}

/// Grants again to every app role of the database (after a migration and a lens sync), so a
/// table that a migration made in another way, or revoked, follows the kinds table. Logs a
/// failure and goes on: the change it follows is committed. Takes `state.ops`, so a grant never
/// lands between the strip and the `DROP ROLE` of [`drop_role`].
pub(crate) async fn regrant_all(engine: &PostgresEngine, state: &DbState) {
    let _ops = state.ops.lock().await;
    regrant_all_held(engine, state).await
}

/// [`regrant_all`] for a caller that holds `state.ops` (a restore in place).
pub(crate) async fn regrant_all_held(engine: &PostgresEngine, state: &DbState) {
    let result = async {
        let mut conn = engine.dbhost(&state.name).await?;
        let r = async {
            let rows = read_rows(&mut conn).await?;
            grant(engine, state, &mut conn, &rows).await
        }
        .await;
        let _ = conn.close().await;
        r
    }
    .await;
    if let Err(e) = result {
        tracing::warn!(database = %state.name, error = %e, "app role grants not renewed");
    }
}

// ------------------------------------------------------------- the apps of a database in the trash

/// The table of the flag "the apps of this database are suspended" (its database is in the
/// trash). Made when it is first needed: a database made before it has none (not suspended).
const ACCESS_TABLE_SQL: &str = "create table if not exists ruum_meta.app_access \
     (id int primary key check (id = 1), suspended boolean not null)";

/// Whether the apps of the database of `conn` (a `ruum_dbhost` connection) are suspended.
pub(crate) async fn access_suspended(conn: &mut PgConnection) -> Result<bool, EngineError> {
    let exists: bool =
        sqlx::query("select pg_catalog.to_regclass('ruum_meta.app_access') is not null")
            .persistent(false)
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| internal("look for the app access flag", e))?
            .get(0);
    if !exists {
        return Ok(false);
    }
    let row = sqlx::query("select suspended from ruum_meta.app_access where id = 1")
        .persistent(false)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| internal("read the app access flag", e))?;
    Ok(row.map(|r| r.get::<bool, _>(0)).unwrap_or(false))
}

/// Sets the flag (in the caller's transaction, or as one statement each).
pub(crate) async fn write_access(
    conn: &mut PgConnection,
    suspended: bool,
) -> Result<(), EngineError> {
    run(
        conn,
        "make the app access flag",
        ACCESS_TABLE_SQL.to_string(),
    )
    .await?;
    sqlx::query(
        "insert into ruum_meta.app_access (id, suspended) values (1, $1)
         on conflict (id) do update set suspended = excluded.suspended",
    )
    .persistent(false)
    .bind(suspended)
    .execute(&mut *conn)
    .await
    .map_err(|e| internal("write the app access flag", e))?;
    Ok(())
}

/// A copy starts with its apps not suspended (it has no app role; its source may be in the
/// trash).
pub(crate) async fn forget_access(conn: &mut PgConnection) -> Result<(), EngineError> {
    if access_suspended(conn).await? {
        write_access(conn, false).await?;
    }
    Ok(())
}

/// A lock of the whole cluster on the life of the database `name` (an advisory lock of a
/// session of its own, keyed by the name): `create_app_role`, `drop_app_role`,
/// `app_credentials`, `suspend_app_access` / `resume_app_access` and `drop_database` hold it, so these steps of different server
/// replicas never interleave (Kimi run 15 on D-5, third round). Held until the returned
/// session closes ([`unlock`]); a session that breaks or is dropped ends and lets it go.
/// Take it before the per-process locks of the database (`ops`, the gate).
pub(crate) async fn lifecycle_lock(
    engine: &PostgresEngine,
    name: &str,
) -> Result<PgConnection, EngineError> {
    let mut conn = PgConnection::connect_with(&engine.config.dbhost_options())
        .await
        .map_err(|e| internal("connect for the lifecycle lock", e))?;
    let locked =
        sqlx::query("select pg_catalog.pg_advisory_lock(pg_catalog.hashtextextended($1, 0))")
            .persistent(false)
            .bind(format!("{LIFECYCLE_KEY}{name}"))
            .execute(&mut conn)
            .await;
    if let Err(e) = locked {
        let _ = conn.close().await;
        return Err(internal("take the lifecycle lock of a database", e));
    }
    Ok(conn)
}

/// Lets the lock of [`lifecycle_lock`] go (the session ends).
pub(crate) async fn unlock(conn: PgConnection) {
    let _ = conn.close().await;
}

/// `suspend_app_access` and `resume_app_access`: the flag first (so a role made later is made
/// `NOLOGIN`, and no password is issued), then every app role of the list; on suspend their
/// sessions end, and the call fails while one is still running.
pub(crate) async fn set_access(
    engine: &PostgresEngine,
    db: &DatabaseRef,
    login: bool,
) -> Result<(), EngineError> {
    let state = engine.resolve(db).await?;
    let lock = lifecycle_lock(engine, &state.name).await?;
    let r = set_access_locked(engine, &state, login).await;
    unlock(lock).await;
    r
}

async fn set_access_locked(
    engine: &PostgresEngine,
    state: &DbState,
    login: bool,
) -> Result<(), EngineError> {
    let _ops = state.ops.lock().await;
    let mut conn = engine.dbhost(&state.name).await?;
    let rows = async {
        write_access(&mut conn, !login).await?;
        read_rows(&mut conn).await
    }
    .await;
    let _ = conn.close().await;
    let mut names = Vec::new();
    for row in rows? {
        if can_login(engine, &row.role_name).await?.is_some() {
            names.push(row.role_name);
        }
    }
    if names.is_empty() {
        return Ok(());
    }
    let list: Vec<String> = names.iter().map(|n| ident(n)).collect();
    let mut admin = engine.admin().await?;
    let sql: String = list
        .iter()
        .map(|r| {
            format!(
                "alter role {r} {};\n",
                if login { "login" } else { "nologin" }
            )
        })
        .collect();
    run(&mut admin, "suspend or resume the app roles", sql).await?;
    drop(admin);
    if !login {
        end_sessions(engine, &names, None).await?;
    }
    Ok(())
}

/// `create_app_role`.
pub(crate) async fn create(
    engine: &PostgresEngine,
    db: &DatabaseRef,
    kind: RoleKind,
    binding: Uuid,
) -> Result<AppRole, EngineError> {
    if !kind.is_app() {
        return Err(EngineError::InvalidRole(format!(
            "{} is not an app kind",
            kind.as_str()
        )));
    }
    let state = engine.resolve(db).await?;
    let lock = lifecycle_lock(engine, &state.name).await?;
    let r = create_locked(engine, db, &state, kind, binding).await;
    unlock(lock).await;
    r
}

async fn create_locked(
    engine: &PostgresEngine,
    db: &DatabaseRef,
    state: &DbState,
    kind: RoleKind,
    binding: Uuid,
) -> Result<AppRole, EngineError> {
    let _gate = engine.enter(state).await?;
    let _ops = state.ops.lock().await;
    #[cfg(test)]
    {
        let pause = engine.create_pause.lock().take();
        if let Some((reached, go)) = pause {
            reached.notify_one();
            go.notified().await;
        }
    }
    // Under the lifecycle lock: a database that another replica dropped is gone here.
    let mut conn = engine.dbhost(&state.name).await?;
    let result = async {
        let rows = read_rows(&mut conn).await?;
        let row = match rows
            .into_iter()
            .find(|r| r.kind == kind && r.binding == binding)
        {
            Some(row) => row,
            None => {
                // The row first: a crash leaves a row without a role, made on the next call.
                let row = AppRow {
                    kind,
                    binding,
                    role_name: neutral_name("r_"),
                };
                sqlx::query(
                    "insert into ruum_meta.roles (kind, binding, role_name) values ($1, $2, $3)",
                )
                .persistent(false)
                .bind(kind.as_str())
                .bind(binding)
                .bind(&row.role_name)
                .execute(&mut conn)
                .await
                .map_err(|e| internal("record the app role", e))?;
                row
            }
        };
        let login = !access_suspended(&mut conn).await?;
        let tenant = ident_tenant(&mut conn, db).await?;
        if let Err(e) = make_cluster_role(engine, &state.name, &row.role_name, login, tenant).await
        {
            // Over the budget: no role and no row.
            if matches!(&e, EngineError::PermissionDenied(_)) {
                let _ = sqlx::query("delete from ruum_meta.roles where role_name = $1")
                    .persistent(false)
                    .bind(&row.role_name)
                    .execute(&mut conn)
                    .await;
            }
            return Err(e);
        }
        grant(engine, state, &mut conn, std::slice::from_ref(&row)).await?;
        Ok(row)
    }
    .await;
    let _ = conn.close().await;
    let row = result?;
    let suspended = !can_login(engine, &row.role_name).await?.unwrap_or(false);
    Ok(AppRole {
        database: db.clone(),
        kind,
        binding,
        role_name: row.role_name,
        suspended,
    })
}

/// The tenant of the database of `conn` (a `ruum_dbhost` connection), from its
/// `ruum_meta.ident`: the database itself says whose share a new role counts against, not the
/// caller. A database without the row (not a live database of this engine) is refused, and so
/// is one whose tenant is not the tenant of `db`.
async fn ident_tenant(conn: &mut PgConnection, db: &DatabaseRef) -> Result<Uuid, EngineError> {
    let tenant: Option<Uuid> =
        sqlx::query_scalar("select tenant_id from ruum_meta.ident where id = 1")
            .persistent(false)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|e| internal("read the tenant of the database", e))?;
    match tenant {
        Some(t) if t == db.tenant_id => Ok(t),
        _ => Err(EngineError::NotFound),
    }
}

/// `app_roles`.
pub(crate) async fn list(
    engine: &PostgresEngine,
    db: &DatabaseRef,
) -> Result<Vec<AppRole>, EngineError> {
    let state = engine.resolve(db).await?;
    let _gate = engine.enter(&state).await?;
    let mut conn = engine.dbhost(&state.name).await?;
    let rows = read_rows(&mut conn).await;
    let _ = conn.close().await;
    let mut out = Vec::new();
    for row in rows? {
        let suspended = !can_login(engine, &row.role_name).await?.unwrap_or(false);
        out.push(AppRole {
            database: db.clone(),
            kind: row.kind,
            binding: row.binding,
            role_name: row.role_name,
            suspended,
        });
    }
    Ok(out)
}

/// The row of an app role reference.
async fn find(
    engine: &PostgresEngine,
    state: &DbState,
    role: &RoleRef,
) -> Result<Option<AppRow>, EngineError> {
    let (kind, binding) = require_app(role)?;
    let mut conn = engine.dbhost(&state.name).await?;
    let rows = read_rows(&mut conn).await;
    let _ = conn.close().await;
    Ok(rows?
        .into_iter()
        .find(|r| r.kind == kind && r.binding == binding))
}

// ------------------------------------------------------------------------------- passwords

/// 32 bytes of the OS RNG as hex (256 bits).
fn new_password() -> Result<String, EngineError> {
    let mut bytes = [0u8; 32];
    rand::rngs::SysRng
        .try_fill_bytes(&mut bytes)
        .map_err(|e| EngineError::Internal(format!("the OS RNG failed: {e}")))?;
    Ok(hex::encode(bytes))
}

fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    for p in parts {
        inner.update(p);
    }
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner);
    let mut out = [0u8; 32];
    out.copy_from_slice(&outer.finalize());
    out
}

/// PBKDF2-HMAC-SHA-256 with a 32-byte output (one block).
fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut u = hmac_sha256(password, &[salt, &1u32.to_be_bytes()]);
    let mut out = u;
    for _ in 1..iterations {
        u = hmac_sha256(password, &[&u]);
        for (o, x) in out.iter_mut().zip(u.iter()) {
            *o ^= x;
        }
    }
    out
}

/// The SCRAM-SHA-256 verifier of a password (RFC 5802, RFC 7677), as Postgres stores it. The
/// password is hex, so SASLprep leaves it as it is.
fn scram_verifier(password: &str) -> Result<String, EngineError> {
    use base64::Engine;
    const ITERATIONS: u32 = 4096;
    let mut salt = [0u8; 16];
    rand::rngs::SysRng
        .try_fill_bytes(&mut salt)
        .map_err(|e| EngineError::Internal(format!("the OS RNG failed: {e}")))?;
    let salted = pbkdf2_sha256(password.as_bytes(), &salt, ITERATIONS);
    let client_key = hmac_sha256(&salted, &[b"Client Key"]);
    let stored_key = Sha256::digest(client_key);
    let server_key = hmac_sha256(&salted, &[b"Server Key"]);
    let b64 = base64::engine::general_purpose::STANDARD;
    Ok(format!(
        "SCRAM-SHA-256${ITERATIONS}:{}${}:{}",
        b64.encode(salt),
        b64.encode(stored_key),
        b64.encode(server_key)
    ))
}

/// `app_credentials`, under the database's lifecycle lock (Kimi run 17 B-1d, third re-check):
/// a password is never set on a role that another replica's `drop_app_role` is dropping, or
/// that another replica's `create_app_role` made again from a stale read of the list.
pub(crate) async fn credentials(
    engine: &PostgresEngine,
    role: &RoleRef,
    valid_for: Duration,
) -> Result<AppCredentials, EngineError> {
    require_app(role)?;
    let state = engine.resolve(role.database()).await?;
    let lock = lifecycle_lock(engine, &state.name).await?;
    let r = credentials_locked(engine, state, role, valid_for).await;
    unlock(lock).await;
    r
}

async fn credentials_locked(
    engine: &PostgresEngine,
    state: Arc<DbState>,
    role: &RoleRef,
    valid_for: Duration,
) -> Result<AppCredentials, EngineError> {
    let _gate = engine.enter(&state).await?;
    let _ops = state.ops.lock().await;
    let row = find(engine, &state, role)
        .await?
        .ok_or_else(|| EngineError::InvalidRole("no such app role".to_string()))?;
    // The database is in the trash: no password, whatever the role's own state.
    let suspended = {
        let mut conn = engine.dbhost(&state.name).await?;
        let r = access_suspended(&mut conn).await;
        let _ = conn.close().await;
        r?
    };
    if suspended {
        return Err(EngineError::PermissionDenied(
            "app_role_suspended".to_string(),
        ));
    }
    match can_login(engine, &row.role_name).await? {
        None => return Err(EngineError::InvalidRole("no such app role".to_string())),
        Some(false) => {
            return Err(EngineError::PermissionDenied(
                "app_role_suspended".to_string(),
            ));
        }
        Some(true) => {}
    }
    let password = new_password()?;
    let verifier = scram_verifier(&password)?;
    let valid_until = time::Duration::try_from(valid_for.max(Duration::from_secs(1)))
        .ok()
        .and_then(|d| OffsetDateTime::now_utc().checked_add(d))
        .ok_or_else(|| EngineError::InvalidRole("the password lifetime is too long".to_string()))?;
    let until = valid_until
        .format(&time::macros::format_description!(
            "[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:6]+00"
        ))
        .map_err(|e| EngineError::Internal(e.to_string()))?;
    let name = ident(&row.role_name);
    let mut admin = engine.admin().await?;
    let mut sql = format!(
        "begin;\n\
         alter role {name} reset all;\n\
         alter role {name} in database {db} reset all;\n",
        db = ident(&state.name),
    );
    let (settings, limited) = settings_sql(engine, &mut admin, &row.role_name).await?;
    // Without a temp file limit one app could fill the cluster's disk (backlog D5-3): no
    // password unless the configuration says that is accepted (tests, the laptop).
    if !limited && engine.config.app_require_temp_file_limit {
        return Err(EngineError::PermissionDenied(
            "temp_file_limit_not_enforced".to_string(),
        ));
    }
    sql.push_str(&settings);
    sql.push_str(&format!(
        "alter role {name} password {pw} valid until {until};\ncommit;",
        pw = literal(&verifier),
        until = literal(&until),
    ));
    if let Err(e) = sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
        .execute(&mut *admin)
        .await
    {
        // The error text never holds the statement (only the server's message and code).
        let _ = run(&mut admin, "rollback", "rollback".to_string()).await;
        return Err(internal("set the app role's password", e));
    }
    drop(admin);
    // The address apps reach (card B-1d): the setting, else the engine's own address.
    let options = engine.config.connect_options();
    let host = engine
        .config
        .app_host
        .clone()
        .unwrap_or_else(|| options.get_host().to_string());
    let port = engine.config.app_port.unwrap_or_else(|| options.get_port());
    Ok(AppCredentials::new(
        row.role_name,
        state.name.clone(),
        host,
        port,
        valid_until,
        password,
    ))
}

// ------------------------------------------------------------------------- the end of a role

/// Ends the sessions of `names` (or only cancels their statements). `ruum_dbhost` takes `INHERIT`
/// membership in the roles for the one transaction of the call (a role's backends may be
/// signalled with its privileges). Returns how many were signalled.
pub(crate) async fn signal(
    engine: &PostgresEngine,
    names: &[String],
    database: Option<&str>,
    terminate: bool,
) -> Result<usize, EngineError> {
    if names.is_empty() {
        return Ok(0);
    }
    let _membership = engine.membership.lock().await;
    let mut admin = engine.admin().await?;
    let list: Vec<String> = names.iter().map(|n| ident(n)).collect();
    let func = if terminate {
        "pg_catalog.pg_terminate_backend(pid, 5000)"
    } else {
        "pg_catalog.pg_cancel_backend(pid)"
    };
    let result = async {
        run(
            &mut admin,
            "take the app roles' privileges",
            format!(
                "begin; grant {} to ruum_dbhost with inherit true, set false",
                list.join(", ")
            ),
        )
        .await?;
        let n: i64 = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select count(*) filter (where {func}) from pg_catalog.pg_stat_activity
              where usename = any($1::text[]) and ($2::text is null or datname = $2)
                and pid <> pg_catalog.pg_backend_pid()"
        )))
        .persistent(false)
        .bind(names)
        .bind(database)
        .fetch_one(&mut *admin)
        .await
        .map_err(|e| internal("signal the app role's sessions", e))?
        .get(0);
        run(
            &mut admin,
            "give the app roles' privileges back",
            format!("revoke {} from ruum_dbhost; commit", list.join(", ")),
        )
        .await?;
        Ok(n.max(0) as usize)
    }
    .await;
    if result.is_err() {
        let _ = run(&mut admin, "rollback", "rollback".to_string()).await;
    }
    result
}

/// How many sessions of `names` (in `database`, or anywhere) are running.
async fn live_sessions(
    engine: &PostgresEngine,
    names: &[String],
    database: Option<&str>,
) -> Result<i64, EngineError> {
    let mut admin = engine.admin().await?;
    let row = sqlx::query(
        "select count(*) from pg_catalog.pg_stat_activity
          where usename = any($1::text[]) and ($2::text is null or datname = $2)
            and pid <> pg_catalog.pg_backend_pid()",
    )
    .persistent(false)
    .bind(names)
    .bind(database)
    .fetch_one(&mut *admin)
    .await
    .map_err(|e| internal("count the app role's sessions", e))?;
    Ok(row.get(0))
}

/// Ends the sessions of `names` and checks that none is left (a new transaction each time:
/// `pg_stat_activity` is a snapshot per transaction). Tries twice, then fails, so a caller never
/// reports an app role suspended or dropped while one of its sessions still runs.
pub(crate) async fn end_sessions(
    engine: &PostgresEngine,
    names: &[String],
    database: Option<&str>,
) -> Result<(), EngineError> {
    if names.is_empty() {
        return Ok(());
    }
    for _ in 0..2 {
        signal(engine, names, database, true).await?;
        for _ in 0..10 {
            if live_sessions(engine, names, database).await? == 0 {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    Err(EngineError::Internal(
        "a session of an app role did not end".to_string(),
    ))
}

/// `suspend_app_role` and `resume_app_role`. A role of a database whose apps are suspended
/// (in the trash) is not resumed alone: [`set_access`] resumes them all.
pub(crate) async fn set_login(
    engine: &PostgresEngine,
    role: &RoleRef,
    login: bool,
) -> Result<(), EngineError> {
    require_app(role)?;
    let state = engine.resolve(role.database()).await?;
    let _ops = state.ops.lock().await;
    let Some(row) = find(engine, &state, role).await? else {
        return Ok(());
    };
    if can_login(engine, &row.role_name).await?.is_none() {
        return Ok(());
    }
    if login {
        let mut conn = engine.dbhost(&state.name).await?;
        let suspended = access_suspended(&mut conn).await;
        let _ = conn.close().await;
        if suspended? {
            return Err(EngineError::PermissionDenied(
                "app_role_suspended".to_string(),
            ));
        }
    }
    let mut admin = engine.admin().await?;
    run(
        &mut admin,
        "suspend or resume the app role",
        format!(
            "alter role {} {}",
            ident(&row.role_name),
            if login { "login" } else { "nologin" }
        ),
    )
    .await?;
    drop(admin);
    if !login {
        end_sessions(engine, std::slice::from_ref(&row.role_name), None).await?;
    }
    Ok(())
}

/// The roles that an object of the database of `conn` depends on (owners, rights, default
/// privileges, policies; `pg_shdepend`) and that are neutral role names (`r_` + 16 hex) other
/// than `keep`: in a copy, the source's app roles (and any role a grant of the source named).
pub(crate) async fn foreign_roles(
    conn: &mut PgConnection,
    keep: &[String],
) -> Result<Vec<String>, EngineError> {
    let rows = sqlx::query(
        "select distinct r.rolname::text
           from pg_catalog.pg_shdepend d
           join pg_catalog.pg_roles r on r.oid = d.refobjid
          where d.dbid = (select oid from pg_catalog.pg_database
                           where datname = pg_catalog.current_database())
            and d.refclassid = 'pg_catalog.pg_authid'::pg_catalog.regclass
            and r.rolname ~ '^r_[0-9a-f]{16}$'
            and not (r.rolname = any($1::text[]))
          order by 1",
    )
    .persistent(false)
    .bind(keep)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| internal("list the foreign roles of the database", e))?;
    Ok(rows.into_iter().map(|r| r.get(0)).collect())
}

/// Takes the roles `names` out of the policies of the database of `conn`: a policy that names
/// only them is dropped (it gave or restricted only them), else they leave its role list.
async fn strip_policies(conn: &mut PgConnection, names: &[String]) -> Result<(), EngineError> {
    let rows = sqlx::query(
        "select pg_catalog.format('%I', p.polname), c.oid::pg_catalog.regclass::text,
                coalesce((select pg_catalog.array_agg(pg_catalog.quote_ident(r.rolname) order by r.rolname)
                            from pg_catalog.unnest(p.polroles) o
                            join pg_catalog.pg_roles r on r.oid = o
                           where not (r.rolname = any($1::text[]))), '{}'::text[]),
                0 = any(p.polroles)
           from pg_catalog.pg_policy p
           join pg_catalog.pg_class c on c.oid = p.polrelid
          where exists (select 1 from pg_catalog.unnest(p.polroles) o
                          join pg_catalog.pg_roles r on r.oid = o
                         where r.rolname = any($1::text[]))",
    )
    .persistent(false)
    .bind(names)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| internal("list the policies of the roles", e))?;
    for row in rows {
        let policy: String = row.get(0);
        let table: String = row.get(1);
        let mut keep: Vec<String> = row.get(2);
        if row.get::<bool, _>(3) {
            keep.push("public".to_string());
        }
        let sql = if keep.is_empty() {
            format!("drop policy {policy} on {table}")
        } else {
            format!("alter policy {policy} on {table} to {}", keep.join(", "))
        };
        run(conn, "take a role out of a policy", sql).await?;
    }
    Ok(())
}

/// Brings the default privileges that `names` own (`ALTER DEFAULT PRIVILEGES` without `FOR
/// ROLE`, which any role may run for itself) back to the built-in defaults, so no row of
/// `pg_default_acl` names them. [`revoke_old_rights`] has revoked every grantee already; a
/// global row is gone once it equals the built-in default again. Needs the privileges of each
/// role.
async fn clear_default_acl(conn: &mut PgConnection, names: &[String]) -> Result<(), EngineError> {
    let rows = sqlx::query(
        "select r.rolname::text, d.defaclobjtype::text
           from pg_catalog.pg_default_acl d
           join pg_catalog.pg_roles r on r.oid = d.defaclrole
          where r.rolname = any($1::text[]) and d.defaclnamespace = 0",
    )
    .persistent(false)
    .bind(names)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| internal("list the default privileges of the roles", e))?;
    for row in rows {
        let role = ident(&row.get::<String, _>(0));
        let target = format!("alter default privileges for role {role}");
        let sql = match row.get::<String, _>(1).as_str() {
            "r" => format!("{target} grant all on tables to {role}"),
            "S" => format!("{target} grant all on sequences to {role}"),
            "f" => format!(
                "{target} grant all on functions to {role}; \
                 {target} grant execute on functions to public"
            ),
            "T" => format!(
                "{target} grant all on types to {role}; {target} grant usage on types to public"
            ),
            "n" => format!("{target} grant all on schemas to {role}"),
            other => {
                return Err(EngineError::Internal(format!(
                    "a default privilege of an unknown object type ({other})"
                )));
            }
        };
        run(conn, "reset a default privilege", sql).await?;
    }
    Ok(())
}

/// Takes every object, right, default privilege and policy of `names` out of the database of
/// `conn` (a `ruum_dbhost` connection inside a transaction). What they own (a large object a
/// superuser gave them: the cluster set-up keeps `lo_create` from them) goes to `owner`, the role that owns the
/// database's objects now, with `REASSIGN OWNED`. `ruum_dbhost` takes `INHERIT` membership in
/// `names` and, unless `owner_held` (the hand-over of a copy holds it already), `INHERIT` and
/// `SET` in `owner` for this transaction only (granted and revoked here, so a rollback takes
/// the grant back too). Fails when anything of the database still depends on one of `names`.
/// The caller holds `engine.membership`.
pub(crate) async fn strip_in_tx(
    conn: &mut PgConnection,
    owner: &str,
    owner_held: bool,
    names: &[String],
) -> Result<(), EngineError> {
    if names.is_empty() {
        return Ok(());
    }
    let list: Vec<String> = names.iter().map(|n| ident(n)).collect();
    let mut grant = format!(
        "grant {} to ruum_dbhost with inherit true, set false",
        list.join(", ")
    );
    let mut members = list.clone();
    if !owner_held {
        grant.push_str(&format!(
            ";\ngrant {} to ruum_dbhost with inherit true, set true",
            ident(owner)
        ));
        members.push(ident(owner));
    }
    run(
        conn,
        "take the privileges of the owner and the roles",
        grant,
    )
    .await?;
    run(
        conn,
        "give what the roles own to the owner",
        format!("reassign owned by {} to {}", list.join(", "), ident(owner)),
    )
    .await?;
    let roles: Vec<EngineRole> = names
        .iter()
        .map(|n| EngineRole {
            kind: RoleKind::AppRead,
            binding: None,
            role_name: n.clone(),
        })
        .collect();
    revoke_old_rights(conn, &roles).await?;
    strip_policies(conn, names).await?;
    clear_default_acl(conn, names).await?;
    run(
        conn,
        "give the privileges back",
        format!("revoke {} from ruum_dbhost", members.join(", ")),
    )
    .await?;
    let left = leftover_dependencies(conn, &roles).await?;
    if left > 0 {
        return Err(EngineError::Internal(format!(
            "{left} objects of the database still depend on a role that leaves it"
        )));
    }
    Ok(())
}

/// `drop_app_role`, under the database's lifecycle lock (Kimi run 17 B-1d, third re-check):
/// without it, another replica's `create_app_role` that read the list before this drop made the
/// role again under the same name after it, with no row in `ruum_meta.roles`, so no later drop
/// by binding found it.
pub(crate) async fn drop_role(engine: &PostgresEngine, role: &RoleRef) -> Result<(), EngineError> {
    require_app(role)?;
    let state = engine.resolve(role.database()).await?;
    let lock = lifecycle_lock(engine, &state.name).await?;
    let r = drop_role_locked(engine, state, role).await;
    unlock(lock).await;
    r
}

async fn drop_role_locked(
    engine: &PostgresEngine,
    state: Arc<DbState>,
    role: &RoleRef,
) -> Result<(), EngineError> {
    let _gate = engine.enter(&state).await?;
    let _ops = state.ops.lock().await;
    let Some(row) = find(engine, &state, role).await? else {
        return Ok(());
    };
    let name = row.role_name.clone();
    if can_login(engine, &name).await?.is_some() {
        let mut admin = engine.admin().await?;
        run(
            &mut admin,
            "refuse logins of the app role",
            format!("alter role {} nologin", ident(&name)),
        )
        .await?;
        drop(admin);
        end_sessions(engine, std::slice::from_ref(&name), None).await?;
        let drop_sql = format!(
            "revoke connect, temporary on database {db} from {r};\n\
             drop role if exists {r}",
            db = ident(&state.name),
            r = ident(&name)
        );
        let mut dropped = false;
        for _ in 0..DROP_ATTEMPTS {
            // Its rights in the database (as the owner and as the role itself).
            {
                let _membership = engine.membership.lock().await;
                let mut conn = engine.dbhost(&state.name).await?;
                let r = async {
                    run(&mut conn, "begin", "begin".to_string()).await?;
                    strip_in_tx(
                        &mut conn,
                        state.meta.role(RoleKind::Owner),
                        false,
                        std::slice::from_ref(&name),
                    )
                    .await?;
                    run(&mut conn, "commit", "commit".to_string()).await
                }
                .await;
                let _ = conn.close().await;
                r?;
            }
            // Its rights in the restore points of the database (snapshot databases), and any
            // right that another database's owner gave it (a grant or a policy may name any
            // role).
            let points = restore_points(engine, &state.name).await?;
            for other in dependent_databases(engine, &name, &state.name).await? {
                if points.iter().any(|(_, _, s)| *s == other) {
                    strip_snapshot(engine, &state, &other, &name).await?;
                } else {
                    strip_elsewhere(engine, &other, &name).await?;
                }
            }
            if try_drop(engine, drop_sql.clone()).await? {
                dropped = true;
                break;
            }
        }
        if !dropped {
            return Err(still_depends());
        }
    }
    // The row last: a crash before this leaves a row whose role is gone, dropped again later.
    let mut conn = engine.dbhost(&state.name).await?;
    let r = sqlx::query("delete from ruum_meta.roles where role_name = $1 and binding is not null")
        .persistent(false)
        .bind(&name)
        .execute(&mut conn)
        .await
        .map_err(|e| internal("delete the app role's row", e));
    let _ = conn.close().await;
    r.map(|_| ())
}

/// Drops the app roles `names` of a database that `drop_database` has dropped (with its
/// restore points): each leaves every other database that names it ([`strip_elsewhere`]), then
/// `DROP ROLE`. Goes on after a failure and returns the first one.
async fn drop_orphans(engine: &PostgresEngine, names: &[String]) -> Result<(), EngineError> {
    let mut first = None;
    for name in names {
        let r = async {
            for _ in 0..DROP_ATTEMPTS {
                for other in dependent_databases(engine, name, "").await? {
                    strip_elsewhere(engine, &other, name).await?;
                }
                if try_drop(engine, format!("drop role if exists {}", ident(name))).await? {
                    return Ok(());
                }
            }
            Err(still_depends())
        }
        .await;
        if let Err(e) = r
            && first.is_none()
        {
            first = Some(e);
        }
    }
    first.map_or(Ok(()), Err)
}

/// The error when every try of [`DROP_ATTEMPTS`] met a dependency that came back.
fn still_depends() -> EngineError {
    EngineError::Internal(format!(
        "drop the app role: a dependency came back after each of {DROP_ATTEMPTS} strips (2BP01)"
    ))
}

/// Runs the `DROP ROLE` statement `sql` of an app role (with the statements before it, in one
/// implicit transaction). `Ok(false)` when an object still depends on the role (2BP01): a right
/// given to it again after the strip; the caller strips again.
async fn try_drop(engine: &PostgresEngine, sql: String) -> Result<bool, EngineError> {
    #[cfg(test)]
    before_drop_hook(engine).await?;
    let mut admin = engine.admin().await?;
    match sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
        .execute(&mut *admin)
        .await
    {
        Ok(_) => Ok(true),
        Err(e) if sqlstate(&e).as_deref() == Some("2BP01") => Ok(false),
        Err(e) => Err(internal("drop the app role", e)),
    }
}

/// Tests only: runs the script of [`PostgresEngine::before_role_drop`] as the owner of its
/// database, once per call while its count lasts.
#[cfg(test)]
async fn before_drop_hook(engine: &PostgresEngine) -> Result<(), EngineError> {
    let job = {
        let mut slot = engine.before_role_drop.lock();
        match slot.as_mut() {
            Some((db, sql, n)) if *n > 0 => {
                *n -= 1;
                Some((db.clone(), sql.clone()))
            }
            _ => None,
        }
    };
    let Some((db, sql)) = job else {
        return Ok(());
    };
    let state = engine.resolve(&db).await?;
    let (mut owner, _) = engine.pool(&state, RoleKind::Owner).open(engine).await?;
    let r = sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
        .execute(&mut owner)
        .await
        .map(|_| ())
        .map_err(|e| internal("run the script before DROP ROLE", e));
    let _ = owner.close().await;
    r
}

/// Marks the app roles `names` of `database`, which `drop_database` is about to drop: a comment
/// on each role names the database ([`DROPPED_WITH`]). The comment is in the shared catalog, so
/// a retried `drop_database` still finds the roles after the database and its `ruum_meta.roles`
/// are gone ([`drop_marked`]). Only `ruum_dbhost` (with `ADMIN` on the role) can set it. A
/// mark stays when the drop fails: it does nothing while the database exists, and taking it off
/// could erase the mark of another process's try that dropped the database.
pub(crate) async fn mark_for_drop(
    engine: &PostgresEngine,
    database: &str,
    names: &[String],
) -> Result<(), EngineError> {
    let text = literal(&format!("{DROPPED_WITH}{database}"));
    let mut sql = String::new();
    for name in names {
        if can_login(engine, name).await?.is_some() {
            sql.push_str(&format!("comment on role {} is {text};\n", ident(name)));
        }
    }
    if sql.is_empty() {
        return Ok(());
    }
    let mut admin = engine.admin().await?;
    run(&mut admin, "mark the app roles of a dropped database", sql).await
}

/// Sets the connection limit of the app roles `names` of a dropped database to 0: a role that
/// another database still holds back no longer counts against the app budget ([`budget`] sums
/// positive limits only). Goes on after a failure and returns the first one.
async fn release_connections(engine: &PostgresEngine, names: &[String]) -> Result<(), EngineError> {
    let mut first = None;
    for name in names {
        let r = async {
            if can_login(engine, name).await?.is_none() {
                return Ok(());
            }
            let mut admin = engine.admin().await?;
            run(
                &mut admin,
                "take a dropped app role out of the budget",
                format!("alter role {} connection limit 0", ident(name)),
            )
            .await
        }
        .await;
        if let Err(e) = r
            && first.is_none()
        {
            first = Some(e);
        }
    }
    first.map_or(Ok(()), Err)
}

/// Drops the app roles that an earlier `drop_database` of `database` marked and could not drop
/// ([`mark_for_drop`]), once `database` is gone: no login (a resume of another replica may have
/// let it in again), out of the budget, then [`drop_orphans`]. While the database exists a
/// mark does nothing.
pub(crate) async fn drop_marked(
    engine: &PostgresEngine,
    database: &str,
) -> Result<(), EngineError> {
    let mut admin = engine.admin().await?;
    let names: Vec<String> = sqlx::query(
        "select r.rolname::text from pg_catalog.pg_roles r
          where r.rolname ~ '^r_[0-9a-f]{16}$'
            and pg_catalog.shobj_description(r.oid, 'pg_authid') = $1
            and not exists (select 1 from pg_catalog.pg_database where datname = $2)
          order by 1",
    )
    .persistent(false)
    .bind(format!("{DROPPED_WITH}{database}"))
    .bind(database)
    .fetch_all(&mut *admin)
    .await
    .map_err(|e| internal("read the marked app roles of a dropped database", e))?
    .into_iter()
    .map(|r| r.get(0))
    .collect();
    drop(admin);
    if names.is_empty() {
        return Ok(());
    }
    refuse_logins(engine, &names).await?;
    // Out of the budget first: a role that stays (the drop fails again) holds back nothing.
    let _ = release_connections(engine, &names).await;
    drop_orphans(engine, &names).await
}

/// Refuses logins of the app roles `names` and ends their sessions (`drop_database`, before its
/// drain). A role that is gone is no error.
pub(crate) async fn refuse_logins(
    engine: &PostgresEngine,
    names: &[String],
) -> Result<(), EngineError> {
    let mut live = Vec::new();
    for name in names {
        if can_login(engine, name).await?.is_some() {
            live.push(name.clone());
        }
    }
    if live.is_empty() {
        return Ok(());
    }
    let sql: String = live
        .iter()
        .map(|n| format!("alter role {} nologin;\n", ident(n)))
        .collect();
    let mut admin = engine.admin().await?;
    run(&mut admin, "refuse logins of the app roles", sql).await?;
    drop(admin);
    end_sessions(engine, &live, None).await
}

/// Takes the app role `name` out of `database`, a database of the engine that is not the
/// role's own and not one of its restore points: another database's owner granted it a right
/// or named it in a policy (a role name is readable in `pg_roles`). [`strip_in_tx`] with that
/// database's owner (from its `ruum_meta.roles`) removes such grants, policy entries and
/// default privileges; the role owns nothing there (it cannot connect), and the call fails
/// closed if it does. A database the engine does not manage (not a neutral name or
/// not owned by `ruum_dbhost`) is an error. A closed database is waited for 2 seconds (a
/// drain is short); one that stays closed is opened for the strip and closed again if it is a
/// restore point of another database (`ruum_meta.ident` says `snapshot`), else the call fails
/// with `NotReady` (try again).
async fn strip_elsewhere(
    engine: &PostgresEngine,
    database: &str,
    name: &str,
) -> Result<(), EngineError> {
    let read = || async {
        let mut admin = engine.admin().await?;
        let row = sqlx::query(
            "select d.datallowconn, d.datdba = (select oid from pg_catalog.pg_roles
                                                  where rolname = current_user)
               from pg_catalog.pg_database d where d.datname = $1",
        )
        .persistent(false)
        .bind(database)
        .fetch_optional(&mut *admin)
        .await
        .map_err(|e| internal("read a database that names the app role", e))?;
        Ok::<_, EngineError>(row.map(|r| (r.get::<bool, _>(0), r.get::<bool, _>(1))))
    };
    let Some((mut open, ours)) = read().await? else {
        return Ok(());
    };
    if !ours || !super::is_db_name(database) {
        return Err(EngineError::Internal(
            "an app role has rights in a database the engine does not manage".to_string(),
        ));
    }
    for _ in 0..10 {
        if open {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        match read().await? {
            Some((o, _)) => open = o,
            None => return Ok(()),
        }
    }
    let opened = !open;
    if opened {
        set_allow_connections(engine, database, true).await?;
    }
    let result = async {
        let _membership = engine.membership.lock().await;
        let mut conn = engine.dbhost(database).await?;
        let r = async {
            run(&mut conn, "begin", "begin".to_string()).await?;
            if opened {
                let state: Option<String> =
                    sqlx::query("select state from ruum_meta.ident where id = 1")
                        .persistent(false)
                        .fetch_optional(&mut conn)
                        .await
                        .map_err(|e| internal("read a closed database", e))?
                        .map(|r| r.get(0));
                if state.as_deref() != Some("snapshot") {
                    return Err(EngineError::NotReady);
                }
            }
            let owned: i64 = sqlx::query(
                "select count(*) from pg_catalog.pg_shdepend
                  where dbid = (select oid from pg_catalog.pg_database
                                 where datname = pg_catalog.current_database())
                    and refclassid = 'pg_catalog.pg_authid'::pg_catalog.regclass
                    and refobjid = (select oid from pg_catalog.pg_roles where rolname = $1)
                    and deptype = 'o'",
            )
            .persistent(false)
            .bind(name)
            .fetch_one(&mut conn)
            .await
            .map_err(|e| internal("look for objects of the app role", e))?
            .get(0);
            if owned > 0 {
                return Err(EngineError::Internal(
                    "an app role owns objects in a database that is not its own".to_string(),
                ));
            }
            // The other database's owner (its grants and policies are revoked with its
            // rights; `ruum_dbhost` holds them for this transaction only).
            let owner: Option<String> = sqlx::query(
                "select role_name from ruum_meta.roles where kind = 'owner' and binding is null",
            )
            .persistent(false)
            .fetch_optional(&mut conn)
            .await
            .map_err(|e| internal("read the owner of another database", e))?
            .map(|r| r.get(0));
            let Some(owner) = owner else {
                return Err(EngineError::Internal(
                    "an app role has rights in a database without an owner role".to_string(),
                ));
            };
            strip_in_tx(&mut conn, &owner, false, &[name.to_string()]).await?;
            run(&mut conn, "commit", "commit".to_string()).await
        }
        .await;
        let _ = conn.close().await;
        r
    }
    .await;
    if opened {
        let closed = set_allow_connections(engine, database, false).await;
        result?;
        return closed;
    }
    result
}

/// The databases other than `own` in which something depends on the role `name`
/// (`pg_shdepend` is shared, so the admin connection sees every database).
async fn dependent_databases(
    engine: &PostgresEngine,
    name: &str,
    own: &str,
) -> Result<Vec<String>, EngineError> {
    let mut admin = engine.admin().await?;
    let rows = sqlx::query(
        "select distinct d.datname::text
           from pg_catalog.pg_shdepend s
           join pg_catalog.pg_database d on d.oid = s.dbid
          where s.refclassid = 'pg_catalog.pg_authid'::pg_catalog.regclass
            and s.refobjid = (select oid from pg_catalog.pg_roles where rolname = $1)
            and d.datname <> $2
          order by 1",
    )
    .persistent(false)
    .bind(name)
    .bind(own)
    .fetch_all(&mut *admin)
    .await
    .map_err(|e| internal("list the databases that depend on the app role", e))?;
    Ok(rows.into_iter().map(|r| r.get(0)).collect())
}

/// Takes the app role `name` out of a snapshot database of `state` (closed to connections
/// again afterwards, also on failure).
async fn strip_snapshot(
    engine: &PostgresEngine,
    state: &DbState,
    snapshot: &str,
    name: &str,
) -> Result<(), EngineError> {
    set_allow_connections(engine, snapshot, true).await?;
    let result = async {
        let _membership = engine.membership.lock().await;
        let mut conn = engine.dbhost(snapshot).await?;
        let r = async {
            run(&mut conn, "begin", "begin".to_string()).await?;
            strip_in_tx(
                &mut conn,
                state.meta.role(RoleKind::Owner),
                false,
                &[name.to_string()],
            )
            .await?;
            sqlx::query("delete from ruum_meta.roles where role_name = $1 and binding is not null")
                .persistent(false)
                .bind(name)
                .execute(&mut conn)
                .await
                .map_err(|e| internal("forget the app role in a restore point", e))?;
            run(&mut conn, "commit", "commit".to_string()).await
        }
        .await;
        let _ = conn.close().await;
        r
    }
    .await;
    let closed = set_allow_connections(engine, snapshot, false).await;
    result?;
    closed
}

/// The app roles of a copy's meta data go: a copy, a restore point and a fresh restore have none.
pub(crate) async fn forget_rows(conn: &mut PgConnection) -> Result<(), EngineError> {
    run(
        conn,
        "forget the source's app roles",
        "delete from ruum_meta.roles where binding is not null".to_string(),
    )
    .await
}

/// Records `rows` as the app roles of the database of `conn` (a restore in place keeps the
/// current app roles, not those of the restore point).
pub(crate) async fn write_rows(
    conn: &mut PgConnection,
    rows: &[AppRow],
) -> Result<(), EngineError> {
    forget_rows(conn).await?;
    for row in rows {
        sqlx::query("insert into ruum_meta.roles (kind, binding, role_name) values ($1, $2, $3)")
            .persistent(false)
            .bind(row.kind.as_str())
            .bind(row.binding)
            .bind(&row.role_name)
            .execute(&mut *conn)
            .await
            .map_err(|e| internal("record the app roles", e))?;
    }
    Ok(())
}

/// Grants `CONNECT` on the database `name` to the app roles `rows` (a restore in place makes the
/// database again).
pub(crate) async fn grant_connect(
    engine: &PostgresEngine,
    name: &str,
    rows: &[AppRow],
) -> Result<(), EngineError> {
    if rows.is_empty() {
        return Ok(());
    }
    let names: Vec<String> = rows.iter().map(|r| ident(&r.role_name)).collect();
    let mut admin = engine.admin().await?;
    run(
        &mut admin,
        "grant connect to the app roles",
        format!(
            "grant connect on database {} to {}",
            ident(name),
            names.join(", ")
        ),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pbkdf2_matches_the_test_vectors() {
        assert_eq!(
            hex::encode(pbkdf2_sha256(b"password", b"salt", 1)),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
        );
        assert_eq!(
            hex::encode(pbkdf2_sha256(b"password", b"salt", 4096)),
            "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a"
        );
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        // RFC 4231 test case 2.
        assert_eq!(
            hex::encode(hmac_sha256(
                b"Jefe",
                &[b"what do ya want ", b"for nothing?"]
            )),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn passwords_are_long_random_hex_and_the_verifier_hides_them() {
        let a = new_password().unwrap();
        let b = new_password().unwrap();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        let v = scram_verifier(&a).unwrap();
        assert!(v.starts_with("SCRAM-SHA-256$4096:"), "{v}");
        assert!(!v.contains(&a));
    }
}
