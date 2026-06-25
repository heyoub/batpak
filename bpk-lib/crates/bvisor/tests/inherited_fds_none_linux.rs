// THE §4 CONTRACT ORACLE for `InheritedFds::None` (proof-spine S5) — dual-channel +
// fail-closed. Proves the COMPLETE path spec → admission → lowering → execution →
// INDEPENDENT observation, INCLUDING the fail-closed branches, so the production
// ceiling may advertise InheritedFds::None=Enforced and the S1 coupling gate couples it.
//
// Compiles only with the real Linux backend + the dangerous-test-hooks harness
// (real clone3 + fexecve through the launcher bin), on Linux.
#![cfg(all(
    feature = "backend-linux",
    feature = "dangerous-test-hooks",
    target_os = "linux"
))]
//! THE BACKEND NEVER GRADES ITSELF. Two independent channels witness the child's open
//! file descriptors:
//!   (A) HOST-SIDE, KERNEL-STATE: the host reads `/proc/<child_pid>/fd` (the kernel's
//!       own fd table) and asserts it contains ONLY the declared/allowlisted fds — never
//!       a workload claim. This is the strongest oracle (genuinely independent).
//!   (B) WORKLOAD SELF-REPORT: the workload tries to WRITE to a non-CLOEXEC SENTINEL fd
//!       the PARENT opened before launch; its write must FAIL (the fd was scrubbed),
//!       reported on the launcher-captured stdout.
//! NO LEAK: the parent opens an inheritable (non-CLOEXEC) sentinel fd before launch and
//! relocates it to a fixed number; the test asserts that fd number is ABSENT from the
//! child's `/proc/<pid>/fd` (scrubbed host-side) AND that nothing the workload wrote
//! crossed the pipe (no data leak).
//!
//! The lowering under test is the REAL contract: the admitted `FdPolicy::None` drives the
//! descriptor-table fd-scrub the launcher runs (every undeclared inherited fd closed
//! before `fexecve`). The host-side /proc observation drives `run_launcher` directly
//! (execute() exposes no child pid — the S4-blessed seam), and the FULL execute() path is
//! independently exercised by the contract-path witness + the fail-closed test below.
//!
//! FAIL-CLOSED: (i) an undeclared inherited fd is scrubbed BEFORE the workload (cited from
//! the launcher mechanism proof `launcher_inherited_fds_linux.rs`); (ii) an unrealized fd
//! policy (a setup failure) ⇒ the target NEVER runs, via the full execute() path.

use bvisor::linux::launch::{self, AuthorityFd};
use bvisor::linux::protocol::{
    DescriptorKind, DescriptorRole, DescriptorShape, DescriptorSlotV1, LinuxLaunchBodyV1,
    LinuxLaunchPlanV1, LoweringWireEntryV1, LoweringWireV1, TargetSpecV1,
};
use bvisor::{
    AdmissionProgramHash, AttemptId, Backend, BackendId, BackendProfileHash, BackendRegistry,
    BoundaryPlanHash, BoundaryPlanner, BoundaryReportBody, BoundarySpec, BudgetRequirements,
    Capability, EnvPolicy, EvidenceRequirements, FdPolicy, HostControl, LinuxBackend, MinGuarantee,
    Outcome, StdStreams, Workload,
};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

// Frozen ids/phase-codes the launcher serves (mirror the launcher's constants).
const ID_AMBIENT_SCRUB: &str = "linux.ambient.scrub.v1";
const ID_EXEC: &str = "linux.exec.v1";
const PHASE_CODE_SCRUB: u8 = 3;
const PHASE_CODE_EXEC: u8 = 5;
const SLOT_EXE: RawFd = 10;

// The injected undeclared (non-CLOEXEC) SENTINEL fd lands here: above the launcher
// channel fds (<= 14) and below the launcher's own relocation base (FD_RELOCATE_BASE ==
// 100), so it can collide with neither the channel plumbing nor a relocated source. It is
// the no-leak proof: it genuinely survives the launcher's execve and reaches the clone3
// child, so its ABSENCE from the child's /proc/<pid>/fd proves the scrub closed it.
const SENTINEL_FD: RawFd = 50;

