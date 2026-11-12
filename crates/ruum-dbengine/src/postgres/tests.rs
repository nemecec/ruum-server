//! The Postgres engine against the data cluster `postgres-data` (`RUUM_DBHOST_URL`): the
//! conformance suite and the security cases of card E-2.
//!
//! Without `RUUM_DBHOST_URL` (or when the cluster does not answer) the tests skip, except in CI
//! (`CI` set) or with `RUUM_DBENGINE_REQUIRE_POSTGRES=1`, where they fail. Databases and roles
//! have random names, so several runs can share one cluster.

mod allow_list;
mod hostile_copy;
mod lens_rebuild;

mod security;
mod spike;
mod tenant_code;
mod tenant_types;

use super::*;
use crate::conformance::{self, Fixture};
use sqlx::Connection;
use std::sync::Mutex;

fn required() -> bool {
    std::env::var_os("CI").is_some() || std::env::var_os("RUUM_DBENGINE_REQUIRE_POSTGRES").is_some()
}

/// The engine of the tests, or `None` (skip) when no cluster is configured outside CI.
pub(super) async fn engine_with(tune: impl FnOnce(&mut PostgresConfig)) -> Option<PostgresEngine> {
    let Some(mut config) = PostgresConfig::from_env() else {
        assert!(
            !required(),
            "RUUM_DBHOST_URL is not set: the Postgres engine tests need the data cluster"
        );
        eprintln!("skipped: RUUM_DBHOST_URL is not set");
        return None;
    };
    tune(&mut config);
    match PostgresEngine::connect(config).await {
        Ok(e) => Some(e),
        Err(e) => {
            assert!(!required(), "the data cluster does not answer: {e}");
            eprintln!("skipped: the data cluster does not answer: {e}");
            None
        }
    }
}

pub(super) async fn engine() -> Option<PostgresEngine> {
    engine_with(|_| {}).await
}

macro_rules! conformance_cases {
    ($($case:ident),* $(,)?) => {
        $(
            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn $case() {
                let Some(engine) = engine().await else { return };
                conformance::$case(&engine).await;
            }
        )*
    };
}

conformance_cases!(
    create,
    drop_database,
    isolation,
    catalog,
    read_only_query,
    write,
    timeout,
    row_cap,
    size_cap,
    transaction_control,
    deferred_constraint,
    migration,
    branch,
    restore_point,
    export,
    tenant_code,
);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_cases_share_one_engine() {
    let Some(engine) = engine().await else { return };
    conformance::run_all(&engine).await;
}

// ------------------------------------------------------------------------------- helpers

/// The key 0x00..0x1f of the test vectors.
pub(super) fn vector_key() -> PseudonymKey {
    PseudonymKey::from_bytes(std::array::from_fn(|i| i as u8))
}

/// A catalog sync that answers fixed classes (by column name; others `normal`).
pub(super) struct ClassSync {
    pub personal: Vec<&'static str>,
    pub secret: Vec<&'static str>,
}

#[async_trait]
impl CatalogSync for ClassSync {
    async fn sync(&self, catalog: &Catalog) -> Result<LensSpec, EngineError> {
        let mut columns = Vec::new();
        for t in &catalog.tables {
            for c in &t.columns {
                let class = if self.personal.contains(&c.name.as_str()) {
                    DataClass::Personal
                } else if self.secret.contains(&c.name.as_str()) {
                    DataClass::Secret
                } else {
                    DataClass::Normal
                };
                columns.push((
                    ColumnRef {
                        schema: t.schema.clone(),
                        table: t.name.clone(),
                        column: c.name.clone(),
                    },
                    class,
                ));
            }
        }
        Ok(LensSpec { columns })
    }
}

/// A session that logs in as the role of `kind` (as the engine does), outside the pool.
pub(super) async fn session(
    engine: &PostgresEngine,
    db: &DatabaseRef,
    kind: RoleKind,
) -> PgConnection {
    let state = engine.resolve(db).await.expect("resolve");
    let pool = engine.pool(&state, kind);
    pool.open(engine).await.expect("log in as the role").0
}

/// The role name of a kind.
pub(super) fn role_name(created: &CreatedDatabase, kind: RoleKind) -> String {
    created
        .roles
        .iter()
        .find(|r| r.kind == kind)
        .unwrap()
        .role_name
        .clone()
}

