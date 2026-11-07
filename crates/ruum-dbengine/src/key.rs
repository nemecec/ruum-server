//! The pseudonym key of a database, as a handle (decision 0004-30).
//!
//! The platform unwraps the key (card Q-4) and hands the engine a [`PseudonymKey`] when it
//! creates a database, copies one (branch, copy, restore into a new node) and opens a SQLite
//! connection. The engine installs it (Postgres: `ruum_mask.key`; SQLite: the per-connection
//! registration of `ruum_pseudonym`) and never gives it back: no method of the trait returns a
//! key, the handle has no public accessor, it does not serialize, and `Debug` prints no bytes.

use std::fmt;
use std::sync::Arc;

/// The length of a pseudonym key in bytes (HMAC-SHA-256 key, decision 0004-4).
pub const KEY_LEN: usize = 32;

/// A handle of the pseudonym key of one database: the tenant key, or the key derived for
/// `databases.key_node_id` (decision 0004-25). Cheap to clone; the bytes stay inside.
#[derive(Clone)]
pub struct PseudonymKey(Arc<[u8; KEY_LEN]>);

impl PseudonymKey {
    /// Wraps the plain key that the key store (Q-4) unwrapped. The caller drops its own copy.
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(Arc::new(bytes))
    }

    /// The key bytes, for the engines of this crate only: E-2 writes them into `ruum_mask.key`,
    /// E-3 registers `ruum_pseudonym` with them. Never returned through the trait.
    #[allow(dead_code)] // used by the Postgres (E-2) and SQLite (E-3) engines
    pub(crate) fn expose(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

impl fmt::Debug for PseudonymKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PseudonymKey(<hidden>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_the_key() {
        let key = PseudonymKey::from_bytes([0xab; KEY_LEN]);
        let text = format!("{key:?}");
        assert_eq!(text, "PseudonymKey(<hidden>)");
        assert!(!text.contains("ab"));
        assert!(!text.contains("171"));
    }

    #[test]
    fn the_engine_sees_the_bytes_handed_in() {
        let key = PseudonymKey::from_bytes([7; KEY_LEN]);
        assert_eq!(key.clone().expose(), &[7; KEY_LEN]);
    }
}
