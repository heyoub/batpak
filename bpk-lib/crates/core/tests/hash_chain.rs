//! Proptest verification of hash chain integrity.
//! Chain verification, tamper detection, genesis event semantics.
//!
//! PROVES: LAW-001 (No Fake Success — tampered chains must fail verification)
//! DEFENDS: FM-022 (Receipt Hollowing — hash integrity prevents forgery)
//! INVARIANTS: INV-HASH-CHAIN-INTEGRITY (cryptographic chain integrity)

use batpak::event::hash::{compute_hash, verify_chain};
use batpak_testkit::prelude::*;
use proptest::prelude::*;

#[path = "common/proptest.rs"]
mod proptest_support;

// --- Genesis ---

#[test]
fn genesis_has_zero_prev_hash() {
    let chain = HashChain::default();
    assert_eq!(
        chain.prev_hash, [0u8; 32],
        "GENESIS INVARIANT VIOLATED: default HashChain must have prev_hash = [0u8; 32]. \
         Investigate: src/event/hash.rs Default impl."
    );
}

#[test]
fn genesis_has_zero_event_hash() {
    let chain = HashChain::default();
    assert_eq!(
        chain.event_hash, [0u8; 32],
        "GENESIS INVARIANT VIOLATED: default HashChain must have event_hash = [0u8; 32]. \
         Investigate: src/event/hash.rs Default impl."
    );
}

// --- Blake3 compute_hash ---

#[test]
fn compute_hash_deterministic() {
    let data = b"hello world";
    let h1 = compute_hash(data);
    let h2 = compute_hash(data);
    assert_eq!(
        h1, h2,
        "HASH DETERMINISM VIOLATED: same input must produce same hash."
    );
}

#[test]
fn compute_hash_different_inputs() {
    let h1 = compute_hash(b"hello");
    let h2 = compute_hash(b"world");
    assert_ne!(
        h1, h2,
        "HASH COLLISION: different inputs must produce different hashes \
        (with overwhelming probability)."
    );
}

#[test]
fn compute_hash_empty_input() {
    let h = compute_hash(b"");
    assert_ne!(
        h, [0u8; 32],
        "Empty input should still produce a non-zero hash."
    );
}

proptest! {
    #![proptest_config(proptest_support::cfg(256))]

    #[test]
    fn verify_chain_accepts_valid(content in proptest::collection::vec(any::<u8>(), 0..100)) {
        let prev_hash = [0u8; 32]; // genesis
        let event_hash = compute_hash(&content);
        let chain = HashChain { prev_hash, event_hash };
        prop_assert!(
            verify_chain(&content, &chain, &prev_hash),
            "CHAIN VERIFICATION FAILED: valid chain should verify. \
             Investigate: src/event/hash.rs verify_chain."
        );
    }

    #[test]
    fn verify_chain_rejects_tampered_content(
        content in proptest::collection::vec(any::<u8>(), 1..100),
    ) {
        let prev_hash = [0u8; 32];
        let event_hash = compute_hash(&content);
        let chain = HashChain { prev_hash, event_hash };

        // Tamper: flip the first byte
        let mut tampered = content.clone();
        tampered[0] = tampered[0].wrapping_add(1);

        prop_assert!(
            !verify_chain(&tampered, &chain, &prev_hash),
            "TAMPER DETECTION FAILED: tampered content should not verify. \
             Investigate: src/event/hash.rs verify_chain."
        );
    }

    #[test]
    fn verify_chain_rejects_wrong_prev_hash(
        content in proptest::collection::vec(any::<u8>(), 0..100),
    ) {
        let prev_hash = [0u8; 32];
        let event_hash = compute_hash(&content);
        let chain = HashChain { prev_hash, event_hash };

        let wrong_prev = [1u8; 32];
        prop_assert!(
            !verify_chain(&content, &chain, &wrong_prev),
            "PREV_HASH CHECK FAILED: wrong prev_hash should not verify. \
             Investigate: src/event/hash.rs verify_chain."
        );
    }
}
