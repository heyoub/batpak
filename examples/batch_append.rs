//! Atomic batch append example.
//!
//! Demonstrates `Store::append_batch()` — multiple events committed atomically
//! with intra-batch causation linking. All events become visible together or
//! none are visible (crash-safe two-phase commit).
//!
//! Run with: `cargo run --example batch_append`

use batpak::prelude::*;
use batpak::store::{BatchAppendItem, CausationRef};

#[allow(clippy::print_stdout)] // example should show observable success path to users.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let config = StoreConfig::new(dir.path())
        .with_sync_every_n_events(25)
        .with_sync_mode(SyncMode::SyncData)
        .with_batch_max_bytes(1024 * 1024); // 1 MB batch limit
    let store = Store::open(config)?;

    // Build a batch of related events across two entities.
    let items = vec![
        // First event: Alice sends a message.
        BatchAppendItem::new(
            Coordinate::new("user:alice", "chat:general")?,
            EventKind::custom(1, 1),
            &serde_json::json!({"text": "Hello everyone!"}),
            AppendOptions::default(),
            CausationRef::None,
        )?,
        // Second event: Bob replies, caused by Alice's message (item 0).
        BatchAppendItem::new(
            Coordinate::new("user:bob", "chat:general")?,
            EventKind::custom(1, 1),
            &serde_json::json!({"text": "Hi Alice!"}),
            AppendOptions::default(),
            CausationRef::PriorItem(0),
        )?,
        // Third event: System audit log, caused by Bob's reply (item 1).
        BatchAppendItem::new(
            Coordinate::new("system:audit", "chat:general")?,
            EventKind::custom(2, 1),
            &serde_json::json!({"action": "message_exchange", "participants": 2}),
            AppendOptions::default(),
            CausationRef::PriorItem(1),
        )?,
    ];

    let receipts = store.append_batch(items)?;

    println!("batch committed: {} events", receipts.len());
    for (i, receipt) in receipts.iter().enumerate() {
        let fetched = store.get(receipt.event_id)?;
        println!(
            "  [{}] event_id={} seq={} entity={}",
            i,
            receipt.event_id,
            receipt.sequence,
            fetched.coordinate.entity(),
        );
    }

    // Verify all events are queryable.
    let alice_events = store.query(&Region::entity("user:alice"));
    println!("\nalice has {} event(s)", alice_events.len());

    Ok(())
}
