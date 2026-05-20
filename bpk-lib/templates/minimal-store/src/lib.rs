use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xA, type_id = 1)]
pub struct ItemRecorded {
    pub value: u64,
}

pub fn run() -> Result<u128, Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!(
        "batpak-template-minimal-store-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Store::open(StoreConfig::new(&dir))?;
    let coord = Coordinate::new("entity:item", "scope:template")?;
    let receipt = store.append_typed(&coord, &ItemRecorded { value: 7 })?;
    Ok(receipt.event_id.into())
}
