//! RED→GREEN witness: a Retention/Tombstone compaction must leave every
//! SURVIVING event that passed through the merge rewrite READABLE and
//! byte-stable.
//!
//! THE BUG (pre-fix): `write_scanned_entry` built `FramePayload { event:
//! entry.event, .. }` from a `reader::ScannedEntry` whose `event.payload` is the
//! DECODED `serde_json::Value`, then `frame_encode`d it — serializing the user
//! payload as a msgpack MAP. But the reader decodes every frame as
//! `FramePayload<Vec<u8>>` (`event.payload` must be raw BYTES). Map-where-bytes
//! ⇒ `Serialization(Syntax("invalid type: map, expected a sequence"))`. So after
//! ANY `Retention`/`Tombstone` compaction, every survivor that was rewritten was
//! present in the index yet UNREADABLE via `get`. `Merge` was unaffected (it
//! byte-copies frames). It hid because the existing compaction tests only assert
//! dropped→`NotFound` + index counts/kinds — none reads a survivor's PAYLOAD
//! back after a Retention/Tombstone merge (see the explicit note in
//! `store_ancestors_retention_coherence.rs`).
//!
//! THE FIX: carry the survivor's ORIGINAL payload bytes on `ScannedEntry` and
//! re-emit THEM verbatim, so a kept frame is byte-identical to the original and
//! its `event_hash` (blake3 over `event.payload`) is byte-stable across
//! compaction; the predicate keeps using the decoded `Value`.

use batpak::id::EntityIdType;
use batpak::store::segment::CompactionOutcome;
use batpak_testkit::prelude::*;
use tempfile::TempDir;

const KIND: EventKind = EventKind::custom(0xF, 1);

/// Survivor payload — distinctive so a faithful read-back is unambiguous.
fn survivor_payload() -> serde_json::Value {
    serde_json::json!({ "role": "survivor", "tag": "alpha", "n": 7 })
}

/// Build a store whose layout guarantees the survivor `S` and the doomed event
/// `D` land in SEALED segments that a single compaction merges (so both pass
/// through `write_scanned_entry`), while a trailing anchor keeps the ACTIVE
/// segment separate. Returns `(dir, store, s_receipt, d_receipt)`.
///
/// Layout: `S` then a >segment_max_bytes filler seal seg0; `D` then another
/// filler seal seg1; the anchor opens seg2 (active). 2 sealed ≥ `min_segments`.
fn store_with_sealed_survivor_and_doomed() -> (TempDir, Store, AppendReceipt, AppendReceipt) {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_segment_max_bytes(512)
        .with_sync_every_n_events(1);
    let store = Store::open(config).expect("open store");

    let coord_s = Coordinate::new("entity:survivor", "scope:test").expect("coord");
    let coord_fill = Coordinate::new("entity:filler", "scope:test").expect("coord");
    let coord_d = Coordinate::new("entity:doomed", "scope:test").expect("coord");
    let coord_anchor = Coordinate::new("entity:anchor", "scope:test").expect("coord");

    let s_receipt = store
        .append(&coord_s, KIND, &survivor_payload())
        .expect("append survivor");
    // Filler frame exceeds segment_max_bytes, so the next append rotates seg0.
    let _ = store
        .append(
            &coord_fill,
            KIND,
            &serde_json::json!({ "blob": "x".repeat(2000) }),
        )
        .expect("append filler 1");
    let d_receipt = store
        .append(&coord_d, KIND, &serde_json::json!({ "role": "doomed" }))
        .expect("append doomed");
    let _ = store
        .append(
            &coord_fill,
            KIND,
            &serde_json::json!({ "blob": "y".repeat(2000) }),
        )
        .expect("append filler 2");
    // Anchor opens a fresh ACTIVE segment so S and D are strictly in the sealed
    // (mergeable) set.
    let _ = store
        .append(
            &coord_anchor,
            KIND,
            &serde_json::json!({ "role": "anchor" }),
        )
        .expect("append anchor");

    (dir, store, s_receipt, d_receipt)
}

