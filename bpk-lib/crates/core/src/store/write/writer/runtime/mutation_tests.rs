use super::*;
use crate::coordinate::{Coordinate, DagPosition};
use crate::event::{Event, EventHeader, EventKind};
use crate::store::index::StoreIndex;
use crate::store::segment::scan::Reader;
use crate::store::write::writer::{
    AppendGuards, ReactorSubscriberList, SubscriberList, WatermarkState,
};
use crate::store::SystemClock;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

#[test]
fn restart_segment_id_advances_from_latest_or_fallback() {
    assert_eq!(
        next_restart_segment_id(Some(9), 3),
        10,
        "PROPERTY: writer restart must advance from the highest segment present on disk"
    );
    assert_eq!(
        next_restart_segment_id(None, 3),
        4,
        "PROPERTY: writer restart must advance from its in-memory fallback when no segment is on disk"
    );
    assert_eq!(
        next_restart_segment_id(Some(u64::MAX), 3),
        u64::MAX,
        "PROPERTY: restart segment advancement saturates instead of wrapping at u64::MAX"
    );
}

#[test]
fn group_commit_drain_budget_is_exclusive_upper_bound() {
    assert!(
        group_commit_drain_budget_remaining(0, 1),
        "PROPERTY: group-commit drain budget permits the first drain attempt"
    );
    assert!(
        group_commit_drain_budget_remaining(1, 2),
        "PROPERTY: group-commit drain budget permits work below the configured cap"
    );
    assert!(
        !group_commit_drain_budget_remaining(1, 1),
        "PROPERTY: group-commit drain budget stops once the configured cap is reached"
    );
    assert!(
        !group_commit_drain_budget_remaining(2, 1),
        "PROPERTY: group-commit drain budget stays closed after the cap is exceeded"
    );
}

#[test]
fn shutdown_in_group_commit_drain_exits_before_shutdown_queue_drain() {
    let dir = TempDir::new().expect("temp dir");
    let config = Arc::new(
        StoreConfig::new(dir.path())
            .with_group_commit_max_batch(2)
            .with_sync_every_n_events(1024)
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    );
    crate::store::platform::fs::create_dir_all(&config.data_dir).expect("create store dir");
    let validated_cfg = Arc::new(config.validated().expect("validated config"));
    assert_eq!(
        validated_cfg.group_commit_drain_budget, 1,
        "PROPERTY: max batch 2 gives the group-commit drain exactly one extra command slot"
    );
    let index = Arc::new(StoreIndex::with_config(&config.index));
    let reader = Arc::new(Reader::new(
        config.data_dir.clone(),
        config.fd_budget,
        &validated_cfg.clock_arc(),
        Arc::clone(config.fs()),
    ));
    let subscribers = SubscriberList::new();
    let reactor_subscribers = ReactorSubscriberList::new();
    let watermark_handle = WatermarkState::handle(Arc::new(SystemClock::new()));
    let segment = Segment::<Active>::create_with_created_ns_on(
        &config.data_dir,
        1,
        validated_cfg.now_wall_ns(),
        config.fs(),
    )
    .expect("create active segment");
    let (tx, rx) = flume::bounded(3);
    let (append_tx, append_rx) = flume::bounded(1);
    let (shutdown_tx, shutdown_rx) = flume::bounded(1);
    let (sync_tx, sync_rx) = flume::bounded(1);
    let kind = EventKind::custom(0xF, 0x51);
    let payload = vec![0xA5];
    let event = Event::new(
        EventHeader::new(
            0xA11CE,
            0xA11CE,
            None,
            validated_cfg.now_wall_ns() / 1_000,
            DagPosition::root(),
            u32::try_from(payload.len()).expect("payload len fits u32"),
            kind,
        ),
        payload,
    );
    let guards = AppendGuards {
        correlation_id: 0xA11CE,
        causation_id: None,
        expected_sequence: None,
        idempotency_key: Some(0xA11CE),
        dag_lane: 0,
        dag_depth: 0,
        dag_branch_root: false,
        extensions: BTreeMap::new(),
    };

    tx.send(WriterCommand::Append {
        coord: Coordinate::new("entity:group-drain", "scope:test").expect("coord"),
        event: Box::new(event),
        kind,
        guards,
        respond: append_tx,
    })
    .expect("queue append");
    tx.send(WriterCommand::Shutdown {
        respond: shutdown_tx,
    })
    .expect("queue shutdown");
    tx.send(WriterCommand::Sync { respond: sync_tx })
        .expect("queue sync behind shutdown");
    drop(tx);

    writer_loop(
        WriterRuntime {
            rx: &rx,
            config: Arc::clone(&config),
            validated_cfg: Arc::clone(&validated_cfg),
            index: Arc::clone(&index),
            subscribers: Arc::new(subscribers),
            reactor_subscribers: Arc::new(reactor_subscribers),
            reader: Arc::clone(&reader),
            watermark_handle: watermark_handle.clone(),
        },
        segment,
        1,
    );

    drop(
        append_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("append reply")
            .expect("append succeeds"),
    );
    shutdown_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("shutdown reply")
        .expect("shutdown succeeds");
    assert!(
        sync_rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "PROPERTY: Shutdown consumed during GroupCommitDrain exits before shutdown queue drain; \
         if the group-drain loop is skipped, Main-phase Shutdown drains this trailing Sync."
    );
}

