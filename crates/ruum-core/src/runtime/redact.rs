//! Exact-match redaction of secret values in log output (rule 7, review R6).
//!
//! A process writes its output in pieces, so a value can be split across two writes. The
//! [`LineRedactor`] keeps the not yet complete end of the output and lets out only complete
//! lines that no value can still overlap. A value with a line end in it (a PEM key) is redacted
//! as one piece too.
//!
//! The output of an app is hostile input, and the redactor runs on the platform side of the
//! sandbox (in the driver's log shipper). Its cost is linear in the output: each piece is
//! scanned once, plus a window of the longest value's length at the end. A piece written one
//! byte at a time with no line end costs no more than one large write.

use std::ops::Range;
use time::OffsetDateTime;

/// The text that replaces a redacted value: the text form of the hidden-value marker
/// (`{"$ruum": "hidden", "class": "secret"}`), in the style of the message markers of
/// `AppLogRecord.message` (`<person 8f3a91>`).
pub const REDACTED: &str = "<hidden secret>";

/// A line longer than this is let out in pieces (each still redacted).
pub const MAX_LINE_BYTES: usize = 64 * 1024;

/// Replaces every exact match of a value in `text` with [`REDACTED`]. At each position the
/// longest value wins. A line in which a value appears again after the replacement (a value
/// that overlaps the marker text) becomes the marker as a whole.
pub fn redact(text: &str, values: &[&str]) -> String {
    let values = normalize(values.iter().copied());
    let matches = find_matches(text, &values, 0);
    let out = replace(text, &matches);
    if !contains_any(&out, &values) {
        return out;
    }
    let lines: Vec<String> = out.split('\n').map(|l| scrub(l, &values)).collect();
    let out = lines.join("\n");
    if contains_any(&out, &values) {
        // A value with a line end that the marker rebuilt across two lines.
        return REDACTED.to_owned();
    }
    out
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

/// The longest value at the start of `rest` (the values are sorted longest first).
fn value_at<'v>(rest: &str, values: &'v [String]) -> Option<&'v String> {
    values.iter().find(|v| rest.starts_with(v.as_str()))
}

