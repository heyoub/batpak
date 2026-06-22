//! # wait_for_durable
//!
//! **Teaches:** waiting for a specific event to cross the durable frontier.
//!
//! Run: `cargo run --example wait_for_durable`

use batpak::prelude::*;
use batpak::store::HlcPoint;
use std::time::Duration;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0x95)]
struct LedgerPosted {
    amount: i64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();

    let dir = tempfile::tempdir()?;
    let config = StoreConfig::new(dir.path())
        .with_sync_every_n_events(25)
        .with_sync_mode(SyncMode::SyncData);
    let store = Store::open(config)?;

    let coord = Coordinate::new("ledger:acct-1", "ledger:2026")?;
    let receipt = store.append_typed(&coord, &LedgerPosted { amount: 42 })?;
    let entry = store
        .query(&Region::entity("ledger:acct-1"))
        .into_iter()
        .find(|entry| entry.event_id() == u128::from(receipt.event_id))
        .ok_or("appended event missing from query")?;
    let target = HlcPoint {
        wall_ms: entry.wall_ms(),
        global_sequence: entry.global_sequence(),
    };

    store.sync()?;
    store.wait_for_durable(target, Duration::from_secs(1))?;

    let _ = writeln!(out, "event {} crossed durable frontier", receipt.event_id);
    store.close()?;
    Ok(())
}
