//! Slugs of the apps domain (`docs/STEP3.md`, "Host names and slugs"; decision 0006-16).
//!
//! An app is reached at `https://<app-slug>.<tenant-slug>.<apps domain>`, a preview at
//! `https://<short_id>--<app-slug>.<tenant-slug>.<apps domain>`. The rules:
//!
//! - An app slug has 2 to 53 characters (so `<8-char short_id>--<slug>` fits in one 63-character
//!   DNS label), a tenant slug 2 to 40. Lower-case ASCII letters, digits and `-`.
//! - No `--` anywhere (it separates the preview prefix), no `-` at either end, no `xn--` prefix
//!   (no punycode look-alikes).
//! - Not reserved: the list of [`RESERVED`], every slug shorter than 3 characters and every slug
//!   that starts with `_`. So in practice a slug has at least 3 characters.
//! - A slug used once is never given to another tenant or app (`retired_app_slugs`; the stores
//!   check it).
//!
//! Slugs come from names ([`candidates`]): the platform turns the node name (or the tenant name)
//! into a slug, and when that slug breaks a rule or is held by another app it picks the next free
//! one from the same name. A slug that the tenant used before for another app is refused
//! (`409 conflict`, hint `slug_retired`; the service decides).

use crate::error::{ApiError, Error, ErrorCode};
use unicode_normalization::UnicodeNormalization;

/// The reserved slugs of `docs/STEP3.md` (tenant and app), copied exactly.
pub const RESERVED: &[&str] = &[
    "content",
    "www",
    "api",
    "admin",
    "static",
    "internal",
    "ruum",
    "status",
    "mail",
    "report",
    "preview",
    "app",
    "apps",
    "auth",
    "login",
    "id",
    "cdn",
    "assets",
    "docs",
    "help",
    "support",
    "blog",
    "dev",
    "staging",
    "test",
    "mta-sts",
    "autoconfig",
];

/// The longest app slug.
pub const APP_MAX: usize = 53;
/// The longest tenant slug.
pub const TENANT_MAX: usize = 40;
/// The shortest slug that is not reserved (every slug shorter than 3 characters is reserved).
pub const MIN: usize = 3;

/// The two kinds of slug.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlugKind {
    App,
    Tenant,
}

impl SlugKind {
    pub fn max(self) -> usize {
        match self {
            SlugKind::App => APP_MAX,
            SlugKind::Tenant => TENANT_MAX,
        }
    }

    /// The `kind` column of `retired_app_slugs`.
    pub fn as_str(self) -> &'static str {
        match self {
            SlugKind::App => "app",
            SlugKind::Tenant => "tenant",
        }
    }
}

/// Why a slug breaks a rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlugProblem {
    TooShort,
    TooLong,
    BadCharacter,
    DashAtEnd,
    DoubleDash,
    Punycode,
    Underscore,
    Reserved,
}

impl SlugProblem {
    pub fn message(self, kind: SlugKind) -> String {
        match self {
            SlugProblem::TooShort => format!("a slug has at least {MIN} characters"),
            SlugProblem::TooLong => format!("a slug has at most {} characters", kind.max()),
            SlugProblem::BadCharacter => {
                "a slug has lower-case letters, digits and single dashes only".into()
            }
            SlugProblem::DashAtEnd => "a slug does not start or end with a dash".into(),
            SlugProblem::DoubleDash => "a slug has no double dash".into(),
            SlugProblem::Punycode => "a slug does not start with xn--".into(),
            SlugProblem::Underscore => "a slug does not start with an underscore".into(),
            SlugProblem::Reserved => "this slug is reserved".into(),
        }
    }
}

/// Checks a slug against every rule of the module docs (not the retired list).
pub fn check(slug: &str, kind: SlugKind) -> Result<(), SlugProblem> {
    if slug.starts_with('_') {
        return Err(SlugProblem::Underscore);
    }
    if !slug
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(SlugProblem::BadCharacter);
    }
    if slug.len() < MIN {
        return Err(SlugProblem::TooShort);
    }
    if slug.len() > kind.max() {
        return Err(SlugProblem::TooLong);
    }
    if slug.starts_with("xn--") {
        return Err(SlugProblem::Punycode);
    }
    if slug.contains("--") {
        return Err(SlugProblem::DoubleDash);
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return Err(SlugProblem::DashAtEnd);
    }
    if RESERVED.contains(&slug) {
        return Err(SlugProblem::Reserved);
    }
    Ok(())
}

/// True when the slug follows every rule.
pub fn is_valid(slug: &str, kind: SlugKind) -> bool {
    check(slug, kind).is_ok()
}

/// `422 validation_failed` for a slug that breaks a rule, with a JSON pointer as hint.
pub fn invalid(slug_pointer: &str, problem: SlugProblem, kind: SlugKind) -> Error {
    Error::Api(
        ApiError::new(ErrorCode::ValidationFailed, problem.message(kind)).with_hint(slug_pointer),
    )
}

