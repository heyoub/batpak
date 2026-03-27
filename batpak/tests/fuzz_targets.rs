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
//! [SPEC:tests/fuzz_targets.rs]

#[cfg(feature = "blake3")]
use batpak::event::hash::HashChain;
use batpak::outcome::wait::{CompensationAction, WaitCondition};
use batpak::prelude::*;
use batpak::store::segment::{frame_decode, frame_encode, SegmentHeader};
use proptest::prelude::*;
use proptest::strategy::BoxedStrategy;

// ============================================================
// FUZZ TARGET 1: frame_decode — CRITICAL
// Parses [len:u32 BE][crc32:u32 BE][msgpack] from arbitrary bytes.
// Boundary: disk data, potentially corrupted.
// ============================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(
        std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(32)
    ))]

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
    #![proptest_config(ProptestConfig::with_cases(
        std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(32)
    ))]

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
    #![proptest_config(ProptestConfig::with_cases(
        std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2048)
    ))]

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
    #![proptest_config(ProptestConfig::with_cases(
        std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(32)
    ))]

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
    #![proptest_config(ProptestConfig::with_cases(
        std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2048)
    ))]

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
    #![proptest_config(ProptestConfig::with_cases(
        std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(32)
    ))]

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
    #![proptest_config(ProptestConfig::with_cases(
        std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(512)
    ))]

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
    #![proptest_config(ProptestConfig::with_cases(
        std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(512)
    ))]

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
    #![proptest_config(ProptestConfig::with_cases(
        std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(32)
    ))]

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
// FUZZ TARGET 10: WaitCondition + CompensationAction serde
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
    #![proptest_config(ProptestConfig::with_cases(
        std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(512)
    ))]

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
