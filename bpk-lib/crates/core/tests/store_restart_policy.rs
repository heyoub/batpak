#![cfg(feature = "dangerous-test-hooks")]
//! Restart policy tests split out of store_advanced.rs.

use batpak::store::{HlcPoint, RestartPolicy, Store, StoreConfig, StoreError};
use batpak_testkit::prelude::*;
use std::time::Duration;
use tempfile::TempDir;

#[test]
#[serial_test::serial(writer_restart)]
fn writer_restart_once_recovers_from_panic() {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig::new(dir.path())
        .with_segment_max_bytes(64 * 1024)
        .with_restart_policy(RestartPolicy::Once);
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("restart:test", "restart:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    store
        .append(&coord, kind, &"before_panic")
        .expect("append before panic");
    store.panic_writer_for_test().expect("send panic command");
    store.append(&coord, kind, &"after_panic").expect(
        "RESTART FAILED: append after writer panic should succeed with RestartPolicy::Once.\n\
         Investigate: src/store/write/writer.rs writer_thread_main() catch_unwind logic.",
    );

    let entries = store.by_entity("restart:test");
    assert_eq!(entries.len(), 2);
}

#[test]
#[serial_test::serial(writer_restart)]
fn writer_restart_recovers_durability_gate_after_panic() {
    // R3 regression: a within-budget panic used to poison the durability gate
    // permanently — `mark_writer_crashed` fired before the budget check and the
    // poison flag was never cleared, so every wait_for_durable/applied/visible
    // returned WriterCrashed forever after the first transient panic. After a
    // recoverable panic + restart the gate must work again. ORIGIN is always
    // at/below the frontier, and wait_for_watermark checks poison FIRST, so the
    // only way this errors is the stale poison flag.
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig::new(dir.path())
        .with_segment_max_bytes(64 * 1024)
        .with_restart_policy(RestartPolicy::Once);
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("restart:durable", "restart:scope").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    store
        .append(&coord, kind, &"before_panic")
        .expect("append before panic");
    store.panic_writer_for_test().expect("send panic command");
    store
        .append(&coord, kind, &"after_panic")
        .expect("append after restart");

    store
        .wait_for_durable(HlcPoint::ORIGIN, Duration::from_secs(5))
        .expect(
            "DURABILITY GATE POISONED: after a within-budget panic + restart, \
             wait_for_durable must recover, not stay WriterCrashed (audit R3).",
        );
}

#[test]
#[serial_test::serial(writer_restart)]
fn writer_restart_once_gives_up_after_second_panic() {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig::new(dir.path())
        .with_segment_max_bytes(64 * 1024)
        .with_restart_policy(RestartPolicy::Once);
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
    let final_err: Option<StoreError> = loop {
        match store.append(&coord, kind, &"should_fail") {
            Err(e) => break Some(e),
            Ok(_) if std::time::Instant::now() >= deadline => break None,
            Ok(_) => std::thread::yield_now(),
        }
    };
    let final_err = final_err.expect(
        "PROPERTY: after exhausting RestartPolicy::Once, append must fail. \
         Writer thread did not die within 5s of receiving the second \
         PanicForTest command. Investigate: src/store/write/writer.rs \
         writer_thread_main restart counter.",
    );
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
    let config = StoreConfig::new(dir.path())
        .with_segment_max_bytes(64 * 1024)
        .with_restart_policy(RestartPolicy::Bounded {
            max_restarts: 2,
            within_ms: 60_000,
        });
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
    let final_err: Option<StoreError> = loop {
        match store.append(&coord, kind, &"should_fail") {
            Err(e) => break Some(e),
            Ok(_) if std::time::Instant::now() >= deadline => break None,
            Ok(_) => std::thread::yield_now(),
        }
    };
    let final_err = final_err.expect(
        "PROPERTY: after exhausting RestartPolicy::Bounded, append must fail. \
         Writer thread did not die within 5s. \
         Investigate: src/store/write/writer.rs restart counter.",
    );
    assert!(
        matches!(final_err, StoreError::WriterCrashed),
        "PROPERTY: append after exhausted bounded restart budget must \
         surface as StoreError::WriterCrashed; got {final_err:?}."
    );
}
