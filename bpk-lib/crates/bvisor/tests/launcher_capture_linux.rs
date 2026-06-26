// HONEST workload-stream capture THROUGH the host-side launcher harness
// (`run_launcher` → `spawn_launcher_with_fds`'s piped stdio). This proves the
// backend→launcher cutover's RESTORED stdout/stderr capture: the workload inherits the
// launcher's fd 0/1/2 (the scrub allowlists stdio), the launcher is stdio-SILENT on every
// workload-running path, so the launcher process's piped stdout/stderr carry EXACTLY the
// workload's output — the honest backing for `CaptureStreams=Enforced`.
//
// Real-OS: spawns the `bvisor-linux-launcher` bin, so it is gated to Linux + the
// backend-linux feature. NO landlock action is scheduled (an exec-only plan), so this
// test needs NO live-ABI gate — capture is orthogonal to confinement and must work on
// every kernel.
#![cfg(all(target_os = "linux", feature = "backend-linux"))]
//! CLEAN-SEPARATION PROOF: a workload prints a KNOWN marker to stdout AND a different
//! marker to stderr (`printf OUT; printf ERR 1>&2`). The test asserts:
//!   - the captured stdout contains `OUT` and NOT `ERR` (streams are not crossed);
//!   - the captured stderr contains `ERR` and NOT `OUT`;
//!   - NEITHER captured stream contains any launcher diagnostic string
//!     (`bvisor-linux-launcher` / `SetupRefused` / `SetupFaulted`), proving the launcher
//!     is stdio-silent and the capture is the workload's output ALONE.
//!
//! Run 5x via the determinism harness below (real process spawn).

use bvisor::linux::launch::{self, AuthorityFd, LaunchObservation};
use bvisor::linux::protocol::{
    DescriptorKind, DescriptorRole, DescriptorShape, DescriptorSlotV1, LinuxLaunchBodyV1,
    LinuxLaunchPlanV1, LoweringWireEntryV1, LoweringWireV1, TargetSpecV1,
};
use bvisor::{AdmissionProgramHash, AttemptId, BackendProfileHash, BoundaryPlanHash};
use std::os::fd::{OwnedFd, RawFd};
use std::path::PathBuf;

// Frozen ids/phase-codes the launcher serves (mirror the launcher's constants).
const ID_AMBIENT_SCRUB: &str = "linux.ambient.scrub.v1";
const ID_EXEC: &str = "linux.exec.v1";
const PHASE_CODE_SCRUB: u8 = 3; // LoweringPhase::FdHygiene.code()
const PHASE_CODE_EXEC: u8 = 5; // LoweringPhase::Launch.code()

// The exe rides this slot fd number (== the descriptor-table slot index). The harness
// places its OWN channel fds strictly ABOVE every authority slot, so a small fixed
// number never collides.
const SLOT_EXE: RawFd = 10;

/// The launcher binary the harness spawns (the test-compiled `[[bin]]`).
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

/// An exec-only plan (scrub + exec, NO landlock action — Confinement NotRequired) whose
/// `h_l` is `blake3(canonical(lowering))` so the launcher's schedule-digest binding
/// passes (the REAL H_L binding is #75 — note it). The workload is `argv`.
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

/// The exe authority handle (the workload binary the launcher `fexecve`s).
fn exe_authority() -> AuthorityFd {
    AuthorityFd {
        slot_index: SLOT_EXE,
        handle: OwnedFd::from(std::fs::File::open("/bin/sh").expect("open /bin/sh")),
    }
}

/// Run the marker workload once through `run_launcher`, returning the observation.
/// `printf OUT` → stdout, `printf ERR 1>&2` → stderr (DISTINCT markers, no trailing
/// newline so the exact bytes are unambiguous).
fn run_marker_workload() -> LaunchObservation {
    let argv = vec![
        "sh".to_string(),
        "-c".to_string(),
        "printf OUT; printf ERR 1>&2".to_string(),
    ];
    let plan = exec_only_plan(argv);
    launch::run_launcher(&launcher_path(), &plan, vec![exe_authority()])
        .expect("the launcher harness runs the marker workload to a verdict")
}

/// Any launcher diagnostic string that MUST NOT appear in a captured workload stream
/// (the launcher is stdio-silent on workload-running paths). If any of these leaks into
/// stdout/stderr the capture is contaminated by launcher output.
const LAUNCHER_DIAGNOSTICS: &[&str] = &[
    "bvisor-linux-launcher",
    "SetupRefused",
    "SetupFaulted",
    "launcher OS fault",
];

