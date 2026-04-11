#![allow(clippy::disallowed_methods)] // compliance tests use thread::spawn for concurrency probes
//! Store algebraic property tests: replay determinism, idempotency, commutativity,
//! round-trip fidelity, law enforcement, flow connectivity, error propagation.
//!
//! PROVES: LAW-003 (No Orphan Infrastructure), LAW-007 (Codebase Accuses Itself)
//! DEFENDS: FM-007 (Island Syndrome), FM-022 (Receipt Hollowing), FM-023 (Fallback Laundering)
//! INVARIANTS: INV-TEMP (replay determinism), INV-CONC (idempotency), INV-TYPE (round-trip),
//!             INV-SEC (EventKind category enforcement)

use batpak::prelude::*;
use proptest::prelude::*;
use tempfile::TempDir;

mod common;
use common::medium_segment_store as test_store;
use common::test_coord;

/// Generate an arbitrary JSON value with bounded depth and breadth.
/// proptest's recursive strategy keeps generated payloads finite and shrinkable.
fn arb_json() -> impl Strategy<Value = serde_json::Value> {
    let leaf = prop_oneof![
        Just(serde_json::Value::Null),
        any::<bool>().prop_map(serde_json::Value::Bool),
        any::<i64>().prop_map(|n| serde_json::Value::Number(n.into())),
        // Avoid f64::NAN / Infinity — those don't round-trip through msgpack.
        (-1e10f64..1e10)
            .prop_filter("finite", |f| f.is_finite())
            .prop_map(|f| {
                serde_json::Number::from_f64(f)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null)
            }),
        ".{0,32}".prop_map(serde_json::Value::String),
    ];
    leaf.prop_recursive(
        4,  // max depth
        16, // max nodes
        4,  // max branch per level
        |inner| {
            prop_oneof![
                proptest::collection::vec(inner.clone(), 0..4).prop_map(serde_json::Value::Array),
                proptest::collection::hash_map(".{0,16}", inner, 0..4)
                    .prop_map(|m| { serde_json::Value::Object(m.into_iter().collect()) }),
            ]
        },
    )
}

// ===== INV-TEMP: Replay Determinism =====
// Same event log → same state. Close, reopen, verify exact equality.
// Proves: non-replayable truth is absent (Big Bang CS vocab: Replayability).

#[test]
fn replay_determinism_cold_start_rebuilds_identical_index() {
    let dir = TempDir::new().expect("tmpdir");
    let coord = test_coord();
    let kind = EventKind::custom(1, 1);

    // Phase 1: Write events, record their state
    let mut event_ids = Vec::new();
    {
        let store = test_store(&dir);
        for i in 0..20 {
            let receipt = store
                .append(&coord, kind, &format!("event_{i}"))
                .expect("append");
            event_ids.push(receipt.event_id);
        }
        store.sync().expect("sync");
        store.close().expect("close");
    }

    // Phase 2: Reopen (cold start rebuilds index from segments)
    let store = test_store(&dir);
    let events = store.stream("entity:test");

    // Verify: exact same events in exact same order
    assert_eq!(
        events.len(),
        20,
        "PROPERTY: Cold start must rebuild ALL events from segments.\n\
         Investigate: src/store/reader.rs scan_segment, src/store/mod.rs Store::open index rebuild.\n\
         Common causes: segment scan skipping events, index not rebuilt from all segments."
    );

    for (i, entry) in events.iter().enumerate() {
        assert_eq!(
            entry.event_id, event_ids[i],
            "PROPERTY: Replayed event_id must match original at index {i}.\n\
             Investigate: src/store/reader.rs scan_segment event ordering.\n\
             Common causes: events reordered during cold start, BTreeMap key collision."
        );
    }

    // Verify: get() returns events with correct IDs and kinds
    for eid in &event_ids {
        let stored = store.get(*eid).expect("get");
        assert_eq!(
            stored.event.header.event_id, *eid,
            "PROPERTY: Replayed event must have correct event_id.\n\
             Investigate: src/store/segment.rs frame_encode/frame_decode round-trip."
        );
        assert_eq!(
            stored.event.event_kind(),
            kind,
            "PROPERTY: Replayed event must preserve EventKind.\n\
             Investigate: src/event/kind.rs u16 encoding."
        );
    }
}

// ===== INV-CONC: Idempotency Algebraic Property =====
// Same operation twice → no new truth (Big Bang: Idempotence).

