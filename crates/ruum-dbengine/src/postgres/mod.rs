//! The Postgres engine on the shared data cluster (card E-2; `docs/STEP2.md` "The engine
//! security model", Postgres; decisions 0004-1, 4, 14, 25, 27, 29, 30, 32 and 34).
//!
//! - **Names.** Databases are `d_` and roles `r_` plus 16 random hex characters (never a node
//!   or tenant ID). Each database has the schema `ruum_meta` (owner `ruum_dbhost`, no grant):
//!   the tenant and node it belongs to, its roles, its restore points and the classes of its
//!   lens. Every call checks the reference against it, so another tenant's reference finds
//!   nothing.
//! - **Two kinds of sessions.** `ruum_dbhost` (`CREATEDB`, `CREATEROLE`, no superuser) runs
//!   platform DDL only. Every untrusted statement (a query, a statement of a migration's `up`)
//!   runs in a session that LOGS IN as the per-database role of the request (`pool.rs`), in its
//!   own transaction with `SET LOCAL` options (`session.rs`). An optional hook
//!   ([`PostgresEngine::with_statement_hook`]) sees `session_user` before every untrusted
//!   statement.
//! - **Lens.** `lens.rs`: the key in `ruum_mask.key`, one pseudonym overload per type, and the
//!   lens views, all owned by the database's `mask` role (`ruum_dbhost` holds `SET`-only
//!   membership in it).
//! - **Copies.** `copy.rs`: `CREATE DATABASE ... TEMPLATE` after the drain of the source
//!   (decision 0004-27), the hand-over of the copy to its own roles, the masking of a masked
//!   copy with the source's key, and restore points (a snapshot database per restore point).
//! - **Export.** `export.rs`: `pg_dump` (the binary in `RUUM_PG_DUMP`, default `pg_dump`; it
//!   must be a PostgreSQL 17 client) without the Ruum schemas, roles and grants, and the import
//!   of such an export into a new database.

mod catalog;
mod copy;
mod errors;
mod export;
pub mod lens;
mod pool;
mod session;
mod setup;
mod split;
#[cfg(test)]
mod tests;
mod values;

use crate::DatabaseEngine;
use crate::error::EngineError;
use crate::key::PseudonymKey;
use crate::model::*;
use crate::role::{RoleKind, RoleRef};
use async_trait::async_trait;
use errors::internal;
use pool::RolePool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, PgConnection, PgPool, Row};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use uuid::Uuid;

pub use session::UntrustedStatement;

/// The settings of the Postgres engine.
#[derive(Clone)]
pub struct PostgresConfig {
    /// The `ruum_dbhost` URL of the data cluster (`RUUM_DBHOST_URL`), with the admin database
    /// (`postgres`).
    pub dbhost_url: String,
    /// The cluster ID in `databases.cluster` (`RUUM_DATA_CLUSTER`, default `dev`).
    pub cluster: String,
    /// The `pg_dump` binary (`RUUM_PG_DUMP`, default `pg_dump`): a PostgreSQL 17 client.
    pub pg_dump: String,
    /// How long a drain waits for running statements before it cancels them (decision 0004-27:
    /// 5 seconds).
    pub drain_wait: Duration,
    /// How long a request waits for a paused database (or a free connection) before
    /// `database_not_ready` (decision 0004-27: 10 seconds).
    pub request_wait: Duration,
    /// How long a role password is valid (`VALID UNTIL`).
    pub password_validity: Duration,
    /// How long a pooled connection lives; shorter than `password_validity`.
    pub connection_lifetime: Duration,
    /// The most connections of one role at a time.
    pub max_connections_per_role: usize,
}

impl std::fmt::Debug for PostgresConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresConfig")
            .field("cluster", &self.cluster)
            .field("pg_dump", &self.pg_dump)
            .finish_non_exhaustive()
    }
}

impl PostgresConfig {
    /// The settings with the defaults of the decisions.
    pub fn new(dbhost_url: impl Into<String>) -> PostgresConfig {
        PostgresConfig {
            dbhost_url: dbhost_url.into(),
            cluster: "dev".to_string(),
            pg_dump: "pg_dump".to_string(),
            drain_wait: Duration::from_secs(5),
            request_wait: Duration::from_secs(10),
            password_validity: Duration::from_secs(15 * 60),
            connection_lifetime: Duration::from_secs(10 * 60),
            max_connections_per_role: 8,
        }
    }

