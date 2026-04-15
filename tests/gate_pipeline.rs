#![allow(clippy::panic)] // test assertions use panic for expected-failure paths
//! Gate and Pipeline integration tests.
//! Registration order, fail-fast evaluation, Receipt TOCTOU guarantee, consumed-once.
//!
//! PROVES: LAW-001 (No Fake Success), LAW-004 (Composition Over Construction)
//! DEFENDS: FM-022 (Receipt Hollowing — Receipt is sealed, single-use)
//! INVARIANTS: INV-STATE (gate evaluation state machine), INV-SEC (Receipt seal)

use batpak::prelude::*;

// --- Test gate implementations ---

struct AlwaysAllow;
impl Gate<()> for AlwaysAllow {
    fn name(&self) -> &'static str {
        "always_allow"
    }
    fn evaluate(&self, _ctx: &()) -> Result<(), Denial> {
        Ok(())
    }
}

struct AlwaysDeny {
    reason: &'static str,
}
impl Gate<()> for AlwaysDeny {
    fn name(&self) -> &'static str {
        "always_deny"
    }
    fn evaluate(&self, _ctx: &()) -> Result<(), Denial> {
        Err(Denial::new("always_deny", self.reason))
    }
}

struct ContextGate;
impl Gate<i32> for ContextGate {
    fn name(&self) -> &'static str {
        "context_gate"
    }
    fn evaluate(&self, ctx: &i32) -> Result<(), Denial> {
        if *ctx > 0 {
            Ok(())
        } else {
            Err(Denial::new("context_gate", "context must be positive"))
        }
    }
}

// --- Gate evaluation ---

#[test]
fn empty_gateset_always_passes() {
    let gates = GateSet::<()>::new();
    let proposal = Proposal::new(42);
    let result = gates.evaluate(&(), proposal);
    assert!(result.is_ok(), "Empty GateSet should always pass.");
}

#[test]
fn single_allow_gate_passes() {
    let mut gates = GateSet::new();
    gates.push(AlwaysAllow);
    let proposal = Proposal::new(42);
    let result = gates.evaluate(&(), proposal);
    assert!(
        result.is_ok(),
        "GATE EVALUATION: single AlwaysAllow gate should pass.\n\
         Investigate: src/guard/mod.rs GateSet::evaluate.\n\
         Common causes: Gate::evaluate returning Err unexpectedly, gate not registered.\n\
         Run: cargo test --test gate_pipeline single_allow_gate_passes"
    );
}

#[test]
fn single_deny_gate_fails() {
    let mut gates = GateSet::new();
    gates.push(AlwaysDeny { reason: "nope" });
    let proposal = Proposal::new(42);
    let result = gates.evaluate(&(), proposal);
    match result {
        Err(denial) => {
            assert_eq!(
                denial.gate, "always_deny",
                "DENIAL GATE NAME: denial.gate should be 'always_deny'.\n\
                 Investigate: src/guard/mod.rs Denial::new gate field.\n\
                 Common causes: gate name not propagated into Denial struct.\n\
                 Run: cargo test --test gate_pipeline single_deny_gate_fails"
            );
            assert_eq!(
                denial.message, "nope",
                "DENIAL MESSAGE: denial.message should be 'nope'.\n\
                 Investigate: src/guard/mod.rs Denial::new message field.\n\
                 Common causes: message not propagated into Denial struct.\n\
                 Run: cargo test --test gate_pipeline single_deny_gate_fails"
            );
        }
        Ok(_) => panic!("Expected Err(Denial), gate should have denied"),
    }
}

// --- Fail-fast ---

#[test]
fn fail_fast_stops_at_first_denial() {
    let mut gates = GateSet::new();
    gates.push(AlwaysDeny { reason: "first" });
    gates.push(AlwaysDeny { reason: "second" });

    let proposal = Proposal::new(42);
    let result = gates.evaluate(&(), proposal);
    let denial = match result {
        Err(d) => d,
        Ok(_) => panic!("Expected Err(Denial)"),
    };
    assert_eq!(
        denial.message, "first",
        "FAIL-FAST VIOLATED: first denial should stop evaluation. \
         Investigate: src/guard/mod.rs GateSet::evaluate."
    );
}

// --- evaluate_all collects all denials ---

