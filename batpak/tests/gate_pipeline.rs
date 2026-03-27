//! Gate and Pipeline integration tests.
//! Registration order, fail-fast evaluation, Receipt TOCTOU guarantee, consumed-once.
//! [SPEC:tests/gate_pipeline.rs]
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
    assert!(
        gates.evaluate(&(-1), proposal_fail).is_err(),
        "CONTEXT GATE: non-positive context should be denied by ContextGate.\n\
         Investigate: src/guard/mod.rs Gate::evaluate context usage.\n\
         Common causes: context value not passed through to gate, comparison logic inverted.\n\
         Run: cargo test --test gate_pipeline context_gate_uses_context"
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
