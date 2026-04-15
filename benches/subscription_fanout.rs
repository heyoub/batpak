//! Benchmark: subscriber fanout under workload and micro-cost shapes.
//!

mod common;

use batpak::prelude::*;
use batpak::store::{Store, StoreConfig};
use common::{apply_profile, throughput_elements, BenchProfile};
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use tempfile::TempDir;

const FANOUT_WORKLOAD_EVENTS: u64 = 10_000;

fn bench_fanout_workload_append(c: &mut Criterion) {
    let mut group = c.benchmark_group("fanout_workload_append_with_subscribers");
    apply_profile(&mut group, BenchProfile::Heavy);

    for subscribers in [1usize, 10, 100] {
        throughput_elements(&mut group, FANOUT_WORKLOAD_EVENTS);
        group.bench_with_input(
            BenchmarkId::new("subscribers", subscribers),
            &subscribers,
            |b, &subscribers| {
                b.iter_batched(
                    || {
                        let dir = TempDir::new().expect("create temp dir");
                        let config = StoreConfig {
                            data_dir: dir.path().to_path_buf(),
                            broadcast_capacity: 20_000,
                            ..StoreConfig::new("")
                        };
                        let store = Store::open(config).expect("open store");
                        let region = Region::entity("fanout:entity");
                        for _ in 0..subscribers {
                            let _ = store.subscribe_lossy(&region);
                        }
                        let coord =
                            Coordinate::new("fanout:entity", "fanout:scope").expect("valid");
                        let kind = EventKind::custom(0xF, 1);
                        (store, dir, coord, kind)
                    },
                    |(store, dir, coord, kind)| {
                        for i in 0..FANOUT_WORKLOAD_EVENTS {
                            store
                                .append(&coord, kind, &serde_json::json!({"i": i}))
                                .expect("append");
                        }
                        (store, dir)
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

fn bench_fanout_workload_drain(c: &mut Criterion) {
    let mut group = c.benchmark_group("fanout_workload_drain_notifications");
    apply_profile(&mut group, BenchProfile::Heavy);

    for subscribers in [1usize, 10, 100] {
        throughput_elements(&mut group, FANOUT_WORKLOAD_EVENTS * subscribers as u64);
        group.bench_with_input(
            BenchmarkId::new("subscribers", subscribers),
            &subscribers,
            |b, &subscribers| {
                b.iter_batched(
                    || {
                        let dir = TempDir::new().expect("create temp dir");
                        let config = StoreConfig {
                            data_dir: dir.path().to_path_buf(),
                            broadcast_capacity: 20_000,
                            ..StoreConfig::new("")
                        };
                        let store = Store::open(config).expect("open store");
                        let coord =
                            Coordinate::new("fanout:entity", "fanout:scope").expect("valid");
                        let kind = EventKind::custom(0xF, 1);
                        let region = Region::entity("fanout:entity");
                        let mut receivers = Vec::new();
                        for _ in 0..subscribers {
                            receivers.push(store.subscribe_lossy(&region));
                        }
                        for i in 0..FANOUT_WORKLOAD_EVENTS {
                            store
                                .append(&coord, kind, &serde_json::json!({"i": i}))
                                .expect("append");
                        }
                        store.sync().expect("sync");
                        (store, dir, receivers)
                    },
                    |(store, dir, receivers)| {
                        for rx in receivers {
                            let mut seen = 0u64;
                            while seen < FANOUT_WORKLOAD_EVENTS {
                                if rx.recv().is_some() {
                                    seen += 1;
                                } else {
                                    break;
                                }
                            }
                        }
                        (store, dir)
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

fn bench_fanout_micro_append(c: &mut Criterion) {
    let mut group = c.benchmark_group("fanout_micro_append_one");
    apply_profile(&mut group, BenchProfile::QuickWarm);

    for subscribers in [1usize, 10, 100] {
        throughput_elements(&mut group, 1);
        let dir = TempDir::new().expect("create temp dir");
        let config = StoreConfig {
            data_dir: dir.path().to_path_buf(),
            broadcast_capacity: 20_000,
            ..StoreConfig::new("")
        };
        let store = Store::open(config).expect("open store");
        let region = Region::entity("fanout:entity");
        for _ in 0..subscribers {
            let _ = store.subscribe_lossy(&region);
        }
        let coord = Coordinate::new("fanout:entity", "fanout:scope").expect("valid");
        let kind = EventKind::custom(0xF, 1);
        let mut next_i = 0u64;

        group.bench_with_input(
            BenchmarkId::new("subscribers", subscribers),
            &subscribers,
            |b, &_subscribers| {
                b.iter(|| {
                    store
                        .append(&coord, kind, &serde_json::json!({"i": next_i}))
                        .expect("append");
                    next_i += 1;
                });
            },
        );

        store.close().expect("close");
    }

    group.finish();
}

fn bench_fanout_micro_drain(c: &mut Criterion) {
    let mut group = c.benchmark_group("fanout_micro_drain_one");
    apply_profile(&mut group, BenchProfile::QuickWarm);

    for subscribers in [1usize, 10, 100] {
        throughput_elements(&mut group, subscribers as u64);
        let dir = TempDir::new().expect("create temp dir");
        let config = StoreConfig {
            data_dir: dir.path().to_path_buf(),
            broadcast_capacity: 20_000,
            ..StoreConfig::new("")
        };
        let store = Store::open(config).expect("open store");
        let region = Region::entity("fanout:entity");
        let mut receivers = Vec::new();
        for _ in 0..subscribers {
            receivers.push(store.subscribe_lossy(&region));
        }
        let coord = Coordinate::new("fanout:entity", "fanout:scope").expect("valid");
        let kind = EventKind::custom(0xF, 1);
        let mut next_i = 0u64;

        group.bench_with_input(
            BenchmarkId::new("subscribers", subscribers),
            &subscribers,
            |b, &_subscribers| {
                b.iter(|| {
                    store
                        .append(&coord, kind, &serde_json::json!({"i": next_i}))
                        .expect("append");
                    next_i += 1;
                    for rx in &receivers {
                        let _ = rx.recv().expect("one notification per append");
                    }
                });
            },
        );

        store.close().expect("close");
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_fanout_workload_append,
    bench_fanout_workload_drain,
    bench_fanout_micro_append,
    bench_fanout_micro_drain
);
criterion_main!(benches);