    /// The settings from the environment: `RUUM_DBHOST_URL` (required), `RUUM_DATA_CLUSTER`,
    /// `RUUM_PG_DUMP`.
    pub fn from_env() -> Option<PostgresConfig> {
        let url = std::env::var("RUUM_DBHOST_URL")
            .ok()
            .filter(|u| !u.is_empty())?;
        let mut config = PostgresConfig::new(url);
        if let Ok(c) = std::env::var("RUUM_DATA_CLUSTER")
            && !c.is_empty()
        {
            config.cluster = c;
        }
        if let Ok(p) = std::env::var("RUUM_PG_DUMP")
            && !p.is_empty()
        {
            config.pg_dump = p;
        }
        Some(config)
    }

    /// The connection options of the cluster (host, port, TLS) with no statement cache: every
    /// statement is sent unnamed, so `DISCARD ALL` never leaves sqlx with a stale one.
    pub(crate) fn connect_options(&self) -> PgConnectOptions {
        PgConnectOptions::from_str(&self.dbhost_url)
            .expect("RUUM_DBHOST_URL was checked when the engine started")
            .statement_cache_capacity(0)
            .application_name("ruum-dbengine")
            .disable_statement_logging()
    }

    /// The connection options of a `ruum_dbhost` session. Its `search_path` is `pg_catalog,
    /// pg_temp`, set at log-in, so it wins over a default of the database or the role. A
    /// tenant may create objects in `public`, and with `public` on the path a platform query
    /// would pick a tenant's operator or function over the one of `pg_catalog` when its argument
    /// types match better (`oid = integer`, `name !~ name`; the CVE-2018-1058 kind). That
    /// code would run with `ruum_dbhost`'s `CREATEROLE` and its `ADMIN` on the roles of every
    /// database (re-review of E-2). The sessions of the per-database roles keep the path of
    /// their kind ([`pool::search_path`]).
    pub(crate) fn dbhost_options(&self) -> PgConnectOptions {
        self.connect_options()
            .options([("search_path", DBHOST_SEARCH_PATH)])
    }
}

/// The `search_path` of every `ruum_dbhost` session: no schema a tenant can write to.
pub(crate) const DBHOST_SEARCH_PATH: &str = "pg_catalog, pg_temp";

/// The meta data of a database in `ruum_meta`.
#[derive(Debug, Clone)]
pub(crate) struct Meta {
    pub tenant_id: Uuid,
    pub node_id: Uuid,
    pub roles: Vec<EngineRole>,
}

impl Meta {
    pub fn role(&self, kind: RoleKind) -> &str {
        self.roles
            .iter()
            .find(|r| r.kind == kind && r.binding.is_none())
            .map(|r| r.role_name.as_str())
            .expect("a database has a role of every kind")
    }
}

/// The pools of a database, per role (kind, binding).
type Pools = HashMap<(RoleKind, Option<Uuid>), Arc<RolePool>>;

/// The statement hook of the tests.
type Hook = Arc<dyn Fn(&UntrustedStatement) + Send + Sync>;

/// The in-process state of one database.
pub(crate) struct DbState {
    pub name: String,
    pub meta: Meta,
    /// Requests and platform calls hold it shared; a drain holds it exclusively (decision
    /// 0004-27).
    pub gate: tokio::sync::RwLock<()>,
    /// Serializes the platform calls that change the database (migrations, lens, copies,
    /// restore points, drop).
    pub ops: tokio::sync::Mutex<()>,
    /// Held exclusively while the lens views are gone (a migration); the lens kinds wait on it
    /// (decision 0004-34 f).
    pub lens: tokio::sync::RwLock<()>,
    /// The lens views are gone and were not rebuilt (a failed catalog sync): the lens kinds
    /// fail closed with `database_not_ready` until the next successful `sync_lens`.
    pub lens_down: AtomicBool,
    pub dropped: AtomicBool,
    pools: crate::lock::Mutex<Pools>,
}

impl DbState {
    fn new(name: String, meta: Meta) -> DbState {
        DbState {
            name,
            meta,
            gate: tokio::sync::RwLock::new(()),
            ops: tokio::sync::Mutex::new(()),
            lens: tokio::sync::RwLock::new(()),
            lens_down: AtomicBool::new(false),
            dropped: AtomicBool::new(false),
            pools: Default::default(),
        }
    }

    /// The pool of a role of this database.
    pub fn pool(&self, kind: RoleKind, binding: Option<Uuid>, max: usize) -> Arc<RolePool> {
        let mut pools = self.pools.lock();
        pools
            .entry((kind, binding))
            .or_insert_with(|| Arc::new(RolePool::new(&self.name, self.meta.role(kind), kind, max)))
            .clone()
    }

    pub fn all_pools(&self) -> Vec<Arc<RolePool>> {
        self.pools.lock().values().cloned().collect()
    }
}

