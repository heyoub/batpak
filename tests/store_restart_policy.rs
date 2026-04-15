#![cfg(feature = "dangerous-test-hooks")]
#![allow(clippy::panic)] // tests use panic! to escape the retry-poll loops
//! Restart policy tests split out of store_advanced.rs.

use batpak::prelude::*;
use batpak::store::{RestartPolicy, Store, StoreConfig, StoreError, WriterConfig};
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

    // Poll for the writer to actually die instead of sleeping a fixed
    // duration. The writer thread takes some non-deterministic time to
    // process the panic command and exit; sleeping a fixed amount makes
    // the test flaky on slow CI runners. Retry append with a deadline.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let final_err = loop {
        match store.append(&coord, kind, &"should_fail") {
            Err(e) => break e,
            Ok(_) if std::time::Instant::now() >= deadline => {
                panic!(
                    "PROPERTY: after exhausting RestartPolicy::Once, append \
                     must fail. Writer thread did not die within 5s of \
                     receiving the second PanicForTest command. \
                     Investigate: src/store/writer.rs writer_thread_main \
                     restart counter."
                )
            }
            Ok(_) => std::thread::yield_now(),
        }
    };
    assert!(
        matches!(final_err, StoreError::WriterCrashed),
        "PROPERTY: append after exhausted restart budget must surface as \
         StoreError::WriterCrashed; got {final_err:?}. \
         Investigate: src/store/mod.rs do_append flume::send error mapping."
    );
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

    // Poll for the writer to die instead of sleeping a fixed duration —
    // see writer_restart_once_gives_up_after_second_panic for rationale.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let final_err = loop {
        match store.append(&coord, kind, &"should_fail") {
            Err(e) => break e,
            Ok(_) if std::time::Instant::now() >= deadline => {
                panic!(
                    "PROPERTY: after exhausting RestartPolicy::Bounded, \
                     append must fail. Writer thread did not die within 5s. \
                     Investigate: src/store/writer.rs restart counter."
                )
            }
            Ok(_) => std::thread::yield_now(),
        }
    };
    assert!(
        matches!(final_err, StoreError::WriterCrashed),
        "PROPERTY: append after exhausted bounded restart budget must \
         surface as StoreError::WriterCrashed; got {final_err:?}."
    );
}
