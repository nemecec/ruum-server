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
//! sequences. Never `TRUNCATE`, `REFERENCES`, `TRIGGER`, ownership or DDL. Tables of a later
//! migration get the same rights through `ALTER DEFAULT PRIVILEGES` of the `owner` kind, and the
//! engine grants again after every `apply_migration` and `sync_lens` ([`regrant_all`]). The
//! grants on tables run in an `owner` session (the owner owns the tables); the schema grants run
//! as `ruum_dbhost` (it owns the schemas and the database).
//!
//! **Limits.** Role settings `statement_timeout` and `idle_in_transaction_session_timeout` (the
//! app may lower or raise them in its own session; they are defaults), `temp_file_limit` (a
//! superuser setting: the app cannot raise it; set when `ruum_dbhost` holds `SET` on it, else a
//! warning, backlog D5-3), and a connection limit per role. The sum of the connection limits
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
//! **Ending a role.** Suspend: `NOLOGIN` and its sessions end. Drop: `NOLOGIN`, sessions end,
//! every right and default privilege it holds or owns in the database goes ([`strip_in_tx`]),
//! `DROP ROLE`, then the row. `ruum_dbhost` may only signal or revoke with the privileges of the
//! role or the owner, so it takes `INHERIT` membership in them inside the one transaction of the
//! step and gives it back before the commit (a failure rolls the grant back too).
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
use super::errors::internal;
use super::lens::{ident, literal};
use super::setup::{self, run};
use super::{DbState, PostgresEngine, neutral_name};
use crate::error::EngineError;
use crate::model::*;
use crate::role::{RoleKind, RoleRef};
use rand::TryRng;
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection, Row};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use time::OffsetDateTime;
use uuid::Uuid;

/// The `search_path` of an app role: lens `base` (decision 0004-34 g).
pub(crate) const APP_SEARCH_PATH: &str = "public, extensions";

/// The key of the advisory lock that serializes the budget check and `CREATE ROLE`.
const BUDGET_LOCK: i64 = 0x5275_756d_4435; // "RuumD5"

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

/// The names of the app roles of the database `name` (empty when it cannot be read).
pub(crate) async fn names(engine: &PostgresEngine, name: &str) -> Vec<String> {
    let Ok(mut conn) = engine.dbhost(name).await else {
        return Vec::new();
    };
    let rows = read_rows(&mut conn).await.unwrap_or_default();
    let _ = conn.close().await;
    rows.into_iter().map(|r| r.role_name).collect()
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
/// `ruum_dbhost` may set it.
async fn settings_sql(
    engine: &PostgresEngine,
    conn: &mut PgConnection,
    name: &str,
) -> Result<String, EngineError> {
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
    Ok(sql)
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
/// `CONNECT` on its own database, in one transaction under the budget lock.
async fn make_cluster_role(
    engine: &PostgresEngine,
    database: &str,
    name: &str,
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
            let role = ident(name);
            let mut sql = format!(
                "create role {role} login noinherit nosuperuser nocreatedb nocreaterole \
                   noreplication nobypassrls connection limit {limit} password null;\n"
            );
            sql.push_str(&settings_sql(engine, &mut admin, name).await?);
            run(&mut admin, "create the app role", sql).await?;
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

/// Grants again to every app role of the database (after a migration, a lens sync and a
/// restore in place), so a table that a migration made in another way, or revoked, follows
/// the kinds table. Logs a failure and goes on: the change it follows is committed.
pub(crate) async fn regrant_all(engine: &PostgresEngine, state: &DbState) {
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
    let _gate = engine.enter(&state).await?;
    let _ops = state.ops.lock().await;
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
        if let Err(e) = make_cluster_role(engine, &state.name, &row.role_name).await {
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
        grant(engine, &state, &mut conn, std::slice::from_ref(&row)).await?;
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

/// `app_credentials`.
pub(crate) async fn credentials(
    engine: &PostgresEngine,
    role: &RoleRef,
    valid_for: Duration,
) -> Result<AppCredentials, EngineError> {
    require_app(role)?;
    let state = engine.resolve(role.database()).await?;
    let _gate = engine.enter(&state).await?;
    let _ops = state.ops.lock().await;
    let row = find(engine, &state, role)
        .await?
        .ok_or_else(|| EngineError::InvalidRole("no such app role".to_string()))?;
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
    let valid_until = OffsetDateTime::now_utc() + valid_for.max(Duration::from_secs(1));
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
    sql.push_str(&settings_sql(engine, &mut admin, &row.role_name).await?);
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
    let options = engine.config.connect_options();
    Ok(AppCredentials::new(
        row.role_name,
        state.name.clone(),
        options.get_host().to_string(),
        options.get_port(),
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

/// `suspend_app_role` and `resume_app_role`.
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
        signal(engine, std::slice::from_ref(&row.role_name), None, true).await?;
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

/// Takes every right, default privilege and policy of `names` out of the database of `conn`
/// (a `ruum_dbhost` connection inside a transaction). `owner` is the role that owns the
/// database's objects now: `ruum_dbhost` takes `INHERIT` membership in it and in `names` for this
/// transaction only (granted and revoked here, so a rollback takes the grant back too). `None`:
/// the caller holds the owner's privileges already (the hand-over of a copy). Fails
/// when anything of the database still depends on one of `names`. The caller holds
/// `engine.membership`.
pub(crate) async fn strip_in_tx(
    conn: &mut PgConnection,
    owner: Option<&str>,
    names: &[String],
) -> Result<(), EngineError> {
    if names.is_empty() {
        return Ok(());
    }
    let mut members: Vec<String> = names.iter().map(|n| ident(n)).collect();
    members.extend(owner.map(ident));
    run(
        conn,
        "take the privileges of the owner and the roles",
        format!(
            "grant {} to ruum_dbhost with inherit true, set false",
            members.join(", ")
        ),
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

/// `drop_app_role`.
pub(crate) async fn drop_role(engine: &PostgresEngine, role: &RoleRef) -> Result<(), EngineError> {
    require_app(role)?;
    let state = engine.resolve(role.database()).await?;
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
        signal(engine, std::slice::from_ref(&name), None, true).await?;
        // Its rights in the database (as the owner and as the role itself).
        {
            let _membership = engine.membership.lock().await;
            let mut conn = engine.dbhost(&state.name).await?;
            let r = async {
                run(&mut conn, "begin", "begin".to_string()).await?;
                strip_in_tx(
                    &mut conn,
                    Some(state.meta.role(RoleKind::Owner)),
                    std::slice::from_ref(&name),
                )
                .await?;
                run(&mut conn, "commit", "commit".to_string()).await
            }
            .await;
            let _ = conn.close().await;
            r?;
        }
        // Its rights in the restore points of the database (snapshot databases).
        for snapshot in dependent_databases(engine, &name, &state.name).await? {
            let points = restore_points(engine, &state.name).await?;
            if !points.iter().any(|(_, _, s)| *s == snapshot) {
                return Err(EngineError::Internal(
                    "an app role has rights in a database that is not its own".to_string(),
                ));
            }
            strip_snapshot(engine, &state, &snapshot, &name).await?;
        }
        let mut admin = engine.admin().await?;
        run(
            &mut admin,
            "drop the app role",
            format!(
                "revoke connect, temporary on database {db} from {r};\n\
                 drop role if exists {r}",
                db = ident(&state.name),
                r = ident(&name)
            ),
        )
        .await?;
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
                Some(state.meta.role(RoleKind::Owner)),
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
