#![allow(clippy::panic, clippy::print_stderr, clippy::unwrap_used)]
//! Fuzz targets for all critical paths in batpak.
//! Uses proptest as a structured fuzzer with high iteration counts.
//! Every target exercises a boundary where untrusted/arbitrary data enters.
//!
//! PROVES: LAW-007 (Codebase Accuses Itself — fuzz as specification)
//! DEFENDS: FM-004 (Phantom Dependency), FM-006 (Version Chimera)
//! INVARIANTS: INV-TYPE (round-trip fidelity for all types), INV-SEC (EventKind packing)
//!
//! Run with: cargo test --test fuzz_targets --all-features
//! Deep fuzz: PROPTEST_CASES=100000 cargo test --test fuzz_targets --all-features --release

#[cfg(feature = "blake3")]
use batpak::event::hash::HashChain;
use batpak::outcome::wait::{CompensationAction, WaitCondition};
use batpak::prelude::*;
use batpak::store::segment::{frame_decode, frame_encode, SegmentHeader};
use proptest::prelude::*;
use proptest::strategy::BoxedStrategy;
mod common;

// ============================================================
// FUZZ TARGET 1: frame_decode — CRITICAL
// Parses [len:u32 BE][crc32:u32 BE][msgpack] from arbitrary bytes.
// Boundary: disk data, potentially corrupted.
// ============================================================

proptest! {
    #![proptest_config(common::proptest::cfg(32))]

    /// Fuzz frame_decode with completely random bytes.
    /// Must never panic, only return Ok or Err.
    #[test]
    fn fuzz_frame_decode_random_bytes(data in proptest::collection::vec(any::<u8>(), 0..1024)) {
        let _ = frame_decode(&data);
    }

    /// Fuzz frame_decode with bytes that have a valid-looking header but garbage body.
    #[test]
    fn fuzz_frame_decode_valid_header_garbage_body(
        len in 0u32..512,
        crc in any::<u32>(),
        body in proptest::collection::vec(any::<u8>(), 0..600),
    ) {
        let mut buf = Vec::with_capacity(8 + body.len());
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&crc.to_be_bytes());
        buf.extend_from_slice(&body);
        let _ = frame_decode(&buf);
    }

    /// Round-trip: frame_encode then frame_decode must produce identical msgpack.
    #[test]
    fn fuzz_frame_roundtrip(payload in any::<String>()) {
        let encoded = frame_encode(&payload).expect("encode");
        let (decoded_msgpack, consumed) = frame_decode(&encoded).expect("decode");
        prop_assert_eq!(consumed, encoded.len(),
            "FRAME ROUNDTRIP SIZE MISMATCH: consumed {} != encoded {}. \
             Investigate: src/store/segment.rs frame_encode/frame_decode.",
            consumed, encoded.len());
        // Deserialize back and check equality
        let decoded: String = rmp_serde::from_slice(decoded_msgpack).expect("deserialize");
        prop_assert_eq!(decoded, payload,
            "FRAME ROUNDTRIP DATA MISMATCH. Investigate: src/store/segment.rs");
    }

    /// frame_decode with truncated frames (header present, body cut short).
    #[test]
    fn fuzz_frame_decode_truncated(
        payload in "\\PC{1,100}",
        cut in 1usize..8,
    ) {
        let encoded = frame_encode(&payload).expect("encode");
        if encoded.len() > 8 + cut {
            let truncated = &encoded[..encoded.len() - cut];
            let result = frame_decode(truncated);
            prop_assert!(result.is_err(),
                "TRUNCATED FRAME ACCEPTED: frame_decode should reject truncated data. \
                 Investigate: src/store/segment.rs frame_decode length check.");
        }
    }

    /// frame_decode with CRC deliberately corrupted.
    #[test]
    fn fuzz_frame_decode_bad_crc(payload in "\\PC{1,50}") {
        let mut encoded = frame_encode(&payload).expect("encode");
        // Flip a bit in the CRC (bytes 4..8)
        if encoded.len() >= 8 {
            encoded[4] ^= 0xFF;
            let result = frame_decode(&encoded);
            prop_assert!(result.is_err(),
                "CRC BYPASS: frame_decode accepted data with corrupted CRC. \
                 Investigate: src/store/segment.rs CRC validation.");
        }
    }
}

// ============================================================
// FUZZ TARGET 2: Wire format u128 serialization
// Boundary: MessagePack deserialization of u128 fields.
// ============================================================

