//! # lifecycle_observer
//!
//! **Teaches:** observing the durable `SYSTEM_OPEN_COMPLETED` lifecycle event
//! emitted by a mutable store open.
//!
//! Run: `cargo run --example lifecycle_observer`

use batpak::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();

    let dir = tempfile::tempdir()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;

    let lifecycle_entries = store.by_fact(EventKind::SYSTEM_OPEN_COMPLETED);
    let open_entry = lifecycle_entries
        .first()
        .ok_or("mutable open should emit SYSTEM_OPEN_COMPLETED")?;
    let open_event = store.read_raw(batpak::id::EventId::from(open_entry.event_id()))?;

    assert_eq!(open_event.coordinate.entity(), "batpak:store");
    assert_eq!(open_event.coordinate.scope(), "batpak:lifecycle");
    assert_eq!(
        open_event.event.header.event_kind,
        EventKind::SYSTEM_OPEN_COMPLETED
    );

    let _ = writeln!(
        out,
        "observed lifecycle open event {} at {}/{}",
        open_event.event.header.event_id,
        open_event.coordinate.entity(),
        open_event.coordinate.scope()
    );

    store.close()?;
    Ok(())
}