fn test_launcher_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_bvisor-linux-launcher"))
}

/// A unique-per-run marker so the host can find THIS run's child in `/proc/<pid>/cmdline`
/// without racing other processes. Combines pid + a monotonic nanos timestamp.
fn unique_marker() -> String {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("BVISOR-FD-MARKER-{pid}-{nanos}")
}

/// Duplicate `fd` to a fresh number at/above `SENTINEL_FD` with CLOEXEC CLEARED (so it
/// survives the launcher's execve and is inherited by the clone3 child). `F_DUPFD`
/// allocates the LOWEST free fd at/above the base — it never clobbers an existing fd — so
/// the test is collision-safe across repeated runs. Returns the owned relocated fd; the
/// caller keeps it alive across the launcher run and reads its number for the workload.
/// (The `place_inheritable_high` pattern from launcher_inherited_fds_linux.rs:115.)
fn place_inheritable_high(fd: RawFd) -> OwnedFd {
    // SAFETY: test-only. F_DUPFD returns a fresh owned fd >= SENTINEL_FD with CLOEXEC
    // CLEARED (unlike F_DUPFD_CLOEXEC), or -1. We adopt it once.
    let new = unsafe { libc::fcntl(fd, libc::F_DUPFD, SENTINEL_FD) };
    assert!(
        (SENTINEL_FD..100).contains(&new),
        "F_DUPFD must land in the collision-free band [{SENTINEL_FD},100); got {new}"
    );
    // SAFETY: `new` is a fresh owned fd from F_DUPFD.
    unsafe { OwnedFd::from_raw_fd(new) }
}

// ── Channel A: the HOST-SIDE /proc/<pid>/fd oracle ──────────────────────────────────

/// Scan `/proc/*/cmdline` for the EXEC'd target — the process whose command line
/// contains `marker` — polling until `deadline`. Returns its pid. `None` if it never
/// appears (so the caller can fail the test honestly rather than panic on a race).
fn host_find_child(marker: &str, deadline: Instant) -> Option<RawFd> {
    while Instant::now() < deadline {
        if let Some(pid) = scan_proc_cmdline(marker) {
            return Some(pid);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// One pass over `/proc/<pid>/cmdline`, returning the pid of the process whose command
/// line (NUL-separated argv) contains `marker`.
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
        let path = format!("/proc/{pid_str}/cmdline");
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let cmdline = String::from_utf8_lossy(&bytes);
        if cmdline.contains(marker) {
            return Some(pid);
        }
    }
    None
}

/// Read the child's OPEN fd numbers from the KERNEL (`/proc/<pid>/fd`), independent of
/// any workload claim. Returns the sorted fd numbers. `None` if the dir is unreadable
/// (the child already exited / a race) so the caller can retry within the deadline.
fn host_read_child_fds(pid: RawFd) -> Option<Vec<RawFd>> {
    let dir = std::fs::read_dir(format!("/proc/{pid}/fd")).ok()?;
    let mut fds: Vec<RawFd> = Vec::new();
    for entry in dir.flatten() {
        if let Some(name) = entry.file_name().to_str() {
            if let Ok(fd) = name.parse::<RawFd>() {
                fds.push(fd);
            }
        }
    }
    fds.sort_unstable();
    Some(fds)
}

// ── Launcher plan plumbing (the scrub is the REAL descriptor-table-driven lowering) ──

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

/// An exec-only launcher plan whose descriptor table declares ONLY the exe slot (so the
/// scrub's allowlist is exactly stdio + exe + the launcher's protocol fds — `FdPolicy::None`).
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
        },
    };
    LinuxLaunchPlanV1 { body }
}

/// `/bin/sh` as the exec'd target (it can list its own fds + try the sentinel write).
fn sh_authority() -> AuthorityFd {
    AuthorityFd {
        slot_index: SLOT_EXE,
        handle: OwnedFd::from(std::fs::File::open("/bin/sh").expect("open /bin/sh")),
    }
}

// ── THE GUARANTEE-HOLDS ORACLE (dual channel + no-leak sentinel) ─────────────────────

