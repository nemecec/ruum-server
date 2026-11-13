//! Migrations of the platform database (design 12.7, decision 42; backlog O-25).
//!
//! In a cluster, migrations run as their own step before the rollout: `ruum-server migrate`
//! in a Kubernetes Job with the new image. It takes a Postgres advisory lock first, so two
//! steps never migrate at the same time, and exits. The serving pods run with
//! `RUUM_MIGRATE=false` and only check at start that the database has every migration of their
//! release ([`check`]); a pod of a new image never serves on an old schema.
//!
//! On a laptop the server still migrates at start (`RUUM_MIGRATE` is on by default), under the
//! same lock.
//!
//! In a cluster the migration step logs in as the database owner and the serving pods as a
//! runtime role without DDL rights (Kimi review of O-25, finding 5). After each run the step
//! takes every write right on `_sqlx_migrations` from the runtime role
//! ([`restrict_runtime_role`]), so a compromised server cannot fake the migration records.

use anyhow::{Context as _, bail};
use sqlx::migrate::Migrator;
use sqlx::{Connection as _, PgConnection, PgPool};
use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

/// The key of Ruum's advisory lock for migrations ("ruum-mig" in ASCII).
pub const LOCK_KEY: i64 = 0x7275_756d_2d6d_6967;
/// How long a migration step waits for another one that holds the lock.
pub const LOCK_WAIT: Duration = Duration::from_secs(600);
/// How often a waiting step asks for the lock again.
const LOCK_POLL: Duration = Duration::from_millis(500);

/// What the process does with the platform database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// Run the pending migrations (`ruum-server migrate`, or a laptop with `RUUM_MIGRATE` on).
    Migrate,
    /// Only check that the database has this release's migrations (`RUUM_MIGRATE=false`).
    Check,
    /// No database: in-memory services. Only on a laptop (`RUUM_MIGRATE` on).
    Memory,
}

/// Chooses the [`Plan`]. `RUUM_MIGRATE=false` means a cluster, where in-memory services would
/// serve empty, divergent state: without `DATABASE_URL` the server then refuses to start (fail
/// closed; Kimi review of O-25, finding 6).
pub fn plan(has_database_url: bool, migrate_only: bool, migrate: bool) -> anyhow::Result<Plan> {
    match (has_database_url, migrate_only, migrate) {
        (true, true, _) | (true, false, true) => Ok(Plan::Migrate),
        (true, false, false) => Ok(Plan::Check),
        (false, true, _) => bail!("ruum-server migrate needs DATABASE_URL"),
        (false, false, false) => bail!(
            "RUUM_MIGRATE=false (a cluster) but DATABASE_URL is missing or empty; refusing to \
             start with in-memory services"
        ),
        (false, false, true) => Ok(Plan::Memory),
    }
}

/// Reads the migrations of this release from `dir`.
///
/// Migrations in the database that this release does not know are allowed: going back one
/// release runs the older image's migration step on the newer schema, and the schema rule
/// (design 12.7) makes that safe. The step then has nothing to do.
pub async fn load(dir: &Path) -> anyhow::Result<Migrator> {
    let mut migrator = Migrator::new(dir)
        .await
        .with_context(|| format!("read the migrations in {}", dir.display()))?;
    migrator.set_ignore_missing(true);
    Ok(migrator)
}

/// Runs the pending migrations under the advisory lock [`LOCK_KEY`]. Waits up to `wait` for
/// another step that holds the lock. Returns the number of migrations it applied.
///
/// The lock lives on a connection of its own (detached from the pool), so it ends with that
/// connection also when a migration fails or the process dies.
pub async fn run(pool: &PgPool, migrator: &Migrator, wait: Duration) -> anyhow::Result<usize> {
    let mut conn = pool
        .acquire()
        .await
        .context("connect for the migrations")?
        .detach();
    let start = Instant::now();
    let mut told = false;
    loop {
        let (got,): (bool,) = sqlx::query_as("SELECT pg_try_advisory_lock($1)")
            .bind(LOCK_KEY)
            .fetch_one(&mut conn)
            .await
            .context("ask for the migration lock")?;
        if got {
            break;
        }
        if start.elapsed() >= wait {
            bail!(
                "another migration step held the advisory lock for {} seconds; not migrating",
                wait.as_secs()
            );
        }
        if !told {
            tracing::info!("waiting for another migration step (advisory lock)");
            told = true;
        }
        tokio::time::sleep(LOCK_POLL).await;
    }
    let result = async {
        let todo = pending(&mut conn, migrator).await?.len();
        migrator
            .run(&mut conn)
            .await
            .context("run the migrations")?;
        anyhow::Ok(todo)
    }
    .await;
    // Closing the connection releases the lock in any case; unlock first to say so clearly.
    let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(LOCK_KEY)
        .execute(&mut conn)
        .await;
    let _ = conn.close().await;
    result
}

