//! # lane_branch
//!
//! **Teaches:** append on independent DAG lanes for the same entity.
//!
//! Run: `cargo run --example lane_branch`

use batpak::prelude::*;
use batpak::store::{AppendOptions, AppendPositionHint, Store, StoreConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();

    let dir = tempfile::tempdir()?;
    let store = Store::open(StoreConfig::new(dir.path()).with_sync_every_n_events(1))?;
    let coord = Coordinate::new("entity:lane-demo", "scope:example")?;

    let _ = store.append(&coord, EventKind::DATA, &serde_json::json!({ "lane": 0, "n": 0 }))?;
    let _ = store.append_with_options(
        &coord,
        EventKind::DATA,
        &serde_json::json!({ "lane": 1, "n": 10 }),
        AppendOptions::new().with_position_hint(AppendPositionHint::branch_root(1, 0)),
    )?;
    let _ = store.append_with_options(
        &coord,
        EventKind::DATA,
        &serde_json::json!({ "lane": 1, "n": 11 }),
        AppendOptions::new().with_position_hint(AppendPositionHint::new(1, 1)),
    )?;

    let lane0 = store.stream_lane("entity:lane-demo", 0);
    let lane1 = store.stream_lane("entity:lane-demo", 1);
    let _ = writeln!(
        out,
        "lane 0 events {} lane 1 events {}",
        lane0.len(),
        lane1.len(),
    );

    store.close()?;
    Ok(())
}