/// The Postgres engine. Cheap to share (`Arc`).
pub struct PostgresEngine {
    pub(crate) config: PostgresConfig,
    admin: PgPool,
    dbs: crate::lock::Mutex<HashMap<String, Arc<DbState>>>,
    hook: Option<Hook>,
    /// Tests only: let transaction control past the splitter, to show the second line.
    #[cfg(test)]
    pub(crate) allow_transaction_control: AtomicBool,
}

/// Whether a name is a neutral database name (`d_` + 16 hex).
pub(crate) fn is_db_name(name: &str) -> bool {
    name.strip_prefix("d_").is_some_and(|rest| {
        rest.len() == 16
            && rest
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}

/// A neutral name: `prefix` + 16 random hex characters.
pub(crate) fn neutral_name(prefix: &str) -> String {
    format!("{prefix}{}", hex::encode(rand::random::<[u8; 8]>()))
}

impl PostgresEngine {
    /// Connects to the data cluster as `ruum_dbhost`.
    pub async fn connect(config: PostgresConfig) -> Result<PostgresEngine, EngineError> {
        PgConnectOptions::from_str(&config.dbhost_url)
            .map_err(|e| EngineError::Internal(format!("RUUM_DBHOST_URL: {e}")))?;
        let admin = PgPoolOptions::new()
            .max_connections(4)
            .min_connections(0)
            .idle_timeout(Duration::from_secs(60))
            .connect_with(config.dbhost_options())
            .await
            .map_err(|e| internal("connect to the data cluster as ruum_dbhost", e))?;
        Ok(PostgresEngine {
            config,
            admin,
            dbs: Default::default(),
            hook: None,
            #[cfg(test)]
            allow_transaction_control: AtomicBool::new(false),
        })
    }

    /// Calls `hook` before every untrusted statement with the session it runs in (the tests
    /// assert that `session_user` is the role of the request, never `ruum_dbhost`).
    pub fn with_statement_hook(
        mut self,
        hook: impl Fn(&UntrustedStatement) + Send + Sync + 'static,
    ) -> PostgresEngine {
        self.hook = Some(Arc::new(hook));
        self
    }

    pub fn config(&self) -> &PostgresConfig {
        &self.config
    }

    /// A connection of `ruum_dbhost` to the admin database.
    pub(crate) async fn admin(
        &self,
    ) -> Result<sqlx::pool::PoolConnection<sqlx::Postgres>, EngineError> {
        self.admin
            .acquire()
            .await
            .map_err(|e| internal("connect as ruum_dbhost", e))
    }

    /// A new connection of `ruum_dbhost` to the database `name` (platform DDL). The caller
    /// closes it.
    pub(crate) async fn dbhost(&self, name: &str) -> Result<PgConnection, EngineError> {
        self.config
            .dbhost_options()
            .database(name)
            .connect()
            .await
            .map_err(|e| match errors::sqlstate(&e).as_deref() {
                Some("3D000") => EngineError::NotFound,
                Some("55000") => EngineError::NotReady,
                _ => internal("connect to the database as ruum_dbhost", e),
            })
    }

    /// The state of a database, after checking that the reference matches it.
    pub(crate) async fn resolve(&self, db: &DatabaseRef) -> Result<Arc<DbState>, EngineError> {
        if db.cluster != self.config.cluster || !is_db_name(&db.engine_name) {
            return Err(EngineError::NotFound);
        }
        let cached = self.dbs.lock().get(&db.engine_name).cloned();
        let state = match cached {
            Some(s) => s,
            None => {
                let mut conn = self.dbhost(&db.engine_name).await?;
                let meta = match setup::read_meta(&mut conn).await {
                    // A database made before the cluster set-up revoked set_config from
                    // PUBLIC is refused until the set-up has run (review Q-1, S-1).
                    Ok(Some(meta)) => setup::check_set_config(&mut conn, &meta.roles)
                        .await
                        .map(|()| Some(meta)),
                    other => other,
                };
                let _ = sqlx::Connection::close(conn).await;
                let Some(meta) = meta? else {
                    return Err(EngineError::NotFound);
                };
                let state = Arc::new(DbState::new(db.engine_name.clone(), meta));
                self.dbs
                    .lock()
                    .entry(db.engine_name.clone())
                    .or_insert(state)
                    .clone()
            }
        };
        if state.meta.tenant_id != db.tenant_id || state.meta.node_id != db.node_id {
            return Err(EngineError::NotFound);
        }
        if state.dropped.load(Ordering::SeqCst) {
            return Err(EngineError::NotFound);
        }
        Ok(state)
    }

    pub(crate) fn remember(&self, state: Arc<DbState>) {
        self.dbs.lock().insert(state.name.clone(), state);
    }

    pub(crate) fn forget(&self, name: &str) {
        self.dbs.lock().remove(name);
    }

    /// Waits for the shared gate of a database (a paused one makes the caller wait up to
    /// `request_wait`, then `database_not_ready`).
    pub(crate) async fn enter<'a>(
        &self,
        state: &'a DbState,
    ) -> Result<tokio::sync::RwLockReadGuard<'a, ()>, EngineError> {
        let guard = tokio::time::timeout(self.config.request_wait, state.gate.read())
            .await
            .map_err(|_| EngineError::NotReady)?;
        if state.dropped.load(Ordering::SeqCst) {
            return Err(EngineError::NotFound);
        }
        Ok(guard)
    }

    pub(crate) fn pool(&self, state: &DbState, kind: RoleKind) -> Arc<RolePool> {
        state.pool(kind, None, self.config.max_connections_per_role)
    }

    /// The size of a database in bytes.
    async fn database_size(&self, name: &str) -> Result<u64, EngineError> {
        let mut admin = self.admin().await?;
        let row = sqlx::query("select pg_catalog.pg_database_size($1::text)::int8")
            .persistent(false)
            .bind(name)
            .fetch_one(&mut *admin)
            .await
            .map_err(|e| internal("read the database size", e))?;
        Ok(row.get::<i64, _>(0).max(0) as u64)
    }
}