#[test]
fn evaluate_all_collects_all_denials() {
    let mut gates = GateSet::new();
    gates.push(AlwaysDeny { reason: "first" });
    gates.push(AlwaysAllow);
    gates.push(AlwaysDeny { reason: "third" });

    let denials = gates.evaluate_all(&());
    assert_eq!(
        denials.len(),
        2,
        "evaluate_all should collect all denials, not fail-fast. \
         Investigate: src/guard/mod.rs GateSet::evaluate_all."
    );
}

// --- Registration order matters ---

#[test]
fn gate_registration_order_preserved() {
    let mut gates = GateSet::new();
    gates.push(AlwaysAllow);
    gates.push(AlwaysDeny { reason: "deny" });

    let names = gates.names();
    assert_eq!(
        names,
        vec!["always_allow", "always_deny"],
        "Gate registration order must be preserved."
    );
}

// --- Receipt TOCTOU guarantee ---

#[test]
fn receipt_wraps_payload_immutably() {
    let mut gates = GateSet::new();
    gates.push(AlwaysAllow);

    let proposal = Proposal::new(42);
    let receipt = gates.evaluate(&(), proposal).expect("should pass");

    assert_eq!(
        *receipt.payload(),
        42,
        "Receipt should wrap the original proposal payload."
    );
    assert_eq!(
        receipt.gates_passed(),
        &["always_allow"],
        "RECEIPT GATES PASSED: receipt should record the gates it passed through.\n\
         Investigate: src/guard/mod.rs Receipt::gates_passed.\n\
         Common causes: gate names not accumulated during evaluation, wrong order.\n\
         Run: cargo test --test gate_pipeline receipt_wraps_payload_immutably"
    );
}

#[test]
fn receipt_consumed_once_via_into_parts() {
    let mut gates = GateSet::new();
    gates.push(AlwaysAllow);

    let proposal = Proposal::new(42);
    let receipt = gates.evaluate(&(), proposal).expect("should pass");

    let (payload, gate_names) = receipt.into_parts();
    assert_eq!(
        payload, 42,
        "RECEIPT CONSUMED ONCE: into_parts should yield the original payload.\n\
         Investigate: src/guard/mod.rs Receipt::into_parts.\n\
         Common causes: payload not moved correctly out of Receipt.\n\
         Run: cargo test --test gate_pipeline receipt_consumed_once_via_into_parts"
    );
    assert_eq!(
        gate_names,
        vec!["always_allow"],
        "RECEIPT CONSUMED ONCE: into_parts should yield the list of passed gate names.\n\
         Investigate: src/guard/mod.rs Receipt::into_parts.\n\
         Common causes: gate names not moved correctly out of Receipt.\n\
         Run: cargo test --test gate_pipeline receipt_consumed_once_via_into_parts"
    );
    // receipt is now consumed — can't be used again (enforced by type system)
}

// --- Pipeline commit ---

#[test]
fn pipeline_commit_with_receipt() {
    let mut gates = GateSet::new();
    gates.push(AlwaysAllow);

    let pipeline = batpak::pipeline::Pipeline::new(gates);
    let proposal = Proposal::new("data".to_string());
    let receipt = pipeline.evaluate(&(), proposal).expect("should pass");

    let committed = pipeline
        .commit(receipt, |payload| {
            Ok::<_, String>(Committed {
                payload,
                event_id: 12345,
                sequence: 0,
                hash: [0u8; 32],
            })
        })
        .expect("commit should succeed");

    assert_eq!(
        committed.payload, "data",
        "PIPELINE COMMIT PAYLOAD: committed payload should match the proposal value.\n\
         Investigate: src/pipeline/mod.rs Pipeline::commit.\n\
         Common causes: payload not forwarded from Receipt into commit closure.\n\
         Run: cargo test --test gate_pipeline pipeline_commit_with_receipt"
    );
    assert_eq!(
        committed.event_id, 12345,
        "PIPELINE COMMIT EVENT_ID: committed event_id should match what the closure returns.\n\
         Investigate: src/pipeline/mod.rs Pipeline::commit.\n\
         Common causes: commit closure result not propagated into Committed struct.\n\
         Run: cargo test --test gate_pipeline pipeline_commit_with_receipt"
    );
}

// --- Context-dependent gate ---

