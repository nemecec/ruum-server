//! The backend the suites run against: the in-memory services or the Postgres stores.
//!
//! | Variable | Values | What |
//! |---|---|---|
//! | `RUUM_SUITE_BACKEND` | `memory` (default), `postgres` | which stores the suites use |
//! | `RUUM_SUITE_AUTHZ` | `memory` (default), `spicedb` | the authz engine of the Postgres backend |
//! | `RUUM_SUITE_RLS` | `on` (default), `off` | row-level security in the suite databases |
//! | `RUUM_SUITE_JOBS` | a number; default 1 (memory), 8 (Postgres) | worlds that run at the same time |
//!
//! **Postgres.** [`Backend::start`] makes a fresh template database
//! `ruum_suite_<unix time>_<random>_tpl` on the server of `DATABASE_URL`
//! (`eval "$(scripts/dev.sh env)"` on the laptop) and applies `migrations/` to it. The worlds
//! get their databases as copies of it:
//!
//! - [`Backend::app_state`]: one shared database per run (`ruum_suite_<...>_run`). Each world
//!   seeds new tenants, users (unique emails) and resources, so the worlds of a run do not meet.
//!   The permission suites use it.
//! - [`Backend::world`]: a database of its own per world (`ruum_suite_<...>_w<n>`), dropped
//!   with [`WorldDb::finish`]. Only tenants A and B are in it, so any data of another tenant in
//!   an answer is A's data. The isolation suite uses it.
//!
//! Each world gets its own set of Postgres stores, wired as `ruum-server` wires them
//! (`crates/ruum-server/src/wiring.rs`): the journal, audit log, nodes, tags, search, sharing,
//! the content store (uploads, versions, diff) and the identity, tenants and agents area. Chunks
//! stay in one in-memory blob store per run. The authz engine is a new in-memory engine per
//! world (the stores write it through the outbox, after the commit), or the SpiceDB of
//! `RUUM_SPICEDB_PRESHARED_KEY` / `SPICEDB_PRESHARED_KEY` with `RUUM_SUITE_AUTHZ=spicedb`. No
//! maintenance job and no outbox worker run. [`Backend::finish`] drops the databases of the
//! run; databases of runs that ended early are dropped by a later run after [`STALE_SECONDS`].
//!
//! **Row-level security.** The policies of `migrations/` are FORCEd, so they apply to the
//! table owner (the `ruum` role of the laptop stack is no superuser). Then a query that forgets
//! its `tenant_id` filter is still held by the policy. `RUUM_SUITE_RLS=off` turns `FORCE` off
//! in the suite databases: the owner bypasses the policies, and the suites test the tenant
//! filters of the queries alone. The mutation check uses it for the tenant-filter mutations. A
//! superuser (the Postgres service of CI) always bypasses the policies.

use ruum_api::{ApiConfig, AppState};
use ruum_core::Services;
use ruum_core::audit::postgres::PgAuditLog;
use ruum_core::authz::Authz;
use ruum_core::authz::memory::MemoryAuthz;
use ruum_core::authz::spicedb::{SpiceDbAuthz, SpiceDbConfig};
use ruum_core::blobs::memory::MemoryBlobStore;
use ruum_core::blobs::postgres::PgContentStore;
use ruum_core::blobs::{BlobStore, ContentSigner};
use ruum_core::identity::setup::{IdentitySettings, install};
use ruum_core::journal::postgres::PgJournal;
use ruum_core::nodes::NodeStore;
use ruum_core::nodes::postgres::PgNodeStore;
use ruum_core::search::postgres::PgSearch;
use ruum_core::sharing::postgres::PgSharingStore;
use ruum_core::tags::postgres::PgTagStore;
use ruum_core::versions::PluginDiffService;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, Connection, PgConnection, PgPool};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The variable that picks the backend.
pub const BACKEND_ENV: &str = "RUUM_SUITE_BACKEND";
/// The variable that picks the authz engine of the Postgres backend.
pub const AUTHZ_ENV: &str = "RUUM_SUITE_AUTHZ";
/// The variable that turns row-level security off in the suite databases.
pub const RLS_ENV: &str = "RUUM_SUITE_RLS";
/// The variable that sets how many worlds run at the same time.
pub const JOBS_ENV: &str = "RUUM_SUITE_JOBS";
/// The name prefix of the suite databases.
pub const DB_PREFIX: &str = "ruum_suite_";
/// Suite databases older than this (by the time in their name) are dropped at the next start.
pub const STALE_SECONDS: u64 = 2 * 3600;
/// The connections of the pool of the shared database of a run.
const RUN_POOL_SIZE: u32 = 16;
/// The connections of the pool of a world with its own database.
const WORLD_POOL_SIZE: u32 = 3;
/// The default number of worlds that run at the same time on Postgres.
const PG_JOBS: usize = 8;

