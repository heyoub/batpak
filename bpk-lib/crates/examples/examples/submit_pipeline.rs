//! # submit_pipeline
//!
//! **Teaches:** async submit ticket with blocking wait.
//!
//! Run: `cargo run --example submit_pipeline`

use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 2)]
struct Tick {
    n: u32,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();

    let dir = tempfile::tempdir()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;

    let coord = Coordinate::new("player:submit", "room:pipeline")?;

    let first = store.submit_typed(&coord, &Tick { n: 1 })?;
    let second = store.submit_typed(&coord, &Tick { n: 2 })?;
    let third = store.submit_typed(&coord, &Tick { n: 3 })?;

    let receipts = [first.wait()?, second.wait()?, third.wait()?];
    let _ = writeln!(
        out,
        "queued {} appends and committed through the blocking wait path",
        receipts.len()
    );

    store.close()?;
    Ok(())
}
