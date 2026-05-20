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

use batpak::prelude::{
    Coordinate, DagPosition, Event, EventKind, EventSourced, Freshness, JsonValueInput, Region,
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
