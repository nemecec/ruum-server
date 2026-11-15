//! The in-memory implementations of the telemetry border: the store (one storage tenant per
//! Ruum tenant), the class source and the secret source. For unit tests and the privacy suite.

use super::*;
use std::sync::Mutex;

/// The records of one storage tenant.
#[derive(Clone, Debug, Default, Serialize)]
struct StorageTenant {
    logs: Vec<StoredLog>,
    metrics: Vec<StoredMetric>,
    spans: Vec<StoredSpan>,
}

/// [`TelemetryStore`] in memory: a map from the Ruum tenant to its storage tenant.
#[derive(Default)]
pub struct MemoryTelemetryStore {
    tenants: Mutex<BTreeMap<TenantId, StorageTenant>>,
}

impl MemoryTelemetryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything the store holds, as JSON (the privacy suite scans it for secrets).
    pub fn dump(&self) -> Vec<u8> {
        let tenants = self.tenants.lock().expect("telemetry store lock");
        serde_json::to_vec(&*tenants).unwrap_or_default()
    }

    /// The tenants that have a storage tenant.
    pub fn tenants(&self) -> Vec<TenantId> {
        let tenants = self.tenants.lock().expect("telemetry store lock");
        tenants.keys().copied().collect()
    }
}

fn source_matches(source: &SourceIdentity, tenant: TenantId, f: &TelemetryFilter) -> bool {
    source.tenant == tenant
        && f.app.is_none_or(|a| a == source.app)
        && f.environment.is_none_or(|e| e == source.environment)
        && f.deployment.is_none_or(|d| d == source.deployment)
}

fn time_matches(t: OffsetDateTime, f: &TelemetryFilter) -> bool {
    f.from.is_none_or(|from| t >= from) && f.to.is_none_or(|to| t < to)
}

fn fields_match(f: &TelemetryFilter, sets: &[&[StoredAttribute]]) -> bool {
    f.fields.iter().all(|eq| {
        sets.iter().any(|set| {
            set.iter()
                .any(|a| a.key == eq.key && a.value.match_text().as_deref() == Some(&eq.value))
        })
    })
}

#[async_trait]
impl TelemetryStore for MemoryTelemetryStore {
    async fn write(&self, identity: &SourceIdentity, batch: ClassifiedBatch) -> Result<()> {
        let mut tenants = self.tenants.lock().expect("telemetry store lock");
        let st = tenants.entry(identity.tenant).or_default();
        st.logs.extend(batch.logs);
        st.metrics.extend(batch.metrics);
        st.spans.extend(batch.spans);
        Ok(())
    }

    async fn read(&self, tenant: TenantId, f: &TelemetryFilter) -> Result<ClassifiedBatch> {
        let tenants = self.tenants.lock().expect("telemetry store lock");
        let Some(st) = tenants.get(&tenant) else {
            return Ok(ClassifiedBatch::default());
        };
        let limit = f.limit.unwrap_or(usize::MAX);
        let want = |s: Signal| f.signal.is_none_or(|w| w == s);
        let mut out = ClassifiedBatch::default();
        if want(Signal::Logs) {
            out.logs = st
                .logs
                .iter()
                .filter(|r| {
                    source_matches(&r.source, tenant, f)
                        && time_matches(r.time, f)
                        && f.min_severity.is_none_or(|s| r.severity >= s)
                        && fields_match(f, &[&r.fields, &r.attributes])
                })
                .take(limit)
                .cloned()
                .collect();
        }
        if want(Signal::Metrics) && f.min_severity.is_none() {
            out.metrics = st
                .metrics
                .iter()
                .filter(|r| {
                    source_matches(&r.source, tenant, f)
                        && time_matches(r.time, f)
                        && fields_match(f, &[&r.labels])
                })
                .take(limit)
                .cloned()
                .collect();
        }
        if want(Signal::Traces) && f.min_severity.is_none() {
            out.spans = st
                .spans
                .iter()
                .filter(|r| {
                    source_matches(&r.source, tenant, f)
                        && time_matches(r.start, f)
                        && fields_match(f, &[&r.attributes])
                })
                .take(limit)
                .cloned()
                .collect();
        }
        Ok(out)
    }
}

/// [`FieldClassSource`] in memory.
#[derive(Default)]
pub struct MemoryFieldClasses {
    apps: Mutex<HashMap<(TenantId, NodeId), FieldClasses>>,
}

impl MemoryFieldClasses {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(
        &self,
        tenant: TenantId,
        app: NodeId,
        kind: FieldKind,
        name: &str,
        entry: FieldClassEntry,
    ) {
        let mut apps = self.apps.lock().expect("class lock");
        apps.entry((tenant, app))
            .or_default()
            .set(kind, name, entry);
    }
}

#[async_trait]
impl FieldClassSource for MemoryFieldClasses {
    async fn classes(&self, tenant: TenantId, app: NodeId) -> Result<Arc<FieldClasses>> {
        let apps = self.apps.lock().expect("class lock");
        Ok(Arc::new(
            apps.get(&(tenant, app)).cloned().unwrap_or_default(),
        ))
    }
}

/// [`SecretSource`] in memory: the secret values of each app.
#[derive(Default)]
pub struct MemorySecretSource {
    apps: Mutex<HashMap<(TenantId, NodeId), Vec<SecretValue>>>,
}

impl MemorySecretSource {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, tenant: TenantId, app: NodeId, values: Vec<SecretValue>) {
        let mut apps = self.apps.lock().expect("secret lock");
        apps.insert((tenant, app), values);
    }
}

#[async_trait]
impl SecretSource for MemorySecretSource {
    async fn secrets(&self, identity: &SourceIdentity) -> Result<Vec<SecretValue>> {
        let apps = self.apps.lock().expect("secret lock");
        Ok(apps
            .get(&(identity.tenant, identity.app))
            .cloned()
            .unwrap_or_default())
    }
}
