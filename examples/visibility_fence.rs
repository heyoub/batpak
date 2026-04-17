use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 4)]
struct Hidden {
    hidden: bool,
}

#[allow(clippy::print_stdout)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;

    let coord = Coordinate::new("player:fence", "room:hidden")?;

    // Fence submit is not yet typed in v1; pass the kind explicitly from the
    // payload type's KIND constant so the callsite still avoids literal
    // (category, type_id) pairs.
    //
    // Ordering: fenced tickets only resolve once `fence.commit()` runs.
    // Call `ticket.wait()` AFTER `fence.commit()`, not before.
    let fence = store.begin_visibility_fence()?;
    let ticket = fence.submit(&coord, Hidden::KIND, &Hidden { hidden: true })?;

    println!(
        "durable before commit: visible_count={}",
        store.by_fact_typed::<Hidden>().len()
    );
    assert_eq!(store.by_fact_typed::<Hidden>().len(), 0);

    fence.commit()?;
    let receipt = ticket.wait()?;

    println!(
        "after commit event {} is visible and query count is {}",
        receipt.event_id,
        store.by_fact_typed::<Hidden>().len()
    );

    store.close()?;
    Ok(())
}
