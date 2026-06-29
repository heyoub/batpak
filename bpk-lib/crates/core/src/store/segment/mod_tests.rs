//! Inline test island for `segment/mod.rs`, extracted to a sibling file to
//! keep `mod.rs` under the production-file line cap (the StoreFs durability
//! seam added for GAUNTLET-B2 pushed the combined file over budget). These
//! pin segment rotation-threshold accounting, create+fsync durability, and
//! SIDX boundary detection.

use super::*;
use tempfile::TempDir;

#[test]
fn needs_rotation_tracks_written_bytes_threshold() {
    let dir = TempDir::new().expect("tmpdir");
    let fs: std::sync::Arc<dyn crate::store::platform::fs::StoreFs> =
        std::sync::Arc::new(crate::store::platform::fs::RealFs);
    let mut segment: Segment<Active> =
        Segment::create_with_created_ns_on(dir.path(), 1, 0, &fs).expect("create segment");
    let frame =
        frame_encode(&serde_json::json!({"payload": "rotation-threshold"})).expect("encode frame");

    assert!(
        !segment.needs_rotation(1024),
        "PROPERTY: a fresh segment must not report rotation before any frames are written"
    );

    segment.write_frame(&frame).expect("write frame");

    assert!(
        segment.needs_rotation(1),
        "PROPERTY: needs_rotation(max_bytes=1) must flip true after any real frame write"
    );
    assert!(
        !segment.needs_rotation(1024),
        "PROPERTY: needs_rotation must stay false below the threshold"
    );
}

#[test]
fn create_with_created_ns_fsyncs_content_and_directory_entry() {
    let dir = TempDir::new().expect("tmpdir");
    let segment_id = 42u64;
    let created_ns = 1_234_567i64;
    {
        // Drop the segment immediately after create so only fsynced bytes
        // remain; nothing else writes to or flushes the file.
        let fs: std::sync::Arc<dyn crate::store::platform::fs::StoreFs> =
            std::sync::Arc::new(crate::store::platform::fs::RealFs);
        let _segment: Segment<Active> =
            Segment::create_with_created_ns_on(dir.path(), segment_id, created_ns, &fs)
                .expect("create segment");
    }

    // The directory entry must be present (dir fsync) and the header bytes
    // durable (file fsync): reopen and round-trip magic + header. The reopen
    // succeeding is itself the directory-entry-visibility proof — open_file
    // fails if the freshly-created entry is not visible — so no separate
    // read_dir scan is needed (and store-layer code must not touch the
    // filesystem directly outside src/store/platform).
    let path = dir.path().join(segment_filename(segment_id));
    let mut file = crate::store::platform::fs::open_file(&path).expect("reopen segment");
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).expect("read magic");
    assert_eq!(&magic, SEGMENT_MAGIC, "PROPERTY: magic must be durable");

    let mut header_len_buf = [0u8; 4];
    file.read_exact(&mut header_len_buf)
        .expect("read header_len");
    let header_len = u32::from_be_bytes(header_len_buf) as usize;
    let mut header_buf = vec![0u8; header_len];
    file.read_exact(&mut header_buf).expect("read header");
    let header: SegmentHeader = crate::encoding::from_bytes(&header_buf).expect("decode header");

    assert_eq!(
        header.segment_id, segment_id,
        "PROPERTY: segment_id must round-trip after create + reopen, proving content is fsynced"
    );
    assert_eq!(header.version, 1, "PROPERTY: version must round-trip");
    assert_eq!(
        header.created_ns, created_ns,
        "PROPERTY: created_ns must round-trip"
    );
}

/// Build a minimal in-memory buffer whose last 16 bytes are a SIDX trailer
/// with the given `string_table_offset`, valid magic, and zero entry_count.
fn sidx_trailer_buf(total_len: usize, string_table_offset: u64) -> Vec<u8> {
    assert!(total_len >= 16, "buffer must hold the 16-byte trailer");
    let mut bytes = vec![0u8; total_len];
    let trailer_start = total_len - 16;
    bytes[trailer_start..trailer_start + 8].copy_from_slice(&string_table_offset.to_le_bytes());
    bytes[trailer_start + 8..trailer_start + 12].copy_from_slice(&0u32.to_le_bytes());
    bytes[trailer_start + 12..trailer_start + 16]
        .copy_from_slice(crate::store::segment::sidx::SIDX_MAGIC);
    bytes
}

#[test]
fn detect_sidx_boundary_accepts_offset_at_max() {
    let file_len = 64u64;
    let max_offset = file_len - 16; // empty string table boundary, valid
    let bytes = sidx_trailer_buf(
        usize::try_from(file_len).expect("file_len fits usize"),
        max_offset,
    );
    let mut cursor = std::io::Cursor::new(bytes);
    let result = detect_sidx_boundary(&mut cursor, file_len, 7);
    assert_eq!(
        result
            .expect("must not error at the max boundary")
            .map(|b| b.frames_end),
        Some(max_offset),
        "PROPERTY: offset == file_len - 16 is the empty-string-table boundary and must be accepted"
    );
}

#[test]
fn detect_sidx_boundary_recognizes_legacy_sdx2_magic() {
    // A pre-0.8.3 segment ends in the legacy `SDX2` magic with the same
    // 16-byte trailer geometry. detect_sidx_boundary must recognize it as a
    // footer BOUNDARY (so the frame scan stops at string_table_offset)
    // even though read_footer refuses to TRUST its un-CRC'd content.
    let file_len = 64u64;
    let max_offset = file_len - 16;
    let mut bytes = vec![0u8; usize::try_from(file_len).expect("file_len fits usize")];
    let trailer_start = bytes.len() - 16;
    bytes[trailer_start..trailer_start + 8].copy_from_slice(&max_offset.to_le_bytes());
    bytes[trailer_start + 8..trailer_start + 12].copy_from_slice(&0u32.to_le_bytes());
    bytes[trailer_start + 12..trailer_start + 16]
        .copy_from_slice(crate::store::segment::sidx::SIDX_MAGIC_LEGACY_SDX2);
    let mut cursor = std::io::Cursor::new(bytes);
    let result = detect_sidx_boundary(&mut cursor, file_len, 7).expect("must not error");
    assert_eq!(
        result,
        Some(SidxBoundary {
            frames_end: max_offset,
            // A legacy SDX2 footer carries no CRC, so its offset is recognized
            // as a boundary but is NEVER trusted.
            trusted: false,
        }),
        "PROPERTY: a legacy SDX2 trailer must be recognized as a frame-region boundary"
    );
    assert!(
        !result.expect("boundary present").trusted,
        "PROPERTY: an un-CRC'd SDX2 boundary must be flagged untrusted"
    );
}

#[test]
fn detect_sidx_boundary_no_magic_returns_none() {
    // A tail without the SIDX magic must read as "no footer", not error.
    let bytes = vec![0u8; 64];
    let file_len = bytes.len() as u64;
    let mut cursor = std::io::Cursor::new(bytes);
    let result = detect_sidx_boundary(&mut cursor, file_len, 7).expect("must not error");
    assert_eq!(result, None, "PROPERTY: absent SIDX magic must return None");
}

#[test]
fn detect_sidx_boundary_tiny_file_returns_none() {
    let bytes = vec![0xAA; 8]; // < 16-byte trailer
    let file_len = bytes.len() as u64;
    let mut cursor = std::io::Cursor::new(bytes);
    let result = detect_sidx_boundary(&mut cursor, file_len, 7).expect("must not error");
    assert_eq!(
        result, None,
        "PROPERTY: a file smaller than the trailer must return None"
    );
}
