//! The in-memory driver and builder: no sandbox, for unit tests and the static path of the
//! laptop. The driver interprets the conformance program ([`super::program`]) as the "code" of
//! an OCI image; the builder registers the digest of an archive as an image.
//!
//! They keep the rules of the trait that a driver can keep without a sandbox: limits are
//! recorded and the memory, disk and time limits are enforced on the program's own counters;
//! routes are recorded exactly; each app, environment and deployment gets its own network ID;
//! log lines are redacted before they are stored.

use super::program::{DISK_FULL, Step};
use super::redact::LineRedactor;
use super::*;
use crate::ids::{NodeId, TenantId};
use bytes::Bytes;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;
use tokio::task::JoinHandle;

// ------------------------------------------------------------------------------ registry

/// The images the in-memory builder made; the in-memory driver starts only these.
#[derive(Default)]
pub struct MemoryRegistry {
    images: Mutex<HashSet<OciDigest>>,
}

impl MemoryRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn push(&self, digest: &OciDigest) {
        self.images
            .lock()
            .expect("registry lock")
            .insert(digest.clone());
    }

    pub fn contains(&self, digest: &OciDigest) -> bool {
        self.images.lock().expect("registry lock").contains(digest)
    }
}

// ------------------------------------------------------------------------------ builder

/// An archive that holds these bytes fails to build in the in-memory builder.
pub const FAILING_ARCHIVE_MARKER: &[u8] = b"ruum-memory-builder:fail";

/// The runtimes the in-memory builder accepts.
pub const MEMORY_RUNTIMES: &[&str] = &["node22"];

/// The in-memory builder: the image digest is the SHA-256 of the archive.
pub struct MemoryBuilder {
    registry: Arc<MemoryRegistry>,
    usage: Arc<dyn RuntimeUsageSink>,
}

impl MemoryBuilder {
    pub fn new(registry: Arc<MemoryRegistry>, usage: Arc<dyn RuntimeUsageSink>) -> Self {
        Self { registry, usage }
    }
}

fn contains(haystack: &Bytes, needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[async_trait]
impl Builder for MemoryBuilder {
    fn name(&self) -> &str {
        "memory"
    }

    async fn build(&self, input: BuildInput) -> Result<BuildOutput, RuntimeError> {
        input.validate()?;
        if !MEMORY_RUNTIMES.contains(&input.runtime.as_str()) {
            return Err(RuntimeError::UnsupportedRuntime {
                runtime: input.runtime,
            });
        }
        let begun = Instant::now();
        let line = |text: String| BuildLogLine {
            at: crate::now(),
            text,
        };
        let mut logs = vec![line(format!(
            "building {} bytes for {}",
            input.archive.len(),
            input.runtime
        ))];
        // A build takes some time; the usage record must not be zero.
        tokio::time::sleep(Duration::from_millis(2)).await;
        let failed = contains(&input.archive, FAILING_ARCHIVE_MARKER);
        let took = begun.elapsed();
        self.usage.record(RuntimeUsage::Build {
            identity: input.identity,
            took,
        });
        if failed {
            return Err(RuntimeError::BuildFailed {
                reason: "the build script exited with code 1".to_owned(),
            });
        }
        let digest = OciDigest::of(&input.archive);
        self.registry.push(&digest);
        logs.push(line(format!("image {digest}")));
        Ok(BuildOutput {
            image: ImageRef::Oci(digest),
            logs,
            took,
            routes: BUILD_ROUTES.to_vec(),
            limits: input.limits,
        })
    }
}

// ------------------------------------------------------------------------------ driver

/// Faults the tests switch on to show that the conformance suite notices a broken driver.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Faults {
    /// Stores log lines without redaction.
    pub no_redaction: bool,
    /// Puts every instance on one network.
    pub shared_network: bool,
    /// Adds the egress proxy to the applied routes.
    pub extra_route: bool,
    /// Never scales to zero.
    pub no_idle: bool,
    /// Lets jobs run past their timeout.
    pub no_job_timeout: bool,
    /// Ignores the memory limit.
    pub no_memory_limit: bool,
}

