// justifies: INV-TEST-PANIC-AS-ASSERTION; DecodeTyped lane tests in tests/decode_typed_seam.rs treat decode failures as test failures; unwrap and panic are the assertion style throughout, and the catch-all match arms panic on any unexpected TypedDecodeError variant (including future ones such as FutureVersion) which is the intended assertion.
#![allow(clippy::unwrap_used, clippy::panic, clippy::wildcard_enum_match_arm)]
//! Per-lane behavioural tests for the `DecodeTyped` seam (Dispatch Chapter T1).
//!
//! Both replay lanes (`Event<serde_json::Value>` and `Event<Vec<u8>>`) must
//! share an identical behavioural contract:
//!   * route_typed on mismatch → Ok(None)
//!   * route_typed on match + success → Ok(Some(T))
//!   * route_typed on match + decode fail → Err(DecodeFailure)
//!   * decode_typed on mismatch → Err(KindMismatch)
//!   * decode_typed on match + decode fail → Err(DecodeFailure)
//!
//! PROVES: LAW-003 (no orphan infrastructure); the seam is live on both lanes.
//! DEFENDS: invariant 5 (neither lane privileged).

use batpak::coordinate::DagPosition;
use batpak::event::{DecodeSource, DecodeTyped, Event, EventHeader, EventKind, TypedDecodeError};
// Bring in both the trait AND the derive macro — crate-root re-export gives us both namespaces.
use batpak::EventPayload;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 1, type_id = 1)]
struct Alpha {
    value: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 1, type_id = 2)]
struct Beta {
    label: String,
}

fn make_header(event_id: u128, kind: EventKind, payload_size: u32) -> EventHeader {
    EventHeader::new(
        event_id,
        0,
        None,
        0,
        DagPosition::root(),
        payload_size,
        kind,
    )
}

fn json_event(kind: EventKind, payload: serde_json::Value) -> Event<serde_json::Value> {
    Event::new(make_header(1, kind, 0), payload)
}

fn msgpack_event(kind: EventKind, bytes: Vec<u8>) -> Event<Vec<u8>> {
    let size = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    Event::new(make_header(2, kind, size), bytes)
}

fn assert_decode_typed_lane<T: DecodeTyped + ?Sized>(_event: &T) {}

// ─── JSON lane ────────────────────────────────────────────────────────────────

mod json_lane {
    use super::*;

    #[test]
    fn route_typed_match_decode_ok() {
        let event = json_event(Alpha::KIND, serde_json::json!({ "value": 42 }));
        assert_decode_typed_lane(&event);
        let routed: Option<Alpha> = event.route_typed().expect("route_typed");
        assert_eq!(routed, Some(Alpha { value: 42 }));
    }

    #[test]
    fn route_typed_kind_mismatch_returns_none() {
        let event = json_event(Beta::KIND, serde_json::json!({ "label": "x" }));
        let routed: Option<Alpha> = event.route_typed().expect("route_typed");
        assert!(
            routed.is_none(),
            "PROPERTY: route_typed must return Ok(None) on kind mismatch, not an error"
        );
    }

    #[test]
    fn route_typed_decode_failure_propagates_err() {
        // kind matches Alpha, but payload is shaped like Beta — decode fails.
        let event = json_event(Alpha::KIND, serde_json::json!({ "label": "not a number" }));
        let result: Result<Option<Alpha>, TypedDecodeError> = event.route_typed();
        let err = result.expect_err("kind matched but decode should fail");
        match err {
            TypedDecodeError::DecodeFailure { kind, source } => {
                assert_eq!(kind, Alpha::KIND);
                assert!(matches!(source, DecodeSource::Json(_)));
            }
            other => panic!("expected DecodeFailure, got {other:?}"),
        }
    }

    #[test]
    fn decode_typed_kind_mismatch_returns_kind_mismatch_err() {
        let event = json_event(Beta::KIND, serde_json::json!({ "label": "x" }));
        let result: Result<Alpha, TypedDecodeError> = event.decode_typed();
        match result {
            Err(TypedDecodeError::KindMismatch { expected, got }) => {
                assert_eq!(expected, Alpha::KIND);
                assert_eq!(got, Beta::KIND);
            }
            other => panic!("expected KindMismatch, got {other:?}"),
        }
    }

    #[test]
    fn decode_typed_decode_failure_returns_decode_failure_err() {
        let event = json_event(Alpha::KIND, serde_json::json!({ "wrong": true }));
        let result: Result<Alpha, TypedDecodeError> = event.decode_typed();
        match result {
            Err(TypedDecodeError::DecodeFailure { kind, source }) => {
                assert_eq!(kind, Alpha::KIND);
                assert!(matches!(source, DecodeSource::Json(_)));
            }
            other => panic!("expected DecodeFailure, got {other:?}"),
        }
    }
}

// ─── msgpack lane ─────────────────────────────────────────────────────────────

mod msgpack_lane {
    use super::*;

    #[test]
    fn route_typed_match_decode_ok() {
        let bytes = rmp_serde::to_vec_named(&Alpha { value: 77 }).expect("encode");
        let event = msgpack_event(Alpha::KIND, bytes);
        let routed: Option<Alpha> = event.route_typed().expect("route_typed");
        assert_eq!(routed, Some(Alpha { value: 77 }));
    }

    #[test]
    fn route_typed_kind_mismatch_returns_none() {
        let bytes = rmp_serde::to_vec_named(&Beta { label: "x".into() }).expect("encode");
        let event = msgpack_event(Beta::KIND, bytes);
        let routed: Option<Alpha> = event.route_typed().expect("route_typed");
        assert!(
            routed.is_none(),
            "PROPERTY: route_typed must return Ok(None) on kind mismatch, not an error"
        );
    }

    #[test]
    fn route_typed_decode_failure_propagates_err() {
        // kind says Alpha but bytes decode as Beta — fails.
        let bytes = rmp_serde::to_vec_named(&Beta { label: "x".into() }).expect("encode");
        let event = msgpack_event(Alpha::KIND, bytes);
        let result: Result<Option<Alpha>, TypedDecodeError> = event.route_typed();
        let err = result.expect_err("kind matched but decode should fail");
        match err {
            TypedDecodeError::DecodeFailure { kind, source } => {
                assert_eq!(kind, Alpha::KIND);
                assert!(matches!(source, DecodeSource::Msgpack(_)));
            }
            other => panic!("expected DecodeFailure, got {other:?}"),
        }
    }

    #[test]
    fn decode_typed_kind_mismatch_returns_kind_mismatch_err() {
        let bytes = rmp_serde::to_vec_named(&Beta { label: "x".into() }).expect("encode");
        let event = msgpack_event(Beta::KIND, bytes);
        let result: Result<Alpha, TypedDecodeError> = event.decode_typed();
        match result {
            Err(TypedDecodeError::KindMismatch { expected, got }) => {
                assert_eq!(expected, Alpha::KIND);
                assert_eq!(got, Beta::KIND);
            }
            other => panic!("expected KindMismatch, got {other:?}"),
        }
    }

    #[test]
    fn decode_typed_decode_failure_returns_decode_failure_err() {
        let event = msgpack_event(Alpha::KIND, b"not-valid-msgpack".to_vec());
        let result: Result<Alpha, TypedDecodeError> = event.decode_typed();
        match result {
            Err(TypedDecodeError::DecodeFailure { kind, source }) => {
                assert_eq!(kind, Alpha::KIND);
                assert!(matches!(source, DecodeSource::Msgpack(_)));
            }
            other => panic!("expected DecodeFailure, got {other:?}"),
        }
    }
}