proptest! {
    #![proptest_config(common::proptest::cfg(32))]

    /// u128 round-trip through MessagePack.
    #[test]
    fn fuzz_u128_wire_roundtrip(val in any::<u128>()) {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Wrapper {
            #[serde(with = "batpak::wire::u128_bytes")]
            v: u128,
        }
        let w = Wrapper { v: val };
        let bytes = rmp_serde::to_vec_named(&w).expect("serialize");
        let decoded: Wrapper = rmp_serde::from_slice(&bytes).expect("deserialize");
        prop_assert_eq!(w, decoded,
            "U128 WIRE ROUNDTRIP FAILED for {}. \
             Investigate: src/wire.rs u128_bytes.", val);
    }

    /// Option<u128> round-trip.
    #[test]
    fn fuzz_option_u128_wire_roundtrip(val in proptest::option::of(any::<u128>())) {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Wrapper {
            #[serde(with = "batpak::wire::option_u128_bytes")]
            v: Option<u128>,
        }
        let w = Wrapper { v: val };
        let bytes = rmp_serde::to_vec_named(&w).expect("serialize");
        let decoded: Wrapper = rmp_serde::from_slice(&bytes).expect("deserialize");
        prop_assert_eq!(w, decoded,
            "OPTION_U128 WIRE ROUNDTRIP FAILED. Investigate: src/wire.rs option_u128_bytes.");
    }

    /// Vec<u128> round-trip.
    #[test]
    fn fuzz_vec_u128_wire_roundtrip(vals in proptest::collection::vec(any::<u128>(), 0..32)) {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Wrapper {
            #[serde(with = "batpak::wire::vec_u128_bytes")]
            v: Vec<u128>,
        }
        let w = Wrapper { v: vals };
        let bytes = rmp_serde::to_vec_named(&w).expect("serialize");
        let decoded: Wrapper = rmp_serde::from_slice(&bytes).expect("deserialize");
        prop_assert_eq!(w, decoded,
            "VEC_U128 WIRE ROUNDTRIP FAILED. Investigate: src/wire.rs vec_u128_bytes.");
    }

    /// Fuzz u128 deserialization with random bytes (must not panic).
    #[test]
    fn fuzz_u128_deser_random(data in proptest::collection::vec(any::<u8>(), 0..128)) {
        #[derive(serde::Deserialize)]
        struct Wrapper {
            #[serde(with = "batpak::wire::u128_bytes")]
            _v: u128,
        }
        // Must not panic, only Ok or Err
        let _ = rmp_serde::from_slice::<Wrapper>(&data);
    }
}

// ============================================================
// FUZZ TARGET 3: EventKind bit-packing
// Boundary: category:type encoding/decoding.
// ============================================================

proptest! {
    #![proptest_config(common::proptest::cfg(2048))]

    /// EventKind custom() round-trips through category()/type_id().
    /// Categories 0x0 (system) and 0xD (effect) are reserved — filter them out.
    #[test]
    fn fuzz_event_kind_roundtrip(cat in 0u8..16, type_id in 0u16..4096) {
        prop_assume!(cat != 0 && cat != 0xD);
        let kind = EventKind::custom(cat, type_id);
        prop_assert_eq!(kind.category(), cat,
            "EVENTKIND CATEGORY MISMATCH: custom({}, {}).category() = {} != {}. \
             Investigate: src/event/kind.rs bit-packing.",
            cat, type_id, kind.category(), cat);
        prop_assert_eq!(kind.type_id(), type_id,
            "EVENTKIND TYPE_ID MISMATCH: custom({}, {}).type_id() = {} != {}. \
             Investigate: src/event/kind.rs bit-packing.",
            cat, type_id, kind.type_id(), type_id);
    }

    /// EventKind serde round-trip.
    /// Categories 0x0 (system) and 0xD (effect) are reserved — filter them out.
    #[test]
    fn fuzz_event_kind_serde(cat in 0u8..16, type_id in 0u16..4096) {
        prop_assume!(cat != 0 && cat != 0xD);
        let kind = EventKind::custom(cat, type_id);
        let bytes = rmp_serde::to_vec_named(&kind).expect("serialize");
        let decoded: EventKind = rmp_serde::from_slice(&bytes).expect("deserialize");
        prop_assert_eq!(kind, decoded,
            "EVENTKIND SERDE ROUNDTRIP FAILED. Investigate: src/event/kind.rs Serialize/Deserialize.");
    }

    /// EventKind with overflow type_id — lower 12 bits should be masked.
    /// Categories 0x0 (system) and 0xD (effect) are reserved — filter them out.
    #[test]
    fn fuzz_event_kind_overflow(cat in 0u8..16, raw_type in 0u16..=u16::MAX) {
        prop_assume!(cat != 0 && cat != 0xD);
        let kind = EventKind::custom(cat, raw_type);
        // type_id must be masked to 12 bits
        prop_assert_eq!(kind.type_id(), raw_type & 0x0FFF,
            "EVENTKIND OVERFLOW: type_id should mask to 12 bits. \
             Investigate: src/event/kind.rs custom().");
    }
}

