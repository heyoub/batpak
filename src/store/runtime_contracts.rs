use super::*;
use crate::store::append::checked_payload_len;
use crate::store::segment::scan as reader;
use crate::store::write::{fanout, writer};
use tempfile::TempDir;

fn test_store_with_writer(tx: flume::Sender<writer::WriterCommand>) -> (Store, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let subscribers = Arc::new(fanout::SubscriberList::new());
    let config = Arc::new(StoreConfig::new(dir.path().to_path_buf()));
    let runtime = Arc::new(config.validated().expect("validated runtime config"));
    let watermark_handle = writer::WatermarkState::handle();
    let store = Store {
        index: Arc::new(index::StoreIndex::new()),
        reader: Arc::new(reader::Reader::new(dir.path().to_path_buf(), 4)),
        cache: Box::new(NoCache),
        writer: Some(writer::WriterHandle::from_parts_for_test(tx, subscribers)),
        projection_registry: projection::registry::ProjectionRegistry::new(Arc::clone(
            &watermark_handle,
        )),
        watermark_handle,
        lifecycle_gate: parking_lot::Mutex::new(()),
        config,
        runtime,
        should_shutdown_on_drop: true,
        open_report: None,
        cumulative_reserved_kind_fallbacks:
            crate::store::segment::sidx::ReservedKindFallbackStats::default(),
        _state: std::marker::PhantomData,
        _store_lock: dir_lock::StoreDirLock::acquire(dir.path(), StoreLockMode::Mutable)
            .expect("test store lock"),
    };
    (store, dir)
}

#[test]
fn sync_reports_writer_crash_when_channel_is_closed() {
    let (tx, rx) = flume::bounded(1);
    drop(rx);
    let (store, _dir) = test_store_with_writer(tx);

    assert!(
        matches!(Store::sync(&store), Err(StoreError::WriterCrashed)),
        "PROPERTY: Store::sync must surface WriterCrashed when the writer channel is disconnected.\n\
         Investigate: src/store/mod.rs Store::sync and src/store/lifecycle.rs sync.\n\
         Common causes: sync() returning success without contacting the writer, disconnected sends being ignored."
    );
}

#[test]
fn drop_sends_shutdown_to_writer_thread() {
    let (tx, rx) = flume::bounded(1);
    let (signal_tx, signal_rx) = flume::bounded::<()>(1);
    let _listener = std::thread::Builder::new()
        .name("store-drop-shutdown-test".into())
        .spawn(move || {
            if let Ok(writer::WriterCommand::Shutdown { respond }) = rx.recv() {
                let _ = respond.send(Ok(()));
                let _ = signal_tx.send(());
            }
        })
        .expect("spawn shutdown listener");

    let (store, _dir) = test_store_with_writer(tx);
    drop(store);

    assert!(
        signal_rx
            .recv_timeout(std::time::Duration::from_millis(500))
            .is_ok(),
        "PROPERTY: dropping Store without close() must send a Shutdown command to the writer for best-effort draining.\n\
         Investigate: src/store/mod.rs Drop for Store.\n\
         Common causes: Drop body removed, shutdown command not sent, or shutdown path returning before notifying the writer."
    );
}

#[test]
fn checked_payload_len_returns_exact_serialized_length() {
    assert_eq!(
        checked_payload_len(&[1, 2, 3, 4]).expect("payload len"),
        4,
        "PROPERTY: checked_payload_len must preserve the exact payload byte length.\n\
         Investigate: src/store/append.rs checked_payload_len.\n\
         Common causes: helper replaced with a constant, off-by-one, or truncated length conversion."
    );
}

#[test]
fn now_us_moves_forward_over_real_time() {
    let first = now_us();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let second = now_us();

    assert!(
        first > 0,
        "PROPERTY: now_us must return a positive microsecond timestamp since the Unix epoch."
    );
    assert!(
        second > first,
        "PROPERTY: now_us must advance as wall-clock time moves forward.\n\
         Investigate: src/store/config.rs now_us.\n\
         Common causes: helper replaced with a constant or non-monotonic sentinel."
    );
}

#[test]
fn sequence_gate_publish_surfaces_typed_error_instead_of_panicking() {
    let index = index::StoreIndex::new();

    assert!(
        matches!(
            index.publish(1, "runtime-contract-publish-overflow"),
            Err(StoreError::SequenceGateViolation {
                operation: "runtime-contract-publish-overflow",
                requested: 1,
                allocated: 0,
                visible: 0,
            })
        ),
        "PROPERTY: sequence gate overflow must surface StoreError::SequenceGateViolation instead of panicking."
    );

    index.sequence.restore_allocator(2);
    index
        .publish(2, "runtime-contract-publish-prime")
        .expect("prime publish");
    assert!(
        matches!(
            index.publish(1, "runtime-contract-publish-regression"),
            Err(StoreError::SequenceGateViolation {
                operation: "runtime-contract-publish-regression",
                requested: 1,
                allocated: 2,
                visible: 2,
            })
        ),
        "PROPERTY: sequence gate regression must surface StoreError::SequenceGateViolation instead of panicking."
    );
}
