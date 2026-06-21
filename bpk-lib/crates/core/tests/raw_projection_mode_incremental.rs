//! Raw projection mode: incremental-apply replay lanes.
//! Harness pattern: Equivalence Harness.
//!
//! PROVES: incremental-apply (`supports_incremental_apply`) replay folds the
//! newly appended relevant event identically on the raw-msgpack and json-value
//! lanes, both for the group-local native cache and for an external native
//! cache reopened from cold.
//! CATCHES: incremental-replay lane drift, and external-cache cold-start
//! divergence where one lane misses or double-applies the trailing event.
//! SEEDED: deterministic / no randomness.

use std::sync::Arc;

mod support;
use batpak::store::{Freshness, Store, StoreConfig};
use serde::{Deserialize, Serialize};
use support::prelude::*;
use tempfile::TempDir;

#[path = "support/raw_projection_mode.rs"]
mod rpm_support;
use rpm_support::{CounterDelta, KIND};

trait MatrixCounterState {
    fn summary(&self) -> (i64, u32);
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ValueIncrementalCounter {
    value: i64,
    seen: u32,
}

impl EventSourced for ValueIncrementalCounter {
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
        let delta = serde_json::from_value::<CounterDelta>(event.payload.clone()).expect(
            "ValueIncrementalCounter::apply_event expects replay payloads that match CounterDelta",
        );
        self.value += delta.amount;
        self.seen += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[KIND]
    }

    fn supports_incremental_apply() -> bool {
        true
    }
}

impl MatrixCounterState for ValueIncrementalCounter {
    fn summary(&self) -> (i64, u32) {
        (self.value, self.seen)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct RawIncrementalCounter {
    value: i64,
    seen: u32,
}

impl EventSourced for RawIncrementalCounter {
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
        let delta = rmp_serde::from_slice::<CounterDelta>(&event.payload).expect(
            "RawIncrementalCounter::apply_event expects replay payloads that decode as CounterDelta",
        );
        self.value += delta.amount;
        self.seen += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[KIND]
    }

    fn supports_incremental_apply() -> bool {
        true
    }
}

impl MatrixCounterState for RawIncrementalCounter {
    fn summary(&self) -> (i64, u32) {
        (self.value, self.seen)
    }
}

fn cached_seeded_store_for(entity: &str) -> (TempDir, Arc<Store>) {
    let dir = TempDir::new().expect("temp dir");
    let cache_path = dir.path().join("cache");
    let config = StoreConfig::new(dir.path().join("data"))
        .with_segment_max_bytes(4096)
        .with_sync_every_n_events(1);
    let store = Arc::new(
        Store::open_with_native_cache(config, &cache_path).expect("open with native cache"),
    );
    let coord = Coordinate::new(entity, "scope:test").expect("coord");
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
    (dir, store)
}

#[test]
fn projection_flow_incremental_group_local_keeps_lanes_equivalent() {
    let (_dir, store) = cached_seeded_store_for("entity:raw-proj-incremental-group-local");
    let baseline_value = store
        .project::<ValueIncrementalCounter>(
            "entity:raw-proj-incremental-group-local",
            &Freshness::Consistent,
        )
        .expect("seed value incremental cache")
        .expect("baseline value state");
    let baseline_raw = store
        .project::<RawIncrementalCounter>(
            "entity:raw-proj-incremental-group-local",
            &Freshness::Consistent,
        )
        .expect("seed raw incremental cache")
        .expect("baseline raw state");
    assert_eq!(baseline_value.summary(), baseline_raw.summary());

    let coord =
        Coordinate::new("entity:raw-proj-incremental-group-local", "scope:test").expect("coord");
    store
        .append(
            &coord,
            KIND,
            &CounterDelta {
                amount: 4,
                label: "group-local-incremental".to_owned(),
            },
        )
        .expect("append relevant incremental event");

    let value = store
        .project::<ValueIncrementalCounter>(
            "entity:raw-proj-incremental-group-local",
            &Freshness::Consistent,
        )
        .expect("project value incremental")
        .expect("value incremental state");
    let raw = store
        .project::<RawIncrementalCounter>(
            "entity:raw-proj-incremental-group-local",
            &Freshness::Consistent,
        )
        .expect("project raw incremental")
        .expect("raw incremental state");

    assert_eq!(
        value.summary(),
        (15, 5),
        "PROPERTY: group-local incremental replay must fold the new relevant event on the value lane."
    );
    assert_eq!(
        raw.summary(),
        value.summary(),
        "PROPERTY: group-local incremental replay must stay equivalent across raw and value lanes."
    );

    let store = Arc::try_unwrap(store).map_err(|_| ()).expect(
        "PROPERTY: incremental group-local test should release all Arc clones before close",
    );
    store.close().expect("close");
}

#[test]
fn projection_flow_incremental_external_cache_keeps_lanes_equivalent() {
    let (dir, store) = cached_seeded_store_for("entity:raw-proj-incremental-external");
    let cache_path = dir.path().join("cache");
    let data_path = dir.path().join("data");
    let baseline_value = store
        .project::<ValueIncrementalCounter>(
            "entity:raw-proj-incremental-external",
            &Freshness::Consistent,
        )
        .expect("seed value external incremental cache")
        .expect("baseline value state");
    let baseline_raw = store
        .project::<RawIncrementalCounter>(
            "entity:raw-proj-incremental-external",
            &Freshness::Consistent,
        )
        .expect("seed raw external incremental cache")
        .expect("baseline raw state");
    assert_eq!(baseline_value.summary(), baseline_raw.summary());

    let store = Arc::try_unwrap(store).map_err(|_| ()).expect(
        "PROPERTY: incremental external-cache seed test should release all Arc clones before close",
    );
    store.close().expect("close seeded store");

    let config = StoreConfig::new(data_path)
        .with_segment_max_bytes(4096)
        .with_sync_every_n_events(1);
    let reopened = Arc::new(
        Store::open_with_native_cache(config, &cache_path).expect("reopen with native cache"),
    );
    let coord =
        Coordinate::new("entity:raw-proj-incremental-external", "scope:test").expect("coord");
    reopened
        .append(
            &coord,
            KIND,
            &CounterDelta {
                amount: 4,
                label: "external-incremental".to_owned(),
            },
        )
        .expect("append relevant incremental event");

    let value = reopened
        .project::<ValueIncrementalCounter>(
            "entity:raw-proj-incremental-external",
            &Freshness::Consistent,
        )
        .expect("project value external incremental")
        .expect("value external incremental state");
    let raw = reopened
        .project::<RawIncrementalCounter>(
            "entity:raw-proj-incremental-external",
            &Freshness::Consistent,
        )
        .expect("project raw external incremental")
        .expect("raw external incremental state");

    assert_eq!(
        value.summary(),
        (15, 5),
        "PROPERTY: external-cache incremental replay must fold the new relevant event on the value lane."
    );
    assert_eq!(
        raw.summary(),
        value.summary(),
        "PROPERTY: external-cache incremental replay must stay equivalent across raw and value lanes."
    );

    let reopened = Arc::try_unwrap(reopened).map_err(|_| ()).expect(
        "PROPERTY: incremental external-cache test should release all Arc clones before close",
    );
    reopened.close().expect("close reopened");
}
