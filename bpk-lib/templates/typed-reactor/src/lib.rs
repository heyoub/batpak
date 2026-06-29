use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xA, type_id = 2)]
pub struct SourceEvent {
    pub value: u64,
}

pub fn run() -> Result<usize, Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!(
        "batpak-template-typed-reactor-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Store::open(StoreConfig::new(&dir))?;
    let coord = Coordinate::new("entity:source", "scope:template")?;
    // Seed one event. This template demonstrates typed projection
    // (`by_fact_typed`), so the append receipt is intentionally discarded here;
    // the `minimal-store` template shows using the receipt's `event_id`.
    let _ = store.append_typed(&coord, &SourceEvent { value: 11 })?;
    Ok(store.by_fact_typed::<SourceEvent>().len())
}
