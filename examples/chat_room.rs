#![allow(clippy::print_stdout, clippy::disallowed_methods)] // example binary — println and thread::spawn are fine
//! # Chat Room — subscriptions, cursors, and real-time event streams
//!
//! A chat system demonstrating batpak's two consumption patterns:
//! 1. **Subscriptions** — push-based, lossy (like a live stream you might miss)
//! 2. **Cursors** — pull-based, guaranteed delivery (like reading a transcript)
//!
//! Plus: filtering, composable SubscriptionOps, and cross-thread event delivery.
//!
//! No async. No tokio. Just threads and flume channels.
//!
//! Run: `cargo run --example chat_room`

use batpak::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

const MSG_SENT: EventKind = EventKind::custom(3, 1);
const MSG_EDITED: EventKind = EventKind::custom(3, 2);
const MSG_DELETED: EventKind = EventKind::custom(3, 3);

#[derive(Serialize, Deserialize, Debug)]
struct ChatMessage {
    from: String,
    text: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = Arc::new(Store::open(StoreConfig::new(dir.path()))?);

    println!("=== Chat Room: Subscriptions & Cursors ===\n");

    // -- Set up a subscriber BEFORE any messages are sent --
    // Subscriptions are push-based: you only see events that happen AFTER subscribing.
    let general_region = Region::scope("chat:general");
    let sub = store.subscribe(&general_region);

    // -- Spawn a listener thread that collects messages via subscription --
    let store_clone = Arc::clone(&store);
    let listener = std::thread::Builder::new()
        .name("chat-room-listener".into())
        .spawn(move || {
            let mut received = vec![];
            // Use SubscriptionOps for composable filtering: only MSG_SENT, take 3
            let mut ops = sub.ops().filter(|n| n.kind == MSG_SENT).take(3);
            while let Some(notif) = ops.recv() {
                received.push(format!(
                    "{}@{} (kind={})",
                    notif.coord.entity(),
                    notif.coord.scope(),
                    notif.kind
                ));
            }
            // Also demonstrate reading full events from notifications
            println!(
                "  Subscriber received {} messages (via push)",
                received.len()
            );
            for r in &received {
                println!("    {}", r);
            }
            // Try to read one of the events
            if let Some(first_notif_desc) = received.first() {
                println!("    (first: {})", first_notif_desc);
            }
            drop(store_clone);
        })
        .expect("spawn chat room listener thread");

    // -- Send messages from different users --
    let alice = Coordinate::new("user:alice", "chat:general")?;
    let bob = Coordinate::new("user:bob", "chat:general")?;
    let charlie = Coordinate::new("user:charlie", "chat:general")?;

    // A few small delays so the subscriber thread can keep up
    store.append(
        &alice,
        MSG_SENT,
        &ChatMessage {
            from: "alice".into(),
            text: "Hey everyone!".into(),
        },
    )?;
    println!("Alice: Hey everyone!");

    store.append(
        &bob,
        MSG_SENT,
        &ChatMessage {
            from: "bob".into(),
            text: "What's up?".into(),
        },
    )?;
    println!("Bob: What's up?");

    // Bob edits his message (different event kind — subscriber filter will skip this)
    store.append(
        &bob,
        MSG_EDITED,
        &ChatMessage {
            from: "bob".into(),
            text: "What's up? (edited)".into(),
        },
    )?;
    println!("Bob: [edited his message]");

    store.append(
        &charlie,
        MSG_SENT,
        &ChatMessage {
            from: "charlie".into(),
            text: "Hey! Just joined.".into(),
        },
    )?;
    println!("Charlie: Hey! Just joined.");

    // Small delay so subscriber processes all notifications
    std::thread::sleep(Duration::from_millis(50));

    // Wait for listener to finish (it takes 3 MSG_SENT events then stops)
    println!("\n--- Subscription Results ---");
    let _ = listener.join();

    // -- Now demonstrate cursors: guaranteed delivery, pull-based --
    println!("\n--- Cursor: Pull-based replay ---");
    println!("  (Cursors see ALL events, even ones before the cursor was created)\n");

    let mut cursor = store.cursor(&general_region);
    let mut cursor_events = vec![];
    while let Some(entry) = cursor.poll() {
        cursor_events.push(entry);
    }
    println!("  Cursor found {} events total:", cursor_events.len());
    for entry in &cursor_events {
        let kind_label = match entry.kind {
            k if k == MSG_SENT => "SENT",
            k if k == MSG_EDITED => "EDITED",
            k if k == MSG_DELETED => "DELETED",
            _ => "OTHER",
        };
        println!(
            "    [{:7}] {} (seq={})",
            kind_label, entry.coord, entry.clock
        );
    }

    // -- Query: filter by entity --
    println!("\n--- Query: Bob's messages only ---");
    let bob_events = store.stream("user:bob");
    println!("  Bob has {} events:", bob_events.len());
    for entry in &bob_events {
        println!("    kind={} seq={}", entry.kind, entry.clock);
    }

    // -- Query: filter by event kind --
    println!("\n--- Query: All edits across all users ---");
    let edits = store.by_fact(MSG_EDITED);
    println!("  {} edit event(s) found", edits.len());

    // -- Batch append: efficient bulk messaging --
    println!("\n--- Batch: Bulk message import ---");
    use batpak::store::{BatchAppendItem, CausationRef};

    // Simulate importing a batch of historical messages
    let historical = vec![
        BatchAppendItem::new(
            Coordinate::new("user:alice", "chat:general")?,
            MSG_SENT,
            &ChatMessage {
                from: "alice".into(),
                text: "[Batch] Historical message 1".into(),
            },
            AppendOptions::default(),
            CausationRef::None,
        )?,
        BatchAppendItem::new(
            Coordinate::new("user:bob", "chat:general")?,
            MSG_SENT,
            &ChatMessage {
                from: "bob".into(),
                text: "[Batch] Historical message 2".into(),
            },
            AppendOptions::default(),
            CausationRef::None,
        )?,
        BatchAppendItem::new(
            Coordinate::new("user:charlie", "chat:general")?,
            MSG_SENT,
            &ChatMessage {
                from: "charlie".into(),
                text: "[Batch] Historical message 3".into(),
            },
            AppendOptions::default(),
            CausationRef::None,
        )?,
    ];

    let batch_receipts = store.append_batch(historical)?;
    println!("  Imported {} messages atomically", batch_receipts.len());
    for (i, receipt) in batch_receipts.iter().enumerate() {
        println!(
            "    Message {}: seq={}, event_id={}",
            i, receipt.sequence, receipt.event_id
        );
    }

    // Verify all batch messages are visible
    let all_general = store.cursor(&general_region).poll_batch(100).len();
    println!("  Total messages in #general: {}", all_general);

    drop(store);
    println!("\nSubscriptions are push (lossy, filtered, composable).");
    println!("Cursors are pull (guaranteed, complete, sequential).");
    println!("Queries are instant (in-memory index).");

    Ok(())
}
