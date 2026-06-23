//! # quickstart
//!
//! **Teaches:** basic typed append + retrieval.
//!
//! Run: `cargo run --example quickstart`

use batpak::prelude::*;

// One struct binds a Rust type to its EventKind at compile time.
// Callsites never touch EventKind::custom(...) again.
#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 1)]
struct PlayerMoved {
    x: i32,
    y: i32,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();

    let dir = tempfile::tempdir()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;

    let coord = Coordinate::new("player:alice", "room:dungeon")?;
    let receipt = store.append_typed(&coord, &PlayerMoved { x: 10, y: 20 })?;

    let fetched = store.get(receipt.event_id)?;
    let _ = writeln!(
        out,
        "stored {} at sequence {} in scope {}",
        fetched.event.header.event_id,
        receipt.global_sequence,
        fetched.coordinate.scope()
    );

    store.close()?;
    Ok(())
}
