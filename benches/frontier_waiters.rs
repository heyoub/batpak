//! Benchmark frontier wake-all fanout costs for wait APIs and append gates.

use batpak::prelude::*;
use batpak::store::{Store, StoreConfig, SyncMode};
use batpak_bench_support::{apply_profile, BenchProfile};
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use std::sync::{Arc, Barrier};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const WAITER_COUNTS: &[usize] = &[1, 8, 32, 128, 512];
const WAIT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
enum TargetDistribution {
    Same,
    Spread,
}

impl TargetDistribution {
    fn label(self) -> &'static str {
        match self {
            Self::Same => "same-target",
            Self::Spread => "spread-targets",
        }
    }

    fn targets(self, store: &Store, waiters: usize) -> Vec<HlcPoint> {
        match self {
            Self::Same => vec![future_point(store); waiters],
            Self::Spread => (1..=waiters)
                .map(|offset| point_after(store, offset))
                .collect(),
        }
    }
}

const TARGET_DISTRIBUTIONS: &[TargetDistribution] =
    &[TargetDistribution::Same, TargetDistribution::Spread];

fn open_store() -> (Arc<Store>, TempDir, Coordinate, EventKind) {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_sync_every_n_events(10_000)
            .with_sync_mode(SyncMode::SyncData),
    )
    .expect("open store");
    let coord = Coordinate::new("bench:frontier", "bench:waiters").expect("coord");
    let kind = EventKind::custom(0xF, 1);
    (Arc::new(store), dir, coord, kind)
}

fn future_point(store: &Store) -> HlcPoint {
    point_after(store, 1)
}

fn point_after(store: &Store, sequence_offset: usize) -> HlcPoint {
    let snapshot = store.dangerous_watermark_snapshot();
    HlcPoint {
        wall_ms: snapshot.accepted_hlc.wall_ms,
        global_sequence: snapshot
            .accepted_hlc
            .global_sequence
            .saturating_add(u64::try_from(sequence_offset).expect("waiter count fits u64")),
    }
}

fn append_one(store: &Store, coord: &Coordinate, kind: EventKind, i: usize) {
    store
        .append(coord, kind, &serde_json::json!({ "i": i }))
        .expect("append benchmark event");
}

fn join_store(store: Arc<Store>) {
    if let Ok(store) = Arc::try_unwrap(store) {
        store.close().expect("close store");
    }
}

fn spawn_named<T, F>(name: String, f: F) -> JoinHandle<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    std::thread::Builder::new()
        .name(name)
        .spawn(f)
        .expect("spawn benchmark thread")
}

