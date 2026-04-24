//! `AllCounter` + `KindFilteredCounter` projections for the watch and
//! projection red tests.
//!
//! Included via `#[path = "support/red_counters.rs"]` only by the two tests
//! that exercise these projections (`unified_watch_red`,
//! `unified_projection_red`). Both consumers use both counters, so neither
//! struct is dead_code in any including binary (see ADR-0012).

use batpak::prelude::*;

/// Counts events whose `event_kind` equals `EventKind::custom(0xF, 1)`,
/// bucketed into `target_count` vs `noise_count`.
#[derive(Default, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct KindFilteredCounter {
    pub target_count: u64,
    pub noise_count: u64,
}

impl EventSourced for KindFilteredCounter {
    type Input = batpak::prelude::JsonValueInput;

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

    fn apply_event(&mut self, event: &Event<serde_json::Value>) {
        if event.event_kind() == EventKind::custom(0xF, 1) {
            self.target_count += 1;
        } else {
            self.noise_count += 1;
        }
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        static KINDS: [EventKind; 1] = [EventKind::custom(0xF, 1)];
        &KINDS
    }
}

/// Counts every event regardless of kind.
#[derive(Default, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct AllCounter {
    pub count: u64,
}

impl EventSourced for AllCounter {
    type Input = batpak::prelude::JsonValueInput;

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
}
