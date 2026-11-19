//! The processor (card O-1b; decision 0006 answer 4): the parts of the telemetry role that do
//! not depend on a transport.
//!
//! - [`LogShipper`]: reads the driver's log buffer of each running instance
//!   ([`RuntimeDriver::logs`], already redacted by the driver) and sends the lines through the
//!   same pipeline ([`TelemetryIngest`]) with the identity that the driver attached
//!   ([`LogLine::identity`]). It is authenticated by construction: the identity comes from the
//!   driver's spec, never from the line.
//! - [`WorkloadSecretSource`]: the real [`SecretSource`]. The values come from where the running
//!   workload got them, [`crate::runtime::WorkloadSpec::redacted_values`] of its instances, as
//!   the driver holds them, through a lookup closure. It is not a secret store (OpenBao and
//!   `SecretStore` are card W-4's, which gives a second implementation).
//! - [`FairBudget`]: the per-identity rate and byte budget of the receiver, with a fair share
//!   of the in-flight bytes per tenant (S-61). The shipper does not use it: its own caps per
//!   tick bound it ([`ShipperConfig`]), and the lines come from the driver's bounded buffer.
//!
//! The OTLP mapping is in [`super::otlp`], the source authentication in [`super::source`], the
//! real store in [`super::lgtm`], and the receiver in `crates/ruum-server/src/telemetry.rs`.
//!
//! Nothing here logs a record value, an attribute value or a secret: `tracing` events carry
//! identities, instance IDs, counts and reasons only.

use super::*;
use crate::runtime::{InstanceId, LogLine, LogStream, RuntimeDriver, RuntimeError};
use std::path::PathBuf;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------- secrets

/// The lookup of [`WorkloadSecretSource`]: the redacted values
/// ([`crate::runtime::WorkloadSpec::redacted_values`]: secret environment values, previous
/// values and the workload token) of the running instances of an identity.
pub type SecretLookup = Arc<dyn Fn(&SourceIdentity) -> Vec<SecretValue> + Send + Sync>;

/// [`SecretSource`] over the values that the driver holds for the running workloads. The values
/// stay in memory; the type has no `Debug` and logs nothing.
pub struct WorkloadSecretSource {
    lookup: SecretLookup,
}

impl WorkloadSecretSource {
    pub fn new(lookup: SecretLookup) -> Self {
        WorkloadSecretSource { lookup }
    }

    /// A source without values (no driver wired yet). The deny-list, the classes and the
    /// scrubber still run.
    pub fn empty() -> Self {
        WorkloadSecretSource {
            lookup: Arc::new(|_| Vec::new()),
        }
    }
}

#[async_trait]
impl SecretSource for WorkloadSecretSource {
    async fn secrets(&self, identity: &SourceIdentity) -> Result<Vec<SecretValue>> {
        Ok((self.lookup)(identity))
    }
}

// ---------------------------------------------------------------------------- budget

/// The caps of [`FairBudget`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetConfig {
    /// Requests per second of one identity (the bucket refills at this rate).
    pub requests_per_second: u32,
    /// Bytes per second of one identity.
    pub bytes_per_second: u64,
    /// The bucket holds this many seconds of the rates (the burst).
    pub burst_seconds: u32,
    /// The bytes that all requests in flight may hold together: the body from its first byte
    /// (or its declared length), and the decoded estimate of the OTLP walk
    /// ([`super::otlp::Shape::decoded_bytes`]) before prost decodes it.
    pub max_in_flight_bytes: u64,
    /// One tenant may hold at most `max_in_flight_bytes / tenant_share` of them.
    pub tenant_share: u64,
    /// The identities whose buckets are kept; idle buckets go first.
    pub max_identities: usize,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        BudgetConfig {
            requests_per_second: 20,
            bytes_per_second: 2 * 1024 * 1024,
            burst_seconds: 5,
            // One request holds at most 8 MiB of body and 48 MiB of decoded estimate
            // (`otlp::DECODED_FACTOR`), so one tenant's share (a quarter) fits one request of
            // the largest kind.
            max_in_flight_bytes: 256 * 1024 * 1024,
            tenant_share: 4,
            max_identities: 10_000,
        }
    }
}

