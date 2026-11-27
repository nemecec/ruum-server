//! The shared launch authority of the sealed records (backlog I620-9; `docs/STEP3.md` "The
//! sandbox supervisor" 4, "Rollback").
//!
//! A writer of ConfigMaps (a holder of the runtime token) can put an older seal of a record back.
//! The seal's generation and its list of launched pods then go back too, and the record cannot
//! say so itself. So the server keeps both outside the record, where that writer cannot reach:
//!
//! - the **generation mark** of each record (instance and the record's `metadata.uid`): the
//!   highest seal generation any server process read or wrote. A seal below it is refused (a
//!   read that is older is read again first: another process can have written in between, T-85).
//! - the **launches**: the pod uids that took their launch data. A pod's launch is claimed here
//!   (insert once) before the answer leaves, so no replica answers it twice, whatever seal it
//!   reads.
//!
//! [`SealLedger`] is the module border. [`MemoryLedger`] keeps both in one process (the tests,
//! and a process alone); `ruum-server` wires the platform database's ledger (Postgres, migration
//! `0018_runtime_seal_marks.sql`), shared by every server replica. The ConfigMap's seal stays the
//! encrypted cache of the values.

use async_trait::async_trait;
use ruum_core::ids::TenantId;
use ruum_core::runtime::{InstanceId, RuntimeError};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// The record a mark belongs to: the instance and its record ConfigMap's `metadata.uid` (a
/// record deleted and made again under the same name is another record).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RecordRef {
    pub tenant: TenantId,
    pub instance: InstanceId,
    pub record_uid: String,
}

/// The anti-rollback state of the sealed records, shared by every server process.
#[async_trait]
pub trait SealLedger: Send + Sync {
    /// Raises the generation mark of `record` to `generation`. `Ok(true)` when `generation` is
    /// at least the mark (the mark is then `generation`), `Ok(false)` when the mark is higher (an
    /// older seal). Atomic: two raises at once both see a consistent mark.
    async fn observe(&self, record: &RecordRef, generation: u64) -> Result<bool, RuntimeError>;

    /// Claims the one launch of `pod_uid` for `record`. `Ok(true)` for the first claim,
    /// `Ok(false)` when the pod was launched before. Atomic.
    async fn claim_launch(&self, record: &RecordRef, pod_uid: &str) -> Result<bool, RuntimeError>;

    /// Takes back the launch claim of `pod_uid` when its launch surely did not happen (the seal
    /// that lists it was not written, and no answer left): the supervisor's next ask may claim
    /// it again (review of I620-G, required change 2). A pod that was not claimed is fine.
    async fn release(&self, record: &RecordRef, pod_uid: &str) -> Result<(), RuntimeError>;

    /// Forgets the marks and launches of a record that is deleted (its uid never comes back).
    async fn forget(&self, record: &RecordRef) -> Result<(), RuntimeError>;
}

/// [`SealLedger`] in this process only. Two server processes with their own `MemoryLedger`
/// do not share it: the rollback residual of backlog I620-9 then holds between them.
#[derive(Default)]
pub struct MemoryLedger {
    marks: Mutex<HashMap<RecordRef, u64>>,
    launches: Mutex<HashMap<RecordRef, HashSet<String>>>,
}

impl MemoryLedger {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SealLedger for MemoryLedger {
    async fn observe(&self, record: &RecordRef, generation: u64) -> Result<bool, RuntimeError> {
        let mut m = self.marks.lock().expect("ledger marks lock");
        let mark = m.entry(record.clone()).or_insert(0);
        if generation < *mark {
            return Ok(false);
        }
        *mark = generation;
        Ok(true)
    }

    async fn claim_launch(&self, record: &RecordRef, pod_uid: &str) -> Result<bool, RuntimeError> {
        Ok(self
            .launches
            .lock()
            .expect("ledger launches lock")
            .entry(record.clone())
            .or_default()
            .insert(pod_uid.to_owned()))
    }

    async fn release(&self, record: &RecordRef, pod_uid: &str) -> Result<(), RuntimeError> {
        if let Some(l) = self
            .launches
            .lock()
            .expect("ledger launches lock")
            .get_mut(record)
        {
            l.remove(pod_uid);
        }
        Ok(())
    }

    async fn forget(&self, record: &RecordRef) -> Result<(), RuntimeError> {
        self.marks.lock().expect("ledger marks lock").remove(record);
        self.launches
            .lock()
            .expect("ledger launches lock")
            .remove(record);
        Ok(())
    }
}

/// The conformance checks of a [`SealLedger`] (the memory one here, the Postgres one in
/// `ruum-server`).
pub async fn conformance(ledger: &dyn SealLedger, tenant: TenantId) {
    let a = RecordRef {
        tenant,
        instance: InstanceId(uuid::Uuid::now_v7()),
        record_uid: "uid-a".to_owned(),
    };
    let a2 = RecordRef {
        record_uid: "uid-a2".to_owned(),
        ..a.clone()
    };
    // The mark goes up, never down; equal is fresh.
    assert!(ledger.observe(&a, 1).await.unwrap());
    assert!(ledger.observe(&a, 3).await.unwrap());
    assert!(ledger.observe(&a, 3).await.unwrap());
    assert!(!ledger.observe(&a, 2).await.unwrap(), "an older seal");
    assert!(ledger.observe(&a, 4).await.unwrap());
    // Another record of the same instance (made again: a new uid) starts from zero.
    assert!(ledger.observe(&a2, 1).await.unwrap());
    assert!(!ledger.observe(&a, 3).await.unwrap());
    // One launch per pod.
    assert!(ledger.claim_launch(&a, "pod-1").await.unwrap());
    assert!(
        !ledger.claim_launch(&a, "pod-1").await.unwrap(),
        "a second launch"
    );
    assert!(ledger.claim_launch(&a, "pod-2").await.unwrap());
    assert!(ledger.claim_launch(&a2, "pod-3").await.unwrap());
    // A released claim (the launch surely did not happen) can be claimed again, once; the
    // release touches no other pod or record, and a release of an unclaimed pod is fine.
    ledger.release(&a, "pod-2").await.unwrap();
    ledger.release(&a, "pod-never").await.unwrap();
    assert!(!ledger.claim_launch(&a, "pod-1").await.unwrap());
    assert!(!ledger.claim_launch(&a2, "pod-3").await.unwrap());
    assert!(ledger.claim_launch(&a, "pod-2").await.unwrap());
    assert!(!ledger.claim_launch(&a, "pod-2").await.unwrap());
    // Forgetting a deleted record leaves the others.
    ledger.forget(&a).await.unwrap();
    assert!(ledger.observe(&a, 1).await.unwrap());
    assert!(!ledger.observe(&a2, 0).await.unwrap());
    assert!(!ledger.claim_launch(&a2, "pod-3").await.unwrap());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_memory_ledger_passes_the_conformance_checks() {
        conformance(&MemoryLedger::new(), TenantId(uuid::Uuid::now_v7())).await;
    }
}
