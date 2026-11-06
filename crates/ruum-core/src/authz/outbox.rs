//! The outbox of relationship writes (table `authz_outbox`, migration 0001).
//!
//! Postgres is the source of truth, SpiceDB holds the same facts as relationships. A write that
//! changes relationships must never leave the two apart, so it goes in this order:
//!
//! 1. In the Postgres transaction of the change, [`enqueue`] stores the relationship updates.
//! 2. After the commit, [`apply`] sends them to SpiceDB ([`super::Authz::write`]). Touch and
//!    delete are idempotent, so a retry is safe. It stores the returned ZedToken and sets
//!    `applied_at`. [`apply_after_commit`] does this on a new connection and leaves a failure to
//!    the worker.
//! 3. A background worker ([`run_worker`], [`apply_pending`]) applies rows that step 2 did not
//!    finish (crash, SpiceDB down), oldest first per tenant.
//!
//! [`apply`] applies the pending rows of the tenant in `id` order up to the given row, under a
//! row lock, so two requests of one tenant never apply their updates out of order.
//!
//! Read-after-write: the request that wrote passes the ZedToken of step 2 to its later checks;
//! other requests use [`latest_token`] of the tenant (`at_least_as_fresh`).
//!
//! Failures and the revocation window: a write commits and returns success even when step 2
//! fails (it retries a few times first, [`AFTER_COMMIT_RETRIES`]). Until the worker applies the
//! row, the engine still has the old relationships: a removed grant or a revoked share link
//! stays effective, and share-link access tokens issued before keep working (they live at most
//! [`crate::sharing::rules::LINK_ACCESS_TOKEN_SECONDS`]). Rows of one tenant apply in order, so
//! a row that fails every time (for example the engine refuses it because of the schema) holds
//! back all later rows of its tenant. Each failure is recorded on the row that failed
//! (`attempts`, `last_error`); from [`STUCK_ATTEMPTS`] attempts on, every failure is logged at
//! error level, and [`pending_stats`] gives the count and the age of the oldest pending row for
//! health checks and alerts. A dead-letter state needs a column in `authz_outbox` (a migration).
//!
//! Row-level security: [`enqueue`] runs in the caller's transaction, which has set
//! `ruum.tenant_id`. The functions that take a pool set `ruum.system = 'on'` (worker) or
//! `ruum.tenant_id` (one tenant) on their own transactions.

use super::{Authz, ConsistencyToken, RelationshipUpdate};
use crate::error::{Error, Result};
use crate::ids::TenantId;
use sqlx::{PgConnection, PgPool, Row};
use std::time::Duration;

/// How many times [`apply_after_commit`] tries again (after 50 ms, then 200 ms) before it leaves
/// the rows to the worker.
pub const AFTER_COMMIT_RETRIES: usize = 2;

/// From this many failed attempts on one row, the failures are logged at error level (an alert):
/// the row holds back the later rows of its tenant.
pub const STUCK_ATTEMPTS: i32 = 10;

/// Stores relationship updates in the caller's transaction. Returns the outbox row ID.
pub async fn enqueue(
    conn: &mut PgConnection,
    tenant: TenantId,
    updates: &[RelationshipUpdate],
) -> Result<i64> {
    let json = serde_json::to_value(updates).map_err(|e| Error::Internal(e.to_string()))?;
    let id: i64 = sqlx::query_scalar(
        "insert into authz_outbox (tenant_id, updates) values ($1, $2) returning id",
    )
    .bind(tenant.0)
    .bind(json)
    .fetch_one(conn)
    .await?;
    Ok(id)
}

/// Applies one outbox row to the engine and marks it applied. Returns the ZedToken. Pending rows
/// of the same tenant with a smaller `id` are applied first. A row that is already applied
/// returns its stored token.
///
/// Call it inside a transaction (it locks the rows until the commit). On an engine error the
/// row stays pending and the error is returned; record it with [`record_failure`] on another
/// transaction, because the caller's transaction will roll back.
pub async fn apply(
    conn: &mut PgConnection,
    authz: &dyn Authz,
    tenant: TenantId,
    id: i64,
) -> Result<ConsistencyToken> {
    apply_rows(conn, authz, tenant, id)
        .await
        .map_err(|f| f.error)
}

