// justifies: INV-TEST-PANIC-AS-ASSERTION; this frontier bootstrap harness uses panic! through assert macros for crisp invariant failures.
#![allow(clippy::panic)]
#![cfg(feature = "dangerous-test-hooks")]
//! PROVES:
//!   - INV-FRONTIER-MONOTONIC, INV-FRONTIER-ORDERING, INV-FRONTIER-TORN-FREE, INV-FRONTIER-OPEN-MONOTONIC, INV-FRONTIER-APPLIED-MIN, INV-FRONTIER-FAULT-ORDINALS.
//!   - Step-1 frontier scaffolding compiles and exposes a coherent dangerous snapshot.
//!   - Immediately after mutable `Store::open`, the lifecycle open event seeds
//!     accepted, written, durable, visible, and emitted to the same HLC point.
//!   - Restart bootstrap is monotonic across mutable and read-only reopen.
//!
//! CATCHES: missing handle plumbing, missing public accessor coverage, or a
//! bootstrap snapshot that does not reflect `SYSTEM_OPEN_COMPLETED`.
//!
//! SEEDED: deterministic tempdir-based open.

use batpak::coordinate::DagPosition;
use batpak::prelude::{
    Coordinate, Event, EventKind, EventSourced, Freshness, JsonValueInput, Region,
};
use batpak::store::{
    CountdownAction, CountdownInjector, FrontierView, HlcPoint, InjectionPoint, ReadOnly, Store,
    StoreConfig, StoreError,
};
use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Barrier;
use std::thread;
use tempfile::TempDir;

const FRONTIER_FAULT_ENTITY: &str = "entity:frontier-fault";

fn kind() -> EventKind {
    EventKind::custom(0xF, 0x90)
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct FrontierProjection {
    count: usize,
}

impl EventSourced for FrontierProjection {
    type Input = JsonValueInput;

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        (!events.is_empty()).then_some(Self {
            count: events.len(),
        })
    }

    fn apply_event(&mut self, _event: &Event<serde_json::Value>) {
        self.count += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        static KINDS: [EventKind; 1] = [EventKind::custom(0xF, 0x90)];
        &KINDS
    }
}

fn coord(entity: &str) -> Coordinate {
    Coordinate::new(entity, "scope:test").expect("coord")
}

fn point(entry: &batpak::store::index::IndexEntry) -> HlcPoint {
    HlcPoint {
        wall_ms: entry.wall_ms(),
        global_sequence: entry.global_sequence(),
    }
}

fn fixed_clock_config(dir: &TempDir, now_us: i64) -> StoreConfig {
    StoreConfig::new(dir.path()).with_clock_fn(move || now_us)
}

fn lifecycle_open_count<State>(store: &Store<State>) -> usize {
    store
        .query(&Region::entity("batpak:store"))
        .into_iter()
        .filter(|entry| entry.event_kind() == EventKind::SYSTEM_OPEN_COMPLETED)
        .count()
}

fn lifecycle_close_entries<State>(store: &Store<State>) -> Vec<batpak::store::index::IndexEntry> {
    store
        .query(&Region::entity("batpak:store"))
        .into_iter()
        .filter(|entry| entry.event_kind() == EventKind::SYSTEM_CLOSE_COMPLETED)
        .collect()
}

