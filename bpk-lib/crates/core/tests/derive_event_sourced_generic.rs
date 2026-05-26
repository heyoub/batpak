// justifies: INV-TEST-PANIC-AS-ASSERTION; test body in tests/derive_event_sourced_generic.rs exercises precondition-holds invariants; .unwrap is acceptable in test code where a panic is a test failure.
#![allow(clippy::unwrap_used, clippy::panic)]
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

mod support;
use serde::{Deserialize, Serialize};
use support::prelude::*;

#[path = "support/small_store.rs"]
mod small_store_support;
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
    let (store, _dir) = small_segment_store().unwrap();
    let coord = Coordinate::new("entity:generic", "scope:test").unwrap();
    store.append_typed(&coord, &Bumped { amount: 2 }).unwrap();
    store.append_typed(&coord, &Bumped { amount: 5 }).unwrap();
    store.append_typed(&coord, &Bumped { amount: 11 }).unwrap();

    let projected = store
        .project::<Foo<u64>>("entity:generic", &Freshness::Consistent)
        .unwrap()
        .expect("generic projection has state");

    assert_eq!(
        projected.state, 18u64,
        "PROPERTY: generic Foo<T=u64> replays through the store and accumulates"
    );
    store.close().unwrap();
}
