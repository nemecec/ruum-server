//! The free-text scrubber (design 9.6): a log message without a template, an exception
//! message, a span name or event name is `personal` and scrubbed with patterns only (no name
//! recognition in step 3).
//!
//! | Pattern | Becomes |
//! |---|---|
//! | Known token prefixes (`ruum_appkey_`, `sk_live_`, `sk_test_`, `rk_live_`, `ghp_`, `gho_`, `ghs_`, `github_pat_`, `xoxb-`, `xoxp-`, `glpat-`, `AKIA` + 16) | hidden |
//! | `Bearer <token>` (the token) | hidden |
//! | JWT shape (`eyJ...` with two dots) | hidden |
//! | Emails | pseudonym |
//! | IBANs (mod-97 checked, with or without group spaces) | pseudonym |
//! | Payment card numbers (13 to 19 digits, Luhn checked, with spaces or dashes) | hidden |
//! | Phone numbers (E.164 `+` and 8 to 15 digits; national forms of 9 to 15 digits with spaces, dashes or an area code in parentheses, or with a leading `0`; North American numbers of 10 digits without separators or as `415.555.2671`, with a valid area code and exchange, not part of a decimal number) | pseudonym |
//! | Long hex runs (32 or more, with a digit and a letter) | hidden |
//! | Long base64 runs (40 or more, with a digit, an upper and a lower case letter) | hidden |
//!
//! A hidden match becomes [`REDACTED`] (`<hidden secret>`), a pseudonym match becomes
//! `<person 8f3a91c2d4e5>` (the 12 hex characters of the pseudonym ID).
//!
//! The scrubber is a hand-written scanner, linear in the input: each pass looks at each byte a
//! bounded number of times (every forward or backward walk stops at the previous match or at a
//! fixed length), and there is no backtracking regex on attacker text. A text longer than
//! [`MAX_TEXT_BYTES`] is not scanned: it becomes [`REDACTED`] as a whole (the pipeline drops
//! records with longer values before that).

use crate::privacy::HmacPseudonymizer;
use crate::runtime::redact::REDACTED;

/// The longest text the scrubber scans.
pub const MAX_TEXT_BYTES: usize = 32 * 1024;

/// What a match becomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Found {
    Hidden,
    Pseudonym,
}

/// The pattern that matched (for tests and counts).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pattern {
    TokenPrefix,
    Bearer,
    Jwt,
    Email,
    Iban,
    Card,
    Phone,
    LongHex,
    LongBase64,
}

impl Pattern {
    pub fn found(self) -> Found {
        match self {
            Pattern::Email | Pattern::Iban | Pattern::Phone => Found::Pseudonym,
            _ => Found::Hidden,
        }
    }
}

/// One match: a byte range of the text (at ASCII boundaries) and its pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    pub start: usize,
    pub end: usize,
    pub pattern: Pattern,
}

/// The counts of one scrub.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub hidden: u64,
    pub pseudonymized: u64,
}

