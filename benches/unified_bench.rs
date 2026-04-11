//! Unified benchmarks for Tier 1+2 enhancements.
//! Group commit, IndexLayout (AoS/SoA/AoSoA), incremental projection, mmap reads.

mod common;

use batpak::prelude::*;
use batpak::store::{Freshness, Store, StoreConfig, SyncMode};
use common::{apply_profile, throughput_elements, BenchProfile};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Projection type for benchmarks
// ---------------------------------------------------------------------------

#[derive(Default, Debug, serde::Serialize, serde::Deserialize)]
struct BenchCounter {
    count: u64,
}

impl EventSourced<serde_json::Value> for BenchCounter {
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
        static KINDS: [EventKind; 1] = [EventKind::custom(0xF, 1)];
        &KINDS
    }
    fn supports_incremental_apply() -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn open_store_with_batch(batch: u32) -> (Store, TempDir, Coordinate, EventKind) {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_group_commit_max_batch(batch)
        .with_sync_every_n_events(1)
        .with_sync_mode(SyncMode::SyncData);
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("bench:entity", "bench:scope").expect("coord");
    let kind = EventKind::custom(0xF, 1);
    (store, dir, coord, kind)
}

fn open_store_with_layout(layout: IndexLayout) -> (Store, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_index_layout(layout)
        .with_sync_every_n_events(10_000);
    (Store::open(config).expect("open"), dir)
}

// ===========================================================================
// GROUP COMMIT: batch_32 vs batch_1 durable throughput
// ===========================================================================

fn bench_group_commit(c: &mut Criterion) {
    let mut group = c.benchmark_group("group_commit_durable");
    apply_profile(&mut group, BenchProfile::Heavy);
    throughput_elements(&mut group, 1_000);

    group.bench_function("batch_32", |b| {
        b.iter_batched(
            || open_store_with_batch(32),
            |(store, _dir, coord, kind)| {
                for i in 0u32..1_000 {
                    let opts = AppendOptions::new().with_idempotency(i as u128 + 1);
                    store
                        .append_with_options(&coord, kind, &serde_json::json!({"i": i}), opts)
                        .expect("append");
                }
                store.close().expect("close");
            },
            BatchSize::LargeInput,
        );
    });

    group.bench_function("batch_1_baseline", |b| {
        b.iter_batched(
            || open_store_with_batch(1),
            |(store, _dir, coord, kind)| {
                for i in 0u32..1_000 {
                    store
                        .append(&coord, kind, &serde_json::json!({"i": i}))
                        .expect("append");
                }
                store.close().expect("close");
            },
            BatchSize::LargeInput,
        );
    });

    group.finish();
}

// ===========================================================================
// INDEX LAYOUT: AoS vs SoA vs AoSoA8 by_fact query speed
// ===========================================================================

fn bench_layout_by_fact(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout_by_fact");
    apply_profile(&mut group, BenchProfile::Quick);

    let kind = EventKind::custom(0xF, 1);
    let coord = Coordinate::new("bench:layout", "bench:scope").expect("coord");

    // Populate three stores with different layouts
    let layouts: Vec<(&str, IndexLayout)> = vec![
        ("aos", IndexLayout::AoS),
        ("soa", IndexLayout::SoA),
        ("aosoa8", IndexLayout::AoSoA8),
    ];

    for (name, layout) in &layouts {
        let (store, _dir) = open_store_with_layout(layout.clone());
        for i in 0u32..1_000 {
            store
                .append(&coord, kind, &serde_json::json!({"i": i}))
                .expect("append");
        }

        // Sanity-check ONCE outside the bench loop, not inside `b.iter`.
        // A correctness assertion inside `iter` (a) crashes the bench
        // run on regression instead of a clean test failure, (b) adds
        // a comparison + branch to every measured iteration polluting
        // the measurement.
        let warmup = store.by_fact(kind);
        assert_eq!(
            warmup.len(),
            1_000,
            "BENCH SETUP: expected 1000 events from by_fact before \
             measurement, got {}. Layout: {name}.",
            warmup.len()
        );

        group.bench_function(*name, |b| {
            b.iter(|| {
                criterion::black_box(store.by_fact(kind));
            });
        });

        store.close().expect("close");
    }

    group.finish();
}

// ===========================================================================
// INCREMENTAL PROJECTION: full replay vs incremental delta
// ===========================================================================

fn bench_incremental_projection(c: &mut Criterion) {
    let mut group = c.benchmark_group("incremental_projection");
    apply_profile(&mut group, BenchProfile::Quick);
    throughput_elements(&mut group, 1_000);

    let kind = EventKind::custom(0xF, 1);
    let coord = Coordinate::new("bench:inc", "bench:scope").expect("coord");

    // Setup: populate 1000 events, project once to warm cache
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_incremental_projection(true)
        .with_sync_every_n_events(10_000);
    let store = Store::open(config).expect("open");
    for i in 0u32..1_000 {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }
    let _: Option<BenchCounter> = store
        .project("bench:inc", &Freshness::Consistent)
        .expect("warm cache");

    // Add 5 more events — incremental path should apply only these
    for i in 1_000u32..1_005 {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append delta");
    }

    group.bench_function("incremental_5_new_events", |b| {
        b.iter(|| {
            let _: Option<BenchCounter> = store
                .project("bench:inc", &Freshness::Consistent)
                .expect("incremental project");
        });
    });

    // Baseline: full replay of all 1005 events (no incremental)
    let dir2 = TempDir::new().expect("temp dir");
    let config2 = StoreConfig::new(dir2.path())
        .with_incremental_projection(false) // force full replay
        .with_sync_every_n_events(10_000);
    let store2 = Store::open(config2).expect("open baseline");
    for i in 0u32..1_005 {
        store2
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }

    group.bench_function("full_replay_1005_events", |b| {
        b.iter(|| {
            let _: Option<BenchCounter> = store2
                .project("bench:inc", &Freshness::Consistent)
                .expect("full replay");
        });
    });

    store.close().expect("close");
    store2.close().expect("close");
    group.finish();
}

criterion_group!(
    benches,
    bench_group_commit,
    bench_layout_by_fact,
    bench_incremental_projection
);
criterion_main!(benches);
