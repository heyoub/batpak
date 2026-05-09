use serde::{Deserialize, Serialize};

/// HashChain: prev_hash + event_hash. Per-entity linear chain.
/// Default (all zeros) = genesis convention.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HashChain {
    /// Blake3 hash of the immediately preceding event; all-zeros signals genesis.
    pub prev_hash: [u8; 32],
    /// Blake3 hash of this event's serialized content bytes.
    pub event_hash: [u8; 32],
}

/// compute_hash: blake3 hash of content bytes.
/// Behind feature = "blake3". When off, Committed.hash is [0u8; 32].
/// `blake3` is the only supported event hash algorithm.
/// \[DEP:blake3::hash\] → returns blake3::Hash, .into() gives \[u8; 32\]
#[cfg(feature = "blake3")]
#[cfg_attr(all(docsrs, not(batpak_stable_docs)), doc(cfg(feature = "blake3")))]
pub fn compute_hash(content_bytes: &[u8]) -> [u8; 32] {
    blake3::hash(content_bytes).into()
}

/// verify_chain: check that event_hash matches content AND prev_hash matches expected.
#[cfg(feature = "blake3")]
#[cfg_attr(all(docsrs, not(batpak_stable_docs)), doc(cfg(feature = "blake3")))]
pub fn verify_chain(content_bytes: &[u8], chain: &HashChain, expected_prev: &[u8; 32]) -> bool {
    chain.prev_hash == *expected_prev && chain.event_hash == compute_hash(content_bytes)
}
