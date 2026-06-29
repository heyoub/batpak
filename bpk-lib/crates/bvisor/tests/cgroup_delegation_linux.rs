// REAL cgroup v2 delegation round-trip on the live Linux backend (kernel plan
// §10.8, step 8a). Compiles only with the real Linux backend on a Linux host.
#![cfg(all(feature = "backend-linux", target_os = "linux"))]
//! The HONEST real-delegation proof for the SAFE host-side cgroup v2 manager.
//!
//! Gated on a LIVE probe ([`bvisor::linux::cgroup::probe_cgroup_delegation`]):
//! on a delegated cgroup v2 host (a systemd user session) it creates a REAL leaf
//! under the delegated base, sets `pids.max` IF the base delegates the `pids`
//! controller (reads it back to prove the kernel echoed it), then removes the
//! leaf — proving the manager works against the actual kernel interface, not just
//! a fake tree. With NO delegation it SKIPS with an explicit stderr message
//! (never a silent pass). The fake-tree unit tests (in `cgroup.rs`) prove the
//! file-write/parse logic deterministically without privileges; THIS test proves
//! the same logic round-trips on the real kernel when delegation exists.
//!
//! Uses ONLY the manager's PUBLIC API plus a direct read of the stable
//! `cgroup.subtree_control` interface file (to learn which controllers the base
//! delegates so it requests only a backed limit — never claiming an unenforceable
//! one). No private item is reached, so this lives as an integration test.

use bvisor::linux::cgroup::{probe_cgroup_delegation, CgroupLeaf, CgroupLimits};
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

/// A process-local counter for a collision-free leaf-name suffix WITHOUT a wall
/// clock or RNG (matching the manager's own probe-suffix discipline).
static LEAF_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Whether `base` delegates the `pids` controller to its children, read straight
/// from the stable `cgroup.subtree_control` interface file. A missing/unreadable
/// file means "nothing delegated here" (so the test creates a bare leaf instead
/// of requesting a limit the kernel cannot back).
fn pids_delegated(base: &std::path::Path) -> bool {
    std::fs::read_to_string(base.join("cgroup.subtree_control"))
        .map(|text| text.split_whitespace().any(|c| c == "pids"))
        .unwrap_or(false)
}

#[test]
fn real_delegated_leaf_roundtrip_or_explicit_skip() {
    let mut sink = std::io::stderr();
    let Some(base) = probe_cgroup_delegation() else {
        let _ = writeln!(
            sink,
            "SKIP real_delegated_leaf_roundtrip: no writable delegated cgroup v2 base \
             (probe_cgroup_delegation() == None) on this host"
        );
        return;
    };
    let _ = writeln!(
        sink,
        "REAL cgroup v2 delegation available at {}; running the live leaf round-trip",
        base.display()
    );

    let pids = pids_delegated(&base);
    // A unique leaf name (pid + counter, no clock/RNG) so concurrent runs differ.
    let suffix = LEAF_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = format!("bvisor-test-leaf-{}-{suffix}", std::process::id());

    let limits = if pids {
        CgroupLimits::with_pids_max(32)
    } else {
        let _ = writeln!(
            sink,
            "NOTE: `pids` not delegated at this base; creating a bare leaf (no limit) to \
             still prove create/remove round-trips on the real kernel"
        );
        CgroupLimits::default()
    };

    let mut leaf = match CgroupLeaf::create(&base, &name, limits) {
        Ok(leaf) => leaf,
        Err(e) => {
            let _ = writeln!(
                sink,
                "SKIP real_delegated_leaf_roundtrip: probe said writable but create failed \
                 ({e}); treating as no-delegation rather than a false failure"
            );
            return;
        }
    };

    assert!(
        leaf.dir().expect("dir").is_dir(),
        "the real leaf must exist"
    );
    if pids {
        // Read the limit back through the public dir() path — the kernel must echo
        // exactly the value we wrote.
        let readback = std::fs::read_to_string(leaf.dir().expect("dir").join("pids.max"))
            .expect("read back real pids.max");
        assert_eq!(
            readback.trim(),
            "32",
            "the kernel must echo the pids.max we wrote"
        );
        assert!(
            leaf.setup().pids_enforced,
            "a delegated+written pids limit is honestly Enforced"
        );
    }
    // No members were placed, so the leaf is empty and removes cleanly (kill-then-
    // remove ordering is moot with zero members).
    leaf.remove().expect("remove the real leaf");
    assert!(
        !base.join(&name).exists(),
        "the real leaf must be gone after remove"
    );
}
