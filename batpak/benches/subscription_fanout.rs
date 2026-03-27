//! Benchmark: subscription fan-out latency with varying subscriber counts.
//! Measures delivery latency for 1/10/100 subscribers receiving 10K events.
//! [SPEC:benches/subscription_fanout.rs]

use batpak::prelude::*;
use batpak::store::{Store, StoreConfig};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::sync::Arc;
use tempfile::TempDir;

fn setup_store() -> (Arc<Store>, TempDir) {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        broadcast_capacity: 16384,
        ..StoreConfig::new("")
    };
    let store = Arc::new(Store::open(config).expect("open store"));
    (store, dir)
}

fn bench_fanout(c: &mut Criterion) {
    let mut group = c.benchmark_group("subscription_fanout");
    group.sample_size(10);

    let event_count = 10_000u64;

    for sub_count in [1usize, 10, 100] {
        group.bench_with_input(
            BenchmarkId::new("subscribers", sub_count),
            &sub_count,
            |b, &n_subs| {
                b.iter_with_setup(
                    || {
                        let (store, dir) = setup_store();
                        let region = Region::entity("bench:fan");
                        let subs: Vec<_> = (0..n_subs).map(|_| store.subscribe(&region)).collect();
                        (store, dir, subs)
                    },
                    |(store, _dir, subs)| {
                        let coord =
                            Coordinate::new("bench:fan", "bench:scope").expect("valid coord");
                        let kind = EventKind::custom(0xF, 1);
                        let payload = serde_json::json!({"x": 1});

                        // Append events — subscribers receive via broadcast
                        for _ in 0..event_count {
                            store.append(&coord, kind, &payload).expect("append");
                        }

                        // Drain all subscriber channels
                        for sub in &subs {
                            let rx = sub.receiver();
                            while rx.try_recv().is_ok() {}
                        }
                    },
                );
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_fanout);
criterion_main!(benches);