/// The in-memory driver.
pub struct MemoryDriver {
    inner: Arc<Inner>,
}

struct Inner {
    registry: Arc<MemoryRegistry>,
    usage: Arc<dyn RuntimeUsageSink>,
    faults: Faults,
    state: Mutex<State>,
}

type NetKey = (TenantId, NodeId, Environment, Uuid);

#[derive(Default)]
struct State {
    instances: HashMap<InstanceId, Inst>,
    networks: HashMap<NetKey, NetworkId>,
}

struct Inst {
    spec: WorkloadSpec,
    steps: Vec<Step>,
    state: InstanceState,
    health: Health,
    applied: Applied,
    started_at: OffsetDateTime,
    logs: Vec<LogLine>,
    next_seq: u64,
    out: LineRedactor,
    err: LineRedactor,
    /// Grows at every end of a run, so the tasks of an earlier run do nothing.
    generation: u64,
    program: Option<JoinHandle<()>>,
    watcher: Option<JoinHandle<()>>,
    run_started: Option<Instant>,
    last_activity: Instant,
    memory_used: u64,
    disk_used: u64,
}

impl Inst {
    fn status(&self, id: InstanceId) -> InstanceStatus {
        InstanceStatus {
            id,
            identity: self.spec.identity,
            kind: self.spec.kind.name(),
            state: self.state,
            health: self.health,
            applied: self.applied.clone(),
            started_at: self.started_at,
        }
    }

    fn append(&mut self, id: InstanceId, stream: LogStream, lines: Vec<(OffsetDateTime, String)>) {
        for (at, text) in lines {
            self.logs.push(LogLine {
                seq: self.next_seq,
                at,
                instance: id,
                identity: self.spec.identity,
                stream,
                text,
            });
            self.next_seq += 1;
        }
    }

    fn write(&mut self, id: InstanceId, stream: LogStream, piece: &str) {
        let at = crate::now();
        let lines = match stream {
            LogStream::Stdout => self.out.push(piece, at),
            LogStream::Stderr => self.err.push(piece, at),
        };
        self.append(id, stream, lines);
    }

    fn flush(&mut self, id: InstanceId) {
        let at = crate::now();
        let out = self.out.finish(at);
        self.append(id, LogStream::Stdout, out);
        let err = self.err.finish(at);
        self.append(id, LogStream::Stderr, err);
    }

    /// Ends the current run: stops its tasks, flushes the logs and reports the usage.
    fn end_run(&mut self, id: InstanceId, usage: &dyn RuntimeUsageSink, next: InstanceState) {
        self.generation += 1;
        if let Some(p) = self.program.take() {
            p.abort();
        }
        if let Some(w) = self.watcher.take() {
            w.abort();
        }
        self.flush(id);
        if let Some(begun) = self.run_started.take() {
            usage.record(RuntimeUsage::Instance {
                identity: self.spec.identity,
                instance: id,
                ran: begun.elapsed(),
                memory_bytes: self.spec.limits.memory_bytes,
            });
        }
        self.state = next;
    }
}

impl MemoryDriver {
    pub fn new(registry: Arc<MemoryRegistry>, usage: Arc<dyn RuntimeUsageSink>) -> Self {
        Self::with_faults(registry, usage, Faults::default())
    }