/// Why [`FairBudget`] refused a request. The order is the order of the checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetRefusal {
    /// The identity sent more requests than its rate.
    IdentityRate,
    /// The identity sent more bytes than its rate.
    IdentityBytes,
    /// The identity's tenant holds its share of the in-flight bytes.
    TenantShare,
    /// All requests in flight hold the global cap, or too many identities are tracked.
    Global,
}

impl BudgetRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            BudgetRefusal::IdentityRate => "identity_rate",
            BudgetRefusal::IdentityBytes => "identity_bytes",
            BudgetRefusal::TenantShare => "tenant_share",
            BudgetRefusal::Global => "global",
        }
    }
}

struct Bucket {
    requests: f64,
    bytes: f64,
    at: Instant,
}

#[derive(Default)]
struct BudgetState {
    buckets: HashMap<SourceIdentity, Bucket>,
    in_flight: u64,
    by_tenant: HashMap<TenantId, u64>,
}

/// The rate and byte budget of the processor (S-61):
///
/// - **Key**: the [`SourceIdentity`] (tenant, app, deployment, environment) for the rates; the
///   tenant for the share of the in-flight bytes.
/// - **Who is refused first**: the identity over its own rate ([`BudgetRefusal::IdentityRate`],
///   [`BudgetRefusal::IdentityBytes`]), then the tenant over its share of the in-flight bytes
///   ([`BudgetRefusal::TenantShare`]). A flood of one tenant fills at most its share, so a
///   batch of another tenant in the same moment is still taken. Only when several tenants hold
///   their shares at once is a request refused by the global cap.
/// - **The refusal log line** (written by the caller) carries the identity, the reason and the
///   counts (bytes, records), never content.
pub struct FairBudget {
    cfg: BudgetConfig,
    state: Mutex<BudgetState>,
}

/// Bytes held in flight; dropping it gives them back. It can grow (each check of the tenant's
/// share and the global cap again) and shrink while the request runs.
pub struct BudgetPermit {
    budget: Arc<FairBudget>,
    tenant: TenantId,
    bytes: u64,
}

impl BudgetPermit {
    /// The bytes held.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Holds `extra` bytes more, if the tenant's share and the global cap allow it. On a
    /// refusal the permit keeps what it held.
    pub fn grow(&mut self, extra: u64) -> std::result::Result<(), BudgetRefusal> {
        let mut st = self.budget.state.lock().expect("budget lock");
        self.budget.try_hold(&mut st, self.tenant, extra)?;
        self.bytes += extra;
        Ok(())
    }

    /// Holds `total` bytes in all, if it is more than now.
    pub fn grow_to(&mut self, total: u64) -> std::result::Result<(), BudgetRefusal> {
        match total.checked_sub(self.bytes) {
            Some(extra) if extra > 0 => self.grow(extra),
            _ => Ok(()),
        }
    }

    /// Gives back what is held over `total`.
    pub fn shrink_to(&mut self, total: u64) {
        if total < self.bytes {
            let mut st = self.budget.state.lock().expect("budget lock");
            FairBudget::release(&mut st, self.tenant, self.bytes - total);
            self.bytes = total;
        }
    }
}

impl Drop for BudgetPermit {
    fn drop(&mut self) {
        let mut st = self.budget.state.lock().expect("budget lock");
        FairBudget::release(&mut st, self.tenant, self.bytes);
    }
}

impl FairBudget {
    pub fn new(cfg: BudgetConfig) -> Arc<Self> {
        Arc::new(FairBudget {
            cfg,
            state: Mutex::new(BudgetState::default()),
        })
    }

    pub fn config(&self) -> BudgetConfig {
        self.cfg
    }