// ============================================================
// FUZZ TARGET 4: Coordinate validation
// Boundary: user-provided entity/scope strings.
// ============================================================

proptest! {
    #![proptest_config(common::proptest::cfg(32))]

    /// Coordinate::new rejects empty strings, accepts non-empty.
    #[test]
    fn fuzz_coordinate_validation(entity in "\\PC{0,50}", scope in "\\PC{0,50}") {
        let result = Coordinate::new(&entity, &scope);
        if entity.is_empty() {
            prop_assert!(result.is_err(),
                "COORDINATE EMPTY ENTITY ACCEPTED: Coordinate::new must reject empty entity string.\n\
                 Investigate: src/coordinate/mod.rs Coordinate::new validation.\n\
                 Common causes: missing empty-string guard, off-by-one in length check.\n\
                 Run: cargo test --test fuzz_targets");
        } else if scope.is_empty() {
            prop_assert!(result.is_err(),
                "COORDINATE EMPTY SCOPE ACCEPTED: Coordinate::new must reject empty scope string.\n\
                 Investigate: src/coordinate/mod.rs Coordinate::new validation.\n\
                 Common causes: missing empty-string guard on scope field.\n\
                 Run: cargo test --test fuzz_targets");
        } else {
            prop_assert!(result.is_ok(),
                "COORDINATE REJECTED VALID INPUT: entity={entity:?}, scope={scope:?}. \
                 Investigate: src/coordinate/mod.rs Coordinate::new.");
            let coord = result.expect("valid");
            prop_assert_eq!(coord.entity(), entity.as_str(),
                "COORDINATE ENTITY MISMATCH: stored entity {:?} != input {:?}.\n\
                 Investigate: src/coordinate/mod.rs Coordinate::new entity storage.\n\
                 Common causes: normalization or trimming applied unexpectedly.\n\
                 Run: cargo test --test fuzz_targets",
                coord.entity(), entity.as_str());
            prop_assert_eq!(coord.scope(), scope.as_str(),
                "COORDINATE SCOPE MISMATCH: stored scope {:?} != input {:?}.\n\
                 Investigate: src/coordinate/mod.rs Coordinate::new scope storage.\n\
                 Common causes: normalization or trimming applied unexpectedly.\n\
                 Run: cargo test --test fuzz_targets",
                coord.scope(), scope.as_str());
        }
    }

    /// Coordinate serde round-trip.
    #[test]
    fn fuzz_coordinate_serde(
        entity in "[a-z][a-z0-9:_]{0,20}",
        scope in "[a-z][a-z0-9:_]{0,20}",
    ) {
        let coord = Coordinate::new(&entity, &scope).expect("valid");
        let bytes = rmp_serde::to_vec_named(&coord).expect("serialize");
        let decoded: Coordinate = rmp_serde::from_slice(&bytes).expect("deserialize");
        prop_assert_eq!(coord, decoded,
            "COORDINATE SERDE ROUNDTRIP FAILED. Investigate: src/coordinate/mod.rs.");
    }

    /// Region::matches_event with arbitrary inputs (must never panic).
    /// Categories 0x0 (system) and 0xD (effect) are reserved — filter them out.
    #[test]
    fn fuzz_region_matches_event(
        prefix in "\\PC{0,10}",
        scope in "\\PC{0,10}",
        entity in "\\PC{1,20}",
        event_scope in "\\PC{1,20}",
        cat in 0u8..16,
        type_id in 0u16..4096,
    ) {
        prop_assume!(cat != 0 && cat != 0xD);
        let region = Region::entity(&prefix).with_scope(&scope);
        let kind = EventKind::custom(cat, type_id);
        // Must never panic
        let _ = region.matches_event(&entity, &event_scope, kind);
    }
}