/// The scrubbed text and its counts.
pub fn scrub(text: &str, pseudonymizer: &HmacPseudonymizer) -> (String, Counts) {
    let mut counts = Counts::default();
    if text.len() > MAX_TEXT_BYTES {
        counts.hidden += 1;
        return (REDACTED.to_owned(), counts);
    }
    let matches = find(text);
    if matches.is_empty() {
        return (text.to_owned(), counts);
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for m in matches {
        out.push_str(&text[at..m.start]);
        match m.pattern.found() {
            Found::Hidden => {
                counts.hidden += 1;
                out.push_str(REDACTED);
            }
            Found::Pseudonym => {
                counts.pseudonymized += 1;
                let p = pseudonymizer.pseudonym_of_canonical(&text[m.start..m.end]);
                out.push_str("<person ");
                out.push_str(p.id().trim_start_matches("p_"));
                out.push('>');
            }
        }
        at = m.end;
    }
    out.push_str(&text[at..]);
    (out, counts)
}

/// The matches of all patterns, sorted, without overlaps (at the same start the pattern listed
/// first wins; an earlier start wins over a later one).
pub fn find(text: &str) -> Vec<Match> {
    let b = text.as_bytes();
    let mut all = Vec::new();
    token_prefixes(text, &mut all);
    bearer(b, &mut all);
    jwt(text, &mut all);
    emails(b, &mut all);
    ibans(b, &mut all);
    cards(b, &mut all);
    phones(b, &mut all);
    runs(b, &mut all);
    all.sort_by_key(|m| (m.start, m.pattern, std::cmp::Reverse(m.end)));
    let mut out: Vec<Match> = Vec::with_capacity(all.len());
    let mut end = 0;
    for m in all {
        if m.start >= end && m.end > m.start {
            end = m.end;
            out.push(m);
        }
    }
    out
}

fn is_alnum(c: u8) -> bool {
    c.is_ascii_alphanumeric()
}

fn prev_is(b: &[u8], i: usize, f: impl Fn(u8) -> bool) -> bool {
    i > 0 && f(b[i - 1])
}

fn token_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

/// The end of the run of `f` characters from `from`.
fn run_end(b: &[u8], from: usize, f: impl Fn(u8) -> bool) -> usize {
    let mut j = from;
    while j < b.len() && f(b[j]) {
        j += 1;
    }
    j
}

const PREFIXES: &[(&str, usize)] = &[
    ("ruum_appkey_", 16),
    ("sk_live_", 8),
    ("sk_test_", 8),
    ("rk_live_", 8),
    ("ghp_", 20),
    ("gho_", 20),
    ("ghs_", 20),
    ("github_pat_", 20),
    ("xoxb-", 10),
    ("xoxp-", 10),
    ("glpat-", 16),
];

fn token_prefixes(text: &str, out: &mut Vec<Match>) {
    let b = text.as_bytes();
    for &(prefix, min) in PREFIXES {
        let mut last_end = 0;
        for (i, _) in text.match_indices(prefix) {
            if i < last_end || prev_is(b, i, is_alnum) {
                continue;
            }
            let end = run_end(b, i + prefix.len(), token_char);
            last_end = end;
            if end - (i + prefix.len()) >= min {
                out.push(Match {
                    start: i,
                    end,
                    pattern: Pattern::TokenPrefix,
                });
            }
        }
    }
    // AWS access key IDs: `AKIA` and 16 upper case letters or digits.
    for (i, _) in text.match_indices("AKIA") {
        if prev_is(b, i, is_alnum) {
            continue;
        }
        let end = run_end(b, i + 4, |c| c.is_ascii_uppercase() || c.is_ascii_digit());
        if end - (i + 4) == 16 && !(end < b.len() && is_alnum(b[end])) {
            out.push(Match {
                start: i,
                end,
                pattern: Pattern::TokenPrefix,
            });
        }
    }
}

fn bearer(b: &[u8], out: &mut Vec<Match>) {
    let lower = b.to_ascii_lowercase();
    let text = String::from_utf8_lossy(&lower);
    // `to_ascii_lowercase` keeps every byte position, and the lossy text keeps every ASCII byte
    // position up to the first invalid byte; the input is valid UTF-8, so all positions hold.
    for (i, _) in text.match_indices("bearer") {
        if prev_is(b, i, is_alnum) {
            continue;
        }
        let mut j = i + 6;
        let spaces = run_end(b, j, |c| c == b' ' || c == b'\t');
        if spaces == j || spaces - j > 4 {
            continue;
        }
        j = spaces;
        let end = run_end(b, j, |c| {
            c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'~' | b'+' | b'/' | b'=' | b'-')
        });
        if end - j >= 8 {
            out.push(Match {
                start: j,
                end,
                pattern: Pattern::Bearer,
            });
        }
    }
}

fn jwt(text: &str, out: &mut Vec<Match>) {
    let b = text.as_bytes();
    let mut last_end = 0;
    for (i, _) in text.match_indices("eyJ") {
        if i < last_end || prev_is(b, i, token_char) {
            continue;
        }
        let end = run_end(b, i, |c| token_char(c) || c == b'.');
        last_end = end;
        let dots = b[i..end].iter().filter(|&&c| c == b'.').count();
        if end - i >= 20 && dots >= 2 {
            out.push(Match {
                start: i,
                end,
                pattern: Pattern::Jwt,
            });
        }
    }
}

