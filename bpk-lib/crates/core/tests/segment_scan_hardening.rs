//! PROVES: a cold-start frame scan over a FULLY-READ committed segment fails
//! closed on metadata/CRC corruption inside committed frames — bad-CRC staged
//! batch items, CRC-valid-but-undecodable MessagePack, a zero batch-begin
//! count, a missing data-frame hash chain — while still ignoring an orphan
//! COMMIT marker and falling back from a structurally-broken SIDX entry_count.
//! CATCHES: a recovery path that silently drops a committed event from the
//! rebuilt index, stages phantom batch items, defaults a corrupt count, or
//! trusts an inconsistent SIDX accelerator instead of the authoritative scan.
//! SEEDED: deterministic single-segment stores (plain + committed batch) whose
//! on-disk frames are surgically corrupted before reopen.
//!
//! [INV-BATCH-CRASH-RECOVERY] A fully-read committed frame with a bad CRC or
//! unreadable metadata fails closed instead of silently disappearing from the
//! rebuilt index. This binary holds the committed-frame fail-closed family; the
//! frame-bounds and untrusted-offset families live in the sibling
//! `segment_scan_hardening_*` binaries. Coverage is black-box: we directly
//! manipulate the on-disk segment bytes and observe what `Store::open` +
//! `query` produce.

use batpak_testkit::segment_scan_hardening as ssh_support;

use batpak::coordinate::Coordinate;
use batpak::event::{Event, EventKind};
use batpak::store::segment;
use batpak::store::{BatchAppendItem, Store, StoreError};
use serde::{Deserialize, Serialize};
use ssh_support::*;
use tempfile::TempDir;

/// Strip a CRC-valid SDX3 footer so the cold start walks the slow frame scan.
/// Used by every frame-mutating helper in this binary; the untrusted-offset
/// family never strips the footer, so this stays inline rather than shared to
/// keep the support module `dead_code`-free.
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

fn raw_msgpack_frame(msgpack: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(8 + msgpack.len());
    let len = u32::try_from(msgpack.len()).expect("test msgpack frame length fits u32");
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&crc32fast::hash(msgpack).to_be_bytes());
    frame.extend_from_slice(msgpack);
    frame
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

    let err = Store::open(config(&dir))
        .map(|_| ())
        .expect_err("PROPERTY: CRC-valid unreadable committed metadata must fail closed");
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

    let err = Store::open(config(&dir))
        .map(|_| ())
        .expect_err("PROPERTY: a SYSTEM_BATCH_BEGIN with count 0 must fail closed during reopen");
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

    let err = Store::open(config(&dir)).map(|_| ()).expect_err(
        "PROPERTY: a persisted data frame without hash_chain must fail closed during reopen",
    );
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

    let err = Store::open(config(&dir))
        .map(|_| ())
        .expect_err("PROPERTY: corrupted committed batch payload must fail closed");
    assert!(
        matches!(err, StoreError::CrcMismatch { .. }),
        "PROPERTY: corrupted committed batch payload must surface as CRC mismatch; got {err:?}"
    );
}
