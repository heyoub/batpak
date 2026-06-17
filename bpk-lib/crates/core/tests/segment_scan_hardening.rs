// justifies: INV-TEST-PANIC-AS-ASSERTION; tests in tests/segment_scan_hardening.rs rely on expect/panic on unreachable failures; clippy::unwrap_used and clippy::panic are the standard harness allowances for integration tests.
#![allow(clippy::unwrap_used, clippy::panic)]
//! Segment-scan hardening.
//!
//! [INV-BATCH-CRASH-RECOVERY] The cold-start scan rejects or stops at every
//! malformed frame without allocating unbounded memory and without panicking:
//!
//!   * a frame header claiming a payload larger than the hard frame-size cap
//!     terminates the scan and preserves every earlier frame;
//!   * a SIDX footer with an absurd entry_count never causes the loader to
//!     allocate against the bogus size — the reopen falls back to the
//!     permissive frame-scan and still surfaces the pre-corruption entries;
//!   * a fully-read committed frame with a bad CRC or unreadable metadata
//!     fails closed instead of silently disappearing from the rebuilt index;
//!   * truncating a segment mid-frame is observable through reduced
//!     visibility, not through a panic.
//!
//! Coverage is black-box: we directly manipulate the on-disk segment bytes
//! and observe what `Store::open` + `query` produce.

use batpak::coordinate::{Coordinate, Region};
use batpak::event::{Event, EventKind};
use batpak::store::segment::{self, SEGMENT_EXTENSION, SEGMENT_MAGIC};
use batpak::store::{BatchAppendItem, Store, StoreConfig, StoreError};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

const KIND: EventKind = EventKind::custom(0xE, 2);

fn config(dir: &TempDir) -> StoreConfig {
    StoreConfig::new(dir.path())
        .with_enable_checkpoint(false) // force a frame scan on reopen
        .with_enable_mmap_index(false)
        .with_sync_every_n_events(1)
}

fn segment_path(dir: &TempDir) -> std::path::PathBuf {
    let mut out = None;
    for entry in std::fs::read_dir(dir.path()).expect("read data dir") {
        let entry = entry.expect("read_dir entry");
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some(SEGMENT_EXTENSION) {
            assert!(
                out.is_none(),
                "test populates exactly one segment; found multiple: {path:?}"
            );
            out = Some(path);
        }
    }
    out.expect("exactly one segment must exist")
}

fn segment_paths_sorted(dir: &TempDir) -> Vec<std::path::PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read data dir")
        .filter_map(|entry| {
            let path = entry.expect("read_dir entry").path();
            (path.extension().and_then(|s| s.to_str()) == Some(SEGMENT_EXTENSION)).then_some(path)
        })
        .collect();
    paths.sort();
    paths
}

fn seed_store(dir: &TempDir, count: u32) {
    let store = Store::open(config(dir)).expect("open store");
    let coord = Coordinate::new("entity:scan", "scope:test").expect("valid coord");
    for i in 0..count {
        store
            .append(&coord, KIND, &serde_json::json!({"i": i}))
            .expect("append");
    }
    store.close().expect("clean close");
}

fn seed_batched_store(dir: &TempDir) {
    let store = Store::open(config(dir)).expect("open store");
    let coord = Coordinate::new("entity:scan-batch", "scope:test").expect("valid coord");
    let items = vec![
        BatchAppendItem::new(
            coord.clone(),
            KIND,
            &serde_json::json!({"i": 0}),
            batpak::store::AppendOptions::new()
                .with_idempotency(batpak::id::IdempotencyKey::from(0xA1)),
            batpak::store::CausationRef::None,
        )
        .expect("batch item 0"),
        BatchAppendItem::new(
            coord,
            KIND,
            &serde_json::json!({"i": 1}),
            batpak::store::AppendOptions::new()
                .with_idempotency(batpak::id::IdempotencyKey::from(0xA2)),
            batpak::store::CausationRef::None,
        )
        .expect("batch item 1"),
    ];
    store.append_batch(items).expect("append batch");
    store.close().expect("clean close");
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RawFramePayload {
    event: Event<serde_json::Value>,
    entity: String,
    scope: String,
}

fn strip_sidx(mut bytes: Vec<u8>) -> Vec<u8> {
    if bytes.len() >= 16 && &bytes[bytes.len() - 4..] == b"SDX3" {
        let string_table_offset = u64::from_le_bytes(
            bytes[bytes.len() - 16..bytes.len() - 8]
                .try_into()
                .expect("8-byte SIDX trailer offset"),
        );
        bytes.truncate(
            usize::try_from(string_table_offset).expect("SIDX string table offset fits usize"),
        );
    }
    bytes
}

fn frame_scan_header_end(bytes: &[u8]) -> usize {
    let header_len = u32::from_be_bytes(bytes[4..8].try_into().expect("4-byte header len"));
    8 + usize::try_from(header_len).expect("segment header len fits usize")
}

fn user_entries(store: &Store) -> Vec<batpak::store::index::IndexEntry> {
    store
        .query(&Region::all())
        .into_iter()
        .filter(|entry| {
            !matches!(
                entry.event_kind(),
                EventKind::SYSTEM_OPEN_COMPLETED | EventKind::SYSTEM_CLOSE_COMPLETED
            )
        })
        .collect()
}

fn raw_msgpack_frame(msgpack: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(8 + msgpack.len());
    let len = u32::try_from(msgpack.len()).expect("test msgpack frame length fits u32");
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&crc32fast::hash(msgpack).to_be_bytes());
    frame.extend_from_slice(msgpack);
    frame
}

