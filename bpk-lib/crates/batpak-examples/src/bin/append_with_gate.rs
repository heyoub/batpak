//! # append_with_gate
//!
//! **Teaches:** durability frontiers — append-time gates, explicit durable waits,
//! and visibility fences with delayed publish.
//!
//! Run: `cargo run -p batpak-examples --bin append_with_gate`

use batpak::prelude::*;
use batpak::store::{BatchAppendItem, CausationRef, DurabilityGate, HlcPoint, WatermarkKind};
use std::time::Duration;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0x96)]
struct AccountAdjusted {
    amount: i64,
}

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 4)]
struct Hidden {
    hidden: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();

    let dir = tempfile::tempdir()?;
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_sync_every_n_events(1)
            .with_sync_mode(SyncMode::SyncData),
    )?;
    let coord = Coordinate::new("account:gate-demo", "ledger:2026")?;

    // Append-time gates block until the requested watermark crosses the committed event.
    let durable_receipt = store.append_typed_with_options(
        &coord,
        &AccountAdjusted { amount: 10 },
        AppendOptions::new().with_gate(DurabilityGate::new(
            WatermarkKind::Durable,
            Duration::from_secs(1),
        )),
    )?;
    let _ = writeln!(out, "durable-gated append: {}", durable_receipt.event_id);

    let batch = vec![
        BatchAppendItem::typed(
            coord.clone(),
            &AccountAdjusted { amount: 20 },
            AppendOptions::new(),
            CausationRef::None,
        )?,
        BatchAppendItem::typed(
            coord.clone(),
            &AccountAdjusted { amount: -5 },
            AppendOptions::new(),
            CausationRef::PriorItem(0),
        )?,
    ];
    let batch_receipts = store.append_batch_with_options(
        batch,
        AppendOptions::new().with_gate(DurabilityGate::new(
            WatermarkKind::Visible,
            Duration::from_secs(1),
        )),
    )?;
    let _ = writeln!(out, "visible-gated batch: {} events", batch_receipts.len());

    let timeout_dir = tempfile::tempdir()?;
    let timeout_store = Store::open(
        StoreConfig::new(timeout_dir.path())
            .with_sync_every_n_events(1000)
            .with_sync_mode(SyncMode::SyncData),
    )?;
    let timeout_coord = Coordinate::new("account:gate-timeout", "ledger:2026")?;
    let timeout = timeout_store.append_typed_with_options(
        &timeout_coord,
        &AccountAdjusted { amount: 99 },
        AppendOptions::new().with_gate(DurabilityGate::new(
            WatermarkKind::Durable,
            Duration::from_millis(50),
        )),
    );
    match timeout {
        Err(StoreError::WaitTimeout { .. }) => {
            let committed = timeout_store.query(&Region::entity("account:gate-timeout"));
            let _ = writeln!(
                out,
                "durable gate timed out; committed events: {}",
                committed.len()
            );
        }
        other => other.map(|_| ())?,
    }

    // Explicit durable wait after append when no append-time gate is used.
    let wait_dir = tempfile::tempdir()?;
    let wait_store = Store::open(
        StoreConfig::new(wait_dir.path())
            .with_sync_every_n_events(25)
            .with_sync_mode(SyncMode::SyncData),
    )?;
    let wait_coord = Coordinate::new("ledger:acct-1", "ledger:2026")?;
    let wait_receipt = wait_store.append_typed(&wait_coord, &AccountAdjusted { amount: 42 })?;
    let wait_entry = wait_store
        .query(&Region::entity("ledger:acct-1"))
        .into_iter()
        .find(|entry| entry.event_id() == wait_receipt.event_id)
        .ok_or("appended event missing from query")?;
    let target = HlcPoint {
        wall_ms: wait_entry.wall_ms(),
        global_sequence: wait_entry.global_sequence(),
    };
    wait_store.sync()?;
    wait_store.wait_for_durable(target, Duration::from_secs(1))?;
    let _ = writeln!(
        out,
        "wait_for_durable ok for event {}",
        wait_receipt.event_id
    );

    // Visibility fence: submit hidden work, then publish on commit.
    let fence_coord = Coordinate::new("player:fence", "room:hidden")?;
    let fence = wait_store.begin_visibility_fence()?;
    let ticket = fence.submit(&fence_coord, Hidden::KIND, &Hidden { hidden: true })?;
    assert_eq!(wait_store.by_fact_typed::<Hidden>().len(), 0);
    fence.commit()?;
    let fence_receipt = ticket.wait()?;
    let _ = writeln!(
        out,
        "visibility fence published event {} (visible count={})",
        fence_receipt.event_id,
        wait_store.by_fact_typed::<Hidden>().len()
    );

    store.close()?;
    timeout_store.close()?;
    wait_store.close()?;
    Ok(())
}