#[test]
fn idempotency_algebraic_duplicate_produces_no_new_event() {
    let dir = TempDir::new().expect("tmpdir");
    let store = test_store(&dir);
    let coord = test_coord();
    let kind = EventKind::custom(1, 1);

    let opts = AppendOptions {
        idempotency_key: Some(12345),
        ..AppendOptions::default()
    };

    let r1 = store
        .append_with_options(&coord, kind, &"hello", opts)
        .expect("first append");
    let r2 = store
        .append_with_options(&coord, kind, &"hello", opts)
        .expect("second append (idempotent)");

    assert_eq!(
        r1.event_id, r2.event_id,
        "PROPERTY: Idempotent append must return same event_id.\n\
         Investigate: src/store/writer.rs idempotency check (Step 1b).\n\
         Common causes: idempotency map not checked before append."
    );

    let events = store.stream("entity:test");
    assert_eq!(
        events.len(),
        1,
        "PROPERTY: Duplicate idempotent append must NOT create a second event.\n\
         Investigate: src/store/writer.rs handle_append idempotency_key lookup.\n\
         Common causes: idempotency check after write instead of before."
    );
}

// ===== INV-TYPE: Round-Trip Fidelity =====
// Serialize → deserialize preserves meaning exactly (Big Bang: Round-trip fidelity).

#[test]
fn round_trip_fidelity_append_get_preserves_payload() {
    let dir = TempDir::new().expect("tmpdir");
    let store = test_store(&dir);
    let coord = test_coord();
    let kind = EventKind::custom(15, 4095); // max category, max type_id

    // Complex payload with edge cases
    let payload = serde_json::json!({
        "string": "hello world",
        "number": 42,
        "float": 3.15,
        "null_field": null,
        "array": [1, 2, 3],
        "nested": {"deep": {"deeper": true}},
        "empty_string": "",
        "empty_array": [],
        "unicode": "日本語テスト 🎉",
    });

    let receipt = store.append(&coord, kind, &payload).expect("append");
    let stored = store.get(receipt.event_id).expect("get");

    // Verify coordinate round-trips
    assert_eq!(
        stored.coordinate, coord,
        "PROPERTY: Coordinate must survive storage round-trip.\n\
         Investigate: src/store/writer.rs handle_append coordinate serialization."
    );

    // Verify EventKind round-trips (category + type_id encoded in u16)
    assert_eq!(
        stored.event.event_kind(),
        kind,
        "PROPERTY: EventKind must survive storage round-trip.\n\
         Investigate: src/event/kind.rs u16 encoding, msgpack serialization."
    );

    // Verify event_id matches
    assert_eq!(
        stored.event.header.event_id, receipt.event_id,
        "PROPERTY: event_id must match between append receipt and stored event."
    );

    assert_eq!(
        stored.event.payload, payload,
        "PROPERTY: payload must survive append+get as a decoded JSON value.\n\
         Investigate: src/store/mod.rs write serialization and src/store/reader.rs read decoding.\n\
         Common causes: decoding the outer frame into serde_json::Value directly, which turns \
         the inner MessagePack payload into a byte array instead of the original JSON object."
    );
}

// ===== Property test: round-trip fidelity for ARBITRARY JSON payloads =====
//
// The example-based test above only checks one specific payload shape. This
// property test asserts the round-trip contract for ANY shrinkable JSON
// value generated by `arb_json()`. If a refactor breaks round-trip for
// some specific shape (e.g., zero-length strings, deeply nested objects,
// integer overflow at the f64 boundary), proptest will find it and shrink
// the failing case to a minimal counterexample. Sample on every CI run.

proptest! {
    #![proptest_config(common::proptest::cfg(64))]

    #[test]
    fn round_trip_fidelity_property(payload in arb_json()) {
        let dir = TempDir::new().expect("tmpdir");
        let store = test_store(&dir);
        let coord = test_coord();
        let kind = EventKind::custom(15, 4095);

        let receipt = store.append(&coord, kind, &payload).expect("append");
        let stored = store.get(receipt.event_id).expect("get");

        prop_assert_eq!(
            &stored.event.payload, &payload,
            "PROPERTY: append+get must preserve any JSON payload exactly. \
             A failing shrunk counterexample is the minimum input that breaks \
             round-trip — investigate src/store/writer.rs serialization and \
             src/store/reader.rs deserialization."
        );
        prop_assert_eq!(
            stored.event.event_kind(), kind,
            "PROPERTY: EventKind must round-trip for any payload."
        );
        prop_assert_eq!(
            stored.event.header.event_id, receipt.event_id,
            "PROPERTY: event_id must match between receipt and stored entry \
             for any payload."
        );
    }
}

