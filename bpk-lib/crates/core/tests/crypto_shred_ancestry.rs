//! Stage E3 coverage for crypto-shred KEY-AWARE ANCESTRY.
//!
//! Stage C made an encrypted event's on-disk payload ciphertext and the
//! Value-decode read seam refuse to decode it. Stages E1/E2 made single-event
//! read, projection, compaction, and live delivery key-aware. E3 closes the last
//! residual read consumer: hash-chain ANCESTRY. Before E3
//! [`Store::walk_ancestors`] decoded each ancestor through the non-key-aware
//! reader, so the first ENCRYPTED ancestor failed to Value-decode and the walk
//! truncated (a false `ReadFailure`/`MissingParent`), misreporting an intact
//! encrypted chain as broken. These tests prove the E3 contract:
//!
//!   * A walk over an ENCRYPTED entity returns the FULL lineage with DECRYPTED
//!     payloads, and an intact encrypted chain reports
//!     [`AncestryBoundary::ReachedGenesis`] (never a spurious truncation).
//!   * A crypto-shredded ancestor still EXISTS in the chain (its hash links are
//!     intact), so the walk INCLUDES it — flagged via
//!     [`AncestorWalk::is_shredded`] / [`AncestorWalk::shredded_ancestors`], with
//!     a `Value::Null` placeholder payload — and CONTINUES past it to genesis. It
//!     is NEVER a false [`AncestryBoundary::MissingParent`].
//!
//! The parent-edge linkage the walk follows is over the hash chain
//! (`prev_hash` → `event_hash`) and is UNAFFECTED by encryption, so only the
//! returned payload decode changes; `verify_chain` stays intact throughout.
//!
//! Gated behind `payload-encryption` (the whole file compiles out of a default
//! build; the plaintext, no-keyset ancestry path is covered byte-identically by
//! the ungated `store_ancestors` / `store_ancestors_retention_coherence` suites).
//!
//! INVARIANTS: INV-CRYPTO-SHRED-SCOPE-DESTROYS-PLAINTEXT — a crypto-shredded
//! ancestor is surfaced as an intact-but-erased chain link (marked, never the
//! ciphertext, never a false chain break); a readable encrypted ancestor is
//! returned decrypted.
#![cfg(feature = "payload-encryption")]

use batpak::coordinate::Coordinate;
use batpak::event::EventKind;
use batpak::id::{EntityIdType, EventId};
use batpak::store::{
    AncestorWalk, AncestryBoundary, KeyScopeGranularity, ShredScope, Store, StoreConfig,
};

const KIND: EventKind = EventKind::custom(0xF, 1);

fn open_encrypted(dir: &std::path::Path, granularity: KeyScopeGranularity) -> Store {
    Store::open(StoreConfig::new(dir).with_payload_encryption(granularity))
        .expect("open encrypted store")
}

/// Append `steps` encrypted events to one entity/coordinate, returning their
/// event ids in append order (genesis first). All share the same hash chain.
fn seed_encrypted_chain(store: &Store, coord: &Coordinate, steps: usize) -> Vec<EventId> {
    (0..steps)
        .map(|step| {
            store
                .append(
                    coord,
                    KIND,
                    &serde_json::json!({ "secret": "pii", "step": step }),
                )
                .expect("encrypted append")
                .event_id
        })
        .collect()
}

