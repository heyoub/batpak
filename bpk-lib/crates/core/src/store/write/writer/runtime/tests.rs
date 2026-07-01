use super::*;
use crate::store::index::StoreIndex;
use crate::store::segment::scan::Reader;
use crate::store::write::writer::{ReactorSubscriberList, SubscriberList, WatermarkState};
use crate::store::SystemClock;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

#[test]
fn writer_thread_name_is_stable_nonempty_and_prefixed() {
    let path = Path::new("batpak/writer-name");
    let name = writer_thread_name(path);

    assert!(
        name.starts_with("batpak-writer-"),
        "PROPERTY: writer thread names carry a stable batpak prefix for diagnostics"
    );
    assert!(
        name.len() > "batpak-writer-".len(),
        "PROPERTY: writer thread names include a data-dir-derived suffix rather than the empty string"
    );
    assert_eq!(
        name,
        writer_thread_name(path),
        "PROPERTY: writer thread names are deterministic for a store directory"
    );
    assert_ne!(
        name,
        writer_thread_name(Path::new("batpak/other-writer-name")),
        "PROPERTY: distinct store directories should not collapse to one diagnostic thread name"
    );
}

#[test]
fn restart_budget_once_allows_exactly_one_restart() {
    let mut restarts = 0;
    let mut window_start = 0;

    assert!(
        restart_budget_allows(&RestartPolicy::Once, &mut restarts, &mut window_start, 0,),
        "PROPERTY: RestartPolicy::Once grants the first restart"
    );
    assert_eq!(
        restarts, 1,
        "PROPERTY: accepting a restart increments the budget counter"
    );
    assert!(
        !restart_budget_allows(&RestartPolicy::Once, &mut restarts, &mut window_start, 0,),
        "PROPERTY: RestartPolicy::Once rejects a second restart"
    );
    assert_eq!(
        restarts, 1,
        "PROPERTY: rejecting a restart must not mutate the accepted restart count"
    );
}

#[test]
fn bounded_restart_budget_resets_after_window() {
    let policy = RestartPolicy::Bounded {
        max_restarts: 1,
        within_ms: 10,
    };
    let base = 1_000_000_000;
    let mut window_start = base;
    let mut restarts = 0;

    assert!(
        restart_budget_allows(&policy, &mut restarts, &mut window_start, base),
        "PROPERTY: bounded policy accepts the first restart in the window"
    );
    assert!(
        !restart_budget_allows(&policy, &mut restarts, &mut window_start, base + 1_000_000),
        "PROPERTY: bounded policy rejects restarts past the per-window cap"
    );
    assert!(
        !restart_budget_allows(&policy, &mut restarts, &mut window_start, base + 10_000_000),
        "PROPERTY: bounded policy window is inclusive at exactly within_ms; \
         a >= reset admits one restart too early"
    );
    assert!(
        restart_budget_allows(&policy, &mut restarts, &mut window_start, base + 11_000_000),
        "PROPERTY: bounded policy resets after its configured time window"
    );
    assert_eq!(
        restarts, 1,
        "PROPERTY: reset starts a fresh window with one accepted restart"
    );
}

#[test]
fn shutdown_drain_limit_is_exclusive_upper_bound() {
    let dir = TempDir::new().expect("temp dir");
    let config = Arc::new(
        StoreConfig::new(dir.path())
            .with_shutdown_drain_limit(1)
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
    let (tx, rx) = flume::bounded(3);
    let (shutdown_tx, shutdown_rx) = flume::bounded(1);
    let (first_sync_tx, first_sync_rx) = flume::bounded(1);
    let (second_sync_tx, second_sync_rx) = flume::bounded(1);

    tx.send(WriterCommand::Shutdown {
        respond: shutdown_tx,
    })
    .expect("queue shutdown");
    tx.send(WriterCommand::Sync {
        respond: first_sync_tx,
    })
    .expect("queue first sync behind shutdown");
    tx.send(WriterCommand::Sync {
        respond: second_sync_tx,
    })
    .expect("queue second sync behind shutdown");
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

    shutdown_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("shutdown reply")
        .expect("shutdown succeeds");
    first_sync_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("first queued sync reply")
        .expect("first queued sync succeeds");
    assert!(
        second_sync_rx
            .recv_timeout(Duration::from_millis(50))
            .is_err(),
        "PROPERTY: shutdown_drain_limit=1 must drain exactly one queued command after Shutdown; \
         a <= loop drains the second Sync too."
    );
}

#[test]
fn shutdown_drain_limit_zero_drains_no_commands_behind_shutdown() {
    let dir = TempDir::new().expect("temp dir");
    let config = Arc::new(
        StoreConfig::new(dir.path())
            .with_shutdown_drain_limit(0)
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
    let (tx, rx) = flume::bounded(2);
    let (shutdown_tx, shutdown_rx) = flume::bounded(1);
    let (sync_tx, sync_rx) = flume::bounded(1);

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

    shutdown_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("shutdown reply")
        .expect("shutdown succeeds");
    assert!(
        sync_rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "PROPERTY: shutdown_drain_limit=0 must not execute any command queued behind Shutdown; \
         a <= drain loop executes one Sync at the zero boundary."
    );
}
