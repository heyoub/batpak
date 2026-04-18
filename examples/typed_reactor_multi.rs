// justifies: example binary demonstrates observable output via println and spawns worker threads via std::thread::spawn as part of the teaching scenario.
#![allow(clippy::print_stdout, clippy::disallowed_methods)]
//! # typed_reactor_multi
//!
//! **Teaches:** `react_loop_multi` with multi-event dispatch via
//! `#[derive(MultiEventReactor)]`.
//!
//! `#[derive(MultiEventReactor)]` + `store.react_loop_multi`: a single
//! reactor bound to multiple payload types, dispatched through the shared
//! canal runner (same plumbing as `react_loop_typed`). One source event in
//! the relevant-kinds set produces one atomic `ReactionBatch` commit.
//!
//! The main thread waits on observable state
//! (`by_fact_typed::<Reaction>().len() >= 4`) before stopping the reactor —
//! no pre-stop sleeps.
//!
//! Decode semantics match the single-kind typed reactor exactly (one unified
//! contract):
//!   * Wrong-kind events (kinds outside `relevant_event_kinds()`) are
//!     filtered silently.
//!   * Matched-kind decode failures surface as `ReactorError::Decode` —
//!     hard correctness signal, never a silent skip.
//!   * User handler errors surface as `ReactorError::User`.
//!
//! Run: `cargo run --example typed_reactor_multi`

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
#[batpak(category = 0xB, type_id = 1)]
struct PayloadA {
    n: u64,
}

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xB, type_id = 2)]
struct PayloadB {
    label: String,
}

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xB, type_id = 3)]
struct PayloadC {
    amount: i64,
}

/// Reaction emitted by the multi-reactor — tagged with which kind triggered it.
#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xB, type_id = 10)]
struct Reaction {
    triggered_by: String,
}

/// Reactor state: per-kind counters plus the pre-built output coordinate
/// so handlers can use `?` instead of `.unwrap()` on batch pushes.
#[derive(MultiEventReactor)]
#[batpak(input = JsonValueInput, error = StoreError)]
#[batpak(event = PayloadA, handler = on_a)]
#[batpak(event = PayloadB, handler = on_b)]
#[batpak(event = PayloadC, handler = on_c)]
struct MultiReactor {
    a_seen: u64,
    b_seen: u64,
    c_seen: u64,
    reaction_coord: Coordinate,
}

impl MultiReactor {
    fn on_a(
        &mut self,
        _event: &StoredEvent<PayloadA>,
        out: &mut ReactionBatch,
    ) -> Result<(), StoreError> {
        self.a_seen += 1;
        self.emit(out, "PayloadA")
    }
    fn on_b(
        &mut self,
        _event: &StoredEvent<PayloadB>,
        out: &mut ReactionBatch,
    ) -> Result<(), StoreError> {
        self.b_seen += 1;
        self.emit(out, "PayloadB")
    }
    fn on_c(
        &mut self,
        _event: &StoredEvent<PayloadC>,
        out: &mut ReactionBatch,
    ) -> Result<(), StoreError> {
        self.c_seen += 1;
        self.emit(out, "PayloadC")
    }

    fn emit(&self, out: &mut ReactionBatch, tag: &str) -> Result<(), StoreError> {
        out.push_typed(
            self.reaction_coord.clone(),
            &Reaction {
                triggered_by: tag.into(),
            },
            CausationRef::None,
        )
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = Arc::new(Store::open(StoreConfig::new(dir.path()))?);

    let source = Coordinate::new("entity:sources", "scope:example")?;
    let reaction_coord = Coordinate::new("entity:reactions", "scope:example")?;

    let handle = store.react_loop_multi(
        &Region::all(),
        ReactorConfig::default(),
        MultiReactor {
            a_seen: 0,
            b_seen: 0,
            c_seen: 0,
            reaction_coord,
        },
    )?;

    store.append_typed(&source, &PayloadA { n: 1 })?;
    store.append_typed(
        &source,
        &PayloadB {
            label: "mid".into(),
        },
    )?;
    store.append_typed(&source, &PayloadC { amount: 5 })?;
    store.append_typed(&source, &PayloadA { n: 2 })?;

    wait_for(
        || store.by_fact_typed::<Reaction>().len() >= 4,
        Duration::from_secs(5),
    )
    .expect("reactor reacted in time");
    if let Err(e) = handle.stop_and_join() {
        return Err(format!("reactor join failed: {e}").into());
    }

    let reactions = store.by_fact_typed::<Reaction>();
    println!(
        "Multi-event reactor emitted {} reactions across 3 payload kinds",
        reactions.len()
    );

    drop(store);
    Ok(())
}
