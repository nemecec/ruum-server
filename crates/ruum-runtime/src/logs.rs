//! The bounded log ring of one instance, the byte budget of the live rings of one process
//! (backlog S-91), and the redacting pump that fills them.

use ruum_core::ids::TenantId;
use ruum_core::runtime::redact::LineRedactor;
use ruum_core::runtime::{InstanceId, LogLine, LogStream, WorkloadIdentity};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use time::OffsetDateTime;

/// The byte budget of the live rings of one driver (backlog S-91, Kimi re-check of the I6-1 fix;
/// `docs/STEP3.md` "Live log rings"): the bytes of every ring that counts to it (each line's text
/// allocation and its [`LogLine`] record), in all and per tenant. A ring counts while its
/// instance runs; an ended or scaled-to-zero ring leaves it ([`Ring::leave_budget`]) and counts
/// to the driver's ended cap instead.
///
/// As a line arrives, the arriving ring drops its own oldest lines (the new line too when the
/// ring is empty) while its tenant is past `max_tenant`, or while the total is past `max_total +
/// max_tenant`: that is a hard bound. The driver's sweep brings the total back under `max_total`
/// by taking from the tenant with the most bytes first ([`LogBudget::over_total`] and
/// [`Ring::shed`]), so a quiet tenant keeps its lines.
pub(crate) struct LogBudget {
    max_total: usize,
    max_tenant: usize,
    st: Mutex<BudgetSt>,
}

#[derive(Default)]
struct BudgetSt {
    total: usize,
    tenants: HashMap<TenantId, usize>,
}

