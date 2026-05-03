//! Unified benchmarks for group commit, topology choices, and incremental replay.

use batpak::prelude::*;
use batpak::store::{Freshness, IndexTopology, Store, StoreConfig, SyncMode};
use batpak_bench_support::{apply_profile, throughput_elements, BenchProfile};
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use std::hint::black_box;
use std::sync::{Arc, Barrier};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Projection type for benchmarks
// ---------------------------------------------------------------------------

#[derive(Default, Debug, serde::Serialize, serde::Deserialize)]
struct BenchCounter {
    count: u64,
}

impl EventSourced for BenchCounter {
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

fn open_store_with_group_commit(
    batch: u32,
    every_n_events: u32,
) -> (Store, TempDir, Coordinate, EventKind) {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_group_commit_max_batch(batch)
        .with_sync_every_n_events(every_n_events)
        .with_sync_mode(SyncMode::SyncData);
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("bench:entity", "bench:scope").expect("coord");
    let kind = EventKind::custom(0xF, 1);
    (store, dir, coord, kind)
}

fn append_ordinary_events(
    store: &Store,
    coord: &Coordinate,
    kind: EventKind,
    total_events: u32,
    use_idempotency: bool,
    idempotency_base: u128,
) {
    for i in 0..total_events {
        let payload = serde_json::json!({ "i": i });
        if use_idempotency {
            let opts = AppendOptions::new().with_idempotency(idempotency_base + u128::from(i));
            store
                .append_with_options(coord, kind, &payload, opts)
                .expect("append with idempotency");
        } else {
            store.append(coord, kind, &payload).expect("append");
        }
    }
}

fn open_store_with_topology(topology: IndexTopology) -> (Store, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_index_topology(topology)
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
            || open_store_with_group_commit(32, 1),
            |(store, _dir, coord, kind)| {
                append_ordinary_events(&store, &coord, kind, 1_000, true, 1);
                store.close().expect("close");
            },
            BatchSize::LargeInput,
        );
    });

    group.bench_function("batch_1_baseline", |b| {
        b.iter_batched(
            || open_store_with_group_commit(1, 1),
            |(store, _dir, coord, kind)| {
                append_ordinary_events(&store, &coord, kind, 1_000, false, 1);
                store.close().expect("close");
            },
            BatchSize::LargeInput,
        );
    });

    group.finish();
}

// ===========================================================================
// GROUP COMMIT SWEEP: find the knee of the queued-drain curve
// ===========================================================================

fn bench_group_commit_batch_sweep(c: &mut Criterion) {
    let mut group = c.benchmark_group("group_commit_batch_sweep");
    apply_profile(&mut group, BenchProfile::Heavy);
    throughput_elements(&mut group, 1_000);

    for batch in [1u32, 2, 4, 8, 16, 32, 64, 0] {
        let label = if batch == 0 {
            "unbounded".to_string()
        } else {
            batch.to_string()
        };
        let use_idempotency = batch != 1;

        group.bench_with_input(BenchmarkId::from_parameter(label), &batch, |b, &batch| {
            b.iter_batched(
                || open_store_with_group_commit(batch, 1),
                |(store, _dir, coord, kind)| {
                    append_ordinary_events(&store, &coord, kind, 1_000, use_idempotency, 1);
                    store.close().expect("close");
                },
                BatchSize::LargeInput,
            );
        });
    }

    group.finish();
}

// ===========================================================================
// SYNC CADENCE SWEEP: separate queue drain from durability cadence
// ===========================================================================

fn bench_group_commit_sync_cadence(c: &mut Criterion) {
    let mut group = c.benchmark_group("group_commit_sync_cadence");
    apply_profile(&mut group, BenchProfile::Heavy);
    throughput_elements(&mut group, 1_000);

    for batch in [1u32, 32] {
        let use_idempotency = batch != 1;
        let batch_label = if batch == 1 { "batch_1" } else { "batch_32" };
        for every_n_events in [1u32, 8, 64, 256, 1_000] {
            group.bench_with_input(
                BenchmarkId::new(batch_label, every_n_events),
                &every_n_events,
                |b, &every_n_events| {
                    b.iter_batched(
                        || open_store_with_group_commit(batch, every_n_events),
                        |(store, _dir, coord, kind)| {
                            append_ordinary_events(&store, &coord, kind, 1_000, use_idempotency, 1);
                            store.close().expect("close");
                        },
                        BatchSize::LargeInput,
                    );
                },
            );
        }
    }

    group.finish();
}

// ===========================================================================
// CONTENDED CALLERS: measure ordinary appends with a truly busy mailbox
// ===========================================================================

fn bench_group_commit_concurrent_callers(c: &mut Criterion) {
    let mut group = c.benchmark_group("group_commit_concurrent_callers");
    apply_profile(&mut group, BenchProfile::Quick);
    throughput_elements(&mut group, 1_000);

    const PRODUCERS: usize = 4;
    const EVENTS_PER_PRODUCER: u32 = 250;

    for batch in [1u32, 32] {
        let use_idempotency = batch != 1;
        let label = if batch == 1 { "batch_1" } else { "batch_32" };

        group.bench_with_input(BenchmarkId::from_parameter(label), &batch, |b, &batch| {
            b.iter_batched(
                || open_store_with_group_commit(batch, 1),
                |(store, _dir, coord, kind)| {
                    let store = Arc::new(store);
                    let barrier = Arc::new(Barrier::new(PRODUCERS));
                    let mut handles = Vec::with_capacity(PRODUCERS);

                    for worker in 0..PRODUCERS {
                        let store = Arc::clone(&store);
                        let barrier = Arc::clone(&barrier);
                        let coord = coord.clone();
                        let handle = std::thread::Builder::new()
                            .name(format!("bench-group-commit-{worker}"))
                            .spawn(move || {
                                barrier.wait();
                                let base = (worker as u128) * u128::from(EVENTS_PER_PRODUCER) + 1;
                                append_ordinary_events(
                                    &store,
                                    &coord,
                                    kind,
                                    EVENTS_PER_PRODUCER,
                                    use_idempotency,
                                    base,
                                );
                            })
                            .expect("spawn benchmark producer thread");
                        handles.push(handle);
                    }

                    for handle in handles {
                        handle.join().expect("producer thread panicked");
                    }

                    let Some(store) = Arc::into_inner(store) else {
                        return;
                    };
                    store.close().expect("close");
                },
                BatchSize::LargeInput,
            );
        });
    }

    group.finish();
}

// ===========================================================================
// INDEX TOPOLOGY: aos vs scan vs tiled by_fact query speed
// ===========================================================================

fn bench_topology_by_fact(c: &mut Criterion) {
    let mut group = c.benchmark_group("topology_by_fact");
    apply_profile(&mut group, BenchProfile::Quick);

    let kind = EventKind::custom(0xF, 1);
    let coord = Coordinate::new("bench:layout", "bench:scope").expect("coord");

    let topologies: Vec<(&str, IndexTopology)> = vec![
        ("aos", IndexTopology::aos()),
        ("scan", IndexTopology::scan()),
        ("tiled", IndexTopology::tiled()),
    ];

    for (name, topology) in &topologies {
        let (store, _dir) = open_store_with_topology(topology.clone());
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
             measurement, got {}. Topology: {name}.",
            warmup.len()
        );

        group.bench_function(*name, |b| {
            b.iter(|| {
                black_box(store.by_fact(kind));
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
    bench_group_commit_batch_sweep,
    bench_group_commit_sync_cadence,
    bench_group_commit_concurrent_callers,
    bench_topology_by_fact,
    bench_incremental_projection
);
criterion_main!(benches);