#[test]
fn single_append_publishes_global_visibility_frontier_past_event() {
    // SEAM: writer-append-publish-frontier. The unfenced single-append path
    // publishes the GLOBAL visibility watermark to `global_seq + 1` (publish is
    // an exclusive upper bound). The first event lands at global_seq 0, so that
    // frontier must advance to 1 for the event to be observable. The
    // `+ 1 -> * 1` mutant publishes `global_seq * 1 == 0`, leaving the global
    // frontier at 0 -> the just-appended event stays hidden and a reader blocked
    // on it hangs (the suite would only "notice" via the cargo-mutants test
    // timeout). We read the frontier DIRECTLY with no blocking wait, so the
    // mutant fails this assertion in microseconds. NOTE: only the GLOBAL arg is
    // mutated; the per-lane frontier still advances to 1, so visible_entries() /
    // is_visible_on_lane would NOT distinguish — the kill must read the global
    // `visible_sequence()`.
    let dir = TempDir::new().expect("temp dir");
    let config = Arc::new(
        StoreConfig::new(dir.path())
            .with_sync_every_n_events(1024)
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    );
    crate::store::platform::fs::create_dir_all(&config.data_dir).expect("create store dir");
    let validated_cfg = Arc::new(config.validated().expect("validated config"));
    let index = Arc::new(StoreIndex::with_config(&config.index));
    let reader = Arc::new(Reader::new(
        config.data_dir.clone(),
        config.fd_budget,
        &validated_cfg.clock_arc(),
        Arc::clone(config.fs()),
    ));
    let subscribers = SubscriberList::new();
    let reactor_subscribers = ReactorSubscriberList::new();
    let watermark_handle = WatermarkState::handle(Arc::new(SystemClock::new()));
    let segment = Segment::<Active>::create_with_created_ns_on(
        &config.data_dir,
        1,
        validated_cfg.now_wall_ns(),
        config.fs(),
    )
    .expect("create active segment");

    let (tx, rx) = flume::bounded(1);
    let (append_tx, append_rx) = flume::bounded(1);
    let kind = EventKind::custom(0xF, 0x51);
    let payload = vec![0xA5];
    let event = Event::new(
        EventHeader::new(
            0xA11CE,
            0xA11CE,
            None,
            validated_cfg.now_wall_ns() / 1_000,
            DagPosition::root(),
            u32::try_from(payload.len()).expect("payload len fits u32"),
            kind,
        ),
        payload,
    );
    let guards = AppendGuards {
        correlation_id: 0xA11CE,
        causation_id: None,
        expected_sequence: None,
        idempotency_key: None,
        dag_lane: 0,
        dag_depth: 0,
        dag_branch_root: false,
        extensions: BTreeMap::new(),
    };

    tx.send(WriterCommand::Append {
        coord: Coordinate::new("entity:frontier", "scope:test").expect("coord"),
        event: Box::new(event),
        kind,
        guards,
        respond: append_tx,
    })
    .expect("queue append");
    drop(tx);

    // writer_loop runs synchronously on THIS thread and returns once the command
    // channel is drained + disconnected — fully bounded, no spin-wait.
    writer_loop(
        WriterRuntime {
            rx: &rx,
            config: Arc::clone(&config),
            validated_cfg: Arc::clone(&validated_cfg),
            index: Arc::clone(&index),
            subscribers: Arc::new(subscribers),
            reactor_subscribers: Arc::new(reactor_subscribers),
            reader: Arc::clone(&reader),
            watermark_handle: watermark_handle.clone(),
        },
        segment,
        1,
    );

    // Bounded recv: the reply is already enqueued by the time writer_loop
    // returns, so this never actually waits. Ok with global_sequence 0 under both
    // real and mutant code; it is NOT the kill signal.
    let receipt = append_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("append reply")
        .expect("append succeeds");
    assert_eq!(
        receipt.global_sequence, 0,
        "PROPERTY: the first append occupies global_sequence 0",
    );

    // DECISIVE, NON-BLOCKING observable: the global visibility watermark is an
    // exclusive upper bound, so after the first event (global_seq 0) it must read
    // 1. The `+ 1 -> * 1` mutant publishes `global_seq * 1 == 0`, leaving it at 0
    // and the event permanently hidden on the global axis.
    assert_eq!(
        index.visible_sequence(),
        1,
        "PROPERTY: unfenced single append must publish the GLOBAL visibility \
         frontier to global_seq + 1 (== 1) so the event at global_seq 0 is \
         visible; the `+ -> *` mutant leaves it at 0 and the event stays hidden",
    );
}

