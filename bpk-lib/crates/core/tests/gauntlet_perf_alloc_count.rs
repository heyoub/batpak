// justifies: INV-TEST-PANIC-AS-ASSERTION; allocation-budget test uses explicit panic/expect branches as assertion failures in tests/gauntlet_perf_alloc_count.rs.
#![allow(clippy::panic)]
#![cfg(feature = "alloc-count")]
//! GAUNT-PERF-5a: hot-path allocation-COUNT contract.
//!
//! PROVES: a single `Store::append` on the hot path performs no more than a
//! fixed, generous allocation budget. This is a DETERMINISTIC regression gate
//! against accidental per-append allocation blowups, not a micro-optimization
//! target — the bound is intentionally loose.
//!
//! This is a DEDICATED single-test binary because a `#[global_allocator]` is
//! process-wide: installing [`CountingAlloc`] here keeps the counters free of
//! allocations from unrelated tests. Run with `--features alloc-count`.
//!
//! Slug: GAUNT-PERF-5a / gauntlet_perf_alloc_count

use batpak::coordinate::Coordinate;
use batpak::event::EventKind;
use batpak::store::alloc::{self, AllocSnapshot, CountingAlloc};
use batpak::store::{Store, StoreConfig};
use tempfile::TempDir;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

const KIND: EventKind = EventKind::custom(0xA, 1);

/// Generous upper bound for allocations in a single steady-state `append`.
///
/// Picked from a quick observed count with comfortable headroom; the point is
/// that the gate exists and is deterministic, not that it is tight. If a change
/// legitimately raises the floor, bump this with a one-line justification.
#[cfg(not(gauntlet_red_fixture))]
const MAX_ALLOCS_PER_APPEND: u64 = 4_096;

/// RED fixture: under `--cfg gauntlet_red_fixture` the budget is flipped to 0, so
/// a real steady-state `append` (which always allocates) EXCEEDS it and the
/// assertions below fail. This proves the budget gate is anti-vacuous — it
/// actually reds on an over-budget append — and is exercised by the
/// `gauntlet-red-fixtures-bite` CI lane / `cargo xtask prove-gates-bite`.
#[cfg(gauntlet_red_fixture)]
const MAX_ALLOCS_PER_APPEND: u64 = 0;

#[test]
fn single_append_stays_under_allocation_budget() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    let coord = Coordinate::new("entity:alloc", "scope:perf").expect("valid coordinate");

    // Warm up: first appends touch lazily-initialized caches/segments whose
    // one-time allocations are not part of the steady-state hot path.
    for n in 0..16u32 {
        store
            .append(&coord, KIND, &serde_json::json!({ "warm": n }))
            .expect("warmup append");
    }

    // Measure a single steady-state append via explicit before/after
    // snapshots (also exercises the AllocSnapshot delta API), and cross-check
    // against the scope() guard.
    let payload = serde_json::json!({ "measured": true });
    // Reference via the `alloc` module path too (keeps the module import live
    // and documents the public seam: `batpak::store::alloc`).
    let before: AllocSnapshot = alloc::CountingAlloc::snapshot();
    let receipt = store
        .append(&coord, KIND, &payload)
        .expect("measured append");
    let after: AllocSnapshot = CountingAlloc::snapshot();
    let _ = receipt;

    let allocating = before.delta_allocating(after);
    let delta_allocs = before.delta_allocs(after);
    let delta_reallocs = before.delta_reallocs(after);
    assert!(
        allocating <= MAX_ALLOCS_PER_APPEND,
        "PROPERTY: a single steady-state Store::append must stay within the \
         allocation budget of {MAX_ALLOCS_PER_APPEND} (observed {allocating} \
         allocating calls: {delta_allocs} allocs + {delta_reallocs} reallocs)",
    );

    // Also exercise the scope() snapshot-delta guard over a second append and
    // confirm it reports a bounded, self-consistent delta.
    let (_r, scoped) = CountingAlloc::scope(|| {
        store
            .append(&coord, KIND, &serde_json::json!({ "scoped": true }))
            .expect("scoped append")
    });
    assert!(
        scoped.allocs + scoped.reallocs <= MAX_ALLOCS_PER_APPEND,
        "PROPERTY: scope()-measured append must also stay within budget \
         (observed {} allocs + {} reallocs)",
        scoped.allocs,
        scoped.reallocs,
    );
}