#[test]
fn context_gate_uses_context() {
    let mut gates = GateSet::new();
    gates.push(ContextGate);

    let proposal_pass = Proposal::new("ok");
    assert!(
        gates.evaluate(&1, proposal_pass).is_ok(),
        "CONTEXT GATE: positive context should pass ContextGate.\n\
         Investigate: src/guard/mod.rs Gate::evaluate context usage.\n\
         Common causes: context value not passed through to gate, comparison logic inverted.\n\
         Run: cargo test --test gate_pipeline context_gate_uses_context"
    );

    let proposal_fail = Proposal::new("fail");
    // Note: `Receipt<&str>` doesn't implement Debug (it carries an opaque
    // sealed token), so `Result::expect_err` doesn't compile here.
    // Match instead.
    let denial = match gates.evaluate(&(-1), proposal_fail) {
        Ok(_) => panic!(
            "PROPERTY: non-positive context must be denied by ContextGate. \
             Investigate: src/guard/mod.rs Gate::evaluate context usage."
        ),
        Err(d) => d,
    };
    assert_eq!(
        denial.gate, "context_gate",
        "PROPERTY: first failed gate must be 'context_gate', got {denial:?}"
    );
}

// --- Denial builder ---

#[test]
fn denial_builder_pattern() {
    let denial = Denial::new("test_gate", "access denied")
        .with_code("403")
        .with_context("user_id", "123")
        .with_context("resource", "secret");

    assert_eq!(
        denial.gate, "test_gate",
        "DENIAL BUILDER: gate field should match the name passed to Denial::new.\n\
         Investigate: src/guard/mod.rs Denial::new.\n\
         Common causes: gate name not stored in Denial struct field.\n\
         Run: cargo test --test gate_pipeline denial_builder_pattern"
    );
    assert_eq!(
        denial.code, "403",
        "DENIAL BUILDER: code field should match the value passed to with_code.\n\
         Investigate: src/guard/mod.rs Denial::with_code.\n\
         Common causes: with_code not storing value, returning default instead.\n\
         Run: cargo test --test gate_pipeline denial_builder_pattern"
    );
    assert_eq!(
        denial.message, "access denied",
        "DENIAL BUILDER: message field should match the reason passed to Denial::new.\n\
         Investigate: src/guard/mod.rs Denial::new.\n\
         Common causes: message not stored in Denial struct field.\n\
         Run: cargo test --test gate_pipeline denial_builder_pattern"
    );
    assert_eq!(denial.context.len(), 2,
        "DENIAL BUILDER: context map should contain exactly 2 entries after two with_context calls.\n\
         Investigate: src/guard/mod.rs Denial::with_context.\n\
         Common causes: with_context not inserting into map, entries being overwritten.\n\
         Run: cargo test --test gate_pipeline denial_builder_pattern");
    assert_eq!(
        denial.to_string(),
        "[test_gate] access denied",
        "DENIAL BUILDER: Display format should be '[gate] message'.\n\
         Investigate: src/guard/mod.rs impl Display for Denial.\n\
         Common causes: Display format string incorrect, gate or message field wrong.\n\
         Run: cargo test --test gate_pipeline denial_builder_pattern"
    );
}

// ===== Wave 2B: BypassReason + BypassReceipt deep tests =====
// These had only quiet_straggler existence checks. Now we test the trait contract,
// audit trail format, and payload consumption semantics.
// DEFENDS: FM-022 (Receipt Hollowing), LAW-001 (No Fake Success via bypass)

struct TestBypassReason;

impl batpak::pipeline::bypass::BypassReason for TestBypassReason {
    fn name(&self) -> &'static str {
        "emergency_override"
    }
    fn justification(&self) -> &'static str {
        "Production incident #1234 — rate limiter disabled"
    }
}

#[test]
fn bypass_reason_trait_returns_correct_values() {
    use batpak::pipeline::bypass::BypassReason;
    let reason = TestBypassReason;
    assert_eq!(
        reason.name(),
        "emergency_override",
        "PROPERTY: BypassReason::name() must return the declared name.\n\
         Investigate: src/pipeline/bypass.rs BypassReason trait.\n\
         Common causes: trait method returning wrong field."
    );
    assert_eq!(
        reason.justification(),
        "Production incident #1234 — rate limiter disabled",
        "PROPERTY: BypassReason::justification() must return the declared justification."
    );
}

#[test]
fn bypass_receipt_getters_match_construction() {
    let receipt = Pipeline::<()>::bypass(Proposal::new("important_payload"), &TestBypassReason);
    assert_eq!(
        receipt.reason(),
        "emergency_override",
        "PROPERTY: BypassReceipt::reason() must match the BypassReason::name().\n\
         Investigate: src/pipeline/mod.rs Pipeline::bypass construction."
    );
    assert_eq!(
        receipt.justification(),
        "Production incident #1234 — rate limiter disabled",
        "PROPERTY: BypassReceipt::justification() must match BypassReason::justification()."
    );
    assert_eq!(
        receipt.payload(),
        &"important_payload",
        "PROPERTY: BypassReceipt::payload() must return the original proposal payload."
    );
}

