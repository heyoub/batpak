//! # visibility_fence
//!
//! **Teaches:** durable visibility fence with delayed publish.
//!
//! Run: `cargo run --example visibility_fence`

use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 4)]
struct Hidden {
    hidden: bool,
}

// justifies: example binary demonstrates the visibility-fence observable via println, which is the user-visible success signal here.
#[allow(clippy::print_stdout)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;

    let coord = Coordinate::new("player:fence", "room:hidden")?;

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
