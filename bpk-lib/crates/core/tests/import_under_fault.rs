//! justifies: INV-IMPORT-CRASH-IDEMPOTENT
//!
//! Import survives StoreFs-level crash: after `abandon_without_shutdown` and
//! `SimFs::crash`, reopening the destination and re-running the same import must
//! deduplicate via durable import keys while preserving payload bytes and the
//! per-entity hash chain.

#![cfg(feature = "dangerous-test-hooks")]

#[test]
fn import_under_fault_crash_reimport_deduplicates() -> Result<(), Box<dyn std::error::Error>> {
    let seed = batpak::__sim::import_fault_replay_seed(0x1B00_DEAD);
    let first: batpak::__sim::ImportFaultOutcomePublic =
        batpak::__sim::run_seeded_import_fault_public(seed).map_err(std::io::Error::other)?;
    let second: batpak::__sim::ImportFaultOutcomePublic =
        batpak::__sim::run_seeded_import_fault_public(seed).map_err(std::io::Error::other)?;
    assert_eq!(
        first, second,
        "PROPERTY: identical seed (0x{seed:X}) must recover identical import fault digest \
         (replay with BATPAK_SEED={seed})"
    );
    assert_eq!(
        first.dest_user_events, first.source_user_events,
        "PROPERTY: destination must hold every source user event after crash + re-import"
    );
    #[cfg(gauntlet_red_fixture)]
    {
        return Err(std::io::Error::other(
            "RED FIXTURE: import re-import failed to deduplicate after crash",
        )
        .into());
    }
    Ok(())
}

#[test]
fn import_under_fault_diverges_across_seeds() -> Result<(), Box<dyn std::error::Error>> {
    let a = batpak::__sim::run_seeded_import_fault_public(0x1B00_0001)
        .map_err(std::io::Error::other)?;
    let b = batpak::__sim::run_seeded_import_fault_public(0x1B00_0002)
        .map_err(std::io::Error::other)?;
    assert_ne!(
        a.digest, b.digest,
        "PROPERTY: distinct seeds should diverge in import fault digest"
    );
    Ok(())
}
