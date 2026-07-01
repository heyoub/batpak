//! Stage E1 coverage: the two CORE-INTERNAL read consumers that fold/inspect a
//! DECODED payload — projection replay and content-based compaction — are made
//! KEY-AWARE, so an encrypted entity no longer fails closed at the Stage C
//! Value-decode seam.
//!
//! Witnesses:
//!   * a projection over an ENCRYPTED entity replays to the correct state
//!     (each event is decrypted before the fold);
//!   * a Retention/Tombstone compaction whose predicate INSPECTS the payload
//!     works over encrypted entities (the predicate sees PLAINTEXT) AND every
//!     kept survivor re-reads byte-stable (ciphertext + `event_hash` unchanged);
//!   * a crypto-shredded event hits the chosen semantics in each path —
//!     projection SKIPS it (skip-with-awareness), compaction KEEPS it
//!     (conservative: cannot drop what it cannot read) — never a panic.
//!
//! Gated behind `payload-encryption` (the whole file compiles out of a default
//! build); the plaintext (`None`) projection + compaction suites are the
//! byte-identical regression baseline and stay green without this file.
#![cfg(feature = "payload-encryption")]

use batpak::store::segment::CompactionOutcome;
use batpak::store::{
    Freshness, KeyScopeGranularity, ReadDisposition, ShredScope, StoreConfig, StoreError,
};
use batpak_testkit::prelude::*;
use tempfile::TempDir;

const KIND: EventKind = EventKind::custom(0xF, 1);

fn open_encrypted(dir: &std::path::Path, granularity: KeyScopeGranularity) -> Store {
    Store::open(StoreConfig::new(dir).with_payload_encryption(granularity)).expect("open encrypted")
}

// ── A projection that folds over the DECODED payload ────────────────────────
//
// It reads a `"n"` field out of every event's payload, so a correct total can
// only come from the DECRYPTED plaintext — folding over ciphertext (or a `Null`
// placeholder) would read no `"n"` and leave the sum at zero.
#[derive(Default, serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq)]
struct SumState {
    total: i64,
    count: u64,
}

impl EventSourced for SumState {
    type Input = JsonValueInput;
    const STATE_CONTRACT: ProjectionStateContract =
        ProjectionStateContract::single_entity("crypto-shred-e1-sum");

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        let mut state = SumState::default();
        for event in events {
            state.apply_event(event);
        }
        Some(state)
    }

    fn apply_event(&mut self, event: &Event<serde_json::Value>) {
        if let Some(n) = event.payload.get("n").and_then(serde_json::Value::as_i64) {
            self.total += n;
            self.count += 1;
        }
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[KIND]
    }

    fn state_extent(&self) -> StateExtent {
        StateExtent::single_entity()
    }
}

#[test]
fn projection_replays_an_encrypted_entity_to_the_correct_state() {
    let dir = TempDir::new().expect("tmpdir");
    let store = open_encrypted(dir.path(), KeyScopeGranularity::PerEntity);
    let coord = Coordinate::new("entity:proj", "scope:c").expect("coord");

    for n in [10_i64, 20, 30] {
        let _ = store
            .append(&coord, KIND, &serde_json::json!({ "n": n }))
            .expect("encrypted append");
    }

    // On disk each payload is CIPHERTEXT: pre-Stage-E1 the projection would have
    // FAILED CLOSED at the Value-decode seam. Key-aware replay decrypts each
    // event before the fold, so the total is the sum of the plaintext `n`s.
    let state: Option<SumState> = store
        .project("entity:proj", &Freshness::Consistent)
        .expect("project over encrypted entity");
    assert_eq!(
        state,
        Some(SumState {
            total: 60,
            count: 3
        }),
        "PROPERTY: projection over an encrypted entity must fold the DECRYPTED payloads"
    );

    store.close().expect("close");
}