#[test]
fn bypass_receipt_into_payload_consumes_and_returns() {
    let receipt = Pipeline::<()>::bypass(Proposal::new(vec![1, 2, 3]), &TestBypassReason);
    let payload = receipt.into_payload();
    assert_eq!(
        payload,
        vec![1, 2, 3],
        "PROPERTY: BypassReceipt::into_payload() must return the original payload by move.\n\
         Investigate: src/pipeline/bypass.rs into_payload()."
    );
}

#[test]
fn bypass_through_full_pipeline_commits_to_store() {
    use batpak::store::{Store, StoreConfig};
    let dir = tempfile::TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open");
    let coord = Coordinate::new("bypass:entity", "bypass:scope").expect("valid");
    let kind = EventKind::custom(1, 1);

    // Bypass gates entirely
    let bypass_receipt = Pipeline::<()>::bypass(
        Proposal::new(serde_json::json!({"bypassed": true})),
        &TestBypassReason,
    );

    // Commit the bypassed payload to the store
    let payload = bypass_receipt.into_payload();
    let append_receipt = store.append(&coord, kind, &payload).expect("append");

    // Verify the event actually persisted
    let stored = store.get(append_receipt.event_id).expect("get");
    assert_eq!(
        stored.coordinate, coord,
        "PROPERTY: Bypassed payload must persist to store with correct coordinate.\n\
         Investigate: Pipeline::bypass → into_payload → store.append flow.\n\
         DEFENDS: FM-007 (Island Syndrome — bypass path must connect to real store)."
    );
}

// ================================================================
// Pipeline, bypass, committed, denial, gateset tests
// ================================================================

struct StragglersTestBypassReason;
impl batpak::pipeline::BypassReason for StragglersTestBypassReason {
    fn name(&self) -> &'static str {
        "test_bypass"
    }
    fn justification(&self) -> &'static str {
        "testing bypass audit trail"
    }
}

static STRAGGLERS_TEST_BYPASS: StragglersTestBypassReason = StragglersTestBypassReason;

#[test]
fn pipeline_bypass_returns_bypass_receipt() {
    let proposal = Proposal::new(42);
    let receipt = batpak::pipeline::Pipeline::<()>::bypass(proposal, &STRAGGLERS_TEST_BYPASS);

    assert_eq!(
        *receipt.payload(),
        42,
        "PROPERTY: BypassReceipt must carry the original proposal payload unchanged.\n\
         Investigate: src/pipeline/mod.rs Pipeline::bypass().\n\
         Common causes: bypass() discarding the proposal value, or BypassReceipt \
         storing the wrong field.\n\
         Run: cargo test --test gate_pipeline pipeline_bypass_returns_bypass_receipt"
    );
    assert_eq!(
        receipt.reason(),
        "test_bypass",
        "PROPERTY: BypassReceipt must record the BypassReason::name() as reason.\n\
         Investigate: src/pipeline/mod.rs Pipeline::bypass() BypassReason::name().\n\
         Common causes: bypass() storing justification() in reason field, or \
         name() not being called at all.\n\
         Run: cargo test --test gate_pipeline pipeline_bypass_returns_bypass_receipt"
    );
    assert_eq!(
        receipt.justification(),
        "testing bypass audit trail",
        "PROPERTY: BypassReceipt must record BypassReason::justification() verbatim.\n\
         Investigate: src/pipeline/mod.rs Pipeline::bypass() BypassReason::justification().\n\
         Common causes: bypass() storing name() in justification field, or \
         justification() returning a hardcoded string instead of the impl's value.\n\
         Run: cargo test --test gate_pipeline pipeline_bypass_returns_bypass_receipt"
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
         Run: cargo test --test gate_pipeline proposal_map_transforms_payload"
    );
}