fn poison_first_frame_length_past_max(seg: &std::path::Path) {
    let mut bytes = strip_sidx(std::fs::read(seg).expect("read segment"));
    let first_frame_offset = frame_scan_header_end(&bytes);
    assert!(
        first_frame_offset + 4 <= bytes.len(),
        "segment must contain a frame header to poison"
    );
    bytes[first_frame_offset..first_frame_offset + 4].copy_from_slice(&u32::MAX.to_be_bytes());
    std::fs::write(seg, bytes).expect("write poisoned segment");
}

fn rewrite_first_matching_frame(
    seg: &std::path::Path,
    mut predicate: impl FnMut(&RawFramePayload) -> bool,
    mutate: impl FnOnce(&mut RawFramePayload),
) {
    let bytes = strip_sidx(std::fs::read(seg).expect("read segment"));
    let header_end = frame_scan_header_end(&bytes);

    let mut mutated = false;
    let mut mutate = Some(mutate);
    let mut cursor = header_end;
    let mut rebuilt = bytes[..header_end].to_vec();

    while cursor < bytes.len() {
        let (msgpack, frame_size) =
            segment::frame_decode(&bytes[cursor..]).expect("seeded frame decodes");
        if !mutated {
            let mut payload: RawFramePayload =
                rmp_serde::from_slice(msgpack).expect("seeded frame payload decodes");
            if predicate(&payload) {
                mutate.take().expect("frame mutator used once")(&mut payload);
                rebuilt.extend(segment::frame_encode(&payload).expect("re-encode mutated frame"));
                mutated = true;
            } else {
                rebuilt.extend_from_slice(&bytes[cursor..cursor + frame_size]);
            }
        } else {
            rebuilt.extend_from_slice(&bytes[cursor..cursor + frame_size]);
        }
        cursor += frame_size;
    }

    assert!(mutated, "test must mutate one matching frame");
    std::fs::write(seg, rebuilt).expect("write mutated segment");
}

fn replace_first_matching_frame(
    seg: &std::path::Path,
    mut predicate: impl FnMut(&RawFramePayload) -> bool,
    replacement: impl FnOnce(&RawFramePayload) -> Vec<u8>,
) {
    let bytes = strip_sidx(std::fs::read(seg).expect("read segment"));
    let header_end = frame_scan_header_end(&bytes);

    let mut replaced = false;
    let mut replacement = Some(replacement);
    let mut cursor = header_end;
    let mut rebuilt = bytes[..header_end].to_vec();

    while cursor < bytes.len() {
        let (msgpack, frame_size) =
            segment::frame_decode(&bytes[cursor..]).expect("seeded frame decodes");
        let payload: RawFramePayload =
            rmp_serde::from_slice(msgpack).expect("seeded frame payload decodes");
        if !replaced && predicate(&payload) {
            rebuilt.extend(replacement
                .take()
                .expect("replacement frame builder used once")(
                &payload
            ));
            replaced = true;
        } else {
            rebuilt.extend_from_slice(&bytes[cursor..cursor + frame_size]);
        }
        cursor += frame_size;
    }

    assert!(replaced, "test must replace one matching frame");
    std::fs::write(seg, rebuilt).expect("write replaced segment");
}

