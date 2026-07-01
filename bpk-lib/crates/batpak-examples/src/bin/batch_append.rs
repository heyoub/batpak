//! # batch_append
//!
//! **Teaches:** atomic batch commit with causation refs.
//!
//! Demonstrates `Store::append_batch()` — multiple events committed atomically
//! with intra-batch causation linking. All events become visible together or
//! none are visible (crash-safe two-part commit protocol).
//!
//! Run: `cargo run -p batpak-examples --bin batch_append`

use batpak::prelude::*;
use batpak::store::{BatchAppendItem, CausationRef};

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 1, type_id = 1)]
struct ThingHappened {
    label: String,
}

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 2, type_id = 1)]
struct Summarized {
    note: String,
    count: u32,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();

    let dir = tempfile::tempdir()?;
    let config = StoreConfig::new(dir.path())
        .with_sync_every_n_events(25)
        .with_sync_mode(SyncMode::SyncData)
        .with_batch_max_bytes(1024 * 1024); // 1 MB batch limit
    let store = Store::open(config)?;

    // Build a batch of related events across two entities.
    let items = vec![
        // First event: entity:a emits.
        BatchAppendItem::typed(
            Coordinate::new("entity:a", "scope:main")?,
            &ThingHappened {
                label: "first".into(),
            },
            AppendOptions::default(),
            CausationRef::None,
        )?,
        // Second event: entity:b emits, caused by item 0.
        BatchAppendItem::typed(
            Coordinate::new("entity:b", "scope:main")?,
            &ThingHappened {
                label: "second".into(),
            },
            AppendOptions::default(),
            CausationRef::PriorItem(0),
        )?,
        // Third event: a summary record, caused by item 1.
        BatchAppendItem::typed(
            Coordinate::new("entity:summary", "scope:main")?,
            &Summarized {
                note: "combined".into(),
                count: 2,
            },
            AppendOptions::default(),
            CausationRef::PriorItem(1),
        )?,
    ];

    let receipts = store.append_batch(items)?;

    let _ = writeln!(out, "batch committed: {} events", receipts.len());
    for (i, receipt) in receipts.iter().enumerate() {
        let fetched = store.get(receipt.event_id)?;
        let _ = writeln!(
            out,
            "  [{}] event_id={} seq={} entity={}",
            i,
            receipt.event_id,
            receipt.global_sequence,
            fetched.coordinate.entity(),
        );
    }

    // Verify all events are queryable.
    let entity_a_events = store.query(&Region::entity("entity:a"));
    let _ = writeln!(out, "\nentity:a has {} event(s)", entity_a_events.len());

    Ok(())
}
