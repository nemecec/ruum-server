//! The sealed part of an instance record (backlog S-89; `docs/STEP3.md` "The sandbox
//! supervisor" 4, decision 0006 I): the values of a run, encrypted and authenticated under the
//! record key that only the server pods hold.
//!
//! The text form is `v1.<key id>.<generation>.<nonce>.<ciphertext and tag>` (base64url without
//! padding for the nonce and the ciphertext). AES-256-GCM with a fresh 96-bit nonce from the
//! operating system's random source for every seal. The associated data is
//! `ruum-record-v1 \0 <record name> \0 <record uid> \0 <generation>`: a seal does not move to
//! another record, nor to a record made again under the same name (a new uid), and its generation
//! cannot be changed without breaking the tag.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

/// The most pod uids a seal keeps in its list of launched pods (the newest ones).
pub(crate) const MAX_LAUNCHED: usize = 16;

/// The record key: 32 random bytes (a Secret in the namespace `ruum`, mounted only into the
/// server pods). Its `Debug` shows only the key id.
#[derive(Clone)]
pub struct RecordKey([u8; 32]);

impl fmt::Debug for RecordKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RecordKey({})", self.id())
    }
}

impl RecordKey {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The key from 64 hexadecimal characters (surrounding white space is ignored). `Err` never
    /// holds the text.
    pub fn from_hex(text: &str) -> Result<Self, String> {
        let raw = hex::decode(text.trim()).map_err(|_| "the record key is not hexadecimal")?;
        let bytes: [u8; 32] = raw
            .try_into()
            .map_err(|_| "the record key is not 32 bytes (64 hexadecimal characters)")?;
        if bytes.iter().all(|b| *b == 0) {
            return Err("the record key is all zero".to_owned());
        }
        Ok(Self(bytes))
    }

    /// The first 8 bytes of the key's SHA-256, as hex: names the key in a seal.
    pub fn id(&self) -> String {
        hex::encode(&Sha256::digest(self.0)[..8])
    }
}

/// The sealed values of an instance. No `Debug` that shows a value.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Sealed {
    /// One more at each seal.
    pub generation: u64,
    /// The SHA-256 (hex) of the record's fixed fields (`pod::fixed_digest`).
    pub digest: String,
    /// The app's command (image entrypoint, then the spec's command or the image's `Cmd`).
    pub argv: Vec<String>,
    /// The secret environment values.
    pub env: Vec<(String, String)>,
    /// The previous values (redacted, never given to the app as values).
    pub previous: Vec<String>,
    /// The current workload token.
    pub token: String,
    /// The tokens it replaced, newest last (at most `MAX_KEPT_TOKENS - 1`): still valid until
    /// their expiry, so a new pod's supervisor redacts them too (Kimi re-check of I6-20, finding
    /// 3). Absent in seals of the release before (expand: an older release ignores it).
    #[serde(default)]
    pub earlier_tokens: Vec<String>,
    /// The run whose pod may take the launch data, and that pod's uid (written after the driver
    /// created the pod; `None` until then).
    pub run: u32,
    pub pod_uid: Option<String>,
    /// The pods (uids) that took their launch data: a launch is once per pod.
    pub launched: Vec<String>,
}