impl LogBudget {
    pub(crate) fn new(max_total: usize, max_tenant: usize) -> Arc<Self> {
        Arc::new(Self {
            max_total,
            max_tenant: max_tenant.min(max_total),
            st: Mutex::new(BudgetSt::default()),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BudgetSt> {
        self.st.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn add(&self, tenant: TenantId, bytes: usize) {
        let mut st = self.lock();
        st.total += bytes;
        *st.tenants.entry(tenant).or_default() += bytes;
    }

    fn sub(&self, tenant: TenantId, bytes: usize) {
        let mut st = self.lock();
        st.total = st.total.saturating_sub(bytes);
        if let Some(t) = st.tenants.get_mut(&tenant) {
            *t = t.saturating_sub(bytes);
            if *t == 0 {
                st.tenants.remove(&tenant);
            }
        }
    }

    /// Whether an arriving line of `tenant` must make room in its own ring.
    fn over(&self, tenant: TenantId) -> bool {
        let st = self.lock();
        st.total > self.max_total.saturating_add(self.max_tenant)
            || st.tenants.get(&tenant).copied().unwrap_or(0) > self.max_tenant
    }

    /// The bytes past `max_total`, and the tenant with the most bytes (the sweep's victim).
    pub(crate) fn over_total(&self) -> Option<(usize, TenantId)> {
        let st = self.lock();
        if st.total <= self.max_total {
            return None;
        }
        let top = st
            .tenants
            .iter()
            .max_by_key(|(_, b)| **b)
            .map(|(t, _)| *t)?;
        Some((st.total - self.max_total, top))
    }

    /// The bytes counted, in all and for `tenant` (tests and operations).
    pub(crate) fn used(&self, tenant: Option<TenantId>) -> usize {
        let st = self.lock();
        match tenant {
            None => st.total,
            Some(t) => st.tenants.get(&t).copied().unwrap_or(0),
        }
    }
}

/// A ring's place in a [`LogBudget`].
struct Budgeted {
    budget: Arc<LogBudget>,
    tenant: TenantId,
    /// Whether the ring's bytes count now (a live instance).
    counted: bool,
}

/// The lines of one instance, oldest first, bounded by a line count and a byte count (and by
/// the driver's live budget while it counts there). The oldest lines go first, so `seq` can start
/// with a gap.
pub(crate) struct Ring {
    lines: VecDeque<LogLine>,
    bytes: usize,
    next_seq: u64,
    max_lines: usize,
    max_bytes: usize,
    max_line_bytes: usize,
    budget: Option<Budgeted>,
}

/// What one kept line costs besides its text.
const LINE_OVERHEAD: usize = std::mem::size_of::<LogLine>();

impl Drop for Ring {
    fn drop(&mut self) {
        if let Some(b) = &self.budget
            && b.counted
        {
            b.budget.sub(b.tenant, self.memory());
        }
    }
}

impl Ring {
    pub(crate) fn new(max_lines: usize, max_bytes: usize, max_line_bytes: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            bytes: 0,
            next_seq: 1,
            max_lines,
            max_bytes,
            max_line_bytes,
            budget: None,
        }
    }

    /// The ring counts to `budget` for `tenant` from now on (a live instance).
    pub(crate) fn with_budget(mut self, budget: Arc<LogBudget>, tenant: TenantId) -> Self {
        budget.add(tenant, self.memory());
        self.budget = Some(Budgeted {
            budget,
            tenant,
            counted: true,
        });
        self.fit_budget();
        self
    }

    /// The instance ended or scaled to zero: its bytes leave the live budget.
    pub(crate) fn leave_budget(&mut self) {
        let held = self.memory();
        if let Some(b) = &mut self.budget
            && b.counted
        {
            b.counted = false;
            b.budget.sub(b.tenant, held);
        }
    }

    /// The instance runs again (a wake): its bytes count to the live budget again, and its own
    /// oldest lines go while a bound is passed.
    pub(crate) fn join_budget(&mut self) {
        let held = self.memory();
        if let Some(b) = &mut self.budget
            && !b.counted
        {
            b.counted = true;
            b.budget.add(b.tenant, held);
        }
        self.fit_budget();
    }

    /// Drops the oldest line (and its bytes from the budget). `false`: the ring is empty.
    fn pop_oldest(&mut self) -> bool {
        match self.lines.pop_front() {
            Some(l) => {
                let n = l.text.capacity();
                self.bytes -= n;
                let n = n + LINE_OVERHEAD;
                if let Some(b) = &self.budget
                    && b.counted
                {
                    b.budget.sub(b.tenant, n);
                }
                true
            }
            None => false,
        }
    }

    /// The sweep's part of the budget: drops this ring's oldest lines until `bytes` are freed or
    /// the ring is empty. Returns what it freed.
    pub(crate) fn shed(&mut self, bytes: usize) -> usize {
        let before = self.memory();
        while before - self.memory() < bytes && self.pop_oldest() {}
        before - self.memory()
    }

    /// Whether the ring counts to a live budget now.
    pub(crate) fn counted(&self) -> bool {
        self.budget.as_ref().is_some_and(|b| b.counted)
    }

    /// Drops this ring's oldest lines while the live budget (the tenant's share, or the hard
    /// bound in all) is passed.
    fn fit_budget(&mut self) {
        loop {
            let over = match &self.budget {
                Some(b) if b.counted => b.budget.over(b.tenant),
                _ => false,
            };
            if !over || !self.pop_oldest() {
                return;
            }
        }
    }

    /// Adds a redacted line. A line over the cap is cut at the cap (at a character boundary);
    /// the cut part is gone. The redactors of the pump cut at the same cap before their check
    /// ([`Redactors::new`]), so this cut only guards: cutting a line after the redactor's check
    /// could join its kept start to the next line and rebuild a multiline value (Kimi review of
    /// run 12, K11 finding 2).
    ///
    /// The ring keeps a copy of the kept bytes, not the line's first allocation, and counts the
    /// allocation it keeps: a cut 1 MiB line holds the cap, not 1 MiB (Kimi review of run 12,
    /// K11 finding 1).
    pub(crate) fn push(
        &mut self,
        instance: InstanceId,
        identity: WorkloadIdentity,
        stream: LogStream,
        at: OffsetDateTime,
        mut text: String,
    ) {
        if text.len() > self.max_line_bytes {
            let mut cut = self.max_line_bytes;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            // `truncate` would keep the whole allocation: copy the kept part instead.
            text = text[..cut].to_owned();
        } else if text.capacity() > text.len() {
            text.shrink_to_fit();
        }
        let n = text.capacity();
        self.bytes += n;
        if let Some(b) = &self.budget
            && b.counted
        {
            b.budget.add(b.tenant, n + LINE_OVERHEAD);
        }
        self.lines.push_back(LogLine {
            seq: self.next_seq,
            at,
            instance,
            identity,
            stream,
            text,
        });
        self.next_seq += 1;
        while (self.lines.len() > self.max_lines || self.bytes > self.max_bytes)
            && self.pop_oldest()
        {}
        // The live budget (S-91): this ring's own oldest lines go first, the new line too when
        // the ring cannot make room.
        self.fit_budget();
    }

    /// The memory the lines take: the allocations of their text and one [`LogLine`] each.
    pub(crate) fn memory(&self) -> usize {
        self.bytes + self.lines.len() * std::mem::size_of::<LogLine>()
    }

    /// Drops every line and keeps the numbering: the next line gets the next `seq`, so a reader
    /// sees a gap, never a number twice.
    pub(crate) fn clear(&mut self) {
        if let Some(b) = &self.budget
            && b.counted
        {
            b.budget.sub(b.tenant, self.memory());
        }
        self.lines = VecDeque::new();
        self.bytes = 0;
    }

    /// The `seq` of the newest line (0 before the first).
    pub(crate) fn last_seq(&self) -> u64 {
        self.next_seq - 1
    }

    pub(crate) fn after(&self, after: Option<u64>, limit: usize) -> Vec<LogLine> {
        let from = after.map_or(0, |a| a + 1);
        let start = self.lines.partition_point(|l| l.seq < from);
        self.lines.iter().skip(start).take(limit).cloned().collect()
    }
}

/// The two redactors of one run (standard output and standard error). A rotation sets the new
/// value set on both, so a run that started before the rotation redacts the new token too. Both
/// cut lines at the ring's line cap before their check, so the check sees what the ring keeps.
pub(crate) struct Redactors {
    pub(crate) out: LineRedactor,
    pub(crate) err: LineRedactor,
}

impl Redactors {
    pub(crate) fn new(values: &[String], max_line_bytes: usize) -> Self {
        let one = || {
            LineRedactor::new(values.iter().map(String::as_str)).with_max_line_bytes(max_line_bytes)
        };
        Self {
            out: one(),
            err: one(),
        }
    }

    pub(crate) fn set(&mut self, values: &[String]) {
        self.out.set_values(values.iter().map(String::as_str));
        self.err.set_values(values.iter().map(String::as_str));
    }
}

#[cfg(test)]
mod tests {
    //! The ring and the redactors together, as the pump runs them (Kimi review of run 12, K11
    //! findings 1 and 2).

    use super::*;
    use ruum_core::ids::{NodeId, TenantId};
    use ruum_core::runtime::Environment;

    fn identity() -> WorkloadIdentity {
        WorkloadIdentity {
            tenant: TenantId::new(),
            app: NodeId::new(),
            deployment: uuid::Uuid::now_v7(),
            environment: Environment::Live,
        }
    }

    /// Writes `pieces` through the redactor of standard output into a ring with the line cap
    /// `cap`, as the pump does, and returns the stored lines joined with line ends (as a log
    /// reader shows them).
    fn through_pump(values: &[&str], cap: usize, pieces: &[&str]) -> String {
        let values: Vec<String> = values.iter().map(|v| (*v).to_owned()).collect();
        let mut r = Redactors::new(&values, cap);
        let mut ring = Ring::new(1000, usize::MAX, cap);
        let (id, who) = (InstanceId::new(), identity());
        let now = OffsetDateTime::now_utc();
        let mut lines = Vec::new();
        for p in pieces {
            lines.extend(r.out.push_bytes(p.as_bytes(), now));
        }
        lines.extend(r.out.finish(now));
        for (at, text) in lines {
            ring.push(id, who, LogStream::Stdout, at, text);
        }
        let stored: Vec<String> = ring.after(None, 1000).into_iter().map(|l| l.text).collect();
        stored.join("\n")
    }

    #[test]
    fn a_cut_line_does_not_rebuild_a_multiline_value() {
        // A small cap: "abcX" is stored as "abc", and the next line is "de".
        let secret = "abc\nde";
        let shown = through_pump(&[secret], 3, &["abcX\n", "de\n"]);
        assert!(!shown.contains(secret), "{shown:?}");

        // The default cap (16 KiB) and a value of the longest allowed length: a three-byte
        // character on the cap is cut whole, so the kept start ends two bytes before the cap.
        let cap = 16 * 1024;
        let start = "P".repeat(cap - 2);
        let secret = format!("{start}\nd");
        assert_eq!(
            secret.len(),
            ruum_core::runtime::redact::MAX_SECRET_VALUE_BYTES
        );
        let first = format!("{start}\u{20ac}tail\n");
        let shown = through_pump(&[&secret], cap, &[&first, "d\n"]);
        assert!(!shown.contains(&secret), "the value was rebuilt");
    }

    /// S-91: many noisy rings stay within the live budget in all and per tenant; a noisy tenant
    /// loses its own lines, never a quiet tenant's; an ended ring leaves the budget.
    #[test]
    fn live_rings_stay_within_the_budget() {
        let budget = LogBudget::new(64 * 1024, 16 * 1024);
        let (noisy, quiet) = (TenantId::new(), TenantId::new());
        let now = OffsetDateTime::now_utc();
        let mut q = Ring::new(1000, 4 * 1024 * 1024, 1024).with_budget(budget.clone(), quiet);
        for i in 0..8 {
            q.push(
                InstanceId::new(),
                identity(),
                LogStream::Stdout,
                now,
                format!("quiet {i:0>1000}"),
            );
        }
        let quiet_kept = q.after(None, 1000).len();
        assert_eq!(quiet_kept, 8);
        let mut rings: Vec<Ring> = (0..64)
            .map(|_| Ring::new(10_000, 4 * 1024 * 1024, 1024).with_budget(budget.clone(), noisy))
            .collect();
        for round in 0..200 {
            for r in &mut rings {
                r.push(
                    InstanceId::new(),
                    identity(),
                    LogStream::Stdout,
                    now,
                    format!("noise {round:0>1000}"),
                );
                assert!(
                    budget.used(None) <= 64 * 1024 + 16 * 1024,
                    "total {}",
                    budget.used(None)
                );
                assert!(
                    budget.used(Some(noisy)) <= 16 * 1024,
                    "tenant {}",
                    budget.used(Some(noisy))
                );
            }
        }
        let held: usize = rings.iter().map(|r| r.memory()).sum();
        assert_eq!(
            held,
            budget.used(Some(noisy)),
            "the budget counts what the rings hold"
        );
        assert_eq!(
            q.after(None, 1000).len(),
            quiet_kept,
            "the quiet tenant lost lines"
        );
        // A ring that leaves (its instance ended) frees the live budget; a dropped ring too.
        let before = budget.used(Some(noisy));
        let gone = rings[0].memory();
        rings[0].leave_budget();
        assert_eq!(budget.used(Some(noisy)), before - gone);
        rings.truncate(1);
        assert_eq!(budget.used(Some(noisy)), 0);
        // Joining again counts its bytes and trims it into the share.
        rings[0].join_budget();
        assert!(budget.used(Some(noisy)) <= 16 * 1024);
        drop(rings);
        drop(q);
        assert_eq!(budget.used(None), 0);
    }

    /// Many tenants within their shares pass the total: the hard bound holds as lines arrive,
    /// and the sweep's shedding takes from the tenant with the most bytes first.
    #[test]
    fn the_sweep_takes_from_the_largest_tenant() {
        let budget = LogBudget::new(32 * 1024, 16 * 1024);
        let now = OffsetDateTime::now_utc();
        let tenants: Vec<TenantId> = (0..4).map(|_| TenantId::new()).collect();
        let mut rings: Vec<Ring> = tenants
            .iter()
            .map(|t| Ring::new(10_000, 4 * 1024 * 1024, 1024).with_budget(budget.clone(), *t))
            .collect();
        for (i, r) in rings.iter_mut().enumerate() {
            for n in 0..(4 + 4 * i) {
                r.push(
                    InstanceId::new(),
                    identity(),
                    LogStream::Stdout,
                    now,
                    format!("{n:0>900}"),
                );
                assert!(budget.used(None) <= 48 * 1024);
            }
        }
        let small_before = rings[0].memory();
        while let Some((excess, top)) = budget.over_total() {
            let i = tenants.iter().position(|t| *t == top).unwrap();
            assert!(rings[i].shed(excess) > 0);
        }
        assert!(budget.used(None) <= 32 * 1024);
        assert_eq!(
            rings[0].memory(),
            small_before,
            "the smallest tenant lost lines"
        );
    }

    #[test]
    fn a_cut_line_keeps_only_the_cap_in_memory() {
        let cap = 16 * 1024;
        let mut ring = Ring::new(1000, 4 * 1024 * 1024, cap);
        let (id, who) = (InstanceId::new(), identity());
        let now = OffsetDateTime::now_utc();
        for _ in 0..8 {
            ring.push(id, who, LogStream::Stdout, now, "x".repeat(1024 * 1024));
        }
        // A short line in a large allocation is counted (and kept) by its length too.
        let mut short = String::with_capacity(1024 * 1024);
        short.push_str("short");
        ring.push(id, who, LogStream::Stdout, now, short);
        let held: usize = ring.lines.iter().map(|l| l.text.capacity()).sum();
        assert!(held <= 8 * cap + 64, "the ring holds {held} bytes of text");
        assert!(
            ring.memory() >= held,
            "memory() counts {} of {held} bytes held",
            ring.memory()
        );
        assert!(ring.memory() <= 8 * cap + 64 + 9 * std::mem::size_of::<LogLine>());
    }
}
