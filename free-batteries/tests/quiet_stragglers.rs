//! Tests for every quiet straggler — pub functions that compile and pass clippy
//! but have zero test coverage. Organized by module.
//! [SPEC:tests/quiet_stragglers.rs]
//!
//! These are the functions that "won't make noise but won't say when they're
//! hurt either." Each test asserts real behavior with investigation pointers.

use free_batteries::prelude::*;
use free_batteries::id::EntityIdType;
use free_batteries::event::Reactive;

// ================================================================
// src/id/mod.rs — EntityIdType + EventId + define_entity_id! macro
// ================================================================

#[test]
fn event_id_now_v7_is_nonzero() {
    let id = free_batteries::id::EventId::now_v7();
    assert_ne!(id.as_u128(), 0,
        "EventId::now_v7() should generate a non-zero UUIDv7. \
         Investigate: src/id/mod.rs generate_v7_id().");
}

#[test]
fn event_id_nil_is_zero() {
    let id = free_batteries::id::EventId::nil();
    assert_eq!(id.as_u128(), 0,
        "EventId::nil() should be zero. Investigate: src/id/mod.rs nil().");
}

#[test]
fn event_id_round_trip() {
    let raw: u128 = 0xDEAD_BEEF_CAFE_BABE_1234_5678_9ABC_DEF0;
    let id = free_batteries::id::EventId::new(raw);
    assert_eq!(id.as_u128(), raw,
        "EventId::new() → as_u128() round-trip failed. Investigate: src/id/mod.rs.");
}

#[test]
fn event_id_display_format() {
    let id = free_batteries::id::EventId::new(0xFF);
    let s = format!("{id}");
    assert!(s.starts_with("event:"),
        "EventId Display should start with 'event:'. Got: {s}. \
         Investigate: src/id/mod.rs define_entity_id! Display impl.");
    assert!(s.contains("ff"),
        "EventId Display should contain hex. Got: {s}.");
}

#[test]
fn event_id_from_str_with_prefix() {
    use std::str::FromStr;
    let id = free_batteries::id::EventId::from_str("event:00000000000000000000000000000042")
        .expect("parse with prefix");
    assert_eq!(id.as_u128(), 0x42);
}

#[test]
fn event_id_from_str_bare_hex() {
    use std::str::FromStr;
    let id = free_batteries::id::EventId::from_str("00000000000000000000000000000042")
        .expect("parse bare hex");
    assert_eq!(id.as_u128(), 0x42);
}

#[test]
fn event_id_from_str_rejects_garbage() {
    use std::str::FromStr;
    let result = free_batteries::id::EventId::from_str("not_hex_at_all");
    assert!(result.is_err(),
        "EventId::FromStr should reject non-hex input. Investigate: src/id/mod.rs FromStr.");
}

#[test]
fn define_entity_id_custom_type() {
    free_batteries::define_entity_id!(OrderId, "order");
    use free_batteries::id::EntityIdType;

    let id = OrderId::now_v7();
    assert_ne!(id.as_u128(), 0);
    assert_eq!(OrderId::ENTITY_NAME, "order");

    let display = format!("{id}");
    assert!(display.starts_with("order:"),
        "Custom entity ID Display should use entity name. Got: {display}");
}

// ================================================================
// src/pipeline/ — Bypass system + Proposal::map
// ================================================================

struct TestBypassReason;
impl free_batteries::pipeline::BypassReason for TestBypassReason {
    fn name(&self) -> &'static str { "test_bypass" }
    fn justification(&self) -> &'static str { "testing bypass audit trail" }
}

static TEST_BYPASS: TestBypassReason = TestBypassReason;

#[test]
fn pipeline_bypass_returns_bypass_receipt() {
    let proposal = Proposal::new(42);
    let receipt = free_batteries::pipeline::Pipeline::<()>::bypass(proposal, &TEST_BYPASS);

    assert_eq!(receipt.payload, 42);
    assert_eq!(receipt.reason, "test_bypass");
    assert_eq!(receipt.justification, "testing bypass audit trail");
}

