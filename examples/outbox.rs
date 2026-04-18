//! # outbox
//!
//! **Teaches:** typed outbox staging for pre-commit item collection.
//!
//! Run: `cargo run --example outbox`

use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 3)]
struct Tick {
    n: u32,
}

// justifies: example main prints outbox events to stdout so the reader can see the staging-then-flush observable result.
#[allow(clippy::print_stdout)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;

    let mut outbox = store.outbox();
    outbox.stage(
        Coordinate::new("player:outbox", "room:batch")?,
        Tick::KIND,
        &Tick { n: 1 },
    )?;
    outbox.stage(
        Coordinate::new("player:outbox", "room:batch")?,
        Tick::KIND,
        &Tick { n: 2 },
    )?;
    outbox.stage(
        Coordinate::new("player:outbox", "room:batch")?,
        Tick::KIND,
        &Tick { n: 3 },
    )?;

    let receipts = outbox.flush()?;
    println!(
        "flushed {} staged events through one batch path",
        receipts.len()
    );

    store.close()?;
    Ok(())
}
