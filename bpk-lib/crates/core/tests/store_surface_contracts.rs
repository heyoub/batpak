// justifies: INV-TEST-PANIC-AS-ASSERTION, INV-MACRO-BOUNDED-CAST; store surface contract tests rely on unwrap/panic as assertion style and intentionally bounded fixture data.
#![allow(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    clippy::cast_possible_truncation,
    clippy::needless_borrows_for_generic_args,
    clippy::panic
)]
//! Store surface contract tests extracted from `store_advanced.rs`.
//!
//! PROVES: display helpers, coordinate formatting, causation helpers, and
//! append-option flag round-trips remain stable on the public surface.
//! DEFENDS: user-visible message drift, coordinate formatting regressions,
//! causation helper regressions, and append flag propagation loss.

use batpak::prelude::*;

#[path = "support/small_store.rs"]
mod small_store_support;

fn test_store() -> (Store, tempfile::TempDir) {
    small_store_support::small_segment_store().expect("small segment store")
}

#[test]
fn store_error_display_variants() {
    use batpak::store::StoreError;

    let not_found = format!("{}", StoreError::NotFound(0xDEAD));
    assert!(
        not_found.contains("dead"),
        "PROPERTY: StoreError::NotFound Display must include the event ID in hex (e.g. 'dead').\n\
         Investigate: src/store/mod.rs StoreError Display impl.\n\
         Common causes: Display arm for NotFound omits the id, uses decimal instead of hex.\n\
         Run: cargo test --test store_surface_contracts store_error_display_variants"
    );

    let writer = format!("{}", StoreError::WriterCrashed);
    assert!(
        writer.contains("writer") || writer.contains("crash"),
        "PROPERTY: StoreError::WriterCrashed Display must mention 'writer' or 'crash'.\n\
         Investigate: src/store/mod.rs StoreError Display impl.\n\
         Common causes: Display arm returns generic message without variant-specific text.\n\
         Run: cargo test --test store_surface_contracts store_error_display_variants"
    );

    let cache = format!("{}", StoreError::CacheFailed("redis timeout".into()));
    assert!(
        cache.contains("redis timeout"),
        "PROPERTY: StoreError::CacheFailed Display must include the inner error message.\n\
         Investigate: src/store/mod.rs StoreError Display impl.\n\
         Common causes: inner string not interpolated, Display arm discards the inner field.\n\
         Run: cargo test --test store_surface_contracts store_error_display_variants"
    );

    let seq = format!(
        "{}",
        StoreError::SequenceMismatch {
            entity: "user:1".into(),
            expected: 5,
            actual: 3
        }
    );
    assert!(
        seq.contains("user:1") && seq.contains("5") && seq.contains("3"),
        "PROPERTY: StoreError::SequenceMismatch Display must include entity, expected, and actual values.\n\
         Investigate: src/store/mod.rs StoreError Display impl.\n\
         Common causes: Display arm omits one or more struct fields, entity string not interpolated.\n\
         Run: cargo test --test store_surface_contracts store_error_display_variants"
    );

    let crc = format!(
        "{}",
        StoreError::CrcMismatch {
            segment_id: 7,
            offset: 42
        }
    );
    assert!(
        crc.contains("7") && crc.contains("42"),
        "PROPERTY: StoreError::CrcMismatch Display must include segment_id (7) and offset (42).\n\
         Investigate: src/store/mod.rs StoreError Display impl.\n\
         Common causes: Display arm for CrcMismatch omits numeric fields, formats only one field.\n\
         Run: cargo test --test store_surface_contracts store_error_display_variants"
    );

    let corrupt = format!(
        "{}",
        StoreError::CorruptSegment {
            segment_id: 3,
            detail: "bad magic".into()
        }
    );
    assert!(
        corrupt.contains("bad magic"),
        "PROPERTY: StoreError::CorruptSegment Display must include the detail string.\n\
         Investigate: src/store/mod.rs StoreError Display impl.\n\
         Common causes: detail field not interpolated into Display output, generic message used.\n\
         Run: cargo test --test store_surface_contracts store_error_display_variants"
    );

    let ser = format!("{}", StoreError::Serialization("unexpected EOF".into()));
    assert!(
        ser.contains("unexpected EOF"),
        "PROPERTY: StoreError::Serialization Display must include the inner error message.\n\
         Investigate: src/store/mod.rs StoreError Display impl.\n\
         Common causes: inner string not forwarded to Display output, variant uses static text only.\n\
         Run: cargo test --test store_surface_contracts store_error_display_variants"
    );
}

