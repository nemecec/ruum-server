//! Secrets: random tokens with a visible prefix, their SHA-256 hashes, and PKCE.
//!
//! Every secret that Ruum hands out (session cookies, agent keys, app passwords, invitation
//! tokens, OAuth codes and refresh tokens) has 256 random bits, so a fast hash (SHA-256) is enough
//! to store it. Only the hash is stored.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// 32 random bytes from the operating system's CSPRNG (through the thread RNG of `rand`).
pub fn random_bytes() -> [u8; 32] {
    let mut b = [0u8; 32];
    rand::fill(&mut b[..]);
    b
}

/// A new secret: `prefix` and 43 characters of base64url (256 bits).
pub fn random_secret(prefix: &str) -> String {
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(random_bytes()))
}

/// The SHA-256 of a secret, as stored.
pub fn sha256(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

/// The part of a secret that is shown again to recognise it: the prefix and 4 characters.
pub fn display_prefix(secret: &str, prefix: &str) -> String {
    let rest: String = secret[prefix.len().min(secret.len())..]
        .chars()
        .take(4)
        .collect();
    format!("{prefix}{rest}")
}

/// The PKCE `S256` challenge of a verifier (RFC 7636): base64url(SHA-256(verifier)).
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Whether `s` is a valid PKCE verifier or challenge: 43 to 128 unreserved characters.
pub fn is_pkce_value(s: &str) -> bool {
    (43..=128).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

/// Compares two byte strings in constant time.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_have_the_prefix_and_256_bits() {
        let s = random_secret("ruum_agent_");
        assert!(s.starts_with("ruum_agent_"));
        assert_eq!(s.len(), "ruum_agent_".len() + 43);
        assert_ne!(s, random_secret("ruum_agent_"));
        assert_eq!(
            display_prefix(&s, "ruum_agent_").len(),
            "ruum_agent_".len() + 4
        );
    }

    #[test]
    fn pkce_matches_rfc_7636() {
        // The example of RFC 7636, appendix B.
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert!(is_pkce_value("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
        assert!(!is_pkce_value("short"));
    }

    #[test]
    fn constant_time_eq_works() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
