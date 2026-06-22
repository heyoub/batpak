//! # chat_room
//!
//! **Teaches:** push subscriptions (lossy) vs pull cursors (ordered replay).
//! Slow subscribers are dropped, not retained.
//!
//! A chat system demonstrating batpak's two consumption patterns:
//! 1. **Subscriptions** — push-based, lossy (slow subscribers dropped on Full)
//! 2. **Cursors** — pull-based, ordered replay (like reading a transcript)
//!
//! Plus: filtering, composable SubscriptionOps, and cross-thread event delivery.
//!
//! No async. No tokio. Just threads and flume channels.
//!
//! Run: `cargo run --example chat_room`

use batpak::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// Three event kinds, three payload types. Category 3 = chat;
// type_id = 1/2/3 for sent/edited/deleted.
#[derive(Serialize, Deserialize, Debug, EventPayload)]
#[batpak(category = 3, type_id = 1)]
struct MessageSent {
    from: String,
    text: String,
}

#[derive(Serialize, Deserialize, Debug, EventPayload)]
#[batpak(category = 3, type_id = 2)]
struct MessageEdited {
    from: String,
    text: String,
}

#[derive(Serialize, Deserialize, Debug, EventPayload)]
#[batpak(category = 3, type_id = 3)]
struct MessageDeleted {
    from: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    // A concurrent listener thread also writes to stdout, so this example
    // locks the handle per write (released immediately) rather than holding a
    // single `StdoutLock` across `listener.join()` — holding it would deadlock
    // against the listener's own `stdout().lock()`.
    let out = std::io::stdout();

    let dir = tempfile::tempdir()?;
    let store = Arc::new(Store::open(StoreConfig::new(dir.path()))?);

    let _ = writeln!(out.lock(), "=== Chat Room: Subscriptions & Cursors ===\n");

    // -- Set up a subscriber BEFORE any messages are sent --
    // Subscriptions are push-based: you only see events that happen AFTER subscribing.
    let general_region = Region::scope("chat:general");
    let sub = store.subscribe_lossy(&general_region);

    // -- Spawn a listener thread that collects messages via subscription --
    let store_clone = Arc::clone(&store);
    let listener = std::thread::Builder::new()
        .name("chat-room-listener".into())
        .spawn(move || {
            let mut listener_out = std::io::stdout().lock();
            let mut received = vec![];
            // Use SubscriptionOps for composable filtering: only MessageSent, take 3
            let mut ops = sub.ops().filter(|n| n.kind == MessageSent::KIND).take(3);
            while let Some(notif) = ops.recv() {
                received.push(format!(
                    "{}@{} (kind={})",
                    notif.coord.entity(),
                    notif.coord.scope(),
                    notif.kind
                ));
            }
            // Also demonstrate reading full events from notifications
            let _ = writeln!(
                listener_out,
                "  Subscriber received {} messages (via push)",
                received.len()
            );
            for r in &received {
                let _ = writeln!(listener_out, "    {}", r);
            }
            // Try to read one of the events
            if let Some(first_notif_desc) = received.first() {
                let _ = writeln!(listener_out, "    (first: {})", first_notif_desc);
            }
            drop(store_clone);
        })
        .expect("spawn chat room listener thread");

    // -- Send messages from different users --
    let alice = Coordinate::new("user:alice", "chat:general")?;
    let bob = Coordinate::new("user:bob", "chat:general")?;
    let charlie = Coordinate::new("user:charlie", "chat:general")?;

    // A few small delays so the subscriber thread can keep up
    let _ = store.append_typed(
        &alice,
        &MessageSent {
            from: "alice".into(),
            text: "Hey everyone!".into(),
        },
    )?;
    let _ = writeln!(out.lock(), "Alice: Hey everyone!");

    let _ = store.append_typed(
        &bob,
        &MessageSent {
            from: "bob".into(),
            text: "What's up?".into(),
        },
    )?;
    let _ = writeln!(out.lock(), "Bob: What's up?");

    // Bob edits his message (different event kind — subscriber filter will skip this)
    let _ = store.append_typed(
        &bob,
        &MessageEdited {
            from: "bob".into(),
            text: "What's up? (edited)".into(),
        },
    )?;
    let _ = writeln!(out.lock(), "Bob: [edited his message]");

