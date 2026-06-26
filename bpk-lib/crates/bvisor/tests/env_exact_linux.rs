// THE §4 CONTRACT ORACLE for `Environment::Exact` (proof-spine S4) — dual-channel +
// fail-closed. Proves the COMPLETE path spec → admission → lowering → execution →
// INDEPENDENT observation, INCLUDING the fail-closed branches, so the production
// ceiling may advertise Environment=Enforced and the S1 coupling gate couples it.
//
// Compiles only with the real Linux backend + the dangerous-test-hooks harness
// (real clone3 + fexecve through the launcher bin), on Linux.
#![cfg(all(
    feature = "backend-linux",
    feature = "dangerous-test-hooks",
    target_os = "linux"
))]
//! THE BACKEND NEVER GRADES ITSELF. Two independent channels witness the child's
//! environment:
//!   (A) HOST-SIDE, KERNEL-STATE: the host scans `/proc/<pid>/environ` for a unique
//!       admitted marker and reads the child's ACTUAL environment from the kernel —
//!       never a workload claim. This is the strongest oracle (genuinely independent).
//!   (B) WORKLOAD SELF-REPORT: the workload's own `env` output, captured through the
//!       launcher's piped stdout.
//! Both must agree the child env EQUALS the admitted table EXACTLY. A SENTINEL var set
//! in the PARENT process env must be ABSENT in the child (no ambient leak).
//!
//! The lowering under test is the REAL contract `lower_env` (spec EnvPolicy::Exact +
//! a host SecretResolver → the concrete envp). A SecretLease resolves to its value IN
//! THE CHILD, but the DURABLE plan + report carry only the lease REF, never the value
//! (asserted by serializing them).
//!
//! FAIL-CLOSED: (i) a lease whose resolver Errs ⇒ the target NEVER runs (no child
//! output, Outcome != Completed); (ii) a contract-invalid policy (duplicate name) ⇒
//! admission REFUSES before any execution.

use bvisor::linux::launch::{self, AuthorityFd, LaunchObservation};
use bvisor::linux::protocol::{
    DescriptorKind, DescriptorRole, DescriptorShape, DescriptorSlotV1, LinuxLaunchBodyV1,
    LinuxLaunchPlanV1, LoweringWireEntryV1, LoweringWireV1, TargetSpecV1,
};
use bvisor::{
    AdmissionProgramHash, AttemptId, Backend, BackendId, BackendProfileHash, BackendRegistry,
    BoundaryPlanHash, BoundaryPlanner, BoundaryReportBody, BoundarySpec, BudgetRequirements,
    Capability, EnvEntry, EnvPolicy, EvidenceRequirements, HostControl, LinuxBackend,
    MapSecretResolver, MinGuarantee, Outcome, PlanError, SecretRef, StdStreams, Workload,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

// Frozen ids/phase-codes the launcher serves (mirror the launcher's constants).
const ID_AMBIENT_SCRUB: &str = "linux.ambient.scrub.v1";
const ID_EXEC: &str = "linux.exec.v1";
const PHASE_CODE_SCRUB: u8 = 3;
const PHASE_CODE_EXEC: u8 = 5;
const SLOT_EXE: std::os::fd::RawFd = 10;

// THE NO-AMBIENT-LEAK SENTINELS: the four launcher-channel env vars that
// `run_launcher` sets on the LAUNCHER process (it `env_clear()`s the launcher to ONLY
// these). They are GUARANTEED present in the spawning launcher's environment, so their
// ABSENCE from the child env is the load-bearing proof that the child env is the
// EXPLICIT admitted table, not an inherited one. (`std::env::set_var` is banned —
// BANNED-003 thread-unsafe — so the sentinel rides the launcher's controlled env, which
// is a STRONGER witness than a test-process var anyway: the launcher's env is what the
// child would inherit if inheritance happened.)
const LEAK_SENTINELS: &[&str] = &[
    "BVISOR_LAUNCH_PLAN_FD",
    "BVISOR_CONTROL_FD",
    "BVISOR_ERROR_FD",
    "BVISOR_ERROR_READ_FD",
];

/// Whether any leak sentinel (or any `BVISOR_*` launcher plumbing) appears in `env`.
fn has_ambient_leak(env: &[String]) -> bool {
    env.iter().any(|line| {
        LEAK_SENTINELS.iter().any(|s| line.contains(s)) || line.contains("BVISOR_LAUNCH")
    })
}

fn test_launcher_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_bvisor-linux-launcher"))
}

