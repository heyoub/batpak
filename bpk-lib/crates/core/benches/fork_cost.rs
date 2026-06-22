//! Benchmark: Store::fork() cost across source depth.
//!
//! The CoW arm uses default fork sharing (reflink/hardlink when available).
//! The deep-copy baseline disables both sharing rungs so sealed segments are
//! copied byte-for-byte. Each iteration forks into a fresh temp directory and
//! does not open the fork.

use batpak::prelude::*;
use batpak::store::{CopyPreference, ForkOptions, Store, StoreConfig, SyncMode};
use batpak_bench_support::{apply_profile, throughput_elements, BenchProfile};
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use std::hint::black_box;
use tempfile::TempDir;

const KIND: EventKind = EventKind::custom(0xF, 0x90);
const DEPTHS: &[u64] = &[10, 50, 100, 500];

fn build_fixture(depth: u64) -> (Store, TempDir) {
    let dir = TempDir::new().expect("create source temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_segment_max_bytes(512)
            .with_sync_every_n_events(1)
            .with_sync_mode(SyncMode::SyncData)
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )
    .expect("open source store");
    let coord = Coordinate::new("bench:fork:entity", "bench:fork").expect("valid coordinate");
    let blob = "x".repeat(300);
    for i in 0..depth {
        store
            .append(&coord, KIND, &serde_json::json!({"i": i, "blob": blob}))
            .expect("append fixture event");
    }
    store.sync().expect("sync fixture store");
    (store, dir)
}

fn bench_fork_cost(c: &mut Criterion) {
    let mut group = c.benchmark_group("fork_cost");
    apply_profile(&mut group, BenchProfile::Quick);

    for &depth in DEPTHS {
        throughput_elements(&mut group, depth);
        let (store, _source_dir) = build_fixture(depth);

        for (label, options) in [
            ("cow", ForkOptions::default()),
            (
                "deep_copy_baseline",
                ForkOptions {
                    copy_preference: CopyPreference::DeepCopyOnly,
                    exclude_caches: true,
                },
            ),
        ] {
            group.bench_with_input(BenchmarkId::new(label, depth), &options, |b, options| {
                b.iter_batched(
                    || TempDir::new().expect("create fork temp dir"),
                    |dest| {
                        let report = store
                            .fork_with_evidence(dest.path(), *options)
                            .expect("fork fixture");
                        black_box(report.body.strategy_counts);
                    },
                    BatchSize::SmallInput,
                );
            });
        }

        store.close().expect("close source store");
    }

    group.finish();
}

criterion_group!(benches, bench_fork_cost);
criterion_main!(benches);
