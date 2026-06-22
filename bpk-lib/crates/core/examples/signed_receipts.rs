//! # signed_receipts
//!
//! **Teaches:** opt-in receipt signing, append receipt verification, and
//! persisted denial receipt verification.
//!
//! Run: `cargo run --example signed_receipts`

use batpak::guard::{Denial, Gate, GateSet};
use batpak::pipeline::Proposal;
use batpak::prelude::*;
use batpak::store::{DenialRequest, SigningKey};

#[derive(Clone, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 6, type_id = 1)]
struct SettingChanged {
    key: String,
    value: String,
}

struct WriteWindowGate {
    open: bool,
}

impl Gate<SettingChanged> for WriteWindowGate {
    fn name(&self) -> &'static str {
        "write_window"
    }

    fn evaluate(&self, _ctx: &SettingChanged) -> Result<(), Denial> {
        if self.open {
            Ok(())
        } else {
            Err(Denial::new(self.name(), "writes are currently paused")
                .with_code("WRITE_WINDOW_CLOSED")
                .with_context("window", "closed"))
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();

    let dir = tempfile::tempdir()?;
    let signing_key = SigningKey::from_bytes([7; 32]);
    let store = Store::open(StoreConfig::new(dir.path()).with_signing_key(signing_key))?;
    let coord = Coordinate::new("settings:primary", "config")?;

    let changed = SettingChanged {
        key: "retention_days".into(),
        value: "30".into(),
    };
    let append_receipt = store.append_typed(&coord, &changed)?;
    assert!(store.verify_append_receipt(&append_receipt));
    let _ = writeln!(out, "append receipt verified: {}", append_receipt.event_id);

    let mut gates = GateSet::new();
    gates.push(WriteWindowGate { open: false });
    let rejected = SettingChanged {
        key: "retention_days".into(),
        value: "7".into(),
    };
    let denial = match gates.evaluate(&rejected, Proposal::new(rejected.clone())) {
        Ok(_) => {
            return Err(std::io::Error::other(
                "example gate must reject while the write window is closed",
            )
            .into());
        }
        Err(denial) => denial,
    };

    let denial_receipt = store.append_denial(DenialRequest {
        coord: &coord,
        proposed_kind: SettingChanged::KIND,
        gate_set: &gates,
        failing: &denial,
        proposed_content_hash: Some(append_receipt.content_hash),
        pipeline_id: Some("example:signed_receipts".to_owned()),
        options: AppendOptions::new(),
    })?;
    assert!(store.verify_denial_receipt(&denial_receipt));

    let denial_event = store.read_raw(denial_receipt.event_id)?;
    assert_eq!(
        denial_event.event.header.event_kind,
        EventKind::SYSTEM_DENIAL
    );
    let _ = writeln!(out, "denial receipt verified: {}", denial_receipt.event_id);

    store.close()?;
    Ok(())
}
