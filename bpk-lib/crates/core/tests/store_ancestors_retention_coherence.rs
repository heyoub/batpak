//! W3/C5 coherence proof: a Retention compaction that drops a MID-CHAIN event
//! must make a surviving descendant's ancestry walk REPORT the truncation
//! instead of returning a short prefix that is indistinguishable from a chain
//! that genuinely reached genesis.
//!
//! PROVES: `Store::walk_ancestors_outcome` distinguishes
//! `AncestryBoundary::ReachedGenesis` (complete) from
//! `AncestryBoundary::MissingParent` (truncated at a retention-dropped link).
//! DEFENDS: the silent mid-chain ancestry loss W3/C5 — a dropped link no longer
//! collapses into the same bare `Vec` shape as a chain that reached genesis.
//!
//! NOTE on construction: Retention compaction rewrites only SEALED segments,
//! and a surviving descendant must be readable for the walk to traverse it, so
//! the walk anchor and the readable tail of the chain are kept in the ACTIVE
//! segment while the dropped mid-chain event sits in a prior SEALED segment.
//! The walk therefore reaches the dangling link after reading only live tail
//! events. (Reading deeper survivors of a Retention/Tombstone merge is a
//! separate concern outside this W3/C5 walk-coherence proof.)

use batpak::id::EntityIdType;
use batpak::store::segment::CompactionOutcome;
use batpak::store::{AncestorWalk, AncestryBoundary};
use batpak_testkit::prelude::*;
use tempfile::TempDir;

/// Small-segment store so a handful of appends seal multiple segments, making a
/// mid-chain event droppable by Retention compaction.
fn small_store() -> (TempDir, Store) {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_segment_max_bytes(512)
        .with_sync_every_n_events(1);
    let store = Store::open(config).expect("open store");
    (dir, store)
}

#[test]
fn retention_dropped_midchain_event_makes_walk_report_truncation() {
    let (_dir, store) = small_store();
    let kind = EventKind::custom(0xF, 1);
    let coord = Coordinate::new("entity:retention-walk", "scope:test").expect("coord");

    // Chain c0..c2 (c2 is the mid-chain DROP target), then a large filler event
    // that forces the active segment to rotate-and-seal c2's segment, then the
    // single live anchor c3 which — being the last appended event — stays in
    // the fresh ACTIVE segment and is therefore readable. c3's prev_hash points
    // at c2 (its same-entity predecessor); dropping c2 makes that link dangle.
    let mut chain = Vec::new();
    for step in 0..3 {
        chain.push(
            store
                .append(&coord, kind, &serde_json::json!({ "step": step }))
                .expect("append head"),
        );
    }

    // Large filler (different entity) whose frame exceeds segment_max_bytes,
    // guaranteeing the anchor append below rotates c2's segment into the sealed
    // set so Retention can drop it.
    let filler_coord = Coordinate::new("entity:retention-filler", "scope:test").expect("coord");
    let filler_blob = "x".repeat(1200);
    let _ = store
        .append(
            &filler_coord,
            kind,
            &serde_json::json!({ "filler": filler_blob }),
        )
        .expect("append filler");

    // The anchor: last appended event of `coord`, lands in the active segment.
    chain.push(
        store
            .append(&coord, kind, &serde_json::json!({ "step": 3 }))
            .expect("append anchor"),
    );

    let genesis = chain[0].event_id;
    let dropped = chain[2].event_id;
    // The anchor IS the dangling child: its parent c2 is the dropped event.
    let anchor = chain[3].event_id;
    let dangling_child = anchor;
    assert_ne!(
        dropped, genesis,
        "precondition: the drop target must NOT be the genesis event"
    );
    assert_ne!(
        dropped, anchor,
        "precondition: the drop target must be mid-chain, NOT the walk anchor"
    );

    let dropped_raw = dropped.as_u128();
    let (result, _report) = store
        .compact(&CompactionConfig {
            min_segments: 1,
            strategy: CompactionStrategy::Retention(Box::new(move |event| {
                event.event.event_id().as_u128() != dropped_raw
            })),
        })
        .expect("compact");

    // PRECONDITIONS (keep the proof non-vacuous): compaction PERFORMED, the
    // mid-chain event is really gone, and the surviving anchor is still
    // readable. If the mid-chain target had not actually been sealed and
    // dropped, the NotFound assertion below fails LOUDLY instead of the test
    // proving nothing.
    assert!(
        matches!(result.outcome, CompactionOutcome::Performed),
        "precondition: Retention compaction must have PERFORMED a merge (outcome={:?})",
        result.outcome
    );
    assert!(
        matches!(store.get(dropped), Err(StoreError::NotFound(_))),
        "precondition: the targeted mid-chain event must be ABSENT after Retention compaction; \
         if it still resolves, no parent link dangles and this test is vacuous"
    );
    assert!(
        store.get(anchor).is_ok(),
        "precondition: the walk anchor (live tail) must remain readable"
    );

    // CORE: walking from the live anchor must now REPORT the truncation at the
    // surviving child whose parent (the dropped event) is absent — not silently
    // stop short with a bare Vec.
    let truncated: AncestorWalk = store.walk_ancestors_outcome(anchor, 64);
    assert!(
        !truncated.reached_genesis(),
        "PROPERTY: a chain truncated at a retention-dropped mid-chain link must NOT report reached-genesis"
    );
    assert_eq!(
        truncated.boundary,
        AncestryBoundary::MissingParent {
            child: dangling_child
        },
        "PROPERTY: the walk must report MissingParent at the surviving child whose parent (the dropped event) is absent"
    );
    assert_eq!(
        truncated.truncated_at(),
        Some(dangling_child),
        "PROPERTY: truncated_at() must name the surviving child where the parent link dangles"
    );

    let walked_ids: Vec<EventId> = truncated
        .ancestors
        .iter()
        .map(|stored| stored.event.event_id())
        .collect();
    assert_eq!(
        walked_ids,
        vec![anchor],
        "PROPERTY: the truncated prefix must be exactly the readable anchor whose parent link dangles"
    );
    assert!(
        !walked_ids.contains(&dropped),
        "PROPERTY: the dropped mid-chain event must not appear in the truncated walk prefix"
    );

    store.close().expect("close");
}

#[test]
fn intact_chain_walk_reports_reached_genesis() {
    // CONTROL: a chain with no dropped links must report reached-genesis with no
    // truncation, proving the new boundary is not vacuously always-MissingParent.
    let (_dir, store) = small_store();
    let kind = EventKind::custom(0xF, 1);
    let coord = Coordinate::new("entity:intact-walk", "scope:test").expect("coord");

    let chain: Vec<AppendReceipt> = (0..4)
        .map(|step| {
            store
                .append(&coord, kind, &serde_json::json!({ "step": step }))
                .expect("append")
        })
        .collect();
    let genesis = chain.first().expect("genesis").event_id;
    let last = chain.last().expect("last").event_id;

    let complete = store.walk_ancestors_outcome(last, 64);
    assert!(
        complete.reached_genesis(),
        "PROPERTY: an intact chain must report reached-genesis"
    );
    assert_eq!(
        complete.boundary,
        AncestryBoundary::ReachedGenesis,
        "PROPERTY: an intact chain's boundary must be ReachedGenesis"
    );
    assert_eq!(
        complete.truncated_at(),
        None,
        "PROPERTY: a complete chain has no truncation point"
    );
    assert_eq!(
        complete
            .ancestors
            .last()
            .map(|stored| stored.event.event_id()),
        Some(genesis),
        "PROPERTY: the complete walk must reach the genesis event"
    );

    store.close().expect("close");
}