/// Which stores the suites use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Memory,
    Postgres,
}

impl Kind {
    /// The kind of `RUUM_SUITE_BACKEND`. Panics on an unknown value, so a typo does not run the
    /// suites on the wrong backend.
    pub fn from_env() -> Kind {
        match std::env::var(BACKEND_ENV).as_deref().map(str::trim) {
            Err(_) | Ok("") | Ok("memory") => Kind::Memory,
            Ok("postgres") | Ok("pg") => Kind::Postgres,
            Ok(other) => panic!("{BACKEND_ENV}={other}: use memory or postgres"),
        }
    }
}

/// The Postgres side of a run.
struct Pg {
    /// The connection options of the server (from `DATABASE_URL`).
    server: PgConnectOptions,
    /// The name of the run; the databases are `<name>_tpl`, `<name>_run` and `<name>_w<n>`.
    name: String,
    /// The pool of the shared database, made at the first [`Backend::app_state`].
    run_pool: tokio::sync::OnceCell<PgPool>,
    worlds: AtomicUsize,
    blobs: Arc<dyn BlobStore>,
    /// The shared SpiceDB engine; `None`: a new in-memory engine per world.
    spicedb: Option<Arc<dyn Authz>>,
    rls: bool,
}

/// The backend of one suite run.
pub struct Backend {
    kind: Kind,
    pg: Option<Pg>,
}