/// Shared survivor-readability + byte-stability assertions for a `S` that passed
/// through the merge rewrite.
fn assert_survivor_intact(store: &Store, s_receipt: &AppendReceipt) {
    let got = store.get(s_receipt.event_id).expect(
        "REGRESSION: a survivor that passed through the Retention/Tombstone merge rewrite \
         must be readable via get(); pre-fix this returned \
         Serialization(\"invalid type: map, expected a sequence\") because write_scanned_entry \
         re-encoded the DECODED serde_json::Value as a msgpack MAP instead of the original bytes",
    );
    assert_eq!(
        got.event.payload,
        survivor_payload(),
        "PROPERTY: a survivor's payload must read back byte-faithful to the original after compaction"
    );
    let stored_hash = got
        .event
        .hash_chain
        .expect("survivor must carry a hash chain")
        .event_hash;
    assert_eq!(
        stored_hash, s_receipt.content_hash,
        "PROPERTY: a survivor's event_hash must be UNCHANGED across compaction (byte-stability, \
         not just decodability) — re-encoding the payload would silently drift the hash"
    );

    // walk_ancestors reads the survivor's frame too: it must surface S with its
    // original payload (S is the genesis of its entity, so the walk is exactly [S]).
    let walk = store.walk_ancestors_outcome(s_receipt.event_id, 16);
    let walked = walk
        .ancestors
        .iter()
        .find(|e| e.event.event_id() == s_receipt.event_id)
        .map(|e| e.event.payload.clone());
    assert_eq!(
        walked,
        Some(survivor_payload()),
        "PROPERTY: walk_ancestors must surface the survivor with its original payload after compaction"
    );
}

#[test]
fn retention_survivor_reads_back_byte_stable() {
    let (_dir, store, s_receipt, d_receipt) = store_with_sealed_survivor_and_doomed();

    let d_raw = d_receipt.event_id.as_u128();
    let (result, _report) = store
        .compact(&CompactionConfig {
            min_segments: 1,
            strategy: CompactionStrategy::Retention(Box::new(move |stored| {
                stored.event.event_id().as_u128() != d_raw
            })),
        })
        .expect("compact");

    // Non-vacuity: the merge actually ran AND rewrote sealed frames (dropping D),
    // so the survivor below was genuinely re-emitted by write_scanned_entry.
    assert!(
        matches!(result.outcome, CompactionOutcome::Performed),
        "precondition: Retention compaction must have PERFORMED a merge (outcome={:?})",
        result.outcome
    );
    assert!(
        result.segments_removed >= 1,
        "precondition: the merge must have consumed ≥1 sealed segment (got {})",
        result.segments_removed
    );
    assert!(
        matches!(store.get(d_receipt.event_id), Err(StoreError::NotFound(_))),
        "precondition: the Retention-dropped event must be ABSENT (proves the merge filtered + \
         rewrote frames, not a no-op)"
    );

    assert_survivor_intact(&store, &s_receipt);

    store.close().expect("close");
}

#[test]
fn tombstone_survivor_reads_back_byte_stable() {
    let (_dir, store, s_receipt, d_receipt) = store_with_sealed_survivor_and_doomed();

    let d_raw = d_receipt.event_id.as_u128();
    // Tombstone keeps every event; predicate FALSE ⇒ the event's kind is rewritten
    // to TOMBSTONE. Keep S (true), tombstone D (false).
    let (result, _report) = store
        .compact(&CompactionConfig {
            min_segments: 1,
            strategy: CompactionStrategy::Tombstone(Box::new(move |stored| {
                stored.event.event_id().as_u128() != d_raw
            })),
        })
        .expect("compact");

    assert!(
        matches!(result.outcome, CompactionOutcome::Performed),
        "precondition: Tombstone compaction must have PERFORMED a merge (outcome={:?})",
        result.outcome
    );
    assert!(
        result.segments_removed >= 1,
        "precondition: the merge must have consumed ≥1 sealed segment (got {})",
        result.segments_removed
    );
    // Non-vacuity: D was genuinely tombstoned (header rewritten through the merge).
    // Index-only check, so it is robust independent of the survivor read path.
    let tombstoned = store
        .query(&Region::entity("entity:doomed").with_fact(KindFilter::Exact(EventKind::TOMBSTONE)));
    assert_eq!(
        tombstoned.len(),
        1,
        "precondition: the doomed event must be tombstoned in the index after compaction"
    );

    assert_survivor_intact(&store, &s_receipt);

    store.close().expect("close");
}
