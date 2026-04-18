//! # read_only
//!
//! **Teaches:** read-only store reopening after graceful close.
//!
//! Run: `cargo run --example read_only`

use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 5)]
struct Archived {
    n: u32,
}

// justifies: example main prints read-only reopening observable output via stdout; println is the success signal for this demo.
#[allow(clippy::print_stdout)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let config = StoreConfig::new(dir.path())
        .with_enable_checkpoint(true)
        .with_enable_mmap_index(true);

    let store = Store::open(config.clone())?;
    let coord = Coordinate::new("player:readonly", "room:archive")?;
    store.append_typed(&coord, &Archived { n: 1 })?;
    store.close()?;

    let read_only = Store::<batpak::store::ReadOnly>::open_read_only(config)?;
    let stream = read_only.stream("player:readonly");
    println!("read-only reopen recovered {} event(s)", stream.len());

    Ok(())
}
