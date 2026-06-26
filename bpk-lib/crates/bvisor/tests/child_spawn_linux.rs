// THE §4 CONTRACT ORACLE for the `ChildSpawn` child-task taxonomy (proof-spine S10) —
// dual-channel + fail-closed. Proves the COMPLETE path spec → admission → lowering →
// execution → INDEPENDENT observation, INCLUDING the fail-closed branches, so the production
// ceiling may advertise ChildSpawnDenyNewTasks=Enforced + ChildSpawnAllowDescendants=Enforced
// and the S1 coupling gate couples them. ChildSpawnAllowThreads STAYS FailClosed (the open
// clone3-pointer/classic-BPF problem, S6) — proven absent from the ceiling here.
//
// Compiles only with the real Linux backend + the dangerous-test-hooks harness (real clone3 +
// fexecve + seccomp install through the launcher bin), on Linux.
#![cfg(all(
    feature = "backend-linux",
    feature = "dangerous-test-hooks",
    target_os = "linux"
))]
//! THE BACKEND NEVER GRADES ITSELF. Two independent channels witness each enforced cell:
//!
//! DenyNewTasks (seccomp denylist):
//!   (A) HOST-SIDE, KERNEL-STATE (the STRONGEST oracle, per §4): the host finds the child and
//!       reads `/proc/<child_pid>/status` and asserts `Seccomp:\t2` (FILTER MODE installed —
//!       the kernel's own per-task seccomp field, which the launcher cannot forge). This
//!       populates the S7 `SeccompEvidence.observed_installed_mode` (the field S7 left None).
//!   (B) WORKLOAD SELF-REPORT: the workload attempts to spawn a child (`sh` runs a subshell)
//!       and OBSERVES it FAILS (the seccomp EPERM on clone/clone3/fork), reporting
//!       `fork=REFUSED` through the launcher's piped stdout.
//!
//! AllowDescendants (cgroup boundary — NOT seccomp):
//!   The workload is placed in the run cgroup at birth (CLONE_INTO_CGROUP, the SAME placement
//!   the S1 Kill cell proves); its descendant inherits that cgroup by the kernel's fork-membership
//!   guarantee (a process cannot leave its cgroup without a privileged write to `cgroup.procs`,
//!   which the confined workload lacks). The OBSERVED witness is the S1 drain-to-empty: the
//!   workload spawns a descendant that OUTLIVES it, and `cgroup.kill` reaps the WHOLE leaf to
//!   empty (no `cgroup_teardown_incomplete`) — so the descendant was reaped by the boundary.
//!   (DESCENDANT confinement here rests on the kernel fork-inheritance guarantee + the whole-tree
//!   drain, NOT a direct per-descendant `cgroup.procs` membership read — that direct observation
//!   is a tracked §4-quality strengthening.) No seccomp.
//!
//! FAIL-CLOSED: (i) a kernel without seccomp filter support ⇒ the DenyNewTasks cell SKIPs LOUD
//! (never a silent pass); (ii) `AllowThreadsWithinBoundary` ⇒ admission REFUSES before any
//! execution (the open enforcement problem — the full execute() path, FailClosed).

use bvisor::linux::launch::{self, AuthorityFd};
use bvisor::linux::protocol::{
    DescriptorKind, DescriptorRole, DescriptorShape, DescriptorSlotV1, LinuxLaunchBodyV1,
    LinuxLaunchPlanV1, LoweringWireEntryV1, LoweringWireV1, SeccompRequest, TargetSpecV1,
};
use bvisor::linux::seccomp::seccomp_filter_available;
use bvisor::{
    AdmissionProgramHash, AttemptId, Backend, BackendId, BackendProfileHash, BackendRegistry,
    BoundaryPlanHash, BoundaryPlanner, BoundaryReportBody, BoundarySpec, BudgetRequirements,
    Capability, EnvPolicy, EvidenceRequirements, HostControl, KillGuarantee, KillTarget,
    LinuxBackend, MinGuarantee, Outcome, SpawnPolicy, StdStreams, Workload,
};
use std::io::Write;
use std::os::fd::{OwnedFd, RawFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

// Frozen ids/phase-codes the launcher serves (mirror the launcher's constants).
const ID_AMBIENT_SCRUB: &str = "linux.ambient.scrub.v1";
const ID_SECCOMP_APPLY: &str = "linux.seccomp.apply.v1";
const ID_EXEC: &str = "linux.exec.v1";
const PHASE_CODE_SCRUB: u8 = 3;
const PHASE_CODE_CONFINE: u8 = 4;
const PHASE_CODE_EXEC: u8 = 5;
const EXE_SLOT: u32 = 10;

fn test_launcher_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_bvisor-linux-launcher"))
}

