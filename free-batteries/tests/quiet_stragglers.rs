//! Tests for every quiet straggler — pub functions that compile and pass clippy
//! but have zero test coverage. Organized by module.
//! [SPEC:tests/quiet_stragglers.rs]
//!
//! These are the functions that "won't make noise but won't say when they're
//! hurt either." Each test asserts real behavior with investigation pointers.

use free_batteries::event::Reactive;
use free_batteries::id::EntityIdType;
use free_batteries::prelude::*;

// ================================================================
// src/id/mod.rs — EntityIdType + EventId + define_entity_id! macro
// ================================================================

#[test]
fn event_id_now_v7_is_nonzero() {
    let id = free_batteries::id::EventId::now_v7();
    assert_ne!(
        id.as_u128(),
        0,
        "PROPERTY: EventId::now_v7() must generate a non-zero UUIDv7.\n\
         Investigate: src/id/mod.rs generate_v7_id().\n\
         Common causes: UUID library returning nil on clock skew, feature flag disabled, \
         or SystemTime before Unix epoch on the test host.\n\
         Run: cargo test --test quiet_stragglers event_id_now_v7_is_nonzero"
    );
}

#[test]
fn event_id_nil_is_zero() {
    let id = free_batteries::id::EventId::nil();
    assert_eq!(
        id.as_u128(),
        0,
        "PROPERTY: EventId::nil() must return the zero UUID.\n\
         Investigate: src/id/mod.rs nil().\n\
         Common causes: nil() forwarding to now_v7() by mistake, or inner type default \
         not being zero-initialized.\n\
         Run: cargo test --test quiet_stragglers event_id_nil_is_zero"
    );
}

#[test]
fn event_id_round_trip() {
    let raw: u128 = 0xDEAD_BEEF_CAFE_BABE_1234_5678_9ABC_DEF0;
    let id = free_batteries::id::EventId::new(raw);
    assert_eq!(
        id.as_u128(),
        raw,
        "PROPERTY: EventId::new(raw).as_u128() must equal raw (lossless round-trip).\n\
         Investigate: src/id/mod.rs new() as_u128().\n\
         Common causes: byte-order swap in new() or as_u128(), truncation of high bits, \
         or wrapping newtype that strips the value.\n\
         Run: cargo test --test quiet_stragglers event_id_round_trip"
    );
}

#[test]
fn event_id_display_format() {
    let id = free_batteries::id::EventId::new(0xFF);
    let s = format!("{id}");
    assert!(
        s.starts_with("event:"),
        "PROPERTY: EventId Display must start with the entity prefix 'event:'.\n\
         Investigate: src/id/mod.rs define_entity_id! Display impl.\n\
         Common causes: macro emitting the wrong prefix string literal, or Display \
         delegating to as_u128() without prepending the prefix.\n\
         Run: cargo test --test quiet_stragglers event_id_display_format"
    );
    assert!(
        s.contains("ff"),
        "PROPERTY: EventId Display must contain the hex digits of the underlying u128.\n\
         Investigate: src/id/mod.rs define_entity_id! Display impl.\n\
         Common causes: Display printing decimal instead of hex, or padding zeroing \
         out the only non-zero byte before formatting.\n\
         Run: cargo test --test quiet_stragglers event_id_display_format"
    );
}

#[test]
fn event_id_from_str_with_prefix() {
    use std::str::FromStr;
    let id = free_batteries::id::EventId::from_str("event:00000000000000000000000000000042")
        .expect("parse with prefix");
    assert_eq!(
        id.as_u128(),
        0x42,
        "PROPERTY: EventId::from_str with 'event:' prefix must parse the hex portion correctly.\n\
         Investigate: src/id/mod.rs define_entity_id! FromStr impl.\n\
         Common causes: prefix stripping off-by-one consuming a hex digit, or \
         from_str not recognising the 'entity:' prefix at all.\n\
         Run: cargo test --test quiet_stragglers event_id_from_str_with_prefix"
    );
}

#[test]
fn event_id_from_str_bare_hex() {
    use std::str::FromStr;
    let id = free_batteries::id::EventId::from_str("00000000000000000000000000000042")
        .expect("parse bare hex");
    assert_eq!(
        id.as_u128(),
        0x42,
        "PROPERTY: EventId::from_str must parse bare hex (no prefix) correctly.\n\
         Investigate: src/id/mod.rs define_entity_id! FromStr impl.\n\
         Common causes: parser requiring the 'entity:' prefix and returning Err on bare \
         hex, or u128::from_str_radix receiving the wrong slice.\n\
         Run: cargo test --test quiet_stragglers event_id_from_str_bare_hex"
    );
}

#[test]
fn event_id_from_str_rejects_garbage() {
    use std::str::FromStr;
    let result = free_batteries::id::EventId::from_str("not_hex_at_all");
    assert!(
        result.is_err(),
        "PROPERTY: EventId::FromStr must reject non-hex input with Err.\n\
         Investigate: src/id/mod.rs define_entity_id! FromStr impl.\n\
         Common causes: parser returning Ok(0) on parse failure instead of Err, \
         or unwrap_or silently swallowing the error.\n\
         Run: cargo test --test quiet_stragglers event_id_from_str_rejects_garbage"
    );
}

#[test]
fn define_entity_id_custom_type() {
    free_batteries::define_entity_id!(OrderId, "order");
    use free_batteries::id::EntityIdType;

    let id = OrderId::now_v7();
    assert_ne!(
        id.as_u128(),
        0,
        "PROPERTY: define_entity_id! macro must generate a non-zero ID via now_v7().\n\
         Investigate: src/id/mod.rs define_entity_id! macro expansion.\n\
         Common causes: macro forwarding to a stub that returns nil, or UUID clock \
         returning zero on test host.\n\
         Run: cargo test --test quiet_stragglers define_entity_id_custom_type"
    );
    assert_eq!(
        OrderId::ENTITY_NAME,
        "order",
        "PROPERTY: define_entity_id! macro must set ENTITY_NAME to the supplied string.\n\
         Investigate: src/id/mod.rs define_entity_id! macro ENTITY_NAME const.\n\
         Common causes: macro hardcoding a different literal, or const not being \
         set from the macro argument.\n\
         Run: cargo test --test quiet_stragglers define_entity_id_custom_type"
    );

    let display = format!("{id}");
    assert!(
        display.starts_with("order:"),
        "PROPERTY: Custom entity ID Display must use the registered entity name as prefix.\n\
         Investigate: src/id/mod.rs define_entity_id! Display impl.\n\
         Common causes: Display impl using a hardcoded prefix instead of ENTITY_NAME, \
         or macro not emitting a Display impl at all.\n\
         Run: cargo test --test quiet_stragglers define_entity_id_custom_type"
    );
}

// ================================================================
// src/pipeline/ — Bypass system + Proposal::map
// ================================================================

struct TestBypassReason;
impl free_batteries::pipeline::BypassReason for TestBypassReason {
    fn name(&self) -> &'static str {
        "test_bypass"
    }
    fn justification(&self) -> &'static str {
        "testing bypass audit trail"
    }
}

static TEST_BYPASS: TestBypassReason = TestBypassReason;

