//! PROVES: NETBAT/2 streaming frame encode/decode contract (Packet B).
//! CATCHES: grammar drift, limit bypasses, and NETBAT/1 cross-acceptance.
//! SEEDED: golden .hex fixtures and bounded proptest vectors.

use netbat as nb;
use proptest::prelude::*;

const STREAM_SUBSCRIBE_HEX: &str = include_str!("golden/stream_subscribe_v2.hex");
const STREAM_SUB_EVENT_HEX: &str = include_str!("golden/stream_sub_event_v2.hex");
const STREAM_SUB_WATERMARK_HEX: &str = include_str!("golden/stream_sub_watermark_v2.hex");
const STREAM_SUB_ACK_HEX: &str = include_str!("golden/stream_sub_ack_v2.hex");
const STREAM_SUB_CANCEL_HEX: &str = include_str!("golden/stream_sub_cancel_v2.hex");
const STREAM_SUB_ERR_HEX: &str = include_str!("golden/stream_sub_err_v2.hex");
const STREAM_SUB_END_HEX: &str = include_str!("golden/stream_sub_end_v2.hex");

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

fn fixture_bytes(hex: &str) -> Vec<u8> {
    let hex = hex.trim();
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).expect("fixture hex is utf8");
            u8::from_str_radix(pair, 16).expect("fixture hex decodes")
        })
        .collect()
}

fn limits() -> nb::Limits {
    nb::Limits::default()
}

fn golden_subscribe_frame() -> nb::StreamFrame {
    nb::StreamFrame::Subscribe(nb::SubscribeFrame {
        subscription_id: nb::SubscriptionToken::new("orders.open.v1", &limits())
            .expect("subscription id"),
        resume_cursor: nb::MaybeCursor::Absent,
        client_window: nb::ClientWindow::new(128).expect("client window"),
    })
}

fn golden_sub_event_frame() -> nb::StreamFrame {
    nb::StreamFrame::SubEvent(nb::SubEventFrame {
        subscription_id: nb::SubscriptionToken::new("orders.open.v1", &limits())
            .expect("subscription id"),
        delivery_index: nb::DeliveryIndex::new(1).expect("delivery index"),
        cursor_before: nb::MaybeCursor::Absent,
        cursor_after: nb::MaybeCursor::Present(nb::CursorBytes::new([0x0a])),
        payload_schema_ref: nb::PayloadSchemaRef::new("hostbat.event.orders.v1")
            .expect("schema ref"),
        payload: b"event".to_vec(),
    })
}

fn golden_sub_watermark_frame() -> nb::StreamFrame {
    nb::StreamFrame::SubWatermark(nb::SubWatermarkFrame {
        subscription_id: nb::SubscriptionToken::new("orders.open.v1", &limits())
            .expect("subscription id"),
        delivery_index: nb::DeliveryIndex::new(1).expect("delivery index"),
        cursor_after: nb::CursorBytes::new([0x0a]),
    })
}

fn golden_sub_ack_frame() -> nb::StreamFrame {
    nb::StreamFrame::SubAck(nb::SubAckFrame {
        subscription_id: nb::SubscriptionToken::new("orders.open.v1", &limits())
            .expect("subscription id"),
        delivery_index: nb::DeliveryIndex::new(1).expect("delivery index"),
        cursor_after: nb::CursorBytes::new([0x0a]),
    })
}

fn golden_sub_cancel_frame() -> nb::StreamFrame {
    nb::StreamFrame::SubCancel(nb::SubCancelFrame {
        subscription_id: nb::SubscriptionToken::new("orders.open.v1", &limits())
            .expect("subscription id"),
        reason_code: nb::StreamReasonCode::new("client.cancel").expect("reason"),
    })
}

fn golden_sub_err_frame() -> nb::StreamFrame {
    nb::StreamFrame::SubErr(nb::SubErrFrame {
        subscription_id: nb::SubscriptionToken::new("orders.open.v1", &limits())
            .expect("subscription id"),
        code: nb::StreamReasonCode::new("slow_consumer").expect("code"),
        last_delivered_cursor: nb::MaybeCursor::Present(nb::CursorBytes::new([0x0a])),
        last_acked_cursor: nb::MaybeCursor::Absent,
        message: b"slow".to_vec(),
    })
}

fn golden_sub_end_frame() -> nb::StreamFrame {
    nb::StreamFrame::SubEnd(nb::SubEndFrame {
        subscription_id: nb::SubscriptionToken::new("orders.open.v1", &limits())
            .expect("subscription id"),
        cursor_after: nb::MaybeCursor::Present(nb::CursorBytes::new([0x0a])),
        reason_code: nb::StreamReasonCode::new("stream.complete").expect("reason"),
    })
}