#[test]
fn proposal_map_transforms_payload() {
    let proposal = Proposal::new(21);
    let doubled = proposal.map(|x| x * 2);
    assert_eq!(*doubled.payload(), 42,
        "Proposal::map should transform the payload. Investigate: src/pipeline/mod.rs map().");
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

    assert_eq!(decoded.payload, "test");
    assert_eq!(decoded.event_id, 0xDEAD_BEEF_CAFE_BABE_1234_5678_9ABC_DEF0,
        "Committed event_id should round-trip through u128_bytes wire format. \
         Investigate: src/pipeline/mod.rs + src/wire.rs u128_bytes.");
    assert_eq!(decoded.sequence, 42);
    assert_eq!(decoded.hash, [0xAA; 32]);
}

// ================================================================
// src/event/header.rs — Flag system
// ================================================================

#[test]
fn event_header_flags_requires_ack() {
    let header = EventHeader::new(
        1, 1, None, 0, DagPosition::root(), 0, EventKind::DATA,
    ).with_flags(0x01);
    assert!(header.requires_ack(),
        "Flag 0x01 should mean requires_ack. Investigate: src/event/header.rs.");
    assert!(!header.is_transactional());
    assert!(!header.is_replay());
}

#[test]
fn event_header_flags_transactional() {
    let header = EventHeader::new(
        1, 1, None, 0, DagPosition::root(), 0, EventKind::DATA,
    ).with_flags(0x02);
    assert!(header.is_transactional());
    assert!(!header.requires_ack());
}

#[test]
fn event_header_flags_replay() {
    let header = EventHeader::new(
        1, 1, None, 0, DagPosition::root(), 0, EventKind::DATA,
    ).with_flags(0x08);
    assert!(header.is_replay());
    assert!(!header.requires_ack());
    assert!(!header.is_transactional());
}

#[test]
fn event_header_flags_zero_all_false() {
    let header = EventHeader::new(
        1, 1, None, 0, DagPosition::root(), 0, EventKind::DATA,
    );
    assert!(!header.requires_ack());
    assert!(!header.is_transactional());
    assert!(!header.is_replay());
}

#[test]
fn event_header_age_us() {
    let header = EventHeader::new(
        1, 1, None, 1_000_000, DagPosition::root(), 0, EventKind::DATA,
    );
    let age = header.age_us(2_000_000);
    assert_eq!(age, 1_000_000,
        "age_us should return delta between now and timestamp. \
         Investigate: src/event/header.rs age_us().");
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
        assert!(kind.is_system(),
            "EventKind {:?} should be is_system(). Investigate: src/event/kind.rs", kind);
        assert!(!kind.is_effect());
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
        assert!(kind.is_effect(),
            "EventKind {:?} should be is_effect(). Investigate: src/event/kind.rs", kind);
        assert!(!kind.is_system());
    }
}

#[test]
fn event_kind_custom_is_neither_system_nor_effect() {
    let custom = EventKind::custom(0x5, 42);
    assert!(!custom.is_system());
    assert!(!custom.is_effect());
    assert_eq!(custom.category(), 0x5);
    assert_eq!(custom.type_id(), 42);
}

#[test]
fn event_kind_display_hex() {
    let kind = EventKind::custom(0xA, 0xBC);
    let s = format!("{kind}");
    assert_eq!(s, "0xA0BC",
        "EventKind Display should be '0x{{:04X}}'. Got: {s}. \
         Investigate: src/event/kind.rs Display impl.");
}

// ================================================================
// src/event/mod.rs — Event convenience methods
// ================================================================

