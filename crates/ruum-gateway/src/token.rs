//! The user token (`contracts/apps/user-token.schema.json`; `Ruum-User`): an EdDSA JWT that the
//! gateway adds to every forwarded request, made for each request, at most
//! [`USER_TOKEN_SECONDS`] long.
//!
//! The key is set apart from the API keys (decision 0006-10): an Ed25519 seed in a file
//! (`RUUM_GATEWAY_TOKEN_KEY_FILE`, never an environment value). The public key set
//! ([`UserTokenSigner::jwks`]) is what the runtime gives the app and the SDK checks against.
//!
//! The same key signs the gateway's sign-in state ([`UserTokenSigner::seal`]), with `typ`
//! `app_auth_state` and its own audience, so a state never passes as a user token (the SDK checks
//! `typ`, `aud` and `env`) and a user token never passes as a state.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::jwk::{Jwk, JwkSet, PublicKeyUse, ThumbprintHash};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use ruum_core::NodeId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// The longest life of a user token (`exp - iat`), in seconds (at most 60, the contract).
pub const USER_TOKEN_SECONDS: i64 = 60;

/// The PKCS#8 v1 header of an Ed25519 private key (RFC 8410); the 32-byte seed follows.
const ED25519_PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

/// The audience of the gateway's own sealed values.
const SEAL_AUDIENCE: &str = "ruum-gateway";

/// The live audience `app:<app-id>`.
pub fn live_aud(app: NodeId) -> String {
    format!("app:{}", app.0)
}

/// The preview audience `app:<app-id>/preview/<deployment-id>`.
pub fn preview_aud(app: NodeId, deployment: Uuid) -> String {
    format!("app:{}/preview/{deployment}", app.0)
}

/// The claims of a user token (the required ones of the schema, and `on_behalf_of`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserClaims {
    pub iss: String,
    pub aud: String,
    pub typ: String,
    pub env: String,
    pub sub: String,
    pub tenant: String,
    pub roles: Vec<String>,
    pub node_roles: serde_json::Map<String, Value>,
    pub actor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<String>,
    pub iat: i64,
    pub exp: i64,
    pub jti: String,
}

/// What the user token says about the visitor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenSubject {
    pub sub: String,
    pub actor: &'static str,
    pub on_behalf_of: Option<String>,
    pub roles: Vec<String>,
}

/// Signs user tokens and the gateway's sealed values with one Ed25519 key.
pub struct UserTokenSigner {
    issuer: String,
    kid: String,
    encoding: EncodingKey,
    decoding: DecodingKey,
    jwk: Jwk,
}

impl std::fmt::Debug for UserTokenSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserTokenSigner")
            .field("issuer", &self.issuer)
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

/// An error of the key or of signing.
#[derive(Debug)]
pub struct KeyError(pub String);

impl std::fmt::Display for KeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl UserTokenSigner {
    /// A signer with the Ed25519 key of `seed`. `issuer` is the instance's origin.
    pub fn from_seed(issuer: &str, seed: [u8; 32]) -> Result<Self, KeyError> {
        let mut der = Vec::with_capacity(48);
        der.extend_from_slice(&ED25519_PKCS8_PREFIX);
        der.extend_from_slice(&seed);
        let encoding = EncodingKey::from_ed_der(&der);
        let mut jwk = Jwk::from_encoding_key(&encoding, Algorithm::EdDSA)
            .map_err(|e| KeyError(format!("user token key: {e}")))?;
        let kid = jwk
            .thumbprint(ThumbprintHash::SHA256)
            .map_err(|e| KeyError(format!("user token key: {e}")))?;
        jwk.common.key_id = Some(kid.clone());
        jwk.common.public_key_use = Some(PublicKeyUse::Signature);
        let decoding =
            DecodingKey::from_jwk(&jwk).map_err(|e| KeyError(format!("user token key: {e}")))?;
        Ok(Self {
            issuer: issuer.trim_end_matches('/').to_string(),
            kid,
            encoding,
            decoding,
            jwk,
        })
    }

    /// A signer with a new random key (tests).
    pub fn generate(issuer: &str) -> Self {
        let mut seed = [0u8; 32];
        rand::fill(&mut seed[..]);
        Self::from_seed(issuer, seed).expect("a random Ed25519 seed is a valid key")
    }