#[async_trait]
impl DatabaseEngine for PostgresEngine {
    fn kind(&self) -> EngineKind {
        EngineKind::Postgres
    }

    async fn create_database(
        &self,
        new: &NewDatabase,
        key: &PseudonymKey,
    ) -> Result<CreatedDatabase, EngineError> {
        setup::create_database(self, new, key).await
    }

    async fn drop_database(&self, db: &DatabaseRef) -> Result<(), EngineError> {
        copy::drop_database(self, db).await
    }

    async fn catalog(&self, db: &DatabaseRef) -> Result<Catalog, EngineError> {
        let state = self.resolve(db).await?;
        let _gate = self.enter(&state).await?;
        let mut conn = self.dbhost(&state.name).await?;
        let out = catalog::read(&mut conn).await;
        let _ = sqlx::Connection::close(conn).await;
        out
    }

    async fn size_bytes(&self, db: &DatabaseRef) -> Result<u64, EngineError> {
        let state = self.resolve(db).await?;
        let _gate = self.enter(&state).await?;
        self.database_size(&state.name).await
    }

    async fn execute(
        &self,
        role: &RoleRef,
        _key: &PseudonymKey,
        request: &StatementRequest,
    ) -> Result<StatementOutcome, EngineError> {
        session::execute(self, role, request).await
    }

    async fn apply_migration(
        &self,
        db: &DatabaseRef,
        _key: &PseudonymKey,
        request: &MigrationRequest,
        sync: &dyn CatalogSync,
    ) -> Result<MigrationOutcome, EngineError> {
        session::apply_migration(self, db, request, sync).await
    }

    async fn sync_lens(
        &self,
        db: &DatabaseRef,
        _key: &PseudonymKey,
        sync: &dyn CatalogSync,
    ) -> Result<Catalog, EngineError> {
        let state = self.resolve(db).await?;
        let _gate = self.enter(&state).await?;
        let _ops = state.ops.lock().await;
        let _lens = state.lens.write().await;
        let catalog = setup::sync_and_rebuild(self, &state, sync, false).await?;
        state.lens_down.store(false, Ordering::SeqCst);
        Ok(catalog)
    }

    async fn copy_database(
        &self,
        source: &DatabaseRef,
        source_key: &PseudonymKey,
        new: &NewDatabase,
        data: &CopyData,
        key: &PseudonymKey,
    ) -> Result<CreatedDatabase, EngineError> {
        copy::copy_database(self, source, source_key, new, data, key).await
    }

    async fn create_restore_point(
        &self,
        db: &DatabaseRef,
    ) -> Result<RestorePointMark, EngineError> {
        copy::create_restore_point(self, db).await
    }

    async fn restore_into_new(
        &self,
        source: &DatabaseRef,
        at: &RestorePointMark,
        new: &NewDatabase,
        key: &PseudonymKey,
    ) -> Result<CreatedDatabase, EngineError> {
        copy::restore_into_new(self, source, at, new, key).await
    }

    async fn restore_in_place(
        &self,
        db: &DatabaseRef,
        at: &RestorePointMark,
        key: &PseudonymKey,
    ) -> Result<(), EngineError> {
        copy::restore_in_place(self, db, at, key).await
    }

    async fn export(&self, db: &DatabaseRef, format: ExportFormat) -> Result<Export, EngineError> {
        export::export(self, db, format).await
    }
}
