//! Benchmark batch-path coordinate construction and append costs to size the
//! payoff of deeper staging/interner work.

use batpak::prelude::*;
use batpak::store::{BatchAppendItem, CausationRef, Store, StoreConfig, SyncMode};
use batpak_bench_support::{apply_profile, throughput_elements, BenchProfile};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use std::hint::black_box;
use tempfile::TempDir;

const BATCH_SIZE: usize = 128;
const BATCHES_PER_ITER: usize = 32;

fn build_reused_items(
    coord: &Coordinate,
    kind: EventKind,
    batch_id: usize,
    count: usize,
) -> Vec<BatchAppendItem> {
    (0..count)
        .map(|offset| {
            BatchAppendItem::new(
                coord.clone(),
                kind,
                &serde_json::json!({
                    "batch": batch_id,
                    "offset": offset,
                    "payload": "x".repeat(64),
                }),
                AppendOptions::default(),
                CausationRef::None,
            )
            .expect("build reused batch item")
        })
        .collect()
}

fn build_fresh_items(kind: EventKind, batch_id: usize, count: usize) -> Vec<BatchAppendItem> {
    (0..count)
        .map(|offset| {
            let coord = Coordinate::new(
                format!("bench:batch:fresh:{batch_id}:{offset}"),
                format!("bench:scope:{}", offset % 8),
            )
            .expect("fresh coordinate");
            BatchAppendItem::new(
                coord,
                kind,
                &serde_json::json!({
                    "batch": batch_id,
                    "offset": offset,
                    "payload": "x".repeat(64),
                }),
                AppendOptions::default(),
                CausationRef::None,
            )
            .expect("build fresh batch item")
        })
        .collect()
}

fn open_writer_store() -> (Store, TempDir, Coordinate, EventKind) {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_sync_every_n_events(10_000)
            .with_sync_mode(SyncMode::SyncData),
    )
    .expect("open store");
    let coord = Coordinate::new("bench:batch:reused", "bench:scope").expect("coord");
    let kind = EventKind::custom(0xF, 1);
    (store, dir, coord, kind)
}

fn bench_writer_batch_staging(c: &mut Criterion) {
    let mut group = c.benchmark_group("writer_batch_staging");
    apply_profile(&mut group, BenchProfile::Quick);

    throughput_elements(&mut group, BATCH_SIZE as u64);
    let coord = Coordinate::new("bench:batch:reused", "bench:scope").expect("coord");
    let kind = EventKind::custom(0xF, 1);

    group.bench_function("build_reused_coordinate_items", |b| {
        let mut batch_id = 0usize;
        b.iter(|| {
            let items = build_reused_items(&coord, kind, batch_id, BATCH_SIZE);
            black_box(items);
            batch_id += 1;
        });
    });

    group.bench_function("build_fresh_coordinate_items", |b| {
        let mut batch_id = 0usize;
        b.iter(|| {
            let items = build_fresh_items(kind, batch_id, BATCH_SIZE);
            black_box(items);
            batch_id += 1;
        });
    });

    throughput_elements(&mut group, (BATCH_SIZE * BATCHES_PER_ITER) as u64);

    group.bench_function("append_batches_reused_coordinate", |b| {
        b.iter_batched(
            open_writer_store,
            |(store, _dir, coord, kind)| {
                for batch_id in 0..BATCHES_PER_ITER {
                    let items = build_reused_items(&coord, kind, batch_id, BATCH_SIZE);
                    store.append_batch(items).expect("append reused batch");
                }
                let _ = black_box(store.stats());
                store.close().expect("close store");
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("append_batches_fresh_coordinate", |b| {
        b.iter_batched(
            open_writer_store,
            |(store, _dir, _coord, kind)| {
                for batch_id in 0..BATCHES_PER_ITER {
                    let items = build_fresh_items(kind, batch_id, BATCH_SIZE);
                    store.append_batch(items).expect("append fresh batch");
                }
                let _ = black_box(store.stats());
                store.close().expect("close store");
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_writer_batch_staging);
criterion_main!(benches);
