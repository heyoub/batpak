//! Generic-projection coverage for `#[derive(EventSourced)]`.
//! Harness pattern: Equivalence Harness (parity lane).
//!
//! The derive's handler-signature pins live inside a generic `impl` so they can
//! reference `Self` with the struct's type parameters. A module-scope
//! `const _: fn(&mut Foo<T>, &E)` could not name `T` without reintroducing it,
//! and would fail to compile on any generic struct. This test pins that fix:
//!
//!   1. A generic `Foo<T: …>` with `#[derive(EventSourced)]` compiles.
//!   2. The concrete instantiation `Foo<u64>` runs a real projection through
//!      the store — end-to-end behaviour matches a non-generic equivalent.

use batpak_testkit::prelude::*;
use serde::{Deserialize, Serialize};

use batpak_testkit::small_store as small_store_support;
use small_store_support::small_segment_store;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 7, type_id = 9)]
struct Bumped {
    amount: u64,
}

/// Generic projection carrying `state: T`. The `T` bounds match what the
/// store requires of a projection payload field plus what `Default::default()`
/// and serde replay need.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize, EventSourced)]
#[batpak(input = JsonValueInput, cache_version = 0)]
#[batpak(event = Bumped, handler = on_bump)]
struct Foo<T>
where
    T: Clone + Send + Sync + 'static + Default + core::ops::AddAssign<u64>,
{
    state: T,
}

impl<T> Foo<T>
where
    T: Clone + Send + Sync + 'static + Default + core::ops::AddAssign<u64>,
{
    fn on_bump(&mut self, p: &Bumped) {
        self.state += p.amount;
    }
}

#[test]
fn generic_projection_compiles_and_projects() {
    let (_dir, store) = small_segment_store().expect("open small segment store");
    let coord = Coordinate::new("entity:generic", "scope:test").expect("valid coord");
    store
        .append_typed(&coord, &Bumped { amount: 2 })
        .expect("append amount 2");
    store
        .append_typed(&coord, &Bumped { amount: 5 })
        .expect("append amount 5");
    store
        .append_typed(&coord, &Bumped { amount: 11 })
        .expect("append amount 11");

    let projected = store
        .project::<Foo<u64>>("entity:generic", &Freshness::Consistent)
        .expect("project generic Foo")
        .expect("generic projection has state");

    assert_eq!(
        projected.state, 18u64,
        "PROPERTY: generic Foo<T=u64> replays through the store and accumulates"
    );
    store.close().expect("close store");
}
