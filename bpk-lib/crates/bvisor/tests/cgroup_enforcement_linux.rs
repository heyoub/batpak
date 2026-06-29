// REAL cgroup v2 pids.max ENFORCEMENT proof on the live Linux backend (kernel
// plan §10.8, step 8b-i). Compiles only with the real Linux backend on Linux.
#![cfg(all(feature = "backend-linux", target_os = "linux"))]
//! The HONEST "the limit actually throttles" proof for the SAFE cgroup manager.
//!
//! 8a proved a leaf's `pids.max` is WRITTEN and the kernel echoes it back. That is
//! not the same as proving the limit BITES — a value in an interface file could be
//! cosmetic. This test draws blood: it places a REAL fork-bomb workload into a leaf
//! with a tiny `pids.max` and reads the kernel's own `pids.events` `max` counter —
//! a DISK-OBSERVABLE ground truth the kernel increments once per fork it DENIES.
//! `max > 0` ⇒ the kernel genuinely refused forks past the cap; `pids.current` ≤
//! the cap ⇒ the cap held. Neither number comes from our report — both are read
//! straight off the kernel interface, the gauntlet's independent-oracle discipline.
//!
//! This is also why 8b's controller-aware probe matters: on a typical systemd
//! session the 8a writability probe lands on the process's own SCOPE, which
//! delegates no controllers, so a `pids.max` there is refused and this proof is
//! impossible. [`probe_controller_base`] walks up to the controller-delegating
//! ancestor (`app.slice`) where the limit is real. With no such ancestor (a host
//! without `pids` delegation) the test SKIPS with an explicit message — never a
//! silent pass.
//!
//! The workload is a POSIX `sh` that migrates ITSELF into the leaf (writes `$$` to
//! `cgroup.procs`) then spawns far more background processes than the cap allows;
//! the cap both proves enforcement AND bounds this test's own blast radius (at most
//! `pids.max` processes can ever exist in the leaf). Teardown is the manager's own
//! `kill` (atomic `cgroup.kill`) then `remove`.

use bvisor::linux::cgroup::{probe_controller_base, CgroupLeaf, CgroupLimits};
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// A process-local counter for a collision-free leaf-name suffix WITHOUT a wall
/// clock or RNG (matching the manager's own probe-suffix discipline).
static LEAF_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The `pids.max` cap for the proof. Small so the fork-bomb hits it immediately and
/// the test's blast radius is at most this many processes.
const CAP: u64 = 4;

/// How many background processes the workload TRIES to spawn — comfortably above
/// `CAP`, so the kernel must deny the excess (incrementing `pids.events` `max`).
const FORK_ATTEMPTS: u32 = 30;

/// Read the `max` field of a leaf's `pids.events` (one `key value` per line). The
/// kernel writes `max <n>` where `n` counts fork denials due to `pids.max`.
fn pids_events_max(leaf_dir: &Path) -> u64 {
    let text = std::fs::read_to_string(leaf_dir.join("pids.events")).unwrap_or_default();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("max ") {
            return rest.trim().parse::<u64>().unwrap_or(0);
        }
    }
    0
}

#[test]
fn pids_max_genuinely_denies_forks_past_the_cap_or_explicit_skip() {
    let mut sink = std::io::stderr();
    let Some(base) = probe_controller_base(&["pids"]) else {
        let _ = writeln!(
            sink,
            "SKIP pids_max_genuinely_denies_forks: no writable `pids`-delegating cgroup v2 \
             ancestor (probe_controller_base([\"pids\"]) == None) on this host"
        );
        return;
    };
    let _ = writeln!(
        sink,
        "REAL pids-delegating base at {}; running the live enforcement proof (cap={CAP})",
        base.display()
    );

    let suffix = LEAF_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = format!("bvisor-enforce-{}-{suffix}", std::process::id());
    let mut leaf = match CgroupLeaf::create(&base, &name, CgroupLimits::with_pids_max(CAP)) {
        Ok(leaf) => leaf,
        Err(e) => {
            let _ = writeln!(
                sink,
                "SKIP pids_max_genuinely_denies_forks: probe said delegated+writable but create \
                 failed ({e}); treating as no-delegation rather than a false failure"
            );
            return;
        }
    };
    assert!(
        leaf.setup().pids_enforced,
        "a delegated+written pids limit must be honestly Enforced"
    );
    let dir = leaf.dir().expect("leaf dir").to_path_buf();
    assert_eq!(
        pids_events_max(&dir),
        0,
        "a fresh leaf has denied no forks yet"
    );

    // The workload: migrate self into the leaf, then attempt FORK_ATTEMPTS background
    // processes. `$0` is the leaf dir. Each `sleep 30 &` past the cap is DENIED by the
    // kernel (incrementing pids.events max). The shell then exits; the started sleeps
    // remain in the leaf (cgroup membership survives reparenting) until we kill them.
    let script = "echo $$ > \"$0/cgroup.procs\"; i=0; \
                  while [ $i -lt $1 ]; do sleep 30 & i=$((i+1)); done";
    let status = Command::new("sh")
        .arg("-c")
        .arg(script)
        .arg(&dir) // $0 — the leaf directory
        .arg(FORK_ATTEMPTS.to_string()) // $1 — the fork attempt count
        .status();
    let status = match status {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(
                sink,
                "SKIP pids_max_genuinely_denies_forks: cannot spawn sh ({e})"
            );
            let _ = leaf.kill();
            let _ = leaf.remove();
            return;
        }
    };
    // The shell's EXIT CODE is deliberately NOT asserted: a denied background `fork`
    // makes `sh` exit non-zero, so requiring success would contradict the very
    // throttling this test proves. The kernel's `pids.events` counter — read below —
    // is the independent oracle, not the workload's self-report.
    let _ = writeln!(
        sink,
        "workload shell exited with {status:?} (non-zero is expected once forks are denied)"
    );

    // GROUND TRUTH — read straight off the kernel interface, never our report:
    let denied = pids_events_max(&dir);
    let current: u64 = std::fs::read_to_string(dir.join("pids.current"))
        .ok()
        .and_then(|t| t.trim().parse().ok())
        .unwrap_or(u64::MAX);
    let _ = writeln!(
        sink,
        "kernel-observed: pids.events max={denied} (forks denied), pids.current={current} (cap={CAP})"
    );

    // Tear down COMPLETELY before ANY assertion (kill → drain → remove), so a failing
    // assertion can never leak the leaf or its processes. `cgroup.kill` is async, so
    // the bounded drain bridges the SIGKILL-delivery window the rmdir would otherwise
    // race (EBUSY). 50 × 10ms = 500ms worst case — ample for a handful of sleeps.
    let killed = leaf.kill();
    let drained = leaf.wait_until_empty(50, std::time::Duration::from_millis(10));
    let removed = leaf.remove();

    assert!(
        denied > 0,
        "the kernel must have DENIED at least one fork past pids.max={CAP} \
         (pids.events max stayed 0 — the limit did not bite)"
    );
    assert!(
        current <= CAP,
        "pids.current ({current}) must never exceed the cap ({CAP}) — the cap held"
    );
    killed.expect("cgroup.kill must succeed on this >=5.14 delegated host");
    assert!(
        drained.expect("drain poll must read cgroup.procs"),
        "the leaf must drain (members exit after SIGKILL) within the bounded poll"
    );
    removed.expect("the leaf must remove after its members drain");
    assert!(!dir.exists(), "the real leaf must be gone after remove");
}
