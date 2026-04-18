// justifies: example binary demonstrates typed-reactor observable output via println and spawns worker threads via std::thread::spawn as part of the demo.
#![allow(clippy::print_stdout, clippy::disallowed_methods)]
//! # typed_reactor
//!
//! **Teaches:** `react_loop_typed<T, R>` with matched-kind decode failure
//! surfacing as `ReactorError::Decode` and the observable-state wait pattern.
//!
//! Demonstrates a typed reactor that watches for `PayloadA` events and emits
//! one `PayloadB` reaction per source event, atomically flushed via
//! `ReactionBatch`. The main thread waits on observable state
//! (`by_fact_typed::<PayloadB>().len() >= 4`) before stopping the reactor —
//! no pre-stop sleeps.
//!
//! Semantics (see ADR-0011):
//!   * At-least-once delivery via `cursor_guaranteed` — never drops events.
//!   * Wrong-kind events are filtered silently (no handler call, no error).
//!   * Matched-kind decode failures stop the loop with
//!     `ReactorError::Decode` (hard correctness signal).
//!   * User handler returns `Err` → `ReactorError::User` surfaced via
//!     `handle.join()`; the `ReactionBatch` is dropped (no partial commits).
//!
//! Run: `cargo run --example typed_reactor`

use batpak::event::StoredEvent;
use batpak::prelude::*;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Poll `cond` every 10ms until it returns true or the deadline elapses.
/// The inner sleep is a polling interval, not a pre-join fixed delay.
fn wait_for(cond: impl Fn() -> bool, timeout: Duration) -> Result<(), &'static str> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err("timed out waiting for condition")
}

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xA, type_id = 1)]
struct PayloadA {
    n: u64,
}

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xA, type_id = 2)]
struct PayloadB {
    derived_from: u64,
    doubled: u64,
}

/// Reactor state: a counter plus the pre-built coordinate reactions are
/// written to. Building the coordinate once at `main()` level keeps the
/// handler fallible only via real runtime errors (batch push → StoreError).
struct Doubler {
    seen: u64,
    reaction_coord: Coordinate,
}

impl TypedReactive<PayloadA> for Doubler {
    type Error = StoreError;
    fn react(
        &mut self,
        event: &StoredEvent<PayloadA>,
        out: &mut ReactionBatch,
    ) -> Result<(), Self::Error> {
        self.seen += 1;
        out.push_typed(
            self.reaction_coord.clone(),
            &PayloadB {
                derived_from: event.event.payload.n,
                doubled: event.event.payload.n * 2,
            },
            CausationRef::None,
        )
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = Arc::new(Store::open(StoreConfig::new(dir.path()))?);

    let reaction_coord = Coordinate::new("entity:reactions", "scope:example")?;
    let source = Coordinate::new("entity:sources", "scope:example")?;

    // Start the reactor loop. The cursor canal is pull-based: it re-scans
    // the indexed event log and will observe matching events written before
    // or after the loop starts, subject to its at-least-once contract.
    let handle = store.react_loop_typed::<PayloadA, _>(
        &Region::all(),
        ReactorConfig::default(),
        Doubler {
            seen: 0,
            reaction_coord,
        },
    )?;

    // Write a few source events.
    for n in [1, 2, 3, 7] {
        store.append_typed(&source, &PayloadA { n })?;
    }

    // Wait until the reactor has produced one PayloadB per source event.
    wait_for(
        || store.by_fact_typed::<PayloadB>().len() >= 4,
        Duration::from_secs(5),
    )
    .expect("reactor reacted in time");

    // Stop the reactor cleanly; surface any reactor error through main's
    // Result for honest exit codes.
    if let Err(e) = handle.stop_and_join() {
        return Err(format!("reactor join failed: {e}").into());
    }

    // Inspect the derived events.
    let reactions = store.by_fact_typed::<PayloadB>();
    println!(
        "Typed reactor emitted {} reactions for 4 source events:",
        reactions.len()
    );
    for entry in &reactions {
        let stored = store.get(entry.event_id)?;
        println!(
            "  reaction event_id={} payload={}",
            entry.event_id, stored.event.payload
        );
    }

    drop(store);
    Ok(())
}
