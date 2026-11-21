//! Card O-2: the read operations of telemetry (logs, metrics, traces) over `TelemetryStore`.
//!
//! [`TelemetryService`] is the telemetry part of the services (`Services::telemetry`): the
//! store, the class source and the tenant keys of the pipeline, so the API and the suites read
//! and write the same records.

use super::*;
use crate::runtime::SecretValue;

/// The telemetry of the services: the store and the pipeline that writes into it.
pub struct TelemetryService {
    store: Arc<dyn TelemetryStore>,
    classes: Arc<dyn FieldClassSource>,
    keys: TenantKeys,
    usage: Arc<dyn UsageSink>,
}

impl TelemetryService {
    pub fn new(
        store: Arc<dyn TelemetryStore>,
        classes: Arc<dyn FieldClassSource>,
        keys: TenantKeys,
        usage: Arc<dyn UsageSink>,
    ) -> Self {
        TelemetryService {
            store,
            classes,
            keys,
            usage,
        }
    }

    /// The in-memory store and class source.
    pub fn memory(keys: TenantKeys, usage: Arc<dyn UsageSink>) -> Self {
        Self::new(
            Arc::new(memory::MemoryTelemetryStore::new()),
            Arc::new(memory::MemoryFieldClasses::new()),
            keys,
            usage,
        )
    }

    /// The store the reads go to.
    pub fn store(&self) -> &Arc<dyn TelemetryStore> {
        &self.store
    }

    /// Runs the pipeline on a batch of `identity` and writes it to the store (the processor's
    /// path; the suites use it to put records in).
    pub async fn ingest(&self, identity: &SourceIdentity, batch: Batch) -> Result<IngestReport> {
        self.ingest_with_secrets(identity, batch, Vec::new()).await
    }

    /// [`TelemetryService::ingest`] with the secret values of the source (the app's secrets and
    /// the workload token) that the pipeline redacts.
    pub async fn ingest_with_secrets(
        &self,
        identity: &SourceIdentity,
        batch: Batch,
        secrets: Vec<SecretValue>,
    ) -> Result<IngestReport> {
        let source = memory::MemorySecretSource::new();
        source.set(identity.tenant, identity.app, secrets);
        PipelineIngest::new(
            self.store.clone(),
            self.classes.clone(),
            Arc::new(source),
            self.keys.clone(),
            self.usage.clone(),
        )
        .ingest(identity, batch)
        .await
    }
}