#[test]
fn pipeline_bypass_returns_bypass_receipt() {
    let proposal = Proposal::new(42);
    let receipt = free_batteries::pipeline::Pipeline::<()>::bypass(proposal, &TEST_BYPASS);

    assert_eq!(
        receipt.payload, 42,
        "PROPERTY: BypassReceipt must carry the original proposal payload unchanged.\n\
         Investigate: src/pipeline/mod.rs Pipeline::bypass().\n\
         Common causes: bypass() discarding the proposal value, or BypassReceipt \
         storing the wrong field.\n\
         Run: cargo test --test quiet_stragglers pipeline_bypass_returns_bypass_receipt"
    );
    assert_eq!(
        receipt.reason, "test_bypass",
        "PROPERTY: BypassReceipt must record the BypassReason::name() as reason.\n\
         Investigate: src/pipeline/mod.rs Pipeline::bypass() BypassReason::name().\n\
         Common causes: bypass() storing justification() in reason field, or \
         name() not being called at all.\n\
         Run: cargo test --test quiet_stragglers pipeline_bypass_returns_bypass_receipt"
    );
    assert_eq!(
        receipt.justification, "testing bypass audit trail",
        "PROPERTY: BypassReceipt must record BypassReason::justification() verbatim.\n\
         Investigate: src/pipeline/mod.rs Pipeline::bypass() BypassReason::justification().\n\
         Common causes: bypass() storing name() in justification field, or \
         justification() returning a hardcoded string instead of the impl's value.\n\
         Run: cargo test --test quiet_stragglers pipeline_bypass_returns_bypass_receipt"
    );
}

#[test]
fn proposal_map_transforms_payload() {
    let proposal = Proposal::new(21);
    let doubled = proposal.map(|x| x * 2);
    assert_eq!(
        *doubled.payload(),
        42,
        "PROPERTY: Proposal::map must transform the payload using the provided closure.\n\
         Investigate: src/pipeline/mod.rs Proposal::map().\n\
         Common causes: map() cloning the old payload instead of applying the closure, \
         or the closure not being called at all.\n\
         Run: cargo test --test quiet_stragglers proposal_map_transforms_payload"
    );
}

#[test]
fn committed_serde_round_trip() {
    let committed = free_batteries::pipeline::Committed {
        payload: "test".to_string(),
        event_id: 0xDEAD_BEEF_CAFE_BABE_1234_5678_9ABC_DEF0,
        sequence: 42,
        hash: [0xAA; 32],
    };

    // Serialize to msgpack then back — exercises u128_bytes wire format
    let bytes = rmp_serde::to_vec_named(&committed).expect("serialize Committed");
    let decoded: free_batteries::pipeline::Committed<String> =
        rmp_serde::from_slice(&bytes).expect("deserialize Committed");

    assert_eq!(
        decoded.payload, "test",
        "PROPERTY: Committed payload must survive msgpack serialization round-trip unchanged.\n\
         Investigate: src/pipeline/mod.rs Committed Serialize/Deserialize impls.\n\
         Common causes: payload field not tagged with serde attribute, or msgpack \
         encoding changing string encoding between versions.\n\
         Run: cargo test --test quiet_stragglers committed_serde_round_trip"
    );
    assert_eq!(
        decoded.event_id, 0xDEAD_BEEF_CAFE_BABE_1234_5678_9ABC_DEF0,
        "PROPERTY: Committed event_id must round-trip through u128_bytes wire format without loss.\n\
         Investigate: src/pipeline/mod.rs src/wire.rs u128_bytes serde helper.\n\
         Common causes: u128_bytes encoding as little-endian but decoding as big-endian, \
         or serde_as attribute missing on the event_id field.\n\
         Run: cargo test --test quiet_stragglers committed_serde_round_trip"
    );
    assert_eq!(
        decoded.sequence, 42,
        "PROPERTY: Committed sequence must survive msgpack serialization round-trip unchanged.\n\
         Investigate: src/pipeline/mod.rs Committed Serialize/Deserialize impls.\n\
         Common causes: sequence field not included in serialization, or deserialized \
         into wrong numeric type causing truncation.\n\
         Run: cargo test --test quiet_stragglers committed_serde_round_trip"
    );
    assert_eq!(
        decoded.hash, [0xAA; 32],
        "PROPERTY: Committed hash must survive msgpack serialization round-trip unchanged.\n\
         Investigate: src/pipeline/mod.rs Committed Serialize/Deserialize impls.\n\
         Common causes: hash field serialized as a sequence vs bytes causing length mismatch, \
         or serde_bytes attribute missing from the hash field.\n\
         Run: cargo test --test quiet_stragglers committed_serde_round_trip"
    );
}

// ================================================================
// src/event/header.rs — Flag system
// ================================================================

#[test]
fn event_header_flags_requires_ack() {
    let header =
        EventHeader::new(1, 1, None, 0, DagPosition::root(), 0, EventKind::DATA).with_flags(0x01);
    assert!(
        header.requires_ack(),
        "PROPERTY: Flag bit 0x01 must set requires_ack() to true.\n\
         Investigate: src/event/header.rs requires_ack() flag mask.\n\
         Common causes: requires_ack() testing bit 0x02 instead of 0x01, or \
         with_flags() not storing the value in the flags field.\n\
         Run: cargo test --test quiet_stragglers event_header_flags_requires_ack"
    );
    assert!(
        !header.is_transactional(),
        "PROPERTY: Flag bit 0x01 must NOT set is_transactional().\n\
         Investigate: src/event/header.rs is_transactional() flag mask.\n\
         Common causes: is_transactional() using the same bit mask as requires_ack().\n\
         Run: cargo test --test quiet_stragglers event_header_flags_requires_ack"
    );
    assert!(
        !header.is_replay(),
        "PROPERTY: Flag bit 0x01 must NOT set is_replay().\n\
         Investigate: src/event/header.rs is_replay() flag mask.\n\
         Common causes: is_replay() using bit 0x01 instead of 0x08.\n\
         Run: cargo test --test quiet_stragglers event_header_flags_requires_ack"
    );
}

#[test]
fn event_header_flags_transactional() {
    let header =
        EventHeader::new(1, 1, None, 0, DagPosition::root(), 0, EventKind::DATA).with_flags(0x02);
    assert!(
        header.is_transactional(),
        "PROPERTY: Flag bit 0x02 must set is_transactional() to true.\n\
         Investigate: src/event/header.rs is_transactional() flag mask.\n\
         Common causes: is_transactional() testing bit 0x01 instead of 0x02, or \
         flag bits defined in the wrong order.\n\
         Run: cargo test --test quiet_stragglers event_header_flags_transactional"
    );
    assert!(
        !header.requires_ack(),
        "PROPERTY: Flag bit 0x02 must NOT set requires_ack().\n\
         Investigate: src/event/header.rs requires_ack() flag mask.\n\
         Common causes: requires_ack() testing bit 0x02 instead of 0x01.\n\
         Run: cargo test --test quiet_stragglers event_header_flags_transactional"
    );
}

#[test]
fn event_header_flags_replay() {
    let header =
        EventHeader::new(1, 1, None, 0, DagPosition::root(), 0, EventKind::DATA).with_flags(0x08);
    assert!(
        header.is_replay(),
        "PROPERTY: Flag bit 0x08 must set is_replay() to true.\n\
         Investigate: src/event/header.rs is_replay() flag mask.\n\
         Common causes: is_replay() testing bit 0x04 instead of 0x08, or \
         flag constant defined incorrectly.\n\
         Run: cargo test --test quiet_stragglers event_header_flags_replay"
    );
    assert!(
        !header.requires_ack(),
        "PROPERTY: Flag bit 0x08 must NOT set requires_ack().\n\
         Investigate: src/event/header.rs requires_ack() flag mask.\n\
         Common causes: requires_ack() mask accidentally overlapping with replay bit.\n\
         Run: cargo test --test quiet_stragglers event_header_flags_replay"
    );
    assert!(
        !header.is_transactional(),
        "PROPERTY: Flag bit 0x08 must NOT set is_transactional().\n\
         Investigate: src/event/header.rs is_transactional() flag mask.\n\
         Common causes: is_transactional() mask accidentally overlapping with replay bit.\n\
         Run: cargo test --test quiet_stragglers event_header_flags_replay"
    );
}

