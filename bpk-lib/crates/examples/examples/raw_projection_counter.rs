//! # raw_projection_counter
//!
//! **Teaches:** hand-written raw projection replay for performance-sensitive folds.
//!
//! This example is the intentional hand-written counterpart to
//! `raw_projection_counter_derived.rs`; production code should use
//! `#[derive(EventSourced)]` with `input = RawMsgpackInput`. It is kept as a
//! reference for what the derive replaces.
//!
//! Same event-sourced counter idea as `event_sourced_counter`, but the
//! projection chooses batpak's raw replay lane instead of eagerly decoding
//! each payload into `serde_json::Value`.
//!
//! Reach for this lane when replay cost matters and your projection can own
//! the MessagePack decoding step directly. The current quick replay-lane bench
//! in this repo consistently shows this pattern ahead of the JSON replay lane
//! on the 1k-event counter-shaped workload.
//!
//! Run: `cargo run --example raw_projection_counter`

use batpak::prelude::*;
use serde::{Deserialize, Serialize};

const INCREMENTED: EventKind = EventKind::custom(1, 1);
const DECREMENTED: EventKind = EventKind::custom(1, 2);

#[derive(Debug, Serialize, Deserialize)]
struct Delta {
    amount: i64,
    reason: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RawCounterState {
    value: i64,
    total_events: u32,
}

impl EventSourced for RawCounterState {
    type Input = RawMsgpackInput;
    const STATE_CONTRACT: ProjectionStateContract =
        ProjectionStateContract::single_entity("raw-projection-counter-example");

    fn from_events(events: &[Event<Vec<u8>>]) -> Option<Self> {
        if events.is_empty() {
            return None;
        }
        let mut state = Self::default();
        for event in events {
            state.apply_event(event);
        }
        Some(state)
    }

    fn apply_event(&mut self, event: &Event<Vec<u8>>) {
        if event.header.event_kind != INCREMENTED && event.header.event_kind != DECREMENTED {
            return;
        }
        let delta = rmp_serde::from_slice::<Delta>(&event.payload)
            .expect("RawCounterState::apply_event expects replay payloads that decode as Delta");
        self.value += delta.amount;
        self.total_events += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[INCREMENTED, DECREMENTED]
    }

    fn state_extent(&self) -> StateExtent {
        StateExtent::single_entity()
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();

    let dir = tempfile::tempdir()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;
    let coord = Coordinate::new("counter:raw", "example")?;

    let _ = store.append(
        &coord,
        INCREMENTED,
        &Delta {
            amount: 3,
            reason: "signup".into(),
        },
    )?;
    let _ = store.append(
        &coord,
        DECREMENTED,
        &Delta {
            amount: -1,
            reason: "cleanup".into(),
        },
    )?;
    let _ = store.append(
        &coord,
        INCREMENTED,
        &Delta {
            amount: 2,
            reason: "bonus".into(),
        },
    )?;

    let state: Option<RawCounterState> = store.project("counter:raw", &Freshness::Consistent)?;

    if let Some(state) = state {
        let _ = writeln!(out, "Raw replay projection state:");
        let _ = writeln!(out, "  value:        {}", state.value);
        let _ = writeln!(out, "  total_events: {}", state.total_events);
        let _ = writeln!(out, "  replay lane:  RawMsgpackInput (performance lane)");
    }

    store.close()?;
    Ok(())
}