#[derive(serde::Serialize, serde::Deserialize)]
struct TestFramePayload {
    event: Event<Vec<u8>>,
    entity: String,
    scope: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CloseLifecyclePayload {
    wall_ms: u64,
    global_sequence: u64,
}

fn forge_close_hlc_regression(
    segment_dir: &Path,
    victim_close_hlc: HlcPoint,
    forged_close_hlc: HlcPoint,
) -> std::io::Result<()> {
    remove_fast_start_artifacts(segment_dir)?;
    let mut segment_paths = segment_paths(segment_dir)?;
    segment_paths.sort();

    for path in segment_paths {
        if rewrite_close_frame_if_present(&path, victim_close_hlc, forged_close_hlc)? {
            return Ok(());
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("no SYSTEM_CLOSE_COMPLETED frame found for HLC {victim_close_hlc:?}"),
    ))
}

fn remove_fast_start_artifacts(segment_dir: &Path) -> std::io::Result<()> {
    for artifact in ["index.ckpt", "index.fbati"] {
        match fs::remove_file(segment_dir.join(artifact)) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn segment_paths(segment_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(segment_dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "fbat") {
            paths.push(path);
        }
    }
    Ok(paths)
}

fn rewrite_close_frame_if_present(
    path: &Path,
    victim_close_hlc: HlcPoint,
    forged_close_hlc: HlcPoint,
) -> std::io::Result<bool> {
    let bytes = fs::read(path)?;
    let frames_end = sidx_frames_end(&bytes).unwrap_or(bytes.len());
    let frames_start = segment_frames_start(&bytes)?;
    let mut cursor = frames_start;
    let mut found = false;
    let mut rewritten = Vec::with_capacity(bytes.len());
    rewritten.extend_from_slice(&bytes[..frames_start]);

    while cursor < frames_end {
        let (msgpack, frame_len) = decode_frame(&bytes[cursor..frames_end])?;
        let mut payload: TestFramePayload =
            rmp_serde::from_slice(msgpack).map_err(std::io::Error::other)?;
        if payload.event.header.event_kind == EventKind::SYSTEM_CLOSE_COMPLETED
            && payload.event.header.position.wall_ms() == victim_close_hlc.wall_ms
        {
            forge_close_payload(&mut payload, forged_close_hlc)?;
            rewritten.extend_from_slice(&encode_frame(&payload)?);
            found = true;
        } else {
            rewritten.extend_from_slice(&bytes[cursor..cursor + frame_len]);
        }
        cursor += frame_len;
    }

    if !found {
        return Ok(false);
    }

    let mut file = OpenOptions::new().write(true).truncate(true).open(path)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&rewritten)?;
    file.flush()?;
    Ok(true)
}

fn segment_frames_start(bytes: &[u8]) -> std::io::Result<usize> {
    if bytes.len() < 8 || &bytes[..4] != b"FBAT" {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "segment file missing FBAT magic",
        ));
    }
    let header_len = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    8usize
        .checked_add(header_len)
        .filter(|start| *start <= bytes.len())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad segment header"))
}

fn sidx_frames_end(bytes: &[u8]) -> Option<usize> {
    const TRAILER_LEN: usize = 16;
    if bytes.len() < TRAILER_LEN || &bytes[bytes.len() - 4..] != b"SDX2" {
        return None;
    }
    let trailer = &bytes[bytes.len() - TRAILER_LEN..];
    let offset = u64::from_le_bytes([
        trailer[0], trailer[1], trailer[2], trailer[3], trailer[4], trailer[5], trailer[6],
        trailer[7],
    ]);
    usize::try_from(offset)
        .ok()
        .filter(|offset| *offset <= bytes.len() - TRAILER_LEN)
}

fn decode_frame(buf: &[u8]) -> std::io::Result<(&[u8], usize)> {
    if buf.len() < 8 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "frame too short for header",
        ));
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let expected_crc = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let frame_len = 8usize.checked_add(len).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "frame length overflow")
    })?;
    if buf.len() < frame_len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "frame payload truncated",
        ));
    }
    let msgpack = &buf[8..frame_len];
    let actual_crc = crc32fast::hash(msgpack);
    if actual_crc != expected_crc {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame CRC mismatch: expected {expected_crc:#010x}, got {actual_crc:#010x}"),
        ));
    }
    Ok((msgpack, frame_len))
}

fn encode_frame(payload: &TestFramePayload) -> std::io::Result<Vec<u8>> {
    let msgpack = rmp_serde::to_vec_named(payload).map_err(std::io::Error::other)?;
    let len = u32::try_from(msgpack.len()).map_err(std::io::Error::other)?;
    let crc = crc32fast::hash(&msgpack);
    let mut frame = Vec::with_capacity(8 + msgpack.len());
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&crc.to_be_bytes());
    frame.extend_from_slice(&msgpack);
    Ok(frame)
}

fn forge_close_payload(
    payload: &mut TestFramePayload,
    forged_close_hlc: HlcPoint,
) -> std::io::Result<()> {
    let position = payload.event.header.position;
    payload.event.header.position = DagPosition::with_hlc(
        forged_close_hlc.wall_ms,
        position.counter(),
        position.depth(),
        position.lane(),
        position.sequence(),
    );
    payload.event.header.timestamp_us =
        i64::try_from(forged_close_hlc.wall_ms.saturating_mul(1000)).unwrap_or(i64::MAX);
    payload.event.payload = rmp_serde::to_vec_named(&CloseLifecyclePayload {
        wall_ms: forged_close_hlc.wall_ms,
        global_sequence: forged_close_hlc.global_sequence,
    })
    .map_err(std::io::Error::other)?;
    payload.event.header.payload_size =
        u32::try_from(payload.event.payload.len()).map_err(std::io::Error::other)?;
    let event_hash = close_payload_hash(&payload.event.payload);
    if let Some(hash_chain) = &mut payload.event.hash_chain {
        hash_chain.event_hash = event_hash;
    }
    payload.event.header.content_hash = event_hash;
    Ok(())
}

