//! Unit tests for the event domain: EventId, EventKind, EventHeader, Event methods,
//! DagPosition, and the define_entity_id! macro.
//! [SPEC:tests/event_api.rs]
//!
//! PROVES: LAW-003 (No Orphan Infrastructure — every pub item exercised)
//! DEFENDS: FM-007 (Island Syndrome — pub items must connect to tests)
//! INVARIANTS: INV-TYPE (event round-trip fidelity), INV-OBS (every pub API has a test witness)

use batpak::id::EntityIdType;
use batpak::prelude::*;

// ================================================================
// src/id/mod.rs — EntityIdType + EventId + define_entity_id! macro
// ================================================================

#[test]
fn event_id_now_v7_is_nonzero() {
    let id = batpak::id::EventId::now_v7();
    assert_ne!(
        id.as_u128(),
        0,
        "PROPERTY: EventId::now_v7() must generate a non-zero UUIDv7.\n\
         Investigate: src/id/mod.rs generate_v7_id().\n\
         Common causes: UUID library returning nil on clock skew, feature flag disabled, \
         or SystemTime before Unix epoch on the test host.\n\
         Run: cargo test --test event_api event_id_now_v7_is_nonzero"
    );
}

#[test]
fn event_id_nil_is_zero() {
    let id = batpak::id::EventId::nil();
    assert_eq!(
        id.as_u128(),
        0,
        "PROPERTY: EventId::nil() must return the zero UUID.\n\
         Investigate: src/id/mod.rs nil().\n\
         Common causes: nil() forwarding to now_v7() by mistake, or inner type default \
         not being zero-initialized.\n\
         Run: cargo test --test event_api event_id_nil_is_zero"
    );
}

#[test]
fn event_id_round_trip() {
    let raw: u128 = 0xDEAD_BEEF_CAFE_BABE_1234_5678_9ABC_DEF0;
    let id = batpak::id::EventId::new(raw);
    assert_eq!(
        id.as_u128(),
        raw,
        "PROPERTY: EventId::new(raw).as_u128() must equal raw (lossless round-trip).\n\
         Investigate: src/id/mod.rs new() as_u128().\n\
         Common causes: byte-order swap in new() or as_u128(), truncation of high bits, \
         or wrapping newtype that strips the value.\n\
         Run: cargo test --test event_api event_id_round_trip"
    );
}

#[test]
fn event_id_display_format() {
    let id = batpak::id::EventId::new(0xFF);
    let s = format!("{id}");
    assert!(
        s.starts_with("event:"),
        "PROPERTY: EventId Display must start with the entity prefix 'event:'.\n\
         Investigate: src/id/mod.rs define_entity_id! Display impl.\n\
         Common causes: macro emitting the wrong prefix string literal, or Display \
         delegating to as_u128() without prepending the prefix.\n\
         Run: cargo test --test event_api event_id_display_format"
    );
    assert!(
        s.contains("ff"),
        "PROPERTY: EventId Display must contain the hex digits of the underlying u128.\n\
         Investigate: src/id/mod.rs define_entity_id! Display impl.\n\
         Common causes: Display printing decimal instead of hex, or padding zeroing \
         out the only non-zero byte before formatting.\n\
         Run: cargo test --test event_api event_id_display_format"
    );
}

#[test]
fn event_id_from_str_with_prefix() {
    use std::str::FromStr;
    let id = batpak::id::EventId::from_str("event:00000000000000000000000000000042")
        .expect("parse with prefix");
    assert_eq!(
        id.as_u128(),
        0x42,
        "PROPERTY: EventId::from_str with 'event:' prefix must parse the hex portion correctly.\n\
         Investigate: src/id/mod.rs define_entity_id! FromStr impl.\n\
         Common causes: prefix stripping off-by-one consuming a hex digit, or \
         from_str not recognising the 'entity:' prefix at all.\n\
         Run: cargo test --test event_api event_id_from_str_with_prefix"
    );
}

