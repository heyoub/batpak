use batpak::prelude::*;
use batpak::store::ReadWalkRequest;

pub fn run() -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!(
        "batpak-template-audit-read-report-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Store::open(StoreConfig::new(&dir))?;
    let coord = Coordinate::new("entity:read", "scope:audit")?;
    store.append(&coord, EventKind::custom(0xA, 3), &serde_json::json!({ "n": 1 }))?;
    let request = ReadWalkRequest::full(Region::scope("scope:audit"));
    let (_entries, report) = store.query_with_read_walk_evidence(&request)?;
    Ok(report.body_hash)
}