#[test]
fn walk_ancestors_over_encrypted_entity_returns_full_decrypted_lineage() {
    // Before E3 the walk truncated at the FIRST encrypted ancestor (ciphertext
    // failed the Value decode). Now the whole lineage comes back DECRYPTED and an
    // intact encrypted chain reports ReachedGenesis with an EMPTY shredded set —
    // identical to a plaintext chain.
    let dir = tempfile::tempdir().expect("tmpdir");
    let store = open_encrypted(dir.path(), KeyScopeGranularity::PerEntity);
    let coord = Coordinate::new("entity:enc-lineage", "scope:e3").expect("coord");

    let ids = seed_encrypted_chain(&store, &coord, 4);
    let anchor = *ids.last().expect("anchor");

    // The full walk returns every ancestor, newest-first, with DECRYPTED payloads.
    let ancestors = store.walk_ancestors(anchor, 16);
    let walked_ids: Vec<EventId> = ancestors.iter().map(|s| s.event.event_id()).collect();
    let expected_ids: Vec<EventId> = ids.iter().rev().copied().collect();
    assert_eq!(
        walked_ids, expected_ids,
        "PROPERTY: the walk over an encrypted entity must return the FULL chain in reverse \
         append order (it used to truncate at the first encrypted ancestor)"
    );
    for (stored, step) in ancestors.iter().zip((0..4).rev()) {
        assert_eq!(
            stored.event.payload,
            serde_json::json!({ "secret": "pii", "step": step }),
            "PROPERTY: each ancestor payload must come back DECRYPTED to its original plaintext"
        );
    }

    // The boundary surface: an intact encrypted chain reached genesis, with no
    // shredded ancestors — the SAME shape as a plaintext intact chain.
    let walk: AncestorWalk = store.walk_ancestors_outcome(anchor, 16);
    assert_eq!(
        walk.boundary,
        AncestryBoundary::ReachedGenesis,
        "PROPERTY: an intact encrypted chain must report ReachedGenesis, not a spurious truncation"
    );
    assert!(
        walk.reached_genesis() && walk.truncated_at().is_none(),
        "PROPERTY: an intact encrypted chain is complete (no truncation point)"
    );
    assert!(
        walk.shredded_ancestors().is_empty() && walk.shredded.is_empty(),
        "PROPERTY: no ancestor was shredded, so the shredded set is empty"
    );

    // The crypto-shred payoff is orthogonal to ancestry: the chain still verifies
    // over the stored ciphertext.
    assert!(
        store.verify_chain().expect("verify_chain").is_intact(),
        "verify_chain stays intact across the encrypted lineage"
    );
}

#[test]
fn shredded_mid_chain_ancestor_is_marked_and_walk_continues_to_genesis() {
    // PerEvent keying gives each event its OWN key, so a SINGLE mid-chain event can
    // be crypto-shredded while its parent and child stay readable. The walk must
    // INCLUDE the shredded ancestor (marked, Null placeholder) and CONTINUE through
    // it to genesis — NOT truncate at it as a false MissingParent.
    let dir = tempfile::tempdir().expect("tmpdir");
    let store = open_encrypted(dir.path(), KeyScopeGranularity::PerEvent);
    let coord = Coordinate::new("entity:enc-shred-mid", "scope:e3").expect("coord");

    let ids = seed_encrypted_chain(&store, &coord, 4);
    let genesis = ids[0];
    let shred_target = ids[2]; // mid-chain
    let anchor = ids[3];

    // Crypto-shred ONLY the mid-chain event's key (PerEvent → Event selector).
    assert!(
        store
            .shred_scope(ShredScope::Event(shred_target))
            .expect("shred mid-chain event"),
        "a live key existed for the mid-chain event and was destroyed"
    );

    let walk = store.walk_ancestors_outcome(anchor, 16);

    // The chain STRUCTURE is complete: every event is present, newest-first, and
    // the walk reached genesis — the shredded ancestor did NOT break the chain.
    let walked_ids: Vec<EventId> = walk.ancestors.iter().map(|s| s.event.event_id()).collect();
    assert_eq!(
        walked_ids,
        ids.iter().rev().copied().collect::<Vec<_>>(),
        "PROPERTY: the walk must include ALL ancestors, including the shredded one, in reverse order"
    );
    assert_eq!(
        walk.boundary,
        AncestryBoundary::ReachedGenesis,
        "PROPERTY: a shredded mid-chain ancestor must NOT truncate the walk — it reaches genesis"
    );
    assert_ne!(
        walk.boundary,
        AncestryBoundary::MissingParent {
            child: shred_target
        },
        "PROPERTY: a shredded ancestor is present in the chain — it is NEVER a false MissingParent"
    );
    assert!(
        walk.truncated_at().is_none(),
        "PROPERTY: a shredded ancestor does not create a truncation point"
    );

    // The shredded ancestor is FLAGGED (only it) and carries a Null placeholder.
    assert_eq!(
        walk.shredded_ancestors(),
        &[shred_target],
        "PROPERTY: exactly the shredded event is flagged in the shredded set"
    );
    assert!(
        walk.is_shredded(shred_target),
        "PROPERTY: is_shredded reports true for the erased ancestor"
    );
    for readable in [genesis, ids[1], anchor] {
        assert!(
            !walk.is_shredded(readable),
            "PROPERTY: a readable ancestor must NOT be flagged shredded"
        );
    }

    // Payload disposition: the shredded ancestor is a Null placeholder; its
    // neighbours decrypt to their original plaintext.
    for stored in &walk.ancestors {
        let id = stored.event.event_id();
        if id == shred_target {
            assert_eq!(
                stored.event.payload,
                serde_json::Value::Null,
                "PROPERTY: a shredded ancestor carries a Null placeholder payload (the plaintext is gone)"
            );
        } else {
            assert_ne!(
                stored.event.payload,
                serde_json::Value::Null,
                "PROPERTY: a readable ancestor returns its DECRYPTED payload, not the placeholder"
            );
            assert_eq!(
                stored.event.payload["secret"],
                serde_json::json!("pii"),
                "PROPERTY: a readable ancestor decrypts to its original plaintext"
            );
        }
    }

    // The crypto-shred payoff: the hash chain is STILL intact — only the plaintext
    // of the one erased ancestor is gone.
    assert!(
        store.verify_chain().expect("verify_chain").is_intact(),
        "verify_chain stays intact after a mid-chain crypto-shred"
    );
}

