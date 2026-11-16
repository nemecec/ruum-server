//! The secret matcher: the first stage of the pipeline (review R6).
//!
//! It is built from the app's current and previous secret values and the workload token.
//! OTel spans reach the processor without passing the runtime's log shipper, so the processor
//! redacts too. Exact matches use `runtime::redact` (the same redactor as the log shipper); this
//! module adds the common encodings of each value as more needles:
//!
//! - standard and URL-safe base64, with and without padding, at the three alignments (the
//!   characters that only the value's bytes decide, so the value matches also inside a longer
//!   base64 text such as `user:value` in a `Basic` header);
//! - lower and upper case hex;
//! - percent-encoding (upper and lower case escapes, and the form variant with `+` for a space);
//! - JSON string escaping (once and twice, and with `\/`).
//!
//! It also reads each text in decoded forms and looks for every needle in them: percent-decoded
//! (`%XX` once, also with `+` read as a space, and twice for double encoding) and
//! JSON-unescaped (`\"`, `\\`, `\/`, `\uXXXX` and the one-letter escapes). So a partial
//! percent-encoding that keeps some reserved characters (JS `encodeURI` keeps `/` and `+`,
//! Python `quote` keeps `/`), a percent-encoded base64 form and a JSON `\uXXXX` escape all match
//! without a needle of their own.
//!
//! # What it cannot catch
//!
//! - A value split across two records or two values (`"sk_" + rest` in two fields).
//! - A value that app code transformed in another way: hashed, encrypted, compressed, cut,
//!   reversed, upper-cased, or in an encoding not listed above (base32, UTF-16, a custom
//!   escape, more than two rounds of percent-encoding, a mix of two encodings other than the
//!   decoded forms above).
//! - Values shorter than [`MIN_RAW_BYTES`], and encodings shorter than [`MIN_ENCODED_BYTES`]
//!   (they would hide too much ordinary text).
//! - Secrets that are not given to it (the app's own keys from elsewhere): the deny-list and the
//!   token patterns of the free-text scrubber cover part of those.

use crate::runtime::redact;
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use std::fmt;

/// A secret shorter than this is not matched (it would hide ordinary text).
pub const MIN_RAW_BYTES: usize = 4;
/// An encoded form shorter than this is not matched.
pub const MIN_ENCODED_BYTES: usize = 8;

/// Finds the secret values and their encodings in text. `Debug` shows the count only.
#[derive(Clone, Default)]
pub struct SecretMatcher {
    needles: Vec<String>,
}

impl fmt::Debug for SecretMatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretMatcher({} needles)", self.needles.len())
    }
}

impl SecretMatcher {
    pub fn new<'a>(values: impl IntoIterator<Item = &'a str>) -> Self {
        let mut needles = Vec::new();
        for v in values {
            if v.len() < MIN_RAW_BYTES {
                continue;
            }
            needles.push(v.to_owned());
            needles.extend(encodings(v));
        }
        needles.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
        needles.dedup();
        SecretMatcher { needles }
    }

    pub fn is_empty(&self) -> bool {
        self.needles.is_empty()
    }

    /// Whether the text holds a secret in any form: a needle in the text or in one of its
    /// decoded forms ([`decoded`]).
    pub fn contains(&self, text: &str) -> bool {
        if self.needles.is_empty() {
            return false;
        }
        self.contains_needle(text) || decoded(text).iter().any(|d| self.contains_needle(d))
    }

    fn contains_needle(&self, text: &str) -> bool {
        self.needles.iter().any(|n| text.contains(n.as_str()))
    }

    /// The text with every match replaced by [`redact::REDACTED`]; `None` when there is no
    /// match. A secret that only a decoded form shows: the whitespace-separated word that holds
    /// it becomes the marker; when no single word holds it, the whole text does.
    pub fn redact(&self, text: &str) -> Option<String> {
        if !self.contains(text) {
            return None;
        }
        let refs: Vec<&str> = self.needles.iter().map(String::as_str).collect();
        let out = redact::redact(text, &refs);
        if !self.contains(&out) {
            return Some(out);
        }
        let mut words = String::with_capacity(out.len());
        for piece in out.split_inclusive(char::is_whitespace) {
            let (word, space) = piece.split_at(piece.trim_end_matches(char::is_whitespace).len());
            if !word.is_empty() && self.contains(word) {
                words.push_str(redact::REDACTED);
            } else {
                words.push_str(word);
            }
            words.push_str(space);
        }
        if self.contains(&words) {
            return Some(redact::REDACTED.to_owned());
        }
        Some(words)
    }
}

/// The decoded forms of a text in which the matcher also looks for its needles: percent-decoded
/// (`%XX`; also with `+` as a space) and JSON-unescaped, each applied once and then once more to
/// its own results. Forms equal to the text or to an earlier form are left out; a text without
/// `%`, `+` or `\` has none. At most 12 forms, each no longer than the text.
pub fn decoded(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if !text.contains(['%', '+', '\\']) {
        return out;
    }
    let mut frontier = vec![text.to_owned()];
    for _ in 0..2 {
        let mut next = Vec::new();
        for t in &frontier {
            for d in [
                percent_decode(t, false),
                percent_decode(t, true),
                json_unescape(t),
            ]
            .into_iter()
            .flatten()
            {
                if d != text && !out.contains(&d) {
                    out.push(d.clone());
                    next.push(d);
                }
            }
        }
        frontier = next;
    }
    out
}

