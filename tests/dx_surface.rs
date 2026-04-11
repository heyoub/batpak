use batpak::prelude::*;

#[test]
fn store_config_builder_methods_are_chainable() {
    let clock: std::sync::Arc<dyn Fn() -> i64 + Send + Sync> = std::sync::Arc::new(|| 42);
    let config = StoreConfig::new("./batpak-data")
        .with_segment_max_bytes(1024)
        .with_sync_every_n_events(7)
        .with_fd_budget(9)
        .with_writer_channel_capacity(10)
        .with_broadcast_capacity(11)
        .with_restart_policy(RestartPolicy::Bounded {
            max_restarts: 2,
            within_ms: 30_000,
        })
        .with_shutdown_drain_limit(13)
        .with_writer_stack_size(Some(14))
        .with_clock(Some(clock))
        .with_sync_mode(SyncMode::SyncData);

    assert_eq!(config.segment_max_bytes, 1024);
    assert_eq!(config.sync.every_n_events, 7);
    assert_eq!(config.fd_budget, 9);
    assert_eq!(config.writer.channel_capacity, 10);
    assert_eq!(config.broadcast_capacity, 11);
    assert!(matches!(
        config.writer.restart_policy,
        RestartPolicy::Bounded {
            max_restarts: 2,
            within_ms: 30_000
        }
    ));
    assert_eq!(config.writer.shutdown_drain_limit, 13);
    assert_eq!(config.writer.stack_size, Some(14));
    assert!(config.clock.is_some());
    assert!(matches!(config.sync.mode, SyncMode::SyncData));
}

#[test]
fn open_with_native_cache_is_available_for_common_setup() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let cache_dir = tempfile::tempdir().expect("cache dir");
    let store = Store::open_with_native_cache(
        StoreConfig::new(data_dir.path()),
        cache_dir.path().join("projection_cache"),
    )
    .expect("open store with native cache");
    let coord = Coordinate::new("entity:native", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 1);
    let receipt = store
        .append(&coord, kind, &serde_json::json!({"hello": "native"}))
        .expect("append");
    assert!(receipt.event_id != 0);
}
