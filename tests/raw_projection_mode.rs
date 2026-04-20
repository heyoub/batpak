// justifies: INV-TEST-PANIC-AS-ASSERTION; raw projection mode tests in tests/raw_projection_mode.rs use panic! as the assertion style when the raw-dispatch contract breaks.
#![allow(clippy::panic)]
//! Raw projection mode parity and flow-matrix tests.
//! Harness pattern: Equivalence Harness.
//!
//! PROVES: `project`, `project_if_changed`, and `watch_projection` converge on
//! the same honest `(generation, folded state)` pair across both replay lanes.
//! CATCHES: replay-lane drift, watcher/project divergence, and generation-only
//! updates that masquerade as semantic state changes.
//! SEEDED: deterministic / no randomness.

use std::sync::Arc;

use batpak::prelude::*;
use batpak::store::{Freshness, ProjectionWatcher, Store, StoreConfig, SyncConfig};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

const KIND: EventKind = EventKind::custom(0xF, 0x31);
const NOISE_KIND: EventKind = EventKind::custom(0xF, 0x32);

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CounterDelta {
    amount: i64,
    label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ValueCounter {
    value: i64,
    seen: u32,
}

impl EventSourced for ValueCounter {
    type Input = JsonValueInput;

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
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct RawCounter {
    value: i64,
    seen: u32,
}

trait MatrixCounterState {
    fn summary(&self) -> (i64, u32);
}

impl MatrixCounterState for ValueCounter {
    fn summary(&self) -> (i64, u32) {
        (self.value, self.seen)
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

impl EventSourced for RawCounter {
    type Input = RawMsgpackInput;

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
}

#[test]
fn projection_input_modes_select_expected_replay_lanes() {
    let raw_mode = <RawMsgpackInput as ProjectionInput>::MODE;
    let json_mode = <JsonValueInput as ProjectionInput>::MODE;
    let expected_raw = ReplayLane::RawMsgpack;
    let expected_json = ReplayLane::Value;
    assert_eq!(raw_mode, expected_raw);
    assert_eq!(json_mode, expected_json);
}

// ProjectionPayload and ProjectionEvent resolve to the correct concrete types.
fn _assert_projection_type_aliases() {
    fn _is_vec_u8(_: ProjectionPayload<RawCounter>) {}
    fn _is_value(_: ProjectionPayload<ValueCounter>) {}
    fn _is_raw_event(_: ProjectionEvent<RawCounter>) {}
    fn _is_value_event(_: ProjectionEvent<ValueCounter>) {}
}

fn seeded_store() -> (Arc<Store>, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let store = Arc::new(Store::open(StoreConfig::new(dir.path())).expect("open"));
    let coord = Coordinate::new("entity:raw-proj", "scope:test").expect("coord");
    for (amount, label) in [(3, "a"), (-1, "b"), (7, "c"), (2, "d")] {
        store
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
    (store, dir)
}

fn cached_seeded_store() -> (Arc<Store>, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let cache_path = dir.path().join("cache");
    let config = StoreConfig {
        data_dir: dir.path().join("data"),
        segment_max_bytes: 4096,
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        ..StoreConfig::new("")
    };
    let store = Arc::new(
        Store::open_with_native_cache(config, &cache_path).expect("open with native cache"),
    );
    let coord = Coordinate::new("entity:raw-proj", "scope:test").expect("coord");
    for (amount, label) in [(3, "a"), (-1, "b"), (7, "c"), (2, "d")] {
        store
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
    (store, dir)
}

macro_rules! observe_projection_flow_matrix_case {
    ($ty:ty, $case:expr) => {{
        let case = $case;
        let (store, _dir) = seeded_store();
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

        store
            .append(
                &coord,
                case.append_kind,
                &CounterDelta {
                    amount: case.append_amount,
                    label: case.label.to_owned(),
                },
            )
            .expect("append matrix event");

        let (watched_generation, watched_state) = watcher.recv().expect("watch projection recv");
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

        drop(watcher);
        let store = match Arc::try_unwrap(store) {
            Ok(store) => store,
            Err(_) => panic!(
                "PROPERTY: projection flow matrix cell '{}' should release all Arc clones before close",
                case.label
            ),
        };
        store.close().expect("close matrix store");

        ProjectionFlowObservation {
            generation: watched_generation,
            state: watched_summary,
        }
    }};
}

#[test]
fn raw_projection_matches_value_projection_live_and_reopen() {
    let (store, dir) = seeded_store();

    let value_live: Option<ValueCounter> = store
        .project("entity:raw-proj", &Freshness::Consistent)
        .expect("value project");
    let raw_live: Option<RawCounter> = store
        .project("entity:raw-proj", &Freshness::Consistent)
        .expect("raw project");
    assert_eq!(
        raw_live.as_ref().map(|state| (state.value, state.seen)),
        value_live.as_ref().map(|state| (state.value, state.seen)),
        "raw-mode and value-mode projections must converge on the same state live"
    );

    let store = match Arc::try_unwrap(store) {
        Ok(store) => store,
        Err(_) => {
            panic!("PROPERTY: raw projection test should release all Arc clones before close")
        }
    };
    store.close().expect("close");

    let reopened = Store::open(StoreConfig::new(dir.path())).expect("reopen");
    let value_reopen: Option<ValueCounter> = reopened
        .project("entity:raw-proj", &Freshness::Consistent)
        .expect("value project after reopen");
    let raw_reopen: Option<RawCounter> = reopened
        .project("entity:raw-proj", &Freshness::Consistent)
        .expect("raw project after reopen");
    assert_eq!(
        raw_reopen.as_ref().map(|state| (state.value, state.seen)),
        value_reopen.as_ref().map(|state| (state.value, state.seen)),
        "raw-mode and value-mode projections must converge after cold start too"
    );
    reopened.close().expect("close reopened");
}

#[test]
fn raw_watch_projection_emits_updated_state() {
    let (store, _dir) = seeded_store();
    let baseline_generation = store
        .entity_generation("entity:raw-proj")
        .expect("seeded entity generation");
    let mut watcher: ProjectionWatcher<RawCounter> =
        Arc::clone(&store).watch_projection::<RawCounter>("entity:raw-proj", Freshness::Consistent);
    let subscription = watcher.subscription();
    let subscription_rx = subscription.filtered_receiver();
    assert!(
        subscription_rx.is_empty(),
        "fresh projection watcher subscription should not have buffered notifications before a write"
    );
    let coord = Coordinate::new("entity:raw-proj", "scope:test").expect("coord");

    store
        .append(
            &coord,
            KIND,
            &CounterDelta {
                amount: 5,
                label: "watch".to_owned(),
            },
        )
        .expect("append watch event");

    let (gen, update) = watcher.recv().expect("watch projection recv");
    let update = update.expect("watch projection state");
    assert_eq!(update.value, 16);
    assert_eq!(update.seen, 5);
    assert!(
        gen > baseline_generation,
        "watch projection generation should advance after a relevant append"
    );

    drop(watcher);
    let store = match Arc::try_unwrap(store) {
        Ok(store) => store,
        Err(_) => panic!("PROPERTY: raw watch test should release all Arc clones before close"),
    };
    store.close().expect("close");
}

#[test]
fn raw_watch_projection_matches_project_if_changed_after_relevant_append() {
    let (store, _dir) = seeded_store();
    let baseline_generation = store
        .entity_generation("entity:raw-proj")
        .expect("seeded entity generation");
    let mut watcher: ProjectionWatcher<RawCounter> =
        Arc::clone(&store).watch_projection::<RawCounter>("entity:raw-proj", Freshness::Consistent);
    let coord = Coordinate::new("entity:raw-proj", "scope:test").expect("coord");

    store
        .append(
            &coord,
            KIND,
            &CounterDelta {
                amount: 4,
                label: "parity".to_owned(),
            },
        )
        .expect("append parity event");

    let (gen, watched) = watcher.recv().expect("watch projection recv");
    let watched = watched.expect("watch projection state");
    let changed = store
        .project_if_changed::<RawCounter>(
            "entity:raw-proj",
            baseline_generation,
            &Freshness::Consistent,
        )
        .expect("project if changed")
        .expect("changed projection");
    let projected = changed.1.expect("projection state");

    assert_eq!(
        watched, projected,
        "PROPERTY: watch_projection and project_if_changed must share the same projection semantics \
         after a relevant event.\n\
         Investigate: src/store/mod.rs ProjectionWatcher::recv + src/store/projection/flow.rs."
    );
    assert_eq!(
        gen, changed.0,
        "PROPERTY: watch_projection and project_if_changed must report the same honest generation \
         after a relevant append."
    );

    drop(watcher);
    let store = match Arc::try_unwrap(store) {
        Ok(store) => store,
        Err(_) => {
            panic!("PROPERTY: raw parity watch test should release all Arc clones before close")
        }
    };
    store.close().expect("close");
}

#[test]
fn raw_watch_projection_matches_project_if_changed_after_irrelevant_append() {
    let (store, _dir) = seeded_store();
    let baseline_generation = store
        .entity_generation("entity:raw-proj")
        .expect("seeded entity generation");
    let baseline_state = store
        .project::<RawCounter>("entity:raw-proj", &Freshness::Consistent)
        .expect("baseline project")
        .expect("baseline state");
    let mut watcher: ProjectionWatcher<RawCounter> =
        Arc::clone(&store).watch_projection::<RawCounter>("entity:raw-proj", Freshness::Consistent);
    let coord = Coordinate::new("entity:raw-proj", "scope:test").expect("coord");

    store
        .append(
            &coord,
            NOISE_KIND,
            &CounterDelta {
                amount: 999,
                label: "noise".to_owned(),
            },
        )
        .expect("append irrelevant event");

    let (gen, watched) = watcher.recv().expect("watch projection recv");
    let watched = watched.expect("watch projection state");
    let changed = store
        .project_if_changed::<RawCounter>(
            "entity:raw-proj",
            baseline_generation,
            &Freshness::Consistent,
        )
        .expect("project if changed")
        .expect("changed projection");
    let projected = changed.1.expect("projection state");

    assert_eq!(
        watched, baseline_state,
        "PROPERTY: an irrelevant event may advance generation, but the watched raw projection must \
         keep the same folded state.\n\
         Investigate: src/store/mod.rs ProjectionWatcher::recv + RawCounter::relevant_event_kinds."
    );
    assert_eq!(
        watched, projected,
        "PROPERTY: watch_projection and project_if_changed must agree even when the entity changes \
         but the projection filter rejects the new event.\n\
         Investigate: src/store/mod.rs ProjectionWatcher::recv + src/store/projection/flow.rs."
    );
    assert_eq!(
        gen, changed.0,
        "PROPERTY: watch_projection and project_if_changed must report the same generation even \
         when the folded state is unchanged."
    );
    assert!(
        changed.0 > baseline_generation,
        "entity generation should still advance on the irrelevant append"
    );

    drop(watcher);
    let store = match Arc::try_unwrap(store) {
        Ok(store) => store,
        Err(_) => {
            panic!(
                "PROPERTY: raw irrelevant parity test should release all Arc clones before close"
            )
        }
    };
    store.close().expect("close");
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
    let (store, _dir) = cached_seeded_store();
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
    store
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

    let store = match Arc::try_unwrap(store) {
        Ok(store) => store,
        Err(_) => panic!("PROPERTY: maybe-stale matrix must release all Arc clones before close"),
    };
    store.close().expect("close maybe stale matrix store");
}