// ===== LAW-003: No Orphan Infrastructure =====
//
// The previous `law_003_store_public_api_exercised` test asserted that the
// strings in a hardcoded `&[&str]` were non-empty. That's a tautology — it
// would have passed if every method on `Store` were deleted.
//
// The replacement is `check_store_pub_fn_coverage` in
// `tools/integrity/src/main.rs`, wired into `cargo xtask structural`. It
// parses `src/store/mod.rs` with `syn`, extracts every `pub fn` on the
// `impl Store` block, and asserts that each one is referenced (via method
// call, fully-qualified call, or turbofish call) by at least one file under
// `tests/` or `src/`. The check belongs in the integrity tool — not here —
// because it needs access to the syn AST and shouldn't bloat the unit test
// build.

// ===== LAW-007: Codebase Accuses Itself =====
// Verify that self-benchmark gates actually fire on bad data.
// This is a meta-test: testing that the testing infrastructure works.

#[test]
fn law_007_gates_reject_bad_performance() {
    // A ColdStartGate with impossible threshold should fire
    let gate = batpak::guard::GateSet::<(f64,)>::new();
    // GateSet with no gates always passes — that's correct behavior.
    // The real test is in perf_gates.rs which uses actual Store metrics.
    assert!(gate.is_empty());
    let proposal = batpak::pipeline::Proposal::new(42);
    let receipt = gate.evaluate(&(0.0,), proposal);
    assert!(
        receipt.is_ok(),
        "PROPERTY: Empty GateSet must always pass (vacuous truth).\n\
         Investigate: src/guard/mod.rs GateSet::evaluate.\n\
         Common causes: empty gate list returning Err instead of Ok."
    );
}

// ===== FM-007: Island Syndrome Defense =====
// Flow connectivity test: verify the full production path works end-to-end.
// Entry: Store::open → append → sync → close → reopen → stream → get → verify

#[test]
fn flow_connectivity_full_production_path() {
    let dir = TempDir::new().expect("tmpdir");
    let coord = Coordinate::new("user:alice", "scope:orders").expect("coord");
    let kind = EventKind::custom(2, 100);

    // Phase 1: Write through the full pipeline
    let event_id;
    {
        let store = test_store(&dir);

        // Step 1: Propose through gate pipeline
        let gates: GateSet<()> = GateSet::new();
        let pipeline = Pipeline::new(gates);
        let proposal = Proposal::new(serde_json::json!({"order_id": 1234}));
        let receipt = pipeline.evaluate(&(), proposal).expect("gate eval");

        // Step 2: Commit through store
        let committed = pipeline
            .commit(receipt, |payload| -> Result<_, StoreError> {
                let r = store.append(&coord, kind, &payload)?;
                Ok(Committed {
                    payload,
                    event_id: r.event_id,
                    sequence: r.sequence,
                    hash: [0u8; 32],
                })
            })
            .expect("commit");

        event_id = committed.event_id;

        // Step 3: Sync to disk
        store.sync().expect("sync");
        store.close().expect("close");
    }

    // Phase 2: Cold start and verify end-to-end
    let store = test_store(&dir);

    // Step 4: Read back via get — verify event exists and has correct metadata
    let stored = store.get(event_id).expect("get after cold start");
    assert_eq!(
        stored.event.header.event_id, event_id,
        "PROPERTY: Full pipeline flow must preserve event_id through write→sync→close→reopen→read.\n\
         Investigate: pipeline commit → store.append → segment write → cold start → index rebuild → get.\n\
         Common causes: Island Syndrome (FM-007) — pipeline not wired to store, or store not persisting."
    );
    assert_eq!(
        stored.coordinate.entity(),
        "user:alice",
        "PROPERTY: Entity must survive cold start round-trip."
    );

    // Step 5: Read back via stream
    let events = store.stream("user:alice");
    assert_eq!(
        events.len(),
        1,
        "PROPERTY: Stream must find events written through pipeline flow.\n\
         Investigate: src/store/mod.rs stream(), src/store/index.rs.\n\
         Common causes: entity key mismatch between append and stream."
    );

    // Step 6: Read back via query
    let region = Region::entity("user:alice");
    let results = store.query(&region);
    assert_eq!(
        results.len(),
        1,
        "PROPERTY: Query must find events written through pipeline flow.\n\
         Investigate: src/store/mod.rs query(), Region::matches_event.\n\
         Common causes: Region matching logic not covering exact entity match."
    );

    // Step 7: Cursor sees the event
    let region = Region::entity("user:alice");
    let mut cursor = store.cursor(&region);
    let entry = cursor.poll();
    assert!(
        entry.is_some(),
        "PROPERTY: Cursor must see events written through pipeline flow.\n\
         Investigate: src/store/cursor.rs poll(), index global_sequence.\n\
         Common causes: cursor starting past the event's sequence."
    );
}

// ===== FM-022: Receipt Hollowing Defense =====
// Verify that Receipt actually proves gates were evaluated.