/// A failed [`apply_rows`]: the row that the engine refused (if the engine failed) and the error.
struct ApplyFailure {
    row: Option<i64>,
    error: Error,
}

impl From<Error> for ApplyFailure {
    fn from(error: Error) -> Self {
        Self { row: None, error }
    }
}

impl From<sqlx::Error> for ApplyFailure {
    fn from(e: sqlx::Error) -> Self {
        Error::from(e).into()
    }
}

async fn apply_rows(
    conn: &mut PgConnection,
    authz: &dyn Authz,
    tenant: TenantId,
    id: i64,
) -> std::result::Result<ConsistencyToken, ApplyFailure> {
    let rows = sqlx::query(
        "select id, updates, applied_at is not null as applied, zed_token from authz_outbox \
         where tenant_id = $1 and id <= $2 and (applied_at is null or id = $2) \
         order by id for update",
    )
    .bind(tenant.0)
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let mut token = None;
    for row in rows {
        let row_id: i64 = row.get("id");
        if row.get::<bool, _>("applied") {
            token = row
                .get::<Option<String>, _>("zed_token")
                .map(ConsistencyToken);
            continue;
        }
        let updates: Vec<RelationshipUpdate> = serde_json::from_value(row.get("updates"))
            .map_err(|e| Error::Internal(format!("authz outbox row {row_id}: bad updates: {e}")))?;
        let t = authz.write(updates).await.map_err(|error| ApplyFailure {
            row: Some(row_id),
            error,
        })?;
        sqlx::query(
            "update authz_outbox set applied_at = now(), zed_token = $3, \
             attempts = attempts + 1, last_error = null where tenant_id = $1 and id = $2",
        )
        .bind(tenant.0)
        .bind(row_id)
        .bind(&t.0)
        .execute(&mut *conn)
        .await?;
        token = Some(t);
    }
    token.ok_or_else(|| Error::not_found("authz outbox row").into())
}

/// Records a failed attempt on a pending row. Returns the attempts of the row so far (`None`
/// when the row is not pending).
pub async fn record_failure(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: i64,
    error: &str,
) -> Result<Option<i32>> {
    Ok(sqlx::query_scalar(
        "update authz_outbox set attempts = attempts + 1, last_error = $3 \
         where tenant_id = $1 and id = $2 and applied_at is null returning attempts",
    )
    .bind(tenant.0)
    .bind(id)
    .bind(error)
    .fetch_optional(conn)
    .await?)
}

/// Records a failure of [`apply_rows`] on its own transaction: on the row that the engine
/// refused, or on `fallback` when the failure was not the engine's. Logs it, at error level from
/// [`STUCK_ATTEMPTS`] on.
async fn note_failure(pool: &PgPool, tenant: TenantId, fallback: i64, f: &ApplyFailure) {
    let row = f.row.unwrap_or(fallback);
    let msg = f.error.to_string();
    let attempts = async {
        let mut tx = pool.begin().await?;
        set_tenant(&mut tx, tenant).await?;
        let n = record_failure(&mut tx, tenant, row, &msg).await?;
        tx.commit().await?;
        Ok::<_, Error>(n)
    }
    .await;
    match attempts {
        Ok(Some(n)) if n >= STUCK_ATTEMPTS => tracing::error!(
            %tenant, outbox_id = row, attempts = n, error = %msg,
            "authz outbox row is stuck; it holds back the later rows of its tenant"
        ),
        Ok(n) => tracing::warn!(
            %tenant, outbox_id = row, attempts = ?n, error = %msg,
            "authz outbox apply failed; the worker retries"
        ),
        Err(e) => tracing::warn!(
            %tenant, outbox_id = row, error = %msg, record_error = %e,
            "authz outbox apply failed and the failure could not be recorded"
        ),
    }
}

