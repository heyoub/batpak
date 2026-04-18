//! # batch_append
//!
//! **Teaches:** atomic batch commit with causation refs.
//!
//! Demonstrates `Store::append_batch()` — multiple events committed atomically
//! with intra-batch causation linking. All events become visible together or
//! none are visible (crash-safe two-part commit protocol).
//!
//! Run: `cargo run --example batch_append`

use batpak::prelude::*;
use batpak::store::{BatchAppendItem, CausationRef};

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 1, type_id = 1)]
struct ChatSent {
    text: String,
}

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 2, type_id = 1)]
struct AuditLogged {
    action: String,
    participants: u32,
}

// justifies: example demonstrates batch append; println output is the observable success path shown to readers of this example.
#[allow(clippy::print_stdout)]
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
        BatchAppendItem::typed(
            Coordinate::new("user:alice", "chat:general")?,
            &ChatSent {
                text: "Hello everyone!".into(),
            },
            AppendOptions::default(),
            CausationRef::None,
        )?,
        // Second event: Bob replies, caused by Alice's message (item 0).
        BatchAppendItem::typed(
            Coordinate::new("user:bob", "chat:general")?,
            &ChatSent {
                text: "Hi Alice!".into(),
            },
            AppendOptions::default(),
            CausationRef::PriorItem(0),
        )?,
        // Third event: System audit log, caused by Bob's reply (item 1).
        BatchAppendItem::typed(
            Coordinate::new("system:audit", "chat:general")?,
            &AuditLogged {
                action: "message_exchange".into(),
                participants: 2,
            },
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
