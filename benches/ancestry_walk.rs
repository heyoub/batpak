//! Benchmark: walk_ancestors chain-length scaling.
//!
//! Each benchmark builds a single-entity chain of N events, then calls
//! `store.walk_ancestors(last_id, N)` to walk the entire chain.
//!
//! ## What to look for
//!
//! After the cheap-pass fix (stream loaded once per walk, not per hop),
//! cost should scale O(N²) in the per-hop linear scan — each hop scans
//! the entity stream (N entries) to find the event whose event_hash matches
//! the current entry's prev_hash.
//!
//! If N² growth is visible, a `by_event_hash: DashMap<[u8;32], Arc<IndexEntry>>`
//! lookup index would collapse that to O(N) total, at the cost of a second
//! DashMap entry per event.  The benchmark result tells us whether that
//! memory rent is worth paying.
//!
//! ## Note on disk I/O
//!
//! `walk_ancestors` reads each event from disk (segment file) to reconstruct
//! the full payload.  The benchmark intentionally uses a warm store so that
//! repeated reads land in the OS page cache, isolating the index scan cost.
//! Cold-path ancestry would be dominated by I/O instead.

mod common;

use batpak::prelude::*;
use batpak::store::{Store, StoreConfig};
use common::{apply_profile, BenchProfile};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tempfile::TempDir;

const KIND: EventKind = EventKind::custom(0xA, 1);

fn build_chain(chain_len: u32) -> (Store, TempDir, u128) {
    let dir = TempDir::new().expect("temp dir");
    let store =
        Store::open(StoreConfig::new(dir.path()).with_sync_every_n_events(100_000)).expect("open");

    let coord = Coordinate::new("bench:entity:chain", "bench:scope").expect("coord");
    let mut last_id = 0u128;
    for step in 0..chain_len {
        last_id = store
            .append(&coord, KIND, &serde_json::json!({"step": step}))
            .expect("append")
            .event_id;
    }

    (store, dir, last_id)
}

fn bench_walk_ancestors(c: &mut Criterion) {
    // Chain lengths chosen to reveal O(N²) vs O(N) scaling shape.
    let chain_lengths: &[u32] = &[10, 50, 100, 250, 500];

    let mut group = c.benchmark_group("ancestry/walk_full_chain");
    apply_profile(&mut group, BenchProfile::Heavy);

    for &chain_len in chain_lengths {
        let (store, _dir, last_id) = build_chain(chain_len);

        // Warm the page cache: one full walk before timing starts.
        let _ = store.walk_ancestors(last_id, chain_len as usize);

        group.throughput(Throughput::Elements(chain_len as u64));
        group.bench_function(BenchmarkId::new("soa", chain_len), |b| {
            b.iter(|| criterion::black_box(store.walk_ancestors(last_id, chain_len as usize)));
        });

        store.close().expect("close");
    }

    group.finish();
}

criterion_group!(benches, bench_walk_ancestors);
criterion_main!(benches);