/// Applies outbox rows right after the commit of the write that enqueued them, on new
/// transactions of `pool`. Tries again [`AFTER_COMMIT_RETRIES`] times, so a short engine hiccup
/// does not leave a revocation to the worker. Returns the ZedToken of the last row, or `None`
/// when the engine failed: the rows stay pending for the worker, and the failure is logged and
/// recorded on the row that failed.
pub async fn apply_after_commit(
    pool: &PgPool,
    authz: &dyn Authz,
    tenant: TenantId,
    ids: &[i64],
) -> Option<ConsistencyToken> {
    let last = *ids.iter().max()?;
    let mut failure = None;
    for attempt in 0..=AFTER_COMMIT_RETRIES {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(if attempt == 1 { 50 } else { 200 })).await;
        }
        let res = async {
            let mut tx = pool.begin().await?;
            set_tenant(&mut tx, tenant).await?;
            let t = apply_rows(&mut tx, authz, tenant, last).await?;
            tx.commit().await?;
            Ok::<_, ApplyFailure>(t)
        }
        .await;
        match res {
            Ok(t) => return Some(t),
            Err(f) => failure = Some(f),
        }
    }
    if let Some(f) = failure {
        note_failure(pool, tenant, last, &f).await;
    }
    None
}

async fn set_tenant(conn: &mut PgConnection, tenant: TenantId) -> Result<()> {
    sqlx::query("select set_config('ruum.tenant_id', $1, true)")
        .bind(tenant.0.to_string())
        .execute(conn)
        .await?;
    Ok(())
}

/// The newest applied ZedToken of the tenant, for read-after-write consistency.
pub async fn latest_token(
    conn: &mut PgConnection,
    tenant: TenantId,
) -> Result<Option<ConsistencyToken>> {
    let t: Option<String> = sqlx::query_scalar(
        "select zed_token from authz_outbox where tenant_id = $1 and applied_at is not null \
         order by id desc limit 1",
    )
    .bind(tenant.0)
    .fetch_optional(conn)
    .await?
    .flatten();
    Ok(t.map(ConsistencyToken))
}

/// The pending rows of all tenants, for health checks, metrics and alerts.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct PendingStats {
    /// Rows not applied yet.
    pub count: i64,
    /// The age of the oldest pending row in seconds (0 when nothing is pending). Alert when it
    /// stays above a few seconds: revocations are not effective in the engine yet.
    pub oldest_age_seconds: f64,
    /// The most failed attempts of one pending row (a row at [`STUCK_ATTEMPTS`] or more is stuck).
    pub max_attempts: i32,
}

