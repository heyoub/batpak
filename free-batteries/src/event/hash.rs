use serde::{Deserialize, Serialize};

/// HashChain: prev_hash + event_hash. Per-entity linear chain.
/// Default (all zeros) = genesis convention.
/// [SPEC:src/event/hash.rs — NO TRAIT. NO ENUM.]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HashChain {
    pub prev_hash: [u8; 32],
    pub event_hash: [u8; 32],
}

/// compute_hash: blake3 hash of content bytes.
/// Behind feature = "blake3". When off, Committed.hash is [0u8; 32].
/// [SPEC:INVARIANTS item 5 — blake3 is the only hash]
/// [DEP:blake3::hash] → returns blake3::Hash, .into() gives [u8; 32]
#[cfg(feature = "blake3")]
pub fn compute_hash(content_bytes: &[u8]) -> [u8; 32] {
    blake3::hash(content_bytes).into()
}

/// verify_chain: check that event_hash matches content AND prev_hash matches expected.
/// [SPEC:src/event/hash.rs — verify_chain]
#[cfg(feature = "blake3")]
pub fn verify_chain(content_bytes: &[u8], chain: &HashChain, expected_prev: &[u8; 32]) -> bool {
    chain.prev_hash == *expected_prev && chain.event_hash == compute_hash(content_bytes)
}
