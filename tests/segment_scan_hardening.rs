// justifies: INV-TEST-PANIC-AS-ASSERTION; tests in tests/segment_scan_hardening.rs rely on expect/panic on unreachable failures; clippy::unwrap_used and clippy::panic are the standard harness allowances for integration tests.
#![allow(clippy::unwrap_used, clippy::panic)]
//! Segment-scan hardening.
//!
//! [INV-SEGMENT-SCAN-BOUNDED] The cold-start scan rejects or stops at every
//! malformed frame without allocating unbounded memory and without panicking:
//!
//!   * a frame header claiming a payload larger than the hard frame-size cap
//!     terminates the scan and preserves every earlier frame;
//!   * a SIDX footer with an absurd entry_count never causes the loader to
//!     allocate against the bogus size — the reopen falls back to the
//!     permissive frame-scan and still surfaces the pre-corruption entries;
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
            batpak::store::AppendOptions::new().with_idempotency(0xA1),
            batpak::store::CausationRef::None,
        )
        .expect("batch item 0"),
        BatchAppendItem::new(
            coord,
            KIND,
            &serde_json::json!({"i": 1}),
            batpak::store::AppendOptions::new().with_idempotency(0xA2),
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
    if bytes.len() >= 16 && &bytes[bytes.len() - 4..] == b"SDX2" {
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

fn rewrite_first_matching_frame(
    seg: &std::path::Path,
    mut predicate: impl FnMut(&RawFramePayload) -> bool,
    mutate: impl FnOnce(&mut RawFramePayload),
) {
    let bytes = strip_sidx(std::fs::read(seg).expect("read segment"));
    let header_len = u32::from_be_bytes(bytes[4..8].try_into().expect("4-byte header len"));
    let header_end = 8 + usize::try_from(header_len).expect("segment header len fits usize");

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

fn corrupt_second_staged_batch_item_crc(seg: &std::path::Path) {
    let bytes = strip_sidx(std::fs::read(seg).expect("read segment"));
    let header_len = u32::from_be_bytes(bytes[4..8].try_into().expect("4-byte header len"));
    let header_end = 8 + usize::try_from(header_len).expect("segment header len fits usize");

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
    // The trailer format is [string_table_offset:u64 LE][count:u32 LE][b"SDX2"].
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

    // Walk past the first real frame so at least ONE legit frame remains
    // recoverable before the pathological header.
    let first_len = u32::from_be_bytes(
        bytes[first_frame_offset..first_frame_offset + 4]
            .try_into()
            .expect("4 bytes"),
    ) as usize;
    let poison_frame_offset = first_frame_offset + 8 + first_len;

    assert!(
        poison_frame_offset + 4 <= bytes.len(),
        "segment must contain a second frame to poison; size={}, target={}",
        bytes.len(),
        poison_frame_offset + 4
    );

    // Overwrite the frame's length field with u32::MAX — far beyond
    // MAX_FRAME_PAYLOAD so the scan terminates immediately.
    bytes[poison_frame_offset..poison_frame_offset + 4].copy_from_slice(&u32::MAX.to_be_bytes());
    std::fs::write(&seg, &bytes).expect("write poisoned segment");

    // Reopen must not panic or error. The scan stops at the poisoned frame.
    let store = Store::open(config(&dir)).expect("reopen with poisoned frame");
    let entries = store.query(&Region::all());

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
        b"SDX2",
        "seeded segment must have the SIDX magic"
    );
    // Corrupt the last byte of the SIDX magic.
    let magic_offset = bytes.len() - 1;
    bytes[magic_offset] = b'Z';
    std::fs::write(&seg, &bytes).expect("write bad-magic segment");

    let store = Store::open(config(&dir)).expect("reopen with SIDX magic corruption");
    let entries = store.query(&Region::all());

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
fn corruption_inside_staged_batch_discards_the_whole_batch() {
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
                batpak::store::AppendOptions::new().with_idempotency(0xC0),
                batpak::store::CausationRef::None,
            )
            .expect("batch item 0"),
            BatchAppendItem::new(
                batch_coord,
                KIND,
                &serde_json::json!({"batched": 1}),
                batpak::store::AppendOptions::new().with_idempotency(0xC1),
                batpak::store::CausationRef::None,
            )
            .expect("batch item 1"),
        ])
        .expect("append committed batch");
    store.close().expect("close");

    let seg = segment_path(&dir);
    corrupt_second_staged_batch_item_crc(&seg);

    let reopened = Store::open(config(&dir)).expect("reopen with corrupted staged batch item");
    let entries = reopened.query(&Region::all());
    assert_eq!(
        entries.len(),
        1,
        "PROPERTY: corruption inside an in-flight staged batch must discard the whole batch and preserve only the unrelated pre-batch event."
    );
    let visible = reopened
        .get(entries[0].event_id)
        .expect("load surviving pre-batch event");
    assert_eq!(
        visible.event.payload["pre"],
        serde_json::json!(true),
        "PROPERTY: the surviving event after staged-batch corruption must be the unrelated pre-batch event."
    );
    assert!(
        reopened
            .query(&Region::entity("entity:scan-corrupt-batch"))
            .is_empty(),
        "PROPERTY: corruption on the second staged batch item must discard the whole batch, not leak the staged prefix."
    );
    reopened.close().expect("close");
}