// ============================================================
// FUZZ TARGET 5: DagPosition ordering
// Boundary: causal ordering correctness under arbitrary positions.
// ============================================================

proptest! {
    #![proptest_config(common::proptest::cfg(2048))]

    /// DagPosition::is_ancestor_of is anti-reflexive.
    #[test]
    fn fuzz_dag_position_not_self_ancestor(d in any::<u32>(), l in any::<u32>(), s in any::<u32>()) {
        let pos = DagPosition::new(d, l, s);
        prop_assert!(!pos.is_ancestor_of(&pos),
            "DAG POSITION SELF-ANCESTOR: position should not be its own ancestor. \
             Investigate: src/coordinate/position.rs is_ancestor_of.");
    }

    /// DagPosition partial_cmp: same lane = comparable, different lane = None.
    #[test]
    fn fuzz_dag_position_ordering(
        d1 in any::<u32>(), l1 in any::<u32>(), s1 in any::<u32>(),
        d2 in any::<u32>(), l2 in any::<u32>(), s2 in any::<u32>(),
    ) {
        let a = DagPosition::new(d1, l1, s1);
        let b = DagPosition::new(d2, l2, s2);
        if l1 != l2 {
            prop_assert_eq!(a.partial_cmp(&b), None,
                "DAG POSITION CROSS-LANE COMPARABLE: different lanes must be incomparable. \
                 Investigate: src/coordinate/position.rs PartialOrd.");
        } else {
            prop_assert!(a.partial_cmp(&b).is_some(),
                "DAG POSITION SAME-LANE INCOMPARABLE: same lane must be comparable.");
        }
    }

    /// DagPosition serde round-trip.
    #[test]
    fn fuzz_dag_position_serde(d in any::<u32>(), l in any::<u32>(), s in any::<u32>()) {
        let pos = DagPosition::new(d, l, s);
        let bytes = rmp_serde::to_vec_named(&pos).expect("serialize");
        let decoded: DagPosition = rmp_serde::from_slice(&bytes).expect("deserialize");
        prop_assert_eq!(pos, decoded,
            "DAGPOSITION SERDE ROUNDTRIP FAILED: decoded position does not match original.\n\
             Investigate: src/coordinate/position.rs DagPosition Serialize/Deserialize.\n\
             Common causes: field ordering mismatch, missing u32 wire encoding, serde rename.\n\
             Run: cargo test --test fuzz_targets");
    }
}

// ============================================================
// FUZZ TARGET 6: Outcome combinators
// Extends monad_laws.rs with deeper fuzzing of edge cases.
// ============================================================

fn arb_outcome_deep() -> impl Strategy<Value = Outcome<i32>> {
    let leaf = prop_oneof![
        any::<i32>().prop_map(Outcome::Ok),
        any::<String>().prop_map(|msg| Outcome::Err(OutcomeError {
            kind: ErrorKind::Internal,
            message: msg,
            compensation: None,
            retryable: false,
        })),
        (any::<u64>(), any::<u32>(), any::<u32>(), any::<String>()).prop_map(
            |(after, attempt, max, reason)| Outcome::Retry {
                after_ms: after,
                attempt,
                max_attempts: max,
                reason,
            }
        ),
        any::<String>().prop_map(|reason| Outcome::Cancelled { reason }),
    ];
    leaf.prop_recursive(3, 32, 8, |inner| {
        proptest::collection::vec(inner, 0..6).prop_map(Outcome::Batch)
    })
}

