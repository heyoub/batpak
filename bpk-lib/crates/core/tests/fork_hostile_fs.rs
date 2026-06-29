//! justifies: INV-FORK-HOSTILE-FS-REFUSAL
//!
//! Offensive hostile-filesystem fork coverage (#48 `fork_hostile_fs`). Drives
//! the real `Store::fork_with_evidence` over a `SimFs` under four adversarial
//! conditions and asserts each is met with an `Err`/canonical refusal — never a
//! silent partial publish:
//!
//!   1. symlink destination leaf      -> Err (refused, nothing published through it)
//!   2. destination == source dir     -> Err (InvalidInput canonical refusal)
//!   3. stale destination artifacts   -> cleared, then forks to the source state
//!   4. ENOSPC mid-copy               -> Err (StorageFull), no partial publish
//!
//! The runners compose the genuine production fork path; the only injected
//! fault is SimFs's deterministic ENOSPC-on-copy. See
//! `crates/core/src/store/sim/fork_hostile.rs`.

#![cfg(feature = "dangerous-test-hooks")]

#[cfg(unix)]
#[test]
fn fork_symlink_dest_leaf_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    // The OS-specific symlink syscall lives here in the test, not in the
    // store-runtime fixture module (platform-isolation lint).
    let dir = tempfile::tempdir()?;
    let real_target = dir.path().join("real-target");
    std::fs::create_dir(&real_target)?;
    let link_dest = dir.path().join("link-target");
    std::os::unix::fs::symlink(&real_target, &link_dest)?;

    let outcome: batpak::__sim::SymlinkDestOutcome =
        batpak::__sim::run_fork_symlink_dest(&link_dest, &real_target)
            .map_err(std::io::Error::other)?;
    assert!(
        outcome.refused,
        "PROPERTY: a fork whose destination LEAF is a symlink must be refused with an IO error \
         (reject_symlink_leaf), not followed through the link"
    );
    assert!(
        outcome.no_publish,
        "PROPERTY: a refused symlink-dest fork must publish no openable store through the link target"
    );
    Ok(())
}

#[test]
fn fork_dest_equals_source_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let outcome: batpak::__sim::DestEqualsSourceOutcome =
        batpak::__sim::run_fork_dest_equals_source().map_err(std::io::Error::other)?;
    assert!(
        outcome.refused,
        "PROPERTY: a fork whose destination canonicalizes to the source data dir must be refused, \
         not allowed to copy a store onto itself"
    );
    assert!(
        outcome.refused_invalid_input,
        "PROPERTY: the dest==source refusal must be the InvalidInput canonical refusal, not an \
         unrelated IO failure"
    );
    Ok(())
}

#[test]
fn fork_over_stale_dest_clears_then_succeeds() -> Result<(), Box<dyn std::error::Error>> {
    // Plant stale store artifacts at the fork destination (raw fs writes live
    // here in the test, not in the store-runtime fixture). Both names are ones
    // the fork's file classifier recognizes as clearable store artifacts.
    let dir = tempfile::tempdir()?;
    let dest_dir = dir.path().join("dest");
    std::fs::create_dir_all(&dest_dir)?;
    std::fs::write(
        dest_dir.join(batpak::__sim::STALE_SEGMENT_FILE),
        b"STALE-SEGMENT-BYTES",
    )?;
    std::fs::write(
        dest_dir.join(batpak::__sim::STALE_RANGES_FILE),
        b"STALE-RANGES",
    )?;

    let outcome: batpak::__sim::StaleDestOutcome =
        batpak::__sim::run_fork_stale_dest(&dest_dir).map_err(std::io::Error::other)?;
    assert!(
        outcome.forked_ok,
        "PROPERTY: a fork over a destination holding stale artifacts must still succeed"
    );
    assert!(
        outcome.cleared_stale,
        "PROPERTY: the fork must positively CLEAR the stale destination artifacts \
         (a DestinationCleared finding), never merge them"
    );
    assert!(
        outcome.dest_matches_source,
        "PROPERTY: after clearing, the published fork must hold exactly the source's committed \
         state — proof the stale bytes were cleared, not merged in"
    );
    Ok(())
}

#[test]
fn fork_enospc_mid_copy_publishes_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let outcome: batpak::__sim::EnospcMidCopyOutcome =
        batpak::__sim::run_fork_enospc_mid_copy().map_err(std::io::Error::other)?;
    assert!(
        outcome.refused,
        "PROPERTY: a fork that runs out of disk space mid-copy must return Err"
    );
    assert!(
        outcome.refused_storage_full,
        "PROPERTY: the ENOSPC-mid-copy refusal must surface as a StorageFull IO error"
    );
    assert!(
        outcome.no_partial_publish,
        "PROPERTY: a fork that failed mid-copy must leave NO openable complete fork at the \
         destination — no partial publish a reader could mistake for a valid fork"
    );
    Ok(())
}