fn close_payload_hash(payload: &[u8]) -> [u8; 32] {
    batpak::event::hash::compute_hash(payload)
}

fn config_with_fault(
    dir: &TempDir,
    filter: impl Fn(&InjectionPoint) -> bool + Send + Sync + 'static,
) -> StoreConfig {
    let injector =
        CountdownInjector::new(1, CountdownAction::Fail("single append fault")).with_filter(filter);
    StoreConfig::new(dir.path())
        .with_sync_every_n_events(1000)
        .with_fault_injector(Some(Arc::new(injector)))
}

fn assert_fault_injected(result: Result<batpak::store::AppendReceipt, StoreError>) {
    match result {
        Ok(_) => panic!("PROPERTY: append must surface the injected error, not a receipt"),
        Err(err) => assert!(
            matches!(err, StoreError::FaultInjected(ref message) if message.contains("single append fault")),
            "PROPERTY: expected injected fault, got {err:?}"
        ),
    }
}

#[test]
fn bootstrap_watermark_snapshot_matches_lifecycle_open_event() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");

    let snapshot: FrontierView = store.dangerous_watermark_snapshot();
    let frontier: FrontierView = store.diagnostics().frontier;
    let open_hlc = snapshot.durable_hlc;

    assert!(open_hlc > HlcPoint::ORIGIN);
    assert_eq!(open_hlc.global_sequence, 0);
    assert_eq!(snapshot.accepted_hlc, open_hlc);
    assert_eq!(snapshot.written_hlc, open_hlc);
    assert_eq!(snapshot.durable_hlc, open_hlc);
    assert_eq!(snapshot.visible_hlc, open_hlc);
    assert_eq!(snapshot.emitted_hlc, open_hlc);
    assert_eq!(snapshot.applied_hlc, open_hlc);
    assert_eq!(snapshot.oldest_pending_write_age_ms, None);

    assert_eq!(frontier.durable_hlc, open_hlc);
    assert_eq!(frontier.visible_hlc, open_hlc);
    assert_eq!(frontier.visible_minus_durable_seq, 0);
    assert_eq!(frontier.oldest_pending_write_age_ms, None);
}

#[test]
fn open_after_close_advances_open_hlc_past_max_pre_close() {
    let dir = TempDir::new().expect("temp dir");
    let coord = coord("entity:frontier-reopen");

    let max_hlc_before_close = {
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        store
            .append(&coord, kind(), &serde_json::json!({"n": 1}))
            .expect("append");
        let entries = store.query(&Region::entity("entity:frontier-reopen"));
        assert_eq!(entries.len(), 1);
        let max_hlc = point(&entries[0]);
        store.close().expect("close");
        max_hlc
    };

    let reopened = Store::open(StoreConfig::new(dir.path())).expect("reopen store");
    let snapshot = reopened.dangerous_watermark_snapshot();
    let open_hlc = snapshot.accepted_hlc;

    assert!(
        open_hlc > max_hlc_before_close,
        "PROPERTY: mutable reopen lifecycle HLC must advance past pre-close max; open={open_hlc:?}, max={max_hlc_before_close:?}"
    );
    assert_eq!(snapshot.written_hlc, open_hlc);
    assert_eq!(snapshot.durable_hlc, open_hlc);
    assert_eq!(snapshot.visible_hlc, open_hlc);
    assert_eq!(snapshot.emitted_hlc, open_hlc);
    assert_eq!(snapshot.applied_hlc, open_hlc);
}

#[test]
fn read_only_reopen_does_not_emit_lifecycle_event() {
    let dir = TempDir::new().expect("temp dir");
    let coord = coord("entity:frontier-readonly-lifecycle");

    let (max_hlc_before_read_only, lifecycle_count_before_read_only) = {
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        store
            .append(&coord, kind(), &serde_json::json!({"n": 1}))
            .expect("append");
        let entries = store.query(&Region::entity("entity:frontier-readonly-lifecycle"));
        assert_eq!(entries.len(), 1);
        let max_hlc = point(&entries[0]);
        let lifecycle_count = lifecycle_open_count(&store);
        assert_eq!(lifecycle_count, 1);
        store.close().expect("close");
        (max_hlc, lifecycle_count)
    };

    let read_only =
        Store::<ReadOnly>::open_read_only(StoreConfig::new(dir.path())).expect("open read-only");
    let snapshot = read_only.dangerous_watermark_snapshot();

    assert_eq!(
        lifecycle_open_count(&read_only),
        lifecycle_count_before_read_only,
        "PROPERTY: read-only open must not append SYSTEM_OPEN_COMPLETED"
    );
    assert!(snapshot.accepted_hlc >= max_hlc_before_read_only);
    assert_eq!(snapshot.written_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.durable_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.visible_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.emitted_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.applied_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.oldest_pending_write_age_ms, None);
}

