//! Benchmark: events/sec for 1K/10K/100K appends (single + concurrent).
//! [SPEC:benches/write_throughput.rs]

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use free_batteries::prelude::*;
use free_batteries::store::{Store, StoreConfig};
use std::sync::Arc;
use tempfile::TempDir;

fn setup_store() -> (Store, TempDir) {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    (store, dir)
}

fn bench_write_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("write_throughput");

    for count in [1_000u64, 10_000, 100_000] {
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            b.iter_with_setup(
                || setup_store(),
                |(store, _dir)| {
                    let coord =
                        Coordinate::new("bench:entity", "bench:scope").expect("valid coord");
                    let kind = EventKind::custom(0xF, 1);
                    let payload = serde_json::json!({"x": 1, "y": 2});
                    for _ in 0..count {
                        store.append(&coord, kind, &payload).expect("append");
                    }
                    store.close().expect("close");
                },
            );
        });
    }

    group.finish();
}

/// Concurrent write throughput: N threads appending simultaneously.
/// Mirrors the pattern from tests/store_integration.rs::concurrent_append_and_query
/// but measures throughput under criterion's statistical framework.
fn bench_concurrent_write_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_write_throughput");

    // 4 threads × 250 events each = 1000 total events per iteration
    let thread_count = 4usize;
    let events_per_thread = 250u64;

    group.bench_function("4_threads_x_250", |b| {
        b.iter_with_setup(
            || {
                let (store, dir) = setup_store();
                (Arc::new(store), dir)
            },
            |(store, _dir)| {
                let mut handles = Vec::with_capacity(thread_count);
                for t in 0..thread_count {
                    let store = Arc::clone(&store);
                    handles.push(std::thread::spawn(move || {
                        let entity = format!("bench:thread{t}");
                        let coord = Coordinate::new(&entity, "bench:scope").expect("valid coord");
                        let kind = EventKind::custom(0xF, 1);
                        let payload = serde_json::json!({"t": t});
                        for _ in 0..events_per_thread {
                            store.append(&coord, kind, &payload).expect("append");
                        }
                    }));
                }
                for h in handles {
                    h.join().expect("thread join");
                }
                // Use Arc::try_unwrap to get ownership for close()
                if let Ok(store) = Arc::try_unwrap(store) {
                    store.close().expect("close");
                }
            },
        );
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_write_throughput,
    bench_concurrent_write_throughput
);
criterion_main!(benches);
