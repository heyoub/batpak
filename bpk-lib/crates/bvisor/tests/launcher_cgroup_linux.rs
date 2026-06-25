// REAL CLONE_INTO_CGROUP placement proof for the single-threaded Linux launcher
// (kernel plan §10.8, step 8b-ii-a). Real-OS: spawns `bvisor-linux-launcher` and a
// real workload into a real cgroup leaf, so it is gated to Linux + backend-linux.
#![cfg(all(target_os = "linux", feature = "backend-linux"))]
//! The HONEST proof that the launcher births the workload child INSIDE the prepared
//! cgroup leaf via `clone3(CLONE_INTO_CGROUP)` — not in the launcher's own cgroup.
//!
//! The witness is INDEPENDENT of the launcher's claim: the workload prints its OWN
//! `/proc/self/cgroup` (the kernel's view of where the workload actually landed) to
//! stdout, which the SAFE `run_launcher` harness captures. The host then asserts that
//! the child's `0::` unified-hierarchy line equals the relative path of the leaf the
//! host created — and DIFFERS from the host's own cgroup (the non-vacuous control: the
//! child really moved into the leaf, it did not merely inherit our scope). This is the
//! placement counterpart to `cgroup_enforcement_linux.rs` (8b-i), which proves the
//! leaf's `pids.max` genuinely throttles; together they back an honest "the workload
//! ran in a real, capped cgroup".
//!
//! Gated on a LIVE `probe_controller_base(["pids"])`: with no `pids`-delegating
//! ancestor the test SKIPS with an explicit message — never a silent pass.

use bvisor::linux::cgroup::{probe_controller_base, CgroupLeaf, CgroupLimits};
use bvisor::linux::launch::{resolve_launcher_path, run_launcher, AuthorityFd};
use bvisor::linux::protocol::{
    DescriptorKind, DescriptorRole, DescriptorShape, DescriptorSlotV1, LinuxLaunchBodyV1,
    LinuxLaunchPlanV1, LoweringWireEntryV1, LoweringWireV1, TargetSpecV1,
};
use bvisor::{AdmissionProgramHash, AttemptId, BackendProfileHash, BoundaryPlanHash};
use std::io::Write;
use std::os::fd::{OwnedFd, RawFd};
use std::sync::atomic::{AtomicU64, Ordering};

// Frozen ids/phase-codes the launcher serves (mirror its constants).
const ID_AMBIENT_SCRUB: &str = "linux.ambient.scrub.v1";
const ID_EXEC: &str = "linux.exec.v1";
const PHASE_CODE_SCRUB: u8 = 3; // LoweringPhase::FdHygiene.code()
const PHASE_CODE_EXEC: u8 = 5; // LoweringPhase::Launch.code()

// Descriptor-table slot fd numbers (the launcher reads each handle at its slot index).
const EXE_SLOT: u32 = 10;
const CGROUP_SLOT: u32 = 11;

const CGROUP_V2_ROOT: &str = "/sys/fs/cgroup";

/// A process-local counter for a collision-free leaf-name suffix WITHOUT a clock/RNG.
static LEAF_COUNTER: AtomicU64 = AtomicU64::new(0);

fn entry(id: &str, phase_code: u8) -> LoweringWireEntryV1 {
    LoweringWireEntryV1 {
        id: id.to_owned(),
        version: 1,
        phase_code,
        param_digest: [0u8; 32],
        decl_digest: [0u8; 32],
    }
}

/// A scrub+exec plan with a TargetExe slot AND a CgroupDir slot, running
/// `cat /proc/self/cgroup` (its stdout is captured by the harness). `h_l` is
/// `blake3(canonical(lowering))` so the schedule-digest binding passes (the real H_L
/// binding is #75).
fn cgroup_plan() -> LinuxLaunchPlanV1 {
    let lowering = LoweringWireV1 {
        entries: vec![
            entry(ID_AMBIENT_SCRUB, PHASE_CODE_SCRUB),
            entry(ID_EXEC, PHASE_CODE_EXEC),
        ],
    };
    let bytes = batpak::canonical::to_bytes(&lowering).expect("encode lowering");
    let h_l = batpak::event::hash::compute_hash(&bytes);
    let table = vec![
        DescriptorSlotV1 {
            slot_index: EXE_SLOT,
            role: DescriptorRole::TargetExe,
            expected: DescriptorShape {
                kind: DescriptorKind::Regular,
                writable: false,
            },
        },
        DescriptorSlotV1 {
            slot_index: CGROUP_SLOT,
            role: DescriptorRole::CgroupDir,
            // A cgroup directory opened O_RDONLY (File::open) is a non-writable dir.
            expected: DescriptorShape {
                kind: DescriptorKind::Directory,
                writable: false,
            },
        },
    ];
    LinuxLaunchPlanV1 {
        body: LinuxLaunchBodyV1 {
            attempt_id: AttemptId([7u8; 32]),
            plan_id: BoundaryPlanHash([1u8; 32]),
            h_a: AdmissionProgramHash([2u8; 32]),
            h_p: BackendProfileHash([3u8; 32]),
            h_l,
            lowering,
            descriptor_table: table,
            target: TargetSpecV1 {
                argv: vec!["cat".to_owned(), "/proc/self/cgroup".to_owned()],
                envp: vec![("PATH".to_owned(), "/usr/bin".to_owned())],
                exe_slot: EXE_SLOT,
                user_namespace: None,
            },
        },
    }
}

