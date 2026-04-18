// justifies: unified red-path config tests use unwrap/panic as assertion style and narrow bounded test counters that fit within u32.
#![allow(clippy::unwrap_used, clippy::cast_possible_truncation, clippy::panic)]

#[path = "support/unified_red.rs"]
mod unified_red_support;

use unified_red_support::*;

#[test]
fn config_validation_rejects_zero_segment_max_bytes() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_segment_max_bytes(0);
    let err = match Store::open(config) {
        Ok(_) => panic!(
            "PROPERTY: segment_max_bytes=0 must be rejected at open time. \
             Investigate: src/store/config.rs StoreConfig::validate."
        ),
        Err(e) => e,
    };
    assert!(
        matches!(err, StoreError::Configuration { .. }),
        "PROPERTY: must surface as StoreError::Configuration, got {err:?}"
    );
}

#[test]
fn config_validation_rejects_zero_writer_channel_capacity() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_writer_channel_capacity(0);
    let err = match Store::open(config) {
        Ok(_) => panic!(
            "PROPERTY: writer.channel_capacity=0 must be rejected at open time \
             (a zero-capacity channel deadlocks on the first append). \
             Investigate: src/store/config.rs StoreConfig::validate."
        ),
        Err(e) => e,
    };
    assert!(
        matches!(err, StoreError::Configuration { .. }),
        "PROPERTY: must surface as StoreError::Configuration, got {err:?}"
    );
}