#[test]
fn explicit_close_emits_system_close_completed_event() {
    let dir = TempDir::new().expect("temp dir");
    let coord = coord("entity:frontier-close-event");

    let max_hlc_before_close = {
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        store
            .append(&coord, kind(), &serde_json::json!({"n": 1}))
            .expect("append");
        let entries = store.query(&Region::entity("entity:frontier-close-event"));
        assert_eq!(entries.len(), 1);
        let max_hlc = point(&entries[0]);
        store.close().expect("close");
        max_hlc
    };

    let read_only =
        Store::<ReadOnly>::open_read_only(StoreConfig::new(dir.path())).expect("open read-only");
    let close_entries = lifecycle_close_entries(&read_only);

    assert_eq!(
        close_entries.len(),
        1,
        "PROPERTY: explicit close must emit exactly one SYSTEM_CLOSE_COMPLETED event"
    );
    assert!(
        point(&close_entries[0]) >= max_hlc_before_close,
        "PROPERTY: close lifecycle HLC must cover all visible events at close; close={:?}, max={max_hlc_before_close:?}",
        point(&close_entries[0])
    );
}

#[test]
fn drop_without_explicit_close_emits_no_close_event() {
    let dir = TempDir::new().expect("temp dir");
    let coord = coord("entity:frontier-drop-no-close");

    {
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        store
            .append(&coord, kind(), &serde_json::json!({"n": 1}))
            .expect("append");
    }

    {
        let read_only = Store::<ReadOnly>::open_read_only(StoreConfig::new(dir.path()))
            .expect("open read-only");
        assert!(
            lifecycle_close_entries(&read_only).is_empty(),
            "PROPERTY: Drop must not emit SYSTEM_CLOSE_COMPLETED"
        );
    }

    let reopened = Store::open(StoreConfig::new(dir.path())).expect("reopen store");
    assert!(
        reopened.frontier().accepted_hlc > HlcPoint::ORIGIN,
        "PROPERTY: reopen without a close event must still bootstrap from recovered events and wall-time floor"
    );
}

#[test]
fn bootstrap_open_hlc_consumes_recorded_close_hlc() {
    let dir = TempDir::new().expect("temp dir");
    let coord = coord("entity:frontier-close-bootstrap");

    let close_hlc_1 = {
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        store
            .append(&coord, kind(), &serde_json::json!({"n": 1}))
            .expect("append");
        store.close().expect("close");

        let read_only = Store::<ReadOnly>::open_read_only(StoreConfig::new(dir.path()))
            .expect("open read-only");
        let close_entries = lifecycle_close_entries(&read_only);
        assert_eq!(close_entries.len(), 1);
        point(&close_entries[0])
    };

    let close_hlc_2 = {
        let store = Store::open(StoreConfig::new(dir.path())).expect("reopen store");
        assert!(
            store.frontier().accepted_hlc >= close_hlc_1,
            "PROPERTY: reopen must consume the recorded close frontier"
        );
        store
            .append(&coord, kind(), &serde_json::json!({"n": 2}))
            .expect("append");
        store.close().expect("close");

        let read_only = Store::<ReadOnly>::open_read_only(StoreConfig::new(dir.path()))
            .expect("open read-only");
        let close_entries = lifecycle_close_entries(&read_only);
        assert_eq!(close_entries.len(), 2);
        let first = point(&close_entries[0]);
        let second = point(&close_entries[1]);
        assert!(
            second >= first,
            "PROPERTY: repeated graceful closes must advance monotonically; first={first:?}, second={second:?}"
        );
        second
    };

    let third = Store::open(StoreConfig::new(dir.path())).expect("third open");
    let open_hlc = third.frontier().accepted_hlc;
    assert!(open_hlc >= close_hlc_1);
    assert!(open_hlc >= close_hlc_2);
}

