//! Token helpers: decode a `Ruum-User` token, and a reference verifier written from the contract
//! (`contracts/apps/user-token.schema.json`, `contracts/apps/README.md` "Rules", decision 0006-10
//! and decision 0006 G point 2). It does what the Ruum SDK must do: check the EdDSA signature
//! against the gateway's key set, `iss`, `aud`, `typ` = `app_user`, `env`, numeric `iat` and
//! `exp`, `exp` after now, `exp - iat` at most 60 s, and the claims against the schema (unknown
//! claims are allowed).
//!
//! [`TestSigner`] makes hand-made tokens for the self-tests of the harness (and the toy gateways
//! of `tests/gateway.rs`); it is never the gateway's key.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::jwk::{Jwk, PublicKeyUse, ThumbprintHash};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde_json::{Value, json};
use std::sync::OnceLock;

/// The text of the user token schema.
pub const USER_TOKEN_SCHEMA: &str =
    include_str!("../../../../contracts/apps/user-token.schema.json");
/// The text of the delegated token schema.
pub const DELEGATED_TOKEN_SCHEMA: &str =
    include_str!("../../../../contracts/apps/delegated-token.schema.json");

/// The longest life of a user token (`exp - iat`), in seconds.
pub const MAX_LIFE_SECONDS: i64 = 60;
/// The clock skew the verifier accepts for `iat` in the future, in seconds. `exp` gets no
/// leeway: the contract says the token must not have expired.
pub const LEEWAY_SECONDS: i64 = 5;

/// What the receiving app expects (`RUUM_ISSUER`, `RUUM_AUDIENCE`, `RUUM_ENV`).
#[derive(Clone, Debug)]
pub struct Expect {
    pub iss: String,
    pub aud: String,
    pub env: String,
}

impl Expect {
    /// The live app `app_id` of the instance `iss`.
    pub fn live(iss: &str, app_id: &str) -> Expect {
        Expect {
            iss: iss.to_string(),
            aud: live_aud(app_id),
            env: "live".into(),
        }
    }

    /// The preview `deployment_id` of `app_id`.
    pub fn preview(iss: &str, app_id: &str, deployment_id: &str) -> Expect {
        Expect {
            iss: iss.to_string(),
            aud: preview_aud(app_id, deployment_id),
            env: "preview".into(),
        }
    }
}

/// `app:<app-id>`.
pub fn live_aud(app_id: &str) -> String {
    format!("app:{app_id}")
}

/// `app:<app-id>/preview/<deployment-id>`.
pub fn preview_aud(app_id: &str, deployment_id: &str) -> String {
    format!("app:{app_id}/preview/{deployment_id}")
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn b64_json(part: &str) -> Result<Value, String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(part.trim_end_matches('='))
        .map_err(|_| "a part that is not base64url".to_string())?;
    serde_json::from_slice(&bytes).map_err(|_| "a part that is not JSON".to_string())
}

/// The header and the claims of a JWT, without any check.
pub fn decode_unverified(token: &str) -> Result<(Value, Value), String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(format!("not a JWT ({} parts)", parts.len()));
    }
    Ok((b64_json(parts[0])?, b64_json(parts[1])?))
}

fn compiled(text: &str) -> jsonschema::Validator {
    let schema: Value = serde_json::from_str(text).expect("the token schema is JSON");
    jsonschema::validator_for(&schema).expect("the token schema compiles")
}

/// The schema problems of user token claims (the JSON pointer and the rule, never a value).
pub fn schema_problems(claims: &Value) -> Vec<String> {
    static V: OnceLock<jsonschema::Validator> = OnceLock::new();
    V.get_or_init(|| compiled(USER_TOKEN_SCHEMA))
        .iter_errors(claims)
        .map(|e| format!("{} ({:?})", e.instance_path(), e.kind().keyword()))
        .collect()
}

/// The schema problems of delegated token claims.
pub fn delegated_schema_problems(claims: &Value) -> Vec<String> {
    static V: OnceLock<jsonschema::Validator> = OnceLock::new();
    V.get_or_init(|| compiled(DELEGATED_TOKEN_SCHEMA))
        .iter_errors(claims)
        .map(|e| format!("{} ({:?})", e.instance_path(), e.kind().keyword()))
        .collect()
}

/// Whether `token` has a valid EdDSA signature by a key of `jwks` (the `kid` of the header
/// picks the key; without a `kid` any key of the set may match).
pub fn signature_ok(token: &str, jwks: &Value) -> Result<(), String> {
    let (header, _) = decode_unverified(token)?;
    if header["alg"] != "EdDSA" {
        return Err(format!("alg {} is not EdDSA", header["alg"]));
    }
    let kid = header["kid"].as_str();
    let keys = jwks["keys"].as_array().cloned().unwrap_or_default();
    let candidates: Vec<&Value> = keys
        .iter()
        .filter(|k| kid.is_none() || k["kid"].as_str() == kid)
        .collect();
    if candidates.is_empty() {
        return Err("no key of the key set has the token's kid".into());
    }
    let mut v = Validation::new(Algorithm::EdDSA);
    v.required_spec_claims.clear();
    v.validate_exp = false;
    v.validate_nbf = false;
    v.validate_aud = false;
    for k in candidates {
        let Ok(jwk) = serde_json::from_value::<Jwk>(k.clone()) else {
            continue;
        };
        let Ok(key) = DecodingKey::from_jwk(&jwk) else {
            continue;
        };
        if jsonwebtoken::decode::<Value>(token, &key, &v).is_ok() {
            return Ok(());
        }
    }
    Err("the signature does not verify with the gateway's key set".into())
}