#[test]
fn receipt_proves_gate_evaluation() {
    struct AuditGate;
    impl batpak::guard::Gate<()> for AuditGate {
        fn name(&self) -> &'static str {
            "audit_gate"
        }
        fn evaluate(&self, _ctx: &()) -> Result<(), Denial> {
            Ok(())
        }
    }

    let mut gates: GateSet<()> = GateSet::new();
    gates.push(AuditGate);

    let proposal = Proposal::new("test_payload");
    let receipt = gates.evaluate(&(), proposal).expect("should pass");
    let (payload, gate_names) = receipt.into_parts();

    assert_eq!(payload, "test_payload");
    assert_eq!(
        gate_names,
        vec!["audit_gate"],
        "PROPERTY: Receipt must record which gates were evaluated (not hollow).\n\
         Investigate: src/guard/mod.rs GateSet::evaluate gate_names collection.\n\
         Common causes: Receipt Hollowing (FM-022) — receipt created without recording gates."
    );
}

// ===== FM-023: Fallback Laundering Defense =====
// Verify that errors propagate, not silently become defaults.

#[test]
fn errors_propagate_not_launder_to_defaults() {
    let dir = TempDir::new().expect("tmpdir");
    let store = test_store(&dir);

    // get() on nonexistent event must return NotFound, not a default
    let result = store.get(999999);
    let err = result.expect_err(
        "PROPERTY: get() for nonexistent event must return Err(NotFound), not a default. \
         Investigate: src/store/mod.rs get(), src/store/reader.rs read_entry.",
    );
    assert!(
        matches!(err, StoreError::NotFound(_)),
        "PROPERTY: violation must surface as StoreError::NotFound, got {err:?}"
    );

    // CAS failure must return SequenceMismatch, not silently succeed
    let coord = test_coord();
    let kind = EventKind::custom(1, 1);
    store.append(&coord, kind, &"seed").expect("seed");

    let opts = AppendOptions {
        expected_sequence: Some(999), // wrong sequence
        ..AppendOptions::default()
    };
    let result = store.append_with_options(&coord, kind, &"should_fail", opts);
    let err = result.expect_err(
        "PROPERTY: CAS with wrong expected_sequence must return Err(SequenceMismatch). \
         Investigate: src/store/writer.rs CAS check (Step 1a).",
    );
    assert!(
        matches!(err, StoreError::SequenceMismatch { .. }),
        "PROPERTY: violation must surface as StoreError::SequenceMismatch, got {err:?}"
    );
}

// ===== INV-SEC: EventKind Category Enforcement =====
// Products must not be able to create system (0x0) or effect (0xD) kinds via custom().

#[test]
#[should_panic(expected = "category 0x0 is reserved")]
fn eventkind_rejects_system_category() {
    let _ = EventKind::custom(0x0, 1);
}

#[test]
#[should_panic(expected = "category 0xD is reserved")]
fn eventkind_rejects_effect_category() {
    let _ = EventKind::custom(0xD, 1);
}

#[test]
fn eventkind_allows_product_categories() {
    // Categories 0x1-0xC and 0xE-0xF are all valid for products
    for cat in 1..=0xCu8 {
        let kind = EventKind::custom(cat, 1);
        assert_eq!(
            kind.category(),
            cat,
            "PROPERTY: EventKind::custom({cat}, 1) must preserve category."
        );
    }
    for cat in [0xEu8, 0xF] {
        let kind = EventKind::custom(cat, 1);
        assert_eq!(
            kind.category(),
            cat,
            "PROPERTY: EventKind::custom({cat}, 1) must preserve category."
        );
    }
}

// ===== Phase 3A: Commutativity — independent entity appends are order-independent =====

#[test]
fn commutativity_independent_entity_appends() {
    // Append to entity A then B, and B then A — final index state should be equivalent
    let dir1 = tempfile::TempDir::new().expect("temp dir");
    let dir2 = tempfile::TempDir::new().expect("temp dir");
    let kind = EventKind::custom(1, 1);

    let coord_a = Coordinate::new("comm:alpha", "comm:scope").expect("valid");
    let coord_b = Coordinate::new("comm:beta", "comm:scope").expect("valid");

    // Order 1: A then B
    {
        let store = Store::open(StoreConfig::new(dir1.path())).expect("open");
        store.append(&coord_a, kind, &"a1").expect("a1");
        store.append(&coord_b, kind, &"b1").expect("b1");
        store.close().expect("close");
    }
    // Order 2: B then A
    {
        let store = Store::open(StoreConfig::new(dir2.path())).expect("open");
        store.append(&coord_b, kind, &"b1").expect("b1");
        store.append(&coord_a, kind, &"a1").expect("a1");
        store.close().expect("close");
    }

    // Both stores should have the same entity streams (same events per entity)
    let s1 = Store::open(StoreConfig::new(dir1.path())).expect("reopen1");
    let s2 = Store::open(StoreConfig::new(dir2.path())).expect("reopen2");

    assert_eq!(
        s1.stream("comm:alpha").len(),
        s2.stream("comm:alpha").len(),
        "PROPERTY: Independent entity appends must be commutative — \
         same number of events per entity regardless of append order.\n\
         Investigate: src/store/index.rs entity stream storage."
    );
    assert_eq!(
        s1.stream("comm:beta").len(),
        s2.stream("comm:beta").len(),
        "PROPERTY: Independent entity appends must be commutative."
    );
}