fn corrupt_second_staged_batch_item_crc(seg: &std::path::Path) {
    let bytes = strip_sidx(std::fs::read(seg).expect("read segment"));
    let header_end = frame_scan_header_end(&bytes);

    let mut corrupted = false;
    let mut in_batch = false;
    let mut staged_items = 0usize;
    let mut cursor = header_end;
    let mut rebuilt = bytes[..header_end].to_vec();

    while cursor < bytes.len() {
        let (msgpack, frame_size) =
            segment::frame_decode(&bytes[cursor..]).expect("seeded frame decodes");
        let payload: RawFramePayload =
            rmp_serde::from_slice(msgpack).expect("seeded frame payload decodes");
        let kind = payload.event.header.event_kind;

        if kind == EventKind::SYSTEM_BATCH_BEGIN {
            in_batch = true;
            staged_items = 0;
        } else if kind == EventKind::SYSTEM_BATCH_COMMIT {
            in_batch = false;
            staged_items = 0;
        } else if in_batch {
            staged_items += 1;
        }

        if !corrupted && in_batch && staged_items == 2 {
            let mut frame = bytes[cursor..cursor + frame_size].to_vec();
            let last = frame
                .len()
                .checked_sub(1)
                .expect("frame must contain payload bytes");
            frame[last] ^= 0x01;
            rebuilt.extend(frame);
            corrupted = true;
        } else {
            rebuilt.extend_from_slice(&bytes[cursor..cursor + frame_size]);
        }
        cursor += frame_size;
    }

    assert!(corrupted, "test must corrupt the second staged batch item");
    std::fs::write(seg, rebuilt).expect("write corrupted segment");
}

#[test]
fn pathological_frame_length_is_bounded_not_panicking() {
    // Seed a segment with several real frames, then overwrite a frame-header
    // length field with the u32::MAX sentinel. The scan must see the length
    // exceeds MAX_FRAME_PAYLOAD (256 MB), log a warning, and stop scanning
    // — preserving every earlier frame.
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 4);

    let seg = segment_path(&dir);
    let mut bytes = std::fs::read(&seg).expect("read segment");
    assert_eq!(
        &bytes[..SEGMENT_MAGIC.len()],
        SEGMENT_MAGIC,
        "seeded segment must start with the canonical segment magic"
    );
    assert!(
        bytes.len() >= 16,
        "segment must have at least a 16-byte SIDX trailer"
    );

    // Strip the SIDX footer so the cold-start walks the slow path.
    // The trailer format is [string_table_offset:u64 LE][count:u32 LE][b"SDX3"].
    let trailer_start = bytes.len() - 16;
    let string_table_offset = u64::from_le_bytes(
        bytes[trailer_start..trailer_start + 8]
            .try_into()
            .expect("8 bytes"),
    );
    bytes.truncate(string_table_offset.try_into().expect("offset fits usize"));

    // Find the first frame header — it lives right after magic(4) +
    // header_len(4) + msgpack header bytes. The msgpack header starts at
    // offset 8; its length is the u32 BE at bytes[4..8].
    let header_len = u32::from_be_bytes(bytes[4..8].try_into().expect("4 bytes")) as usize;
    let first_frame_offset = 8 + header_len;

    // Walk past the first two real frames so at least one user-authored
    // frame remains recoverable before the pathological header even though
    // mutable open now writes a lifecycle event first.
    let first_len = u32::from_be_bytes(
        bytes[first_frame_offset..first_frame_offset + 4]
            .try_into()
            .expect("4 bytes"),
    ) as usize;
    let second_frame_offset = first_frame_offset + 8 + first_len;
    let second_len = u32::from_be_bytes(
        bytes[second_frame_offset..second_frame_offset + 4]
            .try_into()
            .expect("4 bytes"),
    ) as usize;
    let poison_frame_offset = second_frame_offset + 8 + second_len;

    assert!(
        poison_frame_offset + 4 <= bytes.len(),
        "segment must contain a third frame to poison; size={}, target={}",
        bytes.len(),
        poison_frame_offset + 4
    );

    // Overwrite the frame's length field with u32::MAX — far beyond
    // MAX_FRAME_PAYLOAD so the scan terminates immediately.
    bytes[poison_frame_offset..poison_frame_offset + 4].copy_from_slice(&u32::MAX.to_be_bytes());
    std::fs::write(&seg, &bytes).expect("write poisoned segment");

    // Reopen must not panic or error. The scan stops at the poisoned frame.
    let store = Store::open(config(&dir)).expect("reopen with poisoned frame");
    let entries: Vec<_> = store
        .query(&Region::all())
        .into_iter()
        .filter(|entry| {
            !matches!(
                entry.event_kind(),
                EventKind::SYSTEM_OPEN_COMPLETED | EventKind::SYSTEM_CLOSE_COMPLETED
            )
        })
        .collect();

    assert!(
        !entries.is_empty(),
        "PROPERTY: pre-corruption frames must survive a pathological frame-length poison; got 0 entries"
    );
    assert!(
        entries.len() < 4,
        "PROPERTY: poisoning the second frame's length must prevent it and later frames from surfacing; \
         got {} entries (max 3 expected if only the first frame survives)",
        entries.len()
    );

    // The store remains usable.
    let coord = Coordinate::new("entity:scan", "scope:test").expect("valid coord");
    store
        .append(&coord, KIND, &serde_json::json!({"post_poison": true}))
        .expect("append after corrupt reopen");
    store.close().expect("close");
}