#[test]
fn stream_frame_goldens_match_encoder() {
    let cases = [
        (
            "stream_subscribe_v2",
            STREAM_SUBSCRIBE_HEX,
            golden_subscribe_frame(),
        ),
        (
            "stream_sub_event_v2",
            STREAM_SUB_EVENT_HEX,
            golden_sub_event_frame(),
        ),
        (
            "stream_sub_watermark_v2",
            STREAM_SUB_WATERMARK_HEX,
            golden_sub_watermark_frame(),
        ),
        (
            "stream_sub_ack_v2",
            STREAM_SUB_ACK_HEX,
            golden_sub_ack_frame(),
        ),
        (
            "stream_sub_cancel_v2",
            STREAM_SUB_CANCEL_HEX,
            golden_sub_cancel_frame(),
        ),
        (
            "stream_sub_err_v2",
            STREAM_SUB_ERR_HEX,
            golden_sub_err_frame(),
        ),
        (
            "stream_sub_end_v2",
            STREAM_SUB_END_HEX,
            golden_sub_end_frame(),
        ),
    ];
    for (name, golden_hex, frame) in cases {
        let encoded = nb::encode_stream_frame(&frame);
        assert_eq!(hex(&encoded), golden_hex.trim(), "{name} encoder drift");
        assert_eq!(
            encoded.last().copied(),
            Some(b'\n'),
            "{name} must end with \\n"
        );
    }
}

#[test]
fn encoded_stream_frames_use_lowercase_hex() {
    let frame = golden_sub_event_frame();
    let encoded = nb::encode_stream_frame(&frame);
    let text = std::str::from_utf8(&encoded).expect("ascii frame");
    for token in text.split(' ') {
        if token.chars().all(|ch| ch.is_ascii_hexdigit()) && !token.is_empty() {
            assert_eq!(token, token.to_ascii_lowercase());
        }
    }
}

#[test]
fn stream_frame_goldens_decode_to_expected_frames() {
    let cases = [
        (STREAM_SUBSCRIBE_HEX, golden_subscribe_frame()),
        (STREAM_SUB_EVENT_HEX, golden_sub_event_frame()),
        (STREAM_SUB_WATERMARK_HEX, golden_sub_watermark_frame()),
        (STREAM_SUB_ACK_HEX, golden_sub_ack_frame()),
        (STREAM_SUB_CANCEL_HEX, golden_sub_cancel_frame()),
        (STREAM_SUB_ERR_HEX, golden_sub_err_frame()),
        (STREAM_SUB_END_HEX, golden_sub_end_frame()),
    ];
    for (golden_hex, expected) in cases {
        let bytes = fixture_bytes(golden_hex);
        let decoded = nb::decode_stream_line(&bytes, &limits()).expect("golden decodes");
        assert_eq!(decoded, expected);
    }
}

#[test]
fn stream_frame_encode_decode_roundtrip_for_all_variants() {
    for frame in [
        golden_subscribe_frame(),
        golden_sub_event_frame(),
        golden_sub_watermark_frame(),
        golden_sub_ack_frame(),
        golden_sub_cancel_frame(),
        golden_sub_err_frame(),
        golden_sub_end_frame(),
    ] {
        let encoded = nb::encode_stream_frame(&frame);
        let decoded = nb::decode_stream_line(&encoded, &limits()).expect("roundtrip decodes");
        assert_eq!(decoded, frame);
    }
}

#[test]
fn decode_line_rejects_netbat2_stream_frames_as_call_requests() {
    let err = nb::decode_line(&fixture_bytes(STREAM_SUBSCRIBE_HEX), &limits())
        .expect_err("NETBAT/2 must not decode as NETBAT/1 call");
    assert_eq!(
        err,
        nb::NetbatError::UnsupportedProtocolVersion {
            version: "NETBAT/2".to_owned()
        }
    );
}

#[test]
fn decode_stream_line_rejects_netbat1_call_frames() {
    let err = nb::decode_stream_line(b"NETBAT/1 CALL ping 00\n", &limits())
        .expect_err("NETBAT/1 must not decode as stream frame");
    assert_eq!(
        err,
        nb::NetbatError::UnsupportedProtocolVersion {
            version: "NETBAT/1".to_owned()
        }
    );
}

#[test]
fn red_unsupported_stream_protocol_version() {
    let err = nb::decode_stream_line(b"NETBAT/3 SUBSCRIBE orders.open.v1 - 1\n", &limits())
        .expect_err("unsupported stream protocol");
    assert_eq!(
        err,
        nb::NetbatError::UnsupportedProtocolVersion {
            version: "NETBAT/3".to_owned()
        }
    );
}

