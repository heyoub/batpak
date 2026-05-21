//! Benchmarks for recovery-adjacent lanes that were previously tested but not
//! directly visible in the bench surface.

use batpak::coordinate::Coordinate;
use batpak::event::EventKind;
use batpak::id::EventId;
use batpak::store::delivery::cursor::CursorCheckpoint;
use batpak::store::{CheckpointId, Cursor, Store, StoreConfig};
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use std::hint::black_box;
use tempfile::TempDir;

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) {
    std::fs::create_dir_all(dst).expect("create destination directory");
    for entry in std::fs::read_dir(src).expect("read source directory") {
        let entry = entry.expect("directory entry");
        let source_path = entry.path();
        let destination_path = dst.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_dir_recursive(&source_path, &destination_path);
        } else {
            std::fs::copy(&source_path, &destination_path).expect("copy fixture file");
        }
    }
}

fn rebuild_config(dir: &std::path::Path) -> StoreConfig {
    StoreConfig::new(dir)
        .with_segment_max_bytes(512)
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false)
}

fn populated_rebuild_fixture(count: u64) -> (TempDir, EventId) {
    let dir = TempDir::new().expect("fixture temp dir");
    let store = Store::open(rebuild_config(dir.path())).expect("open fixture store");
    let coord = Coordinate::new("bench:recovery", "bench:scope").expect("coord");
    let kind = EventKind::custom(0xF, 1);
    let mut last_id = None;
    for i in 0..count {
        let receipt = store
            .append(
                &coord,
                kind,
                &serde_json::json!({"i": i, "pad": "xxxxxxxxxxxxxxxx"}),
            )
            .expect("append fixture event");
        last_id = Some(receipt.event_id);
    }
    store.sync().expect("sync fixture store");
    store.close().expect("close fixture store");
    (dir, last_id.expect("fixture contains at least one event"))
}

fn bench_cursor_checkpoint(c: &mut Criterion) {
    let mut group = c.benchmark_group("durable_cursor_checkpoint");
    let id = CheckpointId::new("bench-cursor").expect("valid checkpoint id");
    let checkpoint = CursorCheckpoint {
        position: 128,
        started: true,
        process_boot_ns: Some(42),
        region_identity: Some("entity:bench:cursor".to_owned()),
    };

    group.bench_function("save_checkpoint_atomic", |b| {
        b.iter_batched(
            TempDir::new,
            |dir| {
                let dir = dir.expect("temp dir");
                Cursor::save_checkpoint(dir.path(), &id, &checkpoint).expect("save checkpoint");
            },
            BatchSize::SmallInput,
        );
    });

    let load_fixture = TempDir::new().expect("load fixture temp dir");
    Cursor::save_checkpoint(load_fixture.path(), &id, &checkpoint).expect("seed checkpoint");
    group.bench_function("load_checkpoint", |b| {
        b.iter(|| {
            let loaded = Cursor::load_checkpoint(load_fixture.path(), &id)
                .expect("load checkpoint")
                .expect("checkpoint present");
            black_box(loaded);
        });
    });
    group.finish();
}

fn bench_sidx_recovery(c: &mut Criterion) {
    let mut group = c.benchmark_group("sidx_scan_point_read_recovery");
    for count in [128_u64, 1_024] {
        let (fixture, point_read_id) = populated_rebuild_fixture(count);
        group.bench_with_input(
            BenchmarkId::new("rebuild_from_segments", count),
            &count,
            |b, _| {
                b.iter_batched(
                    || {
                        let iter_dir = TempDir::new().expect("iteration temp dir");
                        copy_dir_recursive(fixture.path(), iter_dir.path());
                        iter_dir
                    },
                    |iter_dir| {
                        let store = Store::open(rebuild_config(iter_dir.path())).expect("reopen");
                        let _ = black_box(store.stats());
                    },
                    BatchSize::SmallInput,
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("point_read_after_rebuild", count),
            &count,
            |b, _| {
                b.iter_batched(
                    || {
                        let iter_dir = TempDir::new().expect("iteration temp dir");
                        copy_dir_recursive(fixture.path(), iter_dir.path());
                        let store = Store::open(rebuild_config(iter_dir.path())).expect("reopen");
                        (iter_dir, store)
                    },
                    |(_iter_dir, store)| {
                        let event = store.read_raw(point_read_id).expect("point read");
                        black_box(event);
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_cursor_checkpoint, bench_sidx_recovery);
criterion_main!(benches);