    /// Takes one request of the identity's rate. Call it before reading a body.
    pub fn admit_request(&self, id: &SourceIdentity) -> std::result::Result<(), BudgetRefusal> {
        let mut st = self.state.lock().expect("budget lock");
        let now = Instant::now();
        let bucket = self.bucket(&mut st, id, now)?;
        if bucket.requests < 1.0 {
            return Err(BudgetRefusal::IdentityRate);
        }
        bucket.requests -= 1.0;
        Ok(())
    }

    /// Takes `bytes` of the identity's byte rate and holds them in flight until the permit
    /// drops.
    pub fn admit_bytes(
        self: &Arc<Self>,
        id: &SourceIdentity,
        bytes: u64,
    ) -> std::result::Result<BudgetPermit, BudgetRefusal> {
        let mut st = self.state.lock().expect("budget lock");
        let now = Instant::now();
        let bucket = self.bucket(&mut st, id, now)?;
        if bucket.bytes < bytes as f64 {
            return Err(BudgetRefusal::IdentityBytes);
        }
        self.try_hold(&mut st, id.tenant, bytes)?;
        // Checked above: the bucket exists.
        if let Some(b) = st.buckets.get_mut(id) {
            b.bytes -= bytes as f64;
        }
        Ok(BudgetPermit {
            budget: self.clone(),
            tenant: id.tenant,
            bytes,
        })
    }

    /// Holds `bytes` in flight for the identity's tenant (its share and the global cap; not the
    /// byte rate) until the permit drops. The receiver calls it before it reads a body, with
    /// the declared length or 0, and grows the permit as the body arrives.
    pub fn hold(
        self: &Arc<Self>,
        id: &SourceIdentity,
        bytes: u64,
    ) -> std::result::Result<BudgetPermit, BudgetRefusal> {
        let mut st = self.state.lock().expect("budget lock");
        self.try_hold(&mut st, id.tenant, bytes)?;
        Ok(BudgetPermit {
            budget: self.clone(),
            tenant: id.tenant,
            bytes,
        })
    }

    /// Takes `bytes` of the identity's byte rate (once the body is read).
    pub fn take_bytes(
        &self,
        id: &SourceIdentity,
        bytes: u64,
    ) -> std::result::Result<(), BudgetRefusal> {
        let mut st = self.state.lock().expect("budget lock");
        let bucket = self.bucket(&mut st, id, Instant::now())?;
        if bucket.bytes < bytes as f64 {
            return Err(BudgetRefusal::IdentityBytes);
        }
        bucket.bytes -= bytes as f64;
        Ok(())
    }

    /// Adds `bytes` to the tenant's and the global in-flight bytes, if both caps allow it.
    fn try_hold(
        &self,
        st: &mut BudgetState,
        tenant: TenantId,
        bytes: u64,
    ) -> std::result::Result<(), BudgetRefusal> {
        let held = st.by_tenant.get(&tenant).copied().unwrap_or(0);
        let share = self.cfg.max_in_flight_bytes / self.cfg.tenant_share.max(1);
        if held.saturating_add(bytes) > share {
            return Err(BudgetRefusal::TenantShare);
        }
        if st.in_flight.saturating_add(bytes) > self.cfg.max_in_flight_bytes {
            return Err(BudgetRefusal::Global);
        }
        st.in_flight += bytes;
        *st.by_tenant.entry(tenant).or_default() += bytes;
        Ok(())
    }

    fn release(st: &mut BudgetState, tenant: TenantId, bytes: u64) {
        st.in_flight = st.in_flight.saturating_sub(bytes);
        if let Some(n) = st.by_tenant.get_mut(&tenant) {
            *n = n.saturating_sub(bytes);
            if *n == 0 {
                st.by_tenant.remove(&tenant);
            }
        }
    }