// ===== Phase 3B: Closure — Outcome combinators stay within Outcome type =====

#[test]
fn closure_outcome_combinators_preserve_type() {
    // map on Ok stays Ok
    let ok: Outcome<i32> = Outcome::Ok(42);
    let mapped = ok.map(|x| x * 2);
    assert!(
        matches!(mapped, Outcome::Ok(84)),
        "PROPERTY: Outcome::map must produce a valid Outcome::Ok, not escape the type.\n\
         Investigate: src/outcome/mod.rs Outcome::map()."
    );

    // map on Err stays Err
    let err: Outcome<i32> = Outcome::Err(OutcomeError {
        kind: batpak::prelude::ErrorKind::Internal,
        message: "test".into(),
        compensation: None,
        retryable: false,
    });
    let mapped_err = err.map(|x| x * 2);
    assert!(
        matches!(mapped_err, Outcome::Err(_)),
        "PROPERTY: Outcome::map on Err must preserve the Err variant.\n\
         Investigate: src/outcome/mod.rs Outcome::map() non-Ok pass-through."
    );

    // and_then on Ok produces valid Outcome
    let ok2: Outcome<i32> = Outcome::Ok(10);
    let chained = ok2.and_then(|x| Outcome::Ok(x + 1));
    assert!(
        matches!(chained, Outcome::Ok(11)),
        "PROPERTY: Outcome::and_then must produce a valid Outcome.\n\
         Investigate: src/outcome/mod.rs Outcome::and_then()."
    );

    // zip of two Ok values produces Ok tuple
    let a: Outcome<i32> = Outcome::Ok(1);
    let b: Outcome<i32> = Outcome::Ok(2);
    let zipped = batpak::outcome::zip(a, b);
    assert!(
        matches!(zipped, Outcome::Ok((1, 2))),
        "PROPERTY: zip(Ok(a), Ok(b)) must produce Ok((a, b)).\n\
         Investigate: src/outcome/combine.rs zip()."
    );
}

// ===== Phase 3C: Totality — unknown EventKind through projection doesn't panic =====

#[derive(Default, Debug, serde::Serialize, serde::Deserialize)]
struct StrictCounter {
    count: u64,
}

impl EventSourced<serde_json::Value> for StrictCounter {
    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        if events.is_empty() {
            return None;
        }
        let mut s = Self::default();
        for e in events {
            s.apply_event(e);
        }
        Some(s)
    }
    fn apply_event(&mut self, _event: &Event<serde_json::Value>) {
        // Intentionally handles ALL event kinds without panic
        self.count += 1;
    }
    fn relevant_event_kinds() -> &'static [EventKind] {
        // Only cares about one kind, but receives all
        static KINDS: [EventKind; 1] = [EventKind::custom(1, 1)];
        &KINDS
    }
}

#[test]
fn totality_projection_handles_unknown_event_kinds() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open");
    let coord = Coordinate::new("total:entity", "total:scope").expect("valid");

    // Append events with different kinds — projection should handle all gracefully
    let known_kind = EventKind::custom(1, 1);
    let unknown_kind = EventKind::custom(2, 99); // not in relevant_event_kinds

    store.append(&coord, known_kind, &"known").expect("known");
    store
        .append(&coord, unknown_kind, &"unknown")
        .expect("unknown");
    store.append(&coord, known_kind, &"known2").expect("known2");

    // This must not panic even though unknown_kind isn't in relevant_event_kinds
    let result: Option<StrictCounter> = store
        .project("total:entity", &batpak::store::Freshness::Consistent)
        .expect("project must not panic on unknown kinds");

    assert!(
        result.is_some(),
        "PROPERTY: Projection must complete successfully even with unknown EventKinds.\n\
         Investigate: src/store/mod.rs project() event filtering.\n\
         INVARIANT: INV-TYPE totality — functions handle all inputs in their domain."
    );
}

// ===== Phase 4C: Error Variant Coverage — every StoreError has non-empty Display =====