    /// The key of a key file: the 32-byte seed as base64url (with or without padding and a
    /// trailing newline). The content is never printed.
    pub fn from_key_file(issuer: &str, path: &std::path::Path) -> Result<Self, KeyError> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            KeyError(format!(
                "cannot read the user token key file {}: {}",
                path.display(),
                e.kind()
            ))
        })?;
        let bytes = URL_SAFE_NO_PAD
            .decode(text.trim().trim_end_matches('='))
            .map_err(|_| KeyError("the user token key file is not base64url".into()))?;
        let seed: [u8; 32] = bytes
            .try_into()
            .map_err(|_| KeyError("the user token key file must hold 32 bytes".into()))?;
        Self::from_seed(issuer, seed)
    }

    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// The public key set (JWKS).
    pub fn jwks(&self) -> Value {
        serde_json::to_value(JwkSet {
            keys: vec![self.jwk.clone()],
        })
        .unwrap_or_else(|_| serde_json::json!({ "keys": [] }))
    }

    fn header(&self) -> Header {
        let mut h = Header::new(Algorithm::EdDSA);
        h.kid = Some(self.kid.clone());
        h
    }

    /// The claims of a user token for `subject` at the app `aud`, made now.
    pub fn claims(
        &self,
        aud: String,
        env: &str,
        tenant: &str,
        subject: &TokenSubject,
    ) -> UserClaims {
        let now = ruum_core::now().unix_timestamp();
        UserClaims {
            iss: self.issuer.clone(),
            aud,
            typ: "app_user".into(),
            env: env.into(),
            sub: subject.sub.clone(),
            tenant: tenant.into(),
            roles: subject.roles.clone(),
            node_roles: serde_json::Map::new(),
            actor: subject.actor.into(),
            on_behalf_of: subject.on_behalf_of.clone(),
            iat: now,
            exp: now + USER_TOKEN_SECONDS,
            jti: Uuid::now_v7().to_string(),
        }
    }

    /// Signs user token claims.
    pub fn sign(&self, claims: &UserClaims) -> Result<String, KeyError> {
        jsonwebtoken::encode(&self.header(), claims, &self.encoding)
            .map_err(|e| KeyError(format!("sign the user token: {e}")))
    }

    /// Signs a value of the gateway itself (the sign-in state) for `seconds`.
    pub fn seal(&self, value: &Value, seconds: i64) -> Result<String, KeyError> {
        let now = ruum_core::now().unix_timestamp();
        let claims = serde_json::json!({
            "iss": self.issuer,
            "aud": SEAL_AUDIENCE,
            "typ": "app_auth_state",
            "iat": now,
            "exp": now + seconds,
            "v": value,
        });
        jsonwebtoken::encode(&self.header(), &claims, &self.encoding)
            .map_err(|e| KeyError(format!("seal: {e}")))
    }

    /// Opens a value of [`UserTokenSigner::seal`]: the signature, the issuer, the audience, the
    /// type and the expiry (no leeway). `None` for anything else.
    pub fn open(&self, sealed: &str) -> Option<Value> {
        let mut v = Validation::new(Algorithm::EdDSA);
        v.leeway = 0;
        v.set_issuer(&[self.issuer.as_str()]);
        v.set_audience(&[SEAL_AUDIENCE]);
        v.set_required_spec_claims(&["exp", "iss", "aud"]);
        let data = jsonwebtoken::decode::<Value>(sealed, &self.decoding, &v).ok()?;
        if data.claims["typ"] != "app_auth_state" {
            return None;
        }
        Some(data.claims["v"].clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(token: &str) -> Value {
        let p = token.split('.').nth(1).unwrap();
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(p).unwrap()).unwrap()
    }

    #[test]
    fn user_tokens_live_at_most_60_seconds_with_numeric_times() {
        let s = UserTokenSigner::generate("https://ruum.localhost");
        let app = NodeId::new();
        let who = TokenSubject {
            sub: "pairwise:abc".into(),
            actor: "user",
            on_behalf_of: None,
            roles: vec![],
        };
        let c = s.claims(live_aud(app), "live", "acme", &who);
        let t = s.sign(&c).unwrap();
        let v = decode(&t);
        let (iat, exp) = (v["iat"].as_i64().unwrap(), v["exp"].as_i64().unwrap());
        assert!(exp > iat && exp - iat <= 60);
        assert_eq!(v["typ"], "app_user");
        assert_eq!(v["aud"], format!("app:{}", app.0));
        assert_eq!(v["iss"], "https://ruum.localhost");
        assert!(v.get("on_behalf_of").is_none());
        assert!(v["roles"].is_array() && v["node_roles"].is_object());
        assert!(v["jti"].is_string());
    }

    #[test]
    fn previews_have_their_own_audience() {
        let app = NodeId::new();
        let d = Uuid::now_v7();
        assert_eq!(preview_aud(app, d), format!("app:{}/preview/{d}", app.0));
        assert_ne!(preview_aud(app, d), live_aud(app));
        assert!(preview_aud(app, d).starts_with(&live_aud(app)));
    }

    #[test]
    fn sealed_states_are_not_user_tokens_and_expire() {
        let s = UserTokenSigner::generate("https://ruum.localhost");
        let sealed = s.seal(&serde_json::json!({"host": "a"}), 60).unwrap();
        assert_eq!(s.open(&sealed).unwrap()["host"], "a");
        assert_eq!(decode(&sealed)["typ"], "app_auth_state");
        let expired = s.seal(&serde_json::json!({}), -10).unwrap();
        assert!(s.open(&expired).is_none());
        let other = UserTokenSigner::generate("https://ruum.localhost");
        assert!(other.open(&sealed).is_none());
        // A user token never opens as a state.
        let who = TokenSubject {
            sub: "pairwise:abc".into(),
            actor: "user",
            on_behalf_of: None,
            roles: vec![],
        };
        let t = s
            .sign(&s.claims(live_aud(NodeId::new()), "live", "acme", &who))
            .unwrap();
        assert!(s.open(&t).is_none());
        assert!(s.open("t6made-up").is_none());
    }

    #[test]
    fn the_key_file_holds_a_base64url_seed() {
        let dir = std::env::temp_dir().join(format!("ruum-gw-key-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("key");
        std::fs::write(&f, format!("{}\n", URL_SAFE_NO_PAD.encode([3u8; 32]))).unwrap();
        let a = UserTokenSigner::from_key_file("https://x", &f).unwrap();
        let b = UserTokenSigner::from_seed("https://x", [3u8; 32]).unwrap();
        assert_eq!(a.jwks(), b.jwks());
        std::fs::write(&f, "short").unwrap();
        assert!(UserTokenSigner::from_key_file("https://x", &f).is_err());
        assert!(UserTokenSigner::from_key_file("https://x", &dir.join("missing")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