    pub(crate) fn with_faults(
        registry: Arc<MemoryRegistry>,
        usage: Arc<dyn RuntimeUsageSink>,
        faults: Faults,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                registry,
                usage,
                faults,
                state: Mutex::new(State::default()),
            }),
        }
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("driver lock")
    }

    /// Runs `f` on the instance while the run `generation` is current and running.
    fn with<R>(
        &self,
        id: InstanceId,
        generation: u64,
        f: impl FnOnce(&mut Inst) -> R,
    ) -> Option<R> {
        let mut st = self.lock();
        let inst = st.instances.get_mut(&id)?;
        if inst.generation != generation || inst.state != InstanceState::Running {
            return None;
        }
        Some(f(inst))
    }

    fn finish(&self, id: InstanceId, generation: u64, exit: ExitStatus) {
        let mut st = self.lock();
        if let Some(inst) = st.instances.get_mut(&id)
            && inst.generation == generation
            && inst.state == InstanceState::Running
        {
            // The program task calls this itself: detach it instead of aborting it.
            inst.program = None;
            inst.end_run(id, self.usage.as_ref(), InstanceState::Exited(exit));
        }
    }

    /// Starts a run of the program (at start and at wake). The caller holds the lock.
    fn spawn_run(self: &Arc<Self>, id: InstanceId, inst: &mut Inst) {
        let generation = inst.generation;
        inst.state = InstanceState::Running;
        inst.health = Health::Unknown;
        inst.run_started = Some(Instant::now());
        inst.last_activity = Instant::now();
        inst.memory_used = 0;
        inst.disk_used = 0;
        let steps = inst.steps.clone();
        let (timeout, idle) = match &inst.spec.kind {
            WorkloadKind::Job(j) => (Some(j.timeout), None),
            WorkloadKind::Service(s) => (None, s.idle_timeout),
        };
        let me = self.clone();
        inst.program = Some(tokio::spawn(async move {
            let run = me.clone().run_steps(id, generation, steps);
            let exit = match timeout {
                Some(t) if !me.faults.no_job_timeout => match tokio::time::timeout(t, run).await {
                    Ok(exit) => exit,
                    Err(_) => Some(ExitStatus::of(ExitReason::TimedOut)),
                },
                _ => run.await,
            };
            if let Some(exit) = exit {
                me.finish(id, generation, exit);
            }
        }));
        if let Some(idle) = idle
            && !self.faults.no_idle
        {
            let me = self.clone();
            inst.watcher = Some(tokio::spawn(async move {
                me.watch_idle(id, generation, idle).await;
            }));
        }
    }

    async fn watch_idle(self: Arc<Self>, id: InstanceId, generation: u64, idle: Duration) {
        let tick = (idle / 4).max(Duration::from_millis(5));
        loop {
            tokio::time::sleep(tick).await;
            let mut st = self.lock();
            let Some(inst) = st.instances.get_mut(&id) else {
                return;
            };
            if inst.generation != generation || inst.state != InstanceState::Running {
                return;
            }
            if inst.last_activity.elapsed() >= idle {
                // This task is the watcher: detach it instead of aborting it.
                inst.watcher = None;
                inst.end_run(id, self.usage.as_ref(), InstanceState::ScaledToZero);
                return;
            }
        }
    }

    /// Interprets the program. `None` when the run ended from outside (stop, scale to zero).
    async fn run_steps(
        self: Arc<Self>,
        id: InstanceId,
        generation: u64,
        steps: Vec<Step>,
    ) -> Option<ExitStatus> {
        for step in steps {
            match step {
                Step::Print(t) => self.with(id, generation, |i| {
                    i.write(id, LogStream::Stdout, &format!("{t}\n"))
                })?,
                Step::Write(t) => {
                    self.with(id, generation, |i| i.write(id, LogStream::Stdout, &t))?
                }
                Step::Stderr(t) => self.with(id, generation, |i| {
                    i.write(id, LogStream::Stderr, &format!("{t}\n"))
                })?,
                Step::Env(name) => self.with(id, generation, |i| {
                    let value = i
                        .spec
                        .env
                        .iter()
                        .find(|v| v.name == name)
                        .map(|v| match &v.value {
                            EnvValue::Plain(p) => p.clone(),
                            EnvValue::Secret(s) => s.expose().to_owned(),
                        })
                        .unwrap_or_default();
                    i.write(id, LogStream::Stdout, &format!("{name}={value}\n"));
                })?,
                Step::Token => self.with(id, generation, |i| {
                    let token = i.spec.workload_token.expose().to_owned();
                    i.write(id, LogStream::Stdout, &format!("{token}\n"));
                })?,
                Step::Sleep(d) => tokio::time::sleep(d).await,
                Step::Exit(code) => return Some(ExitStatus::completed(code)),
                Step::Serve => {
                    self.with(id, generation, |i| {
                        if matches!(&i.spec.kind, WorkloadKind::Service(s) if s.health.is_some()) {
                            i.health = Health::Healthy;
                        }
                    })?;
                    std::future::pending::<()>().await;
                }
                Step::Unhealthy => {
                    let check = self.with(id, generation, |i| match &i.spec.kind {
                        WorkloadKind::Service(s) => s.health.clone(),
                        WorkloadKind::Job(_) => None,
                    })?;
                    if let Some(h) = check {
                        tokio::time::sleep(h.interval * h.failure_threshold).await;
                        self.with(id, generation, |i| i.health = Health::Unhealthy)?;
                    }
                    std::future::pending::<()>().await;
                }
                Step::Alloc(n) => {
                    let over = self.with(id, generation, |i| {
                        i.memory_used = i.memory_used.saturating_add(n);
                        i.memory_used > i.spec.limits.memory_bytes
                    })?;
                    if over && !self.faults.no_memory_limit {
                        return Some(ExitStatus::of(ExitReason::OutOfMemory));
                    }
                }
                Step::Disk(n) => {
                    let full = self.with(id, generation, |i| {
                        i.disk_used = i.disk_used.saturating_add(n);
                        let full = i.disk_used > i.spec.limits.disk_bytes;
                        if full {
                            i.write(id, LogStream::Stdout, &format!("{DISK_FULL}\n"));
                        }
                        full
                    })?;
                    if full {
                        return Some(ExitStatus::completed(1));
                    }
                }
            }
        }
        Some(ExitStatus::completed(0))
    }

    fn get(st: &mut State, tenant: TenantId, id: InstanceId) -> Result<&mut Inst, RuntimeError> {
        match st.instances.get_mut(&id) {
            Some(i) if i.spec.identity.tenant == tenant => Ok(i),
            _ => Err(RuntimeError::NotFound { instance: id }),
        }
    }
}

