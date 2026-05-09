//! Compile-fail: Attempting a typestate transition with a type that doesn't
//! implement the sealed state machine trait.
//!
//! Two sealed bounds are enforced here post-FREEZE-7:
//!
//!   1. `From`/`To` type parameters must implement the sealed `StateMarker`
//!      super-trait, which only `define_state_machine!`-generated types can
//!      satisfy (`String` cannot).
//!   2. `Transition<From, To, P>` itself requires `P: EventPayload`.
//!      A payload type that does not implement `EventPayload` cannot appear
//!      in a transition at all.
//!
//! We deliberately violate both bounds in one type so the compiler surfaces
//! the sealed-state and typed-payload protections together.

use batpak::typestate::Transition;

// Define a state machine with two states via the macro.
batpak::define_state_machine!(lock_state_seal, LockState { Acquired, Released });

struct InvalidPayload;

fn main() {
    let _bad: Option<Transition<String, Released, InvalidPayload>> = None;
}