/// The matches in `text[from..]`, leftmost first, the longest value at each position, as
/// ranges of `text`.
fn find_matches(text: &str, values: &[String], from: usize) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut i = from;
    while i < text.len() {
        if !text.is_char_boundary(i) {
            i += 1;
            continue;
        }
        if let Some(v) = value_at(&text[i..], values) {
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

fn contains_any(text: &str, values: &[String]) -> bool {
    values.iter().any(|v| text.contains(v.as_str()))
}

/// The line, or the marker when a value still appears in it.
fn scrub(line: &str, values: &[String]) -> String {
    if contains_any(line, values) {
        REDACTED.to_owned()
    } else {
        line.to_owned()
    }
}

/// True when `tail` is the start of a value but not a whole value: the next piece could
/// complete it.
fn is_proper_prefix(tail: &str, values: &[String]) -> bool {
    values
        .iter()
        .any(|v| v.len() > tail.len() && v.starts_with(tail))
}

/// The range of `ranges` (sorted, not overlapping) that holds the byte `p`.
fn containing(ranges: &[Range<usize>], p: usize) -> Option<&Range<usize>> {
    let i = ranges.partition_point(|m| m.end <= p);
    ranges.get(i).filter(|m| m.start <= p)
}

/// Turns output pieces into redacted lines.
///
/// The state, all as positions in `pending`:
/// - `matches` and `scan`: the matches before `scan` are final. A match at a position is final
///   once the text holds the longest value's length from there; the rest of the text (shorter
///   than the longest value) is scanned again when lines are let out.
/// - `prefixes`: the positions whose end of text is the start of a value (the next piece could
///   complete it). The first one is the safe end: no text before it can still change.
/// - `checked`: no line end before it is a cut point (each one is inside a match).
pub struct LineRedactor {
    values: Vec<String>,
    pending: String,
    /// The end of the bytes given to [`LineRedactor::push_bytes`] that is not yet a whole
    /// UTF-8 character.
    partial: Vec<u8>,
    matches: Vec<Range<usize>>,
    scan: usize,
    prefixes: Vec<usize>,
    checked: usize,
}

impl LineRedactor {
    pub fn new<'a>(values: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            values: normalize(values),
            pending: String::new(),
            partial: Vec::new(),
            matches: Vec::new(),
            scan: 0,
            prefixes: Vec::new(),
            checked: 0,
        }
    }

    /// Adds a value to redact from now on (a renewed token). Values are never removed.
    pub fn add(&mut self, value: &str) {
        let mut all: Vec<&str> = self.values.iter().map(String::as_str).collect();
        all.push(value);
        self.values = normalize(all);
        // The longest value can change: scan the held-back text again (once per renewal).
        self.matches.clear();
        self.scan = 0;
        self.prefixes.clear();
        self.checked = 0;
        self.advance(0);
    }

    fn longest(&self) -> usize {
        self.values.first().map_or(0, String::len)
    }

    /// Takes one piece of raw output (bytes as the process wrote them) and returns the lines
    /// it completed, redacted. A UTF-8 character split across two pieces is joined first, so a
    /// value with non-ASCII characters is still found; bytes that are not UTF-8 become U+FFFD.
    pub fn push_bytes(
        &mut self,
        piece: &[u8],
        at: OffsetDateTime,
    ) -> Vec<(OffsetDateTime, String)> {
        self.partial.extend_from_slice(piece);
        let mut text = String::new();
        let mut rest: &[u8] = &self.partial;
        loop {
            match std::str::from_utf8(rest) {
                Ok(s) => {
                    text.push_str(s);
                    rest = &[];
                    break;
                }
                Err(e) => {
                    let (good, bad) = rest.split_at(e.valid_up_to());
                    text.push_str(std::str::from_utf8(good).unwrap_or_default());
                    match e.error_len() {
                        Some(n) => {
                            text.push('\u{FFFD}');
                            rest = &bad[n..];
                        }
                        // An unfinished character at the end: wait for the next piece.
                        None => {
                            rest = bad;
                            break;
                        }
                    }
                }
            }
        }
        self.partial = rest.to_vec();
        self.push(&text, at)
    }

    /// Takes one piece of output written at `at` and returns the lines it completed, redacted.
    pub fn push(&mut self, piece: &str, at: OffsetDateTime) -> Vec<(OffsetDateTime, String)> {
        let old = self.pending.len();
        self.pending.push_str(piece);
        self.advance(old);
        self.take_lines(at)
    }

    /// Updates the final matches and the prefix positions after text was added at `old`.
    fn advance(&mut self, old: usize) {
        let len = self.pending.len();
        let longest = self.longest();
        if longest == 0 {
            self.scan = len;
            self.prefixes.clear();
            return;
        }
        let (pending, values) = (&self.pending, &self.values);
        while self.scan + longest <= len {
            let i = self.scan;
            if !pending.is_char_boundary(i) {
                self.scan += 1;
                continue;
            }
            if let Some(v) = value_at(&pending[i..], values) {
                self.matches.push(i..i + v.len());
                self.scan += v.len();
            } else {
                self.scan += 1;
            }
        }
        // A position that was not a prefix stays one that is not; only the old prefixes and
        // the new positions in the last `longest - 1` bytes need a check.
        self.prefixes
            .retain(|&k| is_proper_prefix(&pending[k..], values));
        let from = old.max(len.saturating_sub(longest - 1));
        for k in from..len {
            if pending.is_char_boundary(k) && is_proper_prefix(&pending[k..], values) {
                self.prefixes.push(k);
            }
        }
    }

    /// Lets out the complete lines before the safe end, or a piece of a very long line.
    fn take_lines(&mut self, at: OffsetDateTime) -> Vec<(OffsetDateTime, String)> {
        let len = self.pending.len();
        let safe = self.prefixes.first().copied().unwrap_or(len);
        let checked = self.checked.min(safe);
        let has_line_end = self.pending.as_bytes()[checked..safe].contains(&b'\n');
        if !has_line_end && len <= MAX_LINE_BYTES {
            self.checked = safe;
            return Vec::new();
        }
        // The text after `scan` is shorter than the longest value: its matches as they stand.
        // Before `safe` they are final too (no value can start there and still grow).
        let tentative = find_matches(&self.pending, &self.values, self.scan);
        let inside = |p: usize| {
            containing(&self.matches, p)
                .or_else(|| containing(&tentative, p))
                .cloned()
        };
        let bytes = self.pending.as_bytes();
        let mut cut = None;
        if has_line_end {
            let mut p = safe;
            while p > checked {
                p -= 1;
                if bytes[p] == b'\n' && inside(p).is_none() {
                    cut = Some(p + 1);
                    break;
                }
            }
        }
        if cut.is_none() && len > MAX_LINE_BYTES {
            // A very long line: let out what is safe, at a point outside every match.
            let mut c = safe;
            if let Some(m) = inside(c)
                && m.start < c
            {
                c = m.start;
            }
            while !self.pending.is_char_boundary(c) {
                c -= 1;
            }
            if c > 0 {
                cut = Some(c);
            }
        }
        let Some(cut) = cut else {
            self.checked = safe;
            return Vec::new();
        };
        let head: String = self.pending.drain(..cut).collect();
        let split = self.matches.partition_point(|m| m.end <= cut);
        let mut head_matches: Vec<Range<usize>> = self.matches.drain(..split).collect();
        head_matches.extend(tentative.into_iter().filter(|m| m.end <= cut));
        for m in &mut self.matches {
            m.start -= cut;
            m.end -= cut;
        }
        self.scan = self.scan.max(cut) - cut;
        for k in &mut self.prefixes {
            *k -= cut;
        }
        self.checked = safe - cut;
        let text = replace(&head, &head_matches);
        self.lines(&text, at)
    }

    fn lines(&self, text: &str, at: OffsetDateTime) -> Vec<(OffsetDateTime, String)> {
        let text = text.strip_suffix('\n').unwrap_or(text);
        text.split('\n')
            .map(|l| (at, scrub(l, &self.values)))
            .collect()
    }

    /// The end of the output: returns the rest as lines, redacted.
    pub fn finish(&mut self, at: OffsetDateTime) -> Vec<(OffsetDateTime, String)> {
        let mut rest = std::mem::take(&mut self.pending);
        if !self.partial.is_empty() {
            rest.push_str(&String::from_utf8_lossy(&std::mem::take(&mut self.partial)));
        }
        self.matches.clear();
        self.scan = 0;
        self.prefixes.clear();
        self.checked = 0;
        if rest.is_empty() {
            return Vec::new();
        }
        let matches = find_matches(&rest, &self.values, 0);
        let text = replace(&rest, &matches);
        self.lines(&text, at)
    }
}
