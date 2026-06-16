use super::*;
use crate::coordinate::DagPosition;
use crate::event::EventKind;
use crate::store::segment::scan::recovery::sidx_fast_path::SidxTailCoverage;
use crate::store::segment::sidx::{kind_to_raw, read_footer, SidxEntry, SidxEntryCollector};
use std::io::ErrorKind;
use std::io::{Cursor, Seek, SeekFrom, Write};
use tempfile::{NamedTempFile, TempDir};

fn sample_entry(frame_offset: u64, frame_length: u32) -> SidxEntry {
    SidxEntry {
        event_id: 1,
        entity_idx: 0,
        scope_idx: 0,
        kind: kind_to_raw(EventKind::custom(0x1, 1)),
        wall_ms: 1,
        clock: 1,
        dag_lane: 0,
        dag_depth: 0,
        prev_hash: [0; 32],
        event_hash: [1; 32],
        frame_offset,
        frame_length,
        global_sequence: 1,
        correlation_id: 1,
        causation_id: 0,
    }
}

fn footer_bytes(prefix_len: usize, entries: &[SidxEntry]) -> Vec<u8> {
    let mut bytes = vec![0xA5; prefix_len];
    let mut cursor = Cursor::new(&mut bytes);
    cursor.seek(SeekFrom::End(0)).expect("seek to end");

    let mut collector = SidxEntryCollector::new();
    for (idx, entry) in entries.iter().cloned().enumerate() {
        let entity = format!("entity:{idx}");
        collector.record(entry, &entity, "scope:test");
    }
    collector
        .write_footer(&mut cursor, 7)
        .expect("write footer");

    bytes
}

fn footer_file(prefix_len: usize, entries: &[SidxEntry]) -> NamedTempFile {
    let bytes = footer_bytes(prefix_len, entries);
    let mut tmp = NamedTempFile::new().expect("create temp file");
    tmp.write_all(&bytes).expect("write temp bytes");
    tmp.flush().expect("flush temp file");
    tmp
}

fn footer_segment_path(
    dir: &TempDir,
    segment_id: u64,
    prefix_len: usize,
    entries: &[SidxEntry],
) -> std::path::PathBuf {
    let path = dir
        .path()
        .join(crate::store::segment::segment_filename(segment_id));
    std::fs::write(&path, footer_bytes(prefix_len, entries)).expect("write segment footer file");
    path
}

fn scanned_entry(event_id: u128) -> ScannedIndexEntry {
    ScannedIndexEntry {
        header: EventHeader::new(
            event_id,
            event_id,
            None,
            0,
            DagPosition::root(),
            0,
            EventKind::custom(0x1, 1),
        ),
        entity: "entity:test".to_owned(),
        scope: "scope:test".to_owned(),
        hash_chain: HashChain::default(),
        segment_id: 7,
        offset: u64::try_from(event_id).expect("test ids fit u64"),
        length: 16,
        receipt_extensions: std::collections::BTreeMap::new(),
        global_sequence: None,
    }
}

#[test]
fn batch_recovery_state_refuses_items_past_declared_count() {
    let mut state = BatchRecoveryState {
        remaining: 1,
        started_count: 1,
        in_batch: true,
        staged: Vec::new(),
    };

    assert!(
        state.stage_entry(scanned_entry(1)),
        "PROPERTY: the first item in a one-item recovered batch is accepted"
    );
    assert_eq!(
        state.remaining, 0,
        "PROPERTY: staging a recovered batch item decrements the remaining item budget"
    );
    assert!(
        !state.stage_entry(scanned_entry(2)),
        "PROPERTY: recovery must reject items beyond the BEGIN marker's declared count"
    );
    assert_eq!(
        state.staged.len(),
        1,
        "PROPERTY: rejected over-count items must not be silently staged"
    );

    state.discard_incomplete();
    assert!(
        !state.in_batch && state.staged.is_empty() && state.remaining == 0,
        "PROPERTY: corrupt over-count batches can be discarded without leaving pending state"
    );
}

#[test]
fn sidx_covers_segment_tail_requires_last_entry_to_reach_footer_start() {
    let tmp = footer_file(64, &[sample_entry(0, 64)]);
    let (entries, _) = read_footer(tmp.path())
        .expect("read footer")
        .expect("footer should be present");

    assert_eq!(
        Reader::sidx_covers_segment_tail(tmp.path(), &entries),
        SidxTailCoverage::Complete,
        "PROPERTY: SIDX coverage is complete only when the last indexed frame tail reaches the footer start"
    );
}

#[test]
fn sidx_covers_segment_tail_rejects_trailing_unindexed_bytes() {
    let tmp = footer_file(80, &[sample_entry(0, 64)]);
    let (entries, _) = read_footer(tmp.path())
        .expect("read footer")
        .expect("footer should be present");

    assert_eq!(
        Reader::sidx_covers_segment_tail(tmp.path(), &entries),
        SidxTailCoverage::Incomplete,
        "PROPERTY: trailing bytes between the last indexed frame and the footer force frame-scan fallback"
    );
}

