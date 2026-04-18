// justifies: raw projection mode tests use panic! as the assertion style when the raw-dispatch contract breaks.
#![allow(clippy::panic)]

use std::sync::Arc;

use batpak::prelude::*;
use batpak::store::{Freshness, ProjectionWatcher, Store, StoreConfig};
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
    let mut watcher: ProjectionWatcher<RawCounter> =
        Arc::clone(&store).watch_projection::<RawCounter>("entity:raw-proj", Freshness::Consistent);
    let subscription = watcher.subscription();
    let subscription_rx = subscription.receiver();
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

    let update = watcher
        .recv()
        .expect("watch projection recv")
        .expect("watch projection state");
    assert_eq!(update.value, 16);
    assert_eq!(update.seen, 5);

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

    let watched = watcher
        .recv()
        .expect("watch projection recv")
        .expect("watch projection state");
    let projected = store
        .project_if_changed::<RawCounter>(
            "entity:raw-proj",
            baseline_generation,
            &Freshness::Consistent,
        )
        .expect("project if changed")
        .expect("changed projection")
        .1
        .expect("projection state");

    assert_eq!(
        watched, projected,
        "PROPERTY: watch_projection and project_if_changed must share the same projection semantics \
         after a relevant event.\n\
         Investigate: src/store/mod.rs ProjectionWatcher::recv + src/store/projection/flow.rs."
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

    let watched = watcher
        .recv()
        .expect("watch projection recv")
        .expect("watch projection state");
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