#[test]
fn non_tail_pathological_frame_length_fails_closed_on_reopen() {
    // Only the latest existing segment is allowed to use torn-tail recovery.
    // An impossible frame length in older history means committed segment
    // corruption, not a recoverable crash tail.
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(config(&dir).with_segment_max_bytes(512)).expect("open store");
    let coord = Coordinate::new("entity:scan-historical", "scope:test").expect("valid coord");
    for i in 0..40 {
        store
            .append(
                &coord,
                KIND,
                &serde_json::json!({"i": i, "pad": "x".repeat(96)}),
            )
            .expect("append");
    }
    store.close().expect("close");

    let segments = segment_paths_sorted(&dir);
    assert!(
        segments.len() >= 2,
        "test must create historical and latest segments; got {}",
        segments.len()
    );
    poison_first_frame_length_past_max(&segments[0]);

    let err = match Store::open(config(&dir).with_segment_max_bytes(512)) {
        Ok(_) => {
            panic!("PROPERTY: non-tail impossible frame length must fail closed during reopen")
        }
        Err(err) => err,
    };

    assert!(
        matches!(
            err,
            StoreError::CorruptFrame { ref reason, .. }
            if reason.contains("exceeds MAX_FRAME_PAYLOAD")
        ),
        "PROPERTY: non-tail impossible frame length must surface as CorruptFrame; got {err:?}"
    );
}

#[test]
fn sidx_footer_magic_mismatch_falls_back_to_frame_scan() {
    // Overwriting the SIDX magic is a common real-world corruption: the
    // trailer looks present but does not match the sentinel. The loader
    // must treat it as "no SIDX present" and fall back to the frame scan,
    // which still recovers every frame.
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 8);

    let seg = segment_path(&dir);
    let mut bytes = std::fs::read(&seg).expect("read segment");
    assert_eq!(
        &bytes[bytes.len() - 4..],
        b"SDX3",
        "seeded segment must have the SIDX magic"
    );
    // Corrupt the last byte of the SIDX magic.
    let magic_offset = bytes.len() - 1;
    bytes[magic_offset] = b'Z';
    std::fs::write(&seg, &bytes).expect("write bad-magic segment");

    let store = Store::open(config(&dir)).expect("reopen with SIDX magic corruption");
    let entries: Vec<_> = store
        .query(&Region::all())
        .into_iter()
        .filter(|entry| {
            !matches!(
                entry.event_kind(),
                EventKind::SYSTEM_OPEN_COMPLETED | EventKind::SYSTEM_CLOSE_COMPLETED
            )
        })
        .collect();

    // The frame scan recovers every frame despite the SIDX trailer being
    // unreadable — SIDX is an accelerator, not the durability oracle.
    assert_eq!(
        entries.len(),
        8,
        "PROPERTY: a SIDX magic corruption must fall back to the frame scan without data loss; \
         got {} entries (expected 8)",
        entries.len()
    );
    store.close().expect("close");
}

#[test]
fn sidx_footer_offset_forged_too_low_into_an_interior_frame_recovers_all_frames() {
    // ROUND-4 P1: a forged SIDX string_table_offset that lands on an EARLIER real
    // frame boundary (too LOW). Overwriting the offset breaks the SDX3 footer CRC,
    // so the boundary is UNTRUSTED — the offset is GARBAGE and must NEVER bound
    // recovery. Earlier rounds rejected this with CorruptSegment (treating the
    // hint as a truncation proof). The DEFINITIVE behavior: discard the hint and
    // walk the CRC-valid frames bounded only by file_len, so cold start recovers
    // EVERY committed frame instead of either dropping the later ones or failing
    // closed. (Trusting an unauthenticated offset to FAIL is itself a denial-of-
    // availability vector; the CRC-valid frames are the durability oracle.)
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 6);

    let seg = segment_path(&dir);
    let bytes = std::fs::read(&seg).expect("read segment");
    assert_eq!(
        &bytes[bytes.len() - 4..],
        b"SDX3",
        "seeded segment must carry the SDX3 SIDX magic"
    );

    // Walk frames from the header end to find the offset of the SECOND frame —
    // a real frame boundary that sits strictly inside the frame region.
    let frames_start = frame_scan_header_end(&bytes);
    let true_frames_end = usize::try_from(u64::from_le_bytes(
        bytes[bytes.len() - 16..bytes.len() - 8]
            .try_into()
            .expect("8-byte SIDX trailer offset"),
    ))
    .expect("SIDX string table offset fits usize");
    let mut cursor = frames_start;
    // skip the first frame
    let first_len = u32::from_be_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
    cursor += 8 + first_len;
    let forged_offset = cursor;
    assert!(
        forged_offset > frames_start && forged_offset < true_frames_end,
        "forged boundary must land on an interior real frame boundary"
    );

    // Overwrite the SIDX trailer's string_table_offset with the forged boundary.
    let mut forged = bytes.clone();
    let off_pos = forged.len() - 16;
    forged[off_pos..off_pos + 8].copy_from_slice(&(forged_offset as u64).to_le_bytes());
    std::fs::write(&seg, &forged).expect("write forged-offset segment");

    // Cold start must recover ALL committed frames: the untrusted offset is
    // discarded and the CRC-valid frame walk finds every event.
    let store = Store::open(config(&dir))
        .expect("reopen must succeed: a forged too-low untrusted offset must not brick cold start");
    let entries = user_entries(&store);
    assert_eq!(
        entries.len(),
        6,
        "PROPERTY: a forged too-LOW unauthenticated SIDX offset must recover ALL CRC-valid \
         frames (hint discarded), not drop frames or FailClosed; got {} (expected 6)",
        entries.len()
    );
    store.close().expect("close");
}

