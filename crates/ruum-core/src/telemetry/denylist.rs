//! The platform deny-list: field names that are always `secret`, whatever
//! `telemetry-privacy.yaml` or a person says (review R6).
//!
//! # The rule
//!
//! A name is normalised: lower case, and `-`, `_`, `.`, spaces and camel-case boundaries
//! (`userPassword` -> `user`, `password`) are separators. The name is denied when
//!
//! - one of its segments is in [`SEGMENTS`] (short words that would match too much as a
//!   substring, such as `auth` in `author`), or
//! - the name without separators contains one of [`SUBSTRINGS`] (so `userPassword`,
//!   `db.password`, `X-Api-Key`, `stripe_token` and `access_token` all match).
//!
//! Over-matching is accepted: a denied name gives a hidden value, never a leak. A name such as
//! `session_count` or `tokenizer` is hidden too; the app can rename the field.

/// Words that deny a name when the name without separators contains them.
pub const SUBSTRINGS: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "apikey",
    "authorization",
    "cookie",
    "credential",
    "privatekey",
    "accesskey",
    "session",
    "bearer",
    "signature",
    "connectionstring",
];

/// Words that deny a name when one of its segments is exactly the word.
pub const SEGMENTS: &[&str] = &[
    "auth", "pwd", "pass", "jwt", "otp", "dsn", "csrf", "xsrf", "sid", "pin", "cvv", "cvc",
];

/// The segments of a name: lower case, split at `-`, `_`, `.`, spaces and camel-case
/// boundaries (`XMLHttpToken` -> `xml`, `http`, `token`).
pub fn segments(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if matches!(c, '-' | '_' | '.' | ' ' | '/' | ':') {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if c.is_uppercase() && !cur.is_empty() {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            // `userPassword` (lower then upper) and `XMLHttp` (upper, upper then lower).
            if prev.is_lowercase() || prev.is_ascii_digit() || (prev.is_uppercase() && next_lower) {
                out.push(std::mem::take(&mut cur));
            }
        }
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Whether a field name is on the deny-list (always `secret`).
pub fn is_denied(name: &str) -> bool {
    let segs = segments(name);
    if segs.iter().any(|s| SEGMENTS.contains(&s.as_str())) {
        return true;
    }
    let joined: String = segs.concat();
    SUBSTRINGS.iter().any(|w| joined.contains(w))
}