#[test]
fn event_header_flags_zero_all_false() {
    let header = EventHeader::new(1, 1, None, 0, DagPosition::root(), 0, EventKind::DATA);
    assert!(
        !header.requires_ack(),
        "PROPERTY: Zero flags must not set requires_ack().\n\
         Investigate: src/event/header.rs requires_ack() flags field initialization.\n\
         Common causes: EventHeader::new() defaulting flags to a non-zero value, or \
         requires_ack() not masking correctly against 0x01.\n\
         Run: cargo test --test quiet_stragglers event_header_flags_zero_all_false"
    );
    assert!(
        !header.is_transactional(),
        "PROPERTY: Zero flags must not set is_transactional().\n\
         Investigate: src/event/header.rs is_transactional() flags field initialization.\n\
         Common causes: EventHeader::new() defaulting flags to a non-zero value, or \
         is_transactional() not masking correctly against 0x02.\n\
         Run: cargo test --test quiet_stragglers event_header_flags_zero_all_false"
    );
    assert!(
        !header.is_replay(),
        "PROPERTY: Zero flags must not set is_replay().\n\
         Investigate: src/event/header.rs is_replay() flags field initialization.\n\
         Common causes: EventHeader::new() defaulting flags to a non-zero value, or \
         is_replay() not masking correctly against 0x08.\n\
         Run: cargo test --test quiet_stragglers event_header_flags_zero_all_false"
    );
}

#[test]
fn event_header_age_us() {
    let header = EventHeader::new(
        1,
        1,
        None,
        1_000_000,
        DagPosition::root(),
        0,
        EventKind::DATA,
    );
    let age = header.age_us(2_000_000);
    assert_eq!(
        age, 1_000_000,
        "PROPERTY: age_us(now) must return (now - timestamp_us) as the age in microseconds.\n\
         Investigate: src/event/header.rs EventHeader::age_us().\n\
         Common causes: age_us() returning absolute timestamp instead of delta, \
         or subtraction performed in wrong order (timestamp - now).\n\
         Run: cargo test --test quiet_stragglers event_header_age_us"
    );
}

// ================================================================
// src/event/kind.rs — Classification + constants
// ================================================================

#[test]
fn event_kind_system_constants_are_system() {
    let system_kinds = [
        EventKind::DATA,
        EventKind::SYSTEM_INIT,
        EventKind::SYSTEM_SHUTDOWN,
        EventKind::SYSTEM_HEARTBEAT,
        EventKind::SYSTEM_CONFIG_CHANGE,
        EventKind::SYSTEM_CHECKPOINT,
    ];
    for kind in system_kinds {
        assert!(
            kind.is_system(),
            "PROPERTY: EventKind {:?} must return true for is_system().\n\
             Investigate: src/event/kind.rs EventKind::is_system() category check.\n\
             Common causes: system category constant changed, or is_system() checking \
             the wrong nibble in the packed u16 value.\n\
             Run: cargo test --test quiet_stragglers event_kind_system_constants_are_system",
            kind
        );
        assert!(
            !kind.is_effect(),
            "PROPERTY: System EventKind {:?} must NOT return true for is_effect().\n\
             Investigate: src/event/kind.rs EventKind::is_effect() category check.\n\
             Common causes: is_effect() using the same category mask as is_system().\n\
             Run: cargo test --test quiet_stragglers event_kind_system_constants_are_system",
            kind
        );
    }
}

#[test]
fn event_kind_effect_constants_are_effect() {
    let effect_kinds = [
        EventKind::EFFECT_ERROR,
        EventKind::EFFECT_RETRY,
        EventKind::EFFECT_ACK,
        EventKind::EFFECT_BACKPRESSURE,
        EventKind::EFFECT_CANCEL,
        EventKind::EFFECT_CONFLICT,
    ];
    for kind in effect_kinds {
        assert!(
            kind.is_effect(),
            "PROPERTY: EventKind {:?} must return true for is_effect().\n\
             Investigate: src/event/kind.rs EventKind::is_effect() category check.\n\
             Common causes: effect category constant changed, or is_effect() checking \
             the wrong nibble in the packed u16 value.\n\
             Run: cargo test --test quiet_stragglers event_kind_effect_constants_are_effect",
            kind
        );
        assert!(
            !kind.is_system(),
            "PROPERTY: Effect EventKind {:?} must NOT return true for is_system().\n\
             Investigate: src/event/kind.rs EventKind::is_system() category check.\n\
             Common causes: is_system() using the same category mask as is_effect().\n\
             Run: cargo test --test quiet_stragglers event_kind_effect_constants_are_effect",
            kind
        );
    }
}

#[test]
fn event_kind_custom_is_neither_system_nor_effect() {
    let custom = EventKind::custom(0x5, 42);
    assert!(
        !custom.is_system(),
        "PROPERTY: Custom EventKind must NOT be classified as a system event.\n\
         Investigate: src/event/kind.rs EventKind::is_system() category range check.\n\
         Common causes: is_system() treating category 0x5 as reserved system space, \
         or category boundaries defined incorrectly.\n\
         Run: cargo test --test quiet_stragglers event_kind_custom_is_neither_system_nor_effect"
    );
    assert!(
        !custom.is_effect(),
        "PROPERTY: Custom EventKind must NOT be classified as an effect event.\n\
         Investigate: src/event/kind.rs EventKind::is_effect() category range check.\n\
         Common causes: is_effect() treating category 0x5 as reserved effect space, \
         or category boundaries defined incorrectly.\n\
         Run: cargo test --test quiet_stragglers event_kind_custom_is_neither_system_nor_effect"
    );
    assert_eq!(
        custom.category(),
        0x5,
        "PROPERTY: EventKind::custom(0x5, 42) must store and return category 0x5.\n\
         Investigate: src/event/kind.rs EventKind::custom() category() packing.\n\
         Common causes: category packed into wrong nibble position, or category() \
         extracting the wrong bits from the u16.\n\
         Run: cargo test --test quiet_stragglers event_kind_custom_is_neither_system_nor_effect"
    );
    assert_eq!(
        custom.type_id(),
        42,
        "PROPERTY: EventKind::custom(0x5, 42) must store and return type_id 42.\n\
         Investigate: src/event/kind.rs EventKind::custom() type_id() packing.\n\
         Common causes: type_id packed into wrong byte position, or type_id() \
         extracting bits that include the category nibble.\n\
         Run: cargo test --test quiet_stragglers event_kind_custom_is_neither_system_nor_effect"
    );
}

#[test]
fn event_kind_display_hex() {
    let kind = EventKind::custom(0xA, 0xBC);
    let s = format!("{kind}");
    assert_eq!(
        s, "0xA0BC",
        "PROPERTY: EventKind Display must format as '0x{{category_nibble}}{{type_byte:02X}}' (4 hex digits).\n\
         Investigate: src/event/kind.rs EventKind Display impl.\n\
         Common causes: Display using lowercase instead of uppercase hex, missing '0x' prefix, \
         or printing the raw u16 without the structured nibble/byte layout.\n\
         Run: cargo test --test quiet_stragglers event_kind_display_hex"
    );
}