/// The versions of this release's migrations that the database does not have. Fails when an
/// earlier migration failed half-way (sqlx marks it `success = false`).
pub async fn pending(conn: &mut PgConnection, migrator: &Migrator) -> anyhow::Result<Vec<i64>> {
    let (exists,): (bool,) = sqlx::query_as("SELECT to_regclass('_sqlx_migrations') IS NOT NULL")
        .fetch_one(&mut *conn)
        .await
        .context("look for the migrations table")?;
    let applied: Vec<(i64, bool)> = if exists {
        sqlx::query_as("SELECT version, success FROM _sqlx_migrations")
            .fetch_all(&mut *conn)
            .await
            .context("read the applied migrations")?
    } else {
        Vec::new()
    };
    if let Some((version, _)) = applied.iter().find(|(_, success)| !success) {
        bail!("migration {version} failed earlier and left the database dirty; repair it by hand");
    }
    let done: HashSet<i64> = applied.into_iter().map(|(version, _)| version).collect();
    Ok(migrator
        .iter()
        .filter(|m| !m.migration_type.is_down_migration())
        .map(|m| m.version)
        .filter(|version| !done.contains(version))
        .collect())
}

/// Takes every write right on `_sqlx_migrations` from `role` (the runtime role of the serving
/// pods) and leaves it `SELECT`, which the start-up [`check`] needs. Then fails when `role`
/// could still change the schema: a member of the current (owner) role, `CREATE` on the
/// database or on the schema `public`, or a write right on `_sqlx_migrations`. Run by the
/// migration step after [`run`], as the owner of the tables.
pub async fn restrict_runtime_role(pool: &PgPool, role: &str) -> anyhow::Result<()> {
    let valid = !role.is_empty()
        && role.len() <= 63
        && role
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && role
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !valid {
        bail!("RUUM_DB_RUNTIME_ROLE must be a plain Postgres role name ([a-z_][a-z0-9_]*)");
    }
    let mut conn = pool
        .acquire()
        .await
        .context("connect to restrict the runtime role")?;
    let (exists,): (bool,) =
        sqlx::query_as("SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = $1)")
            .bind(role)
            .fetch_one(&mut *conn)
            .await
            .context("look for the runtime role")?;
    if !exists {
        bail!("the runtime role {role} does not exist; apply the platform first (k8s/apply.sh)");
    }
    // Before any change: a REVOKE from the owner itself would take the step's own rights.
    let (member,): (bool,) = sqlx::query_as("SELECT pg_has_role($1, current_user, 'MEMBER')")
        .bind(role)
        .fetch_one(&mut *conn)
        .await
        .context("check the memberships of the runtime role")?;
    if member {
        bail!(
            "the runtime role {role} is the migration role or a member of it; it would have its DDL rights"
        );
    }
    // The name is checked above; quoting keeps it one identifier.
    for statement in [
        format!(
            "REVOKE INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER ON TABLE _sqlx_migrations FROM \"{role}\""
        ),
        format!("GRANT SELECT ON TABLE _sqlx_migrations TO \"{role}\""),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(statement))
            .execute(&mut *conn)
            .await
            .with_context(|| format!("restrict the rights of {role} on _sqlx_migrations"))?;
    }
    let (create_db, create_schema, write_records): (bool, bool, bool) =
        sqlx::query_as(
            "SELECT has_database_privilege($1, current_database(), 'CREATE'), \
                    has_schema_privilege($1, 'public', 'CREATE'), \
                    has_table_privilege($1, '_sqlx_migrations', 'INSERT, UPDATE, DELETE, TRUNCATE')",
        )
        .bind(role)
        .fetch_one(&mut *conn)
        .await
        .context("check the rights of the runtime role")?;
    if create_db || create_schema {
        bail!(
            "the runtime role {role} may create objects (CREATE on the database or the schema public)"
        );
    }
    if write_records {
        bail!(
            "the runtime role {role} may still write _sqlx_migrations (a grant through another role?)"
        );
    }
    Ok(())
}

