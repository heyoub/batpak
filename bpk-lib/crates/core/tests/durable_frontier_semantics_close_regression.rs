#![cfg(feature = "dangerous-test-hooks")]
//! PROVES: INV-FRONTIER-MONOTONIC for the persisted `SYSTEM_CLOSE_COMPLETED`
//! close frontier: a later close frame whose HLC regresses below an earlier
//! close must be rejected on reopen. Reopening a store whose newest close frame
//! has been forged backwards fails with `StoreError::InvariantViolation`, citing
//! both the earlier genuine close `wall_ms` and the forged regressed `wall_ms`.
//!
//! CATCHES: a bootstrap path that trusts a regressed close frontier, drops the
//! monotonicity guard, or surfaces a non-invariant error / message that omits
//! the conflicting close HLCs.
//!
//! SEEDED: deterministic tempdir-based open, then a hand-forged
//! `SYSTEM_CLOSE_COMPLETED` segment frame (FBAT codec + CRC re-stamping) that
//! drives the close HLC backwards.

#[path = "support/durable_frontier_semantics.rs"]
mod dfs_support;

use batpak::coordinate::DagPosition;
use batpak::prelude::{Event, EventKind, Region};
use batpak::store::{HlcPoint, ReadOnly, Store, StoreConfig, StoreError};
use dfs_support::*;
use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

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
    victim_close_clock: u32,
    forged_close_hlc: HlcPoint,
) -> std::io::Result<()> {
    remove_fast_start_artifacts(segment_dir)?;
    let mut segment_paths = segment_paths(segment_dir)?;
    segment_paths.sort();

    for path in segment_paths {
        if rewrite_close_frame_if_present(
            &path,
            victim_close_hlc,
            victim_close_clock,
            forged_close_hlc,
        )? {
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
    victim_close_clock: u32,
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
        // Match the victim by full HLC: wall_ms alone cannot disambiguate two close
        // frames committed in the same millisecond. The DagPosition carries the HLC
        // clock as `sequence()` (constructed from the writer's `timing.clock`), which
        // is exactly the `IndexEntry::clock()` the caller threads through as
        // `victim_close_clock`. Requiring both wall_ms and the clock component to match
        // pins the exact victim frame even when two closes share a millisecond.
        if payload.event.header.event_kind == EventKind::SYSTEM_CLOSE_COMPLETED
            && payload.event.header.position.wall_ms() == victim_close_hlc.wall_ms
            && payload.event.header.position.sequence() == victim_close_clock
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
    let header_len = usize::try_from(u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]))
        .expect("u32 segment header length fits usize");
    8usize
        .checked_add(header_len)
        .filter(|start| *start <= bytes.len())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad segment header"))
}

fn sidx_frames_end(bytes: &[u8]) -> Option<usize> {
    const TRAILER_LEN: usize = 16;
    if bytes.len() < TRAILER_LEN || &bytes[bytes.len() - 4..] != b"SDX3" {
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
    let len = usize::try_from(u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]))
        .expect("u32 frame length fits usize");
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
    let victim_close_clock = close_entries[1].clock();
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
    forge_close_hlc_regression(dir.path(), close_hlc_2, victim_close_clock, forged)
        .expect("forge close_hlc regression");

    let err = Store::open(StoreConfig::new(dir.path()))
        .map(|_| ())
        .expect_err("PROPERTY: opening with regressed close_hlc must fail with InvariantViolation");
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