#[test]
fn coordinate_error_display() {
    let err =
        Coordinate::new("", "scope").expect_err("empty entity should produce CoordinateError");
    let msg = format!("{err}");
    assert!(
        msg.contains("entity"),
        "PROPERTY: CoordinateError for an empty entity string must mention 'entity' in its Display.\n\
         Investigate: src/store/mod.rs CoordinateError Display impl.\n\
         Common causes: EmptyEntity variant Display returns generic string without the word 'entity'.\n\
         Run: cargo test --test store_surface_contracts coordinate_error_display"
    );

    let err =
        Coordinate::new("entity", "").expect_err("empty scope should produce CoordinateError");
    let msg = format!("{err}");
    assert!(
        msg.contains("scope"),
        "PROPERTY: CoordinateError for an empty scope string must mention 'scope' in its Display.\n\
         Investigate: src/store/mod.rs CoordinateError Display impl.\n\
         Common causes: EmptyScope variant Display returns generic string without the word 'scope'.\n\
         Run: cargo test --test store_surface_contracts coordinate_error_display"
    );
}

#[test]
fn coordinate_display_format() {
    let coord = Coordinate::new("entity:42", "scope:alpha").expect("valid");
    let display = format!("{coord}");
    assert_eq!(
        display, "entity:42@scope:alpha",
        "PROPERTY: Coordinate Display must format as 'entity@scope' (e.g. 'entity:42@scope:alpha').\n\
         Investigate: src/store/mod.rs Coordinate Display impl.\n\
         Common causes: separator wrong (e.g. '/' or ':' instead of '@'), fields swapped.\n\
         Run: cargo test --test store_surface_contracts coordinate_display_format"
    );
}

