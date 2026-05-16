// justifies: INV-EXAMPLES-OBSERVABLE-OUTPUT; example binary in examples/dungeon_typestate.rs demonstrates typestate transitions by printing the state trace to stdout; println is the user-observable success path.
#![allow(clippy::print_stdout)]
//! # dungeon_typestate
//!
//! **Teaches:** compile-time state machine enforcement via typestate transitions.
//!
//! A door in a dungeon can be Open, Closed, or Locked. Not every transition
//! makes sense: you can't lock an open door, and you can't open a locked door
//! without unlocking it first.
//!
//! Most programs check these rules at runtime with if-statements and hope for
//! the best. batpak's typestate system encodes them in the type system — the
//! compiler *refuses to compile* invalid transitions. Try uncommenting the
//! illegal transitions at the bottom to see it fail.
//!
//! This is "making illegal states unrepresentable" — if it compiles, the
//! state machine is correct by construction.
//!
//! Run: `cargo run --example dungeon_typestate`

use batpak::prelude::*;
use batpak::typestate::Transition;
use serde::{Deserialize, Serialize};

// -- Step 1: Define the state machine --
// This macro generates: a sealed trait `DoorState`, and zero-sized structs
// `Open`, `Closed`, `Locked` that implement it.
batpak::define_state_machine!(
    door_state_seal,
    DoorState {
        Open,
        Closed,
        Locked
    }
);

// -- Step 2: Define the typestate wrapper --
// This generates `Door<S: DoorState>` with a `name` field and PhantomData<S>.
// Door<Open> and Door<Closed> are *different types* — you can't confuse them.
batpak::define_typestate!(Door<S: DoorState> { name: String });

// -- Step 3: Define one event payload per legal transition. #[derive(EventPayload)]
// binds each struct to its EventKind at compile time — the Transition type then
// pulls the kind out of T::KIND via `Transition::from_payload`, so callsites
// never touch EventKind::custom(...) again.
#[derive(Serialize, Deserialize, EventPayload)]
#[batpak(category = 2, type_id = 1)]
struct DoorClosed {
    door_name: String,
}

#[derive(Serialize, Deserialize, EventPayload)]
#[batpak(category = 2, type_id = 2)]
struct DoorOpened {
    door_name: String,
}

#[derive(Serialize, Deserialize, EventPayload)]
#[batpak(category = 2, type_id = 3)]
struct DoorLocked {
    door_name: String,
}

#[derive(Serialize, Deserialize, EventPayload)]
#[batpak(category = 2, type_id = 4)]
struct DoorUnlocked {
    door_name: String,
}

// -- Step 4: Define legal transitions as methods on specific states --
// Only Door<Open> has a `close()` method. Door<Locked> doesn't.
// The type system enforces the state diagram.

impl Door<Open> {
    fn close(self) -> (Door<Closed>, Transition<Open, Closed, DoorClosed>) {
        let (name,) = self.into_data();
        let door = Door::<Closed>::new(name.clone());
        let transition = Transition::from_payload(DoorClosed { door_name: name });
        (door, transition)
    }
}

impl Door<Closed> {
    fn open(self) -> (Door<Open>, Transition<Closed, Open, DoorOpened>) {
        let (name,) = self.into_data();
        let door = Door::<Open>::new(name.clone());
        let transition = Transition::from_payload(DoorOpened { door_name: name });
        (door, transition)
    }

    fn lock(self, _key: &str) -> (Door<Locked>, Transition<Closed, Locked, DoorLocked>) {
        let (name,) = self.into_data();
        let door = Door::<Locked>::new(name.clone());
        let transition = Transition::from_payload(DoorLocked { door_name: name });
        (door, transition)
    }
}

impl Door<Locked> {
    fn unlock(self, _key: &str) -> (Door<Closed>, Transition<Locked, Closed, DoorUnlocked>) {
        let (name,) = self.into_data();
        let door = Door::<Closed>::new(name.clone());
        let transition = Transition::from_payload(DoorUnlocked { door_name: name });
        (door, transition)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;
    let coord = Coordinate::new("door:vault", "dungeon:level-3")?;

    println!("=== Dungeon Door State Machine ===\n");

    // Start with an open door
    let door = Door::<Open>::new("Vault Door".into());
    println!("Door '{}' starts Open", door.name());

    // Close it — this returns a new Door<Closed> and a Transition event
    let (door, transition) = door.close();
    store.apply_transition(&coord, transition)?;
    println!("  → Closed (event persisted)");

    // Lock it
    let (door, transition) = door.lock("skeleton-key");
    store.apply_transition(&coord, transition)?;
    println!("  → Locked with skeleton-key (event persisted)");

    // Unlock it
    let (door, transition) = door.unlock("skeleton-key");
    store.apply_transition(&coord, transition)?;
    println!("  → Unlocked (event persisted)");

    // Open it
    let (_door, transition) = door.open();
    store.apply_transition(&coord, transition)?;
    println!("  → Open again (event persisted)");

    // -- Show the event log --
    println!("\nEvent log for vault door:");
    for entry in store.stream("door:vault") {
        let stored = store.get(entry.event_id)?;
        println!("  kind={} payload={}", entry.kind, stored.event.payload);
    }

    // -- ILLEGAL TRANSITIONS --
    // Uncomment any of these to see the compiler reject them:

    // Can't lock an open door (must close first):
    // let open_door = Door::<Open>::new("test".into());
    // open_door.lock("key");  // ERROR: no method named `lock` found for `Door<Open>`

    // Can't open a locked door (must unlock first):
    // let locked_door = Door::<Locked>::new("test".into());
    // locked_door.open();  // ERROR: no method named `open` found for `Door<Locked>`

    store.close()?;
    println!("\nThe compiler guarantees every door transition is legal.");
    println!("Try uncommenting the illegal transitions to see it fail!");

    Ok(())
}
