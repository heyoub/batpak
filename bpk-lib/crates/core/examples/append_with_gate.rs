//! # append_with_gate
//!
//! **Teaches:** append-time durability gates.
//!
//! Run: `cargo run --example append_with_gate`

use batpak::prelude::*;
use batpak::store::{BatchAppendItem, CausationRef, DurabilityGate, WatermarkKind};
use std::time::Duration;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0x96)]
struct AccountAdjusted {
    amount: i64,
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

    store.close()?;
    timeout_store.close()?;
    Ok(())
}
