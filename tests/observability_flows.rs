//! Observability proofs for named store flows.

use batpak::prelude::*;
use batpak::store::{
    CacheCapabilities, CacheMeta, CompactionConfig, Freshness, ProjectionCache, Store, StoreConfig,
    StoreError, SyncConfig,
};
use std::io;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use tracing_subscriber::fmt::MakeWriter;

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Counter {
    count: u64,
}

impl EventSourced for Counter {
    type Input = batpak::prelude::JsonValueInput;

    fn apply_event(&mut self, _event: &Event<serde_json::Value>) {
        self.count += 1;
    }

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        if events.is_empty() {
            return None;
        }
        let mut state = Self::default();
        for event in events {
            state.apply_event(event);
        }
        Some(state)
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        static KINDS: [EventKind; 1] = [EventKind::custom(0xF, 1)];
        &KINDS
    }
}

#[derive(Clone, Default)]
struct SharedWriter {
    buf: Arc<Mutex<Vec<u8>>>,
}

struct SharedGuard {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl io::Write for SharedGuard {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.buf.lock().expect("lock").extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for SharedWriter {
    type Writer = SharedGuard;

    fn make_writer(&'a self) -> Self::Writer {
        SharedGuard {
            buf: Arc::clone(&self.buf),
        }
    }
}

struct FailingPrefetchCache;

impl ProjectionCache for FailingPrefetchCache {
    fn capabilities(&self) -> CacheCapabilities {
        CacheCapabilities::prefetch_hints()
    }

    fn get(&self, _key: &[u8]) -> Result<Option<(Vec<u8>, CacheMeta)>, StoreError> {
        Ok(None)
    }

    fn put(&self, _key: &[u8], _value: &[u8], _meta: CacheMeta) -> Result<(), StoreError> {
        Err(StoreError::CacheFailed(
            "synthetic cache put failure".into(),
        ))
    }

    fn delete_prefix(&self, _prefix: &[u8]) -> Result<u64, StoreError> {
        Ok(0)
    }

    fn sync(&self) -> Result<(), StoreError> {
        Ok(())
    }

    fn prefetch(&self, _key: &[u8], _predicted_meta: CacheMeta) -> Result<(), StoreError> {
        Err(StoreError::CacheFailed(
            "synthetic cache prefetch failure".into(),
        ))
    }
}

#[test]
fn named_store_flows_emit_traceable_events() {
    let sink = SharedWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(sink.clone())
        .with_ansi(false)
        .without_time()
        .finish();

    tracing::subscriber::with_default(subscriber, || {
        let dir = TempDir::new().expect("temp dir");
        let config = StoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_bytes: 4096,
            sync: SyncConfig {
                every_n_events: 1,
                ..SyncConfig::default()
            },
            ..StoreConfig::new("")
        };
        let store = Store::open(config).expect("open store");
        let coord = Coordinate::new("entity:obs", "scope:test").expect("coord");
        let kind = EventKind::custom(0xF, 1);

        store
            .append(&coord, kind, &serde_json::json!({"n": 1}))
            .expect("append");
        store
            .append_with_options(
                &coord,
                kind,
                &serde_json::json!({"n": 2}),
                batpak::store::AppendOptions::new().with_idempotency(0xBEEF),
            )
            .expect("append_with_options");
        let _: Option<Counter> = store
            .project("entity:obs", &batpak::store::Freshness::Consistent)
            .expect("project");
        store.sync().expect("sync");
        let snapshot_dir = TempDir::new().expect("snapshot dir");
        store.snapshot(snapshot_dir.path()).expect("snapshot");
        let _ = store
            .compact(&CompactionConfig::default())
            .expect("compact");
        store.close().expect("close");
    });

    let output = String::from_utf8(sink.buf.lock().expect("lock").clone()).expect("utf8");
    for flow in [
        "append",
        "append_with_options",
        "project",
        "sync",
        "snapshot",
        "compact",
        "close",
    ] {
        assert!(
            output.contains(&format!("flow=\"{flow}\"")),
            "Expected tracing output for flow=\"{flow}\", got:\n{output}"
        );
    }
}

#[test]
fn project_failure_paths_emit_cache_warnings_without_hiding_flow() {
    let sink = SharedWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(sink.clone())
        .with_ansi(false)
        .without_time()
        .finish();

    tracing::subscriber::with_default(subscriber, || {
        let dir = TempDir::new().expect("temp dir");
        let config = StoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_bytes: 4096,
            sync: SyncConfig {
                every_n_events: 1,
                ..SyncConfig::default()
            },
            ..StoreConfig::new("")
        };
        let store = Store::open_with_cache(config, Box::new(FailingPrefetchCache))
            .expect("open store with failing cache");
        let coord = Coordinate::new("entity:obs:cache", "scope:test").expect("coord");
        let kind = EventKind::custom(0xF, 1);
        store
            .append(&coord, kind, &serde_json::json!({"n": 1}))
            .expect("append");

        let _: Option<Counter> = store
            .project("entity:obs:cache", &Freshness::Consistent)
            .expect("project");
        store.close().expect("close");
    });

    let output = String::from_utf8(sink.buf.lock().expect("lock").clone()).expect("utf8");
    assert!(
        output.contains("flow=\"project\""),
        "Expected project flow telemetry even when cache helpers fail, got:\n{output}"
    );
    assert!(
        output.contains("cache prefetch failed (non-fatal)")
            && output.contains("cache put failed (non-fatal)"),
        "Expected cache failure telemetry for best-effort project path, got:\n{output}"
    );
}

#[test]
fn append_reaction_emits_distinct_flow_telemetry() {
    let sink = SharedWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(sink.clone())
        .with_ansi(false)
        .without_time()
        .finish();

    tracing::subscriber::with_default(subscriber, || {
        let dir = TempDir::new().expect("temp dir");
        let config = StoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_bytes: 4096,
            sync: SyncConfig {
                every_n_events: 1,
                ..SyncConfig::default()
            },
            ..StoreConfig::new("")
        };
        let store = Store::open(config).expect("open store");
        let coord = Coordinate::new("entity:obs:source", "scope:test").expect("coord");
        let kind = EventKind::custom(0xF, 1);
        let receipt = store
            .append(&coord, kind, &serde_json::json!({"n": 1}))
            .expect("append");
        store
            .append_reaction(
                &Coordinate::new("entity:obs:reaction", "scope:test").expect("coord"),
                EventKind::custom(0xF, 2),
                &serde_json::json!({"from": receipt.event_id.to_string()}),
                receipt.event_id,
                receipt.event_id,
            )
            .expect("append reaction");
        store.close().expect("close");
    });

    let output = String::from_utf8(sink.buf.lock().expect("lock").clone()).expect("utf8");
    assert!(
        output.contains("flow=\"append_reaction\""),
        "Expected append_reaction flow telemetry from the causal append path, got:\n{output}"
    );
}
