//! # raw_projection_counter_derived
//!
//! **Teaches:** derive with `input = RawMsgpackInput` (raw msgpack replay lane).
//!
//! Same counter as [`event_sourced_counter`], but the projection selects the
//! raw MessagePack replay lane via `#[batpak(input = RawMsgpackInput)]`.
//!
//! This example exists as the lane-parity proof for `#[derive(EventSourced)]`:
//! the only line that changes between the JSON and msgpack variants is the
//! `input =` attribute value. Handler signatures, dispatch semantics,
//! compile-time `relevant_event_kinds()` generation, and decode-failure
//! policy are identical.
//!
//! For the intentionally hand-written raw counterpoint (showing the pattern
//! the derive replaces), see `raw_projection_counter.rs`.
//!
//! Run: `cargo run --example raw_projection_counter_derived`

use batpak::prelude::*;
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Default, Serialize, Deserialize, EventSourced)]
#[batpak(input = RawMsgpackInput, cache_version = 0, state_max_cardinality = 1)]
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
        let _ = &p.reason;
    }

    fn on_decremented(&mut self, p: &Decremented) {
        self.value += p.amount;
        self.total_decrements += 1;
        let _ = &p.reason;
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();

    let dir = tempfile::tempdir()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;
    let coord = Coordinate::new("counter:raw-derived", "example")?;

    let _ = store.append_typed(
        &coord,
        &Incremented {
            amount: 1,
            reason: "page view".into(),
        },
    )?;
    let _ = store.append_typed(
        &coord,
        &Incremented {
            amount: 5,
            reason: "bulk import".into(),
        },
    )?;
    let _ = store.append_typed(
        &coord,
        &Decremented {
            amount: -2,
            reason: "cleanup".into(),
        },
    )?;

    let state: Option<CounterState> =
        store.project::<CounterState>("counter:raw-derived", &Freshness::Consistent)?;

    match state {
        Some(s) => {
            let _ = writeln!(
                out,
                "Counter state (reconstructed via RawMsgpackInput lane):"
            );
            let _ = writeln!(out, "  value:            {}", s.value);
            let _ = writeln!(out, "  total_increments: {}", s.total_increments);
            let _ = writeln!(out, "  total_decrements: {}", s.total_decrements);
        }
        None => {
            let _ = writeln!(out, "No events found!");
        }
    }

    store.close()?;
    let _ = writeln!(out, "\nDone. Same derive, different lane — same result.");
    Ok(())
}
