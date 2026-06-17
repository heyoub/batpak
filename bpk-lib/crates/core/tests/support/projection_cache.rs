//! Shared fixtures for the split `projection_cache*` integration harnesses.
//!
//! Included via `#[path = "support/projection_cache.rs"]` by every
//! `projection_cache*` test binary. The harness was split out of a single
//! over-cap file along the seam "cache-freshness modes vs cache-corruption
//! shapes" into three binaries (`projection_cache` backend mechanics,
//! `projection_cache_freshness` window semantics, `projection_cache_corruption`
//! decode fall-backs).
//!
//! Dead-code discipline (see ADR-0012): in an integration/test crate `pub`
//! does NOT suppress `dead_code`, and a support item used by only a subset of
//! binaries warns in the others. So this module holds ONLY items consumed by
//! EVERY split binary. That is exactly one item: [`MaybeStaleCounter`], the
//! projection type folded by all three binaries.
//!
//! The tiny `test_meta()` `CacheMeta` constructor is NOT here: it is used by
//! the backend-mechanics and corruption binaries but never by the
//! freshness-window binary, so hoisting it would leave a dead `test_meta` in
//! that binary. It is inlined in the two binaries that need it instead.

/// Projection type folded by every split `projection_cache*` binary: a plain
/// event counter so the assertions can pin exact fold results across cache
/// hits, stale rows, and corruption fall-backs.
#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
pub struct MaybeStaleCounter {
    pub count: u32,
}

impl batpak::prelude::EventSourced for MaybeStaleCounter {
    type Input = batpak::prelude::JsonValueInput;

    fn from_events(events: &[batpak::prelude::Event<serde_json::Value>]) -> Option<Self> {
        Some(MaybeStaleCounter {
            count: u32::try_from(events.len()).expect("test uses < 2^32 events"),
        })
    }
    fn apply_event(&mut self, _event: &batpak::prelude::Event<serde_json::Value>) {
        self.count += 1;
    }
    fn relevant_event_kinds() -> &'static [batpak::prelude::EventKind] {
        &[]
    }
}