#[async_trait]
impl RuntimeDriver for MemoryDriver {
    fn name(&self) -> &str {
        "memory"
    }

    async fn start(&self, spec: WorkloadSpec) -> Result<InstanceStatus, RuntimeError> {
        spec.validate()?;
        let runs_code = match &spec.image {
            ImageRef::Oci(d) => {
                if !self.inner.registry.contains(d) {
                    return Err(RuntimeError::ImageNotFound {
                        image: d.to_string(),
                    });
                }
                true
            }
            ImageRef::Static(_) => false,
        };
        let steps = if !runs_code {
            Vec::new()
        } else if spec.command.is_empty() {
            vec![Step::Serve]
        } else {
            spec.command
                .iter()
                .map(|a| {
                    Step::parse(a).ok_or_else(|| {
                        RuntimeError::invalid("command", format!("unknown step {a:?}"))
                    })
                })
                .collect::<Result<_, _>>()?
        };
        let id = InstanceId::new();
        let secrets = if self.inner.faults.no_redaction {
            Vec::new()
        } else {
            spec.redacted_values()
        };
        let mut st = self.inner.lock();
        let network = runs_code.then(|| {
            let key = if self.inner.faults.shared_network {
                (
                    TenantId(Uuid::nil()),
                    NodeId(Uuid::nil()),
                    Environment::Live,
                    Uuid::nil(),
                )
            } else {
                (
                    spec.identity.tenant,
                    spec.identity.app,
                    spec.identity.environment,
                    spec.identity.deployment,
                )
            };
            st.networks
                .entry(key)
                .or_insert_with(|| NetworkId(format!("memory-net-{}", Uuid::now_v7().simple())))
                .clone()
        });
        let mut routes = spec.routes.clone();
        if self.inner.faults.extra_route && !routes.contains(&Route::EgressProxy) {
            routes.push(Route::EgressProxy);
        }
        let applied = Applied {
            routes,
            network,
            limits: spec.limits,
        };
        let mut inst = Inst {
            out: LineRedactor::new(secrets.iter().copied()),
            err: LineRedactor::new(secrets.iter().copied()),
            spec,
            steps,
            state: InstanceState::Starting,
            health: Health::Unknown,
            applied,
            started_at: crate::now(),
            logs: Vec::new(),
            next_seq: 1,
            generation: 0,
            program: None,
            watcher: None,
            run_started: None,
            last_activity: Instant::now(),
            memory_used: 0,
            disk_used: 0,
        };
        if runs_code {
            self.inner.spawn_run(id, &mut inst);
        } else {
            // A static image runs no code: the gateway serves the files.
            inst.state = InstanceState::Running;
            inst.health = Health::Healthy;
        }
        let status = inst.status(id);
        st.instances.insert(id, inst);
        Ok(status)
    }