/// A unique-per-run marker value so the host can find THIS run's child in `/proc`
/// without racing other processes. Combines pid + a monotonic nanos timestamp.
fn unique_marker() -> String {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("BVISOR-ENV-MARKER-{pid}-{nanos}")
}

// ── Channel A: the HOST-SIDE /proc/<pid>/environ oracle ────────────────────────────

/// Scan `/proc/*/environ` for the EXEC'd target — the process whose environment
/// contains `marker` AND has exactly `expected_len` entries — polling until `deadline`.
/// The exec'd `sh` has EXACTLY the admitted env; its child helpers (`cat`/`sleep`)
/// inherit it PLUS shell-exported `PWD`/`SHLVL`, so the entry-count pins the target
/// unambiguously. Returns that process's FULL environment as `name=value` lines, read
/// from the KERNEL (independent of any workload claim). `None` if it never appears.
fn host_read_child_environ(
    marker: &str,
    expected_len: usize,
    deadline: Instant,
) -> Option<Vec<String>> {
    while Instant::now() < deadline {
        if let Some(env) = scan_proc_for_marker(marker, expected_len) {
            return Some(env);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// One pass over `/proc/<pid>/environ`, returning the matching exec'd-target env.
fn scan_proc_for_marker(marker: &str, expected_len: usize) -> Option<Vec<String>> {
    let entries = std::fs::read_dir("/proc").ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str() else { continue };
        if !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let path = format!("/proc/{pid}/environ");
        // The environ is NUL-separated `name=value` records (kernel-recorded).
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let env: Vec<String> = bytes
            .split(|&b| b == 0)
            .filter(|r| !r.is_empty())
            .map(|r| String::from_utf8_lossy(r).into_owned())
            .collect();
        if env.len() == expected_len && env.iter().any(|line| line.contains(marker)) {
            return Some(env);
        }
    }
    None
}

// ── Launcher plan plumbing (the env is the REAL lowered envp) ───────────────────────

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

/// An exec-only launcher plan whose target env is exactly `envp` (the lowered table).
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
            network_namespace: None,
        },
    };
    LinuxLaunchPlanV1 { body }
}

/// Resolve a coreutil to whichever of `/usr/bin` or `/bin` holds it on this host, as
/// the exec'd authority handle at the target slot.
fn bin_authority(name: &str) -> AuthorityFd {
    let usr = format!("/usr/bin/{name}");
    let bin = format!("/bin/{name}");
    let path = if std::path::Path::new(&usr).is_file() {
        usr
    } else {
        bin
    };
    AuthorityFd {
        slot_index: SLOT_EXE,
        handle: std::os::fd::OwnedFd::from(
            std::fs::File::open(&path).expect("open the exec target coreutil"),
        ),
    }
}

/// `sleep` as the channel-A exec'd target (keeps the child alive for /proc reads).
fn sleep_authority() -> AuthorityFd {
    bin_authority("sleep")
}

/// `env` as the channel-B exec'd target (prints its inherited environment).
fn env_authority() -> AuthorityFd {
    bin_authority("env")
}