// ================================================================
// src/event/mod.rs — Event convenience methods
// ================================================================

#[test]
fn event_with_hash_chain_sets_field() {
    let header = EventHeader::new(1, 1, None, 0, DagPosition::root(), 0, EventKind::DATA);
    let chain = HashChain {
        prev_hash: [0u8; 32],
        event_hash: [1u8; 32],
    };
    let event = Event::new(header, "payload").with_hash_chain(chain.clone());

    assert_eq!(
        event.hash_chain,
        Some(chain),
        "PROPERTY: Event::with_hash_chain must store the provided HashChain in the hash_chain field.\n\
         Investigate: src/event/mod.rs Event::with_hash_chain().\n\
         Common causes: with_hash_chain() returning a clone of the original event without \
         updating the field, or hash_chain field shadowed by a local variable.\n\
         Run: cargo test --test quiet_stragglers event_with_hash_chain_sets_field"
    );
}

#[test]
fn event_is_genesis_true_when_prev_hash_zero() {
    let header = EventHeader::new(1, 1, None, 0, DagPosition::root(), 0, EventKind::DATA);
    let event = Event::new(header, ()).with_hash_chain(HashChain {
        prev_hash: [0u8; 32],
        event_hash: [1u8; 32],
    });
    assert!(
        event.is_genesis(),
        "PROPERTY: Event with all-zero prev_hash must be identified as a genesis event.\n\
         Investigate: src/event/mod.rs Event::is_genesis().\n\
         Common causes: is_genesis() checking event_hash instead of prev_hash, or \
         comparing against [0xFF; 32] instead of [0u8; 32].\n\
         Run: cargo test --test quiet_stragglers event_is_genesis_true_when_prev_hash_zero"
    );
}

#[test]
fn event_is_genesis_false_when_prev_hash_nonzero() {
    let header = EventHeader::new(1, 1, None, 0, DagPosition::root(), 0, EventKind::DATA);
    let event = Event::new(header, ()).with_hash_chain(HashChain {
        prev_hash: [0xFF; 32],
        event_hash: [1u8; 32],
    });
    assert!(
        !event.is_genesis(),
        "PROPERTY: Event with non-zero prev_hash must NOT be identified as a genesis event.\n\
         Investigate: src/event/mod.rs Event::is_genesis().\n\
         Common causes: is_genesis() always returning true, or using wrong zero-comparison \
         that ignores non-zero bytes in prev_hash.\n\
         Run: cargo test --test quiet_stragglers event_is_genesis_false_when_prev_hash_nonzero"
    );
}

#[test]
fn event_is_genesis_true_when_no_hash_chain() {
    let header = EventHeader::new(1, 1, None, 0, DagPosition::root(), 0, EventKind::DATA);
    let event = Event::new(header, ());
    assert!(
        event.is_genesis(),
        "PROPERTY: Event with no hash_chain (None) must be treated as a genesis event.\n\
         Investigate: src/event/mod.rs Event::is_genesis().\n\
         Common causes: is_genesis() panicking or returning false on None hash_chain, \
         or not handling the Option::None case.\n\
         Run: cargo test --test quiet_stragglers event_is_genesis_true_when_no_hash_chain"
    );
}

#[test]
fn event_map_payload_transforms_preserving_header() {
    let header = EventHeader::new(42, 42, None, 100, DagPosition::child(5), 0, EventKind::DATA);
    let event = Event::new(header, 21);
    let mapped = event.map_payload(|x| x * 2);
    assert_eq!(
        mapped.payload, 42,
        "PROPERTY: Event::map_payload must apply the closure to transform the payload.\n\
         Investigate: src/event/mod.rs Event::map_payload().\n\
         Common causes: map_payload() ignoring the closure and cloning the original \
         payload, or building a new Event with the old payload.\n\
         Run: cargo test --test quiet_stragglers event_map_payload_transforms_preserving_header"
    );
    assert_eq!(
        mapped.header.event_id, 42,
        "PROPERTY: Event::map_payload must preserve the original header unchanged.\n\
         Investigate: src/event/mod.rs Event::map_payload().\n\
         Common causes: map_payload() creating a new header instead of moving the original, \
         or resetting header fields like event_id to defaults.\n\
         Run: cargo test --test quiet_stragglers event_map_payload_transforms_preserving_header"
    );
    assert_eq!(
        mapped.header.timestamp_us, 100,
        "PROPERTY: Event::map_payload must preserve header.timestamp_us unchanged.\n\
         Investigate: src/event/mod.rs Event::map_payload().\n\
         Common causes: map_payload() rebuilding a fresh header that zero-initializes \
         timestamp_us instead of carrying over the original header.\n\
         Run: cargo test --test quiet_stragglers event_map_payload_transforms_preserving_header"
    );
}

#[test]
fn event_position_returns_header_position() {
    let pos = DagPosition::child(7);
    let header = EventHeader::new(1, 1, None, 0, pos, 0, EventKind::DATA);
    let event = Event::new(header, ());
    assert_eq!(
        *event.position(),
        pos,
        "PROPERTY: Event::position() must return the DagPosition from the header.\n\
         Investigate: src/event/mod.rs Event::position().\n\
         Common causes: position() returning a default DagPosition instead of \
         delegating to header.position, or dereferencing the wrong field.\n\
         Run: cargo test --test quiet_stragglers event_position_returns_header_position"
    );
}

// ================================================================
// src/coordinate/position.rs — DagPosition
// ================================================================

#[test]
fn dag_position_root() {
    let pos = DagPosition::root();
    assert_eq!(
        pos.depth, 0,
        "PROPERTY: DagPosition::root() must set depth to 0.\n\
         Investigate: src/coordinate/position.rs DagPosition::root().\n\
         Common causes: root() copying a non-zero depth from a template, or \
         the struct initializer using wrong field ordering.\n\
         Run: cargo test --test quiet_stragglers dag_position_root"
    );
    assert_eq!(
        pos.lane, 0,
        "PROPERTY: DagPosition::root() must set lane to 0.\n\
         Investigate: src/coordinate/position.rs DagPosition::root().\n\
         Common causes: root() initializing lane to 1 instead of 0, or \
         field ordering swap between lane and depth.\n\
         Run: cargo test --test quiet_stragglers dag_position_root"
    );
    assert_eq!(
        pos.sequence, 0,
        "PROPERTY: DagPosition::root() must set sequence to 0.\n\
         Investigate: src/coordinate/position.rs DagPosition::root().\n\
         Common causes: root() not zero-initializing sequence, or sequence \
         defaulting to 1 for 'first event' semantics.\n\
         Run: cargo test --test quiet_stragglers dag_position_root"
    );
    assert!(
        pos.is_root(),
        "PROPERTY: DagPosition::root() must satisfy is_root().\n\
         Investigate: src/coordinate/position.rs DagPosition::is_root().\n\
         Common causes: is_root() checking only depth but not lane or sequence, \
         or root() not producing coordinates that satisfy is_root().\n\
         Run: cargo test --test quiet_stragglers dag_position_root"
    );
}

