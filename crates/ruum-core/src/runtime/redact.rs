//! Exact-match redaction of secret values in log output (rule 7, review R6).
//!
//! A process writes its output in pieces, so a value can be split across two writes. The
//! [`LineRedactor`] keeps the not yet complete end of the output and lets out only complete
//! lines that no value can still overlap. A value with a line end in it (a PEM key) is redacted
//! as one piece too.
//!
//! The output of an app is hostile input, and the redactor runs on the platform side of the
//! sandbox (in the driver's log shipper). Its cost is linear in the output, whatever the values
//! are (backlog S-36): the values form one Aho-Corasick automaton (a trie with failure links,
//! written here, no extra dependency), and each byte of output goes through it once. The
//! automaton gives, for each position, the longest value that starts there and the longest end
//! of the output that could still grow into a value, without comparing a value byte by byte. A
//! value that overlaps itself (`"a"` × (L−1) + `"b"`) written one byte at a time costs no more
//! than any other output. Only the matches that end at the same byte cost extra (one step each),
//! and only values that are suffixes of one another make several.
//!
//! The redaction is exact-match only: a value written in another form (hex, base64, reversed,
//! one character per line) is not found. That is the known limit of rule 7 at this layer; the
//! telemetry pipeline (card O-1a) looks for encoded forms.
//!
//! A value is at most [`MAX_SECRET_VALUE_BYTES`] long, and an instance has at most
//! [`MAX_SECRET_VALUES`] values of at most [`MAX_SECRET_TOTAL_BYTES`] together. Drivers refuse
//! more before they start anything ([`check_secret_value`], [`check_secret_set`]) and keep only
//! the newest [`MAX_KEPT_TOKENS`] workload tokens of an instance, so the automaton, the
//! held-back text and the matches that end at one byte (one step each, at most one per value)
//! stay bounded.
//!
//! The marker never holds a value: when a value is part of [`REDACTED`] (a value such as
//! `secret`), the redactor uses a shorter marker, and at last an empty one ([`marker_for`]).

use std::collections::VecDeque;
use std::ops::Range;
use time::OffsetDateTime;

/// The text that replaces a redacted value: the text form of the hidden-value marker
/// (`{"$ruum": "hidden", "class": "secret"}`), in the style of the message markers of
/// `AppLogRecord.message` (`<person 8f3a91>`).
pub const REDACTED: &str = "<hidden secret>";

/// A line longer than this is let out in pieces (each still redacted).
pub const MAX_LINE_BYTES: usize = 64 * 1024;

/// The longest secret value (or workload token) a driver accepts, in bytes (16 KiB: room for a
/// PEM private key of RSA 4096). Drivers refuse a longer one with `invalid_spec`
/// ([`check_secret_value`]).
pub const MAX_SECRET_VALUE_BYTES: usize = 16 * 1024;

/// The most values to redact in one instance (secret values, previous values and workload
/// tokens). Drivers refuse a spec with more ([`check_secret_set`]).
pub const MAX_SECRET_VALUES: usize = 256;

/// The most bytes of all values of one spec together (256 KiB).
pub const MAX_SECRET_TOTAL_BYTES: usize = 256 * 1024;

/// The workload tokens a driver keeps redacting per instance: the current one and the newest
/// earlier ones. An older token is no longer redacted (it expired long before).
pub const MAX_KEPT_TOKENS: usize = 16;

/// The markers in order of preference: the first one that holds no value is used.
const MARKERS: &[&str] = &[REDACTED, "<hidden>", "***", ""];

/// The marker for a set of values: [`REDACTED`], or a shorter one when a value is part of it
/// (the empty marker holds no value).
pub fn marker_for<S: AsRef<str>>(values: &[S]) -> &'static str {
    MARKERS
        .iter()
        .copied()
        .find(|m| {
            !values
                .iter()
                .any(|v| !v.as_ref().is_empty() && m.contains(v.as_ref()))
        })
        .unwrap_or("")
}

