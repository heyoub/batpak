//! Observability proofs for named store flows.
//! PROVES: INV-OBSERVABILITY-FAILURE-PATHS, INV-OPEN-REPORT-RECEIPT.
//!
//! Tests in this module use `#[serial_test::serial(observability_flows)]` so
//! `stable_batpak_targets_field_shape_at_info_and_trace` can install a global
//! `tracing` subscriber once: writer-thread events (`batpak::fanout`, etc.) do
//! not inherit scoped `with_default` dispatchers from the test thread.

use batpak::coordinate::Region;
mod support;
use batpak::store::{
    AppendOptions, CacheCapabilities, CacheMeta, CompactionConfig, DurabilityGate, Freshness,
    ProjectionCache, Store, StoreConfig, StoreError, WatermarkKind,
};
use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, LazyLock, Mutex, Once};
use support::prelude::*;
use tempfile::TempDir;
use tracing::field::Visit;
use tracing::Subscriber;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::Registry;
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Debug, Clone)]
struct CapturedEvent {
    target: String,
    level: tracing::Level,
    fields: BTreeMap<String, String>,
}

struct CaptureVisit(BTreeMap<String, String>);

impl Visit for CaptureVisit {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.insert(field.name().to_string(), value.to_string());
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.0.insert(field.name().to_string(), value.to_string());
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.0.insert(field.name().to_string(), value.to_string());
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0
            .entry(field.name().to_string())
            .or_insert_with(|| format!("{value:?}"));
    }
}

struct CaptureLayer {
    out: Arc<Mutex<Vec<CapturedEvent>>>,
}

impl<S: Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = CaptureVisit(BTreeMap::new());
        event.record(&mut visitor);
        self.out.lock().expect("lock").push(CapturedEvent {
            target: event.metadata().target().to_string(),
            level: *event.metadata().level(),
            fields: visitor.0,
        });
    }
}

/// Writer-thread `tracing` events (for example `batpak::fanout`) only reach a
/// **global** default subscriber. Scoped `with_default` is thread-local and
/// misses the store writer thread, so we install a global `Registry` once and
/// record structured fields per event.
static CAPTURED_EVENTS: LazyLock<Arc<Mutex<Vec<CapturedEvent>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(Vec::new())));

static GLOBAL_TRACE_SUBSCRIBER: Once = Once::new();

fn ensure_global_trace_subscriber() {
    GLOBAL_TRACE_SUBSCRIBER.call_once(|| {
        let out = Arc::clone(&*CAPTURED_EVENTS);
        Registry::default()
            .with(LevelFilter::TRACE)
            .with(CaptureLayer { out })
            .try_init()
            .expect(
                "observability_flows must install the global trace subscriber first in this test binary",
            );
    });
}

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
#[serial_test::serial(observability_flows)]
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
        let config = StoreConfig::new(dir.path())
            .with_segment_max_bytes(4096)
            .with_sync_every_n_events(1);
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
                batpak::store::AppendOptions::new()
                    .with_idempotency(batpak::id::IdempotencyKey::from(0xBEEF)),
            )
            .expect("append_with_options");
        let _: Option<Counter> = store
            .project("entity:obs", &batpak::store::Freshness::Consistent)
            .expect("project");
        store.sync().expect("sync");
        let snapshot_dir = TempDir::new().expect("snapshot dir");
        store
            .snapshot_with_evidence(snapshot_dir.path())
            .expect("snapshot");
        let (_result, _report) = store
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
    assert!(
        !output.contains("projection returned None despite non-empty filtered event stream"),
        "Healthy full replay should not emit the projection anomaly log, got:\n{output}"
    );
}

