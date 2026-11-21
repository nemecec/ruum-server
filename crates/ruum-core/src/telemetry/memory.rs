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
        serde_json::to_vec(&*tenants).expect("the telemetry store serialises to JSON")
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
        // Oldest first by record time, then in the order written; the limit keeps the oldest
        // (Kimi review of run 12, finding 5). The matches are sorted as references and only
        // the kept ones are cloned.
        let want = |s: Signal| f.signal.is_none_or(|w| w == s);
        let mut out = ClassifiedBatch::default();
        if want(Signal::Logs) {
            out.logs = kept(
                st.logs.iter().filter(|r| {
                    source_matches(&r.source, tenant, f)
                        && time_matches(r.time, f)
                        && f.min_severity.is_none_or(|s| r.severity >= s)
                        && fields_match(f, &[&r.fields, &r.attributes])
                        && log_extra_matches(r, f)
                }),
                limit,
                f.newest,
                |r| r.time,
            );
        }
        // The filters of logs only (severity, words, origin) leave out the other signals.
        let logs_only = f.min_severity.is_some() || !f.words.is_empty() || f.origin.is_some();
        if want(Signal::Metrics) && !logs_only && f.trace_id.is_none() {
            out.metrics = kept(
                st.metrics.iter().filter(|r| {
                    source_matches(&r.source, tenant, f)
                        && time_matches(r.time, f)
                        && fields_match(f, &[&r.labels])
                }),
                limit,
                f.newest,
                |r| r.time,
            );
        }
        if want(Signal::Traces) && !logs_only {
            out.spans = kept(
                st.spans.iter().filter(|r| {
                    source_matches(&r.source, tenant, f)
                        && time_matches(r.start, f)
                        && fields_match(f, &[&r.attributes])
                        && span_extra_matches(r, f)
                }),
                limit,
                f.newest,
                |r| r.start,
            );
        }
        Ok(out)
    }
}

/// The `limit` oldest of `matches` by `time` (or the `limit` newest when `newest`), oldest
/// first (a stable sort, so the order written breaks ties), cloned.
fn kept<'a, T: Clone + 'a>(
    matches: impl Iterator<Item = &'a T>,
    limit: usize,
    newest: bool,
    time: impl Fn(&T) -> OffsetDateTime,
) -> Vec<T> {
    if limit == 0 {
        return Vec::new();
    }
    let mut refs: Vec<&T> = matches.collect();
    refs.sort_by_key(|r| time(r));
    let skip = if newest {
        refs.len().saturating_sub(limit)
    } else {
        0
    };
    refs.into_iter().skip(skip).take(limit).cloned().collect()
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