#[test]
fn dag_position_child() {
    let pos = DagPosition::child(42);
    assert_eq!(
        pos.sequence, 42,
        "PROPERTY: DagPosition::child(42) must set sequence to 42.\n\
         Investigate: src/coordinate/position.rs DagPosition::child().\n\
         Common causes: child() ignoring the sequence argument and hardcoding 0, \
         or storing sequence in the depth field by mistake.\n\
         Run: cargo test --test quiet_stragglers dag_position_child"
    );
    assert_eq!(
        pos.depth, 0,
        "PROPERTY: DagPosition::child() must set depth to 0 (same lane as root).\n\
         Investigate: src/coordinate/position.rs DagPosition::child().\n\
         Common causes: child() incrementing depth like fork(), or copying depth \
         from an earlier position.\n\
         Run: cargo test --test quiet_stragglers dag_position_child"
    );
    assert_eq!(
        pos.lane, 0,
        "PROPERTY: DagPosition::child() must set lane to 0 (main lane).\n\
         Investigate: src/coordinate/position.rs DagPosition::child().\n\
         Common causes: child() calling fork() internally, which would assign a new lane.\n\
         Run: cargo test --test quiet_stragglers dag_position_child"
    );
    assert!(
        !pos.is_root(),
        "PROPERTY: DagPosition::child(42) must NOT satisfy is_root() (non-zero sequence).\n\
         Investigate: src/coordinate/position.rs DagPosition::is_root().\n\
         Common causes: is_root() only checking depth==0 and lane==0 without checking \
         sequence==0.\n\
         Run: cargo test --test quiet_stragglers dag_position_child"
    );
}

#[test]
fn dag_position_fork() {
    let pos = DagPosition::fork(2, 3);
    assert_eq!(
        pos.depth, 3,
        "PROPERTY: DagPosition::fork(parent_depth=2, lane=3) must set depth to parent_depth+1=3.\n\
         Investigate: src/coordinate/position.rs DagPosition::fork().\n\
         Common causes: fork() storing parent_depth unchanged instead of incrementing, \
         or depth and lane arguments swapped in the constructor.\n\
         Run: cargo test --test quiet_stragglers dag_position_fork"
    );
    assert_eq!(
        pos.lane, 3,
        "PROPERTY: DagPosition::fork(2, 3) must set lane to 3.\n\
         Investigate: src/coordinate/position.rs DagPosition::fork().\n\
         Common causes: fork() using an auto-incrementing lane counter instead of \
         the provided lane argument.\n\
         Run: cargo test --test quiet_stragglers dag_position_fork"
    );
    assert_eq!(
        pos.sequence, 0,
        "PROPERTY: DagPosition::fork() must start at sequence 0 (beginning of the new branch).\n\
         Investigate: src/coordinate/position.rs DagPosition::fork().\n\
         Common causes: fork() copying sequence from the parent position instead of \
         resetting it to 0.\n\
         Run: cargo test --test quiet_stragglers dag_position_fork"
    );
    assert!(
        !pos.is_root(),
        "PROPERTY: DagPosition::fork() must NOT be is_root() (non-zero depth and lane).\n\
         Investigate: src/coordinate/position.rs DagPosition::is_root().\n\
         Common causes: is_root() returning true when sequence==0 regardless of depth/lane.\n\
         Run: cargo test --test quiet_stragglers dag_position_fork"
    );
}

#[test]
fn dag_position_is_ancestor_of_same_lane() {
    let a = DagPosition::child(2);
    let b = DagPosition::child(5);
    assert!(
        a.is_ancestor_of(&b),
        "PROPERTY: child(2) must be an ancestor of child(5) on the same lane (lower sequence).\n\
         Investigate: src/coordinate/position.rs DagPosition::is_ancestor_of().\n\
         Common causes: is_ancestor_of() using >= instead of < on sequence, or not \
         checking lane equality before comparing sequence.\n\
         Run: cargo test --test quiet_stragglers dag_position_is_ancestor_of_same_lane"
    );
    assert!(
        !b.is_ancestor_of(&a),
        "PROPERTY: child(5) must NOT be an ancestor of child(2) (higher sequence cannot be ancestor).\n\
         Investigate: src/coordinate/position.rs DagPosition::is_ancestor_of().\n\
         Common causes: is_ancestor_of() not checking direction, returning true for \
         any two positions on the same lane.\n\
         Run: cargo test --test quiet_stragglers dag_position_is_ancestor_of_same_lane"
    );
}

#[test]
fn dag_position_is_ancestor_of_different_lanes() {
    let a = DagPosition::new(0, 0, 2);
    let b = DagPosition::new(0, 1, 5); // different lane
    assert!(
        !a.is_ancestor_of(&b),
        "PROPERTY: Positions on different lanes must never be ancestors of each other.\n\
         Investigate: src/coordinate/position.rs DagPosition::is_ancestor_of().\n\
         Common causes: is_ancestor_of() only comparing sequence without checking \
         that lanes match, treating all lower-sequence positions as ancestors.\n\
         Run: cargo test --test quiet_stragglers dag_position_is_ancestor_of_different_lanes"
    );
}

#[test]
fn dag_position_partial_ord_same_lane() {
    let a = DagPosition::child(2);
    let b = DagPosition::child(5);
    assert!(
        a < b,
        "PROPERTY: child(2) must be less than child(5) on the same lane (lower sequence < higher).\n\
         Investigate: src/coordinate/position.rs DagPosition PartialOrd impl.\n\
         Common causes: PartialOrd returning None for same-lane comparisons, or comparing \
         depth/lane first without falling through to sequence.\n\
         Run: cargo test --test quiet_stragglers dag_position_partial_ord_same_lane"
    );
    assert!(
        b > a,
        "PROPERTY: child(5) must be greater than child(2) on the same lane.\n\
         Investigate: src/coordinate/position.rs DagPosition PartialOrd impl.\n\
         Common causes: PartialOrd impl not providing symmetry, or gt() delegating \
         to an incorrect comparison.\n\
         Run: cargo test --test quiet_stragglers dag_position_partial_ord_same_lane"
    );
    let c = DagPosition::child(2);
    assert!(
        a.partial_cmp(&c) == Some(std::cmp::Ordering::Equal),
        "PROPERTY: Two child(2) positions on the same lane must compare as Equal.\n\
         Investigate: src/coordinate/position.rs DagPosition PartialOrd impl.\n\
         Common causes: PartialOrd returning None for equal positions, or failing to \
         short-circuit when all fields are equal.\n\
         Run: cargo test --test quiet_stragglers dag_position_partial_ord_same_lane"
    );
}

#[test]
fn dag_position_partial_ord_different_lanes_incomparable() {
    let a = DagPosition::new(0, 0, 2);
    let b = DagPosition::new(0, 1, 5);
    assert_eq!(
        a.partial_cmp(&b),
        None,
        "PROPERTY: DagPositions on different lanes must be incomparable (partial_cmp returns None).\n\
         Investigate: src/coordinate/position.rs DagPosition PartialOrd impl.\n\
         Common causes: PartialOrd ignoring lane differences and comparing only by sequence, \
         or returning Some(Less) when lane of a < lane of b.\n\
         Run: cargo test --test quiet_stragglers dag_position_partial_ord_different_lanes_incomparable"
    );
}

#[test]
fn dag_position_display() {
    let pos = DagPosition::new(1, 2, 3);
    assert_eq!(
        format!("{pos}"),
        "1:2:3",
        "PROPERTY: DagPosition Display must format as 'depth:lane:sequence'.\n\
         Investigate: src/coordinate/position.rs DagPosition Display impl.\n\
         Common causes: Display outputting fields in wrong order (e.g. sequence:lane:depth), \
         using a different separator than ':', or printing only some fields.\n\
         Run: cargo test --test quiet_stragglers dag_position_display"
    );
}

