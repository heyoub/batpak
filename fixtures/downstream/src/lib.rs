//! Downstream path-hygiene fixture for every batpak derive.
//!
//! This crate depends on `batpak` the way an external user would: as a
//! normal path dependency, without any direct reference to
//! `batpak-macros` or `batpak-macros-support`. Every derive's generated
//! `::batpak::...` paths must resolve cleanly from here. This fixture
//! covers all three derives — `EventPayload`, `EventSourced`, and
//! `MultiEventReactor` — using neutral payload names so the fixture
//! cannot accidentally leak domain nouns into library space.

use batpak::prelude::*;
use serde::{Deserialize, Serialize};

// ── EventPayload ────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 2, type_id = 10)]
pub struct Tick {
    pub counter: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 2, type_id = 11)]
pub struct Incremented {
    pub amount: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 2, type_id = 12)]
pub struct ThingHappened {
    pub label: String,
}

// ── EventSourced projection ─────────────────────────────────────────────────

/// Minimal projection accumulating increments from `Incremented` events.
/// Touches the EventSourced derive's generic `impl` expansion and the
/// `Default + Serialize + Deserialize` bounds that the projection runtime
/// requires.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize, EventSourced)]
#[batpak(input = JsonValueInput, cache_version = 0)]
#[batpak(event = Incremented, handler = on_inc)]
#[batpak(event = Tick, handler = on_tick)]
pub struct Counter {
    pub total: i64,
    pub ticks: u32,
}

impl Counter {
    fn on_inc(&mut self, payload: &Incremented) {
        self.total += payload.amount;
    }

    fn on_tick(&mut self, _payload: &Tick) {
        self.ticks += 1;
    }
}

// ── MultiEventReactor ───────────────────────────────────────────────────────

#[derive(Debug)]
pub struct NeverFails;

impl std::fmt::Display for NeverFails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "never")
    }
}

impl std::error::Error for NeverFails {}

/// A passive reactor over two bindings. The derive generates the multi-kind
/// dispatcher and the `MultiReactive` impl; the downstream crate exercises
/// the derive's `::batpak::...` path resolution end-to-end.
#[derive(Default, MultiEventReactor)]
#[batpak(input = JsonValueInput, error = NeverFails)]
#[batpak(event = Tick, handler = on_tick)]
#[batpak(event = ThingHappened, handler = on_thing)]
pub struct Observer;

impl Observer {
    fn on_tick(
        &mut self,
        _event: &batpak::event::StoredEvent<Tick>,
        _out: &mut ReactionBatch,
    ) -> Result<(), NeverFails> {
        Ok(())
    }

    fn on_thing(
        &mut self,
        _event: &batpak::event::StoredEvent<ThingHappened>,
        _out: &mut ReactionBatch,
    ) -> Result<(), NeverFails> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_payload_kinds_resolve_from_downstream() {
        assert_eq!(Tick::KIND, EventKind::custom(2, 10));
        assert_eq!(Incremented::KIND, EventKind::custom(2, 11));
        assert_eq!(ThingHappened::KIND, EventKind::custom(2, 12));
    }

    #[test]
    fn event_sourced_derive_drives_a_real_projection() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        let coord = Coordinate::new("entity:downstream", "scope:test").expect("coord");

        store
            .append_typed(&coord, &Incremented { amount: 3 })
            .expect("append Incremented");
        store
            .append_typed(&coord, &Tick { counter: 1 })
            .expect("append Tick");
        store
            .append_typed(&coord, &Incremented { amount: 4 })
            .expect("append Incremented");

        let projected = store
            .project::<Counter>("entity:downstream", &Freshness::Consistent)
            .expect("project counter")
            .expect("counter has state");

        assert_eq!(projected.total, 7);
        assert_eq!(projected.ticks, 1);

        store.close().expect("close");
    }

    #[test]
    fn multi_event_reactor_derive_relevant_kinds_resolve() {
        // The MultiEventReactor derive emits `relevant_event_kinds()` on the
        // `MultiReactive` impl. Reaching those constants through the derive's
        // generated paths proves that `::batpak::event::EventKind` resolves
        // for a downstream crate with only `batpak` in its dependency tree.
        let kinds = <Observer as MultiReactive<batpak::event::JsonValueInput>>::relevant_event_kinds();
        assert_eq!(kinds, &[Tick::KIND, ThingHappened::KIND]);
    }
}