    async fn status(
        &self,
        tenant: TenantId,
        instance: InstanceId,
    ) -> Result<InstanceStatus, RuntimeError> {
        let mut st = self.inner.lock();
        Ok(Inner::get(&mut st, tenant, instance)?.status(instance))
    }

    async fn stop(&self, tenant: TenantId, instance: InstanceId) -> Result<(), RuntimeError> {
        let mut st = self.inner.lock();
        let inst = Inner::get(&mut st, tenant, instance)?;
        if inst.state != InstanceState::Stopped {
            inst.end_run(instance, self.inner.usage.as_ref(), InstanceState::Stopped);
        }
        Ok(())
    }

    async fn touch(&self, tenant: TenantId, instance: InstanceId) -> Result<(), RuntimeError> {
        let mut st = self.inner.lock();
        let inst = Inner::get(&mut st, tenant, instance)?;
        match inst.state {
            InstanceState::Running | InstanceState::ScaledToZero
                if matches!(inst.spec.kind, WorkloadKind::Service(_)) =>
            {
                inst.last_activity = Instant::now();
                Ok(())
            }
            s => Err(RuntimeError::state("touch", &s)),
        }
    }

    async fn wake(
        &self,
        tenant: TenantId,
        instance: InstanceId,
    ) -> Result<InstanceStatus, RuntimeError> {
        let mut st = self.inner.lock();
        let inst = Inner::get(&mut st, tenant, instance)?;
        if !matches!(inst.spec.kind, WorkloadKind::Service(_)) {
            return Err(RuntimeError::state("wake", &inst.state));
        }
        match inst.state {
            InstanceState::Running => inst.last_activity = Instant::now(),
            InstanceState::ScaledToZero => self.inner.spawn_run(instance, inst),
            s => return Err(RuntimeError::state("wake", &s)),
        }
        Ok(inst.status(instance))
    }

    async fn logs(
        &self,
        tenant: TenantId,
        instance: InstanceId,
        after: Option<u64>,
        limit: usize,
    ) -> Result<Vec<LogLine>, RuntimeError> {
        let mut st = self.inner.lock();
        let inst = Inner::get(&mut st, tenant, instance)?;
        let after = after.unwrap_or(0);
        Ok(inst
            .logs
            .iter()
            .filter(|l| l.seq > after)
            .take(limit)
            .cloned()
            .collect())
    }

    async fn rotate_token(
        &self,
        tenant: TenantId,
        instance: InstanceId,
        token: SecretValue,
    ) -> Result<(), RuntimeError> {
        if token.expose().is_empty() {
            return Err(RuntimeError::invalid("workload_token", "must not be empty"));
        }
        let mut st = self.inner.lock();
        let inst = Inner::get(&mut st, tenant, instance)?;
        match inst.state {
            InstanceState::Starting | InstanceState::Running | InstanceState::ScaledToZero => {}
            s => return Err(RuntimeError::state("rotate the token of", &s)),
        }
        if !self.inner.faults.no_redaction {
            inst.out.add(token.expose());
            inst.err.add(token.expose());
        }
        inst.spec.workload_token = token;
        Ok(())
    }
}

impl Drop for MemoryDriver {
    fn drop(&mut self) {
        if let Ok(mut st) = self.inner.state.lock() {
            for inst in st.instances.values_mut() {
                if let Some(p) = inst.program.take() {
                    p.abort();
                }
                if let Some(w) = inst.watcher.take() {
                    w.abort();
                }
            }
        }
    }
}
