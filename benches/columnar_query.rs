//! Benchmark: columnar scan layouts — by_kind and by_category query latency.
//!
//! Compares SoA (scan topology) vs AoSoA64 (tiled topology) on two corpus shapes:
//!
//! - **sorted**: events arrive in same-kind runs (best case for tile-skip)
//! - **interleaved**: events arrive in round-robin kind order (worst case for tile-skip)
//!
//! This benchmark drives the routing decision for the AoSoA64 specialization:
//! if AoSoA64 does not beat SoA clearly on the sorted corpus, the tile-skip
//! optimization is not paying off and the layout does not earn its route.
//!
//! ## What to look for
//!
//! - `by_kind/sorted/*`: tile-skip should skip 7 of 8 tile groups cheaply.
//!   AoSoA64 should be faster than SoA here.
//! - `by_kind/interleaved/*`: many single-entry tiles; tile-skip advantage collapses.
//!   Expect SoA to be equal or better.
//! - `by_category/sorted/*`: tile-skip skips half the tile groups (4 of 8 kinds
//!   share each category). Partial benefit expected.
//! - `by_category/interleaved/*`: same collapse as by_kind interleaved.

mod common;

use batpak::prelude::*;
use batpak::store::{IndexTopology, Store, StoreConfig};
use common::{apply_profile, BenchProfile};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tempfile::TempDir;

// 8 distinct kinds spread across 2 categories.
// category 0x1: KIND_1 .. KIND_4
// category 0x2: KIND_5 .. KIND_8
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

const EVENTS_PER_KIND: u32 = 1_000; // 8_000 total events

// The kind and category we benchmark queries against.
const QUERY_KIND: EventKind = KINDS[0];
const QUERY_CATEGORY: u8 = 0x1;

fn build_sorted_store(topology: IndexTopology) -> (Store, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_index_topology(topology)
            .with_sync_every_n_events(100_000),
    )
    .expect("open");
    // Same-kind runs: all EVENTS_PER_KIND events of kind[0], then kind[1], etc.
    for (i, &kind) in KINDS.iter().enumerate() {
        let coord = Coordinate::new(format!("bench:entity:{i}"), "bench:scope").expect("coord");
        for seq in 0..EVENTS_PER_KIND {
            store
                .append(&coord, kind, &serde_json::json!({"seq": seq}))
                .expect("append");
        }
    }
    (store, dir)
}

fn build_interleaved_store(topology: IndexTopology) -> (Store, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_index_topology(topology)
            .with_sync_every_n_events(100_000),
    )
    .expect("open");
    // Round-robin: kind[0], kind[1], …, kind[7], kind[0], kind[1], … etc.
    let coords: Vec<Coordinate> = KINDS
        .iter()
        .enumerate()
        .map(|(i, _)| Coordinate::new(format!("bench:entity:{i}"), "bench:scope").expect("coord"))
        .collect();
    for seq in 0..EVENTS_PER_KIND {
        for (i, &kind) in KINDS.iter().enumerate() {
            store
                .append(&coords[i], kind, &serde_json::json!({"seq": seq}))
                .expect("append");
        }
    }
    (store, dir)
}

fn bench_by_kind(c: &mut Criterion) {
    let cases = [
        ("soa", IndexTopology::scan()),
        ("aosoa64", IndexTopology::tiled()),
        ("aosoa64simd", IndexTopology::tiled_simd()),
    ];

    let mut group = c.benchmark_group("by_kind/sorted");
    apply_profile(&mut group, BenchProfile::Heavy);
    group.throughput(Throughput::Elements(EVENTS_PER_KIND as u64));

    for (name, topology) in &cases {
        let (store, _dir) = build_sorted_store(topology.clone());
        group.bench_function(BenchmarkId::new(*name, EVENTS_PER_KIND), |b| {
            b.iter(|| criterion::black_box(store.by_fact(QUERY_KIND)));
        });
        store.close().expect("close");
    }
    group.finish();

    let mut group = c.benchmark_group("by_kind/interleaved");
    apply_profile(&mut group, BenchProfile::Heavy);
    group.throughput(Throughput::Elements(EVENTS_PER_KIND as u64));

    for (name, topology) in &cases {
        let (store, _dir) = build_interleaved_store(topology.clone());
        group.bench_function(BenchmarkId::new(*name, EVENTS_PER_KIND), |b| {
            b.iter(|| criterion::black_box(store.by_fact(QUERY_KIND)));
        });
        store.close().expect("close");
    }
    group.finish();
}

fn bench_by_category(c: &mut Criterion) {
    let cases = [
        ("soa", IndexTopology::scan()),
        ("aosoa64", IndexTopology::tiled()),
        ("aosoa64simd", IndexTopology::tiled_simd()),
    ];

    let mut group = c.benchmark_group("by_category/sorted");
    apply_profile(&mut group, BenchProfile::Heavy);
    // 4 kinds share the query category → 4 * EVENTS_PER_KIND results.
    group.throughput(Throughput::Elements(4 * EVENTS_PER_KIND as u64));

    for (name, topology) in &cases {
        let (store, _dir) = build_sorted_store(topology.clone());
        group.bench_function(BenchmarkId::new(*name, EVENTS_PER_KIND), |b| {
            b.iter(|| {
                criterion::black_box(store.query(&Region::all().with_fact_category(QUERY_CATEGORY)))
            });
        });
        store.close().expect("close");
    }
    group.finish();

    let mut group = c.benchmark_group("by_category/interleaved");
    apply_profile(&mut group, BenchProfile::Heavy);
    group.throughput(Throughput::Elements(4 * EVENTS_PER_KIND as u64));

    for (name, topology) in &cases {
        let (store, _dir) = build_interleaved_store(topology.clone());
        group.bench_function(BenchmarkId::new(*name, EVENTS_PER_KIND), |b| {
            b.iter(|| {
                criterion::black_box(store.query(&Region::all().with_fact_category(QUERY_CATEGORY)))
            });
        });
        store.close().expect("close");
    }
    group.finish();
}

criterion_group!(benches, bench_by_kind, bench_by_category);
criterion_main!(benches);