#[test]
fn projection_skips_a_shredded_event_with_awareness() {
    // PerEvent granularity so a single event can be shredded while its siblings
    // stay readable — the projection then folds the survivors and SKIPS the
    // shredded one (skip-with-awareness), never panicking or misdecoding.
    let dir = TempDir::new().expect("tmpdir");
    let store = open_encrypted(dir.path(), KeyScopeGranularity::PerEvent);
    let coord = Coordinate::new("entity:proj-shred", "scope:c").expect("coord");

    let first = store
        .append(&coord, KIND, &serde_json::json!({ "n": 10 }))
        .expect("append first");
    let middle = store
        .append(&coord, KIND, &serde_json::json!({ "n": 20 }))
        .expect("append middle");
    let _last = store
        .append(&coord, KIND, &serde_json::json!({ "n": 30 }))
        .expect("append last");

    // Baseline: all three fold in.
    let full: Option<SumState> = store
        .project("entity:proj-shred", &Freshness::Consistent)
        .expect("baseline project");
    assert_eq!(
        full,
        Some(SumState {
            total: 60,
            count: 3
        })
    );

    // Crypto-shred exactly the middle event's key.
    assert!(
        store
            .shred_scope(ShredScope::Event(middle.event_id))
            .expect("shred the middle event"),
        "the middle event's per-event key existed and was destroyed"
    );
    assert!(
        matches!(
            store
                .get_shreddable(middle.event_id)
                .expect("get_shreddable"),
            ReadDisposition::Shredded
        ),
        "sanity: the middle event now reads Shredded"
    );

    // The projection SKIPS the shredded middle event and folds only the two
    // survivors — state is honestly aware it omits the erased event's effect.
    let after: Option<SumState> = store
        .project("entity:proj-shred", &Freshness::Consistent)
        .expect("project after shred (must not panic or fail closed)");
    assert_eq!(
        after,
        Some(SumState {
            total: 40, // 10 + 30; the shredded 20 is skipped
            count: 2,
        }),
        "PROPERTY: a shredded event is SKIPPED during replay; the state omits its effect \
         (skip-with-awareness), and the still-readable siblings still decrypt"
    );

    // The readable siblings still decrypt directly, confirming a scoped shred
    // did not disturb them.
    assert_eq!(
        store
            .get(first.event_id)
            .expect("first decrypts")
            .event
            .payload,
        serde_json::json!({ "n": 10 })
    );

    store.close().expect("close");
}

// ── Sealed-segment layout for compaction (mirrors store_compaction_survivor) ──
//
// Builds an ENCRYPTED store whose survivor `S` and doomed `D` land in SEALED
// segments a single compaction merges (both pass through the survivor rewrite),
// while a trailing anchor keeps the ACTIVE segment separate.
fn encrypted_store_with_sealed_survivor_and_doomed(
) -> (TempDir, Store, AppendReceipt, AppendReceipt) {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_payload_encryption(KeyScopeGranularity::PerEntity)
        .with_segment_max_bytes(512)
        .with_sync_every_n_events(1);
    let store = Store::open(config).expect("open encrypted store");

    let coord_s = Coordinate::new("entity:survivor", "scope:c").expect("coord");
    let coord_fill = Coordinate::new("entity:filler", "scope:c").expect("coord");
    let coord_d = Coordinate::new("entity:doomed", "scope:c").expect("coord");
    let coord_anchor = Coordinate::new("entity:anchor", "scope:c").expect("coord");

    // Payload-content predicate hook: `keep` drives keep/drop, so a correct
    // decision PROVES the predicate saw plaintext (ciphertext carries no `keep`).
    let s_receipt = store
        .append(
            &coord_s,
            KIND,
            &serde_json::json!({ "keep": true, "tag": "alpha", "n": 7 }),
        )
        .expect("append survivor");
    let _ = store
        .append(
            &coord_fill,
            KIND,
            &serde_json::json!({ "blob": "x".repeat(2000) }),
        )
        .expect("append filler 1");
    let d_receipt = store
        .append(
            &coord_d,
            KIND,
            &serde_json::json!({ "keep": false, "pii": "drop" }),
        )
        .expect("append doomed");
    let _ = store
        .append(
            &coord_fill,
            KIND,
            &serde_json::json!({ "blob": "y".repeat(2000) }),
        )
        .expect("append filler 2");
    let _ = store
        .append(&coord_anchor, KIND, &serde_json::json!({ "keep": true }))
        .expect("append anchor");

    (dir, store, s_receipt, d_receipt)
}

