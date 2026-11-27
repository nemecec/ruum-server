//! The seal ledger of the Kubernetes runtime driver in the platform database (backlog I620-9;
//! `ruum_runtime::kube::ledger`, migration `0018_runtime_seal_marks.sql`): the generation mark of
//! each sealed record and the pods that took their launch data, shared by every server replica,
//! out of reach of a writer of ConfigMaps. Each statement is one atomic upsert or insert in the
//! record's tenant (row-level security).

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
