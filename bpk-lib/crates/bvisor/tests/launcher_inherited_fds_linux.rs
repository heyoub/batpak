// LAUNCHER MECHANISM proof (a building block, NOT a contract-admission proof): a host
// file descriptor INHERITED into the launcher but NOT declared in the plan's
// descriptor table is CLOSED by the child-side fd-scrub before the workload `fexecve`s
// — so an undeclared host fd (a leaked secret handle) cannot survive into the confined
// workload. G6 (no-fd-escape).
//
// SCOPE (codex review 2026-06-25): this proves the launcher's scrub realises
// `FdPolicy::None` (no host fds survive). It does NOT prove the `InheritedFds`
// capability is admitted + honored end to end, and the scrub does NOT implement
// `FdPolicy::Only(..)`. So `InheritedFds` is NOT in the ceiling (fails closed) until
// admission is policy-aware + the contract path is proven.
//
// WHY THIS IS A REAL ORACLE (not a self-report), with a HOST-SIDE witness:
//   * The host creates a pipe and keeps the READ end. It relocates the pipe's WRITE
//     end to a fixed fd in the collision-free band 50..100 (above the launcher's
//     channel fds <= 14, below the launcher's own `FD_RELOCATE_BASE` == 100) with
//     CLOEXEC CLEARED, so the write end genuinely SURVIVES the launcher's execve and
//     is inherited by the clone3 child — it really does reach the boundary.
//   * It is NOT declared in the plan's descriptor table, so the scrub must close it.
//   * The workload tries to write a marker to that fd. TWO independent witnesses
//     must agree the scrub closed it: (a) the workload's OWN report on stdout is
//     `SCRUBBED` (its write failed), and (b) the HOST reads ZERO bytes from the pipe
//     read end (the marker never crossed the boundary). The host-side read is the
//     non-self-report oracle: had the fd leaked, the host would read the marker.
//
// `#![cfg(target_os = "linux")]` — real clone3 + fexecve through the launcher bin.

#![cfg(target_os = "linux")]

use bvisor::linux::launch::{self, AuthorityFd, LaunchObservation};
use bvisor::linux::protocol::{
    DescriptorKind, DescriptorRole, DescriptorShape, DescriptorSlotV1, LinuxLaunchBodyV1,
    LinuxLaunchPlanV1, LoweringWireEntryV1, LoweringWireV1, TargetSpecV1,
};
use bvisor::{AdmissionProgramHash, AttemptId, BackendProfileHash, BoundaryPlanHash};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;

const ID_AMBIENT_SCRUB: &str = "linux.ambient.scrub.v1";
const ID_EXEC: &str = "linux.exec.v1";
const PHASE_CODE_SCRUB: u8 = 3;
const PHASE_CODE_EXEC: u8 = 5;
const SLOT_EXE: RawFd = 10;

// The injected undeclared fd lands here: above the launcher channel fds (<= 14) and
// below the launcher's own relocation base (FD_RELOCATE_BASE == 100), so it can
// collide with neither the channel plumbing nor a relocated source.
const INJECTED_FD: RawFd = 50;

fn launcher_path() -> PathBuf {
    launch::resolve_launcher_path(env!("CARGO_BIN_EXE_bvisor-linux-launcher"))
}

fn entry(id: &str, phase_code: u8) -> LoweringWireEntryV1 {
    LoweringWireEntryV1 {
        id: id.to_owned(),
        version: 1,
        phase_code,
        param_digest: [0u8; 32],
        decl_digest: [0u8; 32],
    }
}

fn exe_slot() -> DescriptorSlotV1 {
    DescriptorSlotV1 {
        slot_index: u32::try_from(SLOT_EXE).expect("fd fits u32"),
        role: DescriptorRole::TargetExe,
        expected: DescriptorShape {
            kind: DescriptorKind::Regular,
            writable: false,
        },
    }
}

fn exec_only_plan(argv: Vec<String>) -> LinuxLaunchPlanV1 {
    let lowering = LoweringWireV1 {
        entries: vec![
            entry(ID_AMBIENT_SCRUB, PHASE_CODE_SCRUB),
            entry(ID_EXEC, PHASE_CODE_EXEC),
        ],
    };
    let bytes = batpak::canonical::to_bytes(&lowering).expect("encode lowering");
    let h_l = batpak::event::hash::compute_hash(&bytes);
    let body = LinuxLaunchBodyV1 {
        attempt_id: AttemptId([7u8; 32]),
        plan_id: BoundaryPlanHash([1u8; 32]),
        h_a: AdmissionProgramHash([2u8; 32]),
        h_p: BackendProfileHash([3u8; 32]),
        h_l,
        lowering,
        descriptor_table: vec![exe_slot()],
        target: TargetSpecV1 {
            argv,
            envp: vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())],
            exe_slot: u32::try_from(SLOT_EXE).expect("fd fits u32"),
            user_namespace: None,
            network_namespace: None,
            seccomp: None,
        },
    };
    LinuxLaunchPlanV1 { body }
}