    /// The identity's bucket, refilled to `now`. A new identity gets a full bucket; when the
    /// map is full, idle buckets (full again) are forgotten first, else the request is refused.
    fn bucket<'a>(
        &self,
        st: &'a mut BudgetState,
        id: &SourceIdentity,
        now: Instant,
    ) -> std::result::Result<&'a mut Bucket, BudgetRefusal> {
        let cap_r = f64::from(self.cfg.requests_per_second) * f64::from(self.cfg.burst_seconds);
        let cap_b = self.cfg.bytes_per_second as f64 * f64::from(self.cfg.burst_seconds);
        if !st.buckets.contains_key(id) && st.buckets.len() >= self.cfg.max_identities {
            let (rps, bps) = (
                f64::from(self.cfg.requests_per_second),
                self.cfg.bytes_per_second as f64,
            );
            st.buckets.retain(|_, b| {
                let dt = now.duration_since(b.at).as_secs_f64();
                b.requests + dt * rps < cap_r || b.bytes + dt * bps < cap_b
            });
            if st.buckets.len() >= self.cfg.max_identities {
                return Err(BudgetRefusal::Global);
            }
        }
        let b = st.buckets.entry(*id).or_insert(Bucket {
            requests: cap_r,
            bytes: cap_b,
            at: now,
        });
        let dt = now.duration_since(b.at).as_secs_f64();
        b.requests = (b.requests + dt * f64::from(self.cfg.requests_per_second)).min(cap_r);
        b.bytes = (b.bytes + dt * self.cfg.bytes_per_second as f64).min(cap_b);
        b.at = now;
        Ok(b)
    }
}

// ---------------------------------------------------------------------------- positions

/// Where the shipper keeps its position per instance (the `seq` of the last line it shipped),
/// so a restart neither loses nor repeats more than one batch.
#[async_trait]
pub trait PositionStore: Send + Sync {
    async fn load(&self) -> Result<HashMap<InstanceId, u64>>;
    async fn save(&self, positions: &HashMap<InstanceId, u64>) -> Result<()>;
}

/// [`PositionStore`] in memory (tests; a server without a state file).
#[derive(Default)]
pub struct MemoryPositions {
    saved: Mutex<HashMap<InstanceId, u64>>,
}

impl MemoryPositions {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl PositionStore for MemoryPositions {
    async fn load(&self) -> Result<HashMap<InstanceId, u64>> {
        Ok(self.saved.lock().expect("position lock").clone())
    }

    async fn save(&self, positions: &HashMap<InstanceId, u64>) -> Result<()> {
        *self.saved.lock().expect("position lock") = positions.clone();
        Ok(())
    }
}

/// [`PositionStore`] in a JSON file (`RUUM_TELEMETRY_SHIPPER_STATE`), written to a temporary
/// file and renamed, so a crash leaves the old or the new positions, never half of them.
pub struct FilePositions {
    path: PathBuf,
}

impl FilePositions {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        FilePositions { path: path.into() }
    }
}

#[async_trait]
impl PositionStore for FilePositions {
    async fn load(&self) -> Result<HashMap<InstanceId, u64>> {
        match tokio::fs::read(&self.path).await {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| crate::Error::Internal(format!("shipper state file: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(e) => Err(crate::Error::Internal(format!("shipper state file: {e}"))),
        }
    }

    async fn save(&self, positions: &HashMap<InstanceId, u64>) -> Result<()> {
        let bytes = serde_json::to_vec(positions)
            .map_err(|e| crate::Error::Internal(format!("shipper state: {e}")))?;
        let tmp = self.path.with_extension("tmp");
        tokio::fs::write(&tmp, bytes)
            .await
            .map_err(|e| crate::Error::Internal(format!("shipper state file: {e}")))?;
        tokio::fs::rename(&tmp, &self.path)
            .await
            .map_err(|e| crate::Error::Internal(format!("shipper state file: {e}")))
    }
}

// ---------------------------------------------------------------------------- shipper

/// The instances whose logs the shipper reads: (tenant, instance). Cards I-4 and I-6 pass one
/// built on the Docker driver's instance list (the driver trait has no list call; the gap is
/// named in the impl notes of O-1b).
pub type InstanceList = Arc<dyn Fn() -> Vec<(TenantId, InstanceId)> + Send + Sync>;

/// The caps of [`LogShipper`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShipperConfig {
    /// The lines read from one instance per tick (one batch).
    pub lines_per_instance: usize,
    /// The lines read from all instances per tick; split evenly between the tenants that have
    /// instances (fair share, S-61), so the memory of one tick is bounded. Within a tenant the
    /// instances take turns: each tick starts at the first instance the last tick did not
    /// reach, so a busy instance cannot starve the tenant's other instances.
    pub lines_per_tick: usize,
    /// The positions kept; positions of instances that are no longer listed go first.
    pub max_positions: usize,
    /// The time between ticks of [`LogShipper::run`].
    pub interval: Duration,
}

impl Default for ShipperConfig {
    fn default() -> Self {
        ShipperConfig {
            lines_per_instance: 500,
            lines_per_tick: 5_000,
            max_positions: 50_000,
            interval: Duration::from_secs(2),
        }
    }
}

/// What one tick did (counts only).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShipReport {
    pub instances: u64,
    pub lines: u64,
    pub stored: u64,
    /// Values and texts in which the pipeline's secret matcher found a secret (counted by the
    /// matcher, never by matching the redaction marker; S-60).
    pub secrets_redacted: u64,
    pub failed_batches: u64,
}

