use super::{StoreError, StoreLockMode};
use std::error::Error as _;
use std::io;

fn assert_display_contains(error: &StoreError, needle: &str) {
    let display = error.to_string();
    assert!(
        display.contains(needle),
        "helper constructor display should contain {needle:?}, got {display:?}"
    );
}

#[test]
fn batch_failed_helper_preserves_item_index_and_source() {
    let error = StoreError::batch_failed(
        3,
        StoreError::Io(io::Error::new(io::ErrorKind::TimedOut, "append timed out")),
    );

    assert!(matches!(
        &error,
        StoreError::BatchFailed {
            item_index: 3,
            source
        } if matches!(source.as_ref(), StoreError::Io(_))
    ));
    assert_display_contains(&error, "batch failed at item 3");
    assert_display_contains(&error, "append timed out");
    assert!(
        error
            .source()
            .is_some_and(|source| source.to_string().contains("append timed out")),
        "BatchFailed helper should expose the wrapped StoreError as source"
    );
}

#[test]
fn batch_sync_failed_helper_preserves_count_and_source() {
    let error = StoreError::batch_sync_failed(4, StoreError::Io(io::Error::other("fsync failed")));

    assert!(matches!(
        &error,
        StoreError::BatchSyncFailed {
            item_count: 4,
            source
        } if matches!(source.as_ref(), StoreError::Io(_))
    ));
    assert_display_contains(&error, "batch sync failed after writing 4 items");
    assert_display_contains(&error, "fsync failed");
    assert!(
        error
            .source()
            .is_some_and(|source| source.to_string().contains("fsync failed")),
        "BatchSyncFailed helper should expose the wrapped StoreError as source"
    );
}

#[test]
fn corrupt_magic_helper_builds_corrupt_segment() {
    let error = StoreError::corrupt_magic(9);

    assert!(
        matches!(
            error,
            StoreError::CorruptSegment {
                segment_id: 9,
                ref detail
            } if detail == "bad magic"
        ),
        "expected bad-magic CorruptSegment, got {error:?}"
    );
    assert_display_contains(&error, "corrupt segment 9");
    assert_display_contains(&error, "bad magic");
    assert!(error.source().is_none());
}

#[test]
fn corrupt_eof_helper_builds_corrupt_segment() {
    let error = StoreError::corrupt_eof(11);

    assert!(
        matches!(
            error,
            StoreError::CorruptSegment {
                segment_id: 11,
                ref detail
            } if detail == "unexpected EOF during read"
        ),
        "expected EOF CorruptSegment, got {error:?}"
    );
    assert_display_contains(&error, "corrupt segment 11");
    assert_display_contains(&error, "unexpected EOF during read");
    assert!(error.source().is_none());
}

#[test]
fn corrupt_version_helper_builds_corrupt_segment() {
    let error = StoreError::corrupt_version(12, 99);

    assert!(
        matches!(
            error,
            StoreError::CorruptSegment {
                segment_id: 12,
                ref detail
            } if detail.contains("unsupported segment version: 99")
        ),
        "expected version CorruptSegment, got {error:?}"
    );
    assert_display_contains(&error, "corrupt segment 12");
    assert_display_contains(&error, "unsupported segment version: 99");
    assert!(error.source().is_none());
}

#[test]
fn cache_msg_helper_builds_cache_failed_without_typed_source() {
    let error = StoreError::cache_msg("cache metadata short read");

    assert!(matches!(error, StoreError::CacheFailed(_)));
    assert_display_contains(&error, "cache error");
    assert_display_contains(&error, "cache metadata short read");
    assert!(
        error
            .source()
            .is_some_and(|source| source.to_string().contains("cache metadata short read")),
        "CacheFailed helper should expose the boxed message error as source"
    );
}

#[test]
fn cache_error_helper_builds_cache_failed_with_typed_source() {
    let error = StoreError::cache_error(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "cache dir denied",
    ));

    assert!(matches!(error, StoreError::CacheFailed(_)));
    assert_display_contains(&error, "cache error");
    assert_display_contains(&error, "cache dir denied");
    assert!(
        error
            .source()
            .is_some_and(|source| source.to_string().contains("cache dir denied")),
        "CacheFailed typed helper should expose the wrapped source"
    );
}

#[test]
fn ser_msg_helper_builds_serialization_error() {
    let error = StoreError::ser_msg("frame exceeds u32::MAX");

    assert!(matches!(error, StoreError::Serialization(_)));
    assert_display_contains(&error, "serialization error");
    assert_display_contains(&error, "frame exceeds u32::MAX");
    assert!(
        error
            .source()
            .is_some_and(|source| source.to_string().contains("frame exceeds u32::MAX")),
        "Serialization helper should expose the boxed message error as source"
    );
}

#[test]
fn corrupt_segment_with_detail_helper_builds_corrupt_segment() {
    let error = StoreError::corrupt_segment_with_detail(13, "valid CRC but malformed msgpack");

    assert!(
        matches!(
            error,
            StoreError::CorruptSegment {
                segment_id: 13,
                ref detail
            } if detail == "valid CRC but malformed msgpack"
        ),
        "expected detail-preserving CorruptSegment, got {error:?}"
    );
    assert_display_contains(&error, "corrupt segment 13");
    assert_display_contains(&error, "valid CRC but malformed msgpack");
    assert!(error.source().is_none());
}

#[test]
fn store_locked_display_names_modes() {
    let read_only = StoreError::StoreLocked {
        path: "fixtures/store".into(),
        mode: StoreLockMode::ReadOnly,
    };
    let mutable = StoreError::StoreLocked {
        path: "fixtures/store".into(),
        mode: StoreLockMode::Mutable,
    };

    assert_display_contains(&read_only, "read-only");
    assert_display_contains(&mutable, "mutable");
}
