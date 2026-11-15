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
//! # What it cannot catch
//!
//! - A value split across two records or two values (`"sk_" + rest` in two fields).
//! - A value that app code transformed in another way: hashed, encrypted, compressed, cut,
//!   reversed, upper-cased, or in an encoding not listed above (base32, UTF-16, a custom
//!   escape).
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

    /// Whether the text holds a secret in any form.
    pub fn contains(&self, text: &str) -> bool {
        self.needles.iter().any(|n| text.contains(n.as_str()))
    }

    /// The text with every match replaced by [`redact::REDACTED`]; `None` when there is no
    /// match.
    pub fn redact(&self, text: &str) -> Option<String> {
        if !self.contains(text) {
            return None;
        }
        let refs: Vec<&str> = self.needles.iter().map(String::as_str).collect();
        Some(redact::redact(text, &refs))
    }
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