proptest! {
    #![proptest_config(common::proptest::cfg(32))]

    /// Deep nested Batch: map(id) == id (functor identity, recursive).
    #[test]
    fn fuzz_outcome_deep_functor_identity(m in arb_outcome_deep()) {
        let original = m.clone();
        let mapped = m.map(|x| x);
        prop_assert_eq!(mapped, original,
            "DEEP FUNCTOR IDENTITY VIOLATED. Investigate: src/outcome/mod.rs map Batch recursion.");
    }

    /// Deep nested Batch: and_then(Ok) == id (right identity, recursive).
    #[test]
    fn fuzz_outcome_deep_right_identity(m in arb_outcome_deep()) {
        let original = m.clone();
        let result = m.and_then(Outcome::Ok);
        prop_assert_eq!(result, original,
            "DEEP RIGHT IDENTITY VIOLATED. Investigate: src/outcome/mod.rs and_then Batch recursion.");
    }

    /// zip with deeply nested Outcomes (must never panic).
    #[test]
    fn fuzz_outcome_zip_deep(a in arb_outcome_deep(), b in arb_outcome_deep()) {
        let _ = batpak::outcome::zip(a, b);
    }

    /// join_all with mixed deep outcomes.
    #[test]
    fn fuzz_outcome_join_all_deep(
        outcomes in proptest::collection::vec(arb_outcome_deep(), 0..8)
    ) {
        let _ = batpak::outcome::join_all(outcomes);
    }

    /// join_any with mixed deep outcomes.
    #[test]
    fn fuzz_outcome_join_any_deep(
        outcomes in proptest::collection::vec(arb_outcome_deep(), 0..8)
    ) {
        let _ = batpak::outcome::join_any(outcomes);
    }

    /// Outcome serde round-trip with deep nesting.
    #[test]
    fn fuzz_outcome_serde_deep(m in arb_outcome_deep()) {
        let bytes = rmp_serde::to_vec_named(&m).expect("serialize");
        let decoded: Outcome<i32> = rmp_serde::from_slice(&bytes).expect("deserialize");
        prop_assert_eq!(m, decoded,
            "OUTCOME SERDE ROUNDTRIP FAILED. Investigate: src/outcome/mod.rs Serialize/Deserialize.");
    }
}

// ============================================================
// FUZZ TARGET 7: EventHeader full round-trip
// ============================================================

proptest! {
    #![proptest_config(common::proptest::cfg(512))]

    #[test]
    fn fuzz_event_header_serde(
        event_id in any::<u128>(),
        corr_id in any::<u128>(),
        caus_id in proptest::option::of(any::<u128>()),
        ts in any::<i64>(),
        depth in any::<u32>(),
        lane in any::<u32>(),
        seq in any::<u32>(),
        payload_size in any::<u32>(),
        cat in 0u8..16,
        type_id in 0u16..4096,
        flags in any::<u8>(),
    ) {
        // Categories 0x0 (system) and 0xD (effect) are reserved
        prop_assume!(cat != 0 && cat != 0xD);
        let header = EventHeader::new(
            event_id, corr_id, caus_id, ts,
            DagPosition::new(depth, lane, seq),
            payload_size,
            EventKind::custom(cat, type_id),
        ).with_flags(flags);

        let bytes = rmp_serde::to_vec_named(&header).expect("serialize");
        let decoded: EventHeader = rmp_serde::from_slice(&bytes).expect("deserialize");
        prop_assert_eq!(header, decoded,
            "EVENTHEADER SERDE ROUNDTRIP FAILED. \
             Investigate: src/event/header.rs + src/wire.rs u128 visitors.");
    }
}

// ============================================================
// FUZZ TARGET 8: SegmentHeader serde
// ============================================================

proptest! {
    #![proptest_config(common::proptest::cfg(512))]

    #[test]
    fn fuzz_segment_header_serde(
        version in any::<u16>(),
        flags in any::<u16>(),
        created_ns in any::<i64>(),
        segment_id in any::<u64>(),
    ) {
        let header = SegmentHeader { version, flags, created_ns, segment_id };
        let bytes = rmp_serde::to_vec_named(&header).expect("serialize");
        let decoded: SegmentHeader = rmp_serde::from_slice(&bytes).expect("deserialize");
        prop_assert_eq!(header.version, decoded.version,
            "SEGMENTHEADER VERSION MISMATCH: {} != {} after serde roundtrip.\n\
             Investigate: src/store/segment.rs SegmentHeader Serialize/Deserialize.\n\
             Common causes: field renamed/skipped in serde attrs, version field type mismatch.\n\
             Run: cargo test --test fuzz_targets",
            header.version, decoded.version);
        prop_assert_eq!(header.flags, decoded.flags,
            "SEGMENTHEADER FLAGS MISMATCH: {} != {} after serde roundtrip.\n\
             Investigate: src/store/segment.rs SegmentHeader Serialize/Deserialize.\n\
             Common causes: flags field skipped or default-overridden during deserialization.\n\
             Run: cargo test --test fuzz_targets",
            header.flags, decoded.flags);
        prop_assert_eq!(header.created_ns, decoded.created_ns,
            "SEGMENTHEADER CREATED_NS MISMATCH: {} != {} after serde roundtrip.\n\
             Investigate: src/store/segment.rs SegmentHeader Serialize/Deserialize.\n\
             Common causes: i64 sign-extension bug, timestamp field lost in wire encoding.\n\
             Run: cargo test --test fuzz_targets",
            header.created_ns, decoded.created_ns);
        prop_assert_eq!(header.segment_id, decoded.segment_id,
            "SEGMENTHEADER SEGMENT_ID MISMATCH: {} != {} after serde roundtrip.\n\
             Investigate: src/store/segment.rs SegmentHeader Serialize/Deserialize.\n\
             Common causes: u64 truncated to u32 in wire format, field ordering error.\n\
             Run: cargo test --test fuzz_targets",
            header.segment_id, decoded.segment_id);
    }
}

