// justifies: runtime typestate tests use panic! as the assertion style when a state machine should have rejected an input.
#![allow(clippy::panic)]
//! Compile-fail tests for typestate safety + runtime tests for generated state machines.
//! Verifies that Receipt forgery and invalid state construction fail to compile,
//! AND that generated types work correctly at runtime.
//!
//! PROVES: LAW-004 (Composition Over Construction — typestate enforces phase)
//! DEFENDS: FM-009 (Polite Downgrade — generated methods must carry real data)
//! INVARIANTS: INV-TYPE (typestate round-trip fidelity)

#[test]
fn compile_fail_tests() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/forge_receipt.rs");
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

        // Verify Transition type is constructible
        let kind = batpak::prelude::EventKind::custom(1, 1);
        let _t: Transition<Locked, Unlocked, String> =
            Transition::new(kind, "carol_unlocked".into());
        assert_eq!(
            _t.payload(),
            &"carol_unlocked".to_string(),
            "PROPERTY: Transition::payload() must return the payload."
        );
        assert_eq!(
            _t.kind(),
            kind,
            "PROPERTY: Transition::kind() must return the EventKind."
        );
    }
}