/// The reference verifier of a user token. Returns the claims, or why the token is refused (the
/// claim names, never their values).
pub fn verify_user_token(token: &str, jwks: &Value, expect: &Expect) -> Result<Value, String> {
    signature_ok(token, jwks)?;
    let (_, c) = decode_unverified(token)?;
    if c["typ"] != "app_user" {
        return Err("typ is not app_user".into());
    }
    if c["iss"].as_str() != Some(expect.iss.as_str()) {
        return Err(format!("iss is not {}", expect.iss));
    }
    if c["aud"].as_str() != Some(expect.aud.as_str()) {
        return Err(format!("aud is not {}", expect.aud));
    }
    if c["env"].as_str() != Some(expect.env.as_str()) {
        return Err(format!("env is not {}", expect.env));
    }
    let (Some(iat), Some(exp)) = (c["iat"].as_i64(), c["exp"].as_i64()) else {
        return Err("iat and exp must both be numbers".into());
    };
    let t = now();
    if exp <= t {
        return Err("exp has passed".into());
    }
    if iat > t + LEEWAY_SECONDS {
        return Err("iat is in the future".into());
    }
    if exp - iat > MAX_LIFE_SECONDS || exp <= iat {
        return Err(format!(
            "exp - iat is {} s, it must be between 1 and {MAX_LIFE_SECONDS}",
            exp - iat
        ));
    }
    let problems = schema_problems(&c);
    if !problems.is_empty() {
        return Err(format!(
            "the claims do not match user-token.schema.json: {}",
            problems.join("; ")
        ));
    }
    Ok(c)
}

/// Valid user token claims for now (for hand-made tokens).
pub fn user_claims(iss: &str, aud: &str, env: &str, tenant: &str, actor: &str) -> Value {
    let t = now();
    json!({
        "iss": iss,
        "aud": aud,
        "typ": "app_user",
        "env": env,
        "sub": "pairwise:t6visitor",
        "tenant": tenant,
        "roles": [],
        "node_roles": {},
        "actor": actor,
        "iat": t,
        "exp": t + 30,
        "jti": format!("t6-{}", uuid::Uuid::now_v7()),
    })
}

/// Hand-made tokens that the SDK must refuse at the live app `app_id` of `iss` (card T-6,
/// acceptance tests): each with what is wrong. Signed with `s` unless the point is the key.
pub fn hand_made_bad_tokens(
    s: &TestSigner,
    iss: &str,
    app_id: &str,
    deployment_id: &str,
) -> Vec<(&'static str, String)> {
    let with = |f: &dyn Fn(&mut Value)| {
        let mut c = user_claims(iss, &live_aud(app_id), "live", "acme", "user");
        f(&mut c);
        c
    };
    let t = now();
    let remove = |c: &mut Value, k: &str| {
        if let Some(o) = c.as_object_mut() {
            o.remove(k);
        }
    };
    vec![
        (
            "wrong aud",
            s.sign(&with(&|c: &mut Value| {
                c["aud"] = json!(live_aud("0190b6c4-a00d-7cc1-9a4b-000000000000"))
            })),
        ),
        (
            "a preview audience at the live app",
            s.sign(&with(&|c: &mut Value| {
                c["aud"] = json!(preview_aud(app_id, deployment_id));
                c["env"] = json!("preview");
            })),
        ),
        (
            "wrong env",
            s.sign(&with(&|c: &mut Value| c["env"] = json!("preview"))),
        ),
        ("no iat", s.sign(&with(&|c: &mut Value| remove(c, "iat")))),
        (
            "iat as a string",
            s.sign(&with(&|c: &mut Value| c["iat"] = json!(t.to_string()))),
        ),
        (
            "exp - iat over 60 s",
            s.sign(&with(&|c: &mut Value| {
                c["iat"] = json!(t - 10);
                c["exp"] = json!(t + 61);
            })),
        ),
        (
            "expired",
            s.sign(&with(&|c: &mut Value| {
                c["iat"] = json!(t - 100);
                c["exp"] = json!(t - 50);
            })),
        ),
        (
            "expired 2 s ago",
            s.sign(&with(&|c: &mut Value| {
                c["iat"] = json!(t - 30);
                c["exp"] = json!(t - 2);
            })),
        ),
        (
            "unknown typ",
            s.sign(&with(&|c: &mut Value| c["typ"] = json!("agent"))),
        ),
        ("no typ", s.sign(&with(&|c: &mut Value| remove(c, "typ")))),
        (
            "wrong iss",
            s.sign(&with(&|c: &mut Value| {
                c["iss"] = json!("https://evil.example")
            })),
        ),
        (
            "a missing required claim (jti)",
            s.sign(&with(&|c: &mut Value| remove(c, "jti"))),
        ),
        (
            "a foreign key",
            TestSigner::new().sign(&with(&|_: &mut Value| {})),
        ),
    ]
}

