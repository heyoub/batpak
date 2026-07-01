//! # subscription_fanout
//!
//! **Teaches:** push subscriptions (lossy) vs pull cursors (ordered replay).
//! Slow subscribers are dropped, not retained.
//!
//! Opaque entities emit events into a shared scope, demonstrating batpak's two
//! consumption patterns:
//! 1. **Subscriptions** — push-based, lossy (slow subscribers dropped on Full)
//! 2. **Cursors** — pull-based, ordered replay of the full stream
//!
//! Plus: filtering, composable SubscriptionOps, and cross-thread event delivery.
//!
//! No async. No tokio. Just threads and flume channels.
//!
//! Run: `cargo run -p batpak-examples --bin subscription_fanout`

use batpak::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// Three event kinds, three payload types. Category 3;
// type_id = 1/2/3 for emit/amend/retract.
#[derive(Serialize, Deserialize, Debug, EventPayload)]
#[batpak(category = 3, type_id = 1)]
struct Emitted {
    source: String,
    value: String,
}

#[derive(Serialize, Deserialize, Debug, EventPayload)]
#[batpak(category = 3, type_id = 2)]
struct Amended {
    source: String,
    value: String,
}

#[derive(Serialize, Deserialize, Debug, EventPayload)]
#[batpak(category = 3, type_id = 3)]
struct Retracted {
    source: String,
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

    let _ = writeln!(
        out.lock(),
        "=== Subscription Fanout: Subscriptions & Cursors ===\n"
    );

    // -- Set up a subscriber BEFORE any events are appended --
    // Subscriptions are push-based: you only see events that happen AFTER subscribing.
    let main_region = Region::scope("scope:main");
    let sub = store.subscribe_lossy(&main_region);

    // -- Spawn a listener thread that collects events via subscription --
    let store_clone = Arc::clone(&store);
    let listener = std::thread::Builder::new()
        .name("fanout-listener".into())
        .spawn(move || {
            let mut listener_out = std::io::stdout().lock();
            let mut received = vec![];
            // Use SubscriptionOps for composable filtering: only Emitted, take 3
            let mut ops = sub.ops().filter(|n| n.kind == Emitted::KIND).take(3);
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
                "  Subscriber received {} events (via push)",
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
        .expect("spawn fanout listener thread");

    // -- Append events from different entities --
    let a = Coordinate::new("entity:a", "scope:main")?;
    let b = Coordinate::new("entity:b", "scope:main")?;
    let c = Coordinate::new("entity:c", "scope:main")?;

    let _ = store.append_typed(
        &a,
        &Emitted {
            source: "a".into(),
            value: "value-1".into(),
        },
    )?;
    let _ = writeln!(out.lock(), "entity:a emitted value-1");

    let _ = store.append_typed(
        &b,
        &Emitted {
            source: "b".into(),
            value: "value-2".into(),
        },
    )?;
    let _ = writeln!(out.lock(), "entity:b emitted value-2");

    // entity:b amends its event (different event kind — subscriber filter will skip this)
    let _ = store.append_typed(
        &b,
        &Amended {
            source: "b".into(),
            value: "value-2 (amended)".into(),
        },
    )?;
    let _ = writeln!(out.lock(), "entity:b amended its event");

    let _ = store.append_typed(
        &c,
        &Emitted {
            source: "c".into(),
            value: "value-3".into(),
        },
    )?;
    let _ = writeln!(out.lock(), "entity:c emitted value-3");

    // Wait for listener to finish (it takes 3 Emitted events then stops)
    let _ = writeln!(out.lock(), "\n--- Subscription Results ---");
    listener
        .join()
        .map_err(|_| std::io::Error::other("fanout listener thread panicked"))?;

    // The listener thread has now joined, so this thread is the only writer.
    let mut out = out.lock();

    // -- Now demonstrate cursors: ordered replay, pull-based --
    let _ = writeln!(out, "\n--- Cursor: Pull-based replay ---");
    let _ = writeln!(
        out,
        "  (Cursors see ALL events, even ones before the cursor was created)\n"
    );

    let mut cursor = store.cursor_guaranteed(&main_region);
    let mut cursor_events = vec![];
    while let Some(entry) = cursor.poll() {
        cursor_events.push(entry);
    }
    let _ = writeln!(out, "  Cursor found {} events total:", cursor_events.len());
    for entry in &cursor_events {
        let kind_label = match entry.event_kind() {
            k if k == Emitted::KIND => "EMIT",
            k if k == Amended::KIND => "AMEND",
            k if k == Retracted::KIND => "RETRACT",
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
    let _ = writeln!(out, "\n--- Query: entity:b's events only ---");
    let b_events = store.by_entity("entity:b");
    let _ = writeln!(out, "  entity:b has {} events:", b_events.len());
    for entry in &b_events {
        let _ = writeln!(out, "    kind={} seq={}", entry.event_kind(), entry.clock());
    }

    // -- Query: filter by event kind --
    let _ = writeln!(out, "\n--- Query: All amendments across all entities ---");
    let amendments = store.by_fact_typed::<Amended>();
    let _ = writeln!(out, "  {} amend event(s) found", amendments.len());

    // -- Batch append: efficient bulk emission --
    let _ = writeln!(out, "\n--- Batch: Bulk event import ---");
    use batpak::store::{BatchAppendItem, CausationRef};

    // Simulate importing a batch of historical events
    let historical = vec![
        BatchAppendItem::typed(
            Coordinate::new("entity:a", "scope:main")?,
            &Emitted {
                source: "a".into(),
                value: "[Batch] historical-1".into(),
            },
            AppendOptions::default(),
            CausationRef::None,
        )?,
        BatchAppendItem::typed(
            Coordinate::new("entity:b", "scope:main")?,
            &Emitted {
                source: "b".into(),
                value: "[Batch] historical-2".into(),
            },
            AppendOptions::default(),
            CausationRef::None,
        )?,
        BatchAppendItem::typed(
            Coordinate::new("entity:c", "scope:main")?,
            &Emitted {
                source: "c".into(),
                value: "[Batch] historical-3".into(),
            },
            AppendOptions::default(),
            CausationRef::None,
        )?,
    ];

    let batch_receipts = store.append_batch(historical)?;
    let _ = writeln!(out, "  Imported {} events atomically", batch_receipts.len());
    for (i, receipt) in batch_receipts.iter().enumerate() {
        let _ = writeln!(
            out,
            "    Event {}: seq={}, event_id={}",
            i, receipt.global_sequence, receipt.event_id
        );
    }

    // Verify all batch events are visible
    let all_main = store.cursor_guaranteed(&main_region).poll_batch(100).len();
    let _ = writeln!(out, "  Total events in scope:main: {}", all_main);

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
