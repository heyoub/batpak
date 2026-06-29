//! PROVES: incremental projection apply matches full replay (audit R9).
//! CATCHES: incremental branch diverging from from_events replay.

use batpak::event::{Event, EventKind, EventSourced};
use batpak::store::{Freshness, Store, StoreConfig};
use batpak_testkit::prelude::*;
use tempfile::TempDir;

macro_rules! single_entity_state_contract {
    ($key_space:literal) => {
        const STATE_CONTRACT: ProjectionStateContract =
            ProjectionStateContract::single_entity($key_space);

        fn state_extent(&self) -> StateExtent {
            StateExtent::single_entity()
        }
    };
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq)]
struct SumCounter {
    total: i64,
}

impl EventSourced for SumCounter {
    type Input = batpak::event::JsonValueInput;
    single_entity_state_contract!("projection-flow-incremental-xcheck");

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        if events.is_empty() {
            return None;
        }
        let mut state = Self { total: 0 };
        for event in events {
            state.apply_event(event);
        }
        Some(state)
    }

    fn apply_event(&mut self, event: &Event<serde_json::Value>) {
        if let Some(n) = event.payload.get("n").and_then(|v| v.as_i64()) {
            self.total += n;
        }
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[]
    }

    fn supports_incremental_apply() -> bool {
        true
    }
}

#[test]
fn incremental_apply_matches_full_replay_cross_check() -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let data = dir.path().join("data");
    let cache = dir.path().join("cache");
    let config = StoreConfig::new(&data)
        .with_sync_every_n_events(1)
        .with_incremental_projection(true);
    let store = Store::open_with_native_cache(config, &cache)?;

    let coord = Coordinate::new("entity:xcheck", "scope:test")?;
    let kind = EventKind::custom(0xF, 1);
    let _ = store.append(&coord, kind, &serde_json::json!({ "n": 10 }))?;
    let _ = store.append(&coord, kind, &serde_json::json!({ "n": 20 }))?;

    let first: Option<SumCounter> = store.project("entity:xcheck", &Freshness::Consistent)?;
    assert_eq!(first, Some(SumCounter { total: 30 }));

    let _ = store.append(&coord, kind, &serde_json::json!({ "n": 7 }))?;

    let incremental: Option<SumCounter> = store.project("entity:xcheck", &Freshness::Consistent)?;
    store.close()?;

    let config_full = StoreConfig::new(&data)
        .with_sync_every_n_events(1)
        .with_incremental_projection(false);
    let store_full = Store::open(config_full)?;
    let full_replay: Option<SumCounter> =
        store_full.project("entity:xcheck", &Freshness::Consistent)?;

    assert_eq!(
        incremental, full_replay,
        "PROPERTY: incremental apply must match full replay for supports_incremental_apply types"
    );
    assert_eq!(full_replay, Some(SumCounter { total: 37 }));

    store_full.close()?;
    Ok(())
}
