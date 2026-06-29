//! Raw projection mode: flow-matrix and maybe-stale cache replay lanes.
//! Harness pattern: Equivalence Harness.
//!
//! PROVES: `project`, `project_if_changed`, and `watch_projection` converge on
//! the same honest `(generation, folded state)` pair across the raw-msgpack and
//! json-value replay lanes, and that cache-enabled `Freshness::MaybeStale`
//! stays honest (generous window may serve cached bytes, zero window forces
//! replay) with both lanes agreeing in each branch.
//! CATCHES: replay-lane drift, watcher/project divergence, generation-only
//! updates masquerading as semantic changes, and stale-cache lane disagreement.
//! SEEDED: deterministic / no randomness.

use std::sync::Arc;

use batpak::store::{Freshness, ProjectionWatcher, Store, StoreConfig};
use batpak_testkit::prelude::*;
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

use batpak_testkit::raw_projection_mode as rpm_support;
use rpm_support::{CounterDelta, KIND};

use batpak_testkit::bounded_blocking;
use bounded_blocking::blocking;

const NOISE_KIND: EventKind = EventKind::custom(0xF, 0x32);

trait MatrixCounterState {
    fn summary(&self) -> (i64, u32);
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ValueCounter {
    value: i64,
    seen: u32,
}

impl EventSourced for ValueCounter {
    type Input = JsonValueInput;
    const STATE_CONTRACT: ProjectionStateContract =
        ProjectionStateContract::single_entity("raw-projection-flow-value-counter");

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        if events.is_empty() {
            return None;
        }
        let mut state = Self::default();
        for event in events {
            state.apply_event(event);
        }
        Some(state)
    }

    fn apply_event(&mut self, event: &Event<serde_json::Value>) {
        let delta = serde_json::from_value::<CounterDelta>(event.payload.clone())
            .expect("ValueCounter::apply_event expects replay payloads that match CounterDelta");
        self.value += delta.amount;
        self.seen += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[KIND]
    }

    fn state_extent(&self) -> StateExtent {
        StateExtent::single_entity()
    }
}

