//! Compile-fail: Attempting a typestate transition with a type that doesn't
//! implement the sealed state machine trait.
//! The define_state_machine! macro creates a sealed trait that external types
//! cannot implement. This ensures only declared states are valid transition
//! endpoints.

use batpak::typestate::Transition;
use batpak::prelude::EventKind;

// Define a state machine with two states via the macro.
batpak::define_state_machine!(LockState { Acquired, Released });

// A function that only accepts transitions FROM a valid LockState.
fn apply_transition<From: LockState, To: LockState>(
    _transition: Transition<From, To, String>,
) {}

fn main() {
    let kind = EventKind::custom(0xF, 1);

    // VALID: Acquired -> Released (both implement LockState)
    // This line would compile, but we comment it to focus on the failure:
    // apply_transition(Transition::<Acquired, Released, String>::new(kind, "ok".into()));

    // INVALID: String does not implement LockState (sealed trait).
    // This must fail to compile, proving the sealed trait enforcement works.
    apply_transition(Transition::<String, Released, String>::new(kind, "bad".into()));
}