// ================================================================
// DagPosition::fork() — scaffolding for multi-lane fan-out
// ================================================================

#[test]
fn dag_position_fork_creates_new_lane() {
    let forked = DagPosition::fork(0, 1);
    assert_eq!(
        forked,
        DagPosition::new(1, 1, 0),
        "PROPERTY: fork(parent_depth=0, new_lane=1) must produce depth=1, lane=1, sequence=0.\n\
         Investigate: src/coordinate/position.rs DagPosition::fork().\n\
         Run: cargo test --test quiet_stragglers dag_position_fork_creates_new_lane"
    );
}

#[test]
fn dag_position_forked_incomparable_with_lane_zero() {
    let main = DagPosition::child(5);
    let forked = DagPosition::fork(0, 1);
    assert!(
        main.partial_cmp(&forked).is_none(),
        "PROPERTY: Forked position (lane=1) must be incomparable with main lane (lane=0).\n\
         Investigate: src/coordinate/position.rs DagPosition PartialOrd impl.\n\
         Run: cargo test --test quiet_stragglers dag_position_forked_incomparable_with_lane_zero"
    );
}

#[test]
fn dag_position_fork_is_not_ancestor_across_lanes() {
    let main = DagPosition::child(0);
    let forked = DagPosition::fork(0, 1);
    assert!(
        !main.is_ancestor_of(&forked),
        "PROPERTY: is_ancestor_of must return false across different lanes.\n\
         Investigate: src/coordinate/position.rs DagPosition::is_ancestor_of().\n\
         Run: cargo test --test quiet_stragglers dag_position_fork_is_not_ancestor_across_lanes"
    );
}

// ================================================================
// src/outcome/error.rs — ErrorKind classification
// ================================================================

#[test]
fn error_kind_is_retryable() {
    assert!(
        ErrorKind::StorageError.is_retryable(),
        "PROPERTY: ErrorKind::StorageError must be classified as retryable.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_retryable().\n\
         Common causes: StorageError missing from the retryable match arm, or \
         is_retryable() returning false by default.\n\
         Run: cargo test --test quiet_stragglers error_kind_is_retryable"
    );
    assert!(
        ErrorKind::Timeout.is_retryable(),
        "PROPERTY: ErrorKind::Timeout must be classified as retryable.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_retryable().\n\
         Common causes: Timeout missing from the retryable match arm, or \
         Timeout placed in the non-retryable group by mistake.\n\
         Run: cargo test --test quiet_stragglers error_kind_is_retryable"
    );
    assert!(
        !ErrorKind::NotFound.is_retryable(),
        "PROPERTY: ErrorKind::NotFound must NOT be retryable (domain error, not transient).\n\
         Investigate: src/outcome/error.rs ErrorKind::is_retryable().\n\
         Common causes: NotFound grouped with operational errors in the retryable arm.\n\
         Run: cargo test --test quiet_stragglers error_kind_is_retryable"
    );
    assert!(
        !ErrorKind::Conflict.is_retryable(),
        "PROPERTY: ErrorKind::Conflict must NOT be retryable (requires resolution, not retry).\n\
         Investigate: src/outcome/error.rs ErrorKind::is_retryable().\n\
         Common causes: Conflict grouped with transient errors, or is_retryable() \
         treating all non-domain errors as retryable.\n\
         Run: cargo test --test quiet_stragglers error_kind_is_retryable"
    );
    assert!(
        !ErrorKind::Internal.is_retryable(),
        "PROPERTY: ErrorKind::Internal must NOT be retryable (programming error, not transient).\n\
         Investigate: src/outcome/error.rs ErrorKind::is_retryable().\n\
         Common causes: Internal grouped with operational transients by mistake.\n\
         Run: cargo test --test quiet_stragglers error_kind_is_retryable"
    );
    assert!(
        !ErrorKind::Custom(99).is_retryable(),
        "PROPERTY: ErrorKind::Custom must NOT be retryable by default.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_retryable().\n\
         Common causes: Custom variant handled by a wildcard arm that returns true, or \
         Custom not having an explicit non-retryable arm.\n\
         Run: cargo test --test quiet_stragglers error_kind_is_retryable"
    );
}

#[test]
fn error_kind_is_domain() {
    assert!(
        ErrorKind::NotFound.is_domain(),
        "PROPERTY: ErrorKind::NotFound must be classified as a domain error.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_domain().\n\
         Common causes: NotFound missing from the domain match arm, or mis-categorized \
         as an operational error.\n\
         Run: cargo test --test quiet_stragglers error_kind_is_domain"
    );
    assert!(
        ErrorKind::Conflict.is_domain(),
        "PROPERTY: ErrorKind::Conflict must be classified as a domain error.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_domain().\n\
         Common causes: Conflict missing from the domain match arm, or grouped \
         with operational errors.\n\
         Run: cargo test --test quiet_stragglers error_kind_is_domain"
    );
    assert!(
        ErrorKind::Validation.is_domain(),
        "PROPERTY: ErrorKind::Validation must be classified as a domain error.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_domain().\n\
         Common causes: Validation missing from the domain match arm.\n\
         Run: cargo test --test quiet_stragglers error_kind_is_domain"
    );
    assert!(
        ErrorKind::PolicyRejection.is_domain(),
        "PROPERTY: ErrorKind::PolicyRejection must be classified as a domain error.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_domain().\n\
         Common causes: PolicyRejection missing from the domain match arm, or \
         grouped with operational errors.\n\
         Run: cargo test --test quiet_stragglers error_kind_is_domain"
    );
    assert!(
        !ErrorKind::StorageError.is_domain(),
        "PROPERTY: ErrorKind::StorageError must NOT be classified as a domain error.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_domain().\n\
         Common causes: StorageError incorrectly placed in the domain match arm, or \
         wildcard arm returning true.\n\
         Run: cargo test --test quiet_stragglers error_kind_is_domain"
    );
    assert!(
        !ErrorKind::Timeout.is_domain(),
        "PROPERTY: ErrorKind::Timeout must NOT be classified as a domain error.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_domain().\n\
         Common causes: Timeout incorrectly placed in the domain match arm.\n\
         Run: cargo test --test quiet_stragglers error_kind_is_domain"
    );
    assert!(
        !ErrorKind::Internal.is_domain(),
        "PROPERTY: ErrorKind::Internal must NOT be classified as a domain error.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_domain().\n\
         Common causes: Internal incorrectly placed in the domain match arm, or \
         wildcard arm returning true.\n\
         Run: cargo test --test quiet_stragglers error_kind_is_domain"
    );
    assert!(
        !ErrorKind::Custom(99).is_domain(),
        "PROPERTY: ErrorKind::Custom must NOT be classified as a domain error by default.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_domain().\n\
         Common causes: Custom variant handled by a wildcard arm that returns true.\n\
         Run: cargo test --test quiet_stragglers error_kind_is_domain"
    );
}