#[test]
fn close_hlc_monotonicity_violation_surfaces_invariant_violation() {
    let dir = TempDir::new().expect("temp dir");
    let coord = coord("entity:frontier-close-regression");

    {
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        store
            .append(&coord, kind(), &serde_json::json!({"n": 1}))
            .expect("append first lifecycle event");
        store.close().expect("close first lifecycle");
    }

    {
        let store = Store::open(StoreConfig::new(dir.path())).expect("reopen store");
        store
            .append(&coord, kind(), &serde_json::json!({"n": 2}))
            .expect("append second lifecycle event");
        store.close().expect("close second lifecycle");
    }

    let read_only =
        Store::<ReadOnly>::open_read_only(StoreConfig::new(dir.path())).expect("open read-only");
    let close_entries = lifecycle_close_entries(&read_only);
    assert_eq!(close_entries.len(), 2);
    let close_hlc_1 = point(&close_entries[0]);
    let close_hlc_2 = point(&close_entries[1]);
    assert!(
        close_hlc_2 > close_hlc_1,
        "PROPERTY: normal close lifecycle must be monotonic before forging"
    );
    drop(read_only);

    let forged = HlcPoint {
        wall_ms: close_hlc_1.wall_ms.saturating_sub(1_000),
        global_sequence: close_hlc_2.global_sequence,
    };
    assert!(
        forged < close_hlc_1,
        "PROPERTY: forged later close HLC must regress below first close"
    );
    forge_close_hlc_regression(dir.path(), close_hlc_2, forged)
        .expect("forge close_hlc regression");

    let err = match Store::open(StoreConfig::new(dir.path())) {
        Ok(_) => {
            panic!("PROPERTY: opening with regressed close_hlc must fail with InvariantViolation")
        }
        Err(error) => error,
    };
    assert!(
        matches!(err, StoreError::InvariantViolation { .. }),
        "wrong error variant: {err:?}"
    );
    let message = err.to_string();
    assert!(
        message.contains(&close_hlc_1.wall_ms.to_string()),
        "error must cite earlier close_hlc wall_ms {}; got: {message}",
        close_hlc_1.wall_ms
    );
    assert!(
        message.contains(&forged.wall_ms.to_string()),
        "error must cite forged later close_hlc wall_ms {}; got: {message}",
        forged.wall_ms
    );
}

#[test]
fn bootstrap_with_clock_skew_preserves_monotonicity() {
    let dir = TempDir::new().expect("temp dir");
    let coord = coord("entity:frontier-clock-skew");

    let max_hlc_before_close = {
        let store = Store::open(fixed_clock_config(&dir, 9_000_000_000)).expect("open store");
        store
            .append(&coord, kind(), &serde_json::json!({"n": 1}))
            .expect("append");
        let entries = store.query(&Region::entity("entity:frontier-clock-skew"));
        assert_eq!(entries.len(), 1);
        let max_hlc = point(&entries[0]);
        store.close().expect("close");
        max_hlc
    };

    let reopened = Store::open(fixed_clock_config(&dir, 1_000_000)).expect("reopen store");
    let open_hlc = reopened.dangerous_watermark_snapshot().accepted_hlc;

    assert!(
        open_hlc > max_hlc_before_close,
        "PROPERTY: reopen must remain monotonic even when the configured clock moves backward; open={open_hlc:?}, max={max_hlc_before_close:?}"
    );
}

#[test]
fn empty_store_open_starts_with_lifecycle_frontier_then_append_advances() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    let open_hlc = store.dangerous_watermark_snapshot().accepted_hlc;
    assert!(open_hlc > HlcPoint::ORIGIN);
    assert_eq!(open_hlc.global_sequence, 0);

    let coord = coord("entity:frontier-empty-advance");
    store
        .append(&coord, kind(), &serde_json::json!({"n": 1}))
        .expect("append");
    let snapshot = store.dangerous_watermark_snapshot();
    assert!(snapshot.accepted_hlc > open_hlc);
}

#[test]
fn single_append_cadence_gt_1_visible_exceeds_durable_frontier() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_sync_every_n_events(1000);
    let store = Store::open(config).expect("open store");
    let bootstrap = store.dangerous_watermark_snapshot();
    let coord = coord("entity:frontier");

    let receipt = store
        .append(&coord, kind(), &serde_json::json!({"n": 1}))
        .expect("append");

    let visible = store.query(&Region::entity("entity:frontier"));
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].event_id(), u128::from(receipt.event_id));

    let snapshot = store.dangerous_watermark_snapshot();
    let frontier = store.diagnostics().frontier;

    assert!(snapshot.visible_hlc > snapshot.durable_hlc);
    assert!(snapshot.accepted_hlc >= snapshot.written_hlc);
    assert!(snapshot.written_hlc >= snapshot.visible_hlc);
    assert_eq!(snapshot.durable_hlc, bootstrap.durable_hlc);
    assert_eq!(snapshot.applied_hlc, bootstrap.applied_hlc);
    assert_eq!(snapshot.emitted_hlc, snapshot.visible_hlc);
    assert!(snapshot.oldest_pending_write_age_ms.is_some());

    assert_eq!(frontier.visible_hlc, snapshot.visible_hlc);
    assert_eq!(frontier.durable_hlc, snapshot.durable_hlc);
    assert!(frontier.visible_minus_durable_seq > 0);
    assert!(frontier.oldest_pending_write_age_ms.is_some());
}

