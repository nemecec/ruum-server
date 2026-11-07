//! The connection pool of one role (database, kind, binding; decision 0004-32).
//!
//! Every connection LOGS IN as the per-database role with a short-lived password (decision
//! 0004-1): the password is random, set by `ruum_dbhost` with `VALID UNTIL`, kept in memory only,
//! and connections are closed before it ends. Before the pool opens connections with a new
//! password, `ruum_dbhost` resets the role (`ALTER ROLE ... RESET ALL`, the fixed `search_path`,
//! the password; decision 0004-34 c), and after each login the engine checks that the role has
//! no other setting, else it closes the connection and resets the role again.
//!
//! A connection goes back to the pool only after `DISCARD ALL`, and only when its request ended
//! its transaction cleanly; any other connection is closed (decision 0004-14).

use super::PostgresEngine;
use super::errors::{internal, sqlstate};
use super::lens::{ident, literal};
use crate::error::EngineError;
use crate::role::RoleKind;
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection, Row};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use time::OffsetDateTime;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// The `search_path` of a kind: its lens schema and `extensions` (decision 0004-34 g).
pub(crate) fn search_path(kind: RoleKind) -> &'static str {
    match kind {
        RoleKind::Owner | RoleKind::SecretRead => "public, extensions",
        RoleKind::Read | RoleKind::Write => "ruum_nosecret, extensions",
        RoleKind::MaskedRead | RoleKind::MaskedWrite => "ruum_masked, extensions",
        RoleKind::Mask => "pg_catalog",
    }
}

/// The password of a role, as the pool knows it.
struct Creds {
    password: String,
    valid_until: OffsetDateTime,
}

struct Idle {
    conn: PgConnection,
    opened: Instant,
    generation: u64,
}

/// The pool of one role.
pub(crate) struct RolePool {
    pub database: String,
    pub role_name: String,
    pub kind: RoleKind,
    idle: crate::lock::Mutex<Vec<Idle>>,
    creds: tokio::sync::Mutex<Option<Creds>>,
    permits: Arc<Semaphore>,
    /// Raised by a drain: a connection of an older generation is closed, not pooled.
    generation: AtomicU64,
}

/// A connection taken from a pool. [`Lease::release`] puts it back after `DISCARD ALL`;
/// [`Lease::close`] (or dropping the lease) closes it.
pub(crate) struct Lease {
    conn: Option<PgConnection>,
    opened: Instant,
    generation: u64,
    pool: Arc<RolePool>,
    pub backend_pid: i32,
    _permit: OwnedSemaphorePermit,
}

impl Lease {
    pub fn conn(&mut self) -> &mut PgConnection {
        self.conn.as_mut().expect("a lease holds its connection")
    }

    /// Puts the connection back after `DISCARD ALL` (or closes it when that fails or the pool
    /// was drained since it was opened).
    pub async fn release(mut self, engine: &PostgresEngine) {
        let Some(mut conn) = self.conn.take() else {
            return;
        };
        let fresh = self.opened.elapsed() < engine.config.connection_lifetime
            && self.generation == self.pool.generation.load(Ordering::SeqCst);
        if !fresh {
            let _ = conn.close().await;
            return;
        }
        match sqlx::raw_sql("DISCARD ALL").execute(&mut conn).await {
            Ok(_) => self.pool.idle.lock().push(Idle {
                conn,
                opened: self.opened,
                generation: self.generation,
            }),
            Err(_) => {
                let _ = conn.close_hard().await;
            }
        }
    }

    /// Closes the connection (its transaction failed or ended, or its state is unknown).
    pub async fn close(mut self) {
        if let Some(conn) = self.conn.take() {
            let _ = conn.close_hard().await;
        }
    }
}

impl RolePool {
    pub fn new(database: &str, role_name: &str, kind: RoleKind, max: usize) -> RolePool {
        RolePool {
            database: database.to_string(),
            role_name: role_name.to_string(),
            kind,
            idle: Default::default(),
            creds: tokio::sync::Mutex::new(None),
            permits: Arc::new(Semaphore::new(max)),
            generation: AtomicU64::new(0),
        }
    }

    /// Closes every idle connection; connections in use are closed when they come back.
    pub async fn close_all(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        let idle: Vec<Idle> = std::mem::take(&mut *self.idle.lock());
        for i in idle {
            let _ = i.conn.close().await;
        }
    }

    /// The number of idle connections (tests).
    #[cfg(test)]
    pub fn idle_count(&self) -> usize {
        self.idle.lock().len()
    }

