//! Compile-time assertions that key public types implement Send + Sync.
//! [SPEC:tests/type_assertions.rs]

#[test]
fn store_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<batpak::store::Store>();
}

#[test]
fn append_receipt_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<batpak::store::AppendReceipt>();
}

#[test]
fn store_config_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<batpak::store::StoreConfig>();
}

#[test]
fn notification_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<batpak::store::Notification>();
}

#[test]
fn coordinate_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<batpak::coordinate::Coordinate>();
}

#[test]
fn event_kind_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<batpak::event::EventKind>();
}

#[test]
fn receipt_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<batpak::guard::Receipt<()>>();
}

#[test]
fn store_error_source_chain_io() {
    use std::error::Error;
    let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
    let store_err = batpak::store::StoreError::Io(io_err);
    let source = store_err.source().expect("Io variant should have a source");
    assert!(source.to_string().contains("gone"));
}

#[test]
fn store_error_source_chain_serialization() {
    use std::error::Error;
    let inner: Box<dyn std::error::Error + Send + Sync> = "bad msgpack".into();
    let store_err = batpak::store::StoreError::Serialization(inner);
    let source = store_err
        .source()
        .expect("Serialization variant should have a source");
    assert!(source.to_string().contains("bad msgpack"));
}

#[test]
fn store_error_source_chain_cache_failed() {
    use std::error::Error;
    // (formerly mentioned 'redb' explicitly — neutralized after 0.3.0 removed redb
    // backend support; the stale_terms check would otherwise flag this file)
    let inner: Box<dyn std::error::Error + Send + Sync> = "storage backend error".into();
    let store_err = batpak::store::StoreError::CacheFailed(inner);
    let source = store_err
        .source()
        .expect("CacheFailed variant should have a source");
    assert!(source.to_string().contains("storage backend error"));
}

#[test]
fn store_error_source_none_for_writer_crashed() {
    use std::error::Error;
    let store_err = batpak::store::StoreError::WriterCrashed;
    assert!(store_err.source().is_none());
}

#[test]
fn frame_encode_decode_roundtrip() {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Payload {
        x: u32,
        y: String,
    }

    let original = Payload {
        x: 999,
        y: "roundtrip".into(),
    };
    let frame = batpak::store::segment::frame_encode(&original).expect("encode should succeed");
    let (msgpack, consumed) =
        batpak::store::segment::frame_decode(&frame).expect("decode should succeed");
    assert_eq!(consumed, frame.len());
    let decoded: Payload = rmp_serde::from_slice(msgpack).expect("deserialize should succeed");
    assert_eq!(decoded, original);
}