/// An Ed25519 key of the harness, for hand-made tokens.
pub struct TestSigner {
    kid: String,
    encoding: EncodingKey,
    jwk: Jwk,
}

/// The PKCS#8 v1 header of an Ed25519 private key (RFC 8410); the 32-byte seed follows.
const ED25519_PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

impl Default for TestSigner {
    fn default() -> Self {
        Self::new()
    }
}

impl TestSigner {
    /// A new random key.
    pub fn new() -> TestSigner {
        let mut der = ED25519_PKCS8_PREFIX.to_vec();
        der.extend_from_slice(&ruum_core::identity::secret::random_bytes());
        let encoding = EncodingKey::from_ed_der(&der);
        let mut jwk =
            Jwk::from_encoding_key(&encoding, Algorithm::EdDSA).expect("an Ed25519 public key");
        let kid = jwk
            .thumbprint(ThumbprintHash::SHA256)
            .expect("a key thumbprint");
        jwk.common.key_id = Some(kid.clone());
        jwk.common.public_key_use = Some(PublicKeyUse::Signature);
        TestSigner { kid, encoding, jwk }
    }

    /// The public key set (`{"keys": [...]}`).
    pub fn jwks(&self) -> Value {
        json!({ "keys": [serde_json::to_value(&self.jwk).expect("a JWK")] })
    }

    /// Signs `claims` (EdDSA, with the key's `kid`).
    pub fn sign(&self, claims: &Value) -> String {
        let mut h = Header::new(Algorithm::EdDSA);
        h.kid = Some(self.kid.clone());
        jsonwebtoken::encode(&h, claims, &self.encoding).expect("sign a token")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISS: &str = "https://ruum.localhost";
    const APP: &str = "0190b6c4-a00d-7cc1-9a4b-1f2e3d4c5b7d";
    const DEP: &str = "0190b6c4-a00d-7cc1-9a4b-1f2e3d4c5b7e";

    fn with(f: impl FnOnce(&mut Value)) -> Value {
        let mut c = user_claims(ISS, &live_aud(APP), "live", "acme", "user");
        f(&mut c);
        c
    }

    /// The reference verifier refuses each hand-made bad token (card T-6, acceptance tests).
    #[test]
    fn the_reference_verifier_refuses_bad_tokens() {
        let s = TestSigner::new();
        let jwks = s.jwks();
        let live = Expect::live(ISS, APP);
        let good = s.sign(&with(|_| {}));
        verify_user_token(&good, &jwks, &live).expect("a good token verifies");
        // Unknown claims are allowed (open for later steps).
        let extra = s.sign(&with(|c| c["later_claim"] = json!({"x": 1})));
        verify_user_token(&extra, &jwks, &live).expect("an extra claim is allowed");
        // A new actor value and a new sub prefix are allowed.
        let open = s.sign(&with(|c| {
            c["actor"] = json!("robot");
            c["sub"] = json!("remote:abc");
        }));
        verify_user_token(&open, &jwks, &live).expect("open sets are allowed");

        let bad = hand_made_bad_tokens(&s, ISS, APP, DEP);
        for (what, tok) in bad {
            assert!(
                verify_user_token(&tok, &jwks, &live).is_err(),
                "the verifier accepted a token with {what}"
            );
        }
        // A token with alg none, and a tampered payload.
        let (_, claims) = decode_unverified(&good).unwrap();
        let none = format!(
            "{}.{}.",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        assert!(verify_user_token(&none, &jwks, &live).is_err());
        let parts: Vec<&str> = good.split('.').collect();
        let mut tampered = claims.clone();
        tampered["tenant"] = json!("globex");
        let forged = format!(
            "{}.{}.{}",
            parts[0],
            URL_SAFE_NO_PAD.encode(tampered.to_string()),
            parts[2]
        );
        assert!(verify_user_token(&forged, &jwks, &live).is_err());
    }

    #[test]
    fn a_preview_token_verifies_only_for_its_preview() {
        let s = TestSigner::new();
        let c = user_claims(ISS, &preview_aud(APP, DEP), "preview", "acme", "user");
        let tok = s.sign(&c);
        verify_user_token(&tok, &s.jwks(), &Expect::preview(ISS, APP, DEP)).unwrap();
        assert!(verify_user_token(&tok, &s.jwks(), &Expect::live(ISS, APP)).is_err());
    }
}
