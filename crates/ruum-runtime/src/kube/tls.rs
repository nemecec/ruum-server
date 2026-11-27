//! The TLS identity of the supervisor listener (backlog I620-2; `docs/STEP3.md` "The sandbox
//! supervisor" 4, "Transport").
//!
//! The launch channel carries the run's secret values from a server pod to a sandbox, and a
//! sandbox's traffic to a server pod on another node crosses the network between the nodes. So
//! the listener speaks TLS 1.3 with a key that every server replica holds and the supervisor
//! pins:
//!
//! - **The key**: an Ed25519 key whose seed is derived from the record key
//!   (`RecordKey::derive("ruum-supervisor-tls-v1")`). The record key is mounted only into the
//!   server pods (no other Secret is needed), every replica derives the same key, and whoever
//!   holds the record key can read every seal anyway.
//! - **The certificate**: self-signed, a fixed template (the subject `ruum-supervisor`, no
//!   extensions, a validity to 9999). Ed25519 signatures are deterministic, so every replica
//!   makes the same certificate, byte for byte.
//! - **The pin**: `sha256:<hex>` of the certificate's DER. The driver puts it into the pod's
//!   command (`--server-pin`), so the exhaustive read-back (`pod::verify`) holds it, and the
//!   supervisor accepts only that certificate and checks the handshake's signature with its key.
//!   No certificate authority, no name check, no expiry to renew.
//!
//! A holder of the runtime token can make a pod with another pin, but such a pod gets no launch
//! data (the seal names the pod a server process made). A network attacker between the nodes
//! cannot read or change a launch answer without the key.

use super::seal::RecordKey;
use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair};
use sha2::{Digest, Sha256};

/// The label of the derived key: a new label is a new key (and a new pin in every new pod).
const LABEL: &str = "ruum-supervisor-tls-v1";

/// The listener's certificate (DER), its private key (PKCS#8 DER) and the pin of the
/// certificate. No `Debug` that shows the key.
pub struct SupervisorTls {
    pub cert_der: Vec<u8>,
    pub key_pkcs8: Vec<u8>,
    pub pin: String,
}

impl std::fmt::Debug for SupervisorTls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SupervisorTls({})", self.pin)
    }
}

/// The Ed25519 algorithm identifier (`1.3.101.112`), without parameters.
const ED25519_ALG: [u8; 7] = [0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70];

/// `CN=ruum-supervisor` as an X.509 Name (28 bytes).
fn name() -> Vec<u8> {
    let cn = b"ruum-supervisor";
    let mut atv = vec![0x06, 0x03, 0x55, 0x04, 0x03, 0x0c, cn.len() as u8];
    atv.extend_from_slice(cn);
    let mut seq = vec![0x30, atv.len() as u8];
    seq.extend(atv);
    let mut set = vec![0x31, seq.len() as u8];
    set.extend(seq);
    let mut out = vec![0x30, set.len() as u8];
    out.extend(set);
    out
}

/// A DER SEQUENCE of `content` (its length below 65 536).
fn sequence(content: &[u8]) -> Vec<u8> {
    let n = content.len();
    let mut out = vec![0x30];
    if n < 0x80 {
        out.push(n as u8);
    } else if n < 0x100 {
        out.extend([0x81, n as u8]);
    } else {
        out.extend([0x82, (n >> 8) as u8, n as u8]);
    }
    out.extend_from_slice(content);
    out
}

/// The `SubjectPublicKeyInfo` of an Ed25519 public key.
fn spki(public: &[u8]) -> Vec<u8> {
    let mut bits = vec![0x03, (public.len() + 1) as u8, 0x00];
    bits.extend_from_slice(public);
    let mut c = ED25519_ALG.to_vec();
    c.extend(bits);
    sequence(&c)
}

/// The self-signed certificate of `pair` (the fixed template above).
fn certificate(pair: &Ed25519KeyPair) -> Vec<u8> {
    let mut tbs = Vec::new();
    tbs.extend([0xa0, 0x03, 0x02, 0x01, 0x02]); // version 3
    tbs.extend([0x02, 0x01, 0x01]); // serial number 1
    tbs.extend(ED25519_ALG);
    tbs.extend(name()); // issuer
    let mut validity = Vec::new();
    for t in [b"20260101000000Z", b"99991231235959Z"] {
        validity.extend([0x18, t.len() as u8]); // GeneralizedTime
        validity.extend_from_slice(t);
    }
    tbs.extend(sequence(&validity));
    tbs.extend(name()); // subject
    tbs.extend(spki(pair.public_key().as_ref()));
    let tbs = sequence(&tbs);
    let sig = pair.sign(&tbs);
    let mut bits = vec![0x03, (sig.as_ref().len() + 1) as u8, 0x00];
    bits.extend_from_slice(sig.as_ref());
    let mut cert = tbs;
    cert.extend(ED25519_ALG);
    cert.extend(bits);
    sequence(&cert)
}

/// The pin of a certificate: `sha256:` and the hex SHA-256 of its DER.
pub fn pin_of(cert_der: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(cert_der)))
}

impl SupervisorTls {
    /// The listener's identity derived from the record key (the same in every replica).
    pub fn derive(key: &RecordKey) -> Result<Self, String> {
        let seed = key.derive(LABEL);
        let pair = Ed25519KeyPair::from_seed_unchecked(&seed)
            .map_err(|_| "the supervisor listener's TLS key cannot be made".to_owned())?;
        let key_pkcs8 = pair
            .to_pkcs8()
            .map_err(|_| "the supervisor listener's TLS key cannot be encoded".to_owned())?
            .as_ref()
            .to_vec();
        let cert_der = certificate(&pair);
        let pin = pin_of(&cert_der);
        Ok(Self {
            cert_der,
            key_pkcs8,
            pin,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_replica_derives_the_same_certificate_and_another_key_another() {
        let k = RecordKey::new([7u8; 32]);
        let a = SupervisorTls::derive(&k).unwrap();
        let b = SupervisorTls::derive(&k).unwrap();
        assert_eq!(a.cert_der, b.cert_der);
        assert_eq!(a.pin, b.pin);
        assert_eq!(a.pin, pin_of(&a.cert_der));
        assert!(a.pin.starts_with("sha256:") && a.pin.len() == 7 + 64);
        let c = SupervisorTls::derive(&RecordKey::new([8u8; 32])).unwrap();
        assert_ne!(a.pin, c.pin);
        // The fixed template: 231 bytes, one SEQUENCE.
        assert_eq!(a.cert_der.len(), 231);
        assert_eq!(&a.cert_der[..3], &[0x30, 0x81, 0xe4]);
        // The debug form names the pin only.
        assert!(!format!("{a:?}").contains(&hex::encode(&a.key_pkcs8)));
    }
}
