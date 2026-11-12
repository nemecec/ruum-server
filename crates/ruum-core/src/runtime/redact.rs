//! Exact-match redaction of secret values in log output (rule 7, review R6).
//!
//! A process writes its output in pieces, so a value can be split across two writes. The
//! [`LineRedactor`] keeps the not yet complete end of the output and lets out only complete
//! lines that no value can still overlap. A value with a line end in it (a PEM key) is redacted
//! as one piece too.

use std::ops::Range;
use time::OffsetDateTime;

/// The text that replaces a redacted value: the text form of the hidden-value marker
/// (`{"$ruum": "hidden", "class": "secret"}`), in the style of the message markers of
/// `AppLogRecord.message` (`<person 8f3a91>`).
pub const REDACTED: &str = "<hidden secret>";

/// A line longer than this is let out in pieces (each still redacted).
pub const MAX_LINE_BYTES: usize = 64 * 1024;

/// Replaces every exact match of a value in `text` with [`REDACTED`]. At each position the
/// longest value wins.
pub fn redact(text: &str, values: &[&str]) -> String {
    let values = normalize(values.iter().copied());
    let matches = find_matches(text, &values);
    replace(text, &matches)
}

/// Longest first, no empty values, no duplicates.
fn normalize<'a>(values: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut v: Vec<String> = values
        .into_iter()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    v.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
    v.dedup();
    v
}

fn find_matches(text: &str, values: &[String]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        if !text.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let rest = &text[i..];
        if let Some(v) = values.iter().find(|v| rest.starts_with(v.as_str())) {
            out.push(i..i + v.len());
            i += v.len();
        } else {
            i += 1;
        }
    }
    out
}

fn replace(text: &str, matches: &[Range<usize>]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for m in matches {
        out.push_str(&text[at..m.start]);
        out.push_str(REDACTED);
        at = m.end;
    }
    out.push_str(&text[at..]);
    out
}

/// Turns output pieces into redacted lines.
pub struct LineRedactor {
    values: Vec<String>,
    pending: String,
}

impl LineRedactor {
    pub fn new<'a>(values: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            values: normalize(values),
            pending: String::new(),
        }
    }

    /// Adds a value to redact from now on (a renewed token). Values are never removed.
    pub fn add(&mut self, value: &str) {
        let mut all: Vec<&str> = self.values.iter().map(String::as_str).collect();
        all.push(value);
        self.values = normalize(all);
    }

    fn longest(&self) -> usize {
        self.values.first().map_or(0, String::len)
    }

    /// Takes one piece of output written at `at` and returns the lines it completed, redacted.
    pub fn push(&mut self, piece: &str, at: OffsetDateTime) -> Vec<(OffsetDateTime, String)> {
        self.pending.push_str(piece);
        let matches = find_matches(&self.pending, &self.values);
        // Hold back the shortest end of the output that the next piece could complete into a
        // value: an end that is the start of a value. It is in the last `longest - 1` bytes.
        let len = self.pending.len();
        let from = len.saturating_sub(self.longest().saturating_sub(1));
        let safe = (from..len)
            .filter(|&k| self.pending.is_char_boundary(k))
            .find(|&k| {
                let tail = &self.pending[k..];
                self.values
                    .iter()
                    .any(|v| v.len() > tail.len() && v.starts_with(tail))
            })
            .unwrap_or(len);
        let inside = |i: usize| matches.iter().any(|m| m.start <= i && i < m.end);
        let mut cut = self.pending[..safe]
            .char_indices()
            .rev()
            .find(|&(i, c)| c == '\n' && !inside(i))
            .map(|(i, _)| i + 1);
        if cut.is_none() && self.pending.len() > MAX_LINE_BYTES {
            // A very long line: let out what is safe, at a point outside every match.
            let mut c = safe;
            while c > 0 && matches.iter().any(|m| m.start < c && c < m.end) {
                c -= 1;
            }
            while !self.pending.is_char_boundary(c) {
                c -= 1;
            }
            if c > 0 {
                cut = Some(c);
            }
        }
        let Some(cut) = cut else {
            return Vec::new();
        };
        let head: String = self.pending.drain(..cut).collect();
        let head_matches: Vec<Range<usize>> =
            matches.into_iter().filter(|m| m.end <= cut).collect();
        let text = replace(&head, &head_matches);
        let text = text.strip_suffix('\n').unwrap_or(&text);
        text.split('\n').map(|l| (at, l.to_owned())).collect()
    }

    /// The end of the output: returns the rest as lines, redacted.
    pub fn finish(&mut self, at: OffsetDateTime) -> Vec<(OffsetDateTime, String)> {
        if self.pending.is_empty() {
            return Vec::new();
        }
        let rest = std::mem::take(&mut self.pending);
        let matches = find_matches(&rest, &self.values);
        let text = replace(&rest, &matches);
        let text = text.strip_suffix('\n').unwrap_or(&text);
        text.split('\n').map(|l| (at, l.to_owned())).collect()
    }
}