/// The predicate the compaction tests use: keep iff the DECRYPTED payload says
/// `keep == true`. Reading `keep` at all requires the plaintext.
fn keep_predicate() -> batpak::store::RetentionPredicate {
    Box::new(|stored| {
        stored
            .event
            .payload
            .get("keep")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    })
}

fn assert_encrypted_survivor_intact(
    store: &Store,
    s_receipt: &AppendReceipt,
    pre_ciphertext: &[u8],
) {
    // The survivor still DECRYPTS to its original plaintext after compaction.
    let got = store
        .get(s_receipt.event_id)
        .expect("a kept encrypted survivor must still decrypt via get() after compaction");
    assert_eq!(
        got.event.payload,
        serde_json::json!({ "keep": true, "tag": "alpha", "n": 7 }),
        "PROPERTY: a kept encrypted survivor decrypts byte-faithfully after compaction"
    );

    // event_hash (blake3 over the CIPHERTEXT payload) is UNCHANGED — the write
    // side re-emitted the ciphertext verbatim, it did not re-encrypt.
    let stored_hash = got
        .event
        .hash_chain
        .expect("survivor carries a hash chain")
        .event_hash;
    assert_eq!(
        stored_hash, s_receipt.content_hash,
        "PROPERTY: a survivor's event_hash is byte-stable across compaction (ciphertext verbatim)"
    );

    // The on-disk ciphertext bytes themselves are byte-identical, and the
    // encryption header survives.
    let raw = store
        .read_raw(s_receipt.event_id)
        .expect("read raw survivor");
    assert_eq!(
        raw.event.payload, pre_ciphertext,
        "PROPERTY: the survivor's ciphertext bytes are re-emitted verbatim (byte-identical)"
    );
    assert!(
        raw.event.header.payload_encryption.is_some(),
        "the survivor is still marked encrypted after compaction"
    );
    assert!(
        store.verify_chain().expect("verify_chain").is_intact(),
        "the hash chain stays intact across an encrypted compaction"
    );
}

#[test]
fn retention_predicate_sees_plaintext_and_survivor_is_byte_stable() {
    let (_dir, store, s_receipt, d_receipt) = encrypted_store_with_sealed_survivor_and_doomed();
    let pre_ciphertext = store
        .read_raw(s_receipt.event_id)
        .expect("read survivor ciphertext pre-compaction")
        .event
        .payload;

    let (result, _report) = store
        .compact(&CompactionConfig {
            min_segments: 1,
            strategy: CompactionStrategy::Retention(keep_predicate()),
        })
        .expect("compact encrypted");

    assert!(
        matches!(result.outcome, CompactionOutcome::Performed),
        "precondition: Retention compaction must have PERFORMED a merge (outcome={:?})",
        result.outcome
    );
    // The doomed event's DECRYPTED `keep:false` drove the drop — proof the
    // predicate saw plaintext (ciphertext/placeholder would have dropped S too).
    assert!(
        matches!(store.get(d_receipt.event_id), Err(StoreError::NotFound(_))),
        "precondition: the doomed (keep:false) encrypted event was DROPPED, proving the \
         predicate read its decrypted payload"
    );

    assert_encrypted_survivor_intact(&store, &s_receipt, &pre_ciphertext);
    store.close().expect("close");
}