/// The start-up check of a serving pod (`RUUM_MIGRATE=false`): the database must have every
/// migration of this release, else the pod does not start (and never becomes ready).
pub async fn check(pool: &PgPool, migrator: &Migrator) -> anyhow::Result<()> {
    let mut conn = pool
        .acquire()
        .await
        .context("connect for the schema check")?;
    let missing = pending(&mut conn, migrator).await?;
    if !missing.is_empty() {
        bail!(
            "the database lacks the migrations {missing:?} of this release; run `ruum-server \
             migrate` (the migration Job of deploy-chart.sh) before the rollout"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migrations_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations")
    }

    /// A copy of the release's migrations plus one more, as the next release would have.
    fn next_release(extra_sql: &str) -> tempdir::Dir {
        let dir = tempdir::Dir::new();
        for entry in std::fs::read_dir(migrations_dir()).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), dir.path().join(entry.file_name())).unwrap();
        }
        std::fs::write(dir.path().join("9999_next_release.sql"), extra_sql).unwrap();
        dir
    }

    #[sqlx::test(migrations = false)]
    async fn the_step_migrates_once_and_the_check_then_passes(pool: PgPool) {
        let migrator = load(&migrations_dir()).await.unwrap();
        // A serving pod refuses an empty database.
        let err = check(&pool, &migrator).await.unwrap_err().to_string();
        assert!(err.contains("ruum-server migrate"), "{err}");
        let all = migrator.iter().count();
        assert_eq!(run(&pool, &migrator, LOCK_WAIT).await.unwrap(), all);
        check(&pool, &migrator).await.unwrap();
        // A second run has nothing to do.
        assert_eq!(run(&pool, &migrator, LOCK_WAIT).await.unwrap(), 0);
    }

    #[sqlx::test(migrations = false)]
    async fn parallel_steps_both_succeed_and_apply_each_migration_once(pool: PgPool) {
        let migrator = load(&migrations_dir()).await.unwrap();
        let (a, b) = tokio::join!(
            run(&pool, &migrator, LOCK_WAIT),
            run(&pool, &migrator, LOCK_WAIT)
        );
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_eq!(a + b, migrator.iter().count(), "applied {a} and {b}");
        check(&pool, &migrator).await.unwrap();
    }

    #[sqlx::test(migrations = false)]
    async fn a_held_lock_stops_the_step_after_the_wait(pool: PgPool) {
        let migrator = load(&migrations_dir()).await.unwrap();
        let mut holder = pool.acquire().await.unwrap().detach();
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(LOCK_KEY)
            .execute(&mut holder)
            .await
            .unwrap();
        let err = run(&pool, &migrator, Duration::from_millis(300))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("advisory lock"), "{err}");
        // Nothing ran while the lock was held.
        let mut conn = pool.acquire().await.unwrap();
        assert_eq!(
            pending(&mut conn, &migrator).await.unwrap().len(),
            migrator.iter().count()
        );
        holder.close().await.unwrap();
        run(&pool, &migrator, LOCK_WAIT).await.unwrap();
    }

    /// The rollout order of design 12.7: the new release's step runs while the old release
    /// serves; the old release passes its check on the new schema, and going back one release
    /// (the old image's step on the new schema) is a no-op, not an error.
    #[sqlx::test(migrations = false)]
    async fn the_previous_release_runs_on_the_new_schema(pool: PgPool) {
        let old = load(&migrations_dir()).await.unwrap();
        run(&pool, &old, LOCK_WAIT).await.unwrap();
        let next = next_release("CREATE TABLE o25_probe (id bigint PRIMARY KEY);");
        let new = load(next.path()).await.unwrap();
        // The new image refuses to serve before its step ran.
        let err = check(&pool, &new).await.unwrap_err().to_string();
        assert!(err.contains("9999"), "{err}");
        assert_eq!(run(&pool, &new, LOCK_WAIT).await.unwrap(), 1);
        check(&pool, &new).await.unwrap();
        // The old release still passes, and its step (a rollback) changes nothing.
        check(&pool, &old).await.unwrap();
        assert_eq!(run(&pool, &old, LOCK_WAIT).await.unwrap(), 0);
    }

    #[sqlx::test(migrations = false)]
    async fn a_failed_migration_leaves_no_lock_and_is_reported(pool: PgPool) {
        let migrator = load(&migrations_dir()).await.unwrap();
        run(&pool, &migrator, LOCK_WAIT).await.unwrap();
        let next = next_release("SELECT * FROM o25_no_such_table;");
        let broken = load(next.path()).await.unwrap();
        assert!(run(&pool, &broken, LOCK_WAIT).await.is_err());
        // The lock is free again: another step gets it at once.
        let mut conn = pool.acquire().await.unwrap();
        let (free,): (bool,) = sqlx::query_as("SELECT pg_try_advisory_lock($1)")
            .bind(LOCK_KEY)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert!(free);
        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(LOCK_KEY)
            .execute(&mut *conn)
            .await
            .unwrap();
        // The new image does not serve without its migration.
        assert!(check(&pool, &broken).await.is_err());
    }

    #[test]
    fn the_plan_fails_closed_in_a_cluster_without_a_database() {
        assert_eq!(plan(true, true, false).unwrap(), Plan::Migrate);
        assert_eq!(plan(true, false, true).unwrap(), Plan::Migrate);
        assert_eq!(plan(true, false, false).unwrap(), Plan::Check);
        // Only a laptop (RUUM_MIGRATE on) may fall back to in-memory services.
        assert_eq!(plan(false, false, true).unwrap(), Plan::Memory);
        let err = plan(false, false, false).unwrap_err().to_string();
        assert!(err.contains("RUUM_MIGRATE=false"), "{err}");
        assert!(err.contains("DATABASE_URL"), "{err}");
        assert!(plan(false, true, true).is_err());
        assert!(plan(false, true, false).is_err());
    }

    /// The tests connect as a role without CREATEROLE, so they use predefined roles that every
    /// Postgres 15+ has: `pg_monitor` stands for a runtime role (no table rights, no CREATE).
    #[sqlx::test(migrations = false)]
    async fn the_step_takes_the_write_rights_on_the_records_from_the_runtime_role(pool: PgPool) {
        let migrator = load(&migrations_dir()).await.unwrap();
        run(&pool, &migrator, LOCK_WAIT).await.unwrap();
        // As the default privileges of the owner would grant it on a fresh cluster.
        sqlx::query("GRANT SELECT, INSERT, UPDATE, DELETE ON _sqlx_migrations TO pg_monitor")
            .execute(&pool)
            .await
            .unwrap();
        restrict_runtime_role(&pool, "pg_monitor").await.unwrap();
        let (write, read): (bool, bool) = sqlx::query_as(
            "SELECT has_table_privilege('pg_monitor', '_sqlx_migrations', 'INSERT, UPDATE, DELETE, TRUNCATE'), \
                    has_table_privilege('pg_monitor', '_sqlx_migrations', 'SELECT')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!write, "the runtime role can still write _sqlx_migrations");
        assert!(read, "the start-up check needs SELECT on _sqlx_migrations");
        // A second run changes nothing and passes.
        restrict_runtime_role(&pool, "pg_monitor").await.unwrap();
    }

    #[sqlx::test(migrations = false)]
    async fn the_step_refuses_a_runtime_role_that_could_change_the_schema(pool: PgPool) {
        let migrator = load(&migrations_dir()).await.unwrap();
        run(&pool, &migrator, LOCK_WAIT).await.unwrap();
        let (me,): (String,) = sqlx::query_as("SELECT current_user::text")
            .fetch_one(&pool)
            .await
            .unwrap();
        // The migration role itself: it owns the tables.
        let err = restrict_runtime_role(&pool, &me)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("is the migration role"), "{err}");
        // Write rights through a role membership that a REVOKE on the table cannot remove.
        let err = restrict_runtime_role(&pool, "pg_write_all_data")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("may still write _sqlx_migrations"), "{err}");
        // CREATE on the schema public (pg_database_owner owns it since Postgres 15).
        let err = restrict_runtime_role(&pool, "pg_database_owner")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("may create objects"), "{err}");
        let err = restrict_runtime_role(&pool, "ruum_no_such_role_o25")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not exist"), "{err}");
        for bad in ["", "Ruum", "ruum app", "ruum\"; drop table x; --", "1ruum"] {
            let err = restrict_runtime_role(&pool, bad)
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("plain Postgres role name"), "{bad}: {err}");
        }
    }

    /// A temporary folder without a new dependency.
    mod tempdir {
        pub struct Dir(std::path::PathBuf);
        impl Dir {
            pub fn new() -> Self {
                let path = std::env::temp_dir().join(format!(
                    "ruum-migrate-test-{}-{}",
                    std::process::id(),
                    uuid_like()
                ));
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }
            pub fn path(&self) -> &std::path::Path {
                &self.0
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        fn uuid_like() -> u128 {
            use std::sync::atomic::{AtomicU64, Ordering};
            static N: AtomicU64 = AtomicU64::new(0);
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            t ^ u128::from(N.fetch_add(1, Ordering::Relaxed))
        }
    }
}