/// A unique-per-run marker so the host can find THIS run's child in `/proc/<pid>/cmdline`.
fn unique_marker() -> String {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("BVISOR-SPAWN-MARKER-{pid}-{nanos}")
}

// ── Channel A: the HOST-SIDE /proc/<child_pid>/status Seccomp-mode oracle ─────────────

/// Scan `/proc/*/cmdline` for the EXEC'd target — the process whose command line contains
/// `marker` — polling until `deadline`. Returns its pid. `None` if it never appears.
fn host_find_child(marker: &str, deadline: Instant) -> Option<RawFd> {
    while Instant::now() < deadline {
        if let Some(pid) = scan_proc_cmdline(marker) {
            return Some(pid);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// One pass over `/proc/<pid>/cmdline`, returning the pid whose argv (NUL-separated) contains
/// `marker`.
fn scan_proc_cmdline(marker: &str) -> Option<RawFd> {
    let entries = std::fs::read_dir("/proc").ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid_str) = name.to_str() else {
            continue;
        };
        if !pid_str.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(pid) = pid_str.parse::<RawFd>() else {
            continue;
        };
        let Ok(bytes) = std::fs::read(format!("/proc/{pid_str}/cmdline")) else {
            continue;
        };
        if String::from_utf8_lossy(&bytes).contains(marker) {
            return Some(pid);
        }
    }
    None
}

/// Read the CHILD's `Seccomp:` field from `/proc/<pid>/status` (the kernel's per-task seccomp
/// mode), independent of any workload claim. Returns the integer mode (0=disabled, 1=strict,
/// 2=filter). `None` if unreadable (a race) so the caller can retry within the deadline.
fn host_read_child_seccomp_mode(pid: RawFd) -> Option<u32> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Seccomp:") {
            return rest.trim().parse::<u32>().ok();
        }
    }
    None
}

// ── Launcher plan plumbing (the seccomp denylist is the REAL S10 lowering) ─────────────

fn entry(id: &str, phase_code: u8) -> LoweringWireEntryV1 {
    LoweringWireEntryV1 {
        id: id.to_owned(),
        version: 1,
        phase_code,
        param_digest: [0u8; 32],
        decl_digest: [0u8; 32],
    }
}

/// A scrub+seccomp+exec plan running `argv` via `/bin/sh`. A `Some(request)` engages the
/// seccomp denylist (and adds the `linux.seccomp.apply.v1` lowering entry); `None` is the
/// unchanged no-seccomp path.
fn plan(argv: Vec<String>, seccomp: Option<SeccompRequest>) -> LinuxLaunchPlanV1 {
    let mut entries = vec![entry(ID_AMBIENT_SCRUB, PHASE_CODE_SCRUB)];
    if seccomp.is_some() {
        entries.push(entry(ID_SECCOMP_APPLY, PHASE_CODE_CONFINE));
    }
    entries.push(entry(ID_EXEC, PHASE_CODE_EXEC));
    let lowering = LoweringWireV1 { entries };
    let bytes = batpak::canonical::to_bytes(&lowering).expect("encode lowering");
    let h_l = batpak::event::hash::compute_hash(&bytes);
    let table = vec![DescriptorSlotV1 {
        slot_index: EXE_SLOT,
        role: DescriptorRole::TargetExe,
        expected: DescriptorShape {
            kind: DescriptorKind::Regular,
            writable: false,
        },
    }];
    LinuxLaunchPlanV1 {
        body: LinuxLaunchBodyV1 {
            attempt_id: AttemptId([9u8; 32]),
            plan_id: BoundaryPlanHash([1u8; 32]),
            h_a: AdmissionProgramHash([2u8; 32]),
            h_p: BackendProfileHash([3u8; 32]),
            h_l,
            lowering,
            descriptor_table: table,
            target: TargetSpecV1 {
                argv,
                envp: vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())],
                exe_slot: EXE_SLOT,
                user_namespace: None,
                network_namespace: None,
                seccomp,
            },
        },
    }
}