/// The [`PendingStats`] of the outbox (system mode).
pub async fn pending_stats(pool: &PgPool) -> Result<PendingStats> {
    let mut tx = pool.begin().await?;
    sqlx::query("set local ruum.system = 'on'")
        .execute(&mut *tx)
        .await?;
    let (count, oldest_age_seconds, max_attempts): (i64, f64, i32) = sqlx::query_as(
        "select count(*), \
         coalesce(extract(epoch from now() - min(created_at)), 0)::float8, \
         coalesce(max(attempts), 0) \
         from authz_outbox where applied_at is null",
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(PendingStats {
        count,
        oldest_age_seconds,
        max_attempts,
    })
}

/// The number of rows that are not applied yet (all tenants). For health checks and tests.
pub async fn pending_count(pool: &PgPool) -> Result<i64> {
    let mut tx = pool.begin().await?;
    sqlx::query("set local ruum.system = 'on'")
        .execute(&mut *tx)
        .await?;
    let n: i64 = sqlx::query_scalar("select count(*) from authz_outbox where applied_at is null")
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(n)
}

/// Applies up to `limit` pending rows, oldest first, tenant by tenant. Returns how many rows
/// were applied. A tenant whose rows fail is skipped until the next round (its later rows wait,
/// to keep the order).
pub async fn apply_pending(pool: &PgPool, authz: &dyn Authz, limit: i64) -> Result<usize> {
    let mut tx = pool.begin().await?;
    sqlx::query("set local ruum.system = 'on'")
        .execute(&mut *tx)
        .await?;
    // The newest pending row of each tenant among the oldest `limit` rows.
    let heads: Vec<(uuid::Uuid, i64, i64)> = sqlx::query_as(
        "select tenant_id, max(id), count(*) from \
         (select tenant_id, id from authz_outbox where applied_at is null order by id limit $1) p \
         group by tenant_id",
    )
    .bind(limit)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut applied = 0usize;
    for (tenant, last, n) in heads {
        let tenant = TenantId(tenant);
        let res = async {
            let mut tx = pool.begin().await?;
            set_tenant(&mut tx, tenant).await?;
            apply_rows(&mut tx, authz, tenant, last).await?;
            tx.commit().await?;
            Ok::<_, ApplyFailure>(())
        }
        .await;
        match res {
            Ok(()) => applied += n as usize,
            Err(f) => note_failure(pool, tenant, last, &f).await,
        }
    }
    Ok(applied)
}

/// The background worker: applies pending rows every `every`, forever. Spawn it once per server.
pub async fn run_worker(pool: PgPool, authz: std::sync::Arc<dyn Authz>, every: Duration) {
    let mut tick = tokio::time::interval(every);
    loop {
        tick.tick().await;
        loop {
            match apply_pending(&pool, authz.as_ref(), 500).await {
                Ok(n) if n >= 500 => continue,
                Ok(_) => break,
                Err(e) => {
                    tracing::warn!(error = %e, "authz outbox worker failed");
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::memory::MemoryAuthz;
    use super::super::{ObjectRef, Relationship, SubjectRef};
    use super::*;

    /// Makes a tenant row (no root node: the outbox needs only the tenant).
    pub(crate) async fn tenant(pool: &PgPool) -> TenantId {
        let t = TenantId::new();
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("set local ruum.system = 'on'")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query(
            "insert into tenants (id, name, kind, created_kind, created_by) \
             values ($1, 'test', 'personal', 'user', $2)",
        )
        .bind(t.0)
        .bind(uuid::Uuid::now_v7())
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        t
    }

    /// A transaction in one tenant (row-level security).
    async fn ttx(pool: &PgPool, t: TenantId) -> sqlx::Transaction<'static, sqlx::Postgres> {
        let mut tx = pool.begin().await.unwrap();
        set_tenant(&mut tx, t).await.unwrap();
        tx
    }

    fn viewer(node: &str, user: &str) -> RelationshipUpdate {
        RelationshipUpdate::Touch(Relationship {
            object: ObjectRef::new("node", node),
            relation: "viewer".into(),
            subject: SubjectRef::object(ObjectRef::new("user", user)),
        })
    }

    async fn sees(authz: &MemoryAuthz, node: &str, user: &str) -> bool {
        authz
            .check(
                &SubjectRef::object(ObjectRef::new("user", user)),
                "view",
                &ObjectRef::new("node", node),
                None,
            )
            .await
            .unwrap()
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn enqueue_then_apply(pool: PgPool) {
        let t = tenant(&pool).await;
        let authz = MemoryAuthz::default();
        let mut tx = ttx(&pool, t).await;
        let id = enqueue(&mut tx, t, &[viewer("n1", "u1")]).await.unwrap();
        tx.commit().await.unwrap();
        assert!(!sees(&authz, "n1", "u1").await);
        assert_eq!(pending_count(&pool).await.unwrap(), 1);
        let mut conn = ttx(&pool, t).await;
        assert_eq!(latest_token(&mut conn, t).await.unwrap(), None);
        drop(conn);

        let token = apply_after_commit(&pool, &authz, t, &[id]).await.unwrap();
        assert!(sees(&authz, "n1", "u1").await);
        assert_eq!(pending_count(&pool).await.unwrap(), 0);
        let mut conn = ttx(&pool, t).await;
        assert_eq!(
            latest_token(&mut conn, t).await.unwrap(),
            Some(token.clone())
        );
        // Applying again returns the stored token and writes nothing.
        let again = apply(&mut conn, &authz, t, id).await.unwrap();
        assert_eq!(again, token);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_rolled_back_write_leaves_nothing(pool: PgPool) {
        let t = tenant(&pool).await;
        let mut tx = ttx(&pool, t).await;
        enqueue(&mut tx, t, &[viewer("n1", "u1")]).await.unwrap();
        tx.rollback().await.unwrap();
        assert_eq!(pending_count(&pool).await.unwrap(), 0);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn the_worker_applies_in_order(pool: PgPool) {
        let t1 = tenant(&pool).await;
        let t2 = tenant(&pool).await;
        let authz = MemoryAuthz::default();
        let mut tx1 = ttx(&pool, t1).await;
        enqueue(&mut tx1, t1, &[viewer("a", "u")]).await.unwrap();
        tx1.commit().await.unwrap();
        let mut tx2 = ttx(&pool, t2).await;
        enqueue(&mut tx2, t2, &[viewer("b", "u")]).await.unwrap();
        tx2.commit().await.unwrap();
        let del = match viewer("a", "u") {
            RelationshipUpdate::Touch(r) => RelationshipUpdate::Delete(r),
            d => d,
        };
        let mut tx1 = ttx(&pool, t1).await;
        enqueue(&mut tx1, t1, &[del]).await.unwrap();
        tx1.commit().await.unwrap();
        assert_eq!(apply_pending(&pool, &authz, 100).await.unwrap(), 3);
        // Touch then delete, in this order: the tuple is gone.
        assert!(!sees(&authz, "a", "u").await);
        assert!(sees(&authz, "b", "u").await);
        assert_eq!(pending_count(&pool).await.unwrap(), 0);
        assert_eq!(apply_pending(&pool, &authz, 100).await.unwrap(), 0);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_failed_apply_stays_pending(pool: PgPool) {
        let t = tenant(&pool).await;
        let authz = MemoryAuthz::default();
        let bad = RelationshipUpdate::Touch(Relationship {
            object: ObjectRef::new("node", "n"),
            relation: "no_such_relation".into(),
            subject: SubjectRef::object(ObjectRef::new("user", "u")),
        });
        let mut tx = ttx(&pool, t).await;
        let id = enqueue(&mut tx, t, &[bad]).await.unwrap();
        tx.commit().await.unwrap();
        assert!(apply_after_commit(&pool, &authz, t, &[id]).await.is_none());
        assert_eq!(pending_count(&pool).await.unwrap(), 1);
        let mut conn = ttx(&pool, t).await;
        let (attempts, err): (i32, Option<String>) = sqlx::query_as(
            "select attempts, last_error from authz_outbox where tenant_id = $1 and id = $2",
        )
        .bind(t.0)
        .bind(id)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(attempts, 1);
        assert!(err.unwrap().contains("no_such_relation"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_failure_is_recorded_on_the_row_that_failed(pool: PgPool) {
        let t = tenant(&pool).await;
        let authz = MemoryAuthz::default();
        let bad = RelationshipUpdate::Touch(Relationship {
            object: ObjectRef::new("node", "n"),
            relation: "no_such_relation".into(),
            subject: SubjectRef::object(ObjectRef::new("user", "u")),
        });
        let mut tx = ttx(&pool, t).await;
        let head = enqueue(&mut tx, t, &[bad]).await.unwrap();
        let later = enqueue(&mut tx, t, &[viewer("n2", "u")]).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(pending_stats(&pool).await.unwrap().count, 2);
        // The worker applies the tenant up to `later`, and the head row fails.
        assert_eq!(apply_pending(&pool, &authz, 100).await.unwrap(), 0);
        assert!(
            apply_after_commit(&pool, &authz, t, &[later])
                .await
                .is_none()
        );
        let mut conn = ttx(&pool, t).await;
        let rows: Vec<(i64, i32)> = sqlx::query_as(
            "select id, attempts from authz_outbox where tenant_id = $1 order by id",
        )
        .bind(t.0)
        .fetch_all(&mut *conn)
        .await
        .unwrap();
        drop(conn);
        assert_eq!(rows, vec![(head, 2), (later, 0)]);
        let stats = pending_stats(&pool).await.unwrap();
        assert_eq!(stats.count, 2);
        assert_eq!(stats.max_attempts, 2);
        assert!(stats.oldest_age_seconds >= 0.0);
        // The later row waits, to keep the order.
        assert!(!sees(&authz, "n2", "u").await);
    }
}