/// Runs a script (simple protocol) and returns the text of the cells of the last statement
/// that returned rows, one line per row, cells joined with `|` (as `psql -At`).
pub(super) async fn psql(conn: &mut PgConnection, sql: &str) -> Result<String, String> {
    use futures::TryStreamExt;
    use sqlx::Executor;
    let mut out: Vec<String> = Vec::new();
    let mut new_result = true;
    let mut stream = conn.fetch_many(sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_string())));
    loop {
        match stream.try_next().await {
            Ok(Some(sqlx::Either::Left(_))) => new_result = true,
            Ok(Some(sqlx::Either::Right(row))) => {
                if new_result {
                    out.clear();
                    new_result = false;
                }
                let cells: Vec<String> = (0..row.len())
                    .map(|i| {
                        let raw = row.try_get_raw(i).unwrap();
                        if sqlx::ValueRef::is_null(&raw) {
                            String::new()
                        } else {
                            raw.as_str().unwrap_or("").to_string()
                        }
                    })
                    .collect();
                out.push(cells.join("|"));
            }
            Ok(None) => break,
            Err(e) => {
                return Err(match &e {
                    sqlx::Error::Database(db) => {
                        format!("{} ({})", db.message(), db.code().unwrap_or_default())
                    }
                    other => other.to_string(),
                });
            }
        }
    }
    Ok(out.join("\n"))
}

/// Runs a script as the superuser of the data cluster, in database `db` (`docker exec` into the
/// container `DBHOST_CHECK_CONTAINER`, as `scripts/dbhost_check.sh` does), and returns the
/// output of `psql -At`. Only a superuser can make a foreign data wrapper, an event trigger or a
/// type with functions of its own, so the tests of those clauses need it. `None` (skip) when the
/// container is not set, except in CI and with `RUUM_DBENGINE_REQUIRE_POSTGRES`, where it fails.
pub(super) fn superuser(db: &str, sql: &str) -> Option<String> {
    use std::io::Write;
    let Some(container) = std::env::var_os("DBHOST_CHECK_CONTAINER") else {
        assert!(
            !required(),
            "DBHOST_CHECK_CONTAINER is not set: this test needs the superuser of the data cluster"
        );
        eprintln!("skipped: DBHOST_CHECK_CONTAINER is not set");
        return None;
    };
    let mut child = std::process::Command::new("docker")
        .arg("exec")
        .arg("-i")
        .arg(container)
        .args([
            "psql",
            "-X",
            "-q",
            "-At",
            "-v",
            "ON_ERROR_STOP=1",
            "-U",
            "postgres",
            "-d",
        ])
        .arg(db)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("docker exec");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(sql.as_bytes())
        .expect("write the script");
    let out = child.wait_with_output().expect("psql as the superuser");
    assert!(
        out.status.success(),
        "psql as the superuser failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A connection of `ruum_dbhost` to the database (inspection only).
pub(super) async fn dbhost(engine: &PostgresEngine, db: &DatabaseRef) -> PgConnection {
    engine.dbhost(&db.engine_name).await.expect("dbhost")
}

/// Records every untrusted statement the engine runs.
#[derive(Default)]
pub(super) struct Seen(pub Mutex<Vec<UntrustedStatement>>);

pub(super) async fn engine_with_hook() -> Option<(PostgresEngine, Arc<Seen>)> {
    let seen = Arc::new(Seen::default());
    let s = seen.clone();
    let engine = engine().await?.with_statement_hook(move |stmt| {
        s.0.lock().unwrap().push(stmt.clone());
    });
    Some((engine, seen))
}

impl Seen {
    pub fn last_pid(&self) -> i32 {
        self.0
            .lock()
            .unwrap()
            .last()
            .expect("a statement ran")
            .backend_pid
    }
}

pub(super) async fn close(conn: PgConnection) {
    let _ = conn.close().await;
}

/// A fixture with the `users` table and the key of the test vectors.
pub(super) async fn vector_fixture(engine: &PostgresEngine) -> Fixture<'_> {
    let key = vector_key();
    let created = engine
        .create_database(&conformance::new_database(Uuid::now_v7()), &key)
        .await
        .expect("create_database");
    Fixture {
        engine,
        created,
        key,
        key_bytes: std::array::from_fn(|i| i as u8),
    }
}
