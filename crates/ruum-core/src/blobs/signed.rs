//! Signed content URLs for the user-content domain (`getContentUrl`, decision 1 and design
//! section 15).
//!
//! The web UI never downloads from the API origin. It asks for a short-lived URL
//! `https://<tenant-id>.content.ruumapps.net/c/<token>` and the content domain serves the bytes
//! without cookies, with a strict CSP sandbox. The token is the credential: it names the tenant,
//! node, version, disposition and expiry, and carries a BLAKE3 keyed hash (MAC) made with the
//! server's content key. Access that is removed after the token was made stays for at most
//! [`CONTENT_URL_SECONDS`].
//!
//! Token format: `base64url(claims JSON) "." base64url(MAC)`.

use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{NodeId, TenantId, VersionId};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// How long a signed content URL works (5 minutes, as the contract says).
pub const CONTENT_URL_SECONDS: i64 = 300;

/// How the content domain sends the file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// A download (`Content-Disposition: attachment`).
    #[default]
    Attachment,
    /// A preview (`Content-Disposition: inline`), still in a CSP sandbox.
    Inline,
}

/// What a signed token allows: one version of one node, until `exp`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentClaims {
    #[serde(rename = "t")]
    pub tenant: TenantId,
    #[serde(rename = "n")]
    pub node: NodeId,
    #[serde(rename = "v")]
    pub version: VersionId,
    #[serde(rename = "d")]
    pub disposition: Disposition,
    /// The file name at the time of signing, for `Content-Disposition`.
    #[serde(rename = "f")]
    pub name: String,
    /// The expiry, in Unix seconds.
    #[serde(rename = "e")]
    pub exp: i64,
}

impl ContentClaims {
    pub fn expires_at(&self) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(self.exp).unwrap_or(OffsetDateTime::UNIX_EPOCH)
    }
}

/// Makes and checks signed content tokens.
#[derive(Clone)]
pub struct ContentSigner {
    key: [u8; 32],
}

impl std::fmt::Debug for ContentSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ContentSigner(..)")
    }
}

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;
const CONTEXT: &str = "ruum content url v1";

fn invalid() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::NotFound,
            "the content link is not valid or has expired",
        )
        .with_hint("ask the API for a new URL (GET /v1/nodes/{node_id}/content-url)"),
    )
}

impl ContentSigner {
    /// A signer with this key. All servers of one deployment share the key.
    pub fn new(key: [u8; 32]) -> Self {
        // Derive the MAC key, so the configured key is never used for anything else directly.
        Self {
            key: blake3::derive_key(CONTEXT, &key),
        }
    }

    /// A signer with a random key (in-memory servers and tests; URLs end at a restart).
    pub fn random() -> Self {
        Self::new(rand::random())
    }

    /// A signer from a base64url (or standard base64) key of 32 bytes, as in
    /// `RUUM_CONTENT_SIGNING_KEY`.
    pub fn from_base64(s: &str) -> Result<Self> {
        let s = s.trim();
        let bytes = B64
            .decode(s.trim_end_matches('='))
            .or_else(|_| base64::engine::general_purpose::STANDARD.decode(s))
            .map_err(|_| Error::Internal("the content signing key is not base64".into()))?;
        let key: [u8; 32] = bytes
            .try_into()
            .map_err(|_| Error::Internal("the content signing key must be 32 bytes".into()))?;
        Ok(Self::new(key))
    }

    /// The claims for a new token: valid for [`CONTENT_URL_SECONDS`] from `now`.
    pub fn claims(
        tenant: TenantId,
        node: NodeId,
        version: VersionId,
        disposition: Disposition,
        name: &str,
        now: OffsetDateTime,
    ) -> ContentClaims {
        ContentClaims {
            tenant,
            node,
            version,
            disposition,
            name: name.to_string(),
            exp: now.unix_timestamp() + CONTENT_URL_SECONDS,
        }
    }

