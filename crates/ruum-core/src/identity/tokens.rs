//! The Ruum token issuer: Ed25519 (`EdDSA`) JWTs, published as JWKS (`getJwks`).
//!
//! All services trust only platform tokens (design 4.2). Access tokens carry `aud` (the API
//! origin) and `typ` (`agent`, `user`, `share_link`). The same key also seals short-lived values
//! of the server, such as the sign-in state and the OAuth consent request; they have the
//! audience `ruum:<purpose>` and no `typ`, so they never pass as access tokens.
//!
//! The key comes from configuration (`RUUM_TOKEN_SIGNING_KEY`: the 32-byte Ed25519 seed as
//! base64url). Without it the server makes a new key at start, so tokens end at a restart.

use super::{AccessClaims, TokenIssuer, TokenType, UserId, unauthenticated};
use crate::error::{ApiError, Error, ErrorCode, Result};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::jwk::{Jwk, JwkSet, PublicKeyUse, ThumbprintHash};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The PKCS#8 v1 header of an Ed25519 private key (RFC 8410); the 32-byte seed follows.
const ED25519_PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

/// Clock skew that verification accepts, in seconds.
const LEEWAY_SECONDS: u64 = 30;

/// Signs and verifies Ruum tokens with one Ed25519 key.
pub struct JwtTokenIssuer {
    issuer: String,
    audience: String,
    kid: String,
    encoding: EncodingKey,
    decoding: DecodingKey,
    jwk: Jwk,
}

impl std::fmt::Debug for JwtTokenIssuer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwtTokenIssuer")
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

impl JwtTokenIssuer {
    /// An issuer with the Ed25519 key of `seed`. `issuer` and `audience` are usually both the
    /// public API origin, for example `https://api.ruum.space`.
    pub fn from_seed(issuer: &str, audience: &str, seed: [u8; 32]) -> Result<Self> {
        let mut der = Vec::with_capacity(48);
        der.extend_from_slice(&ED25519_PKCS8_PREFIX);
        der.extend_from_slice(&seed);
        let encoding = EncodingKey::from_ed_der(&der);
        let mut jwk = Jwk::from_encoding_key(&encoding, Algorithm::EdDSA)
            .map_err(|e| Error::Internal(format!("token key: {e}")))?;
        let kid = jwk
            .thumbprint(ThumbprintHash::SHA256)
            .map_err(|e| Error::Internal(format!("token key: {e}")))?;
        jwk.common.key_id = Some(kid.clone());
        jwk.common.public_key_use = Some(PublicKeyUse::Signature);
        let decoding =
            DecodingKey::from_jwk(&jwk).map_err(|e| Error::Internal(format!("token key: {e}")))?;
        Ok(Self {
            issuer: issuer.trim_end_matches('/').to_string(),
            audience: audience.trim_end_matches('/').to_string(),
            kid,
            encoding,
            decoding,
            jwk,
        })
    }

    /// An issuer with a new random key.
    pub fn generate(issuer: &str, audience: &str) -> Self {
        Self::from_seed(issuer, audience, super::secret::random_bytes())
            .expect("a random Ed25519 seed is a valid key")
    }

    /// An issuer with the key of `RUUM_TOKEN_SIGNING_KEY` (the seed as base64url).
    pub fn from_base64_seed(issuer: &str, audience: &str, seed: &str) -> Result<Self> {
        let bytes = URL_SAFE_NO_PAD
            .decode(seed.trim().trim_end_matches('='))
            .map_err(|_| Error::Internal("RUUM_TOKEN_SIGNING_KEY is not base64url".into()))?;
        let seed: [u8; 32] = bytes
            .try_into()
            .map_err(|_| Error::Internal("RUUM_TOKEN_SIGNING_KEY must hold 32 bytes".into()))?;
        Self::from_seed(issuer, audience, seed)
    }

    fn header(&self) -> Header {
        let mut h = Header::new(Algorithm::EdDSA);
        h.kid = Some(self.kid.clone());
        h
    }

    fn validation(&self, audiences: &[String]) -> Validation {
        let mut v = Validation::new(Algorithm::EdDSA);
        v.leeway = LEEWAY_SECONDS;
        v.set_issuer(&[self.issuer.as_str()]);
        v.set_audience(audiences);
        v.set_required_spec_claims(&["exp", "iss", "aud"]);
        v
    }
}

/// The error of a token that does not verify.
fn token_error(e: jsonwebtoken::errors::Error) -> Error {
    match e.kind() {
        jsonwebtoken::errors::ErrorKind::ExpiredSignature => Error::Api(
            ApiError::new(ErrorCode::TokenExpired, "the access token has expired")
                .with_hint("get a new access token at POST /v1/oauth/token"),
        ),
        _ => unauthenticated(
            "the bearer token is not valid",
            "send a Ruum access token from POST /v1/oauth/token",
        ),
    }
}

/// The claims of a sealed value.
#[derive(Serialize, Deserialize)]
struct Sealed {
    iss: String,
    aud: String,
    iat: i64,
    exp: i64,
    v: Value,
}

impl TokenIssuer for JwtTokenIssuer {
    fn issuer(&self) -> &str {
        &self.issuer
    }

    fn audience(&self) -> &str {
        &self.audience
    }

