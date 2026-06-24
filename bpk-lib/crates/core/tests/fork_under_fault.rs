//! justifies: INV-FORK-CRASH-ATOMIC
//!
//! Fork survives StoreFs-level seeded faults: after `SimFs::crash`, a fork
//! destination must classify as exactly one of {CommittedPrefix | RolledBack |
//! CanonicalRefusal} via the recovery oracle — never an illegal half-fork that
//! `open()` accepts as valid corrupted state.

#![cfg(feature = "dangerous-test-hooks")]

use batpak::__sim::{fork_fault_replay_seed, run_seeded_fork_fault_public, RecoveredClass};

fn assert_legal_fork_classification(
    outcome: &batpak::__sim::ForkFaultOutcomePublic,
    seed: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    match outcome.classification {
        RecoveredClass::CommittedPrefix
        | RecoveredClass::RolledBack
        | RecoveredClass::CanonicalRefusal => {}
    }
    #[cfg(gauntlet_red_fixture)]
    {
        let _ = (outcome, seed);
        return Err(std::io::Error::other(
            "RED FIXTURE: illegal half-fork accepted",
        )
        .into());
    }
    #[cfg(not(gauntlet_red_fixture))]
    {
        let _ = seed;
        Ok(())
    }
}

#[test]
fn fork_under_fault_is_legal_and_deterministic() -> Result<(), Box<dyn std::error::Error>> {
    let seed = fork_fault_replay_seed(0xF0_0F_5EED);
    let first = run_seeded_fork_fault_public(seed).map_err(std::io::Error::other)?;
    let second = run_seeded_fork_fault_public(seed).map_err(std::io::Error::other)?;
    assert_eq!(
        first, second,
        "PROPERTY: identical seed (0x{seed:X}) must yield identical fork fault classification + digest"
    );
    assert_legal_fork_classification(&first, seed)?;
    Ok(())
}

#[test]
fn fork_under_fault_diverges_across_seeds() -> Result<(), Box<dyn std::error::Error>> {
    let a = run_seeded_fork_fault_public(0xF0_0F_0001).map_err(std::io::Error::other)?;
    let b = run_seeded_fork_fault_public(0xF0_0F_0002).map_err(std::io::Error::other)?;
    assert_ne!(
        a.digest, b.digest,
        "PROPERTY: distinct seeds should diverge in fork fault digest"
    );
    Ok(())
}