// ============================================================
// FUZZ TARGET 9: HashChain serde + verification
// ============================================================

#[cfg(feature = "blake3")]
proptest! {
    #![proptest_config(common::proptest::cfg(32))]

    /// Multi-event hash chain: build N events, verify entire chain.
    #[test]
    fn fuzz_hash_chain_multi_event(
        payloads in proptest::collection::vec(
            proptest::collection::vec(any::<u8>(), 1..64),
            2..16
        )
    ) {
        use batpak::event::hash::{compute_hash, verify_chain};

        let mut prev_hash = [0u8; 32]; // genesis
        let mut chains = Vec::new();

        for payload in &payloads {
            let event_hash = compute_hash(payload);
            let chain = HashChain { prev_hash, event_hash };
            chains.push((payload.clone(), chain.clone()));
            prev_hash = event_hash;
        }

        // Verify the entire chain forwards
        let mut expected_prev = [0u8; 32];
        for (payload, chain) in &chains {
            prop_assert!(verify_chain(payload, chain, &expected_prev),
                "MULTI-EVENT CHAIN VERIFICATION FAILED at position. \
                 Investigate: src/event/hash.rs verify_chain.");
            expected_prev = chain.event_hash;
        }
    }

    /// Hash chain serde round-trip.
    #[test]
    fn fuzz_hash_chain_serde(
        prev in proptest::collection::vec(any::<u8>(), 32..=32),
        event in proptest::collection::vec(any::<u8>(), 32..=32),
    ) {
        let mut prev_arr = [0u8; 32];
        let mut event_arr = [0u8; 32];
        prev_arr.copy_from_slice(&prev);
        event_arr.copy_from_slice(&event);
        let chain = HashChain { prev_hash: prev_arr, event_hash: event_arr };
        let bytes = rmp_serde::to_vec_named(&chain).expect("serialize");
        let decoded: HashChain = rmp_serde::from_slice(&bytes).expect("deserialize");
        prop_assert_eq!(chain, decoded,
            "HASHCHAIN SERDE ROUNDTRIP FAILED: decoded HashChain does not match original.\n\
             Investigate: src/event/hash.rs HashChain Serialize/Deserialize.\n\
             Common causes: fixed-size [u8;32] arrays not preserved by serde, byte slice length mismatch.\n\
             Run: cargo test --test fuzz_targets");
    }
}

// ============================================================
// FUZZ TARGET 10: Wire.rs uncovered visitor paths
// INVARIANTS: INV-TYPE (wire format totality — all visitor paths exercised)
// ============================================================

/// Explicit test for Option<u128> None round-trip through msgpack.
/// This exercises the visit_none path in option_u128_bytes.
#[test]
fn fuzz_option_u128_none_explicit() {
    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
    struct Wrapper {
        #[serde(with = "batpak::wire::option_u128_bytes")]
        v: Option<u128>,
    }
    let w = Wrapper { v: None };
    let bytes = rmp_serde::to_vec_named(&w).expect("serialize None");
    let decoded: Wrapper = rmp_serde::from_slice(&bytes).expect("deserialize None");
    assert_eq!(
        w, decoded,
        "OPTION_U128 NONE ROUNDTRIP FAILED: None must survive msgpack roundtrip.\n\
         Investigate: src/wire.rs option_u128_bytes visit_none.\n\
         Run: cargo test --test fuzz_targets fuzz_option_u128_none_explicit"
    );
}

/// u128 round-trip through serde_json, which uses visit_seq (not visit_bytes).
/// This exercises the sequence-based visitor path in u128_bytes.
#[test]
fn fuzz_u128_json_roundtrip() {
    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
    struct Wrapper {
        #[serde(with = "batpak::wire::u128_bytes")]
        v: u128,
    }
    for val in [0u128, 1, u128::MAX, u128::MAX / 2, 42, 0xDEADBEEF_CAFEBABE] {
        let w = Wrapper { v: val };
        let json = serde_json::to_string(&w).expect("serialize to json");
        let decoded: Wrapper = serde_json::from_str(&json).expect("deserialize from json");
        assert_eq!(
            w, decoded,
            "U128 JSON ROUNDTRIP FAILED for {val}: visit_seq path must handle JSON arrays.\n\
             Investigate: src/wire.rs u128_bytes visit_seq.\n\
             Run: cargo test --test fuzz_targets fuzz_u128_json_roundtrip"
        );
    }
}