#[test]
fn committed_serde_round_trip() {
    let committed = batpak::pipeline::Committed {
        payload: "test".to_string(),
        event_id: 0xDEAD_BEEF_CAFE_BABE_1234_5678_9ABC_DEF0,
        sequence: 42,
        hash: [0xAA; 32],
    };

    // Serialize to msgpack then back — exercises u128_bytes wire format
    let bytes = rmp_serde::to_vec_named(&committed).expect("serialize Committed");
    let decoded: batpak::pipeline::Committed<String> =
        rmp_serde::from_slice(&bytes).expect("deserialize Committed");

    assert_eq!(
        decoded.payload, "test",
        "PROPERTY: Committed payload must survive msgpack serialization round-trip unchanged.\n\
         Investigate: src/pipeline/mod.rs Committed Serialize/Deserialize impls.\n\
         Common causes: payload field not tagged with serde attribute, or msgpack \
         encoding changing string encoding between versions.\n\
         Run: cargo test --test gate_pipeline committed_serde_round_trip"
    );
    assert_eq!(
        decoded.event_id, 0xDEAD_BEEF_CAFE_BABE_1234_5678_9ABC_DEF0,
        "PROPERTY: Committed event_id must round-trip through u128_bytes wire format without loss.\n\
         Investigate: src/pipeline/mod.rs src/wire.rs u128_bytes serde helper.\n\
         Common causes: u128_bytes encoding as little-endian but decoding as big-endian, \
         or serde_as attribute missing on the event_id field.\n\
         Run: cargo test --test gate_pipeline committed_serde_round_trip"
    );
    assert_eq!(
        decoded.sequence, 42,
        "PROPERTY: Committed sequence must survive msgpack serialization round-trip unchanged.\n\
         Investigate: src/pipeline/mod.rs Committed Serialize/Deserialize impls.\n\
         Common causes: sequence field not included in serialization, or deserialized \
         into wrong numeric type causing truncation.\n\
         Run: cargo test --test gate_pipeline committed_serde_round_trip"
    );
    assert_eq!(
        decoded.hash, [0xAA; 32],
        "PROPERTY: Committed hash must survive msgpack serialization round-trip unchanged.\n\
         Investigate: src/pipeline/mod.rs Committed Serialize/Deserialize impls.\n\
         Common causes: hash field serialized as a sequence vs bytes causing length mismatch, \
         or serde_bytes attribute missing from the hash field.\n\
         Run: cargo test --test gate_pipeline committed_serde_round_trip"
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
         Run: cargo test --test gate_pipeline denial_is_error_trait"
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
         Run: cargo test --test gate_pipeline denial_serialize"
    );
    assert!(
        json.contains("403"),
        "PROPERTY: Serialized Denial JSON must include the code field value.\n\
         Investigate: src/guard/denial.rs Denial Serialize impl with_code().\n\
         Common causes: code field serialized as null instead of the set value, \
         or with_code() not storing the value in the struct.\n\
         Run: cargo test --test gate_pipeline denial_serialize"
    );
    assert!(
        json.contains("alice"),
        "PROPERTY: Serialized Denial JSON must include context key-value pairs.\n\
         Investigate: src/guard/denial.rs Denial Serialize impl with_context().\n\
         Common causes: context map not included in serialization, or with_context() \
         not inserting into the context HashMap.\n\
         Run: cargo test --test gate_pipeline denial_serialize"
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
         Run: cargo test --test gate_pipeline gateset_default"
    );
}

#[test]
fn gateset_len_and_is_empty() {
    let mut gates = GateSet::<()>::new();
    assert!(
        gates.is_empty(),
        "PROPERTY: A newly created GateSet must be empty.\n\
         Investigate: src/guard/mod.rs GateSet::new() is_empty().\n\
         Common causes: GateSet::new() not initializing an empty inner collection, \
         or is_empty() always returning false.\n\
         Run: cargo test --test gate_pipeline gateset_len_and_is_empty"
    );
    assert_eq!(
        gates.len(),
        0,
        "PROPERTY: A newly created GateSet must have length 0.\n\
         Investigate: src/guard/mod.rs GateSet::new() len().\n\
         Common causes: len() returning 1 from a sentinel element, or GateSet \
         pre-populated with a default gate.\n\
         Run: cargo test --test gate_pipeline gateset_len_and_is_empty"
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
         Run: cargo test --test gate_pipeline gateset_len_and_is_empty"
    );
    assert_eq!(
        gates.len(),
        1,
        "PROPERTY: GateSet must have length 1 after pushing one gate.\n\
         Investigate: src/guard/mod.rs GateSet::push() len().\n\
         Common causes: push() pushing a boxed copy without increasing the count, \
         or len() reading a cached stale value.\n\
         Run: cargo test --test gate_pipeline gateset_len_and_is_empty"
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
         Run: cargo test --test gate_pipeline gate_description_default"
    );
}
