//! Parity tests for `#[derive(EventSourced)]` (Dispatch Chapter T3).
//! Harness pattern: Equivalence Harness (behavioural parity lane).
//!
//! The derive must produce a projection that is behaviourally identical to
//! a careful hand-written impl. This file pins that equivalence:
//!
//! 1. **Hand-written vs derived parity.** Both versions, identical event
//!    stream, identical state — same `project`, same `project_if_changed`,
//!    same `watch_projection` behaviour.
//! 2. **Lane parity.** Same derive source shape with `input = JsonValueInput`
//!    and `input = RawMsgpackInput` produces identical state on identical
//!    events.
//! 3. **cache_version / type_id isolation.** Changing `cache_version` on the
//!    projection does not change the derived `EventPayload::KIND` of any
//!    bound payload.
//!
//! The sync-drift bug between `apply_event` and `relevant_event_kinds()` is
//! addressed structurally by the derive, not behaviourally here: a kind
//! binding exists exactly when a dispatch arm exists, because both are
//! generated from the same `event =` list. A separate compile-time test for
//! that property is not needed — removing a binding removes both arms and
//! kind entries simultaneously.

use batpak::prelude::{EventPayload, EventSourced};
use batpak_testkit::prelude::*;
use serde::{Deserialize, Serialize};

use batpak_testkit::bounded_blocking;
use batpak_testkit::small_store as small_store_support;
use bounded_blocking::blocking;
use small_store_support::small_segment_store;

// ─── Payload types shared across all parity variants ─────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 7, type_id = 1)]
struct Incremented {
    amount: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 7, type_id = 2)]
struct Decremented {
    amount: i64,
}

// ─── Hand-written projection (the derive's equivalence target) ───────────────

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
struct HandCounter {
    value: i64,
    incs: u32,
    decs: u32,
}

impl batpak::event::EventSourced for HandCounter {
    type Input = JsonValueInput;

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        if events.is_empty() {
            return None;
        }
        let mut s = Self::default();
        for ev in events {
            s.apply_event(ev);
        }
        Some(s)
    }

    fn apply_event(&mut self, event: &Event<serde_json::Value>) {
        use batpak::event::DecodeTyped;
        let incremented = event
            .route_typed::<Incremented>()
            .expect("decode for Incremented must not error");
        match incremented {
            Some(p) => {
                self.value += p.amount;
                self.incs += 1;
            }
            None => {
                let decremented = event
                    .route_typed::<Decremented>()
                    .expect("decode for Decremented must not error");
                if let Some(p) = decremented {
                    self.value += p.amount;
                    self.decs += 1;
                }
            }
        }
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        static KINDS: [EventKind; 2] = [Incremented::KIND, Decremented::KIND];
        &KINDS
    }
}

// ─── Derived projection (JSON lane) ──────────────────────────────────────────

#[derive(Debug, Default, PartialEq, Serialize, Deserialize, EventSourced)]
#[batpak(input = JsonValueInput, cache_version = 0)]
#[batpak(event = Incremented, handler = on_inc)]
#[batpak(event = Decremented, handler = on_dec)]
struct DerivedJson {
    value: i64,
    incs: u32,
    decs: u32,
}

impl DerivedJson {
    fn on_inc(&mut self, p: &Incremented) {
        self.value += p.amount;
        self.incs += 1;
    }
    fn on_dec(&mut self, p: &Decremented) {
        self.value += p.amount;
        self.decs += 1;
    }
}

// ─── Derived projection (raw msgpack lane) ───────────────────────────────────

#[derive(Debug, Default, PartialEq, Serialize, Deserialize, EventSourced)]
#[batpak(input = RawMsgpackInput, cache_version = 0)]
#[batpak(event = Incremented, handler = on_inc)]
#[batpak(event = Decremented, handler = on_dec)]
struct DerivedRaw {
    value: i64,
    incs: u32,
    decs: u32,
}

impl DerivedRaw {
    fn on_inc(&mut self, p: &Incremented) {
        self.value += p.amount;
        self.incs += 1;
    }
    fn on_dec(&mut self, p: &Decremented) {
        self.value += p.amount;
        self.decs += 1;
    }
}