    let _ = store.append_typed(
        &charlie,
        &MessageSent {
            from: "charlie".into(),
            text: "Hey! Just joined.".into(),
        },
    )?;
    let _ = writeln!(out.lock(), "Charlie: Hey! Just joined.");

    // Wait for listener to finish (it takes 3 MSG_SENT events then stops)
    let _ = writeln!(out.lock(), "\n--- Subscription Results ---");
    listener
        .join()
        .map_err(|_| std::io::Error::other("chat room listener thread panicked"))?;

    // The listener thread has now joined, so this thread is the only writer.
    let mut out = out.lock();

    // -- Now demonstrate cursors: ordered replay, pull-based --
    let _ = writeln!(out, "\n--- Cursor: Pull-based replay ---");
    let _ = writeln!(
        out,
        "  (Cursors see ALL events, even ones before the cursor was created)\n"
    );

    let mut cursor = store.cursor_guaranteed(&general_region);
    let mut cursor_events = vec![];
    while let Some(entry) = cursor.poll() {
        cursor_events.push(entry);
    }
    let _ = writeln!(out, "  Cursor found {} events total:", cursor_events.len());
    for entry in &cursor_events {
        let kind_label = match entry.event_kind() {
            k if k == MessageSent::KIND => "SENT",
            k if k == MessageEdited::KIND => "EDITED",
            k if k == MessageDeleted::KIND => "DELETED",
            _ => "OTHER",
        };
        let _ = writeln!(
            out,
            "    [{:7}] {} (seq={})",
            kind_label,
            entry.coord(),
            entry.clock()
        );
    }

    // -- Query: filter by entity --
    let _ = writeln!(out, "\n--- Query: Bob's messages only ---");
    let bob_events = store.by_entity("user:bob");
    let _ = writeln!(out, "  Bob has {} events:", bob_events.len());
    for entry in &bob_events {
        let _ = writeln!(out, "    kind={} seq={}", entry.event_kind(), entry.clock());
    }

    // -- Query: filter by event kind --
    let _ = writeln!(out, "\n--- Query: All edits across all users ---");
    let edits = store.by_fact_typed::<MessageEdited>();
    let _ = writeln!(out, "  {} edit event(s) found", edits.len());

    // -- Batch append: efficient bulk messaging --
    let _ = writeln!(out, "\n--- Batch: Bulk message import ---");
    use batpak::store::{BatchAppendItem, CausationRef};

    // Simulate importing a batch of historical messages
    let historical = vec![
        BatchAppendItem::typed(
            Coordinate::new("user:alice", "chat:general")?,
            &MessageSent {
                from: "alice".into(),
                text: "[Batch] Historical message 1".into(),
            },
            AppendOptions::default(),
            CausationRef::None,
        )?,
        BatchAppendItem::typed(
            Coordinate::new("user:bob", "chat:general")?,
            &MessageSent {
                from: "bob".into(),
                text: "[Batch] Historical message 2".into(),
            },
            AppendOptions::default(),
            CausationRef::None,
        )?,
        BatchAppendItem::typed(
            Coordinate::new("user:charlie", "chat:general")?,
            &MessageSent {
                from: "charlie".into(),
                text: "[Batch] Historical message 3".into(),
            },
            AppendOptions::default(),
            CausationRef::None,
        )?,
    ];

    let batch_receipts = store.append_batch(historical)?;
    let _ = writeln!(
        out,
        "  Imported {} messages atomically",
        batch_receipts.len()
    );
    for (i, receipt) in batch_receipts.iter().enumerate() {
        let _ = writeln!(
            out,
            "    Message {}: seq={}, event_id={}",
            i, receipt.global_sequence, receipt.event_id
        );
    }

    // Verify all batch messages are visible
    let all_general = store
        .cursor_guaranteed(&general_region)
        .poll_batch(100)
        .len();
    let _ = writeln!(out, "  Total messages in #general: {}", all_general);

    drop(store);
    let _ = writeln!(
        out,
        "\nSubscriptions are push (lossy, filtered, composable)."
    );
    let _ = writeln!(
        out,
        "Cursors are pull (ordered, at-least-once; restart durability uses checkpointed workers)."
    );
    let _ = writeln!(out, "Queries are instant (in-memory index).");

    Ok(())
}