#[test]
fn event_with_hash_chain_sets_field() {
    let header = EventHeader::new(
        1, 1, None, 0, DagPosition::root(), 0, EventKind::DATA,
    );
    let chain = HashChain { prev_hash: [0u8; 32], event_hash: [1u8; 32] };
    let event = Event::new(header, "payload").with_hash_chain(chain.clone());

    assert_eq!(event.hash_chain, Some(chain),
        "with_hash_chain should set the hash_chain field. Investigate: src/event/mod.rs.");
}

#[test]
fn event_is_genesis_true_when_prev_hash_zero() {
    let header = EventHeader::new(1, 1, None, 0, DagPosition::root(), 0, EventKind::DATA);
    let event = Event::new(header, ()).with_hash_chain(HashChain {
        prev_hash: [0u8; 32],
        event_hash: [1u8; 32],
    });
    assert!(event.is_genesis(), "Event with zero prev_hash should be genesis.");
}

#[test]
fn event_is_genesis_false_when_prev_hash_nonzero() {
    let header = EventHeader::new(1, 1, None, 0, DagPosition::root(), 0, EventKind::DATA);
    let event = Event::new(header, ()).with_hash_chain(HashChain {
        prev_hash: [0xFF; 32],
        event_hash: [1u8; 32],
    });
    assert!(!event.is_genesis(), "Event with non-zero prev_hash should not be genesis.");
}

#[test]
fn event_is_genesis_true_when_no_hash_chain() {
    let header = EventHeader::new(1, 1, None, 0, DagPosition::root(), 0, EventKind::DATA);
    let event = Event::new(header, ());
    assert!(event.is_genesis(), "Event without hash_chain should be treated as genesis.");
}

#[test]
fn event_map_payload_transforms_preserving_header() {
    let header = EventHeader::new(42, 42, None, 100, DagPosition::child(5), 0, EventKind::DATA);
    let event = Event::new(header, 21);
    let mapped = event.map_payload(|x| x * 2);
    assert_eq!(mapped.payload, 42, "map_payload should transform payload.");
    assert_eq!(mapped.header.event_id, 42, "map_payload should preserve header.");
    assert_eq!(mapped.header.timestamp_us, 100);
}

#[test]
fn event_position_returns_header_position() {
    let pos = DagPosition::child(7);
    let header = EventHeader::new(1, 1, None, 0, pos, 0, EventKind::DATA);
    let event = Event::new(header, ());
    assert_eq!(*event.position(), pos);
}

// ================================================================
// src/coordinate/position.rs — DagPosition
// ================================================================

#[test]
fn dag_position_root() {
    let pos = DagPosition::root();
    assert_eq!(pos.depth, 0);
    assert_eq!(pos.lane, 0);
    assert_eq!(pos.sequence, 0);
    assert!(pos.is_root());
}

#[test]
fn dag_position_child() {
    let pos = DagPosition::child(42);
    assert_eq!(pos.sequence, 42);
    assert_eq!(pos.depth, 0);
    assert_eq!(pos.lane, 0);
    assert!(!pos.is_root());
}

#[test]
fn dag_position_fork() {
    let pos = DagPosition::fork(2, 3);
    assert_eq!(pos.depth, 3, "fork(parent_depth=2) should set depth to 3.");
    assert_eq!(pos.lane, 3);
    assert_eq!(pos.sequence, 0, "fork should start at sequence 0.");
    assert!(!pos.is_root());
}

#[test]
fn dag_position_is_ancestor_of_same_lane() {
    let a = DagPosition::child(2);
    let b = DagPosition::child(5);
    assert!(a.is_ancestor_of(&b),
        "child(2) should be ancestor of child(5) on same lane. \
         Investigate: src/coordinate/position.rs is_ancestor_of().");
    assert!(!b.is_ancestor_of(&a), "child(5) should NOT be ancestor of child(2).");
}

#[test]
fn dag_position_is_ancestor_of_different_lanes() {
    let a = DagPosition::new(0, 0, 2);
    let b = DagPosition::new(0, 1, 5); // different lane
    assert!(!a.is_ancestor_of(&b),
        "Different lanes should not be ancestors. Investigate: position.rs is_ancestor_of().");
}