impl fmt::Debug for Sealed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sealed")
            .field("generation", &self.generation)
            .field("run", &self.run)
            .field("pod_uid", &self.pod_uid)
            .field("env", &self.env.iter().map(|(n, _)| n).collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

fn aad(name: &str, record_uid: &str, generation: u64) -> Vec<u8> {
    format!("ruum-record-v1\0{name}\0{record_uid}\0{generation}").into_bytes()
}

/// Seals `s` for the record `name` with `record_uid`.
pub(crate) fn seal(
    key: &RecordKey,
    name: &str,
    record_uid: &str,
    s: &Sealed,
) -> Result<String, String> {
    let plain =
        serde_json::to_vec(s).map_err(|_| "the sealed data does not serialize".to_owned())?;
    let mut nonce = [0u8; 12];
    rand::fill(&mut nonce[..]);
    let cipher = Aes256Gcm::new_from_slice(&key.0).map_err(|_| "the record key".to_owned())?;
    let ct = cipher
        .encrypt(
            &Nonce::from(nonce),
            Payload {
                msg: &plain,
                aad: &aad(name, record_uid, s.generation),
            },
        )
        .map_err(|_| "the seal failed".to_owned())?;
    Ok(format!(
        "v1.{}.{}.{}.{}",
        key.id(),
        s.generation,
        URL_SAFE_NO_PAD.encode(nonce),
        URL_SAFE_NO_PAD.encode(ct)
    ))
}

/// Opens a seal of the record `name` with `record_uid`. `None` for any failure (another key, a
/// changed generation, another record, a broken text): the caller refuses the record.
pub(crate) fn unseal(key: &RecordKey, name: &str, record_uid: &str, text: &str) -> Option<Sealed> {
    let mut parts = text.split('.');
    let (Some("v1"), Some(kid), Some(generation), Some(nonce), Some(ct), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return None;
    };
    if kid != key.id() {
        return None;
    }
    let generation: u64 = generation.parse().ok()?;
    let nonce: [u8; 12] = URL_SAFE_NO_PAD.decode(nonce).ok()?.try_into().ok()?;
    let ct = URL_SAFE_NO_PAD.decode(ct).ok()?;
    let cipher = Aes256Gcm::new_from_slice(&key.0).ok()?;
    let plain = cipher
        .decrypt(
            &Nonce::from(nonce),
            Payload {
                msg: &ct,
                aad: &aad(name, record_uid, generation),
            },
        )
        .ok()?;
    let s: Sealed = serde_json::from_slice(&plain).ok()?;
    (s.generation == generation).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(b: u8) -> RecordKey {
        RecordKey::new([b; 32])
    }

    fn sealed() -> Sealed {
        Sealed {
            generation: 3,
            digest: "d".into(),
            argv: vec!["node".into()],
            env: vec![("API_KEY".into(), "s3cr3t".into())],
            previous: vec!["old".into()],
            token: "tok".into(),
            run: 2,
            pod_uid: Some("uid-a".into()),
            launched: Vec::new(),
            earlier_tokens: vec!["tok-0".into()],
        }
    }

    #[test]
    fn a_seal_opens_only_for_its_record_key_and_generation() {
        let s = sealed();
        let text = seal(&key(1), "ruum-i-a", "rec-uid", &s).unwrap();
        assert!(!text.contains("s3cr3t") && !text.contains("tok"));
        assert_eq!(
            unseal(&key(1), "ruum-i-a", "rec-uid", &text),
            Some(s.clone())
        );
        assert!(
            unseal(&key(2), "ruum-i-a", "rec-uid", &text).is_none(),
            "another key"
        );
        assert!(
            unseal(&key(1), "ruum-i-b", "rec-uid", &text).is_none(),
            "another record"
        );
        assert!(
            unseal(&key(1), "ruum-i-a", "other-uid", &text).is_none(),
            "a record made again"
        );
        let changed_gen = text.replacen(".3.", ".4.", 1);
        assert!(
            unseal(&key(1), "ruum-i-a", "rec-uid", &changed_gen).is_none(),
            "a changed generation"
        );
        let mut flipped = text.clone().into_bytes();
        let n = flipped.len() - 3;
        flipped[n] = if flipped[n] == b'A' { b'B' } else { b'A' };
        let flipped = String::from_utf8(flipped).unwrap();
        assert!(
            unseal(&key(1), "ruum-i-a", "rec-uid", &flipped).is_none(),
            "a changed byte"
        );
    }

    #[test]
    fn every_seal_has_a_new_nonce() {
        let s = sealed();
        let a = seal(&key(1), "n", "u", &s).unwrap();
        let b = seal(&key(1), "n", "u", &s).unwrap();
        let nonce = |t: &str| t.split('.').nth(3).unwrap().to_owned();
        assert_ne!(nonce(&a), nonce(&b));
    }

    #[test]
    fn the_key_parses_from_hex_and_hides_itself() {
        let k = RecordKey::from_hex(&"ab".repeat(32)).unwrap();
        assert!(!format!("{k:?}").contains("abab"));
        assert!(RecordKey::from_hex("abc").is_err());
        assert!(RecordKey::from_hex(&"00".repeat(32)).is_err());
        assert!(RecordKey::from_hex(&"zz".repeat(32)).is_err());
        assert!(!format!("{:?}", sealed()).contains("s3cr3t"));
    }
}