fn local_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'%' | b'+' | b'-')
}

fn domain_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'.' || c == b'-'
}

fn emails(b: &[u8], out: &mut Vec<Match>) {
    // Each walk stops at the previous `@` (left) or the next one (right), so every byte is
    // looked at a bounded number of times.
    let mut floor = 0;
    for i in 0..b.len() {
        if b[i] != b'@' {
            continue;
        }
        let mut start = i;
        while start > floor && local_char(b[start - 1]) {
            start -= 1;
        }
        while start < i && b[start] == b'.' {
            start += 1;
        }
        floor = i + 1;
        let mut end = run_end(b, i + 1, domain_char);
        while end > i + 1 && matches!(b[end - 1], b'.' | b'-') {
            end -= 1;
        }
        if start == i || end == i + 1 {
            continue;
        }
        let domain = &b[i + 1..end];
        let Some(dot) = domain.iter().rposition(|&c| c == b'.') else {
            continue;
        };
        let tld = &domain[dot + 1..];
        if dot == 0 || tld.len() < 2 || !tld.iter().all(u8::is_ascii_alphabetic) {
            continue;
        }
        out.push(Match {
            start,
            end,
            pattern: Pattern::Email,
        });
    }
}

/// mod-97 of an IBAN (ISO 13616): the first four characters moved to the end, letters as
/// 10..35.
pub fn iban_valid(alnum: &[u8]) -> bool {
    if !(15..=34).contains(&alnum.len()) {
        return false;
    }
    let mut rem: u32 = 0;
    for &c in alnum[4..].iter().chain(&alnum[..4]) {
        let v = match c {
            b'0'..=b'9' => u32::from(c - b'0'),
            b'A'..=b'Z' => u32::from(c - b'A') + 10,
            _ => return false,
        };
        rem = if v >= 10 {
            (rem * 100 + v) % 97
        } else {
            (rem * 10 + v) % 97
        };
    }
    rem == 1
}

fn iban_char(c: u8) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit()
}

fn ibans(b: &[u8], out: &mut Vec<Match>) {
    let mut i = 0;
    while i + 4 <= b.len() {
        let starts = b[i].is_ascii_uppercase()
            && b[i + 1].is_ascii_uppercase()
            && b[i + 2].is_ascii_digit()
            && b[i + 3].is_ascii_digit()
            && !prev_is(b, i, is_alnum);
        if !starts {
            i += 1;
            continue;
        }
        // Collect at most 34 characters, with single spaces between groups. Remember where
        // each group ends.
        let mut alnum = Vec::with_capacity(34);
        let mut ends: Vec<(usize, usize)> = Vec::new(); // (byte end, characters so far)
        let mut j = i;
        while j < b.len() && alnum.len() < 35 {
            if iban_char(b[j]) {
                alnum.push(b[j]);
                j += 1;
            } else if b[j] == b' ' && j + 1 < b.len() && iban_char(b[j + 1]) {
                ends.push((j, alnum.len()));
                j += 1;
            } else {
                break;
            }
        }
        if !(j < b.len() && is_alnum(b[j])) && alnum.len() <= 34 {
            ends.push((j, alnum.len()));
        }
        let hit = ends
            .iter()
            .rev()
            .find(|&&(_, n)| n <= 34 && iban_valid(&alnum[..n]))
            .copied();
        match hit {
            Some((end, _)) => {
                out.push(Match {
                    start: i,
                    end,
                    pattern: Pattern::Iban,
                });
                i = end;
            }
            None => i += 1,
        }
    }
}

/// The Luhn check of a card number.
pub fn luhn_valid(digits: &[u8]) -> bool {
    let mut sum = 0u32;
    for (k, &d) in digits.iter().rev().enumerate() {
        let mut v = u32::from(d - b'0');
        if k % 2 == 1 {
            v *= 2;
            if v > 9 {
                v -= 9;
            }
        }
        sum += v;
    }
    sum.is_multiple_of(10)
}