#[test]
fn sidx_footer_offset_forged_mid_frame_recovers_all_frames() {
    // ROUND-4 P1 (the gap that kept slipping): a forged SIDX string_table_offset
    // landing INSIDE a later CRC-valid frame's header/payload — NOT at a frame
    // boundary. No frame begins at the offset, so the old truncation guard never
    // fired; and the old walker (hint as upper bound) hit `frame_tail > hint` for
    // the frame CONTAINING the offset and returned that frame's START — silently
    // dropping that CRC-valid frame and all later ones. With the hint discarded
    // and file_len the only bound, the walk decodes that frame cleanly and
    // recovers EVERY committed event.
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 6);

    let seg = segment_path(&dir);
    let bytes = std::fs::read(&seg).expect("read segment");
    assert_eq!(
        &bytes[bytes.len() - 4..],
        b"SDX3",
        "seeded segment must carry the SDX3 SIDX magic"
    );

    let frames_start = frame_scan_header_end(&bytes);
    let true_frames_end = usize::try_from(u64::from_le_bytes(
        bytes[bytes.len() - 16..bytes.len() - 8]
            .try_into()
            .expect("8-byte SIDX trailer offset"),
    ))
    .expect("SIDX string table offset fits usize");
    // Skip the first two frames to land at the start of the THIRD frame, then
    // point the offset a few bytes INTO it (mid-header / mid-payload).
    let mut cursor = frames_start;
    let first_len = u32::from_be_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
    cursor += 8 + first_len;
    let second_len = u32::from_be_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
    cursor += 8 + second_len;
    let mid_frame_offset = cursor + 3; // strictly inside the third frame
    assert!(
        mid_frame_offset > cursor && mid_frame_offset < true_frames_end,
        "forged offset must land strictly inside a later CRC-valid frame, not at a boundary"
    );

    let mut forged = bytes.clone();
    let off_pos = forged.len() - 16;
    forged[off_pos..off_pos + 8].copy_from_slice(&(mid_frame_offset as u64).to_le_bytes());
    std::fs::write(&seg, &forged).expect("write mid-frame-offset segment");

    let store = Store::open(config(&dir))
        .expect("reopen must succeed: a mid-frame untrusted offset must not brick cold start");
    let entries = user_entries(&store);
    assert_eq!(
        entries.len(),
        6,
        "PROPERTY: a forged MID-FRAME unauthenticated SIDX offset must recover ALL CRC-valid \
         frames (hint discarded), not drop the containing frame; got {} (expected 6)",
        entries.len()
    );
    store.close().expect("close");
}

