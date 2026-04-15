//! Benchmark: batch append throughput vs single append.
//!
//! Measures the overhead reduction from batching multiple events
//! into a single fsync operation.

mod common;

use batpak::prelude::*;
use batpak::store::{BatchAppendItem, CausationRef, Store, StoreConfig, SyncConfig, SyncMode};
use common::{apply_profile, throughput_elements, BenchProfile};
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use tempfile::TempDir;

fn open_bench_store(sync_mode: SyncMode) -> (Store, TempDir, Coordinate, EventKind) {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        sync: SyncConfig {
            every_n_events: 1, // Every batch is a sync
            mode: sync_mode,
        },
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("bench:entity", "bench:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    (store, dir, coord, kind)
}

fn make_batch_items(coord: &Coordinate, kind: EventKind, count: usize) -> Vec<BatchAppendItem> {
    (0..count)
        .map(|i| {
            BatchAppendItem::new(
                coord.clone(),
                kind,
                &serde_json::json!({"i": i, "payload": "x".repeat(100)}),
                AppendOptions::default(),
                CausationRef::None,
            )
            .expect("valid batch item")
        })
        .collect()
}

/// Benchmark: batch append vs single append throughput
fn bench_batch_vs_single(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch_vs_single_append");
    apply_profile(&mut group, BenchProfile::Heavy);

    for batch_size in [10usize, 50, 100, 256] {
        let total_events = 1_000u64;
        throughput_elements(&mut group, total_events);

        // Batch append benchmark
        group.bench_with_input(
            BenchmarkId::new("batch", batch_size),
            &batch_size,
            |b, &batch_size| {
                let batches_needed = usize::try_from(total_events)
                    .expect("total_events fits in usize for benchmark")
                    .div_ceil(batch_size);
                b.iter_batched(
                    || open_bench_store(SyncMode::SyncData),
                    |(store, _dir, coord, kind)| {
                        for batch_idx in 0..batches_needed {
                            let items = make_batch_items(&coord, kind, batch_size);
                            store.append_batch(items).expect("batch append");
                            // Small yield to prevent blocking
                            if batch_idx % 10 == 0 {
                                std::thread::yield_now();
                            }
                        }
                    },
                    BatchSize::SmallInput,
                );
            },
        );

        // Single append benchmark (equivalent work)
        group.bench_with_input(
            BenchmarkId::new("single", batch_size),
            &batch_size,
            |b, &_batch_size| {
                b.iter_batched(
                    || open_bench_store(SyncMode::SyncData),
                    |(store, _dir, coord, kind)| {
                        for i in 0..total_events {
                            store
                                .append(&coord, kind, &serde_json::json!({"i": i}))
                                .expect("single append");
                            if i % 100 == 0 {
                                std::thread::yield_now();
                            }
                        }
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

/// Benchmark: batch durability overhead (SyncAll vs SyncData)
fn bench_batch_durability(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch_durability_overhead");
    apply_profile(&mut group, BenchProfile::Heavy);
    throughput_elements(&mut group, 1_000);

    for sync_mode in [SyncMode::SyncData, SyncMode::SyncAll] {
        let mode_name = if matches!(sync_mode, SyncMode::SyncData) {
            "sync_data"
        } else {
            "sync_all"
        };

        group.bench_with_input(
            BenchmarkId::new(mode_name, 100),
            &sync_mode,
            |b, sync_mode| {
                b.iter_batched(
                    || open_bench_store(sync_mode.clone()),
                    |(store, _dir, coord, kind)| {
                        // 10 batches of 100 = 1000 events
                        for _ in 0..10 {
                            let items = make_batch_items(&coord, kind, 100);
                            store.append_batch(items).expect("batch append");
                        }
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

/// Benchmark: batch size scaling (latency vs throughput tradeoff)
fn bench_batch_size_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch_size_scaling");
    apply_profile(&mut group, BenchProfile::Quick);

    // Fixed total events, varying batch size
    let total_events = 1_000u64;

    for batch_size in [1usize, 10, 50, 100, 256] {
        throughput_elements(&mut group, total_events);

        group.bench_with_input(
            BenchmarkId::from_parameter(batch_size),
            &batch_size,
            |b, &batch_size| {
                let batches_needed = usize::try_from(total_events)
                    .expect("total_events fits in usize for benchmark")
                    .div_ceil(batch_size);
                b.iter_batched(
                    || open_bench_store(SyncMode::SyncData),
                    |(store, _dir, coord, kind)| {
                        for _ in 0..batches_needed {
                            let items = make_batch_items(&coord, kind, batch_size);
                            store.append_batch(items).expect("batch append");
                        }
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

/// Benchmark: batch with cross-entity causation
fn bench_batch_causation(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch_with_causation");
    apply_profile(&mut group, BenchProfile::Heavy);
    throughput_elements(&mut group, 500);

    group.bench_function("causation_chain", |b| {
        b.iter_batched(
            || open_bench_store(SyncMode::SyncData),
            |(store, _dir, coord, kind)| {
                // Create batch with intra-batch causation
                let items: Vec<_> = (0..50)
                    .map(|i| {
                        let causation = if i == 0 {
                            CausationRef::None
                        } else {
                            CausationRef::PriorItem(i - 1)
                        };
                        BatchAppendItem::new(
                            coord.clone(),
                            kind,
                            &serde_json::json!({"seq": i}),
                            AppendOptions::default(),
                            causation,
                        )
                        .expect("valid item")
                    })
                    .collect();
                store.append_batch(items).expect("batch with causation");
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

/// Benchmark: batch recovery after simulated crash
#[cfg(feature = "dangerous-test-hooks")]
fn bench_batch_recovery(c: &mut Criterion) {
    use batpak::store::CountdownInjector;

    let mut group = c.benchmark_group("batch_recovery");
    apply_profile(&mut group, BenchProfile::Heavy);

    group.bench_function("reopen_after_incomplete_batch", |b| {
        b.iter_batched(
            || {
                // Setup: create store with incomplete batch
                let dir = TempDir::new().expect("temp dir");

                // Write some committed events
                {
                    let config = StoreConfig::new(dir.path());
                    let store =
                        Store::open(config).expect("open store for recovery benchmark baseline");
                    store
                        .append(
                            &Coordinate::new("test", "test")
                                .expect("valid recovery benchmark coordinate"),
                            EventKind::DATA,
                            &serde_json::json!({"committed": true}),
                        )
                        .expect("append committed baseline event for recovery benchmark");
                    store
                        .close()
                        .expect("close baseline store for recovery benchmark");
                }

                // Inject crash during batch
                {
                    let mut config = StoreConfig::new(dir.path());
                    config.fault_injector =
                        Some(std::sync::Arc::new(CountdownInjector::after_batch_items(2)));
                    let store =
                        Store::open(config).expect("open fault-injected recovery benchmark store");
                    let items = make_batch_items(
                        &Coordinate::new("test", "test").expect("valid crash benchmark coordinate"),
                        EventKind::DATA,
                        5,
                    );
                    let result = store.append_batch(items);
                    assert!(
                        result.is_err(),
                        "fault-injected recovery benchmark batch should fail"
                    );
                }

                dir
            },
            |dir| {
                // Benchmark: recovery time
                let config = StoreConfig::new(dir.path());
                let store = Store::open(config).expect("recover from incomplete batch");
                store
                    .close()
                    .expect("close recovered store in recovery benchmark");
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

#[cfg(not(feature = "dangerous-test-hooks"))]
fn bench_batch_recovery(_c: &mut Criterion) {
    // Recovery benchmark requires dangerous-test-hooks for fault injection
}

criterion_group!(
    benches,
    bench_batch_vs_single,
    bench_batch_durability,
    bench_batch_size_scaling,
    bench_batch_causation,
    bench_batch_recovery
);
criterion_main!(benches);
