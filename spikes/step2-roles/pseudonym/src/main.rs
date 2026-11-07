//! Spike S-1, point 6: the Rust side of the pseudonym function of docs/STEP2.md ("The engine
//! security model", item 4).
//!
//!   pseudonym(v) = 'p_' || first 12 hex characters of HMAC-SHA-256(key, canonical_text(v))
//!
//! Reads `type<TAB>literal` lines (values.tsv) and prints
//! `type<TAB>literal<TAB>canonical_text<TAB>pseudonym` with the fixed test key 0x00..0x1f.
//! spike.sh compares this output with the SQL function `ruum_mask.pseudonym`.

use sha2::{Digest, Sha256};
use std::io::Read;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

fn hmac_sha256(key: &[u8], msg: &[u8]) -> Vec<u8> {
    assert!(key.len() <= 64);
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for (i, b) in key.iter().enumerate() {
        ipad[i] ^= b;
        opad[i] ^= b;
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(&inner[..]);
    outer.finalize()[..].to_vec()
}

fn pseudonym(key: &[u8], canonical: &str) -> String {
    let mac = hex::encode(hmac_sha256(key, canonical.as_bytes()));
    format!("p_{}", &mac[..12])
}

/// Numbers in their shortest exact form: trailing zeros of the fraction go, and so does a
/// trailing '.'; a zero has no sign (Postgres `trim_scale(numeric)::text`).
fn canonical_numeric(s: &str) -> String {
    let (neg, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let mut out = digits.to_string();
    if out.contains('.') {
        out = out.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    if out.chars().all(|c| c == '0' || c == '.') {
        return "0".into();
    }
    if neg { format!("-{out}") } else { out }
}

/// A float in the shortest form that reads back exactly, in the layout of Postgres `float8out`
/// (extra_float_digits >= 1): fixed notation for a decimal exponent in [-4, 15), else
/// `d.ddde+XX` with at least two exponent digits.
fn canonical_float8(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    let sign = if v.is_sign_negative() { "-" } else { "" };
    let sci = format!("{:e}", v.abs()); // shortest round-trip digits, e.g. "1.5e-5"
    let (mant, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    if v == 0.0 {
        return format!("{sign}0");
    }
    if (-4..15).contains(&exp) {
        let fixed = if exp >= 0 {
            let int_len = (exp + 1) as usize;
            if digits.len() <= int_len {
                format!("{digits}{}", "0".repeat(int_len - digits.len()))
            } else {
                format!("{}.{}", &digits[..int_len], &digits[int_len..])
            }
        } else {
            format!("0.{}{digits}", "0".repeat((-exp - 1) as usize))
        };
        format!("{sign}{fixed}")
    } else {
        let mant = if digits.len() > 1 {
            format!("{}.{}", &digits[..1], &digits[1..])
        } else {
            digits
        };
        let esign = if exp < 0 { '-' } else { '+' };
        format!("{sign}{mant}e{esign}{:02}", exp.abs())
    }
}

/// Times as RFC 3339 in UTC: `YYYY-MM-DDTHH:MM:SS[.ffffff without trailing zeros]Z`.
fn canonical_timestamptz(s: &str) -> String {
    let t = OffsetDateTime::parse(s, &Rfc3339)
        .unwrap()
        .to_offset(time::UtcOffset::UTC);
    let mut out = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    );
    let micro = t.nanosecond() / 1000;
    if micro > 0 {
        let frac = format!("{micro:06}");
        out.push('.');
        out.push_str(frac.trim_end_matches('0'));
    }
    out.push('Z');
    out
}

fn canonical(ty: &str, lit: &str) -> String {
    match ty {
        "text" | "date" | "bool" => lit.to_string(),
        "int8" => lit.parse::<i64>().unwrap().to_string(),
        "numeric" => canonical_numeric(lit),
        "float8" => canonical_float8(lit.parse::<f64>().unwrap()),
        "timestamptz" => canonical_timestamptz(lit),
        "bytea" => lit.trim_start_matches("\\x").to_ascii_lowercase(),
        "uuid" => lit.to_ascii_lowercase(),
        other => panic!("unknown type {other}"),
    }
}

fn main() {
    // RFC 4231, test case 2: the HMAC itself is right.
    let mac = hex::encode(hmac_sha256(b"Jefe", b"what do ya want for nothing?"));
    assert_eq!(
        mac,
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
    let key: Vec<u8> = (0u8..32).collect();
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();
    for line in input.lines() {
        let (ty, lit) = line.split_once('\t').unwrap_or((line, ""));
        let c = canonical(ty, lit);
        println!("{ty}\t{lit}\t{c}\t{}", pseudonym(&key, &c));
    }
}
