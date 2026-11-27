//! The seal ledger of the Kubernetes runtime driver in the platform database (backlog I620-9;
//! `ruum_runtime::kube::ledger`, migration `0018_runtime_seal_marks.sql`): the generation mark of
//! each sealed record and the pods that took their launch data, shared by every server replica,
//! out of reach of a writer of ConfigMaps. Each change is one atomic upsert, insert or delete in
//! the record's tenant (row-level security); a generation that is already the mark is only read.

use async_trait::async_trait;
use ruum_core::journal::postgres::begin_tenant;
use ruum_core::runtime::RuntimeError;
use ruum_runtime::kube::ledger::{RecordRef, SealLedger};
use sqlx::PgPool;

pub struct PgSealLedger {
    pool: PgPool,
}

impl PgSealLedger {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// A database error as the driver's `unavailable` (the supervisor asks again); the message names
/// the step only.
fn unavailable(step: &str, e: impl std::fmt::Display) -> RuntimeError {
    tracing::warn!(target: "ruum::runtime", step, error = %e, "the seal ledger failed");
    RuntimeError::Unavailable {
        reason: format!("the seal ledger cannot {step}"),
    }
}

#[async_trait]
impl SealLedger for PgSealLedger {
    async fn observe(&self, r: &RecordRef, generation: u64) -> Result<bool, RuntimeError> {
        let generation = i64::try_from(generation).map_err(|_| RuntimeError::Internal {
            reason: "a seal generation is out of range".to_owned(),
        })?;
        let mut tx = begin_tenant(&self.pool, r.tenant)
            .await
            .map_err(|e| unavailable("open a transaction", e))?;
        // Read first: every supervisor request (each token renewal too) observes its record's
        // generation, which is nearly always the mark. That answer writes nothing (Kimi, run
        // 17, finding 2: no row version and no WAL per request). Only a missing or a higher
        // generation takes the upsert below, which stays atomic for two raises at once.
        let mark: Option<i64> = sqlx::query_scalar(
            "select generation from runtime_seal_marks \
             where tenant_id = $1 and instance_id = $2 and record_uid = $3",
        )
        .bind(r.tenant.0)
        .bind(r.instance.0)
        .bind(&r.record_uid)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| unavailable("read a mark", e))?;
        if let Some(m) = mark
            && m >= generation
        {
            tx.commit()
                .await
                .map_err(|e| unavailable("commit a mark", e))?;
            return Ok(m == generation);
        }
        // The mark rises or stays; a lower generation returns no row.
        let row: Option<i64> = sqlx::query_scalar(
            "insert into runtime_seal_marks (tenant_id, instance_id, record_uid, generation) \
             values ($1, $2, $3, $4) \
             on conflict (tenant_id, instance_id, record_uid) do update \
               set generation = excluded.generation, updated_at = now() \
               where runtime_seal_marks.generation <= excluded.generation \
             returning generation",
        )
        .bind(r.tenant.0)
        .bind(r.instance.0)
        .bind(&r.record_uid)
        .bind(generation)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| unavailable("raise a mark", e))?;
        tx.commit()
            .await
            .map_err(|e| unavailable("commit a mark", e))?;
        Ok(row.is_some())
    }

    async fn claim_launch(&self, r: &RecordRef, pod_uid: &str) -> Result<bool, RuntimeError> {
        let mut tx = begin_tenant(&self.pool, r.tenant)
            .await
            .map_err(|e| unavailable("open a transaction", e))?;
        let row: Option<String> = sqlx::query_scalar(
            "insert into runtime_launches (tenant_id, instance_id, record_uid, pod_uid) \
             values ($1, $2, $3, $4) on conflict do nothing returning pod_uid",
        )
        .bind(r.tenant.0)
        .bind(r.instance.0)
        .bind(&r.record_uid)
        .bind(pod_uid)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| unavailable("claim a launch", e))?;
        tx.commit()
            .await
            .map_err(|e| unavailable("commit a launch", e))?;
        Ok(row.is_some())
    }

    async fn release(&self, r: &RecordRef, pod_uid: &str) -> Result<(), RuntimeError> {
        let mut tx = begin_tenant(&self.pool, r.tenant)
            .await
            .map_err(|e| unavailable("open a transaction", e))?;
        sqlx::query(
            "delete from runtime_launches \
             where tenant_id = $1 and instance_id = $2 and record_uid = $3 and pod_uid = $4",
        )
        .bind(r.tenant.0)
        .bind(r.instance.0)
        .bind(&r.record_uid)
        .bind(pod_uid)
        .execute(&mut *tx)
        .await
        .map_err(|e| unavailable("release a launch", e))?;
        tx.commit()
            .await
            .map_err(|e| unavailable("commit a release", e))?;
        Ok(())
    }

    async fn forget(&self, r: &RecordRef) -> Result<(), RuntimeError> {
        let mut tx = begin_tenant(&self.pool, r.tenant)
            .await
            .map_err(|e| unavailable("open a transaction", e))?;
        for table in ["runtime_seal_marks", "runtime_launches"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "delete from {table} where tenant_id = $1 and instance_id = $2 and record_uid = $3"
            )))
            .bind(r.tenant.0)
            .bind(r.instance.0)
            .bind(&r.record_uid)
            .execute(&mut *tx)
            .await
            .map_err(|e| unavailable("forget a record", e))?;
        }
        tx.commit()
            .await
            .map_err(|e| unavailable("commit a forget", e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruum_core::ids::TenantId;
    use ruum_core::runtime::InstanceId;

    async fn tenant(pool: &PgPool) -> TenantId {
        let id = TenantId::new();
        let mut tx = begin_tenant(pool, id).await.unwrap();
        sqlx::query(
            "insert into tenants (id, name, kind, created_kind, created_by) \
             values ($1, 'Test', 'team', 'user', $2)",
        )
        .bind(id.0)
        .bind(uuid::Uuid::now_v7())
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        id
    }

    /// The Postgres ledger passes the same checks as the memory one.
    #[sqlx::test(migrations = "../../migrations")]
    async fn the_postgres_ledger_passes_the_conformance_checks(pool: PgPool) {
        let t = tenant(&pool).await;
        ruum_runtime::kube::ledger::conformance(&PgSealLedger::new(pool), t).await;
    }

    /// The row version (`xmin`) of a record's mark: a write makes a new one.
    async fn mark_version(pool: &PgPool, r: &RecordRef) -> String {
        let mut tx = begin_tenant(pool, r.tenant).await.unwrap();
        let v: String = sqlx::query_scalar(
            "select xmin::text from runtime_seal_marks \
             where tenant_id = $1 and instance_id = $2 and record_uid = $3",
        )
        .bind(r.tenant.0)
        .bind(r.instance.0)
        .bind(&r.record_uid)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        v
    }

    /// Kimi, run 17, finding 2: observing the generation that is already the mark (every
    /// supervisor request's case) writes no new row version; a higher one does, and a lower one
    /// is refused without a write.
    #[sqlx::test(migrations = "../../migrations")]
    async fn observing_the_current_mark_writes_nothing(pool: PgPool) {
        let t = tenant(&pool).await;
        let r = RecordRef {
            tenant: t,
            instance: InstanceId(uuid::Uuid::now_v7()),
            record_uid: "uid".to_owned(),
        };
        let l = PgSealLedger::new(pool.clone());
        assert!(l.observe(&r, 3).await.unwrap());
        let first = mark_version(&pool, &r).await;
        for _ in 0..3 {
            assert!(l.observe(&r, 3).await.unwrap());
        }
        assert_eq!(
            mark_version(&pool, &r).await,
            first,
            "an unchanged mark was written again"
        );
        assert!(!l.observe(&r, 2).await.unwrap());
        assert_eq!(
            mark_version(&pool, &r).await,
            first,
            "a refused generation wrote the mark"
        );
        assert!(l.observe(&r, 4).await.unwrap());
        assert_ne!(
            mark_version(&pool, &r).await,
            first,
            "a higher generation did not raise the mark"
        );
    }

    /// Two replicas claim one pod's launch at the same moment: exactly one wins; two raise the
    /// mark at once: it ends at the higher one, and the lower one is refused after it.
    #[sqlx::test(migrations = "../../migrations")]
    async fn concurrent_claims_launch_a_pod_once(pool: PgPool) {
        let t = tenant(&pool).await;
        let r = RecordRef {
            tenant: t,
            instance: InstanceId(uuid::Uuid::now_v7()),
            record_uid: "uid".to_owned(),
        };
        let a = PgSealLedger::new(pool.clone());
        let b = PgSealLedger::new(pool.clone());
        for round in 0..20 {
            let pod = format!("pod-{round}");
            let (x, y) = tokio::join!(a.claim_launch(&r, &pod), b.claim_launch(&r, &pod));
            assert!(x.unwrap() ^ y.unwrap(), "round {round}: one claim only");
        }
        let (x, y) = tokio::join!(a.observe(&r, 5), b.observe(&r, 7));
        assert!(y.unwrap());
        let _ = x.unwrap();
        assert!(!a.observe(&r, 6).await.unwrap());
        assert!(a.observe(&r, 7).await.unwrap());
    }
}