#[test]
fn sidx_covers_segment_tail_treats_truly_empty_segment_as_covered() {
    let tmp = footer_file(0, &[]);
    let (entries, _) = read_footer(tmp.path())
        .expect("read footer")
        .expect("footer should be present");

    assert!(
        entries.is_empty(),
        "SANITY: empty-footer fixture should not produce SIDX entries"
    );
    assert_eq!(
        Reader::sidx_covers_segment_tail(tmp.path(), &entries),
        SidxTailCoverage::Complete,
        "PROPERTY: an empty segment with an empty SIDX footer is fully covered and must not be forced onto the frame-scan fallback"
    );
}

#[test]
fn sidx_covers_segment_tail_rejects_empty_footer_after_frames() {
    let tmp = footer_file(64, &[]);
    let (entries, _) = read_footer(tmp.path())
        .expect("read footer")
        .expect("footer should be present");

    assert!(
        entries.is_empty(),
        "SANITY: fixture should have frames before an empty SIDX footer"
    );
    assert_eq!(
        Reader::sidx_covers_segment_tail(tmp.path(), &entries),
        SidxTailCoverage::Incomplete,
        "PROPERTY: an empty SIDX footer does not cover preceding frame bytes and must force frame-scan fallback"
    );
}

#[test]
fn payload_read_unexpected_eof_respects_tail_policy() {
    assert_eq!(
        tail::classify_payload_read_error(
            7,
            std::io::Error::from(ErrorKind::UnexpectedEof),
            FrameScanTailPolicy::RecoverTornTail,
        )
        .expect("latest tail EOF should be recoverable"),
        PayloadReadFailure::RecoverTornTail,
        "PROPERTY: only the latest tail policy may turn payload EOF into torn-tail recovery"
    );

    let err = tail::classify_payload_read_error(
        7,
        std::io::Error::from(ErrorKind::UnexpectedEof),
        FrameScanTailPolicy::FailClosed,
    )
    .expect_err("non-tail payload EOF must fail closed");
    assert!(
        matches!(
            err,
            StoreError::CorruptSegment { ref detail, .. }
            if detail.contains("frame payload ended before requested length")
        ),
        "PROPERTY: non-tail payload EOF must surface as committed-frame corruption, got {err:?}"
    );
}

#[test]
fn payload_read_non_eof_error_is_never_torn_tail_recovery() {
    let err = tail::classify_payload_read_error(
        7,
        std::io::Error::from(ErrorKind::PermissionDenied),
        FrameScanTailPolicy::RecoverTornTail,
    )
    .expect_err("non-EOF read errors must remain I/O errors");
    assert!(
        matches!(err, StoreError::Io(_)),
        "PROPERTY: torn-tail recovery applies only to UnexpectedEof, got {err:?}"
    );
}

#[test]
fn scan_segment_index_into_uses_sidx_fast_path_for_sealed_segments() {
    let dir = TempDir::new().expect("create temp dir");
    let reader = Reader::new(
        dir.path().to_path_buf(),
        4,
        &(std::sync::Arc::new(crate::store::SystemClock::new())
            as std::sync::Arc<dyn crate::store::Clock>),
    );
    let segment_id = 7;
    let path = footer_segment_path(&dir, segment_id, 64, &[sample_entry(0, 64)]);
    reader.set_active_segment(segment_id + 1);

    let mut rows = Vec::new();
    reader
        .scan_segment_index_into_with_tail_policy(
            &path,
            None,
            FrameScanTailPolicy::FailClosed,
            |row| {
                rows.push(row);
                Ok(())
            },
        )
        .expect("sealed scan should succeed");

    assert_eq!(
            rows.len(),
            1,
            "PROPERTY: sealed segments with full SIDX tail coverage should use the footer fast path and emit indexed rows"
        );
}

#[test]
fn scan_segment_index_into_uses_sidx_fast_path_when_batch_state_is_idle() {
    let dir = TempDir::new().expect("create temp dir");
    let reader = Reader::new(
        dir.path().to_path_buf(),
        4,
        &(std::sync::Arc::new(crate::store::SystemClock::new())
            as std::sync::Arc<dyn crate::store::Clock>),
    );
    let segment_id = 7;
    let path = footer_segment_path(&dir, segment_id, 64, &[sample_entry(0, 64)]);
    reader.set_active_segment(segment_id + 1);
    let mut batch_state = BatchRecoveryState::default();

    let mut rows = Vec::new();
    reader
        .scan_segment_index_into_with_tail_policy(
            &path,
            Some(&mut batch_state),
            FrameScanTailPolicy::FailClosed,
            |row| {
                rows.push(row);
                Ok(())
            },
        )
        .expect("idle batch state should not disable the SIDX fast path");

    assert_eq!(
            rows.len(),
            1,
            "PROPERTY: an idle cross-segment batch state is equivalent to no batch state for SIDX fast-path admission"
        );
}

