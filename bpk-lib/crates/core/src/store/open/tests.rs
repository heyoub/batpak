use super::*;
use tempfile::TempDir;

#[test]
fn next_active_segment_id_is_one_past_latest_existing_segment() -> Result<(), StoreError> {
    let dir = TempDir::new()?;
    crate::store::platform::fs::write_derivative_file_atomically(
        dir.path(),
        &dir.path().join(segment::segment_filename(1)),
        "test segment",
        b"",
    )?;
    crate::store::platform::fs::write_derivative_file_atomically(
        dir.path(),
        &dir.path().join(segment::segment_filename(7)),
        "test segment",
        b"",
    )?;

    assert_eq!(
        next_active_segment_id(dir.path())?,
        8,
        "PROPERTY: reader active segment must be one past the highest existing segment so the last sealed segment remains mmap-eligible"
    );
    Ok(())
}

#[test]
fn highest_index_hlc_reports_non_origin_point_for_appended_entry() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    let coord = Coordinate::new("entity:highest-hlc", "scope:test").expect("coord");
    let receipt = store
        .append(
            &coord,
            EventKind::custom(0xF, 0x77),
            &serde_json::json!({"x": 1}),
        )
        .expect("append");

    let point = highest_index_hlc(&store.index);

    assert_eq!(
        point.global_sequence, receipt.sequence,
        "PROPERTY: highest_index_hlc must observe the committed entry's global sequence"
    );
    assert!(
        point > HlcPoint::ORIGIN,
        "PROPERTY: highest_index_hlc must not collapse a non-empty index to origin/default"
    );

    store.close().expect("close");
}
