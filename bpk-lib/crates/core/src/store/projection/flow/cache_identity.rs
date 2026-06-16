use crate::event::EventSourced;
use std::any::TypeId;
use std::hash::{Hash, Hasher};

/// Build the projection cache key for a given entity and projection type.
///
/// Key layout: `entity + \0 + type_id_hash(u64 LE) + schema_version(u64 LE) +
/// relevant_kinds_hash(u64 LE)`.
///
/// - `type_id_hash` ensures different [`EventSourced`] types never collide on
///   the same entity.
/// - `schema_version` invalidates the cache when replay semantics change.
/// - `relevant_kinds_hash` is a stable hash of `T::relevant_event_kinds()`.
///   Changing which event kinds a projection consumes invalidates the cache
///   automatically — no `schema_version` bump required for that reason.
///   (Changing replay semantics per-kind still requires a `schema_version` bump.)
pub(crate) fn projection_cache_key<T>(entity: &str) -> Vec<u8>
where
    T: EventSourced + 'static,
{
    let schema_v = T::schema_version();
    let type_disc = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        TypeId::of::<T>().hash(&mut h);
        h.finish()
    };
    let kinds_disc = relevant_kinds_hash::<T>();
    let mut cache_key = Vec::with_capacity(entity.len() + 1 + 8 + 8 + 8);
    cache_key.extend_from_slice(entity.as_bytes());
    cache_key.push(0);
    cache_key.extend_from_slice(&type_disc.to_le_bytes());
    cache_key.extend_from_slice(&schema_v.to_le_bytes());
    cache_key.extend_from_slice(&kinds_disc.to_le_bytes());
    cache_key
}

/// Stable hash of `T::relevant_event_kinds()` for use as a cache-key component.
///
/// Event kinds are first serialised with [`crate::event::EventKind::as_raw_u16`],
/// the canonical `(category << 12) | type_id` representation, sorted, then fed
/// into a `DefaultHasher`. The sort makes the hash order-insensitive: a
/// projection that declares `[EFFECT_ERROR, DATA]` and one that declares
/// `[DATA, EFFECT_ERROR]` produce the same key. Uses the same hasher family as
/// the `TypeId` discriminant above to keep the key derivation stylistically
/// consistent.
fn relevant_kinds_hash<T>() -> u64
where
    T: EventSourced + 'static,
{
    let mut kinds: Vec<u16> = T::relevant_event_kinds()
        .iter()
        .map(|k| k.as_raw_u16())
        .collect();
    kinds.sort_unstable();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for k in &kinds {
        k.hash(&mut h);
    }
    // Also fold the count so `[]` and `[0]` cannot collide via the same
    // hash-finish value on an empty feed.
    kinds.len().hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod relevant_kinds_hash_tests {
    use super::*;
    use crate::event::{Event, EventKind};

    struct OneKind;
    impl EventSourced for OneKind {
        type Input = crate::event::JsonValueInput;
        fn apply_event(&mut self, _event: &Event<serde_json::Value>) {}
        fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
            (!events.is_empty()).then_some(Self)
        }
        fn relevant_event_kinds() -> &'static [EventKind] {
            static KINDS: [EventKind; 1] = [EventKind::DATA];
            &KINDS
        }
    }

    struct TwoKinds;
    impl EventSourced for TwoKinds {
        type Input = crate::event::JsonValueInput;
        fn apply_event(&mut self, _event: &Event<serde_json::Value>) {}
        fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
            (!events.is_empty()).then_some(Self)
        }
        fn relevant_event_kinds() -> &'static [EventKind] {
            static KINDS: [EventKind; 2] = [EventKind::DATA, EventKind::EFFECT_ERROR];
            &KINDS
        }
    }

    #[test]
    fn distinct_kind_sets_produce_distinct_hashes() {
        // Pins `relevant_kinds_hash`: collapsing it to a constant (e.g. 0) would
        // make projections with different relevant_event_kinds collide on the
        // same cache key, serving stale state across kind-set changes.
        assert_ne!(
            relevant_kinds_hash::<OneKind>(),
            relevant_kinds_hash::<TwoKinds>(),
            "different relevant_event_kinds must hash differently"
        );
    }

    #[test]
    fn cache_key_kinds_component_tracks_relevant_kinds() {
        // The trailing 8 bytes of the cache key are the kinds hash; two
        // projections with different kind sets must differ there.
        let one = projection_cache_key::<OneKind>("entity");
        let two = projection_cache_key::<TwoKinds>("entity");
        assert_ne!(one[one.len() - 8..], two[two.len() - 8..]);
    }
}
