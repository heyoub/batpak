//! Benchmark: compaction time and bytes reclaimed for sealed segments.
//!
//! Uses realistic parameters to measure actual compaction overhead:
//! - segment_max_bytes: 64KB (small enough to force rotation in a bench,
//!   large enough to reflect real I/O patterns — not 2KB)
//! - sync_every_n_events: 100 (production-like, not 1-per-event)
//! - Payload: ~200 bytes (realistic event size, not 15 bytes)
//!
//! [SPEC:benches/compaction.rs]

use batpak::prelude::*;
use batpak::store::{Store, StoreConfig};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use tempfile::TempDir;

fn populate_segments(dir: &std::path::Path, events_per_segment: u64, segment_count: u64) -> Store {
    let config = StoreConfig {
        data_dir: dir.to_path_buf(),
        segment_max_bytes: 64 * 1024, // 64KB — realistic rotation threshold for benchmarks
        sync_every_n_events: 100,     // production-like batched sync
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store for populate");
    let coord = Coordinate::new("bench:compact", "bench:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let total = events_per_segment * segment_count;
    // Realistic payload: ~200 bytes mimicking a domain event
    let payload = serde_json::json!({
        "action": "transfer",
        "from_account": "acc-12345678",
        "to_account": "acc-87654321",
        "amount_cents": 42_00,
        "currency": "USD",
        "memo": "Monthly payment for services rendered"
    });
    for _i in 0..total {
        store.append(&coord, kind, &payload).expect("append");
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
                        // ~100 events per 64KB segment with ~200-byte payloads
                        let store = populate_segments(dir.path(), 100, seg_count);
                        (store, dir)
                    },
                    |(store, _dir)| {
                        let config = CompactionConfig::default();
                        store.compact(&config).expect("compact")
                    },
                );
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_compaction);
criterion_main!(benches);