/// The digits from `i` with single separators between them, at most `max` digits. Returns the
/// digits, the byte end, and whether more digits follow (the number is longer than `max`).
fn digit_group(
    b: &[u8],
    i: usize,
    max: usize,
    sep: impl Fn(u8) -> bool,
) -> (Vec<u8>, usize, usize, bool) {
    let mut digits = Vec::with_capacity(max);
    let mut seps = 0;
    let mut j = i;
    let mut end = i;
    while j < b.len() {
        if b[j].is_ascii_digit() {
            if digits.len() == max {
                return (digits, end, seps, true);
            }
            digits.push(b[j]);
            j += 1;
            end = j;
        } else if sep(b[j]) && j + 1 < b.len() && b[j + 1].is_ascii_digit() && !digits.is_empty() {
            seps += 1;
            j += 1;
        } else {
            break;
        }
    }
    (digits, end, seps, false)
}

/// Whether position `i` continues a number (`1111 2222`: the `2` after a separator after a
/// digit). A number is only matched from its first digit.
fn continues_number(b: &[u8], i: usize) -> bool {
    i >= 2 && matches!(b[i - 1], b' ' | b'-') && b[i - 2].is_ascii_digit()
}

fn cards(b: &[u8], out: &mut Vec<Match>) {
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_digit() || prev_is(b, i, is_alnum) || continues_number(b, i) {
            i += 1;
            continue;
        }
        let (digits, end, _, more) = digit_group(b, i, 19, |c| c == b' ' || c == b'-');
        let alnum_after = end < b.len() && is_alnum(b[end]);
        if !more && !alnum_after && (13..=19).contains(&digits.len()) && luhn_valid(&digits) {
            out.push(Match {
                start: i,
                end,
                pattern: Pattern::Card,
            });
            i = end;
        } else {
            i += 1;
        }
    }
}

/// Spaces and dashes; not dots, so decimal numbers and versions do not match.
fn phone_sep(c: u8) -> bool {
    matches!(c, b' ' | b'-')
}

/// Ten digits of the North American plan: the area code and the exchange start with 2 to 9.
/// (Unix times in seconds start with `1` until 2033.)
fn nanp(digits: &[u8]) -> bool {
    digits.len() == 10 && (b'2'..=b'9').contains(&digits[0]) && (b'2'..=b'9').contains(&digits[3])
}

/// Whether the number at `start..end` is part of a decimal number or a version
/// (`1.4155552671`, `4155552671.5`).
fn in_decimal(b: &[u8], start: usize, end: usize) -> bool {
    (end + 1 < b.len() && b[end] == b'.' && b[end + 1].is_ascii_digit())
        || (start >= 2 && b[start - 1] == b'.' && b[start - 2].is_ascii_digit())
}

/// `415.555.2671` at `i`: the end, when it is a North American number that stands alone.
fn dotted_nanp(b: &[u8], i: usize) -> Option<usize> {
    let end = i + 12;
    let s = b.get(i..end)?;
    let digit_at = |k: usize| s[k].is_ascii_digit();
    let shape = (0..3).all(digit_at)
        && s[3] == b'.'
        && (4..7).all(digit_at)
        && s[7] == b'.'
        && (8..12).all(digit_at);
    if !shape || (end < b.len() && is_alnum(b[end])) || in_decimal(b, i, end) {
        return None;
    }
    let digits: Vec<u8> = s.iter().copied().filter(u8::is_ascii_digit).collect();
    nanp(&digits).then_some(end)
}