#[test]
fn scan_segment_index_into_rejects_sidx_fast_path_when_batch_is_pending() {
    let dir = TempDir::new().expect("create temp dir");
    let reader = Reader::new(
        dir.path().to_path_buf(),
        4,
        &(std::sync::Arc::new(crate::store::SystemClock::new())
            as std::sync::Arc<dyn crate::store::Clock>),
    );
    let segment_id = 7;
    let path = footer_segment_path(&dir, segment_id, 64, &[sample_entry(0, 64)]);
    reader.set_active_segment(segment_id + 1);
    let mut batch_state = BatchRecoveryState {
        in_batch: true,
        remaining: 1,
        started_count: 1,
        staged: Vec::new(),
    };

    let mut rows = Vec::new();
    let err = reader
        .scan_segment_index_into_with_tail_policy(
            &path,
            Some(&mut batch_state),
            FrameScanTailPolicy::FailClosed,
            |row| {
                rows.push(row);
                Ok(())
            },
        )
        .expect_err("pending batch state must force frame scan over synthetic footer bytes");

    assert!(
            matches!(err, StoreError::CorruptSegment { .. }),
            "PROPERTY: a pending cross-segment batch must not trust a SIDX footer until the batch is resolved; got {err:?}"
        );
    assert!(
        rows.is_empty(),
        "PROPERTY: rejecting the SIDX fast path while a batch is pending must not emit footer rows"
    );
}

#[test]
fn scan_segment_index_into_filters_batch_markers_from_sidx_fast_path() {
    let dir = TempDir::new().expect("create temp dir");
    let reader = Reader::new(
        dir.path().to_path_buf(),
        4,
        &(std::sync::Arc::new(crate::store::SystemClock::new())
            as std::sync::Arc<dyn crate::store::Clock>),
    );
    let segment_id = 7;
    let mut begin = sample_entry(0, 64);
    begin.event_id = 10;
    begin.kind = kind_to_raw(EventKind::SYSTEM_BATCH_BEGIN);
    begin.global_sequence = 10;
    let mut item = sample_entry(64, 64);
    item.event_id = 11;
    item.kind = kind_to_raw(EventKind::custom(0x1, 2));
    item.global_sequence = 11;
    let mut commit = sample_entry(128, 64);
    commit.event_id = 12;
    commit.kind = kind_to_raw(EventKind::SYSTEM_BATCH_COMMIT);
    commit.global_sequence = 12;
    let path = footer_segment_path(&dir, segment_id, 192, &[begin, item, commit]);
    reader.set_active_segment(segment_id + 1);

    let mut rows = Vec::new();
    reader
        .scan_segment_index_into_with_tail_policy(
            &path,
            None,
            FrameScanTailPolicy::FailClosed,
            |row| {
                rows.push(row);
                Ok(())
            },
        )
        .expect("sealed scan should succeed");

    assert_eq!(
            rows.len(),
            1,
            "PROPERTY: SIDX fast-path recovery must filter BEGIN/COMMIT markers and emit only logical user rows"
        );
    assert_eq!(
        rows[0].header.event_id,
        crate::id::EventId::from(11u128),
        "PROPERTY: filtering batch markers must preserve the real batch item"
    );
    assert_eq!(
        rows[0].header.event_kind,
        EventKind::custom(0x1, 2),
        "PROPERTY: filtering batch markers must not rewrite the surviving item kind"
    );
}

#[test]
fn scan_segment_index_into_ignores_sidx_footer_for_active_segments() {
    let dir = TempDir::new().expect("create temp dir");
    let reader = Reader::new(
        dir.path().to_path_buf(),
        4,
        &(std::sync::Arc::new(crate::store::SystemClock::new())
            as std::sync::Arc<dyn crate::store::Clock>),
    );
    let segment_id = 7;
    let path = footer_segment_path(&dir, segment_id, 64, &[sample_entry(0, 64)]);
    reader.set_active_segment(segment_id);

    let mut rows = Vec::new();
    let err = reader
        .scan_segment_index_into_with_tail_policy(
            &path,
            None,
            FrameScanTailPolicy::FailClosed,
            |row| {
                rows.push(row);
                Ok(())
            },
        )
        .expect_err("active segment must not trust the synthetic SIDX footer fixture");

    assert!(
            matches!(err, StoreError::CorruptSegment { .. }),
            "PROPERTY: the active segment must refuse the SIDX fast path and fall back to frame scan even if a footer is present"
        );
    assert!(
            rows.is_empty(),
            "PROPERTY: falling back to frame scan must not synthesize rows from the active segment's footer bytes"
        );
}