proptest! {
    #![proptest_config(common::proptest::cfg(256))]

    /// Feed JSON arrays of wrong lengths to u128_bytes deserializer (visit_seq) — must error, not panic.
    #[test]
    fn fuzz_u128_malformed_length(len in 0usize..33) {
        prop_assume!(len != 16); // 16 is the valid length
        // Build a JSON array with `len` u8 values, feed to deserializer via visit_seq path
        let arr: Vec<u8> = vec![0xABu8; len];
        let json = format!("{{\"_v\":{}}}", serde_json::to_string(&arr).expect("json array"));
        #[derive(serde::Deserialize)]
        struct Wrapper {
            #[serde(with = "batpak::wire::u128_bytes")]
            _v: u128,
        }
        let result = serde_json::from_str::<Wrapper>(&json);
        prop_assert!(result.is_err(),
            "U128 MALFORMED LENGTH ACCEPTED: {} bytes should be rejected (expected 16).\n\
             Investigate: src/wire.rs u128_bytes visit_seq length check.\n\
             Run: cargo test --test fuzz_targets fuzz_u128_malformed_length", len);
    }
}

// ============================================================
// FUZZ TARGET 11: WaitCondition + CompensationAction serde
// Recursive enum types — test deeply nested structures.
// ============================================================

fn arb_wait_condition() -> impl Strategy<Value = WaitCondition> {
    let leaf = prop_oneof![
        any::<u64>().prop_map(|ms| WaitCondition::Timeout { resume_at_ms: ms }),
        any::<u128>().prop_map(|id| WaitCondition::Event { event_id: id }),
        (any::<u16>(), proptest::collection::vec(any::<u8>(), 0..16))
            .prop_map(|(tag, data)| WaitCondition::Custom { tag, data }),
    ];
    leaf.prop_recursive(3, 32, 6, |inner: BoxedStrategy<WaitCondition>| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(WaitCondition::All),
            proptest::collection::vec(inner, 0..4).prop_map(WaitCondition::Any),
        ]
    })
}

proptest! {
    #![proptest_config(common::proptest::cfg(512))]

    #[test]
    fn fuzz_wait_condition_serde(wc in arb_wait_condition()) {
        let bytes = rmp_serde::to_vec_named(&wc).expect("serialize");
        let decoded: WaitCondition = rmp_serde::from_slice(&bytes).expect("deserialize");
        prop_assert_eq!(wc, decoded,
            "WAITCONDITION SERDE ROUNDTRIP FAILED. Investigate: src/outcome/wait.rs.");
    }

    #[test]
    fn fuzz_compensation_action_serde(
        variant in 0u8..4,
        ids in proptest::collection::vec(any::<u128>(), 0..8),
        msg in any::<String>(),
        data in proptest::collection::vec(any::<u8>(), 0..32),
    ) {
        let action = match variant % 4 {
            0 => CompensationAction::Rollback { event_ids: ids },
            1 => CompensationAction::Notify {
                target_id: ids.first().copied().unwrap_or(0),
                message: msg,
            },
            2 => CompensationAction::Release { resource_ids: ids },
            _ => CompensationAction::Custom {
                action_type: msg,
                data,
            },
        };
        let bytes = rmp_serde::to_vec_named(&action).expect("serialize");
        let decoded: CompensationAction = rmp_serde::from_slice(&bytes).expect("deserialize");
        prop_assert_eq!(action, decoded,
            "COMPENSATIONACTION SERDE ROUNDTRIP FAILED. Investigate: src/outcome/wait.rs.");
    }
}

// ============================================================
// FUZZ TARGET 9: BatchAppendItem payload bounds
// ============================================================

use batpak::store::{BatchAppendItem, CausationRef};