    /// The signed token of the claims.
    pub fn sign(&self, claims: &ContentClaims) -> String {
        let body = B64.encode(serde_json::to_vec(claims).expect("claims serialize"));
        let mac = blake3::keyed_hash(&self.key, body.as_bytes());
        format!("{body}.{}", B64.encode(mac.as_bytes()))
    }

    /// Checks the MAC and the expiry and returns the claims. Every failure is `not_found`, so
    /// the answer does not tell a forged token from an expired one.
    pub fn verify(&self, token: &str, now: OffsetDateTime) -> Result<ContentClaims> {
        let (body, mac) = token.split_once('.').ok_or_else(invalid)?;
        let mac = B64.decode(mac).map_err(|_| invalid())?;
        let mac: [u8; 32] = mac.try_into().map_err(|_| invalid())?;
        // `blake3::Hash` compares in constant time.
        if blake3::keyed_hash(&self.key, body.as_bytes()) != blake3::Hash::from(mac) {
            return Err(invalid());
        }
        let claims: ContentClaims =
            serde_json::from_slice(&B64.decode(body).map_err(|_| invalid())?)
                .map_err(|_| invalid())?;
        if claims.exp <= now.unix_timestamp() {
            return Err(invalid());
        }
        Ok(claims)
    }
}

/// The URL of a token on the content domain: the tenant ID becomes the first label of the host
/// of `content_base` (`https://content.ruumapps.net` gives
/// `https://<tenant>.content.ruumapps.net/c/<token>`).
pub fn content_url(content_base: &str, tenant: TenantId, token: &str) -> String {
    let base = content_base.trim_end_matches('/');
    match base.split_once("://") {
        Some((scheme, host)) => format!("{scheme}://{tenant}.{host}/c/{token}"),
        None => format!("https://{tenant}.{base}/c/{token}"),
    }
}

/// The tenant of a content-domain host (`<tenant>.content.ruumapps.net`), if the first label
/// is a tenant ID.
pub fn tenant_of_host(host: &str) -> Option<TenantId> {
    host.split('.').next().and_then(|l| l.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(now: OffsetDateTime) -> ContentClaims {
        ContentSigner::claims(
            TenantId::new(),
            NodeId::new(),
            VersionId::new(),
            Disposition::Inline,
            "plan.txt",
            now,
        )
    }

    #[test]
    fn a_token_round_trips_until_it_expires() {
        let s = ContentSigner::random();
        let now = crate::now();
        let c = claims(now);
        let t = s.sign(&c);
        assert_eq!(s.verify(&t, now).unwrap(), c);
        assert!(s.verify(&t, now + time::Duration::seconds(301)).is_err());
    }

    #[test]
    fn a_changed_or_foreign_token_fails() {
        let s = ContentSigner::random();
        let now = crate::now();
        let t = s.sign(&claims(now));
        assert!(ContentSigner::random().verify(&t, now).is_err());
        let (body, mac) = t.split_once('.').unwrap();
        let other = s.sign(&claims(now));
        let (_, other_mac) = other.split_once('.').unwrap();
        assert!(s.verify(&format!("{body}.{other_mac}"), now).is_err());
        assert!(s.verify(&format!("{body}x.{mac}"), now).is_err());
        assert!(s.verify("nonsense", now).is_err());
    }

    #[test]
    fn the_url_has_the_tenant_as_first_label() {
        let t = TenantId::new();
        assert_eq!(
            content_url("https://content.ruumapps.net/", t, "abc"),
            format!("https://{t}.content.ruumapps.net/c/abc")
        );
        assert_eq!(
            tenant_of_host(&format!("{t}.content.ruumapps.net")),
            Some(t)
        );
        assert_eq!(tenant_of_host("content.ruumapps.net"), None);
    }

    #[test]
    fn keys_load_from_base64() {
        let key = B64.encode([7u8; 32]);
        let a = ContentSigner::from_base64(&key).unwrap();
        let b = ContentSigner::new([7u8; 32]);
        let c = claims(crate::now());
        assert_eq!(a.sign(&c), b.sign(&c));
        assert!(ContentSigner::from_base64("c2hvcnQ").is_err());
    }
}
