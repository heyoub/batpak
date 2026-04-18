// justifies: shared support module is included via #[path] by multiple unified_red test binaries; each binary only exercises a subset of the helpers and imports so dead_code and unused_imports are expected per-binary.
#![allow(dead_code, unused_imports)]

pub(crate) use batpak::prelude::*;
pub(crate) use batpak::store::{Freshness, IndexTopology, Store, StoreConfig, StoreError};
pub(crate) use std::sync::Arc;
pub(crate) use tempfile::TempDir;

#[path = "../common/mod.rs"]
mod common;
pub(crate) use common::test_coord;

/// Counter that only cares about kind 0xF:1. noise_count tracks leakage.
#[derive(Default, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub(crate) struct KindFilteredCounter {
    pub(crate) target_count: u64,
    pub(crate) noise_count: u64,
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

/// Counter that replays everything (empty relevant_event_kinds).
#[derive(Default, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub(crate) struct AllCounter {
    pub(crate) count: u64,
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

/// Counter with schema_version override for cache isolation tests.
#[derive(Default, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub(crate) struct VersionedCounterV2 {
    pub(crate) count: u64,
}

impl EventSourced for VersionedCounterV2 {
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

    fn schema_version() -> u64 {
        2
    }
}

/// Counter that opts into incremental apply.
#[derive(Default, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub(crate) struct IncrementalCounter {
    pub(crate) count: u64,
}

impl EventSourced for IncrementalCounter {
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

    fn supports_incremental_apply() -> bool {
        true
    }
}

pub(crate) fn kind_a() -> EventKind {
    EventKind::custom(0xF, 1)
}

pub(crate) fn kind_b() -> EventKind {
    EventKind::custom(0xF, 2)
}

pub(crate) fn payload(i: u32) -> serde_json::Value {
    serde_json::json!({ "i": i })
}