#[test]
fn explicit_sync_advances_durable_and_clears_pending_write_age() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_sync_every_n_events(1000);
    let store = Store::open(config).expect("open store");
    let coord = coord("entity:frontier-sync");

    store
        .append(&coord, kind(), &serde_json::json!({"n": 1}))
        .expect("append");

    let before_sync = store.dangerous_watermark_snapshot();
    assert!(before_sync.visible_hlc > before_sync.durable_hlc);
    assert!(before_sync.oldest_pending_write_age_ms.is_some());

    store.sync().expect("sync");

    let after_sync = store.dangerous_watermark_snapshot();
    assert_eq!(after_sync.durable_hlc, after_sync.accepted_hlc);
    assert_eq!(after_sync.durable_hlc, after_sync.visible_hlc);
    assert_eq!(after_sync.oldest_pending_write_age_ms, None);
    assert_eq!(
        store.diagnostics().frontier.oldest_pending_write_age_ms,
        None
    );
}

#[test]
fn frontier_api_is_public_and_returns_consistent_view() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_sync_every_n_events(1);
    let store = Store::open(config).expect("open store");
    let coord = coord("entity:frontier-api");

    for n in 0..5 {
        store
            .append(&coord, kind(), &serde_json::json!({"n": n}))
            .expect("append");
    }
    store.sync().expect("sync");

    let frontier = store.frontier();
    assert!(frontier.accepted_hlc > HlcPoint::ORIGIN);
    assert_eq!(frontier.accepted_hlc, frontier.written_hlc);
    assert_eq!(frontier.accepted_hlc, frontier.durable_hlc);
    assert_eq!(frontier.accepted_hlc, frontier.visible_hlc);
    assert_eq!(frontier.emitted_hlc, frontier.visible_hlc);
    assert!(frontier.visible_hlc >= frontier.applied_hlc);
    assert_eq!(frontier.visible_minus_durable_seq, 0);
    assert_eq!(frontier.oldest_pending_write_age_ms, None);
    assert_eq!(store.diagnostics().frontier, frontier);
}

#[test]
fn frontier_visible_minus_durable_seq_is_positive_under_cadence_gt_1() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_sync_every_n_events(1000);
    let store = Store::open(config).expect("open store");
    let coord = coord("entity:frontier-api-gap");

    for n in 0..10 {
        store
            .append(&coord, kind(), &serde_json::json!({"n": n}))
            .expect("append");
    }

    let before_sync = store.frontier();
    assert!(before_sync.visible_hlc > before_sync.durable_hlc);
    assert!(before_sync.visible_minus_durable_seq > 0);
    assert!(before_sync.oldest_pending_write_age_ms.is_some());

    store.sync().expect("sync");

    let after_sync = store.frontier();
    assert_eq!(after_sync.visible_hlc, after_sync.durable_hlc);
    assert_eq!(after_sync.visible_minus_durable_seq, 0);
    assert_eq!(after_sync.oldest_pending_write_age_ms, None);
}

#[test]
fn concurrent_snapshot_never_observes_torn_emitted_below_visible() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_sync_every_n_events(1000);
    let store = Arc::new(Store::open(config).expect("open store"));
    let coord = coord("entity:frontier-concurrent");
    let start = Arc::new(Barrier::new(2));
    let done = Arc::new(AtomicBool::new(false));

    let observer_store = Arc::clone(&store);
    let observer_start = Arc::clone(&start);
    let observer_done = Arc::clone(&done);
    // Intentional: barrier waits coordinate exactly one observer and one writer.
    let observer = thread::Builder::new()
        .name("frontier-snapshot-observer".to_string())
        .spawn(move || {
            observer_start.wait();
            let mut snapshots = Vec::new();
            while !observer_done.load(Ordering::Acquire) {
                let frontier = observer_store.frontier();
                if frontier.visible_hlc > HlcPoint::ORIGIN {
                    snapshots.push(frontier);
                }
                thread::yield_now();
            }
            for _ in 0..256 {
                let frontier = observer_store.frontier();
                if frontier.visible_hlc > HlcPoint::ORIGIN {
                    snapshots.push(frontier);
                }
            }
            snapshots
        })
        .expect("spawn frontier observer");

    start.wait();
    for n in 0..300 {
        store
            .append(&coord, kind(), &serde_json::json!({"n": n}))
            .expect("append");
        if n % 8 == 0 {
            thread::yield_now();
        }
    }
    done.store(true, Ordering::Release);

    let snapshots = observer.join().expect("observer thread");
    assert!(
        !snapshots.is_empty(),
        "PROPERTY: concurrent observer must collect frontier snapshots"
    );
    for frontier in snapshots {
        assert!(
            frontier.emitted_hlc >= frontier.visible_hlc,
            "PROPERTY: emitted must never be observed below visible: {frontier:?}"
        );
        assert!(
            frontier.visible_hlc >= frontier.applied_hlc,
            "PROPERTY: applied must never be observed above visible: {frontier:?}"
        );
    }
}