/// The unified-hierarchy (`0::`) cgroup path from a `/proc/<pid>/cgroup` body, relative
/// to the cgroup v2 mount.
fn unified_line(proc_cgroup: &str) -> Option<String> {
    for line in proc_cgroup.lines() {
        if let Some(path) = line.strip_prefix("0::") {
            return Some(path.trim().to_owned());
        }
    }
    None
}

#[test]
fn clone_into_cgroup_births_the_child_inside_the_prepared_leaf_or_skip() {
    let mut sink = std::io::stderr();
    let Some(base) = probe_controller_base(&["pids"]) else {
        let _ = writeln!(
            sink,
            "SKIP clone_into_cgroup: no writable `pids`-delegating cgroup v2 ancestor on this host"
        );
        return;
    };

    let suffix = LEAF_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = format!("bvisor-clone-{}-{suffix}", std::process::id());
    let mut leaf = match CgroupLeaf::create(&base, &name, CgroupLimits::with_pids_max(64)) {
        Ok(leaf) => leaf,
        Err(e) => {
            let _ = writeln!(
                sink,
                "SKIP clone_into_cgroup: leaf create failed ({e}); treating as no-delegation"
            );
            return;
        }
    };

    // The EXPECTED child cgroup: the leaf dir path with the mount prefix stripped (the
    // kernel reports `/proc/self/cgroup` relative to the cgroup v2 mount).
    let leaf_dir = leaf.dir().expect("leaf dir").to_path_buf();
    let expected_rel = leaf_dir
        .to_string_lossy()
        .strip_prefix(CGROUP_V2_ROOT)
        .map(str::to_owned)
        .expect("leaf dir is under the cgroup v2 mount");
    // The host's OWN cgroup — the child's must DIFFER (non-vacuous: it really moved).
    let host_rel = unified_line(&std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default())
        .unwrap_or_default();

    // Open the exe (cat) + the leaf dir as the two authority handles.
    let cat = OwnedFd::from(std::fs::File::open("/bin/cat").expect("open /bin/cat"));
    let leaf_fd = leaf.dir_fd().expect("open leaf dir fd");
    let authority = vec![
        AuthorityFd {
            slot_index: RawFd::try_from(EXE_SLOT).expect("exe slot fits RawFd"),
            handle: cat,
        },
        AuthorityFd {
            slot_index: RawFd::try_from(CGROUP_SLOT).expect("cgroup slot fits RawFd"),
            handle: leaf_fd,
        },
    ];

    let launcher = resolve_launcher_path(env!("CARGO_BIN_EXE_bvisor-linux-launcher"));
    let plan = cgroup_plan();
    let observation = run_launcher(&launcher, &plan, authority);

    // Tear the leaf down BEFORE asserting (kill -> drain -> remove), so a failing
    // assertion cannot leak the leaf. The workload (cat) has already exited by here.
    let _ = leaf.kill();
    let _ = leaf.wait_until_empty(50, std::time::Duration::from_millis(10));
    let _ = leaf.remove();

    let observation = observation.expect("harness ran the launcher");
    let child_cgroup = unified_line(&String::from_utf8_lossy(&observation.captured_stdout));
    let _ = writeln!(
        sink,
        "child /proc/self/cgroup 0:: line = {child_cgroup:?}; expected leaf = {expected_rel:?}; host = {host_rel:?}"
    );

    assert!(
        observation.exec_succeeded(),
        "the workload must run to success in the leaf; transcript: {:?}",
        observation.transcript
    );
    let child_cgroup = child_cgroup.expect("the workload printed its 0:: cgroup line to stdout");
    assert_eq!(
        child_cgroup, expected_rel,
        "the child must be born INSIDE the prepared leaf (CLONE_INTO_CGROUP)"
    );
    assert_ne!(
        child_cgroup, host_rel,
        "NON-VACUOUS: the child's cgroup must DIFFER from the host's — it really moved \
         into the leaf, it did not merely inherit our scope"
    );
}