/// `%XX` replaced by its byte (a `%` without two hex digits stays), and `+` by a space when
/// `plus`. Bytes that are not UTF-8 become U+FFFD: a needle is UTF-8, so a needle in the bytes
/// stays whole. `None` when there is nothing to decode.
fn percent_decode(text: &str, plus: bool) -> Option<String> {
    if !text.contains('%') && !(plus && text.contains('+')) {
        return None;
    }
    let b = text.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len()
                && b[i + 1].is_ascii_hexdigit()
                && b[i + 2].is_ascii_hexdigit() =>
            {
                out.push((hex_val(b[i + 1]) << 4) | hex_val(b[i + 2]));
                i += 3;
            }
            b'+' if plus => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

fn hex_val(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => c - b'A' + 10,
    }
}

/// JSON string escapes resolved: `\"`, `\\`, `\/`, `\b`, `\f`, `\n`, `\r`, `\t` and
/// `\uXXXX` (with surrogate pairs; a lone surrogate becomes U+FFFD). Other backslashes stay.
/// `None` when the text has no backslash.
fn json_unescape(text: &str) -> Option<String> {
    if !text.contains('\\') {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('\\') {
        out.push_str(&rest[..at]);
        let esc = &rest[at..];
        let (decoded, used) = match esc[1..].chars().next() {
            Some('"') => ('"', 2),
            Some('\\') => ('\\', 2),
            Some('/') => ('/', 2),
            Some('b') => ('\u{8}', 2),
            Some('f') => ('\u{c}', 2),
            Some('n') => ('\n', 2),
            Some('r') => ('\r', 2),
            Some('t') => ('\t', 2),
            Some('u') => unicode_escape(esc).unwrap_or(('\\', 1)),
            _ => ('\\', 1),
        };
        out.push(decoded);
        rest = &esc[used..];
    }
    out.push_str(rest);
    Some(out)
}

/// The character of `\uXXXX` (or a surrogate pair `\uD8XX\uDCXX`) at the start of `esc`, and
/// the bytes it used.
fn unicode_escape(esc: &str) -> Option<(char, usize)> {
    let unit = |s: &str| -> Option<u32> {
        let h = s.get(2..6)?;
        if s.starts_with("\\u") && h.bytes().all(|c| c.is_ascii_hexdigit()) {
            u32::from_str_radix(h, 16).ok()
        } else {
            None
        }
    };
    let hi = unit(esc)?;
    if (0xD800..0xDC00).contains(&hi) {
        if let Some(lo) = esc.get(6..).and_then(unit)
            && (0xDC00..0xE000).contains(&lo)
        {
            let c = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
            return Some((char::from_u32(c).unwrap_or('\u{fffd}'), 12));
        }
        return Some(('\u{fffd}', 6));
    }
    Some((char::from_u32(hi).unwrap_or('\u{fffd}'), 6))
}

/// The encoded forms of a value (without the value itself), each at least
/// [`MIN_ENCODED_BYTES`] long and different from the value.
pub fn encodings(value: &str) -> Vec<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    for engine in [&STANDARD_NO_PAD, &URL_SAFE_NO_PAD] {
        for shift in 0..3 {
            out.push(base64_core(engine, bytes, shift));
        }
    }
    out.push(hex::encode(bytes));
    out.push(hex::encode_upper(bytes));
    let pct = percent(bytes, true);
    out.push(pct.replace("%20", "+"));
    out.push(pct);
    out.push(percent(bytes, false));
    let json = json_escape(value);
    out.push(json_escape(&json));
    out.push(json.replace('/', "\\/"));
    out.push(json);
    out.retain(|e| e.len() >= MIN_ENCODED_BYTES && e != value);
    out.sort();
    out.dedup();
    out
}

/// The base64 characters that only the value decides when `shift` unknown bytes come before
/// it: the encoding of `shift` zero bytes and the value, without the characters that hold bits
/// of the bytes before or after.
pub(crate) fn base64_core(
    engine: &base64::engine::GeneralPurpose,
    bytes: &[u8],
    shift: usize,
) -> String {
    let mut buf = vec![0u8; shift];
    buf.extend_from_slice(bytes);
    let enc = engine.encode(&buf);
    let start = (shift * 8).div_ceil(6);
    let end = ((shift + bytes.len()) * 8) / 6;
    if end <= start {
        return String::new();
    }
    enc[start..end].to_owned()
}

/// Percent-encoding of every byte outside the unreserved set of RFC 3986.
fn percent(bytes: &[u8], upper: bool) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for &b in bytes {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            s.push(b as char);
        } else if upper {
            s.push_str(&format!("%{b:02X}"));
        } else {
            s.push_str(&format!("%{b:02x}"));
        }
    }
    s
}

/// The value as the inside of a JSON string.
fn json_escape(value: &str) -> String {
    let quoted = serde_json::to_string(value).unwrap_or_default();
    quoted
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or_default()
        .to_owned()
}
