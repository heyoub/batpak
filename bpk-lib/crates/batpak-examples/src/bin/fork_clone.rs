//! # fork_clone
//!
//! **Teaches:** fork a store into an isolated directory and reopen it read-only.
//!
//! Run: `cargo run -p batpak-examples --bin fork_clone`

use batpak::prelude::*;
use batpak::store::{ForkOptions, ReadOnly, Store, StoreConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();

    let source_dir = tempfile::tempdir()?;
    let store = Store::open(
        StoreConfig::new(source_dir.path())
            .with_segment_max_bytes(512)
            .with_sync_every_n_events(1),
    )?;

    let coord = Coordinate::new("entity:fork-demo", "scope:example")?;
    let kind = EventKind::custom(0xF, 0x01);
    for i in 0..4 {
        let _ = store.append(&coord, kind, &serde_json::json!({ "i": i }))?;
    }
    let before = store.stats().event_count;

    let fork_dir = tempfile::tempdir()?;
    let report = store.fork_with_evidence(fork_dir.path(), ForkOptions::default())?;
    let forked = Store::<ReadOnly>::open_read_only(StoreConfig::new(fork_dir.path()))?;
    let _ = writeln!(
        out,
        "fork copied {} events (report hash {:02x}{:02x}…)",
        forked.stats().event_count,
        report.body_hash[0],
        report.body_hash[1],
    );
    assert_eq!(forked.stats().event_count, before);

    let _ = store.append(&coord, kind, &serde_json::json!({ "i": 99 }))?;
    assert_eq!(
        forked.stats().event_count,
        before,
        "parent writes after fork must not appear in the fork"
    );

    store.close()?;
    Ok(())
}
