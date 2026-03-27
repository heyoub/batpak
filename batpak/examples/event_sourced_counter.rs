#![allow(clippy::print_stdout, clippy::wildcard_enum_match_arm, clippy::cast_possible_truncation)] // example binary
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
//! Run: `cargo run --example event_sourced_counter`

use batpak::prelude::*;
use serde::{Deserialize, Serialize};

// -- Step 1: Define your event kinds --
// EventKind uses category:type encoding. Category 1, types 1-2.
const INCREMENTED: EventKind = EventKind::custom(1, 1);
const DECREMENTED: EventKind = EventKind::custom(1, 2);

// -- Step 2: Define event payloads --
#[derive(Serialize, Deserialize)]
struct IncrementedBy {
    amount: i64,
    reason: String,
}

// -- Step 3: Define your projection (the "read model") --
// This is what you reconstruct by replaying events.
#[derive(Debug, Default, Serialize, Deserialize)]
struct CounterState {
    value: i64,
    total_increments: u32,
    total_decrements: u32,
}

// -- Step 4: Implement EventSourced to teach batpak how to fold events --
impl EventSourced<serde_json::Value> for CounterState {
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
        if kind == INCREMENTED || kind == DECREMENTED {
            // The store serializes payloads to msgpack bytes, so when read back
            // as serde_json::Value, the payload is an array of u8 values.
            // Extract the bytes and deserialize from msgpack.
            let bytes: Vec<u8> = match &event.payload {
                serde_json::Value::Array(arr) => arr
                    .iter()
                    .filter_map(|v| v.as_u64().map(|n| n as u8))
                    .collect(),
                _ => return,
            };
            if let Ok(payload) = rmp_serde::from_slice::<IncrementedBy>(&bytes) {
                self.value += payload.amount;
                if payload.amount > 0 {
                    self.total_increments += 1;
                } else {
                    self.total_decrements += 1;
                }
            }
        }
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[INCREMENTED, DECREMENTED]
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

    store.append(
        &coord,
        INCREMENTED,
        &IncrementedBy {
            amount: 1,
            reason: "page view".into(),
        },
    )?;
    store.append(
        &coord,
        INCREMENTED,
        &IncrementedBy {
            amount: 5,
            reason: "bulk import".into(),
        },
    )?;
    store.append(
        &coord,
        DECREMENTED,
        &IncrementedBy {
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