/// `409 conflict` with the hint `slug_retired`: the tenant used this slug before for another app.
pub fn retired() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::Conflict,
            "this address was used before by another app and is never given again",
        )
        .with_hint("slug_retired"),
    )
}

/// The slug part of a name: NFKD, ASCII letters and digits in lower case, every other run of
/// characters one `-`, no `-` at either end, cut to `max` characters (and no `-` at the end
/// after the cut). May be empty.
pub fn from_name(name: &str, max: usize) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in name.nfkd() {
        if c.is_ascii_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(c.to_ascii_lowercase());
        } else if c.is_alphanumeric() || c.is_whitespace() || c.is_ascii_punctuation() {
            // Letters outside ASCII (after NFKD: the base letter came first, marks are
            // dropped below), spaces and punctuation separate words.
            dash = true;
        }
        // Combining marks and other characters are dropped.
    }
    out.truncate(max);
    while out.ends_with('-') {
        out.pop();
    }
    // A cut can leave `--` only if the input had it; the loop above never writes two.
    out
}

/// The slugs to try for a name, in order: the slug of the name when it follows the rules, then
/// the same with `-2`, `-3`, ... (cut so they fit), so a clash with a rule or with another app
/// still gives a slug from the name. A name with no usable letters starts from `app`
/// (`app-2`, ... for tenants `space`). At most `n` candidates, all valid.
pub fn candidates(name: &str, kind: SlugKind, n: usize) -> Vec<String> {
    let max = kind.max();
    let mut base = from_name(name, max);
    let fallback = match kind {
        SlugKind::App => "app",
        SlugKind::Tenant => "space",
    };
    if base.is_empty() {
        base = fallback.to_string();
    }
    let mut out = Vec::new();
    if is_valid(&base, kind) {
        out.push(base.clone());
    }
    // A reserved or short base gets a suffix that makes it long enough (`id` -> `id-2`).
    let mut i = 2u32;
    while out.len() < n {
        let suffix = format!("-{i}");
        let mut stem = base.clone();
        stem.truncate(max - suffix.len());
        while stem.ends_with('-') {
            stem.pop();
        }
        if stem.is_empty() {
            stem = fallback.to_string();
        }
        let c = format!("{stem}{suffix}");
        if is_valid(&c, kind) && !out.contains(&c) {
            out.push(c);
        }
        i += 1;
        if i > 10_000 {
            break;
        }
    }
    out
}

