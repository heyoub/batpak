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
            config: &config,
            validated_cfg: &validated_cfg,
            index: &index,
            subscribers: &subscribers,
            reactor_subscribers: &reactor_subscribers,
            reader: &reader,
            watermark_handle: &watermark_handle,
        },
        segment,
        1,
    );

    append_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("append reply")
        .expect("append succeeds");
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
