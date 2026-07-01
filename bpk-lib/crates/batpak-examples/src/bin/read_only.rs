//! # read_only
//!
//! **Teaches:** read-only store reopening after graceful close.
//!
//! Run: `cargo run -p batpak-examples --bin read_only`

use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 5)]
struct Archived {
    n: u32,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();

    let dir = tempfile::tempdir()?;
    let config = StoreConfig::new(dir.path())
        .with_enable_checkpoint(true)
        .with_enable_mmap_index(true);

    let store = Store::open(config.clone())?;
    let coord = Coordinate::new("entity:readonly", "scope:archive")?;
    let _ = store.append_typed(&coord, &Archived { n: 1 })?;
    store.close()?;

    let read_only = Store::<batpak::store::ReadOnly>::open_read_only(config)?;
    let stream = read_only.by_entity("entity:readonly");
    let _ = writeln!(out, "read-only reopen recovered {} event(s)", stream.len());

    Ok(())
}
