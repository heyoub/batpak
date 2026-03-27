//! Benchmark: index rebuild latency for 1K/10K/100K/1M events.
//!
//! [SPEC:benches/cold_start.rs]
//!
//! SPEC line 259: "criterion: index rebuild for 1K/10K/100K/1M events"
//! The 1M case uses a reduced sample_size to keep bench runtime manageable.

use batpak::prelude::*;
use batpak::store::{Store, StoreConfig};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use tempfile::TempDir;

fn populate_store(dir: &std::path::Path, count: u64) {
    let config = StoreConfig {
        data_dir: dir.to_path_buf(),
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store for populate");
    let coord = Coordinate::new("bench:entity", "bench:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let payload = serde_json::json!({"x": 1});
    for _ in 0..count {
        store.append(&coord, kind, &payload).expect("append");
    }
    store.sync().expect("sync");
    store.close().expect("close");
}

fn bench_cold_start(c: &mut Criterion) {
    let mut group = c.benchmark_group("cold_start");

    for count in [1_000u64, 10_000, 100_000, 1_000_000] {
        // Reduce sample size for large event counts to keep total bench time bounded.
        // 1M events × 100 samples = minutes of wall time; 10 samples is sufficient
        // for detecting regressions at this scale.
        if count >= 1_000_000 {
            group.sample_size(10);
        }

        // Pre-populate a temp dir with events
        let dir = TempDir::new().expect("create temp dir");
        populate_store(dir.path(), count);

        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &_count| {
            b.iter(|| {
                // Cold start: open the store (triggers index rebuild from segments)
                let config = StoreConfig {
                    data_dir: dir.path().to_path_buf(),
                    ..StoreConfig::new("")
                };
                let store = Store::open(config).expect("cold start open");
                store.close().expect("close");
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_cold_start);
criterion_main!(benches);
