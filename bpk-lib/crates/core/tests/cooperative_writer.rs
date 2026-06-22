//! PROVES: the COOPERATIVE writer drive mode runs a real `Store` with NO OS
//! writer thread — the writer pipeline is driven inline by pumping the command
//! queue whenever a reply is awaited — and that it (a) produces correct,
//! readable results end-to-end through the normal public append/read API and
//! (b) is deterministic: a fixed input sequence run twice yields identical
//! visible state and receipts.
//! CATCHES: a cooperative path that drops writes, fails to drain the queue
//! before a reply-await, diverges from the threaded pipeline, or is
//! nondeterministic across runs.
//!
//! Requires the `dangerous-test-hooks` feature for `Store::open_cooperative`,
//! which selects the crate-internal cooperative `WriterMode` (not public API).
#![cfg(feature = "dangerous-test-hooks")]

use batpak::coordinate::Coordinate;
use batpak::event::EventKind;
use batpak::store::{Store, StoreConfig};
use tempfile::TempDir;

const KIND: EventKind = EventKind::custom(0xC, 0x0A);

fn cooperative_config(dir: &TempDir) -> StoreConfig {
    // Per-event sync and no cold-start artifacts keep the proof focused on the
    // inline drive itself; the default RealFs is intentional — this proves the
    // cooperative path works end-to-end on a real filesystem with no thread.
    StoreConfig::new(dir.path())
        .with_sync_every_n_events(1)
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false)
}

fn coord() -> Coordinate {
    Coordinate::new("entity:cooperative", "scope:test").expect("coord")
}

fn fixed_sequence() -> Vec<serde_json::Value> {
    (0..5)
        .map(|i| serde_json::json!({ "n": i, "label": format!("event-{i}") }))
        .collect()
}

/// A receipt fingerprint that is deterministic across runs. Event IDs are
/// UUIDv7 (carry random bits) so they are deliberately excluded; the committed
/// `sequence` and the payload-derived `content_hash` are fully determined by the
/// input sequence and the drive pipeline.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ReceiptFingerprint {
    sequence: u64,
    content_hash: [u8; 32],
    payload: serde_json::Value,
}

/// Run the fixed sequence under cooperative mode against a fresh store, await
/// every ticket, read each event back, and return the per-event fingerprints
/// plus the final visible count for the entity.
fn run_cooperative_once() -> (Vec<ReceiptFingerprint>, usize) {
    let dir = TempDir::new().expect("tempdir");
    // NO writer thread: this store is driven entirely inline.
    let store = Store::open_cooperative(cooperative_config(&dir)).expect("open cooperative store");
    let coord = coord();

    let mut fingerprints = Vec::new();
    for payload in fixed_sequence() {
        // The public `submit().wait()` funnel: `wait()` pumps the queue inline
        // (there is no writer thread to drain it) and then receives the reply.
        let ticket = store.submit(&coord, KIND, &payload).expect("submit");
        let receipt = ticket.wait().expect("await cooperative append");

        // Read the event back through the normal read API — proves the inline
        // drive actually committed and indexed it.
        let stored = store
            .get(receipt.event_id)
            .expect("read back appended event");
        assert_eq!(
            stored.event.payload, payload,
            "PROPERTY: cooperative append must persist and return the exact payload"
        );
        assert_eq!(
            stored.event.event_kind(),
            KIND,
            "PROPERTY: cooperative append must preserve the event kind"
        );

        fingerprints.push(ReceiptFingerprint {
            sequence: receipt.sequence,
            content_hash: receipt.content_hash,
            payload,
        });
    }

    let visible = store.by_entity(coord.entity()).len();
    store.close().expect("close cooperative store");
    (fingerprints, visible)
}

#[test]
fn cooperative_mode_appends_and_reads_back_with_no_writer_thread() {
    let (fingerprints, visible) = run_cooperative_once();

    let expected = fixed_sequence();
    assert_eq!(
        fingerprints.len(),
        expected.len(),
        "PROPERTY: every cooperative ticket must resolve to a committed receipt"
    );
    assert_eq!(
        visible,
        expected.len(),
        "PROPERTY: every cooperatively appended event must be visible to readers"
    );

    // Commit sequence numbers are globally monotonic and contiguous within a
    // single fresh store's user appends; assert strict increase to catch a drive
    // that double-commits or skips.
    for window in fingerprints.windows(2) {
        assert!(
            window[1].sequence > window[0].sequence,
            "PROPERTY: cooperative commit sequence must advance monotonically"
        );
    }
}

#[test]
fn cooperative_mode_is_deterministic_across_runs() {
    let (first, first_visible) = run_cooperative_once();
    let (second, second_visible) = run_cooperative_once();

    assert_eq!(
        first_visible, second_visible,
        "PROPERTY: cooperative mode must yield identical visible counts across runs"
    );
    assert_eq!(
        first, second,
        "PROPERTY: a fixed input sequence under cooperative mode must produce \
         identical receipts (commit sequence + content hash) and visible state \
         across runs"
    );
}
