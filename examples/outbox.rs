use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 3)]
struct Tick {
    n: u32,
}

// Outbox staging is not yet typed in v1; feed the payload type's KIND
// constant so the callsite still never writes a literal (category, type_id)
// pair. Typed outbox staging lands in the next lock.
#[allow(clippy::print_stdout)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;

    let mut outbox = store.outbox();
    outbox.stage(
        Coordinate::new("player:outbox", "room:batch")?,
        Tick::KIND,
        &Tick { n: 1 },
    )?;
    outbox.stage(
        Coordinate::new("player:outbox", "room:batch")?,
        Tick::KIND,
        &Tick { n: 2 },
    )?;
    outbox.stage(
        Coordinate::new("player:outbox", "room:batch")?,
        Tick::KIND,
        &Tick { n: 3 },
    )?;

    let receipts = outbox.flush()?;
    println!(
        "flushed {} staged events through one batch path",
        receipts.len()
    );

    store.close()?;
    Ok(())
}
