//! Benchmark: compaction time and bytes reclaimed for sealed segments.
//! [SPEC:benches/compaction.rs]

use batpak::prelude::*;
use batpak::store::{Store, StoreConfig};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use tempfile::TempDir;

fn populate_segments(dir: &std::path::Path, events_per_segment: u64, segment_count: u64) -> Store {
    let config = StoreConfig {
        data_dir: dir.to_path_buf(),
        segment_max_bytes: 2048, // small to force rotation
        sync_every_n_events: 1,
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store for populate");
    let coord = Coordinate::new("bench:compact", "bench:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let total = events_per_segment * segment_count;
    for i in 0..total {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }
    store.sync().expect("sync");
    store
}

fn bench_compaction(c: &mut Criterion) {
    let mut group = c.benchmark_group("compaction");
    group.sample_size(10);

    for segment_count in [10u64, 50, 100] {
        group.bench_with_input(
            BenchmarkId::new("merge_segments", segment_count),
            &segment_count,
            |b, &seg_count| {
                b.iter_with_setup(
                    || {
                        let dir = TempDir::new().expect("temp dir");
                        let store = populate_segments(dir.path(), 20, seg_count);
                        (store, dir)
                    },
                    |(store, _dir)| {
                        let config = CompactionConfig::default();
                        let result = store.compact(&config).expect("compact");
                        result
                    },
                );
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_compaction);
criterion_main!(benches);