#[test]
fn dag_position_partial_ord_same_lane() {
    let a = DagPosition::child(2);
    let b = DagPosition::child(5);
    assert!(a < b, "child(2) < child(5) on same lane.");
    assert!(b > a);
    let c = DagPosition::child(2);
    assert!(a.partial_cmp(&c) == Some(std::cmp::Ordering::Equal));
}

#[test]
fn dag_position_partial_ord_different_lanes_incomparable() {
    let a = DagPosition::new(0, 0, 2);
    let b = DagPosition::new(0, 1, 5);
    assert_eq!(a.partial_cmp(&b), None,
        "Different lanes should be incomparable (None). \
         Investigate: src/coordinate/position.rs PartialOrd.");
}

#[test]
fn dag_position_display() {
    let pos = DagPosition::new(1, 2, 3);
    assert_eq!(format!("{pos}"), "1:2:3",
        "DagPosition Display should be 'depth:lane:sequence'.");
}

// ================================================================
// src/outcome/error.rs — ErrorKind classification
// ================================================================

#[test]
fn error_kind_is_retryable() {
    assert!(ErrorKind::StorageError.is_retryable());
    assert!(ErrorKind::Timeout.is_retryable());
    assert!(!ErrorKind::NotFound.is_retryable());
    assert!(!ErrorKind::Conflict.is_retryable());
    assert!(!ErrorKind::Internal.is_retryable());
    assert!(!ErrorKind::Custom(99).is_retryable());
}

#[test]
fn error_kind_is_domain() {
    assert!(ErrorKind::NotFound.is_domain());
    assert!(ErrorKind::Conflict.is_domain());
    assert!(ErrorKind::Validation.is_domain());
    assert!(ErrorKind::PolicyRejection.is_domain());
    assert!(!ErrorKind::StorageError.is_domain());
    assert!(!ErrorKind::Timeout.is_domain());
    assert!(!ErrorKind::Internal.is_domain());
    assert!(!ErrorKind::Custom(99).is_domain());
}

#[test]
fn error_kind_is_operational() {
    assert!(ErrorKind::StorageError.is_operational());
    assert!(ErrorKind::Timeout.is_operational());
    assert!(ErrorKind::Serialization.is_operational());
    assert!(ErrorKind::Internal.is_operational());
    assert!(!ErrorKind::NotFound.is_operational());
    assert!(!ErrorKind::Conflict.is_operational());
    assert!(!ErrorKind::Custom(99).is_operational());
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
    assert!(s.contains("Conflict"), "Display should contain kind. Got: {s}");
    assert!(s.contains("double booking"), "Display should contain message. Got: {s}");
}

// ================================================================
// src/guard/ — GateSet helpers + Denial
// ================================================================

#[test]
fn gateset_len_and_is_empty() {
    let mut gates = GateSet::<()>::new();
    assert!(gates.is_empty());
    assert_eq!(gates.len(), 0);

    struct DummyGate;
    impl Gate<()> for DummyGate {
        fn name(&self) -> &'static str { "dummy" }
        fn evaluate(&self, _: &()) -> Result<(), Denial> { Ok(()) }
    }

    gates.push(DummyGate);
    assert!(!gates.is_empty());
    assert_eq!(gates.len(), 1);
}

#[test]
fn gateset_default() {
    let gates = GateSet::<()>::default();
    assert!(gates.is_empty());
}

#[test]
fn gate_description_default() {
    struct DescGate;
    impl Gate<()> for DescGate {
        fn name(&self) -> &'static str { "desc_gate" }
        fn evaluate(&self, _: &()) -> Result<(), Denial> { Ok(()) }
        // description() uses default impl
    }
    let gate = DescGate;
    assert_eq!(gate.description(), "",
        "Default Gate::description() should return empty string.");
}