#[test]
fn tombstone_predicate_sees_plaintext_and_survivor_is_byte_stable() {
    let (_dir, store, s_receipt, d_receipt) = encrypted_store_with_sealed_survivor_and_doomed();
    let pre_ciphertext = store
        .read_raw(s_receipt.event_id)
        .expect("read survivor ciphertext pre-compaction")
        .event
        .payload;

    let (result, _report) = store
        .compact(&CompactionConfig {
            min_segments: 1,
            strategy: CompactionStrategy::Tombstone(keep_predicate()),
        })
        .expect("compact encrypted");

    assert!(
        matches!(result.outcome, CompactionOutcome::Performed),
        "precondition: Tombstone compaction must have PERFORMED a merge (outcome={:?})",
        result.outcome
    );
    // The doomed event's decrypted `keep:false` drove the tombstone rewrite.
    let tombstoned = store
        .query(&Region::entity("entity:doomed").with_fact(KindFilter::Exact(EventKind::TOMBSTONE)));
    assert_eq!(
        tombstoned.len(),
        1,
        "precondition: the doomed (keep:false) encrypted event was tombstoned, proving the \
         predicate read its decrypted payload"
    );
    let _ = d_receipt;

    assert_encrypted_survivor_intact(&store, &s_receipt, &pre_ciphertext);
    store.close().expect("close");
}

#[test]
fn compaction_conservatively_keeps_a_shredded_event() {
    // PerEvent so we can shred exactly one sealed event. A Retention predicate
    // that would DROP a keep:false event must instead KEEP the shredded one
    // (it cannot decide to drop what it cannot read).
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_payload_encryption(KeyScopeGranularity::PerEvent)
        .with_segment_max_bytes(512)
        .with_sync_every_n_events(1);
    let store = Store::open(config).expect("open encrypted store");

    let coord_keep = Coordinate::new("entity:keep", "scope:c").expect("coord");
    let coord_doomed = Coordinate::new("entity:doomed", "scope:c").expect("coord");
    let coord_shred = Coordinate::new("entity:shred", "scope:c").expect("coord");
    let coord_fill = Coordinate::new("entity:filler", "scope:c").expect("coord");
    let coord_anchor = Coordinate::new("entity:anchor", "scope:c").expect("coord");

    let keep = store
        .append(&coord_keep, KIND, &serde_json::json!({ "keep": true }))
        .expect("append keep");
    let doomed = store
        .append(&coord_doomed, KIND, &serde_json::json!({ "keep": false }))
        .expect("append doomed");
    let shred = store
        .append(&coord_shred, KIND, &serde_json::json!({ "keep": false }))
        .expect("append shred");
    // Filler exceeds segment_max_bytes → the next append seals this segment.
    let _ = store
        .append(
            &coord_fill,
            KIND,
            &serde_json::json!({ "blob": "z".repeat(2000) }),
        )
        .expect("append filler");
    let _ = store
        .append(&coord_anchor, KIND, &serde_json::json!({ "keep": true }))
        .expect("append anchor");

    // Crypto-shred the shred event's per-event key BEFORE compaction.
    assert!(
        store
            .shred_scope(ShredScope::Event(shred.event_id))
            .expect("shred"),
        "the shred event's per-event key existed and was destroyed"
    );

    let (result, _report) = store
        .compact(&CompactionConfig {
            min_segments: 1,
            strategy: CompactionStrategy::Retention(keep_predicate()),
        })
        .expect("compact (must not panic on the shredded event)");
    assert!(
        matches!(result.outcome, CompactionOutcome::Performed),
        "precondition: compaction must have PERFORMED a merge (outcome={:?})",
        result.outcome
    );

    // A READABLE keep:false event is dropped...
    assert!(
        matches!(store.get(doomed.event_id), Err(StoreError::NotFound(_))),
        "a readable keep:false event is dropped by the predicate"
    );
    // ...but the SHREDDED keep:false event is CONSERVATIVELY KEPT: still present
    // in the chain, still reading Shredded (the predicate could not evaluate it,
    // so it was never dropped) — never a panic, never a silent erase.
    assert!(
        matches!(
            store
                .get_shreddable(shred.event_id)
                .expect("get_shreddable"),
            ReadDisposition::Shredded
        ),
        "PROPERTY: a shredded event is conservatively KEPT by compaction (present + Shredded)"
    );
    // The keep:true survivor is untouched and still decrypts.
    assert_eq!(
        store
            .get(keep.event_id)
            .expect("keep decrypts")
            .event
            .payload,
        serde_json::json!({ "keep": true })
    );
    assert!(
        store.verify_chain().expect("verify_chain").is_intact(),
        "the chain stays intact after compacting across a shredded event"
    );

    store.close().expect("close");
}