#[test]
fn recreate_restart_segment_returns_some_on_valid_restart_precondition() {
    // SEAM writer-recreate-restart-segment: exercise the restart/recovery path
    // directly. On real code the active segment is recreated and the writer is
    // operational; the `-> None` mutant skips creation, writer_thread_main then
    // returns without poisoning the gate, and any post-restart append blocks
    // forever (the suite otherwise only notices by hanging at the cargo-mutants
    // timeout). Here the return value is read synchronously: no channel recv, no
    // thread spawn, no join, so the mutant fails by assertion in microseconds.
    let dir = TempDir::new().expect("temp dir");
    let config = Arc::new(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    );
    crate::store::platform::fs::create_dir_all(&config.data_dir).expect("create store dir");
    let validated_cfg = Arc::new(config.validated().expect("validated config"));
    let index = Arc::new(StoreIndex::with_config(&config.index));
    let reader = Arc::new(Reader::new(
        config.data_dir.clone(),
        config.fd_budget,
        &validated_cfg.clock_arc(),
        Arc::clone(config.fs()),
    ));
    let watermark_handle = WatermarkState::handle(Arc::new(SystemClock::new()));
    let (_tx, rx) = flume::bounded::<WriterCommand>(1);
    let runtime = WriterRuntime {
        rx: &rx,
        config: Arc::clone(&config),
        validated_cfg: Arc::clone(&validated_cfg),
        index: Arc::clone(&index),
        subscribers: Arc::new(SubscriberList::new()),
        reactor_subscribers: Arc::new(ReactorSubscriberList::new()),
        reader: Arc::clone(&reader),
        watermark_handle: watermark_handle.clone(),
    };

    // Recovery precondition: data_dir exists and segment id 7 is free on disk,
    // exactly the state after find_latest_segment_id + next_restart_segment_id on
    // a restart. The real create + fsync through the fs seam must yield a live
    // Active segment; the mutant returns None unconditionally.
    let recovered = recreate_restart_segment(&runtime, 7);

    assert!(
        recovered.is_some(),
        "PROPERTY: on writer restart with an existing data_dir and a free segment \
         id, recreate_restart_segment must hand back a live Active segment so the \
         writer comes back online; the `-> None` mutant returns None and strands \
         the restart loop in writer_thread_main, which the suite otherwise only \
         notices by hanging at the cargo-mutants test timeout."
    );
}