#[test]
fn denial_serialize() {
    let denial = Denial::new("test_gate", "access denied")
        .with_code("403")
        .with_context("user", "alice");

    let json = serde_json::to_string(&denial).expect("Denial should serialize");
    assert!(json.contains("test_gate"), "Serialized Denial should contain gate. Got: {json}");
    assert!(json.contains("403"), "Serialized Denial should contain code. Got: {json}");
    assert!(json.contains("alice"));
}

#[test]
fn denial_is_error_trait() {
    let denial = Denial::new("g", "msg");
    // Verify it implements std::error::Error (this is a compile-time check + runtime use)
    let err: &dyn std::error::Error = &denial;
    let display = format!("{err}");
    assert!(display.contains("[g]") && display.contains("msg"),
        "Denial Error Display should be '[gate] message'. Got: {display}");
}

// ================================================================
// Outcome::flatten() — fixed abstraction level
// ================================================================

#[test]
fn flatten_unwraps_nested_ok() {
    let nested: Outcome<Outcome<i32>> = Outcome::Ok(Outcome::Ok(42));
    let flat = nested.flatten();
    assert_eq!(flat, Outcome::Ok(42),
        "flatten should unwrap Outcome<Outcome<T>> to Outcome<T>. \
         Investigate: src/outcome/mod.rs impl Outcome<Outcome<T>>.");
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
    assert!(flat.is_err(), "flatten should propagate outer Err.");
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
    assert!(flat.is_err(), "flatten should propagate inner Err.");
}

#[test]
fn flatten_distributes_over_batch() {
    let nested: Outcome<Outcome<i32>> = Outcome::Batch(vec![
        Outcome::Ok(Outcome::Ok(1)),
        Outcome::Ok(Outcome::Ok(2)),
    ]);
    let flat = nested.flatten();
    assert_eq!(flat, Outcome::Batch(vec![Outcome::Ok(1), Outcome::Ok(2)]));
}

// ================================================================
// Reactive<P> — SPEC-mandated subscribe→react→append pattern
// ================================================================

use free_batteries::store::{Store, StoreConfig};
use tempfile::TempDir;
use std::sync::Arc;

struct OrderReactor;
impl free_batteries::event::Reactive<serde_json::Value> for OrderReactor {
    fn react(&self, event: &Event<serde_json::Value>) -> Vec<(Coordinate, EventKind, serde_json::Value)> {
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
        ..StoreConfig::default()
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
        store_w.append(&coord_w, kind, &serde_json::json!({"item": "widget"}))
            .expect("append root")
    });
    let root_receipt = writer.join().expect("writer thread");

    // Receive the notification
    let rx = sub.receiver();
    let notif = rx.recv_timeout(std::time::Duration::from_secs(2))
        .expect("should receive notification");

    // React: the OrderReactor decides what to emit
    let reactor = OrderReactor;
    // Build a minimal event for the reactor (it only needs kind + event_id)
    let header = EventHeader::new(
        notif.event_id, notif.correlation_id, notif.causation_id,
        0, DagPosition::root(), 0, notif.kind,
    );
    let event = Event::<serde_json::Value>::new(header, serde_json::Value::Null);
    let reactions = reactor.react(&event);

    assert_eq!(reactions.len(), 1,
        "OrderReactor should produce 1 reaction. Investigate: Reactive<P> impl.");

    // Append reactions via append_reaction (the causal link)
    for (react_coord, react_kind, react_payload) in reactions {
        store.append_reaction(
            &react_coord, react_kind, &react_payload,
            root_receipt.event_id, root_receipt.event_id,
        ).expect("append reaction");
    }

    // Verify: 2 events total (root + reaction)
    let stats = store.stats();
    assert_eq!(stats.event_count, 2,
        "Should have root + reaction event. \
         Investigate: Reactive pattern glue in src/event/sourcing.rs.");

    store.sync().expect("sync");
}
