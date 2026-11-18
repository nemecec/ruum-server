//! The bounded log ring of one instance and the redacting pump that fills it.

use ruum_core::runtime::redact::LineRedactor;
use ruum_core::runtime::{InstanceId, LogLine, LogStream, WorkloadIdentity};
use std::collections::VecDeque;
use time::OffsetDateTime;

/// The lines of one instance, oldest first, bounded by a line count and a byte count. The oldest
/// lines go first, so `seq` can start with a gap.
pub(crate) struct Ring {
    lines: VecDeque<LogLine>,
    bytes: usize,
    next_seq: u64,
    max_lines: usize,
    max_bytes: usize,
    max_line_bytes: usize,
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
        self.bytes += text.capacity();
        self.lines.push_back(LogLine {
            seq: self.next_seq,
            at,
            instance,
            identity,
            stream,
            text,
        });
        self.next_seq += 1;
        while self.lines.len() > self.max_lines || self.bytes > self.max_bytes {
            match self.lines.pop_front() {
                Some(l) => self.bytes -= l.text.capacity(),
                None => break,
            }
        }
    }

    /// The memory the lines take: the allocations of their text and one [`LogLine`] each.
    pub(crate) fn memory(&self) -> usize {
        self.bytes + self.lines.len() * std::mem::size_of::<LogLine>()
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
