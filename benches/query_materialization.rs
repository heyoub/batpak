//! Benchmark: QueryHit seam — scan cost vs. upgrade cost.
//!
//! After the QueryHit refactor, scan hot paths return `QueryHit` (Copy POD, ~52 bytes)
//! instead of `Arc<IndexEntry>`. Full `IndexEntry` materialization (`upgrade_hit`) is
//! deferred and paid only once per *returned* result, not per *scanned* entry.
//!
//! ## Cursor path
//!
//! `cursor.poll_batch(max)` now pays upgrade_hit only for the `max` entries it
//! returns. The old path called `query()` which upgraded every matching hit in the
//! corpus before the cursor sliced the result. With 1000 matching entries and batch_size=1,
//! the old path paid ~1000 atomic ops + 1000 memcpys; the new path pays 1.
//!
//! The cursor groups below show poll_batch at varying batch sizes against a fixed corpus
//! (8 kinds × 1000 = 8000 events, cursor filtered to 1 kind → 1000 matching). The cost
//! should scale with batch_size, not corpus size.
//!
//! ## Query path
//!
//! `store.by_fact()` and `store.query()` always upgrade all matching results. The
//! QueryHit pass doesn't change the *number* of upgrades here — it eliminates the
//! redundant intermediate Arc allocation that the old scan chain paid. These groups
//! confirm no regression and show the baseline cost profile across corpus sizes.

mod common;

use batpak::prelude::*;
use batpak::store::{IndexTopology, Store, StoreConfig};
use common::{apply_profile, BenchProfile};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tempfile::TempDir;

const KINDS: [EventKind; 8] = [
    EventKind::custom(0x1, 1),
    EventKind::custom(0x1, 2),
    EventKind::custom(0x1, 3),
    EventKind::custom(0x1, 4),
    EventKind::custom(0x2, 1),
    EventKind::custom(0x2, 2),
    EventKind::custom(0x2, 3),
    EventKind::custom(0x2, 4),
];

const QUERY_KIND: EventKind = KINDS[0];

fn build_store(events_per_kind: u32) -> (Store, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_index_topology(IndexTopology::scan())
            .with_sync_every_n_events(100_000),
    )
    .expect("open");

    let coords: Vec<Coordinate> = KINDS
        .iter()
        .enumerate()
        .map(|(i, _)| Coordinate::new(format!("bench:entity:{i}"), "bench:scope").expect("coord"))
        .collect();

    // Interleaved so the cursor scan sees all kinds mixed — worst case for selectivity.
    for seq in 0..events_per_kind {
        for (i, &kind) in KINDS.iter().enumerate() {
            store
                .append(&coords[i], kind, &serde_json::json!({"seq": seq}))
                .expect("append");
        }
    }

    (store, dir)
}

fn close_store(store: Store) {
    store.close().expect("close");
}

// --- Cursor benchmarks -------------------------------------------------------
//
// Measures: scan all matching + upgrade only batch_size entries.
// Fresh cursor each iteration so every call is a cold poll from the beginning.

fn bench_cursor_poll_batch(c: &mut Criterion) {
    const EVENTS_PER_KIND: u32 = 1_000; // 8_000 total; 1_000 match QUERY_KIND
    let (store, _dir) = build_store(EVENTS_PER_KIND);
    let region = Region::all().with_fact(KindFilter::Exact(QUERY_KIND));

    let batch_sizes: &[usize] = &[1, 16, 64, 256];

    let mut group = c.benchmark_group("cursor/poll_batch");
    apply_profile(&mut group, BenchProfile::Heavy);

    for &batch_size in batch_sizes {
        group.throughput(Throughput::Elements(batch_size as u64));
        group.bench_function(BenchmarkId::new("soa", batch_size), |b| {
            b.iter(|| {
                let mut cursor = store.cursor_guaranteed(&region);
                criterion::black_box(cursor.poll_batch(batch_size))
            });
        });
    }

    group.finish();

    close_store(store);
}

// --- Query benchmarks --------------------------------------------------------
//
// Measures: scan + upgrade all matching entries.
// Varying corpus sizes to show linear cost profile and confirm no regression.

fn bench_by_kind(c: &mut Criterion) {
    let corpus_sizes: &[u32] = &[100, 500, 1_000, 4_000];

    let mut group = c.benchmark_group("query/by_kind");
    apply_profile(&mut group, BenchProfile::Heavy);

    for &events_per_kind in corpus_sizes {
        let (store, _dir) = build_store(events_per_kind);
        group.throughput(Throughput::Elements(events_per_kind as u64));
        group.bench_function(BenchmarkId::new("soa", events_per_kind), |b| {
            b.iter(|| criterion::black_box(store.by_fact(QUERY_KIND)));
        });
        close_store(store);
    }

    group.finish();
}

fn bench_query_region(c: &mut Criterion) {
    // Scans all events, returns all — stress-tests the upgrade path.
    let corpus_sizes: &[u32] = &[100, 500, 1_000];

    let mut group = c.benchmark_group("query/region_all");
    apply_profile(&mut group, BenchProfile::Heavy);

    for &events_per_kind in corpus_sizes {
        let total = events_per_kind as u64 * KINDS.len() as u64;
        let (store, _dir) = build_store(events_per_kind);
        group.throughput(Throughput::Elements(total));
        group.bench_function(BenchmarkId::new("soa", events_per_kind), |b| {
            b.iter(|| criterion::black_box(store.query(&Region::all())));
        });
        close_store(store);
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_cursor_poll_batch,
    bench_by_kind,
    bench_query_region
);
criterion_main!(benches);