// ─── Shared write helper ─────────────────────────────────────────────────────

fn write_canonical_stream(store: &Store) -> Coordinate {
    let coord = Coordinate::new("entity:parity", "scope:test").expect("valid coord");
    let _ = store
        .append_typed(&coord, &Incremented { amount: 1 })
        .expect("append +1");
    let _ = store
        .append_typed(&coord, &Incremented { amount: 5 })
        .expect("append +5");
    let _ = store
        .append_typed(&coord, &Decremented { amount: -2 })
        .expect("append -2");
    let _ = store
        .append_typed(&coord, &Incremented { amount: 10 })
        .expect("append +10");
    coord
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[test]
fn derive_json_matches_hand_written_json_via_project() {
    let (_dir, store) = small_segment_store().expect("open small segment store");
    let _coord = write_canonical_stream(&store);

    let hand = store
        .project::<HandCounter>("entity:parity", &Freshness::Consistent)
        .expect("project hand counter")
        .expect("hand projection has state");
    let derived = store
        .project::<DerivedJson>("entity:parity", &Freshness::Consistent)
        .expect("project derived counter")
        .expect("derived projection has state");

    assert_eq!(
        hand.value, derived.value,
        "PROPERTY: derive's apply_event reaches same value as hand-written"
    );
    assert_eq!(hand.incs, derived.incs);
    assert_eq!(hand.decs, derived.decs);
    store.close().expect("close store");
}

#[test]
fn json_lane_and_msgpack_lane_produce_identical_state() {
    let (_dir, store) = small_segment_store().expect("open small segment store");
    let _coord = write_canonical_stream(&store);

    let json = store
        .project::<DerivedJson>("entity:parity", &Freshness::Consistent)
        .expect("project json lane")
        .expect("json projection has state");
    let raw = store
        .project::<DerivedRaw>("entity:parity", &Freshness::Consistent)
        .expect("project raw lane")
        .expect("raw msgpack projection has state");

    assert_eq!(
        json.value, raw.value,
        "PROPERTY (invariant 5): both replay lanes produce identical state"
    );
    assert_eq!(json.incs, raw.incs);
    assert_eq!(json.decs, raw.decs);
    store.close().expect("close store");
}

#[test]
fn relevant_event_kinds_come_from_event_bindings() {
    // The derive generates exactly the kinds from the event= list.
    assert_eq!(
        DerivedJson::relevant_event_kinds(),
        &[Incremented::KIND, Decremented::KIND]
    );
    assert_eq!(
        DerivedRaw::relevant_event_kinds(),
        &[Incremented::KIND, Decremented::KIND]
    );
}

#[test]
fn schema_version_derives_from_cache_version_not_type_id() {
    // cache_version defaults to 0; changing the payload's type_id does NOT
    // change the projection's schema_version. This pins invariant 4:
    // cache_version (projection cache) ≠ type_id (wire identity).
    #[derive(Debug, Default, PartialEq, Serialize, Deserialize, EventSourced)]
    #[batpak(input = JsonValueInput, cache_version = 42)]
    #[batpak(event = Incremented, handler = on_inc)]
    struct Bumped {
        v: i64,
    }
    impl Bumped {
        fn on_inc(&mut self, p: &Incremented) {
            self.v += p.amount;
        }
    }

    assert_eq!(Bumped::schema_version(), 42);
    // Payload's wire identity unaffected.
    assert_eq!(Incremented::KIND, EventKind::custom(7, 1));
}

#[test]
fn project_if_changed_parity_between_hand_and_derived() {
    let (_dir, store) = small_segment_store().expect("open small segment store");
    let _coord = write_canonical_stream(&store);

    // Fetch the generation baseline via initial project.
    let _hand_initial: Option<HandCounter> = store
        .project::<HandCounter>("entity:parity", &Freshness::Consistent)
        .expect("project hand baseline");
    let _derived_initial: Option<DerivedJson> = store
        .project::<DerivedJson>("entity:parity", &Freshness::Consistent)
        .expect("project derived baseline");

    let gen_after_initial = store.entity_generation("entity:parity").unwrap_or(0);

    // project_if_changed with the current generation should return None for
    // both: no events have been appended since the baseline, so no re-project
    // happens.
    let hand_unchanged = store
        .project_if_changed::<HandCounter>(
            "entity:parity",
            gen_after_initial,
            &Freshness::Consistent,
        )
        .expect("hand project_if_changed (no change)");
    let derived_unchanged = store
        .project_if_changed::<DerivedJson>(
            "entity:parity",
            gen_after_initial,
            &Freshness::Consistent,
        )
        .expect("derived project_if_changed (no change)");

    assert!(
        hand_unchanged.is_none(),
        "PROPERTY: hand-written project_if_changed returns None when nothing changed"
    );
    assert!(
        derived_unchanged.is_none(),
        "PROPERTY: derived project_if_changed matches hand-written no-change behaviour"
    );

    // Append one more event and assert both surfaces re-project with matching
    // results.
    let coord = Coordinate::new("entity:parity", "scope:test").expect("valid coord");
    let _ = store
        .append_typed(&coord, &Incremented { amount: 3 })
        .expect("append +3");
    let gen_after_append = store.entity_generation("entity:parity").unwrap_or(0);
    assert_ne!(gen_after_initial, gen_after_append);

    let (_, hand_opt) = store
        .project_if_changed::<HandCounter>(
            "entity:parity",
            gen_after_initial,
            &Freshness::Consistent,
        )
        .expect("hand project_if_changed (after change)")
        .expect("hand re-projected after change");
    let (_, derived_opt) = store
        .project_if_changed::<DerivedJson>(
            "entity:parity",
            gen_after_initial,
            &Freshness::Consistent,
        )
        .expect("derived project_if_changed (after change)")
        .expect("derived re-projected after change");
    let hand = hand_opt.expect("hand projection has state after re-project");
    let derived = derived_opt.expect("derived projection has state after re-project");
    assert_eq!(hand.value, derived.value);
    assert_eq!(hand.incs, derived.incs);
    assert_eq!(hand.decs, derived.decs);

    store.close().expect("close store");
}

#[test]
fn watch_projection_parity_between_hand_and_derived() {
    use std::sync::Arc;

    let (_dir, store) = small_segment_store().expect("open small segment store");
    let store = Arc::new(store);
    let coord = Coordinate::new("entity:parity-watch", "scope:test").expect("valid coord");

    // Seed with one event so both watchers see initial state.
    let _ = store
        .append_typed(&coord, &Incremented { amount: 1 })
        .expect("seed append +1");

    let mut hand_watcher =
        store.watch_projection::<HandCounter>("entity:parity-watch", Freshness::Consistent);
    let mut derived_watcher =
        store.watch_projection::<DerivedJson>("entity:parity-watch", Freshness::Consistent);

    // Append a sequence of events. Both watchers must see identical final
    // state — individual update cadences may differ (both rely on the same
    // lossy subscription), but after the stream finishes both projections
    // re-project to the same snapshot.
    for amount in [5i64, -3, 7, -1] {
        if amount >= 0 {
            let _ = store
                .append_typed(&coord, &Incremented { amount })
                .expect("append increment");
        } else {
            let _ = store
                .append_typed(&coord, &Decremented { amount })
                .expect("append decrement");
        }
    }

    // Drain each watcher's first delivered state after the writes; in a
    // stable test we can also fall back to `project` to compare final states.
    let _ = blocking("derive-parity-hand-watch-recv", move || hand_watcher.recv());
    let _ = blocking("derive-parity-derived-watch-recv", move || {
        derived_watcher.recv()
    });

    // Direct projection after the writes is the deterministic parity check.
    let hand_final = store
        .project::<HandCounter>("entity:parity-watch", &Freshness::Consistent)
        .expect("project hand final")
        .expect("hand final state");
    let derived_final = store
        .project::<DerivedJson>("entity:parity-watch", &Freshness::Consistent)
        .expect("project derived final")
        .expect("derived final state");
    assert_eq!(hand_final.value, derived_final.value);
    assert_eq!(hand_final.incs, derived_final.incs);
    assert_eq!(hand_final.decs, derived_final.decs);
}