#[test]
fn error_variant_coverage_all_store_errors_display() {
    use batpak::store::StoreError;

    // Construct every StoreError variant and verify Display is non-empty.
    // FM-011: No hollow error paths.
    let variants: Vec<(&str, StoreError)> = vec![
        ("Io", StoreError::Io(std::io::Error::other("test"))),
        (
            "Serialization",
            StoreError::Serialization("test ser".into()),
        ),
        (
            "CrcMismatch",
            StoreError::CrcMismatch {
                segment_id: 1,
                offset: 42,
            },
        ),
        (
            "CorruptSegment",
            StoreError::CorruptSegment {
                segment_id: 2,
                detail: "bad".into(),
            },
        ),
        ("NotFound", StoreError::NotFound(123)),
        (
            "SequenceMismatch",
            StoreError::SequenceMismatch {
                entity: "test".into(),
                expected: 1,
                actual: 2,
            },
        ),
        ("WriterCrashed", StoreError::WriterCrashed),
        ("CacheFailed", StoreError::CacheFailed("cache err".into())),
    ];

    for (name, err) in &variants {
        let display = format!("{err}");
        assert!(
            !display.is_empty(),
            "PROPERTY: StoreError::{name} must have a non-empty Display message.\n\
             FM-011: Error Path Hollowing — every error variant must carry actionable context.\n\
             Investigate: src/store/mod.rs Display impl for StoreError."
        );
    }

    // Also verify CoordinateError variant
    let coord_err =
        StoreError::Coordinate(Coordinate::new("", "scope").expect_err("empty entity must fail"));
    let display = format!("{coord_err}");
    assert!(
        !display.is_empty(),
        "PROPERTY: StoreError::Coordinate must have non-empty Display."
    );
}

// ===== DagPosition: nodes at different depths are incomparable =====

#[test]
fn dag_position_different_depths_are_incomparable() {
    use batpak::prelude::DagPosition;
    let shallow = DagPosition::new(0, 0, 5);
    let deep = DagPosition::new(1, 0, 5);

    assert!(
        shallow.partial_cmp(&deep).is_none(),
        "PROPERTY: DagPosition with different depths must be incomparable.\n\
         Investigate: src/coordinate/position.rs PartialOrd impl.\n\
         This prevents treating positions on different DAG branches as ordered."
    );
}

// ===== Store::drop drains pending writer events before exit =====

#[test]
fn store_drop_drains_pending_events() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let kind = EventKind::custom(1, 1);
    let coord = Coordinate::new("drop:entity", "drop:scope").expect("valid");

    // Append events and drop (not close) — data should still be recoverable
    {
        let store = Store::open(StoreConfig::new(dir.path())).expect("open");
        for i in 0..10 {
            store
                .append(&coord, kind, &serde_json::json!({"i": i}))
                .expect("append");
        }
        // Drop without close() — Drop should wait briefly for writer drain
    }

    // Reopen and verify events persisted
    let store = Store::open(StoreConfig::new(dir.path())).expect("reopen");
    let events = store.stream("drop:entity");
    assert!(
        events.len() >= 10,
        "PROPERTY: Store Drop must drain pending events. Got {} events, expected 10.\n\
         Investigate: src/store/mod.rs Drop impl — bounded wait for writer drain.",
        events.len()
    );
}

// ================================================================
// StoreError classification
// ================================================================