#[test]
fn read_only_open_bootstraps_frontier_from_rebuilt_index() {
    let dir = TempDir::new().expect("temp dir");
    let coord = coord("entity:frontier-readonly");

    let max_hlc_before_close = {
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        store
            .append(&coord, kind(), &serde_json::json!({"n": 1}))
            .expect("append");
        let entries = store.query(&Region::entity("entity:frontier-readonly"));
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        let point = HlcPoint {
            wall_ms: entry.wall_ms(),
            global_sequence: entry.global_sequence(),
        };
        assert!(point > HlcPoint::ORIGIN);
        store.close().expect("close");
        point
    };

    let read_only =
        Store::<ReadOnly>::open_read_only(StoreConfig::new(dir.path())).expect("open read-only");
    let snapshot = read_only.dangerous_watermark_snapshot();

    assert!(snapshot.accepted_hlc >= max_hlc_before_close);
    assert_eq!(snapshot.written_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.durable_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.visible_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.emitted_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.applied_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.oldest_pending_write_age_ms, None);
}

#[test]
fn applied_starts_at_open_hlc_when_no_projections_registered() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    let open_hlc = store.dangerous_watermark_snapshot().applied_hlc;
    let coord = coord("entity:frontier-applied-none");

    for n in 0..3 {
        store
            .append(&coord, kind(), &serde_json::json!({"n": n}))
            .expect("append");
    }

    let snapshot = store.dangerous_watermark_snapshot();
    assert_eq!(
        snapshot.applied_hlc, open_hlc,
        "PROPERTY: without registered projections, applied remains at the bootstrap frontier"
    );
    assert_ne!(snapshot.applied_hlc, snapshot.emitted_hlc);
}

#[test]
fn applied_advances_with_single_projection() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    let coord = coord("entity:frontier-applied-one");
    store.dangerous_register_projection_for::<FrontierProjection>("entity:frontier-applied-one");

    for n in 0..3 {
        store
            .append(&coord, kind(), &serde_json::json!({"n": n}))
            .expect("append");
    }

    let projected = store
        .project::<FrontierProjection>("entity:frontier-applied-one", &Freshness::Consistent)
        .expect("project")
        .expect("projection state");
    assert_eq!(projected.count, 3);

    let snapshot = store.dangerous_watermark_snapshot();
    let frontier = store.diagnostics().frontier;
    assert_eq!(snapshot.applied_hlc, snapshot.emitted_hlc);
    assert_eq!(frontier.applied_hlc, snapshot.applied_hlc);
}

#[test]
fn applied_is_min_across_two_projections() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    let coord = coord("entity:frontier-applied-two");

    for n in 0..5 {
        store
            .append(&coord, kind(), &serde_json::json!({"n": n}))
            .expect("append");
    }

    let entries = store.query(&Region::entity("entity:frontier-applied-two"));
    assert_eq!(entries.len(), 5);
    let second_event = point(&entries[1]);
    let fifth_event = point(&entries[4]);

    store.dangerous_register_projection("frontier:p1");
    store.dangerous_register_projection("frontier:p2");
    store.dangerous_notify_projection_applied("frontier:p1", fifth_event);
    store.dangerous_notify_projection_applied("frontier:p2", second_event);

    let snapshot = store.dangerous_watermark_snapshot();
    assert_eq!(snapshot.applied_hlc, second_event);
    assert_ne!(snapshot.applied_hlc, fifth_event);

    store.dangerous_notify_projection_applied("frontier:p2", fifth_event);
    assert_eq!(
        store.dangerous_watermark_snapshot().applied_hlc,
        fifth_event
    );
}