fn phones(b: &[u8], out: &mut Vec<Match>) {
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        // E.164: `+` and 8 to 15 digits.
        if c == b'+' && i + 1 < b.len() && b[i + 1].is_ascii_digit() && !prev_is(b, i, is_alnum) {
            let (digits, end, _, more) = digit_group(b, i + 1, 15, phone_sep);
            if !more && (8..=15).contains(&digits.len()) && !(end < b.len() && is_alnum(b[end])) {
                out.push(Match {
                    start: i,
                    end,
                    pattern: Pattern::Phone,
                });
                i = end;
                continue;
            }
            i += 1;
            continue;
        }
        // National forms: `(0xx) xxx xxxx`, `0xx-xxx-xxxx`, `0xxxxxxxxx`, `555 123 4567`.
        let paren =
            c == b'(' && i + 1 < b.len() && b[i + 1].is_ascii_digit() && !prev_is(b, i, is_alnum);
        let digit_start = c.is_ascii_digit()
            && !prev_is(b, i, |p| is_alnum(p) || p == b'+')
            && !continues_number(b, i);
        if digit_start && let Some(end) = dotted_nanp(b, i) {
            out.push(Match {
                start: i,
                end,
                pattern: Pattern::Phone,
            });
            i = end;
            continue;
        }
        if paren || digit_start {
            let mut j = if paren { i + 1 } else { i };
            let (mut digits, mut end, mut seps, mut more) = digit_group(b, j, 15, phone_sep);
            if paren && !more && end < b.len() && b[end] == b')' {
                // `(020) 7946 0958`: the area code in parentheses, then the rest.
                j = end + 1;
                let k = run_end(b, j, |c| c == b' ');
                if k - j <= 1 && k < b.len() && b[k].is_ascii_digit() {
                    let (rest, e, s, m) = digit_group(b, k, 15 - digits.len().min(15), phone_sep);
                    digits.extend(rest);
                    end = e;
                    seps += s + 1;
                    more = m;
                }
            } else if paren {
                i += 1;
                continue;
            }
            let leading_zero = digits.first() == Some(&b'0');
            let ok = !more
                && !(end < b.len() && is_alnum(b[end]))
                && (9..=15).contains(&digits.len())
                && (seps >= 1
                    || leading_zero
                    || (nanp(&digits) && !paren && !in_decimal(b, i, end)));
            if ok {
                out.push(Match {
                    start: i,
                    end,
                    pattern: Pattern::Phone,
                });
                i = end;
                continue;
            }
        }
        i += 1;
    }
}

fn b64_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'+' | b'/' | b'_' | b'-')
}

/// Long hex and base64 runs: maximal runs of base64 characters (and `=` padding) that stand
/// alone (no letter or digit around them).
fn runs(b: &[u8], out: &mut Vec<Match>) {
    let mut i = 0;
    while i < b.len() {
        if !b64_char(b[i]) {
            i += 1;
            continue;
        }
        let mut end = run_end(b, i, b64_char);
        let core_end = end;
        while end < b.len() && end - core_end < 2 && b[end] == b'=' {
            end += 1;
        }
        let run = &b[i..core_end];
        let has_digit = run.iter().any(u8::is_ascii_digit);
        let has_upper = run.iter().any(u8::is_ascii_uppercase);
        let has_lower = run.iter().any(u8::is_ascii_lowercase);
        let all_hex = run.iter().all(u8::is_ascii_hexdigit);
        let has_alpha = has_upper || has_lower;
        if all_hex && run.len() >= 32 && has_digit && has_alpha {
            out.push(Match {
                start: i,
                end: core_end,
                pattern: Pattern::LongHex,
            });
        } else if run.len() >= 40 && has_digit && has_upper && has_lower {
            out.push(Match {
                start: i,
                end,
                pattern: Pattern::LongBase64,
            });
        } else {
            // A long hex segment inside a longer run (`key-<hex>`): the letter and digit
            // segments between `-`, `_`, `+` and `/`.
            let mut s = i;
            while s < core_end {
                let e = run_end(b, s, is_alnum).min(core_end);
                let seg = &b[s..e];
                if seg.len() >= 32
                    && seg.iter().all(u8::is_ascii_hexdigit)
                    && seg.iter().any(u8::is_ascii_digit)
                    && seg.iter().any(u8::is_ascii_alphabetic)
                {
                    out.push(Match {
                        start: s,
                        end: e,
                        pattern: Pattern::LongHex,
                    });
                }
                s = e + 1;
            }
        }
        i = end.max(i + 1);
    }
}