#[test]
fn index_entry_causation_helpers() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:helpers", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let root = store
        .append(&coord, kind, &serde_json::json!({"cmd": "create"}))
        .expect("root");

    let reaction = store
        .append_reaction(
            &coord,
            kind,
            &serde_json::json!({"evt": "created"}),
            root.event_id,
            root.event_id,
        )
        .expect("reaction");

    let entries = store.stream("entity:helpers");
    assert_eq!(
        entries.len(),
        2,
        "PROPERTY: stream must return exactly 2 events (root + reaction) for entity:helpers.\n\
         Investigate: src/store/mod.rs stream, src/store/index/mod.rs entity lookup.\n\
         Common causes: reaction event stored under wrong entity key, stream skips reaction frames.\n\
         Run: cargo test --test store_surface_contracts index_entry_causation_helpers"
    );

    let root_entry = entries
        .iter()
        .find(|e| e.event_id == root.event_id)
        .expect("find root");
    let root_is_root_cause = root_entry.is_root_cause();
    let root_is_correlated = root_entry.is_correlated();
    assert!(
        root_is_root_cause,
        "PROPERTY: an event with no explicit causation must be identified as a root cause.\n\
         Investigate: src/store/mod.rs IndexEntry::is_root_cause.\n\
         Common causes: is_root_cause checks wrong field, causation_id default value incorrect.\n\
         Run: cargo test --test store_surface_contracts index_entry_causation_helpers"
    );
    assert!(
        !root_is_correlated,
        "PROPERTY: a self-correlated event (correlation_id == event_id) must not be 'correlated'.\n\
         Investigate: src/store/mod.rs IndexEntry::is_correlated.\n\
         Common causes: is_correlated returns true for self-correlation, field comparison inverted.\n\
         Run: cargo test --test store_surface_contracts index_entry_causation_helpers"
    );

    let react_entry = entries
        .iter()
        .find(|e| e.event_id == reaction.event_id)
        .expect("find reaction");
    let reaction_is_root_cause = react_entry.is_root_cause();
    let reaction_is_correlated = react_entry.is_correlated();
    let reaction_is_caused_by_root = react_entry.is_caused_by(root.event_id);
    let reaction_is_caused_by_unrelated = react_entry.is_caused_by(root.event_id.wrapping_add(1));
    assert!(
        !reaction_is_root_cause,
        "PROPERTY: a reaction event with an explicit cause must not be identified as a root cause.\n\
         Investigate: src/store/mod.rs IndexEntry::is_root_cause.\n\
         Common causes: is_root_cause ignores causation_id field, always returns true.\n\
         Run: cargo test --test store_surface_contracts index_entry_causation_helpers"
    );
    assert!(
        reaction_is_correlated,
        "PROPERTY: a reaction event with a correlation_id different from its own event_id must be 'correlated'.\n\
         Investigate: src/store/mod.rs IndexEntry::is_correlated.\n\
         Common causes: correlation_id not set on reaction frame, is_correlated comparison wrong.\n\
         Run: cargo test --test store_surface_contracts index_entry_causation_helpers"
    );
    assert!(
        reaction_is_caused_by_root,
        "PROPERTY: a reaction event must report is_caused_by(root.event_id) == true.\n\
         Investigate: src/store/mod.rs IndexEntry::is_caused_by.\n\
         Common causes: causation_id not stored in reaction frame, is_caused_by checks wrong field.\n\
         Run: cargo test --test store_surface_contracts index_entry_causation_helpers"
    );
    assert!(
        !reaction_is_caused_by_unrelated,
        "PROPERTY: is_caused_by must be exact, not a broad 'has any cause' predicate.\n\
         Investigate: src/store/index/mod.rs IndexEntry::is_caused_by.\n\
         Common causes: is_caused_by always returns true for caused events.\n\
         Run: cargo test --test store_surface_contracts index_entry_causation_helpers"
    );

    store.close().expect("close");
}

#[test]
fn append_with_flags_round_trips() {
    use batpak::event::header::{FLAG_REPLAY, FLAG_REQUIRES_ACK, FLAG_TRANSACTIONAL};

    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:flags", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let flags = FLAG_REQUIRES_ACK | FLAG_TRANSACTIONAL | FLAG_REPLAY;

    let opts = AppendOptions {
        flags,
        ..Default::default()
    };
    let receipt = store
        .append_with_options(&coord, kind, &serde_json::json!({"flagged": true}), opts)
        .expect("append with flags");

    let stored = store.get(receipt.event_id).expect("get");
    assert_eq!(
        stored.event.header.flags, flags,
        "PROPERTY: flags set via AppendOptions must round-trip through append→get.\n\
         Investigate: src/store/mod.rs append_with_options, src/store/write/writer.rs handle_append.\n\
         Common causes: flags not propagated from AppendOptions to EventHeader, writer overwrites flags.\n\
         Run: cargo test --test store_surface_contracts append_with_flags_round_trips"
    );
    assert!(
        stored.event.header.requires_ack(),
        "PROPERTY: FLAG_REQUIRES_ACK must be readable via requires_ack() accessor.\n\
         Investigate: src/event/header.rs requires_ack.\n\
         Run: cargo test --test store_surface_contracts append_with_flags_round_trips"
    );
    assert!(
        stored.event.header.is_transactional(),
        "PROPERTY: FLAG_TRANSACTIONAL must be readable via is_transactional() accessor.\n\
         Investigate: src/event/header.rs is_transactional.\n\
         Run: cargo test --test store_surface_contracts append_with_flags_round_trips"
    );
    assert!(
        stored.event.header.is_replay(),
        "PROPERTY: FLAG_REPLAY must be readable via is_replay() accessor.\n\
         Investigate: src/event/header.rs is_replay.\n\
         Run: cargo test --test store_surface_contracts append_with_flags_round_trips"
    );

    store.close().expect("close");
}
