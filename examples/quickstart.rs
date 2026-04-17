use batpak::prelude::*;

// One struct binds a Rust type to its EventKind at compile time.
// Callsites never touch EventKind::custom(...) again.
#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 1)]
struct PlayerMoved {
    x: i32,
    y: i32,
}

#[allow(clippy::print_stdout)] // quickstart should show an observable success path to new users.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let config = StoreConfig::new(dir.path())
        .with_sync_every_n_events(25)
        .with_sync_mode(SyncMode::SyncData);
    let store = Store::open(config)?;

    let coord = Coordinate::new("player:alice", "room:dungeon")?;
    let receipt = store.append_typed(&coord, &PlayerMoved { x: 10, y: 20 })?;

    let fetched = store.get(receipt.event_id)?;
    println!(
        "stored {} at sequence {} in scope {}",
        fetched.event.header.event_id,
        receipt.sequence,
        fetched.coordinate.scope()
    );

    store.close()?;
    Ok(())
}