/// Parse the workload's self-reported environment (channel B): the EXEC'd target is
/// `/usr/bin/env` DIRECTLY (no shell), so its stdout is exactly its inherited
/// environment as `name=value` lines — `env` adds nothing to its own environment, and
/// without an intervening shell there is no PWD/SHLVL/_ synthesis. So channel B is the
/// workload's self-report of exactly the EXEC'd environment.
fn workload_reported_env(obs: &LaunchObservation) -> Vec<String> {
    String::from_utf8_lossy(&obs.captured_stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn child_env_equals_the_admitted_table_with_no_ambient_leak() {
    let marker = unique_marker();
    // The admitted Environment::Exact table: an explicit PATH literal, the unique
    // marker literal (so the host can find the child in /proc), and a SecretLease that
    // must resolve to its value IN THE CHILD.
    let secret_value = "RESOLVED-SECRET-IN-CHILD";
    let policy = EnvPolicy::Exact(vec![
        EnvEntry::literal("PATH", "/usr/bin:/bin"),
        EnvEntry::literal("BVISOR_ENV_MARKER", &marker),
        EnvEntry::lease("CHILD_TOKEN", SecretRef::new("lease://child/token")),
    ]);
    // SPEC → ADMISSION GATE: the table is contract-valid.
    assert_eq!(
        policy.validate(),
        Ok(()),
        "the admitted table must be valid"
    );

    // LOWERING: the REAL contract lower_env resolves literals + the lease in the parent.
    let resolver = MapSecretResolver::new().with("lease://child/token", secret_value);
    let envp = bvisor::lower_env(&policy, &resolver).expect("the policy lowers cleanly");

    // The admitted table, as the `name=value` lines the child env must EQUAL exactly.
    let mut expected: Vec<String> = envp
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    expected.sort();

    // ── CHANNEL A: host-side /proc/<pid>/environ ────────────────────────────────────
    // EXEC the target DIRECTLY as `/bin/sleep <secs>` (no shell) so its kernel-recorded
    // environ is EXACTLY the admitted table — a shell would synthesize PWD/SHLVL/_ and a
    // child helper would inherit them. The sleep keeps the child ALIVE so the host can
    // read its /proc environ from the kernel while it runs.
    let sleep_argv = vec!["sleep".to_string(), "3".to_string()];
    let sleep_plan = exec_only_plan(sleep_argv, envp.clone());
    let launcher = test_launcher_path();
    let deadline = Instant::now() + Duration::from_millis(2500);
    // `Builder::spawn` (not `thread::spawn`, which is banned — it panics on failure).
    let handle = std::thread::Builder::new()
        .name("env-oracle-launcher".to_string())
        .spawn(move || {
            launch::run_launcher(&launcher, &sleep_plan, vec![sleep_authority()])
                .expect("the launcher runs the sleep workload to a verdict")
        })
        .expect("spawn the launcher driver thread");
    let host_env = host_read_child_environ(&marker, expected.len(), deadline);
    let sleep_obs = handle.join().expect("sleep launcher thread joins");
    assert!(
        sleep_obs.exec_succeeded(),
        "the sleep workload must reach ExecSucceeded; terminal={:?} notes={:?}",
        sleep_obs.terminal,
        sleep_obs.notes
    );
    let mut host_env = host_env.expect(
        "CHANNEL A: the host must observe the child's /proc/<pid>/environ while it is alive",
    );
    host_env.sort();
    assert_eq!(
        host_env, expected,
        "CHANNEL A: the child's /proc environ must EQUAL the admitted table exactly"
    );
    // NO AMBIENT LEAK (channel A): no launcher-env sentinel reached the child — proven
    // host-side from the kernel-recorded environ.
    assert!(
        !has_ambient_leak(&host_env),
        "CHANNEL A: a launcher-env sentinel leaked into the child env: {host_env:?}"
    );
    // The secret resolved to its VALUE in the child (channel A sees the value).
    assert!(
        host_env
            .iter()
            .any(|l| l == &format!("CHILD_TOKEN={secret_value}")),
        "CHANNEL A: the secret lease must resolve to its value in the child: {host_env:?}"
    );

    // ── CHANNEL B: the workload's own self-report ───────────────────────────────────
    // EXEC `/usr/bin/env` DIRECTLY (no shell) so its stdout is exactly its inherited
    // (admitted) environment — env adds nothing to its own environment.
    let env_argv = vec!["env".to_string()];
    let env_plan = exec_only_plan(env_argv, envp);
    let env_obs = launch::run_launcher(&test_launcher_path(), &env_plan, vec![env_authority()])
        .expect("the launcher runs the env workload to a verdict");
    assert!(
        env_obs.exec_succeeded(),
        "the env workload must reach ExecSucceeded; terminal={:?} notes={:?}",
        env_obs.terminal,
        env_obs.notes
    );
    let mut reported = workload_reported_env(&env_obs);
    reported.sort();
    assert_eq!(
        reported, expected,
        "CHANNEL B: the workload's reported env must EQUAL the admitted table exactly"
    );
    assert!(
        !has_ambient_leak(&reported),
        "CHANNEL B: a launcher-env sentinel leaked into the workload's reported env: {reported:?}"
    );
}

// ── The durable no-leak proof (through the full execute() contract path) ────────────

/// A spec whose ONLY capability is an `Environment::Exact` table, plus launch +
/// capture. The LinuxBackend admits it (Environment is Enforced in the ceiling).
fn env_spec(policy: EnvPolicy) -> BoundarySpec {
    BoundarySpec {
        workload: Workload::Process {
            exe: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "env".to_string()],
        },
        capabilities: vec![Capability::Environment { policy }],
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

/// Run a spec through the LinuxBackend `execute()` contract path with a chosen
/// resolver, returning the sealed durable report body + the durable plan (both serde).
fn run_execute(spec: &BoundarySpec, resolver: MapSecretResolver) -> (BoundaryReportBody, Vec<u8>) {
    let backend = Arc::new(
        LinuxBackend::with_launcher_path(test_launcher_path())
            .with_secret_resolver(Arc::new(resolver)),
    );
    let id: BackendId = backend.id();
    let mut registry = BackendRegistry::new();
    registry.register(Arc::clone(&backend) as Arc<dyn Backend>);

    let plan = BoundaryPlanner::new(&registry)
        .plan(spec, &id)
        .expect("the LinuxBackend admits an Environment::Exact spec");
    // The DURABLE plan carries the EnvPolicy with the lease REF, never the value.
    let plan_bytes = batpak::canonical::to_bytes(&plan).expect("encode the durable plan");
    let report = bvisor::BoundaryRunner::new(&registry)
        .run(&plan)
        .expect("the run seals a terminal report")
        .body;
    (report, plan_bytes)
}

#[test]
fn a_secret_lease_resolves_but_the_durable_plan_and_report_carry_only_the_ref() {
    let secret_value = "DURABLE-MUST-NOT-CONTAIN-THIS-SECRET";
    let lease_ref = "lease://durable/token";
    let policy = EnvPolicy::Exact(vec![
        EnvEntry::literal("PATH", "/usr/bin:/bin"),
        EnvEntry::lease("DB_TOKEN", SecretRef::new(lease_ref)),
    ]);
    let resolver = MapSecretResolver::new().with(lease_ref, secret_value);
    let (report, plan_bytes) = run_execute(&env_spec(policy), resolver);

    // The run COMPLETED (the secret resolved in the child, so the workload ran).
    assert_eq!(
        report.outcome,
        Outcome::Completed,
        "the lease resolved ⇒ the workload runs: {:?}",
        report.observed
    );

    // The DURABLE plan carries the lease REF, never the resolved value.
    let plan_text = String::from_utf8_lossy(&plan_bytes);
    assert!(
        plan_text.contains(lease_ref),
        "the durable plan must carry the lease REF"
    );
    assert!(
        !plan_text.contains(secret_value),
        "the durable plan must NOT carry the resolved secret value"
    );

    // The DURABLE report likewise carries only the ref (its admitted requirements hold
    // the EnvPolicy with the lease ref; no observed fact carries the value).
    let report_bytes = batpak::canonical::to_bytes(&report).expect("encode the durable report");
    let report_text = String::from_utf8_lossy(&report_bytes);
    assert!(
        !report_text.contains(secret_value),
        "the durable report must NOT carry the resolved secret value"
    );
    assert!(
        report_text.contains(lease_ref),
        "the durable report must carry the lease REF (the policy identity)"
    );
}

// ── FAIL-CLOSED branch 1: an unresolvable lease ⇒ the target NEVER runs ──────────────

#[test]
fn an_unresolvable_lease_fails_closed_and_the_target_never_runs() {
    // A lease the resolver cannot satisfy (empty resolver ⇒ Unknown).
    let policy = EnvPolicy::Exact(vec![EnvEntry::lease(
        "MISSING_TOKEN",
        SecretRef::new("lease://does-not-exist"),
    )]);
    let (report, _plan_bytes) = run_execute(&env_spec(policy), MapSecretResolver::new());

    // FAIL-CLOSED: lowering refused in the parent, so the workload NEVER ran.
    assert_ne!(
        report.outcome,
        Outcome::Completed,
        "an unresolvable lease must NOT complete the workload: {:?}",
        report.observed
    );
    assert!(
        report
            .observed
            .iter()
            .any(|f| f.kind == "environment_lowering_failed"),
        "the report must record the fail-closed lowering refusal: {:?}",
        report.observed
    );
    // The target never executed: no workload-launched fact was recorded.
    assert!(
        !report
            .observed
            .iter()
            .any(|f| f.kind == "workload_launched"),
        "the target must NEVER run when a lease is unresolvable: {:?}",
        report.observed
    );
}

// ── FAIL-CLOSED branch 2: a contract-invalid policy ⇒ admission refuses ──────────────

#[test]
fn a_contract_invalid_policy_is_refused_before_execution() {
    // A duplicate name is contract-invalid: admission must REFUSE before any execution.
    let policy = EnvPolicy::Exact(vec![
        EnvEntry::literal("DUP", "a"),
        EnvEntry::literal("DUP", "b"),
    ]);
    let backend = Arc::new(LinuxBackend::with_launcher_path(test_launcher_path()));
    let id: BackendId = backend.id();
    let mut registry = BackendRegistry::new();
    registry.register(Arc::clone(&backend) as Arc<dyn Backend>);

    let result = BoundaryPlanner::new(&registry).plan(&env_spec(policy), &id);
    assert!(
        matches!(result, Err(PlanError::InvalidPolicy { .. })),
        "a contract-invalid Environment policy must be REFUSED at admission, got {result:?}"
    );
}