fn assert_clean_capture(obs: &LaunchObservation) {
    let out = String::from_utf8_lossy(&obs.captured_stdout);
    let err = String::from_utf8_lossy(&obs.captured_stderr);

    // The workload exec'd to success — the capture is of a real run, not a refusal.
    assert!(
        obs.exec_succeeded(),
        "the exec-only marker workload must reach ExecSucceeded; terminal={:?} \
         notes={:?}",
        obs.terminal,
        obs.notes
    );

    // Streams carry their OWN marker and NOT the other's (no crossing).
    assert!(
        out.contains("OUT"),
        "captured stdout must contain the stdout marker OUT; got {out:?}"
    );
    assert!(
        !out.contains("ERR"),
        "captured stdout must NOT contain the stderr marker ERR (streams crossed); \
         got {out:?}"
    );
    assert!(
        err.contains("ERR"),
        "captured stderr must contain the stderr marker ERR; got {err:?}"
    );
    assert!(
        !err.contains("OUT"),
        "captured stderr must NOT contain the stdout marker OUT (streams crossed); \
         got {err:?}"
    );

    // CLEAN: no launcher diagnostic leaked into either captured stream — proving the
    // launcher is stdio-silent and the capture is the workload's output ALONE.
    for diag in LAUNCHER_DIAGNOSTICS {
        assert!(
            !out.contains(diag),
            "captured stdout must be free of launcher diagnostic {diag:?} (launcher \
             not stdio-silent); got {out:?}"
        );
        assert!(
            !err.contains(diag),
            "captured stderr must be free of launcher diagnostic {diag:?} (launcher \
             not stdio-silent); got {err:?}"
        );
    }
}

/// HONEST workload-stream capture through the launcher's inherited piped stdio: the
/// workload's stdout/stderr markers are captured cleanly and NO launcher diagnostic
/// contaminates either stream. Repeated 5x for determinism (real process spawn).
#[test]
fn launcher_captures_workload_streams_cleanly_and_deterministically() {
    for iteration in 0..5 {
        let obs = run_marker_workload();
        assert_clean_capture(&obs);
        // Determinism: the EXACT captured bytes are identical every iteration (the
        // workload is deterministic and the launcher adds nothing).
        assert_eq!(
            obs.captured_stdout, b"OUT",
            "iteration {iteration}: captured stdout must be exactly the workload's \
             `OUT` bytes (no launcher contamination, deterministic)"
        );
        assert_eq!(
            obs.captured_stderr, b"ERR",
            "iteration {iteration}: captured stderr must be exactly the workload's \
             `ERR` bytes (no launcher contamination, deterministic)"
        );
    }
}

/// A workload that floods stdout far past one kernel pipe buffer (~64 KiB) is captured
/// IN FULL without deadlock. This is the regression guard for the concurrent-drain fix:
/// the old read-AFTER-wait capture would hang here — the workload blocks writing to a
/// full pipe, never exits, so `child.wait()` never returns. Draining stdout/stderr on
/// scoped threads while the launcher runs keeps the pipe emptying so the workload always
/// makes progress. 256 KiB > 4x a default pipe buffer.
#[test]
fn large_workload_output_is_fully_captured_without_deadlock() {
    const FLOOD_BYTES: usize = 256 * 1024;
    let argv = vec![
        "sh".to_string(),
        "-c".to_string(),
        // `head -c N /dev/zero | tr` emits exactly N printable bytes to stdout.
        format!("head -c {FLOOD_BYTES} /dev/zero | tr '\\0' 'X'"),
    ];
    let plan = exec_only_plan(argv);
    let obs = launch::run_launcher(&launcher_path(), &plan, vec![exe_authority()])
        .expect("the launcher harness runs the flood workload to a verdict");
    assert!(
        obs.exec_succeeded(),
        "the flood workload must reach ExecSucceeded; terminal={:?} notes={:?}",
        obs.terminal,
        obs.notes
    );
    assert_eq!(
        obs.captured_stdout.len(),
        FLOOD_BYTES,
        "every flooded byte must be captured (no deadlock, no truncation)"
    );
    assert!(
        obs.captured_stdout.iter().all(|&b| b == b'X'),
        "the captured flood must be exactly the workload's bytes"
    );
}
