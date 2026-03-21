//! Benchmark: events/sec for 1K/10K/100K appends.
//! [SPEC:benches/write_throughput.rs]

use criterion::{criterion_group, criterion_main, Criterion, BenchmarkId};
use free_batteries::prelude::*;
use free_batteries::store::{Store, StoreConfig};
use tempfile::TempDir;

fn setup_store() -> (Store, TempDir) {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        ..StoreConfig::default()
    };
    let store = Store::open(config).expect("open store");
    (store, dir)
}

fn bench_write_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("write_throughput");

    for count in [1_000u64, 10_000, 100_000] {
        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &count,
            |b, &count| {
                b.iter_with_setup(
                    || setup_store(),
                    |(store, _dir)| {
                        let coord = Coordinate::new("bench:entity", "bench:scope")
                            .expect("valid coord");
                        let kind = EventKind::custom(0xF, 1);
                        let payload = serde_json::json!({"x": 1, "y": 2});
                        for _ in 0..count {
                            store.append(&coord, kind, &payload).expect("append");
                        }
                        store.close().expect("close");
                    },
                );
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_write_throughput);
criterion_main!(benches);
