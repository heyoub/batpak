use batpak::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let temp = std::env::temp_dir().join("batpak-minimal-app");
    let store = Store::open(StoreConfig::new(temp).with_sync_every_n_events(10))?;
    let coord = Coordinate::new("template:demo", "workspace:local")?;
    let kind = EventKind::custom(0xF, 1);
    let receipt = store.append(&coord, kind, &serde_json::json!({"hello": "batpak"}))?;
    println!("stored {} at {}", receipt.event_id, receipt.sequence);
    Ok(())
}
