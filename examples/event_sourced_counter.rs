// justifies: example binary demonstrates counter output via println, matches only the variants used in the demo with a wildcard fallback, and narrows bounded demo counters to smaller integer types.
#![allow(
    clippy::print_stdout,
    clippy::wildcard_enum_match_arm,
    clippy::cast_possible_truncation
)]
//! # event_sourced_counter
//!
//! **Teaches:** `#[derive(EventSourced)]` with `JsonValueInput` replay lane.
//!
//! ## From first principles
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
//! This example uses `JsonValueInput`, the ergonomic default replay lane.
//! When replay throughput matters more than projection simplicity, compare it
//! with `raw_projection_counter`, which uses the same derive on the
//! raw-msgpack lane via `input = RawMsgpackInput`.
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

// -- Step 2: Define your projection (the "read model") + bind events to
//    handler methods via #[derive(EventSourced)].
//
// The derive generates:
//   - `type Input = JsonValueInput`
//   - `from_events` (default fold over Default::default())
//   - `apply_event` — dispatches by kind via DecodeTyped::route_typed
//   - `relevant_event_kinds` — one source of truth, generated from the
//     `event =` list; the sync-drift bug against `apply_event` is
//     structurally impossible.
//   - `schema_version` — from `cache_version` (projection cache invalidation
//     only; unrelated to payload wire `type_id`).
#[derive(Debug, Default, Serialize, Deserialize, EventSourced)]
#[batpak(input = JsonValueInput, cache_version = 0)]
#[batpak(event = Incremented, handler = on_incremented)]
#[batpak(event = Decremented, handler = on_decremented)]
struct CounterState {
    value: i64,
    total_increments: u32,
    total_decrements: u32,
}

impl CounterState {
    fn on_incremented(&mut self, p: &Incremented) {
        self.value += p.amount;
        self.total_increments += 1;
        let _ = &p.reason; // keep the field for audit log; example doesn't use it here.
    }

    fn on_decremented(&mut self, p: &Decremented) {
        // Decremented payloads carry a negative amount by convention.
        self.value += p.amount;
        self.total_decrements += 1;
        let _ = &p.reason;
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