/// The driver log shipper. See the module docs.
pub struct LogShipper {
    driver: Arc<dyn RuntimeDriver>,
    instances: InstanceList,
    ingest: Arc<dyn TelemetryIngest>,
    store: Arc<dyn PositionStore>,
    positions: tokio::sync::Mutex<HashMap<InstanceId, u64>>,
    /// Per tenant: the index (in the tenant's instance list) where the next tick starts.
    cursors: tokio::sync::Mutex<HashMap<TenantId, usize>>,
    cfg: ShipperConfig,
}

impl LogShipper {
    /// Loads the saved positions.
    pub async fn new(
        driver: Arc<dyn RuntimeDriver>,
        instances: InstanceList,
        ingest: Arc<dyn TelemetryIngest>,
        store: Arc<dyn PositionStore>,
        cfg: ShipperConfig,
    ) -> Result<Self> {
        let positions = store.load().await?;
        Ok(LogShipper {
            driver,
            instances,
            ingest,
            store,
            positions: tokio::sync::Mutex::new(positions),
            cursors: tokio::sync::Mutex::new(HashMap::new()),
            cfg,
        })
    }

    /// The saved position of an instance.
    pub async fn position(&self, instance: InstanceId) -> Option<u64> {
        self.positions.lock().await.get(&instance).copied()
    }