#[test]
fn child_inherits_only_the_declared_fds_no_sentinel_leak() {
    let marker = unique_marker();

    // NO-LEAK SETUP: the PARENT opens an inheritable (non-CLOEXEC) sentinel via a pipe
    // write end relocated high, so it genuinely survives the launcher's execve and reaches
    // the clone3 child. It is NOT declared in the descriptor table, so the scrub MUST
    // close it. Both `writer` and `sentinel` stay alive across the spawn.
    let (mut reader, writer) = std::io::pipe().expect("create pipe");
    let sentinel = place_inheritable_high(writer.as_raw_fd());
    let sentinel_fd = sentinel.as_raw_fd();

    // The workload: keep alive (so the host can read /proc while it runs), carry the unique
    // marker IN THE SCRIPT (so the host can find it via /proc/<pid>/cmdline — the `: MARKER`
    // no-op embeds it where the shell's `-c` argument is recorded), AND try to write a LEAK
    // marker to the sentinel fd — its OWN report (channel B). The trailing `true` keeps the
    // shell RESIDENT (without it, bash tail-call-execs `sleep` and loses the marker); the
    // `sleep` keeps it alive while the host reads /proc.
    let script = format!(
        ": {marker}; \
         if printf LEAK >&{sentinel_fd}; then printf WROTE; else printf SCRUBBED; fi; \
         sleep 3; true"
    );
    let argv = vec!["sh".to_string(), "-c".to_string(), script];
    let plan = exec_only_plan(argv);
    let launcher = test_launcher_path();
    let deadline = Instant::now() + Duration::from_millis(2500);

    // `Builder::spawn` (not `thread::spawn`, which panics on failure).
    let handle = std::thread::Builder::new()
        .name("fd-oracle-launcher".to_string())
        .spawn(move || {
            launch::run_launcher(&launcher, &plan, vec![sh_authority()])
                .expect("the launcher runs the fd-scrub workload to a verdict")
        })
        .expect("spawn the launcher driver thread");

    // ── CHANNEL A: host-side /proc/<child_pid>/fd (kernel state) ─────────────────────
    // Find the child, then read its open fds from the kernel while it sleeps.
    let mut host_fds: Option<Vec<RawFd>> = None;
    if let Some(pid) = host_find_child(&marker, deadline) {
        while Instant::now() < deadline {
            if let Some(fds) = host_read_child_fds(pid) {
                host_fds = Some(fds);
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    let obs = handle.join().expect("fd-oracle launcher thread joins");
    // Drop every host-side WRITE end so the pipe read sees EOF.
    drop(sentinel);
    drop(writer);
    let mut leaked = Vec::new();
    reader
        .read_to_end(&mut leaked)
        .expect("read the pipe read end");

    // Collect-and-assert (panic! is banned even in tests): gather every failure, assert once.
    let mut failures: Vec<String> = Vec::new();

    if !obs.exec_succeeded() {
        failures.push(format!(
            "the workload must reach ExecSucceeded; terminal={:?} notes={:?}",
            obs.terminal, obs.notes
        ));
    }

    // CHANNEL A: the host must have observed the child's kernel fd table while it was
    // alive; if not, that is itself a failure (no panic — collect-and-assert).
    match host_fds {
        None => failures.push(
            "CHANNEL A: the host must observe the child's /proc/<pid>/fd while it is alive"
                .to_string(),
        ),
        Some(host_fds) => {
            // The child's open fds are EXACTLY the declared allowlist — stdio (0,1,2) plus
            // the declared TargetExe slot fd (SLOT_EXE == 10), which the workload inherits
            // as its own image fd. EVERY other low fd is undeclared and MUST have been
            // scrubbed; in particular the SENTINEL fd (50) MUST be absent. No fd in the
            // collision-free band below the launcher's relocation base (< 100), other than
            // stdio + the declared exe slot, may survive.
            let declared = [0, 1, 2, SLOT_EXE];
            let undeclared: Vec<RawFd> = host_fds
                .iter()
                .copied()
                .filter(|fd| !declared.contains(fd) && *fd < 100)
                .collect();
            if !undeclared.is_empty() {
                failures.push(format!(
                    "CHANNEL A: the child's /proc/<pid>/fd must contain ONLY the declared \
                     allowlist; undeclared low fds survived: {undeclared:?} (full set {host_fds:?})"
                ));
            }
            if host_fds.contains(&sentinel_fd) {
                failures.push(format!(
                    "CHANNEL A (no-leak): the undeclared sentinel fd {sentinel_fd} was NOT \
                     scrubbed — it survived into the child: {host_fds:?}"
                ));
            }
            // Stdio must still be present (the workload needs its inherited stdio).
            for std_fd in [0, 1, 2] {
                if !host_fds.contains(&std_fd) {
                    failures.push(format!(
                        "CHANNEL A: the declared stdio fd {std_fd} must survive the scrub: \
                         {host_fds:?}"
                    ));
                }
            }
        }
    }

    // CHANNEL B: the workload's OWN report — its write to the sentinel fd failed.
    let out = String::from_utf8_lossy(&obs.captured_stdout);
    if !out.contains("SCRUBBED") || out.contains("WROTE") {
        failures.push(format!(
            "CHANNEL B: the workload must report the sentinel fd was SCRUBBED; got stdout={out:?}"
        ));
    }
    // NO LEAK (independent, host-side): nothing the workload wrote crossed the pipe.
    if !leaked.is_empty() {
        failures.push(format!(
            "no-leak: the sentinel fd LEAKED across the boundary: host read {leaked:?} from the pipe"
        ));
    }

    assert!(
        failures.is_empty(),
        "fd-scrub oracle failures: {failures:#?}"
    );
}

// ── The full-execute()-path witness + the contract-level fail-closed branch ──────────

/// A spec whose ONLY capability is `InheritedFds { policy }`, plus launch + capture. The
/// LinuxBackend admits the `None` policy (InheritedFdsNone is Enforced in the ceiling).
fn fds_spec(policy: FdPolicy) -> BoundarySpec {
    BoundarySpec {
        workload: Workload::Process {
            exe: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "exit 0".to_string()],
        },
        capabilities: vec![
            Capability::InheritedFds { policy },
            // An empty explicit env so the child gets a clean, declared environment.
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

/// Run a spec through the LinuxBackend `execute()` contract path, returning the sealed
/// durable report body. `None` from `plan()` ⇒ admission refused (the caller asserts that).
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
fn a_none_policy_spec_runs_through_the_execute_path() {
    // The FULL execute()/BoundaryRunner contract-path witness: a None-policy spec admits
    // (InheritedFdsNone is Enforced) and runs to a clean verdict, with the lowering fact
    // recorded — the lowering rides the production contract, not only a launcher-direct plan.
    let report = run_execute(&fds_spec(FdPolicy::None))
        .expect("an InheritedFds::None spec must ADMIT (the cell is Enforced)");

    let mut failures: Vec<String> = Vec::new();
    if report.outcome != Outcome::Completed {
        failures.push(format!(
            "the None-policy workload must run to Completed: {:?} / {:?}",
            report.outcome, report.observed
        ));
    }
    // The lowering recorded its fact on the production execute() path.
    if !report
        .observed
        .iter()
        .any(|f| f.kind == "inherited_fds_lowered")
    {
        failures.push(format!(
            "the execute() path must record the fd lowering: {:?}",
            report.observed
        ));
    }
    assert!(
        failures.is_empty(),
        "execute()-path witness failures: {failures:#?}"
    );
}

#[test]
fn an_unrealized_fd_policy_fails_closed_and_the_target_never_runs() {
    // CONTRACT-LEVEL FAIL-CLOSED: `FdPolicy::Only` is NOT realized by this backend (the
    // scrub realizes only `None`). It is absent from the ceiling, so it must REFUSE before
    // execution — the target NEVER runs. This proves the fail-closed branch on the full
    // contract path (admission), not only a launcher-direct mechanism.
    let report = run_execute(&fds_spec(FdPolicy::Only(vec![7])));
    assert!(
        report.is_none(),
        "an InheritedFds::Only spec must FAIL CLOSED at admission (the cell is Unsupported) — \
         the target never runs; got a sealed report {report:?}"
    );
}