/// Checks the values of one spec together: at most [`MAX_SECRET_VALUES`] values and
/// [`MAX_SECRET_TOTAL_BYTES`] bytes. A driver calls it with
/// [`super::WorkloadSpec::redacted_values`] before it starts anything.
pub fn check_secret_set(values: &[&str]) -> Result<(), super::RuntimeError> {
    if values.len() > MAX_SECRET_VALUES {
        return Err(super::RuntimeError::invalid(
            "env",
            format!("at most {MAX_SECRET_VALUES} secret values, previous values and tokens"),
        ));
    }
    let total: usize = values.iter().map(|v| v.len()).sum();
    if total > MAX_SECRET_TOTAL_BYTES {
        return Err(super::RuntimeError::invalid(
            "env",
            format!("the secret values are at most {MAX_SECRET_TOTAL_BYTES} bytes together"),
        ));
    }
    Ok(())
}

/// Checks the length of a value to redact: at most [`MAX_SECRET_VALUE_BYTES`]. A driver calls it
/// for every secret value, previous value and workload token before it starts anything.
pub fn check_secret_value(field: &str, value: &str) -> Result<(), super::RuntimeError> {
    if value.len() > MAX_SECRET_VALUE_BYTES {
        return Err(super::RuntimeError::invalid(
            field,
            format!("a secret value is at most {MAX_SECRET_VALUE_BYTES} bytes"),
        ));
    }
    Ok(())
}