#[test]
fn red_unknown_stream_verb() {
    let err = nb::decode_stream_line(b"NETBAT/2 SUB_FUTURE orders.open.v1\n", &limits())
        .expect_err("unknown verb");
    assert_eq!(
        err,
        nb::NetbatError::MalformedStreamFrame {
            reason: "unknown stream verb"
        }
    );
}

#[test]
fn red_wrong_field_count_and_invalid_subscription_id() {
    let too_few = nb::decode_stream_line(b"NETBAT/2 SUBSCRIBE orders.open.v1 -\n", &limits())
        .expect_err("missing client window");
    assert_eq!(
        too_few,
        nb::NetbatError::MalformedStreamFrame {
            reason: "missing client window"
        }
    );

    let invalid_id = nb::decode_stream_line(b"NETBAT/2 SUBSCRIBE orders.open - 1\n", &limits())
        .expect_err("invalid subscription id");
    assert_eq!(
        invalid_id,
        nb::NetbatError::MalformedStreamFrame {
            reason: "subscription id must contain a .v version suffix"
        }
    );
}

#[test]
fn red_zero_delivery_index_and_client_window() {
    let zero_window = nb::decode_stream_line(b"NETBAT/2 SUBSCRIBE orders.open.v1 - 0\n", &limits())
        .expect_err("zero client window");
    assert_eq!(
        zero_window,
        nb::NetbatError::MalformedStreamFrame {
            reason: "client window must be nonzero"
        }
    );

    let zero_index = nb::decode_stream_line(
        b"NETBAT/2 SUB_EVENT orders.open.v1 0 - - hostbat.event.orders.v1 00\n",
        &limits(),
    )
    .expect_err("zero delivery index");
    assert_eq!(
        zero_index,
        nb::NetbatError::MalformedStreamFrame {
            reason: "delivery index must be nonzero"
        }
    );
}

#[test]
fn red_cursor_payload_and_message_limits() {
    let cursor_limits = limits().with_max_cursor_bytes(1);
    let cursor =
        nb::decode_stream_line(b"NETBAT/2 SUB_ACK orders.open.v1 1 aabb\n", &cursor_limits)
            .expect_err("cursor too large");
    assert_eq!(cursor, nb::NetbatError::CursorTooLarge { max: 1 });

    let payload_limits = limits().with_max_stream_payload_bytes(1);
    let payload = nb::decode_stream_line(
        b"NETBAT/2 SUB_EVENT orders.open.v1 1 - - hostbat.event.orders.v1 aabb\n",
        &payload_limits,
    )
    .expect_err("payload too large");
    assert_eq!(payload, nb::NetbatError::StreamPayloadTooLarge { max: 1 });

    let message_limits = limits().with_max_stream_error_message_bytes(1);
    let message = nb::decode_stream_line(
        b"NETBAT/2 SUB_ERR orders.open.v1 slow_consumer - - aabb\n",
        &message_limits,
    )
    .expect_err("message too large");
    assert_eq!(message, nb::NetbatError::StreamMessageTooLarge { max: 1 });
}

#[test]
fn red_invalid_hex_and_reason_code_and_line_too_long() {
    let bad_hex = nb::decode_stream_line(
        b"NETBAT/2 SUB_EVENT orders.open.v1 1 - - hostbat.event.orders.v1 nope\n",
        &limits(),
    )
    .expect_err("invalid payload hex");
    assert_eq!(
        bad_hex,
        nb::NetbatError::MalformedStreamFrame {
            reason: "input is not hex"
        }
    );

    let bad_reason = nb::decode_stream_line(
        b"NETBAT/2 SUB_CANCEL orders.open.v1 bad reason\n",
        &limits(),
    )
    .expect_err("malformed reason");
    assert_eq!(
        bad_reason,
        nb::NetbatError::MalformedStreamFrame {
            reason: "too many fields"
        }
    );

    let bad_code = nb::decode_stream_line(
        b"NETBAT/2 SUB_CANCEL orders.open.v1 Bad.Reason\n",
        &limits(),
    )
    .expect_err("invalid reason code grammar");
    assert_eq!(
        bad_code,
        nb::NetbatError::MalformedStreamFrame {
            reason: "reason code has characters outside [a-z0-9._-]"
        }
    );

    let limits = limits().with_max_line_bytes(8);
    let long = nb::decode_stream_line(&fixture_bytes(STREAM_SUBSCRIBE_HEX), &limits)
        .expect_err("line too long");
    assert_eq!(long, nb::NetbatError::LineTooLong { max: 8 });
}

#[test]
fn red_subscription_id_too_long_uses_typed_error() {
    let limits = limits().with_max_subscription_id_bytes(8);
    let err = nb::decode_stream_line(b"NETBAT/2 SUBSCRIBE orders.open.v1 - 1\n", &limits)
        .expect_err("subscription id too long");
    assert_eq!(err, nb::NetbatError::SubscriptionIdTooLong { max: 8 });
}

