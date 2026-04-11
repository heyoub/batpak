#![cfg(feature = "test-support")]
//! Restart policy tests split out of store_advanced.rs.
//! [SPEC:tests/store_restart_policy.rs]

use batpak::prelude::*;
use batpak::store::{RestartPolicy, Store, StoreConfig, WriterConfig};
use tempfile::TempDir;

#[test]
#[serial_test::serial(writer_restart)]
fn writer_restart_once_recovers_from_panic() {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 64 * 1024,
        writer: WriterConfig {
            restart_policy: RestartPolicy::Once,
            ..WriterConfig::default()
        },
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("restart:test", "restart:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    store
        .append(&coord, kind, &"before_panic")
        .expect("append before panic");
    store.panic_writer_for_test().expect("send panic command");
    store.append(&coord, kind, &"after_panic").expect(
        "RESTART FAILED: append after writer panic should succeed with RestartPolicy::Once.\n\
         Investigate: src/store/writer.rs writer_thread_main() catch_unwind logic.",
    );

    let entries = store.stream("restart:test");
    assert_eq!(entries.len(), 2);
}

#[test]
#[serial_test::serial(writer_restart)]
fn writer_restart_once_gives_up_after_second_panic() {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 64 * 1024,
        writer: WriterConfig {
            restart_policy: RestartPolicy::Once,
            ..WriterConfig::default()
        },
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("restart:exhaust", "restart:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    store.panic_writer_for_test().expect("send first panic");
    let _ = store.panic_writer_for_test();
    std::thread::sleep(std::time::Duration::from_millis(100));

    let result = store.append(&coord, kind, &"should_fail");
    assert!(result.is_err());
}

#[test]
#[serial_test::serial(writer_restart)]
fn writer_restart_bounded_respects_limit() {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 64 * 1024,
        writer: WriterConfig {
            restart_policy: RestartPolicy::Bounded {
                max_restarts: 2,
                within_ms: 60_000,
            },
            ..WriterConfig::default()
        },
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("restart:bounded", "restart:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    store.panic_writer_for_test().expect("first panic");
    store
        .append(&coord, kind, &"after_panic_1")
        .expect("append after first restart");
    store.panic_writer_for_test().expect("second panic");
    store
        .append(&coord, kind, &"after_panic_2")
        .expect("append after second restart");
    let _ = store.panic_writer_for_test();
    std::thread::sleep(std::time::Duration::from_millis(100));

    let result = store.append(&coord, kind, &"should_fail");
    assert!(result.is_err());
}