/// Replaces every exact match of a value in `text` with the marker ([`REDACTED`], or
/// [`marker_for`] the values). At each position the longest value wins. A line in which a value
/// appears again after the replacement (a value that overlaps the marker text) becomes the
/// marker as a whole.
pub fn redact(text: &str, values: &[&str]) -> String {
    let values = normalize(values.iter().copied());
    let marker = marker_for(&values);
    let ac = Automaton::new(&values);
    let matches = ac.find_all(text.as_bytes());
    let out = replace(text, &matches, marker);
    if !ac.any(out.as_bytes()) {
        return out;
    }
    let lines: Vec<String> = out.split('\n').map(|l| scrub(l, &ac, marker)).collect();
    let out = lines.join("\n");
    if ac.any(out.as_bytes()) {
        // A value with a line end that the marker rebuilt across two lines.
        return marker.to_owned();
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

// ------------------------------------------------------------------------------ the automaton

const ROOT: u32 = 0;
const NONE: u32 = u32::MAX;

/// An Aho-Corasick automaton over the bytes of the values.
///
/// Every node is a prefix of a value. A node's failure link is the longest proper suffix of it
/// that is a node too. Feeding the output byte by byte keeps the state at the longest end of the
/// output that is a node; the matches that end at a byte are the state and the terminal nodes
/// on its failure chain ([`Automaton::dict`]). The amortized cost per byte is constant.
struct Automaton {
    /// The edges of each node, sorted by byte.
    edges: Vec<Vec<(u8, u32)>>,
    fail: Vec<u32>,
    depth: Vec<u32>,
    /// True when the node is a whole value.
    terminal: Vec<bool>,
    /// The next terminal node on the failure chain (not the node itself), or [`NONE`].
    dict: Vec<u32>,
    /// The longest suffix of the node (the node itself or one on its failure chain) that is a
    /// proper prefix of a value: a node with edges. [`ROOT`] when there is none.
    open: Vec<u32>,
}

impl Automaton {
    fn new(values: &[String]) -> Self {
        let mut a = Automaton {
            edges: vec![Vec::new()],
            fail: vec![ROOT],
            depth: vec![0],
            terminal: vec![false],
            dict: vec![NONE],
            open: vec![ROOT],
        };
        for v in values {
            let mut n = ROOT;
            for &b in v.as_bytes() {
                n = match a.edge(n, b) {
                    Some(next) => next,
                    None => {
                        let next = a.edges.len() as u32;
                        a.edges.push(Vec::new());
                        a.fail.push(ROOT);
                        a.depth.push(a.depth[n as usize] + 1);
                        a.terminal.push(false);
                        a.dict.push(NONE);
                        a.open.push(ROOT);
                        let e = &mut a.edges[n as usize];
                        let at = e.partition_point(|(x, _)| *x < b);
                        e.insert(at, (b, next));
                        next
                    }
                };
            }
            a.terminal[n as usize] = true;
        }
        // Breadth first: a node's failure link is shallower, so it is done before the node.
        let mut queue: VecDeque<u32> = VecDeque::new();
        for &(_, child) in &a.edges[ROOT as usize] {
            queue.push_back(child);
        }
        while let Some(n) = queue.pop_front() {
            let ni = n as usize;
            // The failure link of `n` was set when its parent was taken from the queue.
            let f = a.fail[ni];
            a.dict[ni] = if a.terminal[f as usize] {
                f
            } else {
                a.dict[f as usize]
            };
            a.open[ni] = if a.edges[ni].is_empty() {
                a.open[f as usize]
            } else {
                n
            };
            for i in 0..a.edges[ni].len() {
                let (b, child) = a.edges[ni][i];
                let mut f = a.fail[ni];
                let link = loop {
                    if let Some(x) = a.edge(f, b) {
                        break x;
                    }
                    if f == ROOT {
                        break ROOT;
                    }
                    f = a.fail[f as usize];
                };
                a.fail[child as usize] = link;
                queue.push_back(child);
            }
        }
        a
    }

    fn edge(&self, n: u32, b: u8) -> Option<u32> {
        let e = &self.edges[n as usize];
        e.binary_search_by_key(&b, |(x, _)| *x).ok().map(|i| e[i].1)
    }

    /// The state after one more byte.
    fn step(&self, mut n: u32, b: u8) -> u32 {
        loop {
            if let Some(next) = self.edge(n, b) {
                return next;
            }
            if n == ROOT {
                return ROOT;
            }
            n = self.fail[n as usize];
        }
    }

    /// True when a value occurs in `text`: one pass, one step per byte (amortized), whatever
    /// the number of values.
    fn any(&self, text: &[u8]) -> bool {
        let mut n = ROOT;
        for &b in text {
            n = self.step(n, b);
            if self.terminal[n as usize] || self.dict[n as usize] != NONE {
                return true;
            }
        }
        false
    }

    /// Calls `found(len)` for each value that ends at the state `n`, longest first.
    fn matches_at(&self, n: u32, mut found: impl FnMut(usize)) {
        if self.terminal[n as usize] {
            found(self.depth[n as usize] as usize);
        }
        let mut d = self.dict[n as usize];
        while d != NONE {
            found(self.depth[d as usize] as usize);
            d = self.dict[d as usize];
        }
    }

    /// The length of the longest end of the text fed so far (state `n`) that a later byte could
    /// still grow into a value.
    fn open_len(&self, n: u32) -> usize {
        self.depth[self.open[n as usize] as usize] as usize
    }

    fn longest(&self) -> usize {
        self.depth.iter().copied().max().unwrap_or(0) as usize
    }

    /// The matches of the whole text, leftmost first, the longest value at each position.
    fn find_all(&self, text: &[u8]) -> Vec<Range<usize>> {
        let mut best = vec![0u32; text.len()];
        let mut n = ROOT;
        for (i, &b) in text.iter().enumerate() {
            n = self.step(n, b);
            self.matches_at(n, |len| record(&mut best, i + 1, len));
        }
        greedy(&best, 0, text.len())
    }
}

/// Notes a value of length `len` that ends at `end`: `best[start]` is the longest value that
/// starts at `start`. Ends come in order, so a later end at the same start is longer.
fn record(best: &mut [u32], end: usize, len: usize) {
    if let Some(start) = end.checked_sub(len) {
        let slot = &mut best[start];
        if (len as u32) > *slot {
            *slot = len as u32;
        }
    }
}

/// The leftmost-longest matches of `best[from..to]` that end at or before `to`.
fn greedy(best: &[u32], from: usize, to: usize) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut i = from;
    while i < to {
        let len = best[i] as usize;
        if len > 0 && i + len <= to {
            out.push(i..i + len);
            i += len;
        } else {
            i += 1;
        }
    }
    out
}

fn replace(text: &str, matches: &[Range<usize>], marker: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for m in matches {
        out.push_str(&text[at..m.start]);
        out.push_str(marker);
        at = m.end;
    }
    out.push_str(&text[at..]);
    out
}

/// The line, or the marker when a value still appears in it (one pass through the automaton,
/// not one search per value).
fn scrub(line: &str, ac: &Automaton, marker: &str) -> String {
    if ac.any(line.as_bytes()) {
        marker.to_owned()
    } else {
        line.to_owned()
    }
}

/// The range of `ranges` (sorted, not overlapping) that holds the byte `p`.
fn containing(ranges: &[Range<usize>], p: usize) -> Option<&Range<usize>> {
    let i = ranges.partition_point(|m| m.end <= p);
    ranges.get(i).filter(|m| m.start <= p)
}

/// Turns output pieces into redacted lines.
///
/// The state, all as positions in `pending`:
/// - `state`: the automaton state after all of `pending`.
/// - `best`: for each position, the length of the longest value that starts there and ends
///   inside `pending` (0 for none).
/// - `matches` and `scan`: the matches before `scan` are final. A match at a position is final
///   once the text holds the longest value's length from there; the rest of the text (shorter
///   than the longest value) is matched again from `best` when lines are let out.
/// - the safe end: `pending.len()` minus the longest end of `pending` that could still grow
///   into a value. No text before it can still change.
/// - `checked`: no line end before it is a cut point (each one is inside a match).
pub struct LineRedactor {
    values: Vec<String>,
    marker: &'static str,
    ac: Automaton,
    longest: usize,
    pending: String,
    /// The end of the bytes given to [`LineRedactor::push_bytes`] that is not yet a whole
    /// UTF-8 character.
    partial: Vec<u8>,
    state: u32,
    best: Vec<u32>,
    matches: Vec<Range<usize>>,
    scan: usize,
    checked: usize,
}

impl LineRedactor {
    pub fn new<'a>(values: impl IntoIterator<Item = &'a str>) -> Self {
        let values = normalize(values);
        let marker = marker_for(&values);
        let ac = Automaton::new(&values);
        let longest = ac.longest();
        Self {
            values,
            marker,
            ac,
            longest,
            pending: String::new(),
            partial: Vec::new(),
            state: ROOT,
            best: Vec::new(),
            matches: Vec::new(),
            scan: 0,
            checked: 0,
        }
    }

    /// Adds a value to redact from now on (a renewed token).
    pub fn add(&mut self, value: &str) {
        let mut all: Vec<String> = self.values.clone();
        all.push(value.to_owned());
        self.set_values(all.iter().map(String::as_str));
    }

    /// Replaces the values to redact from now on (a renewed token, with the oldest kept token
    /// dropped: [`MAX_KEPT_TOKENS`]). The held-back text goes through the new values.
    pub fn set_values<'a>(&mut self, values: impl IntoIterator<Item = &'a str>) {
        self.values = normalize(values);
        self.marker = marker_for(&self.values);
        self.ac = Automaton::new(&self.values);
        self.longest = self.ac.longest();
        // Feed the held-back text through the new automaton (once per renewal).
        self.state = ROOT;
        self.best.clear();
        self.best.resize(self.pending.len(), 0);
        self.matches.clear();
        self.scan = 0;
        self.checked = 0;
        self.advance(0);
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
        self.best.resize(self.pending.len(), 0);
        self.advance(old);
        self.take_lines(at)
    }

    /// Feeds the bytes from `old` on through the automaton and moves the final matches on.
    fn advance(&mut self, old: usize) {
        let len = self.pending.len();
        if self.longest == 0 {
            self.scan = len;
            return;
        }
        let bytes = self.pending.as_bytes();
        for (i, &b) in bytes.iter().enumerate().take(len).skip(old) {
            self.state = self.ac.step(self.state, b);
            let best = &mut self.best;
            self.ac.matches_at(self.state, |l| record(best, i + 1, l));
        }
        while self.scan + self.longest <= len {
            let l = self.best[self.scan] as usize;
            if l > 0 {
                self.matches.push(self.scan..self.scan + l);
                self.scan += l;
            } else {
                self.scan += 1;
            }
        }
    }

    /// The first byte that a later piece could still change: the start of the longest end of
    /// the text that is a proper prefix of a value.
    fn safe_end(&self) -> usize {
        self.pending.len() - self.ac.open_len(self.state)
    }

    /// Lets out the complete lines before the safe end, or a piece of a very long line.
    fn take_lines(&mut self, at: OffsetDateTime) -> Vec<(OffsetDateTime, String)> {
        let len = self.pending.len();
        let safe = self.safe_end();
        let checked = self.checked.min(safe);
        let has_line_end = self.pending.as_bytes()[checked..safe].contains(&b'\n');
        if !has_line_end && len <= MAX_LINE_BYTES {
            self.checked = safe;
            return Vec::new();
        }
        // The text after `scan` is shorter than the longest value: its matches as they stand.
        // Before `safe` they are final too (no value can start there and still grow).
        let tentative = greedy(&self.best, self.scan, len);
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
        self.best.drain(..cut);
        let split = self.matches.partition_point(|m| m.end <= cut);
        let mut head_matches: Vec<Range<usize>> = self.matches.drain(..split).collect();
        head_matches.extend(tentative.into_iter().filter(|m| m.end <= cut));
        for m in &mut self.matches {
            m.start -= cut;
            m.end -= cut;
        }
        self.scan = self.scan.max(cut) - cut;
        self.checked = safe - cut;
        let text = replace(&head, &head_matches, self.marker);
        self.lines(&text, at)
    }

    fn lines(&self, text: &str, at: OffsetDateTime) -> Vec<(OffsetDateTime, String)> {
        let text = text.strip_suffix('\n').unwrap_or(text);
        text.split('\n')
            .map(|l| (at, scrub(l, &self.ac, self.marker)))
            .collect()
    }

    /// The end of the output: returns the rest as lines, redacted.
    pub fn finish(&mut self, at: OffsetDateTime) -> Vec<(OffsetDateTime, String)> {
        let mut rest = std::mem::take(&mut self.pending);
        if !self.partial.is_empty() {
            rest.push_str(&String::from_utf8_lossy(&std::mem::take(&mut self.partial)));
        }
        self.state = ROOT;
        self.best.clear();
        self.matches.clear();
        self.scan = 0;
        self.checked = 0;
        if rest.is_empty() {
            return Vec::new();
        }
        let matches = self.ac.find_all(rest.as_bytes());
        let text = replace(&rest, &matches, self.marker);
        self.lines(&text, at)
    }
}

/// The plain leftmost-longest search, value by value at each position: the reference the tests
/// check the automaton against. Quadratic; never used on app output.
#[cfg(test)]
pub(crate) fn find_matches_naive(text: &str, values: &[&str]) -> Vec<Range<usize>> {
    let values = normalize(values.iter().copied());
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        if !text.is_char_boundary(i) {
            i += 1;
            continue;
        }
        if let Some(v) = values.iter().find(|v| text[i..].starts_with(v.as_str())) {
            out.push(i..i + v.len());
            i += v.len();
        } else {
            i += 1;
        }
    }
    out
}

/// The automaton's leftmost-longest search, for the tests.
#[cfg(test)]
pub(crate) fn find_matches(text: &str, values: &[&str]) -> Vec<Range<usize>> {
    let values = normalize(values.iter().copied());
    Automaton::new(&values).find_all(text.as_bytes())
}