#[test]
fn corrupt_sdx3_footer_offset_too_high_into_footer_recovers_all_crc_valid_frames() {
    // ROUND-3 P1: a CORRUPT (CRC-failing) SDX3 footer whose `string_table_offset`
    // points too HIGH — past the real frames, INTO the footer region (string
    // table / entries / CRC). The round-2 truncation guard only rejects an offset
    // too LOW (a CRC-valid frame begins at the claimed boundary). A too-HIGH
    // offset lands on footer bytes, where NO CRC-valid frame begins, so the guard
    // passed it through. Before this fix, cold start trusted that unauthenticated
    // offset as `frames_end`, scanned the real frames fine, then parsed FOOTER
    // bytes as frame headers and FailClosed — bricking recovery even though every
    // real frame is CRC-valid. With provenance-aware recovery, the untrusted
    // offset is clamped down to the true end of CRC-valid frames, so all events
    // are recovered.
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 6);

    let seg = segment_path(&dir);
    let mut bytes = std::fs::read(&seg).expect("read segment");
    assert_eq!(
        &bytes[bytes.len() - 4..],
        b"SDX3",
        "seeded segment must carry the SDX3 SIDX magic"
    );

    let n = bytes.len();
    let off_pos = n - 16;
    let true_frames_end = u64::from_le_bytes(
        bytes[off_pos..off_pos + 8]
            .try_into()
            .expect("8-byte SIDX trailer offset"),
    );
    // The footer region spans [true_frames_end .. n). Point the offset INTO it,
    // strictly past the real frames but still <= file_len - 16 (the trailer can't
    // begin inside itself). Landing it a few bytes into the string table is the
    // adversarial too-high case. This also breaks the footer CRC (read_layout
    // recomputes the covered region from the offset and the geometry no longer
    // matches the stored CRC / errors), so the footer is UNTRUSTED and recovery
    // must rebuild from the CRC-valid frames it actually finds.
    let max_offset = (n as u64) - 16;
    let forged_high = (true_frames_end + 4).min(max_offset);
    assert!(
        forged_high > true_frames_end,
        "forged offset must point strictly into the footer region (too high)"
    );
    bytes[off_pos..off_pos + 8].copy_from_slice(&forged_high.to_le_bytes());
    std::fs::write(&seg, &bytes).expect("write too-high-offset segment");

    let store = Store::open(config(&dir))
        .expect("reopen must succeed: a too-high corrupt-footer offset must not brick cold start");
    let entries = user_entries(&store);
    assert_eq!(
        entries.len(),
        6,
        "PROPERTY: a too-HIGH unauthenticated SIDX offset (into the footer) must recover ALL \
         CRC-valid frames, not FailClosed; got {} (expected 6)",
        entries.len()
    );
    store.close().expect("close");
}

#[test]
fn sidx_footer_entry_count_disagreement_falls_back_to_frame_scan() {
    // Corrupting the SIDX entry_count makes the footer structurally
    // inconsistent with the actual footer block. Cold start must not trust
    // the accelerator; it should fall back to the authoritative frame scan.
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 6);

    let seg = segment_path(&dir);
    let mut bytes = std::fs::read(&seg).expect("read segment");
    assert_eq!(
        &bytes[bytes.len() - 4..],
        b"SDX3",
        "seeded segment must have the SIDX magic"
    );
    let count_offset = bytes.len() - 8;
    bytes[count_offset..count_offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    std::fs::write(&seg, &bytes).expect("write bad-count segment");

    let store = Store::open(config(&dir)).expect("reopen with SIDX count disagreement");
    let entries = user_entries(&store);
    assert_eq!(
        entries.len(),
        6,
        "PROPERTY: SIDX entry_count disagreement must fall back to the frame scan without data loss; got {} entries",
        entries.len()
    );
    store.close().expect("close");
}

/// Rewrite only the trailing 4-byte SIDX magic from the current `SDX3` to the
/// legacy pre-0.8.3 `SDX2`, leaving the whole footer (string table + entries +
/// 16-byte trailer geometry) byte-for-byte intact. This reproduces a real
/// pre-0.8.3 sealed segment on disk: a structurally-valid SIDX footer whose
/// magic the post-bump reader no longer trusts (no CRC32 in the SDX2 format),
/// so `read_footer` returns `Ok(None)` and cold start must fall back to the
/// CRC-verified frame scan.
fn downgrade_sidx_magic_to_sdx2(seg: &std::path::Path) {
    let mut bytes = std::fs::read(seg).expect("read segment");
    let n = bytes.len();
    assert!(n >= 16, "segment must hold the 16-byte SIDX trailer");
    assert_eq!(
        &bytes[n - 4..],
        b"SDX3",
        "seeded segment must carry the current SDX3 SIDX magic before downgrade"
    );
    bytes[n - 4..].copy_from_slice(b"SDX2");
    std::fs::write(seg, bytes).expect("write SDX2-downgraded segment");
}

#[test]
fn legacy_sdx2_tail_segment_recovers_all_events_via_frame_scan() {
    // BACKWARD-COMPAT (P1): a pre-0.8.3 sealed segment carries an SDX2 footer
    // with no CRC32. After the SDX2->SDX3 magic bump, `read_footer` refuses to
    // trust SDX2 content (Ok(None)) and cold start frame-scans. The scan must
    // still honor the SDX2 footer's BOUNDARY (string_table_offset) so it stops
    // at the true end of frames instead of over-running into the footer bytes.
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 8);

    let seg = segment_path(&dir);
    downgrade_sidx_magic_to_sdx2(&seg);

    let store = Store::open(config(&dir)).expect("reopen pre-0.8.3 SDX2 tail segment");
    let entries = user_entries(&store);
    assert_eq!(
        entries.len(),
        8,
        "PROPERTY: a pre-0.8.3 SDX2 sealed segment must recover ALL events via the \
         frame-scan fallback; got {} (expected 8)",
        entries.len()
    );
    store.close().expect("close");
}

