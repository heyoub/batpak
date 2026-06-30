//! `Store::verify_chain` red fixture (W1 verifiability).
//!
//! A plain read trusts the self-reported `event_hash` (CRC-guarded only).
//! `verify_chain` instead recomputes the blake3 content hash of every committed
//! event and confirms every non-genesis link references a verified event — the
//! on-demand tamper-evidence pass that closes the "event_hash is never
//! recomputed in production" gap.

use batpak::coordinate::Coordinate;
use batpak::event::EventKind;
use batpak::store::{ChainVerificationReport, Store, StoreConfig};
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
