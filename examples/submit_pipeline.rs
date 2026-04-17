use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 2)]
struct Tick {
    n: u32,
}

#[allow(clippy::print_stdout)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;

    let coord = Coordinate::new("player:submit", "room:pipeline")?;

    let first = store.submit_typed(&coord, &Tick { n: 1 })?;
    let second = store.submit_typed(&coord, &Tick { n: 2 })?;
    let third = store.submit_typed(&coord, &Tick { n: 3 })?;

    let receipts = [first.wait()?, second.wait()?, third.wait()?];
    println!(
        "queued {} appends and committed through the blocking wait path",
        receipts.len()
    );

    store.close()?;
    Ok(())
}