#[test]
fn error_kind_is_operational() {
    assert!(
        ErrorKind::StorageError.is_operational(),
        "PROPERTY: ErrorKind::StorageError must be classified as operational.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_operational().\n\
         Common causes: StorageError missing from the operational match arm, or \
         is_operational() not including infrastructure errors.\n\
         Run: cargo test --test quiet_stragglers error_kind_is_operational"
    );
    assert!(
        ErrorKind::Timeout.is_operational(),
        "PROPERTY: ErrorKind::Timeout must be classified as operational.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_operational().\n\
         Common causes: Timeout missing from the operational match arm.\n\
         Run: cargo test --test quiet_stragglers error_kind_is_operational"
    );
    assert!(
        ErrorKind::Serialization.is_operational(),
        "PROPERTY: ErrorKind::Serialization must be classified as operational.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_operational().\n\
         Common causes: Serialization missing from the operational match arm, or \
         grouped with domain errors.\n\
         Run: cargo test --test quiet_stragglers error_kind_is_operational"
    );
    assert!(
        ErrorKind::Internal.is_operational(),
        "PROPERTY: ErrorKind::Internal must be classified as operational.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_operational().\n\
         Common causes: Internal missing from the operational match arm.\n\
         Run: cargo test --test quiet_stragglers error_kind_is_operational"
    );
    assert!(
        !ErrorKind::NotFound.is_operational(),
        "PROPERTY: ErrorKind::NotFound must NOT be classified as operational (it is a domain error).\n\
         Investigate: src/outcome/error.rs ErrorKind::is_operational().\n\
         Common causes: is_operational() wildcard arm returning true for domain errors.\n\
         Run: cargo test --test quiet_stragglers error_kind_is_operational"
    );
    assert!(
        !ErrorKind::Conflict.is_operational(),
        "PROPERTY: ErrorKind::Conflict must NOT be classified as operational (it is a domain error).\n\
         Investigate: src/outcome/error.rs ErrorKind::is_operational().\n\
         Common causes: Conflict incorrectly placed in the operational match arm.\n\
         Run: cargo test --test quiet_stragglers error_kind_is_operational"
    );
    assert!(
        !ErrorKind::Custom(99).is_operational(),
        "PROPERTY: ErrorKind::Custom must NOT be classified as operational by default.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_operational().\n\
         Common causes: Custom variant matched by wildcard arm that returns true.\n\
         Run: cargo test --test quiet_stragglers error_kind_is_operational"
    );
}

#[test]
fn outcome_error_display() {
    let err = OutcomeError {
        kind: ErrorKind::Conflict,
        message: "double booking".into(),
        compensation: None,
        retryable: false,
    };
    let s = format!("{err}");
    assert!(
        s.contains("Conflict"),
        "PROPERTY: OutcomeError Display must include the ErrorKind name.\n\
         Investigate: src/outcome/error.rs OutcomeError Display impl.\n\
         Common causes: Display formatting only the message field without including \
         the kind, or kind printed as a raw discriminant number instead of its name.\n\
         Run: cargo test --test quiet_stragglers outcome_error_display"
    );
    assert!(
        s.contains("double booking"),
        "PROPERTY: OutcomeError Display must include the error message string.\n\
         Investigate: src/outcome/error.rs OutcomeError Display impl.\n\
         Common causes: Display printing only the kind and omitting the message, \
         or message field not being formatted into the output string.\n\
         Run: cargo test --test quiet_stragglers outcome_error_display"
    );
}

// ================================================================
// src/guard/ — GateSet helpers + Denial
// ================================================================

#[test]
fn gateset_len_and_is_empty() {
    let mut gates = GateSet::<()>::new();
    assert!(
        gates.is_empty(),
        "PROPERTY: A newly created GateSet must be empty.\n\
         Investigate: src/guard/mod.rs GateSet::new() is_empty().\n\
         Common causes: GateSet::new() not initializing an empty inner collection, \
         or is_empty() always returning false.\n\
         Run: cargo test --test quiet_stragglers gateset_len_and_is_empty"
    );
    assert_eq!(
        gates.len(),
        0,
        "PROPERTY: A newly created GateSet must have length 0.\n\
         Investigate: src/guard/mod.rs GateSet::new() len().\n\
         Common causes: len() returning 1 from a sentinel element, or GateSet \
         pre-populated with a default gate.\n\
         Run: cargo test --test quiet_stragglers gateset_len_and_is_empty"
    );

    struct DummyGate;
    impl Gate<()> for DummyGate {
        fn name(&self) -> &'static str {
            "dummy"
        }
        fn evaluate(&self, _: &()) -> Result<(), Denial> {
            Ok(())
        }
    }

    gates.push(DummyGate);
    assert!(
        !gates.is_empty(),
        "PROPERTY: GateSet must not be empty after pushing one gate.\n\
         Investigate: src/guard/mod.rs GateSet::push() is_empty().\n\
         Common causes: push() not actually inserting into the inner collection, \
         or is_empty() not reflecting the current state.\n\
         Run: cargo test --test quiet_stragglers gateset_len_and_is_empty"
    );
    assert_eq!(
        gates.len(),
        1,
        "PROPERTY: GateSet must have length 1 after pushing one gate.\n\
         Investigate: src/guard/mod.rs GateSet::push() len().\n\
         Common causes: push() pushing a boxed copy without increasing the count, \
         or len() reading a cached stale value.\n\
         Run: cargo test --test quiet_stragglers gateset_len_and_is_empty"
    );
}

#[test]
fn gateset_default() {
    let gates = GateSet::<()>::default();
    assert!(
        gates.is_empty(),
        "PROPERTY: GateSet::default() must produce an empty gate set.\n\
         Investigate: src/guard/mod.rs GateSet Default impl.\n\
         Common causes: Default not delegating to new(), or Default adding a \
         built-in gate that should not be present.\n\
         Run: cargo test --test quiet_stragglers gateset_default"
    );
}

#[test]
fn gate_description_default() {
    struct DescGate;
    impl Gate<()> for DescGate {
        fn name(&self) -> &'static str {
            "desc_gate"
        }
        fn evaluate(&self, _: &()) -> Result<(), Denial> {
            Ok(())
        }
        // description() uses default impl
    }
    let gate = DescGate;
    assert_eq!(
        gate.description(),
        "",
        "PROPERTY: The default Gate::description() impl must return an empty string.\n\
         Investigate: src/guard/mod.rs Gate trait default description() impl.\n\
         Common causes: default impl returning the gate name instead of \"\", or \
         trait not providing a default impl and requiring every implementor to define it.\n\
         Run: cargo test --test quiet_stragglers gate_description_default"
    );
}

#[test]
fn denial_serialize() {
    let denial = Denial::new("test_gate", "access denied")
        .with_code("403")
        .with_context("user", "alice");

    let json = serde_json::to_string(&denial).expect("Denial should serialize");
    assert!(
        json.contains("test_gate"),
        "PROPERTY: Serialized Denial JSON must include the gate name.\n\
         Investigate: src/guard/denial.rs Denial Serialize impl.\n\
         Common causes: gate field omitted from serde derive, or field renamed \
         to something other than 'gate' in the serialized output.\n\
         Run: cargo test --test quiet_stragglers denial_serialize"
    );
    assert!(
        json.contains("403"),
        "PROPERTY: Serialized Denial JSON must include the code field value.\n\
         Investigate: src/guard/denial.rs Denial Serialize impl with_code().\n\
         Common causes: code field serialized as null instead of the set value, \
         or with_code() not storing the value in the struct.\n\
         Run: cargo test --test quiet_stragglers denial_serialize"
    );
    assert!(
        json.contains("alice"),
        "PROPERTY: Serialized Denial JSON must include context key-value pairs.\n\
         Investigate: src/guard/denial.rs Denial Serialize impl with_context().\n\
         Common causes: context map not included in serialization, or with_context() \
         not inserting into the context HashMap.\n\
         Run: cargo test --test quiet_stragglers denial_serialize"
    );
}

