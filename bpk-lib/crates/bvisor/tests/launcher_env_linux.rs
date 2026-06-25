// LAUNCHER MECHANISM proof (a building block, NOT a contract-admission proof): the
// workload's process environment is EXACTLY the `TargetSpecV1.envp` the launcher is
// handed — nothing is inherited from the launcher (or, transitively, the host). The
// launcher serves that envp directly to `fexecve`.
//
// SCOPE (codex review 2026-06-25): this proves the launcher CAN serve an explicit
// envp; it does NOT prove the `Environment` capability is admitted + honored end to
// end. plan_build currently emits a HARDCODED envp and never lowers the spec's
// `EnvPolicy`, so `Environment` is NOT in the ceiling (fails closed). When the policy
// is genuinely lowered, this becomes the mechanism half + a contract-level oracle
// (spec -> plan -> execute) is added alongside.
//
// WHY THIS IS A REAL ORACLE (not a self-report). The witness is the WORKLOAD's own
// `env` output, captured through the launcher's piped stdout — never the backend's
// claim. And it is non-vacuous: `run_launcher` runs the launcher with its
// environment CLEARED to ONLY the four `BVISOR_*_FD` channel variables
// (`backend/linux/sys.rs::spawn_launcher_with_fds` calls `env_clear()` then sets
// just those). Those variables are GUARANTEED present in the launcher's own
// environment, so if the workload's `env` output does not contain them, the
// launcher demonstrably did NOT inherit its environment into the workload — it
// passed the explicit declared envp. A declared marker variable proves the
// declared set passes through.
//
// `#![cfg(target_os = "linux")]` — real clone3 + fexecve through the launcher bin.

#![cfg(target_os = "linux")]

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

const SLOT_EXE: RawFd = 10;

// The launcher-channel environment variables `run_launcher` sets on the launcher
// process (mirrors the private `ENV_*` consts in `backend/linux/launch.rs`). These
// are GUARANTEED to be in the launcher's environment, so their absence from the
// workload's environment is the load-bearing proof of non-inheritance.
const LAUNCHER_CHANNEL_ENV: &[&str] = &[
    "BVISOR_LAUNCH_PLAN_FD",
    "BVISOR_CONTROL_FD",
    "BVISOR_ERROR_FD",
    "BVISOR_ERROR_READ_FD",
];

// A distinctive declared variable: present in the plan's envp, so it MUST reach the
// workload (the declared set passes through).
const DECLARED_MARKER: &str = "BVISOR_DECLARED_ONLY=present";

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

/// An exec-only plan whose workload is `argv` and whose declared environment is
/// `envp`. `h_l` binds the schedule digest (the real H_L binding is #75).
fn exec_only_plan(argv: Vec<String>, envp: Vec<(String, String)>) -> LinuxLaunchPlanV1 {
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
            envp,
            exe_slot: u32::try_from(SLOT_EXE).expect("fd fits u32"),
            user_namespace: None,
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

/// Run `sh -c 'env'` with a declared envp and capture the workload's environment.
fn run_env_workload() -> LaunchObservation {
    let argv = vec!["sh".to_string(), "-c".to_string(), "env".to_string()];
    let envp = vec![
        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
        ("BVISOR_DECLARED_ONLY".to_owned(), "present".to_owned()),
    ];
    let plan = exec_only_plan(argv, envp);
    launch::run_launcher(&launcher_path(), &plan, vec![exe_authority()])
        .expect("the launcher harness runs the env workload to a verdict")
}

#[test]
fn workload_environment_is_exactly_the_declared_envp() {
    let obs = run_env_workload();
    assert!(
        obs.exec_succeeded(),
        "the env workload must reach ExecSucceeded; terminal={:?} notes={:?}",
        obs.terminal,
        obs.notes
    );
    let env = String::from_utf8_lossy(&obs.captured_stdout);

    // The declared variable passes through to the workload.
    assert!(
        env.contains(DECLARED_MARKER),
        "the declared envp must reach the workload; expected {DECLARED_MARKER:?} in env output:\n{env}"
    );

    // The launcher's OWN channel environment does NOT leak into the workload — the
    // load-bearing proof that the workload's env is the explicit declared set, not
    // an inherited one.
    for leaked in LAUNCHER_CHANNEL_ENV {
        assert!(
            !env.contains(leaked),
            "launcher channel env var {leaked:?} leaked into the workload environment \
             (env is inherited, not explicit); env output:\n{env}"
        );
    }
    // No BVISOR_*_FD launcher plumbing of any shape reached the workload.
    assert!(
        !env.contains("BVISOR_LAUNCH"),
        "no launcher plumbing variable may reach the workload; env output:\n{env}"
    );
}

/// Determinism: the environment isolation holds across repeated real spawns.
#[test]
fn environment_isolation_is_deterministic_across_runs() {
    for run in 0..5 {
        let obs = run_env_workload();
        let env = String::from_utf8_lossy(&obs.captured_stdout);
        assert!(
            env.contains(DECLARED_MARKER) && !env.contains("BVISOR_LAUNCH"),
            "run {run}: environment isolation must be deterministic; env output:\n{env}"
        );
    }
}