#[test]
fn fully_shredded_encrypted_chain_still_reaches_genesis_all_marked() {
    // The extreme case: shred the WHOLE entity (PerEntity → one key covers every
    // payload). Every ancestor becomes unrecoverable, yet the walk must STILL
    // return the complete chain to genesis with every ancestor marked shredded —
    // an intact chain of intact-but-erased links, never a chain break.
    let dir = tempfile::tempdir().expect("tmpdir");
    let store = open_encrypted(dir.path(), KeyScopeGranularity::PerEntity);
    let coord = Coordinate::new("entity:enc-shred-all", "scope:e3").expect("coord");

    let ids = seed_encrypted_chain(&store, &coord, 3);
    let anchor = *ids.last().expect("anchor");

    assert!(
        store
            .shred_scope(ShredScope::Entity(&coord))
            .expect("shred entity"),
        "the entity's key existed and was destroyed"
    );

    let walk = store.walk_ancestors_outcome(anchor, 16);
    assert_eq!(
        walk.boundary,
        AncestryBoundary::ReachedGenesis,
        "PROPERTY: a fully-shredded chain still reaches genesis — erasure is not a chain break"
    );
    let walked_ids: Vec<EventId> = walk.ancestors.iter().map(|s| s.event.event_id()).collect();
    assert_eq!(
        walked_ids,
        ids.iter().rev().copied().collect::<Vec<_>>(),
        "PROPERTY: every ancestor is present despite every payload being erased"
    );
    let mut shredded_sorted: Vec<EventId> = walk.shredded_ancestors().to_vec();
    shredded_sorted.sort_by_key(EventId::as_u128);
    let mut expected_sorted = ids.clone();
    expected_sorted.sort_by_key(EventId::as_u128);
    assert_eq!(
        shredded_sorted, expected_sorted,
        "PROPERTY: every ancestor is flagged shredded when the whole chain's key is destroyed"
    );
    for stored in &walk.ancestors {
        assert!(
            walk.is_shredded(stored.event.event_id()),
            "PROPERTY: each returned ancestor is flagged shredded"
        );
        assert_eq!(
            stored.event.payload,
            serde_json::Value::Null,
            "PROPERTY: each shredded ancestor carries the Null placeholder"
        );
    }

    // Even with every payload gone, the hash chain still verifies over ciphertext.
    assert!(
        store.verify_chain().expect("verify_chain").is_intact(),
        "verify_chain stays intact after a full-entity crypto-shred"
    );
}