#[test]
fn denial_is_error_trait() {
    let denial = Denial::new("g", "msg");
    // Verify it implements std::error::Error (this is a compile-time check + runtime use)
    let err: &dyn std::error::Error = &denial;
    let display = format!("{err}");
    assert!(
        display.contains("[g]") && display.contains("msg"),
        "PROPERTY: Denial Display (via std::error::Error) must format as '[gate] message'.\n\
         Investigate: src/guard/denial.rs Denial Display impl.\n\
         Common causes: Display not wrapping the gate name in brackets, or printing \
         only the message without the gate name prefix.\n\
         Run: cargo test --test quiet_stragglers denial_is_error_trait"
    );
}

// ================================================================
// Outcome::flatten() — fixed abstraction level
// ================================================================

#[test]
fn flatten_unwraps_nested_ok() {
    let nested: Outcome<Outcome<i32>> = Outcome::Ok(Outcome::Ok(42));
    let flat = nested.flatten();
    assert_eq!(
        flat,
        Outcome::Ok(42),
        "PROPERTY: Outcome::flatten on Ok(Ok(42)) must produce Ok(42).\n\
         Investigate: src/outcome/mod.rs impl Outcome<Outcome<T>> flatten().\n\
         Common causes: flatten() returning the outer Ok without unwrapping the inner, \
         or not handling the doubly-nested case.\n\
         Run: cargo test --test quiet_stragglers flatten_unwraps_nested_ok"
    );
}

#[test]
fn flatten_propagates_outer_err() {
    let err = OutcomeError {
        kind: ErrorKind::Internal,
        message: "outer".into(),
        compensation: None,
        retryable: false,
    };
    let nested: Outcome<Outcome<i32>> = Outcome::Err(err);
    let flat = nested.flatten();
    assert!(
        flat.is_err(),
        "PROPERTY: Outcome::flatten on Err must propagate the outer error unchanged.\n\
         Investigate: src/outcome/mod.rs impl Outcome<Outcome<T>> flatten().\n\
         Common causes: flatten() converting outer Err to Ok(default), or returning \
         Outcome::Pending instead of the error.\n\
         Run: cargo test --test quiet_stragglers flatten_propagates_outer_err"
    );
}

#[test]
fn flatten_propagates_inner_err() {
    let err = OutcomeError {
        kind: ErrorKind::Internal,
        message: "inner".into(),
        compensation: None,
        retryable: false,
    };
    let nested: Outcome<Outcome<i32>> = Outcome::Ok(Outcome::Err(err));
    let flat = nested.flatten();
    assert!(
        flat.is_err(),
        "PROPERTY: Outcome::flatten on Ok(Err) must propagate the inner error.\n\
         Investigate: src/outcome/mod.rs impl Outcome<Outcome<T>> flatten().\n\
         Common causes: flatten() treating Ok(Err) as Ok(default) by ignoring the \
         inner variant, or only handling the outer layer.\n\
         Run: cargo test --test quiet_stragglers flatten_propagates_inner_err"
    );
}

#[test]
fn flatten_distributes_over_batch() {
    let nested: Outcome<Outcome<i32>> = Outcome::Batch(vec![
        Outcome::Ok(Outcome::Ok(1)),
        Outcome::Ok(Outcome::Ok(2)),
    ]);
    let flat = nested.flatten();
    assert_eq!(
        flat,
        Outcome::Batch(vec![Outcome::Ok(1), Outcome::Ok(2)]),
        "PROPERTY: Outcome::flatten on Batch must flatten each inner Outcome element.\n\
         Investigate: src/outcome/mod.rs impl Outcome<Outcome<T>> flatten() Batch arm.\n\
         Common causes: flatten() not recursing into Batch items, or collecting the \
         batch as Outcome<Vec<Outcome<T>>> instead of Outcome::Batch(Vec<Outcome<T>>).\n\
         Run: cargo test --test quiet_stragglers flatten_distributes_over_batch"
    );
}

// ================================================================
// Reactive<P> — SPEC-mandated subscribe→react→append pattern
// ================================================================

use free_batteries::store::{Store, StoreConfig};
use std::sync::Arc;
use tempfile::TempDir;

struct OrderReactor;
impl free_batteries::event::Reactive<serde_json::Value> for OrderReactor {
    fn react(
        &self,
        event: &Event<serde_json::Value>,
    ) -> Vec<(Coordinate, EventKind, serde_json::Value)> {
        // When we see a "create_order" event, emit an "order_created" reaction
        if event.event_kind() == EventKind::custom(0xA, 1) {
            vec![(
                Coordinate::new("order:reactions", "scope:test").expect("valid"),
                EventKind::custom(0xA, 2),
                serde_json::json!({"reacted_to": event.event_id().to_string()}),
            )]
        } else {
            vec![]
        }
    }
}

#[test]
fn reactive_subscribe_react_append_pattern() {
    // This test proves the SPEC's "7 lines of glue" pattern works:
    // subscribe → receive → react() → append_reaction()

    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        sync_every_n_events: 1,
        ..StoreConfig::new("")
    };
    let store = Arc::new(Store::open(config).expect("open"));
    let coord = Coordinate::new("order:1", "scope:test").expect("valid");
    let kind = EventKind::custom(0xA, 1); // "create_order"

    // Subscribe before writing
    let region = Region::all();
    let sub = store.subscribe(&region);

    // Write the root event from another thread
    let store_w = Arc::clone(&store);
    let coord_w = coord.clone();
    let writer = std::thread::spawn(move || {
        store_w
            .append(&coord_w, kind, &serde_json::json!({"item": "widget"}))
            .expect("append root")
    });
    let root_receipt = writer.join().expect("writer thread");

    // Receive the notification
    let rx = sub.receiver();
    let notif = rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("should receive notification");

    // React: the OrderReactor decides what to emit
    let reactor = OrderReactor;
    // Build a minimal event for the reactor (it only needs kind + event_id)
    let header = EventHeader::new(
        notif.event_id,
        notif.correlation_id,
        notif.causation_id,
        0,
        DagPosition::root(),
        0,
        notif.kind,
    );
    let event = Event::<serde_json::Value>::new(header, serde_json::Value::Null);
    let reactions = reactor.react(&event);

    assert_eq!(
        reactions.len(),
        1,
        "PROPERTY: OrderReactor must produce exactly 1 reaction for a create_order event.\n\
         Investigate: src/event/sourcing.rs Reactive trait react() method.\n\
         Common causes: react() returning an empty vec because event_kind comparison \
         fails, or EventKind::custom encoding mismatch between writer and reactor.\n\
         Run: cargo test --test quiet_stragglers reactive_subscribe_react_append_pattern"
    );

    // Append reactions via append_reaction (the causal link)
    for (react_coord, react_kind, react_payload) in reactions {
        store
            .append_reaction(
                &react_coord,
                react_kind,
                &react_payload,
                root_receipt.event_id,
                root_receipt.event_id,
            )
            .expect("append reaction");
    }

    // Verify: 2 events total (root + reaction)
    let stats = store.stats();
    assert_eq!(
        stats.event_count, 2,
        "PROPERTY: After root event + 1 reaction, store must contain exactly 2 events.\n\
         Investigate: src/store/mod.rs Store::append_reaction() src/event/sourcing.rs.\n\
         Common causes: append_reaction() not writing to the store, or stats.event_count \
         not counting reaction events that go to a different coordinate.\n\
         Run: cargo test --test quiet_stragglers reactive_subscribe_react_append_pattern"
    );

    store.sync().expect("sync");
}
