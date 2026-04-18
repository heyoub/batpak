//! Compile-fail: Attempting a typestate transition with a type that doesn't
//! implement the sealed state machine trait.
//!
//! Two sealed bounds are enforced here post-FREEZE-7:
//!
//!   1. `From`/`To` type parameters must implement the sealed `StateMarker`
//!      super-trait, which only `define_state_machine!`-generated types can
//!      satisfy (`String` cannot).
//!   2. `Transition::from_payload(payload: P)` requires `P: EventPayload`.
//!      Raw `String` does not implement `EventPayload`, so the ergonomic
//!      old `new(kind, payload)` shape is unreachable — there is no
//!      constructor that accepts an independent `EventKind` argument.
//!
//! Either bound failing is sufficient for this fixture; we deliberately
//! violate both so the stderr is richer.

use batpak::typestate::{StateMarker, Transition};

// Define a state machine with two states via the macro.
batpak::define_state_machine!(lock_state_seal, LockState { Acquired, Released });

// A function that only accepts transitions FROM a valid LockState.
// The payload parameter is still `String` — this in isolation is fine (the
// `Transition` struct itself does not bound `P` at the type level), but the
// sole constructor `from_payload` requires `P: EventPayload`, which is where
// the second compile error surfaces.
fn apply_transition<From: LockState + StateMarker, To: LockState + StateMarker>(
    _transition: Transition<From, To, String>,
) {
}

fn main() {
    // VALID (commented for clarity): Acquired -> Released would only
    // compile if `String: EventPayload`, which it is not. It is left here
    // as a reference for the valid transition shape.
    // apply_transition(Transition::<Acquired, Released, SomePayload>::from_payload(payload));

    // INVALID #1: `String: !StateMarker` — the first type parameter is
    // not a member of the sealed state machine.
    // INVALID #2: `String: !EventPayload` — `from_payload` rejects the
    // payload even if the state parameters were valid.
    apply_transition(Transition::<String, Released, String>::from_payload("bad".to_string()));
}
