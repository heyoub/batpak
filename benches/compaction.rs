//! Benchmark: segment compaction (merge small segments into one).
//!
//! [SPEC:benches/compaction.rs]

mod common;

use batpak::prelude::*;
use batpak::store::{CompactionConfig, Store, StoreConfig};
use common::{apply_profile, throughput_elements, BenchProfile};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use tempfile::TempDir;

fn populate_small_segments(store: &Store, coord: &Coordinate, segments: u64, kind: EventKind) {
    for seg in 0..segments {
        for i in 0..100u64 {
            store
                .append(coord, kind, &serde_json::json!({"seg": seg, "i": i}))
                .expect("append");
        }
    }
    store.sync().expect("sync");
}

fn bench_compaction(c: &mut Criterion) {
    let mut group = c.benchmark_group("compaction");

    for segments in [10u64, 50, 100] {
        let profile = if segments == 10 {
            BenchProfile::Heavy
        } else {
            BenchProfile::Massive
        };
        apply_profile(&mut group, profile);
        throughput_elements(&mut group, segments * 100);
        group.bench_with_input(
            BenchmarkId::new("merge_segments", segments),
            &segments,
            |b, &segments| {
                // `iter_batched` (BatchSize::SmallInput) replaces the
                // deprecated `iter_with_setup` and avoids accumulating
                // setup cost in the measurement.
                b.iter_batched(
                    || {
                        let dir = TempDir::new().expect("create temp dir");
                        let config = StoreConfig {
                            data_dir: dir.path().to_path_buf(),
                            segment_max_bytes: 1024,
                            ..StoreConfig::new("")
                        };
                        let store = Store::open(config).expect("open store");
                        let coord =
                            Coordinate::new("compact:entity", "compact:scope").expect("coord");
                        let kind = EventKind::custom(0xF, 1);
                        populate_small_segments(&store, &coord, segments, kind);
                        (store, dir)
                    },
                    |(store, _dir)| {
                        store
                            .compact(&CompactionConfig::default())
                            .expect("compact");
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_compaction);
criterion_main!(benches);