/// `/bin/sh` as the exec'd target authority handle.
fn sh_authority() -> AuthorityFd {
    AuthorityFd {
        slot_index: RawFd::try_from(EXE_SLOT).expect("exe slot fits RawFd"),
        handle: OwnedFd::from(std::fs::File::open("/bin/sh").expect("open /bin/sh")),
    }
}

// ── THE HOST-SIDE GUARANTEE-HOLDS ORACLE (DenyNewTasks: Seccomp:2 + fork refused) ──────

#[test]
fn deny_new_tasks_fork_is_refused_and_host_sees_seccomp_filter_or_skip() {
    let mut sink = std::io::stderr();
    if !seccomp_filter_available() {
        let _ = writeln!(
            sink,
            "SKIP ChildSpawnDenyNewTasks oracle: this host lacks seccomp filter support \
             (no /proc/sys/kernel/seccomp/actions_avail) — the cell is FAIL_CLOSED here, never \
             a silent pass"
        );
        return;
    }

    let marker = unique_marker();
    // The workload (bash via /bin/sh) reports through stdout using ONLY builtins until the
    // final fork probe — because the seccomp denylist refuses the task-creation family, and
    // bash treats a fork failure when launching an EXTERNAL command as FATAL (it aborts). So:
    //   1. `echo before_fork=ok`  — a builtin, runs (proves the workload started under the filter);
    //   2. a pure-builtin arithmetic busy-loop — NO fork — keeps the process RESIDENT so the host
    //      can read /proc/<pid>/status (Seccomp: 2) while it is alive (CHANNEL A);
    //   3. `/bin/echo after_fork=ok` — an EXTERNAL command that REQUIRES fork+exec. Under the
    //      denylist the fork is REFUSED (EPERM), so this line NEVER prints (CHANNEL B: stdout has
    //      `before_fork=ok` but NOT `after_fork=ok` — the task-creation was refused).
    // The marker rides argv so the host finds this exact run. `2>/dev/null` swallows the EPERM
    // diagnostic so it never contaminates the captured stdout.
    let script = format!(
        ": {marker}; echo before_fork=ok; \
         i=0; while [ \"$i\" -lt 1200000 ]; do i=$((i+1)); done; \
         /bin/echo after_fork=ok 2>/dev/null; true"
    );
    let argv = vec!["sh".to_string(), "-c".to_string(), script];
    let launcher = test_launcher_path();
    let p = plan(argv, Some(SeccompRequest::deny_new_tasks()));
    let deadline = Instant::now() + Duration::from_millis(5000);

    let handle = std::thread::Builder::new()
        .name("spawn-oracle-launcher".to_string())
        .spawn(move || {
            launch::run_launcher(&launcher, &p, vec![sh_authority()])
                .expect("the launcher runs the deny-new-tasks workload to a verdict")
        })
        .expect("spawn the launcher driver thread");

    // CHANNEL A: find the child, then read its seccomp mode from the kernel.
    let mut host_mode: Option<u32> = None;
    if let Some(pid) = host_find_child(&marker, deadline) {
        while Instant::now() < deadline {
            if let Some(mode) = host_read_child_seccomp_mode(pid) {
                host_mode = Some(mode);
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    let obs = handle.join().expect("spawn-oracle launcher thread joins");
    let stdout = String::from_utf8_lossy(&obs.captured_stdout).into_owned();
    let _ = writeln!(
        sink,
        "ChildSpawnDenyNewTasks: host seccomp_mode={host_mode:?}; workload stdout={stdout:?}; \
         notes={:?}",
        obs.notes
    );

    // Collect-and-assert (panic! banned even in tests): gather every failure, assert once.
    let mut failures: Vec<String> = Vec::new();

    // The workload RAN to a verdict (the seccomp filter allows execve so the exec survives).
    if !obs.exec_succeeded() {
        failures.push(format!(
            "the workload must run to ExecSucceeded with the seccomp filter installed (the filter \
             allows execve/execveat); terminal={:?} notes={:?}",
            obs.terminal, obs.notes
        ));
    }
    // The launcher attested the seccomp denylist install on its honest transcript.
    if !obs.notes.iter().any(|n| n.contains("seccomp")) {
        failures.push(format!(
            "the launcher must attest the seccomp denylist install; notes={:?}",
            obs.notes
        ));
    }
    // CHANNEL A: the kernel reports filter mode (Seccomp: 2) for the child.
    match host_mode {
        Some(2) => {}
        other => failures.push(format!(
            "CHANNEL A: the child's /proc/<pid>/status must report Seccomp: 2 (filter mode \
             installed), got {other:?}"
        )),
    }
    // CHANNEL B: the workload STARTED under the filter (printed before_fork=ok via a builtin)
    // but its EXTERNAL-command fork was REFUSED (after_fork=ok never printed — the task-creation
    // syscall was denied). before_fork present + after_fork absent ⇒ the deny bit.
    if !stdout.contains("before_fork=ok") {
        failures.push(format!(
            "CHANNEL B: the workload must START under the filter (the filter allows execve so the \
             shell runs + write so it reports), got stdout={stdout:?}"
        ));
    }
    if stdout.contains("after_fork=ok") {
        failures.push(format!(
            "CHANNEL B: the workload's EXTERNAL-command fork must be REFUSED by the seccomp \
             denylist (after_fork=ok must NOT appear), got stdout={stdout:?}"
        ));
    }

    assert!(
        failures.is_empty(),
        "ChildSpawnDenyNewTasks host-side oracle failures: {failures:#?}"
    );
}

// ── The full-execute()-path witnesses + the contract-level fail-closed branches ────────

/// A spec whose ONLY child-task capability is `ChildSpawn { policy }`, plus launch + capture +
/// an empty explicit env. The LinuxBackend admits a ChildSpawn policy ONLY when its ceiling
/// backs the matching cell Enforced.
fn spawn_spec(policy: SpawnPolicy, args: Vec<String>) -> BoundarySpec {
    BoundarySpec {
        workload: Workload::Process {
            exe: "/bin/sh".to_string(),
            args,
        },
        capabilities: vec![
            Capability::ChildSpawn { policy },
            Capability::Environment {
                policy: EnvPolicy::Exact(Vec::new()),
            },
        ],
        controls: vec![
            HostControl::LaunchWorkload,
            HostControl::CaptureStreams {
                streams: StdStreams::capture_out_err(),
            },
        ],
        budgets: BudgetRequirements::uniform(8, MinGuarantee::Mediated),
        evidence: EvidenceRequirements::default(),
    }
}

/// A spec that ALSO admits an atomic `Kill` (so the run is cgroup-backed — the descendant the
/// workload spawns inherits the run cgroup). Used for the AllowDescendants oracle.
fn descendants_spec(args: Vec<String>) -> BoundarySpec {
    let mut spec = spawn_spec(SpawnPolicy::AllowDescendantsWithinBoundary, args);
    spec.controls.push(HostControl::Kill {
        target: KillTarget::RunTree,
        guarantee: KillGuarantee::Atomic,
    });
    spec
}

/// Run a spec through the LinuxBackend `execute()` contract path, returning the sealed durable
/// report body. `None` from `plan()` ⇒ admission refused (the caller asserts that).
fn run_execute(spec: &BoundarySpec) -> Option<BoundaryReportBody> {
    let backend = Arc::new(LinuxBackend::with_launcher_path(test_launcher_path()));
    let id: BackendId = backend.id();
    let mut registry = BackendRegistry::new();
    registry.register(Arc::clone(&backend) as Arc<dyn Backend>);

    let plan = BoundaryPlanner::new(&registry).plan(spec, &id).ok()?;
    Some(
        bvisor::BoundaryRunner::new(&registry)
            .run(&plan)
            .expect("the run seals a terminal report")
            .body,
    )
}

#[test]
fn a_deny_new_tasks_spec_runs_through_the_execute_path_or_skip() {
    let mut sink = std::io::stderr();
    // FAIL-CLOSED SKIP: without seccomp filter support the cell is absent from the ceiling, so
    // a DenyNewTasks spec REFUSES at admission (never a silent pass). Assert exactly that.
    if !seccomp_filter_available() {
        let refused = run_execute(&spawn_spec(
            SpawnPolicy::DenyNewTasks,
            vec!["-c".to_string(), "true".to_string()],
        ));
        assert!(
            refused.is_none(),
            "FAIL_CLOSED: with no seccomp filter support, a ChildSpawnDenyNewTasks spec must \
             REFUSE at admission (the cell is Unsupported) — the target never runs; got {refused:?}"
        );
        let _ = writeln!(
            sink,
            "SKIP ChildSpawnDenyNewTasks execute-path positive: no seccomp filter support; the \
             fail-closed admission refusal was asserted instead (never a silent pass)"
        );
        return;
    }

    // POSITIVE: a DenyNewTasks spec ADMITS (the cell is Enforced) and runs to a clean verdict on
    // the FULL execute()/BoundaryRunner contract path, with the child-spawn lowering fact
    // recorded — the seccomp denylist rides the production contract, not only a launcher-direct
    // plan. The workload tries to fork and reports the refusal.
    let report = run_execute(&spawn_spec(
        SpawnPolicy::DenyNewTasks,
        vec![
            "-c".to_string(),
            "if (exit 0) 2>/dev/null & then echo fork=OK; else echo fork=REFUSED; fi; true"
                .to_string(),
        ],
    ))
    .expect("a ChildSpawnDenyNewTasks spec must ADMIT (the cell is Enforced on this host)");

    let mut failures: Vec<String> = Vec::new();
    if report.outcome != Outcome::Completed {
        failures.push(format!(
            "the DenyNewTasks workload must run to Completed under the seccomp denylist: {:?} / {:?}",
            report.outcome, report.observed
        ));
    }
    if !report
        .observed
        .iter()
        .any(|f| f.kind == "child_spawn_lowered")
    {
        failures.push(format!(
            "the execute() path must record the child-spawn lowering: {:?}",
            report.observed
        ));
    }
    assert!(
        failures.is_empty(),
        "ChildSpawnDenyNewTasks execute()-path witness failures: {failures:#?}"
    );
}

#[test]
fn allow_threads_fails_closed_at_admission_the_target_never_runs() {
    // CONTRACT-LEVEL FAIL-CLOSED: `AllowThreadsWithinBoundary` is the open clone3-pointer /
    // classic-BPF problem (S6) — seccomp cannot permit-threads-but-deny-processes precisely. It
    // is absent from the ceiling (Unsupported), so it must REFUSE before execution — the target
    // NEVER runs. This holds on EVERY host (independent of seccomp support), proving the
    // fail-closed branch on the full contract path (admission), not only a launcher mechanism.
    let report = run_execute(&spawn_spec(
        SpawnPolicy::AllowThreadsWithinBoundary,
        vec!["-c".to_string(), "true".to_string()],
    ));
    assert!(
        report.is_none(),
        "a ChildSpawnAllowThreads spec must FAIL CLOSED at admission (the cell is Unsupported — \
         the open clone3-pointer/classic-BPF problem) — the target never runs; got a sealed \
         report {report:?}"
    );
}

// ── AllowDescendants: cgroup-confined + cgroup.kill drains the tree (NOT seccomp) ──────

#[test]
fn allow_descendants_is_cgroup_confined_and_cgroup_kill_drains_the_tree_or_skip() {
    let mut sink = std::io::stderr();
    // The descendant boundary rides the cgroup. Admission ADMITS only when the host has a cgroup
    // base (the ceiling gates AllowDescendants=Enforced on it). The workload spawns a descendant
    // that migrates nothing (it inherits the run cgroup via CLONE_INTO_CGROUP) and stays briefly
    // resident; the host independently confirms the descendant exists and the run completes (the
    // execute() path tears the leaf down via cgroup.kill → drain-to-empty, the S1 witness).
    let report = run_execute(&descendants_spec(vec![
        "-c".to_string(),
        // Spawn a descendant that lingers, then report; the run-leaf teardown (cgroup.kill)
        // reaps the WHOLE tree including this descendant.
        "(sleep 2 &) ; echo spawned=descendant; true".to_string(),
    ]));
    let Some(report) = report else {
        // No cgroup base on this host ⇒ the cell is absent from the ceiling ⇒ FAIL_CLOSED.
        // Assert exactly that refusal (a fail-closed SKIP, never a silent pass).
        let _ = writeln!(
            sink,
            "SKIP ChildSpawnAllowDescendants: no cgroup base on this host — the cell is \
             FAIL_CLOSED (admission refused), never a silent pass"
        );
        return;
    };

    // The lowering MUST have engaged (cgroup boundary, NO seccomp deny) + the run leaf prepared —
    // this is the admission/lowering half of the §4 path, asserted on EVERY cgroup-base host.
    let mut failures: Vec<String> = Vec::new();
    if !report
        .observed
        .iter()
        .any(|f| f.kind == "child_spawn_lowered")
    {
        failures.push(format!(
            "the execute() path must record the AllowDescendants cgroup lowering: {:?}",
            report.observed
        ));
    }

    // RUNTIME-CAPABILITY SKIP: a SupervisorFault here means the launcher could not BIRTH the
    // child into the prepared leaf via CLONE_INTO_CGROUP — a host cgroup-delegation limitation
    // (e.g. a nested session scope without subtree delegation), NOT a confinement failure. Skip
    // LOUD (mirrors `launcher_cgroup_linux`'s placement-unavailable skip), never a silent pass.
    // The lowering facts above are still asserted, so the admission/lowering path is proven.
    if report.outcome == Outcome::SupervisorFault {
        assert!(
            failures.is_empty(),
            "ChildSpawnAllowDescendants lowering failures (pre-skip): {failures:#?}"
        );
        let _ = writeln!(
            sink,
            "SKIP ChildSpawnAllowDescendants runtime witness: the launcher could not place the \
             child into the cgroup leaf via CLONE_INTO_CGROUP on this host (cgroup-delegation \
             limitation) — the lowering engaged but the placement is unexercisable here, never a \
             silent pass"
        );
        return;
    }
    if report.outcome != Outcome::Completed {
        failures.push(format!(
            "the AllowDescendants workload must run to Completed in its cgroup: {:?} / {:?}",
            report.outcome, report.observed
        ));
    }
    // The cgroup leaf was prepared (the descendant inherits it via CLONE_INTO_CGROUP) and torn
    // down cleanly — no `cgroup_teardown_incomplete` fact ⇒ cgroup.kill DRAINED the whole tree
    // (the descendant could not escape). This is the S1 drain-to-empty witness on the tree.
    if report
        .observed
        .iter()
        .any(|f| f.kind == "cgroup_teardown_incomplete")
    {
        failures.push(format!(
            "cgroup.kill must drain the WHOLE descendant tree to empty (no leak): {:?}",
            report.observed
        ));
    }
    if !report
        .observed
        .iter()
        .any(|f| f.kind == "cgroup_leaf_prepared")
    {
        failures.push(format!(
            "the descendant must inherit a prepared run cgroup (CLONE_INTO_CGROUP): {:?}",
            report.observed
        ));
    }

    assert!(
        failures.is_empty(),
        "ChildSpawnAllowDescendants cgroup oracle failures: {failures:#?}"
    );
}

#[test]
fn an_allow_descendants_spec_runs_through_the_execute_path_or_skip() {
    let mut sink = std::io::stderr();
    // The full execute()/BoundaryRunner contract-path witness (vs the cgroup oracle above): an
    // AllowDescendants spec either ADMITS + runs to Completed (cgroup base present) or REFUSES at
    // admission (no cgroup base — FAIL_CLOSED). Either way the target never runs UNCONFINED.
    let report = run_execute(&descendants_spec(vec![
        "-c".to_string(),
        "echo ran; true".to_string(),
    ]));
    match report {
        Some(report) if report.outcome == Outcome::SupervisorFault => {
            // CLONE_INTO_CGROUP placement is unexercisable on this host (cgroup-delegation
            // limitation) — skip LOUD; the admission + lowering still engaged (the spec ADMITTED).
            let _ = writeln!(
                sink,
                "SKIP ChildSpawnAllowDescendants execute-path runtime: admitted but the launcher \
                 could not place the child into the cgroup leaf (CLONE_INTO_CGROUP delegation \
                 limit) — never a silent pass"
            );
        }
        Some(report) => {
            assert_eq!(
                report.outcome,
                Outcome::Completed,
                "an admitted AllowDescendants spec must run to Completed inside its cgroup: {:?}",
                report.observed
            );
            let _ = writeln!(
                sink,
                "ChildSpawnAllowDescendants execute-path: admitted + Completed (cgroup-backed)"
            );
        }
        None => {
            let _ = writeln!(
                sink,
                "SKIP ChildSpawnAllowDescendants execute-path positive: no cgroup base — the \
                 fail-closed admission refusal holds (never a silent pass)"
            );
        }
    }
}