fn bench_waiters(c: &mut Criterion) {
    let mut group = c.benchmark_group("frontier_waiter_wake_all");
    apply_profile(&mut group, BenchProfile::Quick);

    for &distribution in TARGET_DISTRIBUTIONS {
        for &waiters in WAITER_COUNTS {
            group.bench_with_input(
                BenchmarkId::new(format!("durable/{}", distribution.label()), waiters),
                &waiters,
                |b, &waiters| {
                    b.iter_batched(
                        open_store,
                        |(store, _dir, coord, kind)| {
                            let targets = distribution.targets(&store, waiters);
                            let barrier = Arc::new(Barrier::new(waiters + 1));
                            let mut handles = Vec::with_capacity(waiters);
                            for target in targets {
                                let store = Arc::clone(&store);
                                let barrier = Arc::clone(&barrier);
                                handles.push(spawn_named(
                                    format!("durable-waiter-{waiters}"),
                                    move || {
                                        barrier.wait();
                                        store
                                            .wait_for_durable(target, WAIT_TIMEOUT)
                                            .expect("durable waiter wakes")
                                    },
                                ));
                            }
                            barrier.wait();
                            std::thread::sleep(Duration::from_millis(2));
                            let appends = match distribution {
                                TargetDistribution::Same => 1,
                                TargetDistribution::Spread => waiters,
                            };
                            for i in 0..appends {
                                append_one(&store, &coord, kind, i);
                            }
                            let writer_start = Instant::now();
                            store.sync().expect("sync benchmark store");
                            let writer_elapsed = writer_start.elapsed();
                            for handle in handles {
                                handle.join().expect("join waiter");
                            }
                            std::hint::black_box(writer_elapsed);
                            join_store(store);
                        },
                        BatchSize::SmallInput,
                    );
                },
            );

            group.bench_with_input(
                BenchmarkId::new(format!("visible/{}", distribution.label()), waiters),
                &waiters,
                |b, &waiters| {
                    b.iter_batched(
                        open_store,
                        |(store, _dir, coord, kind)| {
                            let targets = distribution.targets(&store, waiters);
                            let barrier = Arc::new(Barrier::new(waiters + 1));
                            let mut handles = Vec::with_capacity(waiters);
                            for target in targets {
                                let store = Arc::clone(&store);
                                let barrier = Arc::clone(&barrier);
                                handles.push(spawn_named(
                                    format!("visible-waiter-{waiters}"),
                                    move || {
                                        barrier.wait();
                                        store
                                            .wait_for_visible(target, WAIT_TIMEOUT)
                                            .expect("visible waiter wakes")
                                    },
                                ));
                            }
                            barrier.wait();
                            std::thread::sleep(Duration::from_millis(2));
                            let writer_start = Instant::now();
                            let appends = match distribution {
                                TargetDistribution::Same => 1,
                                TargetDistribution::Spread => waiters,
                            };
                            for i in 0..appends {
                                append_one(&store, &coord, kind, i);
                            }
                            let writer_elapsed = writer_start.elapsed();
                            for handle in handles {
                                handle.join().expect("join waiter");
                            }
                            std::hint::black_box(writer_elapsed);
                            join_store(store);
                        },
                        BatchSize::SmallInput,
                    );
                },
            );

            group.bench_with_input(
                BenchmarkId::new(format!("applied/{}", distribution.label()), waiters),
                &waiters,
                |b, &waiters| {
                    b.iter_batched(
                        open_store,
                        |(store, _dir, coord, kind)| {
                            store.dangerous_register_projection("bench-frontier-waiter");
                            let targets = distribution.targets(&store, waiters);
                            let barrier = Arc::new(Barrier::new(waiters + 1));
                            let mut handles = Vec::with_capacity(waiters);
                            for target in targets.iter().copied() {
                                let store = Arc::clone(&store);
                                let barrier = Arc::clone(&barrier);
                                handles.push(spawn_named(
                                    format!("applied-waiter-{waiters}"),
                                    move || {
                                        barrier.wait();
                                        store
                                            .wait_for_applied(target, WAIT_TIMEOUT)
                                            .expect("applied waiter wakes")
                                    },
                                ));
                            }
                            barrier.wait();
                            std::thread::sleep(Duration::from_millis(2));
                            let appends = match distribution {
                                TargetDistribution::Same => 1,
                                TargetDistribution::Spread => waiters,
                            };
                            for i in 0..appends {
                                append_one(&store, &coord, kind, i);
                            }
                            let writer_start = Instant::now();
                            match distribution {
                                TargetDistribution::Same => store
                                    .dangerous_notify_projection_applied(
                                        "bench-frontier-waiter",
                                        targets[0],
                                    ),
                                TargetDistribution::Spread => {
                                    for target in targets {
                                        store.dangerous_notify_projection_applied(
                                            "bench-frontier-waiter",
                                            target,
                                        );
                                    }
                                }
                            }
                            let writer_elapsed = writer_start.elapsed();
                            for handle in handles {
                                handle.join().expect("join waiter");
                            }
                            std::hint::black_box(writer_elapsed);
                            join_store(store);
                        },
                        BatchSize::SmallInput,
                    );
                },
            );
        }
    }

    group.finish();
}

fn bench_append_gate_waiters(c: &mut Criterion) {
    let mut group = c.benchmark_group("frontier_append_gate_wake_all");
    apply_profile(&mut group, BenchProfile::Quick);

    for &distribution in TARGET_DISTRIBUTIONS {
        for &waiters in WAITER_COUNTS {
            group.bench_with_input(
                BenchmarkId::new(format!("durable_gate/{}", distribution.label()), waiters),
                &waiters,
                |b, &waiters| {
                    b.iter_batched(
                        open_store,
                        |(store, _dir, coord, kind)| {
                            let barrier = Arc::new(Barrier::new(waiters + 1));
                            let mut handles = Vec::with_capacity(waiters);
                            for i in 0..waiters {
                                let store = Arc::clone(&store);
                                let coord = coord.clone();
                                let barrier = Arc::clone(&barrier);
                                handles.push(spawn_named(format!("durable-gate-{i}"), move || {
                                    barrier.wait();
                                    if matches!(distribution, TargetDistribution::Spread) {
                                        std::thread::sleep(Duration::from_micros((i as u64) * 25));
                                    }
                                    store
                                        .append_with_options(
                                            &coord,
                                            kind,
                                            &serde_json::json!({ "i": i }),
                                            AppendOptions::new().with_gate(DurabilityGate {
                                                kind: WatermarkKind::Durable,
                                                timeout: WAIT_TIMEOUT,
                                            }),
                                        )
                                        .expect("durable gate append wakes")
                                }));
                            }
                            barrier.wait();
                            std::thread::sleep(Duration::from_millis(2));
                            let writer_start = Instant::now();
                            store.sync().expect("sync gated append benchmark store");
                            let writer_elapsed = writer_start.elapsed();
                            for handle in handles {
                                let receipt = handle.join().expect("join gated append");
                                std::hint::black_box(receipt.event_id);
                            }
                            std::hint::black_box(writer_elapsed);
                            join_store(store);
                        },
                        BatchSize::SmallInput,
                    );
                },
            );
        }
    }

    group.finish();
}

criterion_group!(benches, bench_waiters, bench_append_gate_waiters);
criterion_main!(benches);