impl MatrixCounterState for ValueCounter {
    fn summary(&self) -> (i64, u32) {
        (self.value, self.seen)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct RawCounter {
    value: i64,
    seen: u32,
}

impl EventSourced for RawCounter {
    type Input = RawMsgpackInput;
    const STATE_CONTRACT: ProjectionStateContract =
        ProjectionStateContract::single_entity("raw-projection-flow-raw-counter");

    fn from_events(events: &[Event<Vec<u8>>]) -> Option<Self> {
        if events.is_empty() {
            return None;
        }
        let mut state = Self::default();
        for event in events {
            state.apply_event(event);
        }
        Some(state)
    }

    fn apply_event(&mut self, event: &Event<Vec<u8>>) {
        let delta = rmp_serde::from_slice::<CounterDelta>(&event.payload)
            .expect("RawCounter::apply_event expects replay payloads that decode as CounterDelta");
        self.value += delta.amount;
        self.seen += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[KIND]
    }

    fn state_extent(&self) -> StateExtent {
        StateExtent::single_entity()
    }
}

impl MatrixCounterState for RawCounter {
    fn summary(&self) -> (i64, u32) {
        (self.value, self.seen)
    }
}

#[derive(Clone, Copy)]
struct ProjectionFlowMatrixCase {
    label: &'static str,
    append_kind: EventKind,
    append_amount: i64,
    expected_after: (i64, u32),
    expect_state_change: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProjectionFlowObservation {
    generation: u64,
    state: (i64, u32),
}

fn seeded_store() -> (TempDir, Arc<Store>) {
    let dir = TempDir::new().expect("temp dir");
    let store = Arc::new(Store::open(StoreConfig::new(dir.path())).expect("open"));
    let coord = Coordinate::new("entity:raw-proj", "scope:test").expect("coord");
    for (amount, label) in [(3, "a"), (-1, "b"), (7, "c"), (2, "d")] {
        let _ = store
            .append(
                &coord,
                KIND,
                &CounterDelta {
                    amount,
                    label: label.to_owned(),
                },
            )
            .expect("append");
    }
    (dir, store)
}

fn cached_seeded_store() -> (TempDir, Arc<Store>) {
    let dir = TempDir::new().expect("temp dir");
    let cache_path = dir.path().join("cache");
    let config = StoreConfig::new(dir.path().join("data"))
        .with_segment_max_bytes(4096)
        .with_sync_every_n_events(1);
    let store = Arc::new(
        Store::open_with_native_cache(config, &cache_path).expect("open with native cache"),
    );
    let coord = Coordinate::new("entity:raw-proj", "scope:test").expect("coord");
    for (amount, label) in [(3, "a"), (-1, "b"), (7, "c"), (2, "d")] {
        let _ = store
            .append(
                &coord,
                KIND,
                &CounterDelta {
                    amount,
                    label: label.to_owned(),
                },
            )
            .expect("append");
    }
    (dir, store)
}

macro_rules! observe_projection_flow_matrix_case {
    ($ty:ty, $case:expr) => {{
        let case = $case;
        let (_dir, store) = seeded_store();
        let baseline_generation = store
            .entity_generation("entity:raw-proj")
            .expect("seeded entity generation");
        let baseline = store
            .project::<$ty>("entity:raw-proj", &Freshness::Consistent)
            .expect("baseline project")
            .expect("baseline projection state");
        let mut watcher: ProjectionWatcher<$ty> =
            Arc::clone(&store).watch_projection::<$ty>("entity:raw-proj", Freshness::Consistent);
        let coord = Coordinate::new("entity:raw-proj", "scope:test").expect("coord");

        let _ = store
            .append(
                &coord,
                case.append_kind,
                &CounterDelta {
                    amount: case.append_amount,
                    label: case.label.to_owned(),
                },
            )
            .expect("append matrix event");

        let (watched_generation, watched_state) =
            blocking("raw-projection-watch-recv", move || watcher.recv())
                .expect("watch projection recv");
        let watched_state = watched_state.expect("watch projection state");
        let changed = store
            .project_if_changed::<$ty>(
                "entity:raw-proj",
                baseline_generation,
                &Freshness::Consistent,
            )
            .expect("project if changed")
            .expect("changed projection");
        let projected = store
            .project::<$ty>("entity:raw-proj", &Freshness::Consistent)
            .expect("full project after append")
            .expect("full projection state");
        let current_generation = store
            .entity_generation("entity:raw-proj")
            .expect("entity generation after append");

        let watched_summary = watched_state.summary();
        let changed_summary = changed.1.expect("changed state").summary();
        let projected_summary = projected.summary();
        let baseline_summary = baseline.summary();

        assert_eq!(
            watched_summary, changed_summary,
            "PROPERTY: watch_projection and project_if_changed must converge on the same folded \
             state for matrix cell '{}'.\n\
             Investigate: src/store/projection/watch.rs recv + src/store/projection/flow.rs.",
            case.label
        );
        assert_eq!(
            watched_summary, projected_summary,
            "PROPERTY: watch_projection and project must converge on the same folded state for \
             matrix cell '{}'.",
            case.label
        );
        assert_eq!(
            watched_generation, changed.0,
            "PROPERTY: watch_projection and project_if_changed must return the same honest \
             generation for matrix cell '{}'.",
            case.label
        );
        assert_eq!(
            watched_generation, current_generation,
            "PROPERTY: matrix cell '{}' must report the entity's latest visible generation.",
            case.label
        );
        assert_eq!(
            watched_summary, case.expected_after,
            "PROPERTY: projection flow matrix cell '{}' must reach the expected folded state.",
            case.label
        );
        assert_eq!(
            watched_summary != baseline_summary,
            case.expect_state_change,
            "PROPERTY: projection flow matrix cell '{}' must truthfully distinguish semantic \
             state changes from generation-only changes.",
            case.label
        );
        let store = Arc::try_unwrap(store).map_err(|_| ()).expect(
            "PROPERTY: projection flow matrix cell should release all Arc clones before close",
        );
        store.close().expect("close matrix store");

        ProjectionFlowObservation {
            generation: watched_generation,
            state: watched_summary,
        }
    }};
}

#[test]
fn projection_flow_matrix_keeps_project_watch_and_project_if_changed_equivalent() {
    // PROVES: the projection flow surfaces (`project`, `project_if_changed`,
    // and `watch_projection`) stay observationally equivalent across both
    // replay lanes, even when entity generation advances without a semantic
    // state change.
    let cases = [
        ProjectionFlowMatrixCase {
            label: "relevant-append",
            append_kind: KIND,
            append_amount: 4,
            expected_after: (15, 5),
            expect_state_change: true,
        },
        ProjectionFlowMatrixCase {
            label: "irrelevant-append",
            append_kind: NOISE_KIND,
            append_amount: 999,
            expected_after: (11, 4),
            expect_state_change: false,
        },
    ];

    for case in cases {
        let value = observe_projection_flow_matrix_case!(ValueCounter, case);
        let raw = observe_projection_flow_matrix_case!(RawCounter, case);

        assert_eq!(
            raw, value,
            "PROPERTY: raw-msgpack and json-value replay lanes must agree on the same honest \
             (generation, folded state) pair for matrix cell '{}'.\n\
             Investigate: src/store/projection/flow.rs ReplayInput dispatch.",
            case.label
        );
    }
}

#[test]
fn projection_flow_maybe_stale_keeps_replay_lanes_equivalent() {
    // PROVES: cache-enabled `Freshness::MaybeStale` stays honest across both
    // replay lanes: a generous stale window may serve cached bytes, but a
    // zero window must force replay, and raw/value lanes must agree in both
    // branches.
    let (_dir, store) = cached_seeded_store();
    let baseline_value = store
        .project::<ValueCounter>("entity:raw-proj", &Freshness::Consistent)
        .expect("seed value cache")
        .expect("baseline value state");
    let baseline_raw = store
        .project::<RawCounter>("entity:raw-proj", &Freshness::Consistent)
        .expect("seed raw cache")
        .expect("baseline raw state");
    assert_eq!(
        baseline_value.summary(),
        baseline_raw.summary(),
        "baseline cache warmup must agree across replay lanes"
    );

    let coord = Coordinate::new("entity:raw-proj", "scope:test").expect("coord");
    let _ = store
        .append(
            &coord,
            KIND,
            &CounterDelta {
                amount: 4,
                label: "maybe-stale".to_owned(),
            },
        )
        .expect("append relevant maybe stale event");

    let value_stale = store
        .project::<ValueCounter>(
            "entity:raw-proj",
            &Freshness::MaybeStale {
                max_stale_ms: 60_000,
            },
        )
        .expect("value maybe stale")
        .expect("value stale state");
    let raw_stale = store
        .project::<RawCounter>(
            "entity:raw-proj",
            &Freshness::MaybeStale {
                max_stale_ms: 60_000,
            },
        )
        .expect("raw maybe stale")
        .expect("raw stale state");
    assert_eq!(
        value_stale.summary(),
        baseline_value.summary(),
        "PROPERTY: MaybeStale with a generous window may serve the previously cached folded state."
    );
    assert_eq!(
        raw_stale.summary(),
        baseline_raw.summary(),
        "PROPERTY: raw replay lane must expose the same stale cached state as the value lane."
    );
    assert_eq!(
        value_stale.summary(),
        raw_stale.summary(),
        "PROPERTY: raw and value replay lanes must agree on the stale cache-hit branch."
    );

    let value_strict = store
        .project::<ValueCounter>(
            "entity:raw-proj",
            &Freshness::MaybeStale { max_stale_ms: 0 },
        )
        .expect("value strict maybe stale")
        .expect("value strict state");
    let raw_strict = store
        .project::<RawCounter>(
            "entity:raw-proj",
            &Freshness::MaybeStale { max_stale_ms: 0 },
        )
        .expect("raw strict maybe stale")
        .expect("raw strict state");
    let value_consistent = store
        .project::<ValueCounter>("entity:raw-proj", &Freshness::Consistent)
        .expect("value consistent")
        .expect("value consistent state");
    let raw_consistent = store
        .project::<RawCounter>("entity:raw-proj", &Freshness::Consistent)
        .expect("raw consistent")
        .expect("raw consistent state");

    assert_eq!(
        value_strict.summary(),
        value_consistent.summary(),
        "PROPERTY: MaybeStale with max_stale_ms=0 must force replay on the value lane."
    );
    assert_eq!(
        raw_strict.summary(),
        raw_consistent.summary(),
        "PROPERTY: MaybeStale with max_stale_ms=0 must force replay on the raw lane."
    );
    assert_eq!(
        value_strict.summary(),
        raw_strict.summary(),
        "PROPERTY: raw and value replay lanes must agree on the strict replay branch."
    );

    let store = Arc::try_unwrap(store)
        .map_err(|_| ())
        .expect("PROPERTY: maybe-stale matrix must release all Arc clones before close");
    store.close().expect("close maybe stale matrix store");
}