    /// Reads and ships one batch per instance, within the caps.
    pub async fn tick(&self) -> ShipReport {
        let mut report = ShipReport::default();
        let listed = (self.instances)();
        let mut by_tenant: BTreeMap<TenantId, Vec<InstanceId>> = BTreeMap::new();
        for (tenant, instance) in &listed {
            by_tenant.entry(*tenant).or_default().push(*instance);
        }
        let tenants = by_tenant.len().max(1);
        let tenant_lines = (self.cfg.lines_per_tick / tenants).max(1);
        let mut positions = self.positions.lock().await;
        let mut cursors = self.cursors.lock().await;
        let mut next_cursors = HashMap::with_capacity(by_tenant.len());
        for (tenant, instances) in &by_tenant {
            let n = instances.len();
            let start = cursors.get(tenant).copied().unwrap_or(0) % n;
            // When every instance was reached, the next tick still starts one further on.
            let mut next = (start + 1) % n;
            let mut left = tenant_lines;
            let mut shipped = false;
            for k in 0..n {
                let idx = (start + k) % n;
                if left == 0 {
                    next = idx;
                    break;
                }
                let instance = &instances[idx];
                report.instances += 1;
                let limit = self.cfg.lines_per_instance.min(left).max(1);
                let after = positions.get(instance).copied();
                let lines = match self.driver.logs(*tenant, *instance, after, limit).await {
                    Ok(lines) => lines,
                    Err(RuntimeError::NotFound { .. }) => {
                        positions.remove(instance);
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!(tenant = %tenant, instance = %instance, code = e.code(), "telemetry shipper: reading the driver logs failed");
                        continue;
                    }
                };
                // A driver that ignores `limit` cannot make the shipper hold more.
                let lines: Vec<LogLine> = lines
                    .into_iter()
                    .filter(|l| l.instance == *instance && l.identity.tenant == *tenant)
                    .take(limit)
                    .collect();
                let Some(last) = lines.last().map(|l| l.seq) else {
                    continue;
                };
                left = left.saturating_sub(lines.len());
                report.lines += lines.len() as u64;
                match self.ship(lines).await {
                    Ok(r) => {
                        report.stored += r.stored_logs;
                        report.secrets_redacted += r.secrets_redacted;
                        positions.insert(*instance, last);
                        shipped = true;
                    }
                    Err(e) => {
                        report.failed_batches += 1;
                        tracing::warn!(tenant = %tenant, instance = %instance, error = %e, "telemetry shipper: a batch was not stored; it is read again on the next tick");
                    }
                }
            }
            next_cursors.insert(*tenant, next);
            // Saved once per tenant: a crash repeats at most the batches of one tenant's turn.
            if shipped && let Err(e) = self.store.save(&positions).await {
                tracing::warn!(error = %e, "telemetry shipper: saving the positions failed");
            }
        }
        // Only the listed tenants keep a cursor, so the map stays bounded.
        *cursors = next_cursors;
        if positions.len() > self.cfg.max_positions {
            let listed: std::collections::HashSet<InstanceId> =
                listed.iter().map(|(_, i)| *i).collect();
            positions.retain(|i, _| listed.contains(i));
        }
        report
    }

    /// Ships the lines of one instance: one batch per identity the driver attached.
    async fn ship(&self, lines: Vec<LogLine>) -> Result<IngestReport> {
        let mut by_identity: Vec<(SourceIdentity, Batch)> = Vec::new();
        for line in lines {
            // The identity comes from the driver (the spec of the instance), never from the
            // line's text.
            let identity = SourceIdentity::from(&line.identity);
            let record = log_record(line);
            match by_identity.iter_mut().find(|(i, _)| *i == identity) {
                Some((_, b)) => b.logs.push(record),
                None => by_identity.push((
                    identity,
                    Batch {
                        logs: vec![record],
                        ..Default::default()
                    },
                )),
            }
        }
        let mut total = IngestReport::default();
        for (identity, batch) in by_identity {
            let r = self.ingest.ingest(&identity, batch).await?;
            total.stored_logs += r.stored_logs;
            total.secrets_redacted += r.secrets_redacted;
        }
        Ok(total)
    }

    /// Ticks until `stop` resolves.
    pub async fn run(self: Arc<Self>, stop: impl std::future::Future<Output = ()> + Send) {
        tokio::pin!(stop);
        loop {
            let r = self.tick().await;
            if r.lines > 0 || r.failed_batches > 0 {
                tracing::debug!(
                    instances = r.instances,
                    lines = r.lines,
                    stored = r.stored,
                    secrets_redacted = r.secrets_redacted,
                    failed = r.failed_batches,
                    "telemetry shipper tick"
                );
            }
            tokio::select! {
                _ = &mut stop => return,
                _ = tokio::time::sleep(self.cfg.interval) => {}
            }
        }
    }
}

/// The OTel attribute of the output stream of a line.
pub const ATTR_IOSTREAM: &str = "log.iostream";

/// A driver log line as a log record: free text (`personal`, scrubbed), `info` for stdout and
/// `error` for stderr, and the stream as an attribute.
pub fn log_record(line: LogLine) -> LogRecord {
    let (severity, stream) = match line.stream {
        LogStream::Stdout => (Severity::Info, "stdout"),
        LogStream::Stderr => (Severity::Error, "stderr"),
    };
    LogRecord {
        time: line.at,
        severity,
        body: LogBody::Text(line.text),
        attributes: vec![Attribute::new(ATTR_IOSTREAM, stream)],
    }
}

use std::sync::Mutex;