/// The database of one world ([`Backend::world`]). Drop it with [`WorldDb::finish`].
pub struct WorldDb {
    server: PgConnectOptions,
    database: String,
    pool: PgPool,
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Backend {
    /// The in-memory backend.
    pub fn memory() -> Backend {
        Backend {
            kind: Kind::Memory,
            pg: None,
        }
    }

    /// The backend of `RUUM_SUITE_BACKEND`. For Postgres: a fresh, migrated template database.
    /// Panics when Postgres is asked for and cannot be set up (no `DATABASE_URL`, no server).
    pub async fn start() -> Backend {
        match Kind::from_env() {
            Kind::Memory => Backend::memory(),
            Kind::Postgres => Backend {
                kind: Kind::Postgres,
                pg: Some(Pg::start().await),
            },
        }
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// A short name for reports: `memory`, `postgres (memory authz)` or
    /// `postgres (spicedb, rls off)`.
    pub fn name(&self) -> String {
        match &self.pg {
            None => "memory".into(),
            Some(pg) => {
                let mut extra = vec![if pg.spicedb.is_some() {
                    "spicedb"
                } else {
                    "memory authz"
                }];
                if !pg.rls {
                    extra.push("rls off");
                }
                format!("postgres ({})", extra.join(", "))
            }
        }
    }

    /// How many worlds the suites run at the same time (`RUUM_SUITE_JOBS`). Each world has its
    /// own services and tenants, so the order of the checks does not matter; the report keeps
    /// the order of the operations.
    pub fn jobs(&self) -> usize {
        var(JOBS_ENV)
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(match self.kind {
                Kind::Memory => 1,
                Kind::Postgres => PG_JOBS,
            })
    }

    /// The services of a new world in the shared database of the run. Memory: a new
    /// `AppState::in_memory()`. Postgres: new stores over the run's database; the world's
    /// tenants are new, so worlds do not meet.
    pub async fn app_state(&self) -> AppState {
        match &self.pg {
            None => AppState::in_memory(),
            Some(pg) => {
                let pool = pg
                    .run_pool
                    .get_or_init(|| async {
                        let db = pg.copy("run").await;
                        pg.pool(&db, RUN_POOL_SIZE).await
                    })
                    .await;
                AppState::new(pg.services(pool), ApiConfig::default())
            }
        }
    }

    /// The services of a new world with a database of its own (Postgres) or new in-memory
    /// services. Call [`WorldDb::finish`] on the database when the world is done.
    pub async fn world(&self) -> (AppState, Option<WorldDb>) {
        match &self.pg {
            None => (AppState::in_memory(), None),
            Some(pg) => {
                let n = pg.worlds.fetch_add(1, Ordering::Relaxed);
                let database = pg.copy(&format!("w{n}")).await;
                let pool = pg.pool(&database, WORLD_POOL_SIZE).await;
                let state = AppState::new(pg.services(&pool), ApiConfig::default());
                let db = WorldDb {
                    server: pg.server.clone(),
                    database,
                    pool,
                };
                (state, Some(db))
            }
        }
    }

    /// Ends the run: closes the pools and drops the databases of the run.
    pub async fn finish(self) {
        if let Some(pg) = self.pg {
            pg.finish().await;
        }
    }
}

impl WorldDb {
    /// Closes the pool and drops the database of the world.
    pub async fn finish(self) {
        self.pool.close().await;
        drop_database(&self.server, &self.database).await;
    }
}

impl Pg {
    async fn start() -> Pg {
        let url = var("DATABASE_URL").unwrap_or_else(|| {
            panic!(
                "{BACKEND_ENV}=postgres needs DATABASE_URL (eval \"$(scripts/dev.sh env)\" on the laptop)"
            )
        });
        let server = PgConnectOptions::from_str(&url).expect("DATABASE_URL is a Postgres URL");
        let mut admin = server
            .connect()
            .await
            .unwrap_or_else(|e| panic!("connect to DATABASE_URL: {e}"));
        drop_stale(&mut admin).await;
        let _ = admin.close().await;
        let name = format!(
            "{DB_PREFIX}{}_{}",
            now_secs(),
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let rls = !matches!(var(RLS_ENV).as_deref(), Some("off") | Some("0"));

        // The template: migrated, then closed, so it can be copied.
        let template = format!("{name}_tpl");
        create_database(&server, &template, None).await;
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(server.clone().database(&template))
            .await
            .unwrap_or_else(|e| panic!("connect to {template}: {e}"));
        let migrations = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
        sqlx::migrate::Migrator::new(migrations.as_path())
            .await
            .expect("read migrations")
            .run(&pool)
            .await
            .unwrap_or_else(|e| panic!("migrate {template}: {e}"));
        if !rls {
            no_force_rls(&pool).await;
        }
        pool.close().await;

        let spicedb: Option<Arc<dyn Authz>> = match var(AUTHZ_ENV).as_deref() {
            None | Some("memory") => None,
            Some("spicedb") => {
                let cfg = SpiceDbConfig::from_env().unwrap_or_else(|| {
                    panic!("{AUTHZ_ENV}=spicedb needs RUUM_SPICEDB_PRESHARED_KEY or SPICEDB_PRESHARED_KEY")
                });
                Some(Arc::new(
                    SpiceDbAuthz::connect(cfg)
                        .await
                        .unwrap_or_else(|e| panic!("connect to SpiceDB: {e}")),
                ))
            }
            Some(other) => panic!("{AUTHZ_ENV}={other}: use memory or spicedb"),
        };
        eprintln!(
            "suite backend: postgres, databases {name}_*, authz {}, rls {}",
            if spicedb.is_some() {
                "spicedb"
            } else {
                "memory"
            },
            if rls { "on" } else { "off" }
        );
        Pg {
            server,
            name,
            run_pool: tokio::sync::OnceCell::new(),
            worlds: AtomicUsize::new(0),
            blobs: Arc::new(MemoryBlobStore::default()),
            spicedb,
            rls,
        }
    }

    /// Makes `<name>_<suffix>` as a copy of the template and returns its name.
    async fn copy(&self, suffix: &str) -> String {
        let database = format!("{}_{suffix}", self.name);
        let template = format!("{}_tpl", self.name);
        create_database(&self.server, &database, Some(&template)).await;
        database
    }

    async fn pool(&self, database: &str, size: u32) -> PgPool {
        PgPoolOptions::new()
            .max_connections(size)
            .connect_with(self.server.clone().database(database))
            .await
            .unwrap_or_else(|e| panic!("connect to {database}: {e}"))
    }

    /// The Postgres services of one world over `pool`, as `ruum-server` wires them, without
    /// the maintenance job and the outbox worker.
    fn services(&self, pool: &PgPool) -> Services {
        let mut s = Services::in_memory();
        s.authz = match &self.spicedb {
            Some(engine) => engine.clone(),
            None => Arc::new(MemoryAuthz::default()),
        };
        // Every request reads at least as fresh as its tenant's newest applied outbox token
        // (decision 0002-4), as in ruum-server.
        s.freshness = Arc::new(ruum_core::authz::outbox::PgFreshness::new(pool.clone()));
        s.journal = Arc::new(PgJournal::new(pool.clone()));
        let nodes: Arc<dyn NodeStore> = Arc::new(PgNodeStore::new(pool.clone(), s.authz.clone()));
        s.nodes = nodes.clone();
        s.tags = Arc::new(PgTagStore::new(
            pool.clone(),
            s.authz.clone(),
            nodes.clone(),
        ));
        s.search = Arc::new(PgSearch::new(pool.clone(), s.authz.clone(), nodes.clone()));
        s.audit = Arc::new(PgAuditLog::new(pool.clone()));
        s.sharing = Arc::new(PgSharingStore::new(
            pool.clone(),
            s.authz.clone(),
            nodes.clone(),
        ));
        let content = Arc::new(PgContentStore::new(
            pool.clone(),
            s.authz.clone(),
            nodes.clone(),
            self.blobs.clone(),
            ContentSigner::random(),
        ));
        s.blobs = self.blobs.clone();
        s.transfers = content.clone();
        s.versions = content.clone();
        s.diffs = Arc::new(PluginDiffService::new(nodes, content.clone(), content));
        let settings = IdentitySettings {
            public_url: ApiConfig::default().public_url,
            ..IdentitySettings::default()
        };
        install(&mut s, &settings, Some(pool.clone())).expect("install the identity area");
        s
    }

    async fn finish(self) {
        if let Some(pool) = self.run_pool.get() {
            pool.close().await;
        }
        let names: Vec<String> = match self.server.connect().await {
            Ok(mut admin) => {
                let names = sqlx::query_scalar(
                    "select datname::text from pg_database where datname like $1",
                )
                .bind(format!("{}\\_%", self.name))
                .fetch_all(&mut admin)
                .await
                .unwrap_or_default();
                let _ = admin.close().await;
                names
            }
            Err(_) => Vec::new(),
        };
        for n in names {
            drop_database(&self.server, &n).await;
        }
    }
}

/// Creates `database`, as a copy of `template` when given.
async fn create_database(server: &PgConnectOptions, database: &str, template: Option<&str>) {
    let mut admin = server
        .connect()
        .await
        .unwrap_or_else(|e| panic!("connect to DATABASE_URL: {e}"));
    let sql = match template {
        Some(t) => format!("create database \"{database}\" template \"{t}\""),
        None => format!("create database \"{database}\""),
    };
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
        .execute(&mut admin)
        .await
        .unwrap_or_else(|e| panic!("create database {database}: {e}"));
    let _ = admin.close().await;
}

async fn drop_database(server: &PgConnectOptions, database: &str) {
    match server.connect().await {
        Ok(mut admin) => {
            let sql = format!("drop database if exists \"{database}\" with (force)");
            if let Err(e) = sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
                .execute(&mut admin)
                .await
            {
                eprintln!("drop database {database}: {e}");
            }
            let _ = admin.close().await;
        }
        Err(e) => eprintln!("drop database {database}: {e}"),
    }
}

/// Drops the suite databases whose name time is older than [`STALE_SECONDS`].
async fn drop_stale(admin: &mut PgConnection) {
    let names: Vec<String> =
        sqlx::query_scalar("select datname::text from pg_database where datname like $1")
            .bind(format!("{DB_PREFIX}%"))
            .fetch_all(&mut *admin)
            .await
            .unwrap_or_default();
    let cutoff = now_secs().saturating_sub(STALE_SECONDS);
    for name in names {
        let born = name
            .strip_prefix(DB_PREFIX)
            .and_then(|rest| rest.split('_').next())
            .and_then(|t| t.parse::<u64>().ok());
        if born.is_some_and(|t| t < cutoff) {
            let sql = format!("drop database if exists \"{name}\" with (force)");
            let _ = sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
                .execute(&mut *admin)
                .await;
        }
    }
}

/// Turns `FORCE ROW LEVEL SECURITY` off on every table, so the owner bypasses the policies.
async fn no_force_rls(pool: &PgPool) {
    let tables: Vec<String> = sqlx::query_scalar(
        "select c.relname::text from pg_class c join pg_namespace n on n.oid = c.relnamespace \
         where n.nspname = current_schema() and c.relkind = 'r' and c.relforcerowsecurity",
    )
    .fetch_all(pool)
    .await
    .expect("list the tables with forced row-level security");
    for t in tables {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "alter table \"{t}\" no force row level security"
        )))
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("no force row level security on {t}: {e}"));
    }
}