#[test]
fn applied_unregister_recomputes_from_remaining_projection_progress() {
    fn run_case(unregister_fast_first: bool) {
        let dir = TempDir::new().expect("temp dir");
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        let coord = coord(if unregister_fast_first {
            "entity:frontier-unregister-fast"
        } else {
            "entity:frontier-unregister-slow"
        });

        for n in 0..5 {
            store
                .append(&coord, kind(), &serde_json::json!({"n": n}))
                .expect("append");
        }

        let entries = store.query(&Region::entity(coord.entity()));
        assert_eq!(entries.len(), 5);
        let slow = point(&entries[1]);
        let fast = point(&entries[4]);

        store.dangerous_register_projection("frontier:fast");
        store.dangerous_register_projection("frontier:slow");
        store.dangerous_notify_projection_applied("frontier:fast", fast);
        store.dangerous_notify_projection_applied("frontier:slow", slow);
        assert_eq!(store.dangerous_watermark_snapshot().applied_hlc, slow);

        if unregister_fast_first {
            store.dangerous_unregister_projection("frontier:fast");
            assert_eq!(store.dangerous_watermark_snapshot().applied_hlc, slow);
        } else {
            store.dangerous_unregister_projection("frontier:slow");
            assert_eq!(store.dangerous_watermark_snapshot().applied_hlc, fast);
        }
    }

    run_case(true);
    run_case(false);
}

#[test]
fn single_append_start_fault_fires_before_watermarks_advance() {
    let dir = TempDir::new().expect("temp dir");
    let config = config_with_fault(&dir, |point| {
        matches!(
            point,
            InjectionPoint::SingleAppendStart { entity }
                if entity == FRONTIER_FAULT_ENTITY
        )
    });
    let store = Store::open(config).expect("open store");
    let bootstrap = store.dangerous_watermark_snapshot();
    let coord = coord(FRONTIER_FAULT_ENTITY);

    assert_fault_injected(store.append(&coord, kind(), &serde_json::json!({"n": 1})));

    let snapshot = store.dangerous_watermark_snapshot();
    assert_eq!(snapshot, bootstrap);
    assert!(store
        .query(&Region::entity(FRONTIER_FAULT_ENTITY))
        .is_empty());
}

#[test]
fn single_append_written_fault_fires_after_written_before_visible() {
    let dir = TempDir::new().expect("temp dir");
    let config = config_with_fault(&dir, |point| {
        matches!(
            point,
            InjectionPoint::SingleAppendWritten { entity }
                if entity == FRONTIER_FAULT_ENTITY
        )
    });
    let store = Store::open(config).expect("open store");
    let bootstrap = store.dangerous_watermark_snapshot();
    let coord = coord(FRONTIER_FAULT_ENTITY);

    assert_fault_injected(store.append(&coord, kind(), &serde_json::json!({"n": 1})));

    let snapshot = store.dangerous_watermark_snapshot();
    assert!(snapshot.accepted_hlc > bootstrap.accepted_hlc);
    assert_eq!(snapshot.written_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.durable_hlc, bootstrap.durable_hlc);
    assert_eq!(snapshot.visible_hlc, bootstrap.visible_hlc);
    assert_eq!(snapshot.emitted_hlc, bootstrap.emitted_hlc);
    assert!(snapshot.oldest_pending_write_age_ms.is_some());
    assert!(store
        .query(&Region::entity(FRONTIER_FAULT_ENTITY))
        .is_empty());
}

#[test]
fn single_append_published_fault_fires_after_visibility_before_receipt() {
    let dir = TempDir::new().expect("temp dir");
    let config = config_with_fault(&dir, |point| {
        matches!(
            point,
            InjectionPoint::SingleAppendPublished { entity }
                if entity == FRONTIER_FAULT_ENTITY
        )
    });
    let store = Store::open(config).expect("open store");
    let bootstrap = store.dangerous_watermark_snapshot();
    let coord = coord(FRONTIER_FAULT_ENTITY);

    assert_fault_injected(store.append(&coord, kind(), &serde_json::json!({"n": 1})));

    let visible = store.query(&Region::entity(FRONTIER_FAULT_ENTITY));
    assert_eq!(
        visible.len(),
        1,
        "PROPERTY: published injection fires after query visibility"
    );

    let snapshot = store.dangerous_watermark_snapshot();
    assert!(snapshot.visible_hlc > snapshot.durable_hlc);
    assert_eq!(snapshot.durable_hlc, bootstrap.durable_hlc);
    assert_eq!(snapshot.emitted_hlc, snapshot.visible_hlc);
    assert_eq!(snapshot.written_hlc, snapshot.visible_hlc);
    assert_eq!(snapshot.accepted_hlc, snapshot.visible_hlc);
    assert!(snapshot.oldest_pending_write_age_ms.is_some());
}
