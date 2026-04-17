#![allow(
    clippy::print_stdout,
    clippy::wildcard_enum_match_arm,
    clippy::cast_possible_truncation
)] // example binary
//! # Event-Sourced Counter — from first principles
//!
//! The simplest possible event-sourced system: a counter that only goes up.
//! Instead of storing "count = 7", we store the *history* of increments.
//! The current value is derived by replaying history — that's event sourcing.
//!
//! Why bother? Because the log is append-only:
//! - You can audit every change ("who incremented, when, why?")
//! - You can rebuild state at any point in time
//! - Two systems can independently derive the same count from the same events
//! - You never lose information — a decrement is a new event, not an overwrite
//!
//! This example intentionally uses `JsonValueInput`, the ergonomic default
//! replay lane. When replay throughput matters more than projection simplicity,
//! compare it with `raw_projection_counter`.
//!
//! Run: `cargo run --example event_sourced_counter`

use batpak::prelude::*;
use serde::{Deserialize, Serialize};

// -- Step 1: Define event payload types. #[derive(EventPayload)] binds
//    each Rust struct to its EventKind at compile time, so callsites
//    never write EventKind::custom(...) again.
#[derive(Serialize, Deserialize, EventPayload)]
#[batpak(category = 1, type_id = 1)]
struct Incremented {
    amount: i64,
    reason: String,
}

#[derive(Serialize, Deserialize, EventPayload)]
#[batpak(category = 1, type_id = 2)]
struct Decremented {
    amount: i64,
    reason: String,
}

// -- Step 2: Define your projection (the "read model") --
// This is what you reconstruct by replaying events.
#[derive(Debug, Default, Serialize, Deserialize)]
struct CounterState {
    value: i64,
    total_increments: u32,
    total_decrements: u32,
}

// -- Step 3: Implement EventSourced to teach batpak how to fold events --
//
// The dispatch table here is hand-written: compare `kind` against each
// payload type's `KIND` constant, then deserialize into the right type.
// A future `#[derive(EventSourced)]` will generate this dispatch from
// per-handler attributes, but that's the next lock. For now the KIND
// constants keep the comparisons honest.
impl EventSourced for CounterState {
    type Input = batpak::prelude::JsonValueInput;

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        if events.is_empty() {
            return None;
        }
        let mut state = Self::default();
        for event in events {
            state.apply_event(event);
        }
        Some(state)
    }

    fn apply_event(&mut self, event: &Event<serde_json::Value>) {
        let kind = event.header.event_kind;
        if kind == Incremented::KIND {
            let p = serde_json::from_value::<Incremented>(event.payload.clone())
                .expect("CounterState::apply_event: Incremented payload decode");
            self.value += p.amount;
            self.total_increments += 1;
        } else if kind == Decremented::KIND {
            let p = serde_json::from_value::<Decremented>(event.payload.clone())
                .expect("CounterState::apply_event: Decremented payload decode");
            self.value += p.amount;
            self.total_decrements += 1;
        }
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        static KINDS: [EventKind; 2] = [Incremented::KIND, Decremented::KIND];
        &KINDS
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a temporary store (disappears when the program exits)
    let dir = tempfile::tempdir()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;

    // Our counter lives at this coordinate: entity "counter:hits", scope "example"
    let coord = Coordinate::new("counter:hits", "example")?;

    // -- Write some events --
    println!("Writing events...\n");

    store.append_typed(
        &coord,
        &Incremented {
            amount: 1,
            reason: "page view".into(),
        },
    )?;
    store.append_typed(
        &coord,
        &Incremented {
            amount: 5,
            reason: "bulk import".into(),
        },
    )?;
    store.append_typed(
        &coord,
        &Decremented {
            amount: -2,
            reason: "cleanup".into(),
        },
    )?;

    // -- Project: replay events to get current state --
    let state: Option<CounterState> =
        store.project::<CounterState>("counter:hits", &Freshness::Consistent)?;

    match state {
        Some(s) => {
            println!("Counter state (reconstructed from {} events):", 3);
            println!("  value:            {}", s.value);
            println!("  total_increments: {}", s.total_increments);
            println!("  total_decrements: {}", s.total_decrements);
            println!("  replay lane:      JsonValueInput (ergonomic default)");
        }
        None => println!("No events found!"),
    }

    // -- Query: browse the raw event log --
    println!("\nRaw event log:");
    let entries = store.stream("counter:hits");
    for entry in &entries {
        let stored = store.get(entry.event_id)?;
        println!(
            "  seq={} kind={} payload={}",
            entry.clock, entry.kind, stored.event.payload
        );
    }

    // -- Walk ancestors: trace causation backwards --
    if let Some(last) = entries.last() {
        println!("\nAncestor walk from last event:");
        let ancestors = store.walk_ancestors(last.event_id, 10);
        for (i, a) in ancestors.iter().enumerate() {
            println!(
                "  {}: kind={} payload={}",
                i, a.event.header.event_kind, a.event.payload
            );
        }
    }

    store.close()?;
    println!("\nDone. The event log told us the count is 4, and we know exactly why.");

    Ok(())
}
