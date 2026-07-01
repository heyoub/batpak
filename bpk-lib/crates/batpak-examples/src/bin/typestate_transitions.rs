//! # typestate_transitions
//!
//! **Teaches:** compile-time state machine enforcement via typestate transitions.
//!
//! A resource can be Idle, Active, or Sealed. Not every transition makes sense:
//! you can't seal an active resource, and you can't activate a sealed resource
//! without unsealing it first.
//!
//! Most programs check these rules at runtime with if-statements and hope for
//! the best. batpak's typestate system encodes them in the type system — the
//! compiler *refuses to compile* invalid transitions. Try uncommenting the
//! illegal transitions at the bottom to see it fail.
//!
//! This is "making illegal states unrepresentable" — if it compiles, the
//! state machine is correct by construction.
//!
//! Run: `cargo run -p batpak-examples --bin typestate_transitions`

use batpak::prelude::*;
use batpak::typestate::Transition;
use serde::{Deserialize, Serialize};

// -- Step 1: Define the state machine --
// This macro generates: a sealed trait `ResourceState`, and zero-sized structs
// `Idle`, `Active`, `Sealed` that implement it.
batpak::define_state_machine!(
    resource_state_seal,
    ResourceState {
        Idle,
        Active,
        Sealed
    }
);

// -- Step 2: Define the typestate wrapper --
// This generates `Resource<S: ResourceState>` with a `name` field and PhantomData<S>.
// Resource<Idle> and Resource<Active> are *different types* — you can't confuse them.
batpak::define_typestate!(Resource<S: ResourceState> { name: String });

// -- Step 3: Define one event payload per legal transition. #[derive(EventPayload)]
// binds each struct to its EventKind at compile time — the Transition type then
// pulls the kind out of T::KIND via `Transition::from_payload`, so callsites
// never touch EventKind::custom(...) again.
#[derive(Serialize, Deserialize, EventPayload)]
#[batpak(category = 2, type_id = 1)]
struct Deactivated {
    name: String,
}

#[derive(Serialize, Deserialize, EventPayload)]
#[batpak(category = 2, type_id = 2)]
struct Activated {
    name: String,
}

#[derive(Serialize, Deserialize, EventPayload)]
#[batpak(category = 2, type_id = 3)]
struct SealSet {
    name: String,
}

#[derive(Serialize, Deserialize, EventPayload)]
#[batpak(category = 2, type_id = 4)]
struct SealCleared {
    name: String,
}

// -- Step 4: Define legal transitions as methods on specific states --
// Only Resource<Active> has a `deactivate()` method. Resource<Sealed> doesn't.
// The type system enforces the state diagram.

impl Resource<Active> {
    fn deactivate(self) -> (Resource<Idle>, Transition<Active, Idle, Deactivated>) {
        let (name,) = self.into_data();
        let resource = Resource::<Idle>::new(name.clone());
        let transition = Transition::from_payload(Deactivated { name });
        (resource, transition)
    }
}

impl Resource<Idle> {
    fn activate(self) -> (Resource<Active>, Transition<Idle, Active, Activated>) {
        let (name,) = self.into_data();
        let resource = Resource::<Active>::new(name.clone());
        let transition = Transition::from_payload(Activated { name });
        (resource, transition)
    }

    fn seal(self, _token: &str) -> (Resource<Sealed>, Transition<Idle, Sealed, SealSet>) {
        let (name,) = self.into_data();
        let resource = Resource::<Sealed>::new(name.clone());
        let transition = Transition::from_payload(SealSet { name });
        (resource, transition)
    }
}

impl Resource<Sealed> {
    fn unseal(self, _token: &str) -> (Resource<Idle>, Transition<Sealed, Idle, SealCleared>) {
        let (name,) = self.into_data();
        let resource = Resource::<Idle>::new(name.clone());
        let transition = Transition::from_payload(SealCleared { name });
        (resource, transition)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();

    let dir = tempfile::tempdir()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;
    let coord = Coordinate::new("entity:resource", "scope:main")?;

    let _ = writeln!(out, "=== Resource State Machine ===\n");

    // Start with an active resource
    let resource = Resource::<Active>::new("R1".into());
    let _ = writeln!(out, "Resource '{}' starts Active", resource.name());

    // Deactivate it — this returns a new Resource<Idle> and a Transition event
    let (resource, transition) = resource.deactivate();
    let _ = store.apply_transition(&coord, transition)?;
    let _ = writeln!(out, "  → Idle (event persisted)");

    // Seal it
    let (resource, transition) = resource.seal("token");
    let _ = store.apply_transition(&coord, transition)?;
    let _ = writeln!(out, "  → Sealed with token (event persisted)");

    // Unseal it
    let (resource, transition) = resource.unseal("token");
    let _ = store.apply_transition(&coord, transition)?;
    let _ = writeln!(out, "  → Unsealed (event persisted)");

    // Activate it
    let (_resource, transition) = resource.activate();
    let _ = store.apply_transition(&coord, transition)?;
    let _ = writeln!(out, "  → Active again (event persisted)");

    // -- Show the event log --
    let _ = writeln!(out, "\nEvent log for the resource:");
    for entry in store.by_entity("entity:resource") {
        let stored = store.get(entry.event_id())?;
        let _ = writeln!(
            out,
            "  kind={} payload={}",
            entry.event_kind(),
            stored.event.payload
        );
    }

    // -- ILLEGAL TRANSITIONS --
    // Uncomment any of these to see the compiler reject them:

    // Can't seal an active resource (must deactivate first):
    // let active = Resource::<Active>::new("test".into());
    // active.seal("token");  // ERROR: no method named `seal` found for `Resource<Active>`

    // Can't activate a sealed resource (must unseal first):
    // let sealed = Resource::<Sealed>::new("test".into());
    // sealed.activate();  // ERROR: no method named `activate` found for `Resource<Sealed>`

    store.close()?;
    let _ = writeln!(
        out,
        "\nThe compiler guarantees every resource transition is legal."
    );
    let _ = writeln!(
        out,
        "Try uncommenting the illegal transitions to see it fail!"
    );

    Ok(())
}