fn exe_authority() -> AuthorityFd {
    AuthorityFd {
        slot_index: SLOT_EXE,
        handle: OwnedFd::from(std::fs::File::open("/bin/sh").expect("open /bin/sh")),
    }
}

/// Duplicate `fd` to a fresh number in the collision-free band (>= `INJECTED_FD`)
/// with CLOEXEC CLEARED (so it survives the launcher's execve and is inherited by
/// the clone3 child). Uses `F_DUPFD`, which allocates the LOWEST free fd at/above
/// the base — it never clobbers an existing fd — so the test is collision-safe
/// across repeated runs. Returns the owned relocated fd; the caller keeps it alive
/// across the launcher run and reads its number for the workload argv.
fn place_inheritable_high(fd: RawFd) -> OwnedFd {
    // SAFETY: test-only. F_DUPFD returns a fresh owned fd >= INJECTED_FD with CLOEXEC
    // CLEARED (unlike F_DUPFD_CLOEXEC), or -1. We adopt it once.
    let new = unsafe { libc::fcntl(fd, libc::F_DUPFD, INJECTED_FD) };
    assert!(
        (INJECTED_FD..100).contains(&new),
        "F_DUPFD must land in the collision-free band [{INJECTED_FD},100); got {new}"
    );
    // SAFETY: `new` is a fresh owned fd from F_DUPFD.
    unsafe { OwnedFd::from_raw_fd(new) }
}

/// Run the workload with an undeclared inherited pipe write end. Returns the
/// launcher observation and whatever bytes the HOST read from the pipe read end
/// (empty == nothing leaked across the boundary).
fn run_with_undeclared_fd() -> (LaunchObservation, Vec<u8>) {
    let (mut reader, writer) = std::io::pipe().expect("create pipe");
    // Place the WRITE end at a fresh inheritable fd; keep it owned for the run. Both
    // `writer` and `injected` stay alive until AFTER the spawn so the inherited copy
    // reaches the launcher.
    let injected = place_inheritable_high(writer.as_raw_fd());
    let injected_fd = injected.as_raw_fd();

    let argv = vec![
        "sh".to_string(),
        "-c".to_string(),
        // Try to write a marker to the undeclared fd. If the scrub closed it the
        // redirect fails and we report SCRUBBED; if it leaked we write LEAK into the
        // host's pipe and report WROTE.
        format!("if printf LEAK >&{injected_fd}; then printf WROTE; else printf SCRUBBED; fi"),
    ];
    let plan = exec_only_plan(argv);
    let obs = launch::run_launcher(&launcher_path(), &plan, vec![exe_authority()])
        .expect("the launcher harness runs the fd-scrub workload to a verdict");

    // Drop every host-side WRITE end so the pipe read sees EOF (the inherited copies
    // in the launcher/child are already closed: scrubbed, or by exit).
    drop(injected);
    drop(writer);

    let mut leaked = Vec::new();
    reader
        .read_to_end(&mut leaked)
        .expect("read the pipe read end");
    (obs, leaked)
}

#[test]
fn undeclared_inherited_fd_is_scrubbed_before_the_workload() {
    let (obs, leaked) = run_with_undeclared_fd();
    assert!(
        obs.exec_succeeded(),
        "the fd-scrub workload must reach ExecSucceeded; terminal={:?} notes={:?}",
        obs.terminal,
        obs.notes
    );
    let out = String::from_utf8_lossy(&obs.captured_stdout);

    // Witness (a): the workload's OWN report — its write to the undeclared fd failed.
    assert!(
        out.contains("SCRUBBED") && !out.contains("WROTE"),
        "the workload must report the undeclared fd was SCRUBBED (closed); got stdout={out:?}"
    );

    // Witness (b), INDEPENDENT + host-side: nothing was written through the pipe, so
    // the undeclared fd never carried data across the boundary.
    assert!(
        leaked.is_empty(),
        "an undeclared inherited fd LEAKED across the boundary: host read {leaked:?} from the pipe"
    );
}

#[test]
fn fd_scrub_is_deterministic_across_runs() {
    for run in 0..5 {
        let (obs, leaked) = run_with_undeclared_fd();
        let out = String::from_utf8_lossy(&obs.captured_stdout);
        assert!(
            obs.exec_succeeded() && out.contains("SCRUBBED") && leaked.is_empty(),
            "run {run}: the fd scrub must be deterministic; stdout={out:?} leaked={leaked:?}"
        );
    }
}