#[test]
fn legacy_sdx2_non_tail_segment_recovers_all_events_via_frame_scan() {
    // The dangerous case the P1 actually bricked: a NON-TAIL (historical) SDX2
    // segment frame-scans under the fail-closed tail policy. Before the boundary
    // fix, `detect_sidx_boundary` matched only SDX3, returned None for SDX2, set
    // frames_end = file_len, and the scan over-ran into the SDX2 string-table
    // bytes — whose first msgpack byte reads as an oversized frame length,
    // surfacing CorruptFrame and FAILING the entire store reopen. Recognizing
    // the SDX2 magic as a boundary marker makes frames_end land exactly at the
    // end of the frame region, so every committed event is recovered.
    let dir = TempDir::new().expect("temp dir");
    let store =
        Store::open(config(&dir).with_segment_max_bytes(512)).expect("open store for rotation");
    let coord = Coordinate::new("entity:scan-legacy", "scope:test").expect("valid coord");
    for i in 0..40 {
        store
            .append(
                &coord,
                KIND,
                &serde_json::json!({"i": i, "pad": "x".repeat(96)}),
            )
            .expect("append");
    }
    store.close().expect("close");

    let segments = segment_paths_sorted(&dir);
    assert!(
        segments.len() >= 2,
        "test must create at least one historical (non-tail) segment plus a tail; got {}",
        segments.len()
    );
    // Downgrade the FIRST (oldest, non-tail) sealed segment to the SDX2 format.
    downgrade_sidx_magic_to_sdx2(&segments[0]);

    let store = Store::open(config(&dir).with_segment_max_bytes(512))
        .expect("reopen must succeed: a non-tail SDX2 segment must not brick cold start");
    let entries = user_entries(&store);
    assert_eq!(
        entries.len(),
        40,
        "PROPERTY: every event across all segments must survive when an older \
         segment is in the legacy SDX2 format; got {} (expected 40)",
        entries.len()
    );
    store.close().expect("close");
}

#[test]
fn truncating_segment_mid_frame_never_panics() {
    // Truncate a segment inside a frame body. The scanner sees an
    // UnexpectedEof on read_exact for the payload and stops cleanly.
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 4);

    let seg = segment_path(&dir);
    let bytes = std::fs::read(&seg).expect("read segment");
    // Strip SIDX trailer first so the scan takes the slow path.
    let trailer_offset = u64::from_le_bytes(
        bytes[bytes.len() - 16..bytes.len() - 8]
            .try_into()
            .expect("8 bytes"),
    );
    let truncated_len = (usize::try_from(trailer_offset).expect("offset fits usize")) / 2;
    std::fs::write(&seg, &bytes[..truncated_len]).expect("write truncated segment");

    let store = Store::open(config(&dir)).expect("reopen with mid-frame truncation");
    let entries = store.query(&Region::all());
    assert!(
        entries.len() <= 4,
        "PROPERTY: truncated segment scan must not fabricate entries; got {} (max 4)",
        entries.len()
    );
    store.close().expect("close");
}

#[test]
fn valid_crc_unreadable_frame_metadata_fails_closed() {
    // Replacing a data frame with CRC-valid bytes that are not valid
    // MessagePack exercises the non-CRC metadata decode branch. This is a
    // fully-read committed frame, so reopening must fail closed instead of
    // silently deleting that event from the rebuilt index.
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 5);

    let seg = segment_path(&dir);
    replace_first_matching_frame(
        &seg,
        |payload| {
            !matches!(
                payload.event.header.event_kind,
                EventKind::SYSTEM_BATCH_BEGIN
                    | EventKind::SYSTEM_BATCH_COMMIT
                    | EventKind::SYSTEM_OPEN_COMPLETED
                    | EventKind::SYSTEM_CLOSE_COMPLETED
            )
        },
        |_payload| raw_msgpack_frame(&[0xC1]),
    );

    let err = match Store::open(config(&dir)) {
        Ok(_) => panic!("PROPERTY: CRC-valid unreadable committed metadata must fail closed"),
        Err(err) => err,
    };
    assert!(
        matches!(err, StoreError::CorruptSegment { .. }),
        "PROPERTY: CRC-valid unreadable committed metadata must surface as corrupt segment; got {err:?}"
    );
}