#[test]
#[serial_test::serial(observability_flows)]
fn stable_batpak_targets_field_shape_at_info_and_trace() {
    ensure_global_trace_subscriber();
    CAPTURED_EVENTS.lock().expect("lock").clear();

    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_segment_max_bytes(4096)
        .with_sync_every_n_events(1);
    let store = Store::open(config).expect("open store");
    let _sub = store.subscribe_lossy(&Region::all());

    let coord = Coordinate::new("entity:obs:trace", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 1);
    store
        .append(&coord, kind, &serde_json::json!({"n": 1}))
        .expect("append");

    let point = store.frontier().visible_hlc;
    store
        .wait_for_visible(point, std::time::Duration::from_secs(2))
        .expect("wait_for_visible");

    store
        .append_with_options(
            &coord,
            kind,
            &serde_json::json!({"n": 2}),
            AppendOptions::new().with_gate(DurabilityGate {
                kind: WatermarkKind::Visible,
                timeout: std::time::Duration::from_secs(2),
            }),
        )
        .expect("append_with_gate");

    let _: Option<Counter> = store
        .project("entity:obs:trace", &Freshness::Consistent)
        .expect("project");

    store.close().expect("close");

    let cap = CAPTURED_EVENTS.lock().expect("lock");
    let open_line: Vec<&CapturedEvent> = cap
        .iter()
        .filter(|e| e.target == "batpak::open" && e.level == tracing::Level::INFO)
        .collect();
    assert!(
        open_line
            .iter()
            .any(|e| e.fields.contains_key("elapsed_us")),
        "expected info-level batpak::open with elapsed_us, got {open_line:?}"
    );

    let frontier: Vec<&CapturedEvent> = cap
        .iter()
        .filter(|e| e.target == "batpak::frontier_wait")
        .collect();
    assert!(
        !frontier.is_empty(),
        "expected batpak::frontier_wait events, got {cap:?}"
    );
    for e in &frontier {
        assert!(
            e.fields.contains_key("watermark"),
            "frontier_wait missing watermark on same event: {e:?}"
        );
        assert!(
            e.fields.contains_key("waited_us"),
            "frontier_wait missing waited_us on same event: {e:?}"
        );
    }

    let gate: Vec<&CapturedEvent> = cap
        .iter()
        .filter(|e| e.target == "batpak::durability_gate")
        .collect();
    assert!(
        !gate.is_empty(),
        "expected batpak::durability_gate events, got {cap:?}"
    );
    for e in &gate {
        assert!(
            e.fields.contains_key("kind")
                && e.fields.contains_key("waited_us")
                && e.fields.contains_key("ok"),
            "durability_gate missing kind/waited_us/ok on same event: {e:?}"
        );
    }

    let fan: Vec<&CapturedEvent> = cap
        .iter()
        .filter(|e| e.target == "batpak::fanout")
        .collect();
    assert!(
        !fan.is_empty(),
        "expected batpak::fanout events, got {cap:?}"
    );
    assert!(
        fan.iter().any(|e| {
            e.fields.contains_key("push_notifications")
                || e.fields.contains_key("subscribers_before")
        }),
        "fanout events should carry push_notifications and/or subscribers_before: {fan:?}"
    );

    let proj_main: Vec<&CapturedEvent> = cap
        .iter()
        .filter(|e| {
            e.target == "batpak::projection" && e.fields.get("flow").is_some_and(|f| f == "project")
        })
        .collect();
    assert!(
        !proj_main.is_empty(),
        "expected batpak::projection flow=project events, got {cap:?}"
    );
    for e in &proj_main {
        assert!(
            e.fields.contains_key("cache_status")
                && e.fields.contains_key("total_us")
                && e.fields.contains_key("returned_generation"),
            "projection project trace missing cache_status/total_us/returned_generation: {e:?}"
        );
    }

    for e in cap.iter().filter(|e| {
        e.target == "batpak::projection"
            && e.fields
                .get("flow")
                .is_some_and(|f| f.contains("external_cache"))
    }) {
        assert!(
            e.fields.contains_key("probe_us") && e.fields.contains_key("outcome"),
            "external_cache_probe trace missing probe_us/outcome: {e:?}"
        );
    }
}

#[test]
#[serial_test::serial(observability_flows)]
fn external_cache_projection_probe_event_is_required_on_external_cache_path() {
    ensure_global_trace_subscriber();
    CAPTURED_EVENTS.lock().expect("lock").clear();

    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_segment_max_bytes(4096)
        .with_sync_every_n_events(1);
    let store = Store::open_with_cache(config, Box::new(FailingPrefetchCache)).expect("open store");
    let coord = Coordinate::new("entity:obs:external-cache", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 1);
    store
        .append(&coord, kind, &serde_json::json!({"n": 1}))
        .expect("append");

    let _: Option<Counter> = store
        .project("entity:obs:external-cache", &Freshness::Consistent)
        .expect("project");
    store.close().expect("close");

    let cap = CAPTURED_EVENTS.lock().expect("lock");
    let probe: Vec<&CapturedEvent> = cap
        .iter()
        .filter(|e| {
            e.target == "batpak::projection"
                && e.fields
                    .get("flow")
                    .is_some_and(|f| f == "external_cache_probe")
        })
        .collect();
    assert!(
        !probe.is_empty(),
        "external-cache projection path must emit flow=external_cache_probe, got {cap:?}"
    );
    assert!(
        probe.iter().any(|e| {
            e.fields
                .get("entity")
                .is_some_and(|entity| entity == "entity:obs:external-cache")
                && e.fields
                    .get("outcome")
                    .is_some_and(|outcome| outcome == "none")
                && e.fields.contains_key("probe_us")
        }),
        "external_cache_probe must carry entity/probe_us/outcome=none on the same event: {probe:?}"
    );
}

#[test]
#[serial_test::serial(observability_flows)]
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
        let config = StoreConfig::new(dir.path())
            .with_segment_max_bytes(4096)
            .with_sync_every_n_events(1);
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
#[serial_test::serial(observability_flows)]
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
        let config = StoreConfig::new(dir.path())
            .with_segment_max_bytes(4096)
            .with_sync_every_n_events(1);
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
                batpak::id::CorrelationId::from(u128::from(receipt.event_id)),
                batpak::id::CausationId::from(u128::from(receipt.event_id)),
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