    fn issue(&self, claims: &AccessClaims) -> Result<String> {
        encode(&self.header(), claims, &self.encoding)
            .map_err(|e| Error::Internal(format!("sign token: {e}")))
    }

    fn verify(&self, token: &str) -> Result<AccessClaims> {
        let audiences = [self.audience.clone(), format!("{}/mcp", self.audience)];
        decode::<AccessClaims>(token, &self.decoding, &self.validation(&audiences))
            .map(|d| d.claims)
            .map_err(token_error)
    }

    fn jwks(&self) -> Value {
        serde_json::to_value(JwkSet {
            keys: vec![self.jwk.clone()],
        })
        .unwrap_or_else(|_| serde_json::json!({ "keys": [] }))
    }

    fn seal(&self, purpose: &str, value: &Value, seconds: i64) -> Result<String> {
        let now = crate::now().unix_timestamp();
        let claims = Sealed {
            iss: self.issuer.clone(),
            aud: format!("ruum:{purpose}"),
            iat: now,
            exp: now + seconds,
            v: value.clone(),
        };
        encode(&self.header(), &claims, &self.encoding)
            .map_err(|e| Error::Internal(format!("seal: {e}")))
    }

    fn open(&self, purpose: &str, sealed: &str) -> Result<Value> {
        decode::<Sealed>(
            sealed,
            &self.decoding,
            &self.validation(&[format!("ruum:{purpose}")]),
        )
        .map(|d| d.claims.v)
        .map_err(|e| match e.kind() {
            jsonwebtoken::errors::ErrorKind::ExpiredSignature => Error::api(
                ErrorCode::ValidationFailed,
                format!("the {purpose} has expired"),
            ),
            _ => Error::api(
                ErrorCode::ValidationFailed,
                format!("the {purpose} is not valid"),
            ),
        })
    }
}

/// Issues a user access token (`typ` `user`) for native clients and tests, valid for `seconds`.
pub fn issue_user_token(issuer: &dyn TokenIssuer, user: UserId, seconds: i64) -> Result<String> {
    issuer.issue(&AccessClaims::new(issuer, TokenType::User, user.0, seconds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn issuer() -> JwtTokenIssuer {
        JwtTokenIssuer::generate("https://ruum.localhost", "https://ruum.localhost")
    }

    #[test]
    fn a_token_round_trips() {
        let i = issuer();
        let user = UserId(Uuid::now_v7());
        let t = issue_user_token(&i, user, 60).unwrap();
        let c = i.verify(&t).unwrap();
        assert_eq!(c.sub, user.0);
        assert_eq!(c.typ, TokenType::User);
        assert_eq!(c.aud, "https://ruum.localhost");
    }

    #[test]
    fn another_key_is_refused() {
        let t = issue_user_token(&issuer(), UserId(Uuid::now_v7()), 60).unwrap();
        assert!(issuer().verify(&t).is_err());
    }

    #[test]
    fn an_expired_token_is_token_expired() {
        let i = issuer();
        let t = issue_user_token(&i, UserId(Uuid::now_v7()), -120).unwrap();
        match i.verify(&t) {
            Err(Error::Api(e)) => assert_eq!(e.code, ErrorCode::TokenExpired),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn another_audience_is_refused() {
        let i = issuer();
        let mut c = AccessClaims::new(&i, TokenType::User, Uuid::now_v7(), 60);
        c.aud = "https://apps.example".into();
        assert!(i.verify(&i.issue(&c).unwrap()).is_err());
        c.aud = "https://ruum.localhost/mcp".into();
        assert!(i.verify(&i.issue(&c).unwrap()).is_ok());
    }

    #[test]
    fn an_unknown_typ_is_refused() {
        let i = issuer();
        let now = crate::now().unix_timestamp();
        let claims = serde_json::json!({
            "iss": i.issuer(), "aud": i.audience(), "typ": "provider", "sub": Uuid::now_v7(),
            "tid": null, "iat": now, "exp": now + 60, "jti": Uuid::now_v7(),
        });
        let t = encode(&i.header(), &claims, &i.encoding).unwrap();
        assert!(i.verify(&t).is_err());
    }

    #[test]
    fn a_sealed_value_is_not_an_access_token() {
        let i = issuer();
        let s = i
            .seal("oauth_request", &serde_json::json!({ "a": 1 }), 60)
            .unwrap();
        assert_eq!(i.open("oauth_request", &s).unwrap()["a"], 1);
        assert!(i.open("login", &s).is_err());
        assert!(i.verify(&s).is_err());
    }

    #[test]
    fn jwks_has_the_public_key() {
        let i = issuer();
        let jwks = i.jwks();
        let key = &jwks["keys"][0];
        assert_eq!(key["kty"], "OKP");
        assert_eq!(key["crv"], "Ed25519");
        assert_eq!(key["kid"], i.kid.as_str());
        assert!(key.get("d").is_none(), "no private part");
        let from_seed = JwtTokenIssuer::from_base64_seed(
            "https://ruum.localhost",
            "https://ruum.localhost",
            &URL_SAFE_NO_PAD.encode([7u8; 32]),
        )
        .unwrap();
        let again = JwtTokenIssuer::from_seed(
            "https://ruum.localhost",
            "https://ruum.localhost",
            [7u8; 32],
        )
        .unwrap();
        assert_eq!(from_seed.kid, again.kid);
    }
}
