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
    /// the cut part is gone. Cutting redacted text can never make a secret appear.
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
            text.truncate(cut);
        }
        self.bytes += text.len();
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
                Some(l) => self.bytes -= l.text.len(),
                None => break,
            }
        }
    }

    pub(crate) fn after(&self, after: Option<u64>, limit: usize) -> Vec<LogLine> {
        let from = after.map_or(0, |a| a + 1);
        let start = self.lines.partition_point(|l| l.seq < from);
        self.lines.iter().skip(start).take(limit).cloned().collect()
    }
}

/// The two redactors of one run (standard output and standard error). A rotation sets the new
/// value set on both, so a run that started before the rotation redacts the new token too.
pub(crate) struct Redactors {
    pub(crate) out: LineRedactor,
    pub(crate) err: LineRedactor,
}

impl Redactors {
    pub(crate) fn new(values: &[String]) -> Self {
        Self {
            out: LineRedactor::new(values.iter().map(String::as_str)),
            err: LineRedactor::new(values.iter().map(String::as_str)),
        }
    }

    pub(crate) fn set(&mut self, values: &[String]) {
        self.out.set_values(values.iter().map(String::as_str));
        self.err.set_values(values.iter().map(String::as_str));
    }
}