#[test]
fn orphan_commit_marker_is_ignored_without_stopping_scan() {
    // A COMMIT marker without a preceding BEGIN is malformed batch metadata,
    // but it must not stop recovery of independent frames around it.
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 5);

    let seg = segment_path(&dir);
    rewrite_first_matching_frame(
        &seg,
        |payload| {
            !matches!(
                payload.event.header.event_kind,
                EventKind::SYSTEM_BATCH_BEGIN
                    | EventKind::SYSTEM_BATCH_COMMIT
                    | EventKind::SYSTEM_OPEN_COMPLETED
                    | EventKind::SYSTEM_CLOSE_COMPLETED
            )
        },
        |payload| {
            payload.event.header.event_kind = EventKind::SYSTEM_BATCH_COMMIT;
            payload.event.header.payload_size = 0;
            payload.event.payload = serde_json::Value::Null;
        },
    );

    let store = Store::open(config(&dir)).expect("reopen with orphan COMMIT marker");
    let entries = user_entries(&store);
    assert_eq!(
        entries.len(),
        4,
        "PROPERTY: orphan COMMIT marker should be ignored while later independent frames survive; got {} entries",
        entries.len()
    );
    store.close().expect("close");
}

#[test]
fn invalid_batch_begin_count_fails_closed_on_reopen() {
    // Slow-path recovery uses SYSTEM_BATCH_BEGIN.payload_size as the claimed
    // batch item count. Corrupting it to zero must fail closed instead of
    // staging phantom items or silently defaulting the count.
    let dir = TempDir::new().expect("temp dir");
    seed_batched_store(&dir);

    let seg = segment_path(&dir);
    rewrite_first_matching_frame(
        &seg,
        |payload| payload.event.header.event_kind == EventKind::SYSTEM_BATCH_BEGIN,
        |payload| {
            payload.event.header.payload_size = 0;
        },
    );

    let err = match Store::open(config(&dir)) {
        Ok(_) => {
            panic!("PROPERTY: a SYSTEM_BATCH_BEGIN with count 0 must fail closed during reopen")
        }
        Err(err) => err,
    };
    assert!(
        matches!(
            err,
            StoreError::CorruptSegment { ref detail, .. }
            if detail.contains("invalid batch marker count")
        ),
        "PROPERTY: corrupt batch marker count must surface a clear CorruptSegment detail, got {err:?}"
    );
}

#[test]
fn missing_hash_chain_for_data_frame_fails_closed_on_reopen() {
    // Slow-path recovery no longer defaults missing hash chains for ordinary
    // data events. Removing it from a persisted frame must fail closed.
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 2);

    let seg = segment_path(&dir);
    rewrite_first_matching_frame(
        &seg,
        |payload| {
            !matches!(
                payload.event.header.event_kind,
                EventKind::SYSTEM_BATCH_BEGIN | EventKind::SYSTEM_BATCH_COMMIT
            )
        },
        |payload| {
            payload.event.hash_chain = None;
        },
    );

    let err = match Store::open(config(&dir)) {
        Ok(_) => panic!(
            "PROPERTY: a persisted data frame without hash_chain must fail closed during reopen"
        ),
        Err(err) => err,
    };
    assert!(
        matches!(
            err,
            StoreError::CorruptSegment { ref detail, .. }
            if detail.contains("missing hash_chain")
        ),
        "PROPERTY: missing hash_chain must surface a clear CorruptSegment detail, got {err:?}"
    );
}

#[test]
fn corruption_inside_committed_batch_fails_closed() {
    // Slow-path recovery stages batch items until the COMMIT marker arrives.
    // A CRC failure inside that staged window must discard the entire batch,
    // not leak the valid prefix that appeared before the corruption.
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(config(&dir)).expect("open store");
    let pre_coord = Coordinate::new("entity:scan-corrupt-pre", "scope:test").expect("pre coord");
    let batch_coord =
        Coordinate::new("entity:scan-corrupt-batch", "scope:test").expect("batch coord");

    store
        .append(&pre_coord, KIND, &serde_json::json!({"pre": true}))
        .expect("append pre-batch event");
    store
        .append_batch(vec![
            BatchAppendItem::new(
                batch_coord.clone(),
                KIND,
                &serde_json::json!({"batched": 0}),
                batpak::store::AppendOptions::new()
                    .with_idempotency(batpak::id::IdempotencyKey::from(0xC0)),
                batpak::store::CausationRef::None,
            )
            .expect("batch item 0"),
            BatchAppendItem::new(
                batch_coord,
                KIND,
                &serde_json::json!({"batched": 1}),
                batpak::store::AppendOptions::new()
                    .with_idempotency(batpak::id::IdempotencyKey::from(0xC1)),
                batpak::store::CausationRef::None,
            )
            .expect("batch item 1"),
        ])
        .expect("append committed batch");
    store.close().expect("close");

    let seg = segment_path(&dir);
    corrupt_second_staged_batch_item_crc(&seg);

    let err = match Store::open(config(&dir)) {
        Ok(_) => panic!("PROPERTY: corrupted committed batch payload must fail closed"),
        Err(err) => err,
    };
    assert!(
        matches!(err, StoreError::CrcMismatch { .. }),
        "PROPERTY: corrupted committed batch payload must surface as CRC mismatch; got {err:?}"
    );
}
