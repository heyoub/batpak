//! # outbox
//!
//! **Teaches:** typed outbox staging for pre-commit item collection.
//!
//! Run: `cargo run -p batpak-examples --bin outbox`

use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 3)]
struct Tick {
    n: u32,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();

    let dir = tempfile::tempdir()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;

    let mut outbox = store.outbox();
    outbox.stage(
        Coordinate::new("entity:outbox", "scope:batch")?,
        Tick::KIND,
        &Tick { n: 1 },
    )?;
    outbox.stage(
        Coordinate::new("entity:outbox", "scope:batch")?,
        Tick::KIND,
        &Tick { n: 2 },
    )?;
    outbox.stage(
        Coordinate::new("entity:outbox", "scope:batch")?,
        Tick::KIND,
        &Tick { n: 3 },
    )?;

    let receipts = outbox.flush()?;
    let _ = writeln!(
        out,
        "flushed {} staged events through one batch path",
        receipts.len()
    );

    store.close()?;
    Ok(())
}
