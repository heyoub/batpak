//! `VersionedCounterV2` + `IncrementalCounter` projections exercised only by
//! `unified_projection_red`.
//!
//! Included via `#[path = "support/red_versioned_counters.rs"]` by its sole
//! consumer, which uses both structs to prove schema-version isolation and
//! the incremental-apply lane. Splitting these out keeps the broader
//! `red_counters` module dead_code-clean for watch (see ADR-0012).

use batpak::prelude::*;

/// Like `AllCounter`, but advertises schema version 2 so cache-slot isolation
/// tests can prove different versions get different cache slots.
#[derive(Default, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct VersionedCounterV2 {
    pub count: u64,
}

impl EventSourced for VersionedCounterV2 {
    type Input = batpak::prelude::JsonValueInput;
    const STATE_CONTRACT: ProjectionStateContract =
        ProjectionStateContract::single_entity("testkit-versioned-counter-v2");

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        if events.is_empty() {
            return None;
        }
        let mut s = Self::default();
        for e in events {
            s.apply_event(e);
        }
        Some(s)
    }

    fn apply_event(&mut self, _event: &Event<serde_json::Value>) {
        self.count += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[]
    }

    fn schema_version() -> u64 {
        2
    }

    fn state_extent(&self) -> StateExtent {
        StateExtent::single_entity()
    }
}

/// Like `AllCounter`, but opts into incremental apply so projection tests can
/// exercise the incremental-apply lane.
#[derive(Default, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct IncrementalCounter {
    pub count: u64,
}

impl EventSourced for IncrementalCounter {
    type Input = batpak::prelude::JsonValueInput;
    const STATE_CONTRACT: ProjectionStateContract =
        ProjectionStateContract::single_entity("testkit-incremental-counter");

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        if events.is_empty() {
            return None;
        }
        let mut s = Self::default();
        for e in events {
            s.apply_event(e);
        }
        Some(s)
    }

    fn apply_event(&mut self, _event: &Event<serde_json::Value>) {
        self.count += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[]
    }

    fn supports_incremental_apply() -> bool {
        true
    }

    fn state_extent(&self) -> StateExtent {
        StateExtent::single_entity()
    }
}