proptest! {
    #![proptest_config(common::proptest::cfg(256))]

    #[test]
    fn fuzz_batch_item_new_preserves_fields(
        entity in "[a-zA-Z0-9_:]{1,50}",
        scope in "[a-zA-Z0-9_:]{1,50}",
        payload in proptest::collection::vec(any::<u8>(), 0..1024),
        correlation_id in proptest::option::of(any::<u128>()),
        causation_id in proptest::option::of(any::<u128>()),
        flags in any::<u8>(),
        variant in 0u8..3,
        absolute_id in any::<u128>(),
        prior_index in 0usize..64,
    ) {
        let coord = Coordinate::new(&entity, &scope).expect("valid coordinate");
        let value = serde_json::Value::Array(
            payload.iter().copied().map(serde_json::Value::from).collect()
        );
        let options = AppendOptions {
            correlation_id,
            causation_id,
            flags,
            ..AppendOptions::default()
        };
        let causation = match variant {
            0 => CausationRef::None,
            1 => CausationRef::PriorItem(prior_index),
            _ => CausationRef::Absolute(absolute_id),
        };
        let item = BatchAppendItem::new(
            coord.clone(),
            EventKind::DATA,
            &value,
            options,
            causation,
        )
        .expect("construct BatchAppendItem");

        let decoded: serde_json::Value =
            rmp_serde::from_slice(&item.payload_bytes).expect("decode payload bytes");

        prop_assert_eq!(item.coord, coord,
            "BATCH ITEM COORD MISMATCH. Investigate: src/store/contracts.rs BatchAppendItem::new.");
        prop_assert_eq!(item.kind, EventKind::DATA,
            "BATCH ITEM KIND MISMATCH. Investigate: src/store/contracts.rs BatchAppendItem::new.");
        prop_assert_eq!(item.options.correlation_id, correlation_id,
            "BATCH ITEM CORRELATION MISMATCH. Investigate: src/store/contracts.rs AppendOptions preservation.");
        prop_assert_eq!(item.options.causation_id, causation_id,
            "BATCH ITEM CAUSATION OPTION MISMATCH. Investigate: src/store/contracts.rs AppendOptions preservation.");
        prop_assert_eq!(item.options.flags, flags,
            "BATCH ITEM FLAGS MISMATCH. Investigate: src/store/contracts.rs AppendOptions preservation.");
        prop_assert_eq!(item.causation, causation,
            "BATCH ITEM CAUSATION REF MISMATCH. Investigate: src/store/contracts.rs CausationRef preservation.");
        prop_assert_eq!(decoded, value,
            "BATCH ITEM PAYLOAD ROUNDTRIP FAILED. Investigate: src/store/contracts.rs BatchAppendItem::new payload encoding.");
    }

    #[test]
    fn fuzz_batch_varying_item_count(
        item_count in 1usize..50,
        payload_size in 10usize..500,
    ) {
        let coord = Coordinate::new("fuzz", "batch").expect("valid");
        let items: Vec<_> = (0..item_count)
            .map(|i| {
                let value = serde_json::Value::Array(
                    std::iter::repeat_n(serde_json::Value::from(i as u64), payload_size)
                        .collect()
                );
                BatchAppendItem::new(
                    coord.clone(),
                    EventKind::DATA,
                    &value,
                    AppendOptions::default(),
                    CausationRef::None,
                )
                .expect("construct batch item")
            })
            .collect();

        prop_assert_eq!(items.len(), item_count,
            "BATCH ITEM COUNT MISMATCH. Investigate: src/store/contracts.rs batch item construction.");
        prop_assert!(items.iter().all(|item| item.coord == coord),
            "BATCH ITEM COORD DRIFT. Investigate: src/store/contracts.rs BatchAppendItem construction.");
        prop_assert!(items.iter().all(|item| item.kind == EventKind::DATA),
            "BATCH ITEM KIND DRIFT. Investigate: src/store/contracts.rs BatchAppendItem construction.");
        prop_assert!(items.iter().all(|item| !item.payload_bytes.is_empty()),
            "BATCH ITEM PAYLOAD BYTES SHOULD NOT BE EMPTY FOR NON-EMPTY JSON ARRAYS.");
    }

    #[test]
    fn fuzz_causation_ref_current_variants(variant in 0u8..3, index in 0usize..100, event_id in any::<u128>()) {
        let causation = match variant {
            0 => CausationRef::None,
            1 => CausationRef::PriorItem(index),
            _ => CausationRef::Absolute(event_id),
        };

        let copied = causation;
        prop_assert_eq!(copied, causation,
            "CAUSATION REF COPY/EQ FAILED. Investigate: src/store/contracts.rs CausationRef.");
    }
}