proptest! {
    #[test]
    fn decode_stream_line_is_total_on_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = nb::decode_stream_line(&bytes, &limits());
    }

    #[test]
    fn decode_stream_line_is_total_on_netbat2_prefix_plus_garbage(
        suffix in proptest::collection::vec(any::<u8>(), 0..256),
    ) {
        let mut line = b"NETBAT/2 ".to_vec();
        line.extend_from_slice(&suffix);
        let _ = nb::decode_stream_line(&line, &limits());
    }

    #[test]
    fn stream_frame_encoder_is_deterministic(frame in arb_stream_frame()) {
        let first = nb::encode_stream_frame(&frame);
        let second = nb::encode_stream_frame(&frame);
        prop_assert_eq!(first, second);
    }

    #[test]
    fn stream_frame_roundtrip(frame in arb_stream_frame()) {
        let encoded = nb::encode_stream_frame(&frame);
        let decoded = nb::decode_stream_line(&encoded, &limits()).expect("roundtrip decodes");
        prop_assert_eq!(decoded, frame);
    }
}

fn arb_stream_frame() -> impl Strategy<Value = nb::StreamFrame> {
    let subscription = "orders.open.v1";
    let schema = "hostbat.event.orders.v1";
    let cursor = prop_oneof![
        Just(nb::MaybeCursor::Absent),
        any::<u8>().prop_map(|byte| nb::MaybeCursor::Present(nb::CursorBytes::new([byte]))),
    ];
    let payload = prop::collection::vec(any::<u8>(), 0..8);
    let message = prop::collection::vec(any::<u8>(), 0..8);
    prop_oneof![
        (1u32..=32u32).prop_map(move |window| {
            nb::StreamFrame::Subscribe(nb::SubscribeFrame {
                subscription_id: nb::SubscriptionToken::new(subscription, &limits())
                    .expect("subscription id"),
                resume_cursor: nb::MaybeCursor::Absent,
                client_window: nb::ClientWindow::new(window).expect("client window"),
            })
        }),
        (1u64..=16u64, cursor.clone(), cursor.clone(), payload).prop_map(
            move |(index, before, after, payload)| {
                nb::StreamFrame::SubEvent(nb::SubEventFrame {
                    subscription_id: nb::SubscriptionToken::new(subscription, &limits())
                        .expect("subscription id"),
                    delivery_index: nb::DeliveryIndex::new(index).expect("delivery index"),
                    cursor_before: before,
                    cursor_after: after,
                    payload_schema_ref: nb::PayloadSchemaRef::new(schema).expect("schema ref"),
                    payload,
                })
            },
        ),
        (1u64..=16u64, any::<u8>()).prop_map(move |(index, byte)| {
            nb::StreamFrame::SubWatermark(nb::SubWatermarkFrame {
                subscription_id: nb::SubscriptionToken::new(subscription, &limits())
                    .expect("subscription id"),
                delivery_index: nb::DeliveryIndex::new(index).expect("delivery index"),
                cursor_after: nb::CursorBytes::new([byte]),
            })
        }),
        (1u64..=16u64, any::<u8>()).prop_map(move |(index, byte)| {
            nb::StreamFrame::SubAck(nb::SubAckFrame {
                subscription_id: nb::SubscriptionToken::new(subscription, &limits())
                    .expect("subscription id"),
                delivery_index: nb::DeliveryIndex::new(index).expect("delivery index"),
                cursor_after: nb::CursorBytes::new([byte]),
            })
        }),
        Just(nb::StreamFrame::SubCancel(nb::SubCancelFrame {
            subscription_id: nb::SubscriptionToken::new(subscription, &limits())
                .expect("subscription id"),
            reason_code: nb::StreamReasonCode::new("client.cancel").expect("reason"),
        })),
        (cursor.clone(), cursor.clone(), message).prop_map(move |(delivered, acked, message)| {
            nb::StreamFrame::SubErr(nb::SubErrFrame {
                subscription_id: nb::SubscriptionToken::new(subscription, &limits())
                    .expect("subscription id"),
                code: nb::StreamReasonCode::new("slow_consumer").expect("code"),
                last_delivered_cursor: delivered,
                last_acked_cursor: acked,
                message,
            })
        }),
        (
            cursor.clone(),
            Just(nb::StreamReasonCode::new("stream.complete").expect("reason"))
        )
            .prop_map(move |(cursor, reason)| {
                nb::StreamFrame::SubEnd(nb::SubEndFrame {
                    subscription_id: nb::SubscriptionToken::new(subscription, &limits())
                        .expect("subscription id"),
                    cursor_after: cursor,
                    reason_code: reason,
                })
            },),
    ]
}