/// `candidates` and then `spares` more with a random suffix (`<stem>-<6 base32>`), for names that
/// many tenants share (`Home`, `Team`), so a busy name never runs out of slugs.
pub fn candidates_with_spares(name: &str, kind: SlugKind, n: usize, spares: usize) -> Vec<String> {
    let mut out = candidates(name, kind, n);
    let first = out.first().cloned().unwrap_or_default();
    let mut stem = first
        .rsplit_once('-')
        .filter(|(_, tail)| tail.bytes().all(|b| b.is_ascii_digit()))
        .map(|(s, _)| s.to_string())
        .unwrap_or(first);
    stem.truncate(kind.max() - 7);
    while stem.ends_with('-') {
        stem.pop();
    }
    if stem.len() < MIN {
        stem = match kind {
            SlugKind::App => "app".into(),
            SlugKind::Tenant => "space".into(),
        };
    }
    for _ in 0..spares * 4 {
        if out.len() >= n + spares {
            break;
        }
        let c = format!("{stem}-{}", &short_id()[..6]);
        if is_valid(&c, kind) && !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

/// A short ID of a preview host: 8 characters of lower-case base32 (RFC 4648 alphabet without
/// padding, lower case).
pub fn short_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
    let bytes: [u8; 8] = rand::random();
    bytes
        .iter()
        .map(|b| ALPHABET[(*b as usize) % 32] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reserved_slug_is_refused_for_both_kinds() {
        for s in RESERVED {
            for kind in [SlugKind::App, SlugKind::Tenant] {
                let got = check(s, kind);
                assert!(
                    got == Err(SlugProblem::Reserved) || got == Err(SlugProblem::TooShort),
                    "{s} ({kind:?}): {got:?}"
                );
                assert!(!is_valid(s, kind), "{s} is reserved");
            }
        }
        // The list is the one of docs/STEP3.md, exactly.
        assert_eq!(RESERVED.len(), 27);
        let doc = include_str!("../../../../docs/STEP3.md");
        let line = doc
            .lines()
            .skip_while(|l| !l.contains("**Reserved slugs**"))
            .take(4)
            .collect::<Vec<_>>()
            .join(" ");
        for s in RESERVED {
            assert!(line.contains(&format!("`{s}`")), "{s} is in docs/STEP3.md");
        }
    }

    #[test]
    fn length_edges() {
        assert_eq!(check("ab", SlugKind::App), Err(SlugProblem::TooShort));
        assert_eq!(check("a", SlugKind::Tenant), Err(SlugProblem::TooShort));
        assert_eq!(check("", SlugKind::App), Err(SlugProblem::TooShort));
        assert!(is_valid("abc", SlugKind::App));
        assert!(is_valid(&"a".repeat(53), SlugKind::App));
        assert_eq!(
            check(&"a".repeat(54), SlugKind::App),
            Err(SlugProblem::TooLong)
        );
        assert!(is_valid(&"a".repeat(40), SlugKind::Tenant));
        assert_eq!(
            check(&"a".repeat(41), SlugKind::Tenant),
            Err(SlugProblem::TooLong)
        );
        // The longest app slug with a preview prefix fits in one DNS label.
        assert_eq!(format!("{}--{}", short_id(), "a".repeat(53)).len(), 63);
    }

    #[test]
    fn dashes_punycode_underscore_and_characters() {
        assert_eq!(
            check("plan--ner", SlugKind::App),
            Err(SlugProblem::DoubleDash)
        );
        assert_eq!(check("xn--abc", SlugKind::App), Err(SlugProblem::Punycode));
        assert_eq!(
            check("xn--abc", SlugKind::Tenant),
            Err(SlugProblem::Punycode)
        );
        assert_eq!(check("-abc", SlugKind::App), Err(SlugProblem::DashAtEnd));
        assert_eq!(check("abc-", SlugKind::Tenant), Err(SlugProblem::DashAtEnd));
        assert_eq!(check("_abc", SlugKind::App), Err(SlugProblem::Underscore));
        assert_eq!(check("_a", SlugKind::Tenant), Err(SlugProblem::Underscore));
        assert_eq!(check("Abc", SlugKind::App), Err(SlugProblem::BadCharacter));
        assert_eq!(check("a.bc", SlugKind::App), Err(SlugProblem::BadCharacter));
        assert_eq!(check("a_bc", SlugKind::App), Err(SlugProblem::BadCharacter));
        assert!(is_valid("x-n-abc", SlugKind::App));
        assert!(is_valid("plan-ner-2", SlugKind::App));
        // The SQL checks of 0011 hold for every valid slug of these examples.
        assert!(is_valid("a1b", SlugKind::App));
    }

    #[test]
    fn slugs_from_names() {
        assert_eq!(from_name("Plant planner", 53), "plant-planner");
        assert_eq!(from_name("  Ööbiku  aed!! ", 53), "oobiku-aed");
        assert_eq!(from_name("a--b", 53), "a-b");
        assert_eq!(from_name("日本", 53), "");
        assert_eq!(from_name(&"ab ".repeat(40), 53).len(), 53);
        assert_eq!(
            candidates("Plant planner", SlugKind::App, 3),
            ["plant-planner", "plant-planner-2", "plant-planner-3"]
        );
        // A name that breaks a rule still gives slugs from the name.
        assert_eq!(candidates("www", SlugKind::App, 2), ["www-2", "www-3"]);
        assert_eq!(candidates("id", SlugKind::Tenant, 1), ["id-2"]);
        assert_eq!(candidates("_x", SlugKind::App, 1), ["x-2"]);
        assert_eq!(candidates("日本", SlugKind::App, 1), ["app-2"]);
        assert_eq!(candidates("xn--evil", SlugKind::App, 1), ["xn-evil"]);
        let long = candidates(&"a".repeat(80), SlugKind::App, 2);
        assert_eq!(long[0].len(), 53);
        assert_eq!(long[1].len(), 53);
        assert!(long[1].ends_with("-2"));
        for c in candidates("Garden -- planner!", SlugKind::Tenant, 20) {
            assert!(is_valid(&c, SlugKind::Tenant), "{c}");
        }
    }

    #[test]
    fn spares_have_random_suffixes() {
        let c = candidates_with_spares("Home", SlugKind::Tenant, 3, 5);
        assert_eq!(&c[..3], ["home", "home-2", "home-3"]);
        assert_eq!(c.len(), 8);
        for s in &c[3..] {
            assert!(s.starts_with("home-") && s.len() == 11, "{s}");
            assert!(is_valid(s, SlugKind::Tenant), "{s}");
        }
        let long = candidates_with_spares(&"a".repeat(80), SlugKind::Tenant, 1, 2);
        assert!(
            long.iter().all(|s| is_valid(s, SlugKind::Tenant)),
            "{long:?}"
        );
        let www = candidates_with_spares("www", SlugKind::App, 1, 1);
        assert!(www[1].starts_with("www-"), "{www:?}");
    }

    #[test]
    fn short_ids_are_lower_case_base32() {
        for _ in 0..100 {
            let s = short_id();
            assert_eq!(s.len(), 8);
            assert!(
                s.bytes()
                    .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
            );
        }
    }
}