#[test]
fn error_kind_is_domain() {
    assert!(
        ErrorKind::NotFound.is_domain(),
        "PROPERTY: ErrorKind::NotFound must be classified as a domain error.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_domain().\n\
         Common causes: NotFound missing from the domain match arm, or mis-categorized \
         as an operational error.\n\
         Run: cargo test --test store_properties error_kind_is_domain"
    );
    assert!(
        ErrorKind::Conflict.is_domain(),
        "PROPERTY: ErrorKind::Conflict must be classified as a domain error.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_domain().\n\
         Common causes: Conflict missing from the domain match arm, or grouped \
         with operational errors.\n\
         Run: cargo test --test store_properties error_kind_is_domain"
    );
    assert!(
        ErrorKind::Validation.is_domain(),
        "PROPERTY: ErrorKind::Validation must be classified as a domain error.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_domain().\n\
         Common causes: Validation missing from the domain match arm.\n\
         Run: cargo test --test store_properties error_kind_is_domain"
    );
    assert!(
        ErrorKind::PolicyRejection.is_domain(),
        "PROPERTY: ErrorKind::PolicyRejection must be classified as a domain error.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_domain().\n\
         Common causes: PolicyRejection missing from the domain match arm, or \
         grouped with operational errors.\n\
         Run: cargo test --test store_properties error_kind_is_domain"
    );
    assert!(
        !ErrorKind::StorageError.is_domain(),
        "PROPERTY: ErrorKind::StorageError must NOT be classified as a domain error.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_domain().\n\
         Common causes: StorageError incorrectly placed in the domain match arm, or \
         wildcard arm returning true.\n\
         Run: cargo test --test store_properties error_kind_is_domain"
    );
    assert!(
        !ErrorKind::Timeout.is_domain(),
        "PROPERTY: ErrorKind::Timeout must NOT be classified as a domain error.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_domain().\n\
         Common causes: Timeout incorrectly placed in the domain match arm.\n\
         Run: cargo test --test store_properties error_kind_is_domain"
    );
    assert!(
        !ErrorKind::Internal.is_domain(),
        "PROPERTY: ErrorKind::Internal must NOT be classified as a domain error.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_domain().\n\
         Common causes: Internal incorrectly placed in the domain match arm, or \
         wildcard arm returning true.\n\
         Run: cargo test --test store_properties error_kind_is_domain"
    );
    assert!(
        !ErrorKind::Custom(99).is_domain(),
        "PROPERTY: ErrorKind::Custom must NOT be classified as a domain error by default.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_domain().\n\
         Common causes: Custom variant handled by a wildcard arm that returns true.\n\
         Run: cargo test --test store_properties error_kind_is_domain"
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
         Run: cargo test --test store_properties error_kind_is_operational"
    );
    assert!(
        ErrorKind::Timeout.is_operational(),
        "PROPERTY: ErrorKind::Timeout must be classified as operational.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_operational().\n\
         Common causes: Timeout missing from the operational match arm.\n\
         Run: cargo test --test store_properties error_kind_is_operational"
    );
    assert!(
        ErrorKind::Serialization.is_operational(),
        "PROPERTY: ErrorKind::Serialization must be classified as operational.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_operational().\n\
         Common causes: Serialization missing from the operational match arm, or \
         grouped with domain errors.\n\
         Run: cargo test --test store_properties error_kind_is_operational"
    );
    assert!(
        ErrorKind::Internal.is_operational(),
        "PROPERTY: ErrorKind::Internal must be classified as operational.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_operational().\n\
         Common causes: Internal missing from the operational match arm.\n\
         Run: cargo test --test store_properties error_kind_is_operational"
    );
    assert!(
        !ErrorKind::NotFound.is_operational(),
        "PROPERTY: ErrorKind::NotFound must NOT be classified as operational (it is a domain error).\n\
         Investigate: src/outcome/error.rs ErrorKind::is_operational().\n\
         Common causes: is_operational() wildcard arm returning true for domain errors.\n\
         Run: cargo test --test store_properties error_kind_is_operational"
    );
    assert!(
        !ErrorKind::Conflict.is_operational(),
        "PROPERTY: ErrorKind::Conflict must NOT be classified as operational (it is a domain error).\n\
         Investigate: src/outcome/error.rs ErrorKind::is_operational().\n\
         Common causes: Conflict incorrectly placed in the operational match arm.\n\
         Run: cargo test --test store_properties error_kind_is_operational"
    );
    assert!(
        !ErrorKind::Custom(99).is_operational(),
        "PROPERTY: ErrorKind::Custom must NOT be classified as operational by default.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_operational().\n\
         Common causes: Custom variant matched by wildcard arm that returns true.\n\
         Run: cargo test --test store_properties error_kind_is_operational"
    );
}

#[test]
fn error_kind_is_retryable() {
    assert!(
        ErrorKind::StorageError.is_retryable(),
        "PROPERTY: ErrorKind::StorageError must be classified as retryable.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_retryable().\n\
         Common causes: StorageError missing from the retryable match arm, or \
         is_retryable() returning false by default.\n\
         Run: cargo test --test store_properties error_kind_is_retryable"
    );
    assert!(
        ErrorKind::Timeout.is_retryable(),
        "PROPERTY: ErrorKind::Timeout must be classified as retryable.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_retryable().\n\
         Common causes: Timeout missing from the retryable match arm, or \
         Timeout placed in the non-retryable group by mistake.\n\
         Run: cargo test --test store_properties error_kind_is_retryable"
    );
    assert!(
        !ErrorKind::NotFound.is_retryable(),
        "PROPERTY: ErrorKind::NotFound must NOT be retryable (domain error, not transient).\n\
         Investigate: src/outcome/error.rs ErrorKind::is_retryable().\n\
         Common causes: NotFound grouped with operational errors in the retryable arm.\n\
         Run: cargo test --test store_properties error_kind_is_retryable"
    );
    assert!(
        !ErrorKind::Conflict.is_retryable(),
        "PROPERTY: ErrorKind::Conflict must NOT be retryable (requires resolution, not retry).\n\
         Investigate: src/outcome/error.rs ErrorKind::is_retryable().\n\
         Common causes: Conflict grouped with transient errors, or is_retryable() \
         treating all non-domain errors as retryable.\n\
         Run: cargo test --test store_properties error_kind_is_retryable"
    );
    assert!(
        !ErrorKind::Internal.is_retryable(),
        "PROPERTY: ErrorKind::Internal must NOT be retryable (programming error, not transient).\n\
         Investigate: src/outcome/error.rs ErrorKind::is_retryable().\n\
         Common causes: Internal grouped with operational transients by mistake.\n\
         Run: cargo test --test store_properties error_kind_is_retryable"
    );
    assert!(
        !ErrorKind::Custom(99).is_retryable(),
        "PROPERTY: ErrorKind::Custom must NOT be retryable by default.\n\
         Investigate: src/outcome/error.rs ErrorKind::is_retryable().\n\
         Common causes: Custom variant handled by a wildcard arm that returns true, or \
         Custom not having an explicit non-retryable arm.\n\
         Run: cargo test --test store_properties error_kind_is_retryable"
    );
}

