//! Benchmark writer-path coordinate construction vs reuse.

use batpak::prelude::*;
use batpak::store::{Store, StoreConfig};
use batpak_bench_support::{apply_profile, throughput_elements, BenchProfile};
use criterion::{criterion_group, criterion_main, Criterion};
use tempfile::TempDir;

fn bench_writer_coordinate_churn(c: &mut Criterion) {
    let mut group = c.benchmark_group("writer_coordinate_churn");
    apply_profile(&mut group, BenchProfile::Quick);
    throughput_elements(&mut group, 1_000);

    group.bench_function("append_reused_coordinate", |b| {
        let dir = TempDir::new().expect("temp dir");
        let store = Store::open(StoreConfig::new(dir.path())).expect("open");
        let coord = Coordinate::new("bench:reused", "bench:scope").expect("coord");
        let kind = EventKind::custom(0xF, 1);
        let mut i = 0u64;
        b.iter(|| {
            let _ = store
                .append(&coord, kind, &serde_json::json!({"i": i}))
                .expect("append");
            i += 1;
        });
        store.close().expect("close");
    });

    group.bench_function("append_fresh_coordinate", |b| {
        let dir = TempDir::new().expect("temp dir");
        let store = Store::open(StoreConfig::new(dir.path())).expect("open");
        let kind = EventKind::custom(0xF, 1);
        let mut i = 0u64;
        b.iter(|| {
            let coord = Coordinate::new(format!("bench:fresh:{i}"), "bench:scope").expect("coord");
            let _ = store
                .append(&coord, kind, &serde_json::json!({"i": i}))
                .expect("append");
            i += 1;
        });
        store.close().expect("close");
    });

    group.finish();
}

criterion_group!(benches, bench_writer_coordinate_churn);
criterion_main!(benches);
