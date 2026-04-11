//! Benchmark: append throughput (single-threaded, durable, concurrent, sync modes).
//!
//! [SPEC:benches/write_throughput.rs]

mod common;

use batpak::prelude::*;
use batpak::store::{Store, StoreConfig, SyncConfig, SyncMode};
use common::{apply_profile, profile_for_event_count, throughput_elements, BenchProfile};
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use std::sync::Arc;
use tempfile::TempDir;

fn saturating_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn open_bench_store(
    sync_every_n_events: u32,
    sync_mode: SyncMode,
) -> (Store, TempDir, Coordinate, EventKind) {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        sync: SyncConfig {
            every_n_events: sync_every_n_events,
            mode: sync_mode,
        },
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("bench:entity", "bench:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    (store, dir, coord, kind)
}

fn bench_write_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("write_throughput");

    for count in [1_000u64, 10_000, 100_000] {
        apply_profile(&mut group, profile_for_event_count(count));
        throughput_elements(&mut group, count);

        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            b.iter_batched(
                || open_bench_store(saturating_u32(count).saturating_add(1), SyncMode::default()),
                |(store, dir, coord, kind)| {
                    for i in 0..count {
                        store
                            .append(&coord, kind, &serde_json::json!({"i": i}))
                            .expect("append");
                    }
                    (store, dir)
                },
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();

    let mut durable = c.benchmark_group("durable_write_throughput");
    for count in [1_000u64, 10_000] {
        let profile = if count >= 10_000 {
            BenchProfile::Massive
        } else {
            BenchProfile::Heavy
        };
        apply_profile(&mut durable, profile);
        throughput_elements(&mut durable, count);
        durable.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            b.iter_batched(
                || open_bench_store(1, SyncMode::default()),
                |(store, dir, coord, kind)| {
                    for i in 0..count {
                        store
                            .append(&coord, kind, &serde_json::json!({"i": i}))
                            .expect("append");
                    }
                    (store, dir)
                },
                BatchSize::SmallInput,
            );
        });
    }
    durable.finish();
}

fn bench_concurrent_write_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_write_throughput");
    apply_profile(&mut group, BenchProfile::Quick);
    let thread_count = 4usize;
    let events_per_thread = 250u64;
    throughput_elements(&mut group, thread_count as u64 * events_per_thread);

    group.bench_function(
        BenchmarkId::new(
            "4_threads_x_250",
            format!("{thread_count}_threads_x_{events_per_thread}"),
        ),
        |b| {
            let thread_count_u64 = u64::try_from(thread_count).unwrap_or(u64::MAX);
            b.iter_batched(
                || {
                    let (store, dir, _coord, kind) = open_bench_store(
                        saturating_u32(thread_count_u64.saturating_mul(events_per_thread))
                            .saturating_add(1),
                        SyncMode::default(),
                    );
                    (Arc::new(store), dir, kind)
                },
                |(store, _dir, kind)| {
                    let mut handles = Vec::new();
                    for t in 0..thread_count {
                        let store = Arc::clone(&store);
                        let coord = Coordinate::new(format!("bench:entity:{t}"), "bench:scope")
                            .expect("valid coord");
                        handles.push(
                            std::thread::Builder::new()
                                .name(format!("bench-writer-{t}"))
                                .spawn(move || {
                                    for i in 0..events_per_thread {
                                        store
                                            .append(&coord, kind, &serde_json::json!({"i": i}))
                                            .expect("append");
                                    }
                                })
                                .expect("spawn thread"),
                        );
                    }
                    for handle in handles {
                        handle.join().expect("join thread");
                    }
                    store
                },
                BatchSize::SmallInput,
            );
        },
    );

    group.finish();
}

fn bench_sync_mode_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("sync_mode_comparison");
    apply_profile(&mut group, BenchProfile::Heavy);
    let count = 1_000u64;
    throughput_elements(&mut group, count);

    for (name, sync_mode) in [
        ("sync_all", SyncMode::SyncAll),
        ("sync_data", SyncMode::SyncData),
    ] {
        let profile = if name == "sync_all" {
            BenchProfile::Massive
        } else {
            BenchProfile::Heavy
        };
        apply_profile(&mut group, profile);
        group.bench_function(name, |b| {
            b.iter_batched(
                || open_bench_store(1, sync_mode.clone()),
                |(store, dir, coord, kind)| {
                    for i in 0..count {
                        store
                            .append(&coord, kind, &serde_json::json!({"i": i}))
                            .expect("append");
                    }
                    (store, dir)
                },
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_write_throughput,
    bench_concurrent_write_throughput,
    bench_sync_mode_comparison
);
criterion_main!(benches);