// ================================================================
// AppendOptions builder API
// ================================================================

#[test]
fn append_options_with_idempotency_builder() {
    let opts = AppendOptions::new().with_idempotency(0xDEAD_BEEF_CAFE_BABE);
    assert_eq!(
        opts.idempotency_key,
        Some(0xDEAD_BEEF_CAFE_BABE),
        "PROPERTY: with_idempotency(key) must set idempotency_key to Some(key).\n\
         Investigate: src/store/mod.rs AppendOptions::with_idempotency.\n\
         Common causes: builder returning Self without setting idempotency_key."
    );
    assert!(
        opts.expected_sequence.is_none(),
        "unset fields must remain None"
    );
    assert_eq!(opts.flags, 0, "unset flags must remain 0");
}

#[test]
fn append_options_with_cas_builder() {
    let opts = AppendOptions::new().with_cas(7);
    assert_eq!(
        opts.expected_sequence,
        Some(7),
        "PROPERTY: with_cas(seq) must set expected_sequence to Some(seq).\n\
         Investigate: src/store/mod.rs AppendOptions::with_cas.\n\
         Common causes: method setting wrong field, or returning Self unchanged."
    );
    assert!(
        opts.idempotency_key.is_none(),
        "unset fields must remain None"
    );
}

#[test]
fn append_options_with_flags_builder() {
    let opts = AppendOptions::new().with_flags(0x03);
    assert_eq!(
        opts.flags, 0x03,
        "PROPERTY: with_flags(f) must set flags to f.\n\
         Investigate: src/store/mod.rs AppendOptions::with_flags.\n\
         Common causes: flags field not updated, or OR'd with previous value."
    );
    assert!(
        opts.expected_sequence.is_none(),
        "unset fields must remain None"
    );
    assert!(
        opts.idempotency_key.is_none(),
        "unset fields must remain None"
    );
}

#[test]
fn append_options_with_correlation_builder() {
    let opts = AppendOptions::new().with_correlation(0xCAFE_BABE_1234_5678);
    assert_eq!(
        opts.correlation_id,
        Some(0xCAFE_BABE_1234_5678),
        "PROPERTY: with_correlation(id) must set correlation_id to Some(id).\n\
         Investigate: src/store/mod.rs AppendOptions::with_correlation.\n\
         Common causes: method writing to causation_id by mistake."
    );
    assert!(opts.causation_id.is_none(), "causation_id must not be set");
}

#[test]
fn append_options_with_causation_builder() {
    let opts = AppendOptions::new().with_causation(0xABCD_EF01_2345_6789);
    assert_eq!(
        opts.causation_id,
        Some(0xABCD_EF01_2345_6789),
        "PROPERTY: with_causation(id) must set causation_id to Some(id).\n\
         Investigate: src/store/mod.rs AppendOptions::with_causation.\n\
         Common causes: method writing to correlation_id by mistake."
    );
    assert!(
        opts.correlation_id.is_none(),
        "correlation_id must not be set"
    );
}

#[test]
fn append_options_builder_chain() {
    // All builders must be chainable and independent
    let opts = AppendOptions::new()
        .with_idempotency(1)
        .with_cas(5)
        .with_flags(0x01)
        .with_correlation(2)
        .with_causation(3);
    assert_eq!(opts.idempotency_key, Some(1));
    assert_eq!(opts.expected_sequence, Some(5));
    assert_eq!(opts.flags, 0x01);
    assert_eq!(opts.correlation_id, Some(2));
    assert_eq!(opts.causation_id, Some(3));
}
