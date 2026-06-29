//! # idempotent_pass
//!
//! **Teaches:** a re-runnable, durable idempotent append pass.
//!
//! A keyed append is deduplicated as a true no-op. With the durable
//! idempotency store (Phase 3) the dedup survives retention compaction and
//! cold-start, so a re-run of the same operation is ALWAYS a no-op — the
//! SQLite-grade durability property.
//!
//! The key here is derived with [`IdempotencyKey::for_operation`], which hashes
//! a length-delimited (domain, components) tuple into a stable u128. Re-running
//! the pass recomputes the SAME key, so the second append returns the original
//! receipt instead of writing a duplicate.
//!
//! Run: `cargo run -p batpak-examples --bin idempotent_pass`

use batpak::id::IdempotencyKey;
use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0x20)]
struct AccountCredited {
    account: String,
    amount_cents: u64,
}

/// One idempotent pass: derive the operation key, append under it, return the
/// receipt. Calling this twice with the same arguments is a no-op the second
/// time — that is the point.
fn credit_once(
    store: &Store,
    coord: &Coordinate,
    request_id: &str,
    account: &str,
    amount_cents: u64,
) -> Result<AppendReceipt, Box<dyn std::error::Error>> {
    // Operation identity, NOT a content hash: "this specific credit request".
    let key = IdempotencyKey::for_operation("account.credit", &[account, request_id]);
    let receipt = store.append_typed_with_options(
        coord,
        &AccountCredited {
            account: account.to_owned(),
            amount_cents,
        },
        AppendOptions::new().with_idempotency(key),
    )?;
    Ok(receipt)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();

    let dir = tempfile::tempdir()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;
    let coord = Coordinate::new("account:alice", "ledger:main")?;

    // First pass: the credit is committed.
    let first = credit_once(&store, &coord, "req-2026-0001", "account:alice", 5_000)?;
    let _ = writeln!(
        out,
        "first pass committed credit at sequence {} (event {})",
        first.global_sequence, first.event_id
    );

    // Re-run the SAME pass (e.g. a retry after a crash). It is a no-op: the
    // same key resolves to the original receipt — no duplicate is written.
    let replay = credit_once(&store, &coord, "req-2026-0001", "account:alice", 5_000)?;
    let _ = writeln!(
        out,
        "re-run was a no-op: sequence {} (same as first: {})",
        replay.global_sequence,
        replay.global_sequence == first.global_sequence
    );
    assert_eq!(
        first.global_sequence, replay.global_sequence,
        "idempotent re-run must return the original receipt"
    );

    let _ = writeln!(
        out,
        "durable idempotency keys held: {}",
        store.durable_idempotency_key_count()
    );

    store.close()?;
    Ok(())
}
