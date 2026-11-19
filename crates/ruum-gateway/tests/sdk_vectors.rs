//! Card O-5: the gateway's user tokens and the Node SDK's test vectors agree.
//!
//! `sdk/node/testdata/user-tokens.json` holds user tokens made by the SDK's signer
//! (`sdk/node/scripts/make-vectors.mjs`) with a test key made from a phrase. Ed25519 signatures
//! are deterministic, so the gateway's own `UserTokenSigner`, given the same seed and claims, must
//! give the same token byte for byte. The SDK's tests verify the same file (live, preview,
//! another app, expired, `exp - iat` = 61, an extra claim, an unknown actor). So a change of the
//! gateway's token format (header, claim order, key id, signature) fails here or there.
//!
//! The seed is the first 32 bytes of the phrase in the file, made at run time: no source line
//! looks like a key.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::jwk::Jwk;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use ruum_gateway::token::{UserClaims, UserTokenSigner};
use serde_json::{Value, json};

const VECTORS: &str = include_str!("../../../sdk/node/testdata/user-tokens.json");

fn file() -> Value {
    serde_json::from_str(VECTORS).expect("the vector file is JSON")
}

fn signer(f: &Value) -> UserTokenSigner {
    let phrase = f["seed_phrase"].as_str().expect("seed_phrase");
    let seed: [u8; 32] = phrase.as_bytes()[..32]
        .try_into()
        .expect("the phrase has at least 32 bytes");
    UserTokenSigner::from_seed(f["issuer"].as_str().expect("issuer"), seed).expect("a valid seed")
}

fn part_json(part: &str) -> Value {
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).expect("base64url")).expect("JSON")
}

#[test]
fn the_key_id_and_key_set_match_the_sdk() {
    let f = file();
    let s = signer(&f);
    let jwks = s.jwks();
    let keys = jwks["keys"].as_array().expect("keys");
    assert_eq!(keys.len(), 1);
    let k = &keys[0];
    assert_eq!(
        k["kid"], f["kid"],
        "the RFC 7638 thumbprint is the same in both"
    );
    assert_eq!(k["kty"], "OKP");
    assert_eq!(k["crv"], "Ed25519");
    assert_eq!(k["alg"], "EdDSA");
    assert_eq!(k["use"], "sig");
    assert!(k.get("d").is_none(), "the key set has no private part");
}

#[test]
fn the_gateway_signs_the_sdk_vectors_byte_for_byte() {
    let f = file();
    let s = signer(&f);
    let vectors = f["vectors"].as_array().expect("vectors");
    assert!(vectors.len() >= 9, "the file holds every case of the card");
    let names: Vec<&str> = vectors.iter().filter_map(|v| v["name"].as_str()).collect();
    for n in [
        "live",
        "preview",
        "another app",
        "expired",
        "exp - iat = 61",
        "extra claim",
        "unknown actor and sub prefix, with on_behalf_of",
    ] {
        assert!(names.contains(&n), "the vector {n} is in the file");
    }

    let jwk: Jwk = serde_json::from_value(s.jwks()["keys"][0].clone()).expect("a JWK");
    let key = DecodingKey::from_jwk(&jwk).expect("a decoding key");
    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.required_spec_claims.clear();
    validation.validate_exp = false;
    validation.validate_nbf = false;
    validation.validate_aud = false;

    let mut signed = 0;
    for v in vectors {
        let name = v["name"].as_str().expect("name");
        let parts: Vec<&str> = v["parts"]
            .as_array()
            .expect("parts")
            .iter()
            .map(|p| p.as_str().expect("a part"))
            .collect();
        assert_eq!(parts.len(), 3, "{name}");
        let token = parts.join(".");
        assert_eq!(
            part_json(parts[0]),
            json!({ "typ": "JWT", "alg": "EdDSA", "kid": f["kid"] }),
            "{name}: the gateway's header"
        );
        // Every token verifies with the gateway's key set and carries the claims of the file.
        let data = jsonwebtoken::decode::<Value>(&token, &key, &validation)
            .unwrap_or_else(|e| panic!("{name}: the gateway's key verifies the vector: {e}"));
        assert_eq!(data.claims, v["claims"], "{name}");

        if v["gateway_signs"] == true {
            let claims: UserClaims =
                serde_json::from_value(v["claims"].clone()).expect("the gateway's claim struct");
            assert_eq!(
                serde_json::to_value(&claims).expect("claims"),
                v["claims"],
                "{name}: the struct holds every claim of the vector"
            );
            assert_eq!(
                s.sign(&claims).expect("sign"),
                token,
                "{name}: the gateway's token equals the SDK vector byte for byte"
            );
            signed += 1;
        }
    }
    assert!(signed >= 8, "the gateway signed {signed} vectors");
}

#[test]
fn a_changed_claim_gives_a_different_token() {
    // The positive control of the byte comparison: it is not vacuous.
    let f = file();
    let s = signer(&f);
    let v = &f["vectors"][0];
    let mut claims: UserClaims = serde_json::from_value(v["claims"].clone()).expect("claims");
    let token: Vec<&str> = v["parts"]
        .as_array()
        .expect("parts")
        .iter()
        .map(|p| p.as_str().expect("a part"))
        .collect();
    assert_eq!(s.sign(&claims).expect("sign"), token.join("."));
    claims.exp += 1;
    assert_ne!(s.sign(&claims).expect("sign"), token.join("."));
}
