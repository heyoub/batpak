//! # append_with_gate
//!
//! **Teaches:** append-time durability gates.
//!
//! Run: `cargo run --example append_with_gate`

use batpak::prelude::*;
use std::time::Duration;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0x96)]
struct AccountAdjusted {
    amount: i64,
}

// justifies: INV-EXAMPLES-OBSERVABLE-OUTPUT; example in examples/append_with_gate.rs prints observable gate outcomes for durable, visible, and timeout paths.
#[allow(clippy::print_stdout)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
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
        AppendOptions::new().with_gate(DurabilityGate {
            kind: WatermarkKind::Durable,
            timeout: Duration::from_secs(1),
        }),
    )?;
    println!("durable-gated append: {}", durable_receipt.event_id);

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
        AppendOptions::new().with_gate(DurabilityGate {
            kind: WatermarkKind::Visible,
            timeout: Duration::from_secs(1),
        }),
    )?;
    println!("visible-gated batch: {} events", batch_receipts.len());

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
        AppendOptions::new().with_gate(DurabilityGate {
            kind: WatermarkKind::Durable,
            timeout: Duration::from_millis(50),
        }),
    );
    match timeout {
        Err(StoreError::WaitTimeout { .. }) => {
            let committed = timeout_store.query(&Region::entity("account:gate-timeout"));
            println!(
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