#[test]
fn event_id_from_str_bare_hex() {
    use std::str::FromStr;
    let id =
        batpak::id::EventId::from_str("00000000000000000000000000000042").expect("parse bare hex");
    assert_eq!(
        id.as_u128(),
        0x42,
        "PROPERTY: EventId::from_str must parse bare hex (no prefix) correctly.\n\
         Investigate: src/id/mod.rs define_entity_id! FromStr impl.\n\
         Common causes: parser requiring the 'entity:' prefix and returning Err on bare \
         hex, or u128::from_str_radix receiving the wrong slice.\n\
         Run: cargo test --test event_api event_id_from_str_bare_hex"
    );
}

#[test]
fn event_id_from_str_rejects_garbage() {
    use std::str::FromStr;
    let result = batpak::id::EventId::from_str("not_hex_at_all");
    assert!(
        result.is_err(),
        "PROPERTY: EventId::FromStr must reject non-hex input with Err.\n\
         Investigate: src/id/mod.rs define_entity_id! FromStr impl.\n\
         Common causes: parser returning Ok(0) on parse failure instead of Err, \
         or unwrap_or silently swallowing the error.\n\
         Run: cargo test --test event_api event_id_from_str_rejects_garbage"
    );
}

#[test]
fn define_entity_id_custom_type() {
    batpak::define_entity_id!(OrderId, "order");
    use batpak::id::EntityIdType;

    let id = OrderId::now_v7();
    assert_ne!(
        id.as_u128(),
        0,
        "PROPERTY: define_entity_id! macro must generate a non-zero ID via now_v7().\n\
         Investigate: src/id/mod.rs define_entity_id! macro expansion.\n\
         Common causes: macro forwarding to a stub that returns nil, or UUID clock \
         returning zero on test host.\n\
         Run: cargo test --test event_api define_entity_id_custom_type"
    );
    assert_eq!(
        OrderId::ENTITY_NAME,
        "order",
        "PROPERTY: define_entity_id! macro must set ENTITY_NAME to the supplied string.\n\
         Investigate: src/id/mod.rs define_entity_id! macro ENTITY_NAME const.\n\
         Common causes: macro hardcoding a different literal, or const not being \
         set from the macro argument.\n\
         Run: cargo test --test event_api define_entity_id_custom_type"
    );

    let display = format!("{id}");
    assert!(
        display.starts_with("order:"),
        "PROPERTY: Custom entity ID Display must use the registered entity name as prefix.\n\
         Investigate: src/id/mod.rs define_entity_id! Display impl.\n\
         Common causes: Display impl using a hardcoded prefix instead of ENTITY_NAME, \
         or macro not emitting a Display impl at all.\n\
         Run: cargo test --test event_api define_entity_id_custom_type"
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
         Run: cargo test --test event_api event_header_flags_requires_ack"
    );
    assert!(
        !header.is_transactional(),
        "PROPERTY: Flag bit 0x01 must NOT set is_transactional().\n\
         Investigate: src/event/header.rs is_transactional() flag mask.\n\
         Common causes: is_transactional() using the same bit mask as requires_ack().\n\
         Run: cargo test --test event_api event_header_flags_requires_ack"
    );
    assert!(
        !header.is_replay(),
        "PROPERTY: Flag bit 0x01 must NOT set is_replay().\n\
         Investigate: src/event/header.rs is_replay() flag mask.\n\
         Common causes: is_replay() using bit 0x01 instead of 0x08.\n\
         Run: cargo test --test event_api event_header_flags_requires_ack"
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
         Run: cargo test --test event_api event_header_flags_transactional"
    );
    assert!(
        !header.requires_ack(),
        "PROPERTY: Flag bit 0x02 must NOT set requires_ack().\n\
         Investigate: src/event/header.rs requires_ack() flag mask.\n\
         Common causes: requires_ack() testing bit 0x02 instead of 0x01.\n\
         Run: cargo test --test event_api event_header_flags_transactional"
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
         Run: cargo test --test event_api event_header_flags_replay"
    );
    assert!(
        !header.requires_ack(),
        "PROPERTY: Flag bit 0x08 must NOT set requires_ack().\n\
         Investigate: src/event/header.rs requires_ack() flag mask.\n\
         Common causes: requires_ack() mask accidentally overlapping with replay bit.\n\
         Run: cargo test --test event_api event_header_flags_replay"
    );
    assert!(
        !header.is_transactional(),
        "PROPERTY: Flag bit 0x08 must NOT set is_transactional().\n\
         Investigate: src/event/header.rs is_transactional() flag mask.\n\
         Common causes: is_transactional() mask accidentally overlapping with replay bit.\n\
         Run: cargo test --test event_api event_header_flags_replay"
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
         Run: cargo test --test event_api event_header_flags_zero_all_false"
    );
    assert!(
        !header.is_transactional(),
        "PROPERTY: Zero flags must not set is_transactional().\n\
         Investigate: src/event/header.rs is_transactional() flags field initialization.\n\
         Common causes: EventHeader::new() defaulting flags to a non-zero value, or \
         is_transactional() not masking correctly against 0x02.\n\
         Run: cargo test --test event_api event_header_flags_zero_all_false"
    );
    assert!(
        !header.is_replay(),
        "PROPERTY: Zero flags must not set is_replay().\n\
         Investigate: src/event/header.rs is_replay() flags field initialization.\n\
         Common causes: EventHeader::new() defaulting flags to a non-zero value, or \
         is_replay() not masking correctly against 0x08.\n\
         Run: cargo test --test event_api event_header_flags_zero_all_false"
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
         Run: cargo test --test event_api event_header_age_us"
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
             Run: cargo test --test event_api event_kind_system_constants_are_system",
            kind
        );
        assert!(
            !kind.is_effect(),
            "PROPERTY: System EventKind {:?} must NOT return true for is_effect().\n\
             Investigate: src/event/kind.rs EventKind::is_effect() category check.\n\
             Common causes: is_effect() using the same category mask as is_system().\n\
             Run: cargo test --test event_api event_kind_system_constants_are_system",
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
             Run: cargo test --test event_api event_kind_effect_constants_are_effect",
            kind
        );
        assert!(
            !kind.is_system(),
            "PROPERTY: Effect EventKind {:?} must NOT return true for is_system().\n\
             Investigate: src/event/kind.rs EventKind::is_system() category check.\n\
             Common causes: is_system() using the same category mask as is_effect().\n\
             Run: cargo test --test event_api event_kind_effect_constants_are_effect",
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
         Run: cargo test --test event_api event_kind_custom_is_neither_system_nor_effect"
    );
    assert!(
        !custom.is_effect(),
        "PROPERTY: Custom EventKind must NOT be classified as an effect event.\n\
         Investigate: src/event/kind.rs EventKind::is_effect() category range check.\n\
         Common causes: is_effect() treating category 0x5 as reserved effect space, \
         or category boundaries defined incorrectly.\n\
         Run: cargo test --test event_api event_kind_custom_is_neither_system_nor_effect"
    );
    assert_eq!(
        custom.category(),
        0x5,
        "PROPERTY: EventKind::custom(0x5, 42) must store and return category 0x5.\n\
         Investigate: src/event/kind.rs EventKind::custom() category() packing.\n\
         Common causes: category packed into wrong nibble position, or category() \
         extracting the wrong bits from the u16.\n\
         Run: cargo test --test event_api event_kind_custom_is_neither_system_nor_effect"
    );
    assert_eq!(
        custom.type_id(),
        42,
        "PROPERTY: EventKind::custom(0x5, 42) must store and return type_id 42.\n\
         Investigate: src/event/kind.rs EventKind::custom() type_id() packing.\n\
         Common causes: type_id packed into wrong byte position, or type_id() \
         extracting bits that include the category nibble.\n\
         Run: cargo test --test event_api event_kind_custom_is_neither_system_nor_effect"
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
         Run: cargo test --test event_api event_kind_display_hex"
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
         Run: cargo test --test event_api event_with_hash_chain_sets_field"
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
         Run: cargo test --test event_api event_is_genesis_true_when_prev_hash_zero"
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
         Run: cargo test --test event_api event_is_genesis_false_when_prev_hash_nonzero"
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
         Run: cargo test --test event_api event_is_genesis_true_when_no_hash_chain"
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
         Run: cargo test --test event_api event_map_payload_transforms_preserving_header"
    );
    assert_eq!(
        mapped.header.event_id, 42,
        "PROPERTY: Event::map_payload must preserve the original header unchanged.\n\
         Investigate: src/event/mod.rs Event::map_payload().\n\
         Common causes: map_payload() creating a new header instead of moving the original, \
         or resetting header fields like event_id to defaults.\n\
         Run: cargo test --test event_api event_map_payload_transforms_preserving_header"
    );
    assert_eq!(
        mapped.header.timestamp_us, 100,
        "PROPERTY: Event::map_payload must preserve header.timestamp_us unchanged.\n\
         Investigate: src/event/mod.rs Event::map_payload().\n\
         Common causes: map_payload() rebuilding a fresh header that zero-initializes \
         timestamp_us instead of carrying over the original header.\n\
         Run: cargo test --test event_api event_map_payload_transforms_preserving_header"
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
         Run: cargo test --test event_api event_position_returns_header_position"
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
         Run: cargo test --test event_api dag_position_root"
    );
    assert_eq!(
        pos.lane, 0,
        "PROPERTY: DagPosition::root() must set lane to 0.\n\
         Investigate: src/coordinate/position.rs DagPosition::root().\n\
         Common causes: root() initializing lane to 1 instead of 0, or \
         field ordering swap between lane and depth.\n\
         Run: cargo test --test event_api dag_position_root"
    );
    assert_eq!(
        pos.sequence, 0,
        "PROPERTY: DagPosition::root() must set sequence to 0.\n\
         Investigate: src/coordinate/position.rs DagPosition::root().\n\
         Common causes: root() not zero-initializing sequence, or sequence \
         defaulting to 1 for 'first event' semantics.\n\
         Run: cargo test --test event_api dag_position_root"
    );
    assert!(
        pos.is_root(),
        "PROPERTY: DagPosition::root() must satisfy is_root().\n\
         Investigate: src/coordinate/position.rs DagPosition::is_root().\n\
         Common causes: is_root() checking only depth but not lane or sequence, \
         or root() not producing coordinates that satisfy is_root().\n\
         Run: cargo test --test event_api dag_position_root"
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
         Run: cargo test --test event_api dag_position_child"
    );
    assert_eq!(
        pos.depth, 0,
        "PROPERTY: DagPosition::child() must set depth to 0 (same lane as root).\n\
         Investigate: src/coordinate/position.rs DagPosition::child().\n\
         Common causes: child() incrementing depth like fork(), or copying depth \
         from an earlier position.\n\
         Run: cargo test --test event_api dag_position_child"
    );
    assert_eq!(
        pos.lane, 0,
        "PROPERTY: DagPosition::child() must set lane to 0 (main lane).\n\
         Investigate: src/coordinate/position.rs DagPosition::child().\n\
         Common causes: child() calling fork() internally, which would assign a new lane.\n\
         Run: cargo test --test event_api dag_position_child"
    );
    assert!(
        !pos.is_root(),
        "PROPERTY: DagPosition::child(42) must NOT satisfy is_root() (non-zero sequence).\n\
         Investigate: src/coordinate/position.rs DagPosition::is_root().\n\
         Common causes: is_root() only checking depth==0 and lane==0 without checking \
         sequence==0.\n\
         Run: cargo test --test event_api dag_position_child"
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
         Run: cargo test --test event_api dag_position_fork"
    );
    assert_eq!(
        pos.lane, 3,
        "PROPERTY: DagPosition::fork(2, 3) must set lane to 3.\n\
         Investigate: src/coordinate/position.rs DagPosition::fork().\n\
         Common causes: fork() using an auto-incrementing lane counter instead of \
         the provided lane argument.\n\
         Run: cargo test --test event_api dag_position_fork"
    );
    assert_eq!(
        pos.sequence, 0,
        "PROPERTY: DagPosition::fork() must start at sequence 0 (beginning of the new branch).\n\
         Investigate: src/coordinate/position.rs DagPosition::fork().\n\
         Common causes: fork() copying sequence from the parent position instead of \
         resetting it to 0.\n\
         Run: cargo test --test event_api dag_position_fork"
    );
    assert!(
        !pos.is_root(),
        "PROPERTY: DagPosition::fork() must NOT be is_root() (non-zero depth and lane).\n\
         Investigate: src/coordinate/position.rs DagPosition::is_root().\n\
         Common causes: is_root() returning true when sequence==0 regardless of depth/lane.\n\
         Run: cargo test --test event_api dag_position_fork"
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
         Run: cargo test --test event_api dag_position_is_ancestor_of_same_lane"
    );
    assert!(
        !b.is_ancestor_of(&a),
        "PROPERTY: child(5) must NOT be an ancestor of child(2) (higher sequence cannot be ancestor).\n\
         Investigate: src/coordinate/position.rs DagPosition::is_ancestor_of().\n\
         Common causes: is_ancestor_of() not checking direction, returning true for \
         any two positions on the same lane.\n\
         Run: cargo test --test event_api dag_position_is_ancestor_of_same_lane"
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
         Run: cargo test --test event_api dag_position_is_ancestor_of_different_lanes"
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
         Run: cargo test --test event_api dag_position_partial_ord_same_lane"
    );
    assert!(
        b > a,
        "PROPERTY: child(5) must be greater than child(2) on the same lane.\n\
         Investigate: src/coordinate/position.rs DagPosition PartialOrd impl.\n\
         Common causes: PartialOrd impl not providing symmetry, or gt() delegating \
         to an incorrect comparison.\n\
         Run: cargo test --test event_api dag_position_partial_ord_same_lane"
    );
    let c = DagPosition::child(2);
    assert!(
        a.partial_cmp(&c) == Some(std::cmp::Ordering::Equal),
        "PROPERTY: Two child(2) positions on the same lane must compare as Equal.\n\
         Investigate: src/coordinate/position.rs DagPosition PartialOrd impl.\n\
         Common causes: PartialOrd returning None for equal positions, or failing to \
         short-circuit when all fields are equal.\n\
         Run: cargo test --test event_api dag_position_partial_ord_same_lane"
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
         Run: cargo test --test event_api dag_position_partial_ord_different_lanes_incomparable"
    );
}

#[test]
fn dag_position_display() {
    let pos = DagPosition::new(1, 2, 3);
    assert_eq!(
        format!("{pos}"),
        "1:2:3@0.0",
        "PROPERTY: DagPosition Display must format as 'depth:lane:sequence@wall_ms.counter'.\n\
         Investigate: src/coordinate/position.rs DagPosition Display impl.\n\
         Common causes: Display outputting fields in wrong order (e.g. sequence:lane:depth), \
         using a different separator than ':', or printing only some fields.\n\
         Run: cargo test --test event_api dag_position_display"
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
         Run: cargo test --test event_api dag_position_fork_creates_new_lane"
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
         Run: cargo test --test event_api dag_position_forked_incomparable_with_lane_zero"
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
         Run: cargo test --test event_api dag_position_fork_is_not_ancestor_across_lanes"
    );
}
