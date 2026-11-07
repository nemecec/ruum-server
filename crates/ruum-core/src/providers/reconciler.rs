//! The reconciler: the control loop of typed nodes (section 8.1, the operator pattern).
//!
//! Each round takes the rows that need it ([`TypedNodeStore::reconcile_candidates`], the rows of
//! `typed_nodes_reconcile_idx`: `provisioning`, `updating`, `degraded`, or `observed_version`
//! behind `desired_version`; not `failed` or `deleted`, where the loop stops) and, through the
//! registry, asks the node's provider:
//!
//! - The observed version is behind the desired one: `applyManifest` with the newest manifest
//!   version, the observed version as `previous_version_id`, and the principal pair that wrote
//!   the newest version. The delivery ID comes from the tenant, the node and the version
//!   ([`apply_delivery_id`]), so each round sends the same delivery again until the provider
//!   reports, and the provider does the work once.
//! - Else (the node is busy at the desired version, or the provider never reported a version):
//!   `getTypedNodeStatus`.
//!
//! The answer goes into the store like a status report ([`TypedNodeStore::report`]): only a
//! newer generation changes the row. A row whose state changed less than `settled` ago waits
//! for the next round, so the loop does not race work that is under way.
//!
//! How the server starts the reconciler is the server's wiring ([`Reconciler::run`] in a task).

use super::store::{ReconcileItem, ReportOutcome, TypedNodeStore, error_text};
use super::*;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;

/// The delivery ID of the reconciler's `applyManifest` of `version` on a node: the same for
/// every round, so a retry is the same delivery. A UUIDv7 in form: the time bits of the version
/// ID (itself a UUIDv7), and random-looking bits from SHA-256 of the tenant, the node and the
/// version.
pub fn apply_delivery_id(tenant: TenantId, node: NodeId, version: VersionId) -> DeliveryId {
    let mut h = Sha256::new();
    h.update(b"ruum.reconcile.apply\0");
    h.update(tenant.0.as_bytes());
    h.update(node.0.as_bytes());
    h.update(version.0.as_bytes());
    let digest = h.finalize();
    let mut b = [0u8; 16];
    b[..6].copy_from_slice(&version.0.as_bytes()[..6]);
    b[6..].copy_from_slice(&digest[..10]);
    b[6] = (b[6] & 0x0f) | 0x70;
    b[8] = (b[8] & 0x3f) | 0x80;
    DeliveryId(Uuid::from_bytes(b))
}

/// What one round did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReconcileRound {
    /// `applyManifest` calls.
    pub applied: usize,
    /// `getTypedNodeStatus` calls.
    pub polled: usize,
    /// Answers that changed a row.
    pub stored: usize,
    /// Calls that failed (logged; the row is tried again in a later round).
    pub errors: usize,
}

/// The reconciler.
pub struct Reconciler {
    store: Arc<dyn TypedNodeStore>,
    registry: Arc<ProviderRegistry>,
    settled: Duration,
    batch: usize,
}

impl Reconciler {
    /// A reconciler that waits 30 seconds after a change of state and takes 100 rows a round.
    pub fn new(store: Arc<dyn TypedNodeStore>, registry: Arc<ProviderRegistry>) -> Self {
        Self {
            store,
            registry,
            settled: Duration::from_secs(30),
            batch: 100,
        }
    }

    /// Sets how long a row's state must be unchanged before the reconciler looks at it.
    pub fn settled(mut self, settled: Duration) -> Self {
        self.settled = settled;
        self
    }

    /// Sets the most rows of one round.
    pub fn batch(mut self, batch: usize) -> Self {
        self.batch = batch.max(1);
        self
    }

    /// One round over the rows that need it.
    pub async fn run_once(&self) -> Result<ReconcileRound> {
        let mut round = ReconcileRound::default();
        for item in self
            .store
            .reconcile_candidates(self.settled, self.batch)
            .await?
        {
            let row = &item.row;
            match self.reconcile(&item, &mut round).await {
                Ok(true) => round.stored += 1,
                Ok(false) => {}
                Err(e) => {
                    round.errors += 1;
                    tracing::warn!(
                        tenant = %row.tenant_id,
                        node = %row.node_id,
                        provider = %row.provider,
                        error = %error_text(&e),
                        "the reconciler could not reach the provider"
                    );
                }
            }
        }
        Ok(round)
    }

    /// Asks the provider about one row and stores the answer. True when the row changed.
    async fn reconcile(&self, item: &ReconcileItem, round: &mut ReconcileRound) -> Result<bool> {
        let row = &item.row;
        let provider = self
            .registry
            .provider(&row.provider)
            .ok_or_else(|| Error::not_found(format!("provider {}", row.provider)))?;
        let status = match row.observed_version {
            Some(observed) if observed != row.desired_version => {
                round.applied += 1;
                let delivery = apply_delivery_id(row.tenant_id, row.node_id, row.desired_version);
                provider
                    .apply_manifest(
                        delivery,
                        row.node_id,
                        ApplyManifestRequest {
                            tenant_id: row.tenant_id,
                            version_id: row.desired_version,
                            manifest: item.manifest.clone(),
                            previous_version_id: observed,
                            by: item.by,
                        },
                    )
                    .await?
                    .into_inner()
            }
            _ => {
                round.polled += 1;
                provider
                    .get_typed_node_status(row.tenant_id, row.node_id)
                    .await?
            }
        };
        if status.node_id != row.node_id {
            return Err(Error::Internal(format!(
                "provider {} answered for another node",
                row.provider
            )));
        }
        Ok(self.store.report(row.tenant_id, &status).await? == ReportOutcome::Stored)
    }

    /// Runs rounds for ever, one every `interval`.
    pub async fn run(&self, interval: Duration) {
        loop {
            if let Err(e) = self.run_once().await {
                tracing::warn!(error = %e, "the reconciler failed a round");
            }
            tokio::time::sleep(interval).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_delivery_ids_are_stable_v7_and_distinct() {
        let (t, n, v) = (TenantId::new(), NodeId::new(), VersionId::new());
        let id = apply_delivery_id(t, n, v);
        assert_eq!(id, apply_delivery_id(t, n, v));
        assert_eq!(id.0.get_version_num(), 7);
        assert_eq!(id.0.get_variant(), uuid::Variant::RFC4122);
        assert_eq!(&id.0.as_bytes()[..6], &v.0.as_bytes()[..6]);
        assert_ne!(id, apply_delivery_id(TenantId::new(), n, v));
        assert_ne!(id, apply_delivery_id(t, NodeId::new(), v));
        assert_ne!(id, apply_delivery_id(t, n, VersionId::new()));
    }
}
