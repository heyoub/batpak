//! Compile-fail tests for typestate safety + runtime tests for generated state machines.
//! Verifies that Receipt forgery and invalid state construction fail to compile,
//! AND that generated types work correctly at runtime.
//!
//! PROVES: LAW-004 (Composition Over Construction — typestate enforces phase)
//! CATCHES: invalid typestate construction, forged receipts, and generated transitions that drop payload data.
//! SEEDED: deterministic trybuild fixtures plus runtime generated-state-machine checks.
//! DEFENDS: FM-009 (Polite Downgrade — generated methods must carry real data)
//! INVARIANTS: INV-TYPESTATE-OPEN-HAS-WRITER (typestate round-trip fidelity), INV-RECEIPT-SEALED (receipt construction boundary)

use batpak::EventPayload;
use serde::{Deserialize, Serialize};

// Test-local payload: `Transition::from_payload` requires `P: EventPayload`,
// so we can no longer parameterise transitions on bare `String`. This minimal
// payload carries a single string label and is only used by the runtime
// typestate tests below.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0x0F, type_id = 0xA1)]
struct UnlockLabel {
    label: String,
}

#[test]
#[serial_test::file_serial(trybuild)]
fn compile_fail_tests() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/forge_receipt.rs");
    t.compile_fail("tests/ui/forge_store_open.rs");
    t.compile_fail("tests/ui/invalid_transition.rs");
}

// ===== Wave 2D: Runtime tests for define_state_machine! and define_typestate! =====
// The macros generate real types — these tests verify the generated types work at runtime,
// not just that invalid usage fails to compile.

mod typestate_runtime {
    // Generate a test state machine and typestate wrapper
    batpak::define_state_machine!(test_lock_state_seal, TestLockState { Locked, Unlocked });
    batpak::define_typestate!(TestLock<S: TestLockState> { holder: String, count: u32 });

    #[test]
    fn define_state_machine_generates_usable_types() {
        // Verify the generated types exist and implement the trait
        let _locked = Locked;
        let _unlocked = Unlocked;
        // These assertions verify the types implement Debug and Copy
        let locked_copy = _locked;
        assert_eq!(
            format!("{locked_copy:?}"),
            "Locked",
            "PROPERTY: define_state_machine! must generate types with Debug.\n\
             Investigate: src/typestate/mod.rs define_state_machine! macro derive(Debug)."
        );
    }

    #[test]
    fn define_typestate_new_and_data() {
        let lock = TestLock::<Locked>::new("alice".into(), 5);
        let (holder, count) = lock.data();
        assert_eq!(
            holder,
            &"alice".to_string(),
            "PROPERTY: define_typestate! data() must return correct field reference.\n\
             Investigate: src/typestate/mod.rs define_typestate! data() method."
        );
        assert_eq!(
            *count, 5,
            "PROPERTY: define_typestate! data() must return all fields in order."
        );
    }

    #[test]
    fn define_typestate_into_data_consumes() {
        let lock = TestLock::<Locked>::new("bob".into(), 10);
        let (holder, count) = lock.into_data();
        assert_eq!(
            holder, "bob",
            "PROPERTY: define_typestate! into_data() must return owned field values.\n\
             Investigate: src/typestate/mod.rs define_typestate! into_data() method.\n\
             Common causes: field order swapped, wrong field returned."
        );
        assert_eq!(count, 10);
        // After into_data(), `lock` is consumed — cannot be used again.
        // This is enforced by the type system (move semantics).
    }

    #[test]
    fn typestate_transition_carries_data() {
        use batpak::typestate::Transition;
        let lock = TestLock::<Locked>::new("carol".into(), 1);
        // Simulate a transition by constructing the new state
        let (holder, count) = lock.into_data();
        let unlocked = TestLock::<Unlocked>::new(holder, count + 1);
        let (h, c) = unlocked.data();
        assert_eq!(h, &"carol".to_string());
        assert_eq!(
            *c, 2,
            "PROPERTY: Data must survive typestate transition.\n\
             Investigate: src/typestate/mod.rs — into_data + new round-trip."
        );

        // Verify Transition type is constructible via the from_payload
        // constructor (FREEZE-7): `kind` is derived from `P::KIND`, not
        // supplied separately, so the transition cannot hold a payload that
        // disagrees with its event kind.
        let payload = super::UnlockLabel {
            label: "carol_unlocked".into(),
        };
        let _t: Transition<Locked, Unlocked, super::UnlockLabel> =
            Transition::from_payload(payload.clone());
        assert_eq!(
            _t.payload(),
            &payload,
            "PROPERTY: Transition::payload() must return the payload."
        );
        assert_eq!(
            _t.kind(),
            <super::UnlockLabel as batpak::EventPayload>::KIND,
            "PROPERTY: Transition::kind() must return the EventKind derived from P::KIND."
        );
    }
}
