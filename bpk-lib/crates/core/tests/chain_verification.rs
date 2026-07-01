//! `Store::verify_chain` red fixture (W1 verifiability).
//!
//! A plain read trusts the self-reported `event_hash` (CRC-guarded only).
//! `verify_chain` instead recomputes the blake3 content hash of every committed
//! event and confirms every non-genesis link references a verified event — the
//! on-demand tamper-evidence pass that closes the "event_hash is never
//! recomputed in production" gap.

use batpak::coordinate::Coordinate;
use batpak::event::EventKind;
use batpak::store::{ChainVerification, ChainVerificationReport, Store, StoreConfig, StoreError};
use tempfile::TempDir;

#[test]
fn verify_chain_recomputes_and_confirms_an_untampered_store_is_intact() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    let kind = EventKind::custom(0xF, 0x010);
    let alice = Coordinate::new("alice", "scope").expect("coord alice");
    let bob = Coordinate::new("bob", "scope").expect("coord bob");

    let _ = store
        .append(&alice, kind, &serde_json::json!({ "n": 1 }))
        .expect("append alice 1");
    let _ = store
        .append(&bob, kind, &serde_json::json!({ "n": 2 }))
        .expect("append bob 1");
    let _ = store
        .append(&alice, kind, &serde_json::json!({ "n": 3 }))
        .expect("append alice 2");

    let report: ChainVerificationReport = store.verify_chain().expect("verify chain");
    assert!(
        report.events_checked >= 3,
        "at least the three appended events must be recomputed (got {})",
        report.events_checked
    );
    assert!(
        report.content_hash_mismatches.is_empty(),
        "an untampered store has no content-hash mismatch: {:?}",
        report.content_hash_mismatches
    );
    assert!(
        report.dangling_links.is_empty(),
        "every non-genesis prev_hash must reference a verified event: {:?}",
        report.dangling_links
    );
    assert!(report.is_intact(), "an untampered store verifies intact");
}

#[test]
fn recompute_at_open_keeps_an_untampered_multi_entity_store_intact() {
    // RED fixture (the regulated opt-in): a store opened with
    // `ChainVerification::Recompute` runs the full at-open blake3 recompute over
    // every recovered committed event and must open intact for untampered data —
    // a regular `Crc` store pays nothing, a regulated store gets the check.
    let dir = TempDir::new().expect("temp dir");
    let kind = EventKind::custom(0xF, 0x011);
    let alice = Coordinate::new("alice", "scope").expect("coord alice");
    let bob = Coordinate::new("bob", "scope").expect("coord bob");
    let carol = Coordinate::new("carol", "scope").expect("coord carol");

    {
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        for (coord, n) in [(&alice, 1), (&bob, 2), (&carol, 3), (&alice, 4)] {
            let _ = store
                .append(coord, kind, &serde_json::json!({ "n": n }))
                .expect("append");
        }
    }

    let reopened = Store::open(
        StoreConfig::new(dir.path()).with_chain_verification(ChainVerification::Recompute),
    );
    assert!(
        reopened.is_ok(),
        "ChainVerification::Recompute must open an untampered multi-entity store: {:?}",
        reopened.err()
    );
}

#[test]
fn chain_verification_failed_error_names_both_integrity_counts() {
    // The fail-closed refusal carries (and renders) both integrity counts so an
    // operator sees how the store failed verification. Referencing the variant
    // by name also keeps the new public surface test-witnessed.
    let err = StoreError::ChainVerificationFailed {
        content_hash_mismatches: 2,
        dangling_links: 1,
    };
    let rendered = err.to_string();
    assert!(
        rendered.contains('2') && rendered.contains('1'),
        "ChainVerificationFailed Display must name both counts: {rendered}"
    );
    assert!(
        rendered.contains("verification failed"),
        "ChainVerificationFailed Display must explain the refusal: {rendered}"
    );
}