    /// A connection of this role: an idle one, or a new one.
    pub async fn acquire(self: &Arc<Self>, engine: &PostgresEngine) -> Result<Lease, EngineError> {
        let permit = tokio::time::timeout(
            engine.config.request_wait,
            self.permits.clone().acquire_owned(),
        )
        .await
        .map_err(|_| EngineError::NotReady)?
        .map_err(|_| EngineError::NotReady)?;
        let generation = self.generation.load(Ordering::SeqCst);
        loop {
            let idle = self.idle.lock().pop();
            let Some(idle) = idle else { break };
            if idle.generation == generation
                && idle.opened.elapsed() < engine.config.connection_lifetime
            {
                let mut conn = idle.conn;
                let pid = match sqlx::query("select pg_catalog.pg_backend_pid()")
                    .persistent(false)
                    .fetch_one(&mut conn)
                    .await
                {
                    Ok(row) => row.get::<i32, _>(0),
                    Err(_) => {
                        let _ = conn.close_hard().await;
                        continue;
                    }
                };
                return Ok(Lease {
                    conn: Some(conn),
                    opened: idle.opened,
                    generation,
                    pool: self.clone(),
                    backend_pid: pid,
                    _permit: permit,
                });
            }
            let _ = idle.conn.close().await;
        }
        let (conn, pid) = self.open(engine).await?;
        Ok(Lease {
            conn: Some(conn),
            opened: Instant::now(),
            generation,
            pool: self.clone(),
            backend_pid: pid,
            _permit: permit,
        })
    }

    /// Opens a connection that is not pooled (a drain's cancel, `pg_dump`, tests). The caller
    /// closes it.
    pub async fn open(&self, engine: &PostgresEngine) -> Result<(PgConnection, i32), EngineError> {
        let mut last = None;
        for attempt in 0..3 {
            let password = self.password(engine, attempt > 0).await?;
            let options = self.options(engine, &password);
            let mut conn = match PgConnection::connect_with(&options).await {
                Ok(c) => c,
                Err(e) => {
                    // The role changed its own password (decision 0004-34 c): reset it.
                    if sqlstate(&e).as_deref() == Some("28P01") {
                        last = Some(internal("log in as the role", e));
                        continue;
                    }
                    return Err(internal("log in as the role", e));
                }
            };
            match self.check_settings(&mut conn).await {
                Ok((true, pid)) => return Ok((conn, pid)),
                Ok((false, _)) => {
                    // The role has settings of its own: close, reset, log in again.
                    tracing::warn!(role = %self.role_name, "role settings changed; reset");
                    let _ = conn.close().await;
                    last = Some(EngineError::Internal(
                        "the role has settings the platform did not set".to_string(),
                    ));
                }
                Err(e) => {
                    let _ = conn.close_hard().await;
                    return Err(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| EngineError::Internal("cannot log in as the role".into())))
    }

    /// The connection options of the role, with `password`.
    pub fn options(&self, engine: &PostgresEngine, password: &str) -> PgConnectOptions {
        engine
            .config
            .connect_options()
            .username(&self.role_name)
            .password(password)
            .database(&self.database)
    }

    /// The current password, after a reset of the role when there is none, when it ends too
    /// soon, or when `force` is set.
    pub async fn password(
        &self,
        engine: &PostgresEngine,
        force: bool,
    ) -> Result<String, EngineError> {
        let mut creds = self.creds.lock().await;
        let now = OffsetDateTime::now_utc();
        let margin = engine.config.connection_lifetime + std::time::Duration::from_secs(60);
        if !force
            && let Some(c) = creds.as_ref()
            && c.valid_until - now > margin
        {
            return Ok(c.password.clone());
        }
        let password = hex::encode(rand::random::<[u8; 24]>());
        let valid_until = now + engine.config.password_validity;
        let until = valid_until
            .format(&time::macros::format_description!(
                "[year]-[month]-[day] [hour]:[minute]:[second]+00"
            ))
            .map_err(|e| EngineError::Internal(e.to_string()))?;
        let role = ident(&self.role_name);
        let sql = format!(
            "alter role {role} reset all; \
             alter role {role} in database {db} reset all; \
             alter role {role} set search_path = {sp}; \
             alter role {role} password {pw} valid until {until}",
            db = ident(&self.database),
            sp = search_path(self.kind),
            pw = literal(&password),
            until = literal(&until),
        );
        let mut admin = engine.admin().await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
            .execute(&mut *admin)
            .await
            .map_err(|e| internal("reset the role", e))?;
        *creds = Some(Creds {
            password: password.clone(),
            valid_until,
        });
        Ok(password)
    }

    /// After login: the role has only the platform's settings (decision 0004-34 c). Returns
    /// whether it does, and the backend PID.
    async fn check_settings(&self, conn: &mut PgConnection) -> Result<(bool, i32), EngineError> {
        let row = sqlx::query(
            "select coalesce((select pg_catalog.array_agg(s order by s)
                                from pg_catalog.pg_db_role_setting r,
                                     pg_catalog.unnest(r.setconfig) s
                               where r.setrole = (select oid from pg_catalog.pg_roles
                                                   where rolname = current_user)
                                  or r.setdatabase = (select oid from pg_catalog.pg_database
                                                       where datname = pg_catalog.current_database())),
                             '{}'::text[]) as settings,
                    pg_catalog.pg_backend_pid() as pid",
        )
        .persistent(false)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| internal("check the role settings", e))?;
        let settings: Vec<String> = row.get("settings");
        let expected = vec![format!("search_path={}", search_path(self.kind))];
        Ok((settings == expected, row.get("pid")))
    }
}
