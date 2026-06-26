// THE §4 CONTRACT ORACLE for `NetworkDenyAll` (proof-spine S9 / D3) — dual-channel +
// fail-closed. Proves the COMPLETE path spec → admission → lowering → execution →
// INDEPENDENT observation, INCLUDING the fail-closed branches, so the production ceiling
// may advertise NetworkDenyAll=Enforced and the S1 coupling gate couples it. Built ON the
// S8 userns rendezvous (unprivileged CLONE_NEWNET requires the child to be root-in-userns).
//
// Compiles only with the real Linux backend + the dangerous-test-hooks harness (real
// clone3 + fexecve through the launcher bin), on Linux.
#![cfg(all(
    feature = "backend-linux",
    feature = "dangerous-test-hooks",
    target_os = "linux"
))]
//! THE BACKEND NEVER GRADES ITSELF. Two independent channels witness the child's network
//! isolation:
//!   (A) HOST-SIDE, KERNEL-STATE (the STRONGEST oracle, per §4): the host finds the child
//!       and reads the CHILD's netns interface list from `/proc/<child_pid>/net/dev` — the
//!       kernel's own per-netns interface table — and asserts it contains ONLY `lo`
//!       (loopback), NO external interface (eth0/etc.). This is the independent "the netns
//!       has ZERO external interfaces" witness; the launcher cannot forge it.
//!   (B) WORKLOAD SELF-REPORT: the workload enumerates its OWN interfaces (its
//!       `/proc/self/net/dev`) AND checks it has NO route (its `/proc/self/net/route` is
//!       empty — no default route, so no externally-routable destination is reachable), and
//!       reports it CANNOT reach the network — captured through the launcher's piped stdout.
//!
//! THE D3 "NETWORK" DEFINITION ENCODED: NetworkDenyAll = an ISOLATED, EMPTY netns. No
//! external interface ⇒ no externally-routable socket op can succeed; the S5 fd-scrub already
//! closed every undeclared inherited fd (incl. any inherited socket) ⇒ no inherited routable
//! socket reaches the workload; loopback `lo` exists (kernel-reported `IFF_UP`) but has NO
//! address + NO routes ⇒ unreachable (`127.0.0.1` included) unless separately admitted — the
//! oracle witnesses this as only-`lo` + `route_count=0`. HOSTCONTROL CARVE-OUT: the launcher's own declared control channels
//! (the protocol socket / error-pipe / sync-pipe fds it fd-PASSES to the child) are
//! HostControl, NOT workload network authority — netns isolation does not affect fd-passed
//! sockets, so the launcher protocol STILL runs the workload to a verdict (proven by the
//! workload running to ExecSucceeded INSIDE the empty netns).
//!
//! FAIL-CLOSED: (i) a kernel without unprivileged userns+netns ⇒ the cell SKIPs LOUD (never
//! a silent pass — mirrors the landlock-ABI-floor SKIP); (ii) an unrealized `AllowList`
//! policy ⇒ admission REFUSES before any execution (the full execute() path).

use bvisor::linux::launch::{self, unprivileged_userns_available, AuthorityFd};
use bvisor::linux::protocol::{
    DescriptorKind, DescriptorRole, DescriptorShape, DescriptorSlotV1, LinuxLaunchBodyV1,
    LinuxLaunchPlanV1, LoweringWireEntryV1, LoweringWireV1, NetworkNsRequest, TargetSpecV1,
    UserNsRequest,
};
use bvisor::{
    AdmissionProgramHash, AttemptId, Backend, BackendId, BackendProfileHash, BackendRegistry,
    BoundaryPlanHash, BoundaryPlanner, BoundaryReportBody, BoundarySpec, BudgetRequirements,
    Capability, EnvPolicy, EvidenceRequirements, HostControl, LinuxBackend, MinGuarantee, NetDest,
    NetPolicy, Outcome, StdStreams, Workload,
};
use std::io::Write;
use std::os::fd::{OwnedFd, RawFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

// Frozen ids/phase-codes the launcher serves (mirror the launcher's constants).
const ID_AMBIENT_SCRUB: &str = "linux.ambient.scrub.v1";
const ID_EXEC: &str = "linux.exec.v1";
const PHASE_CODE_SCRUB: u8 = 3;
const PHASE_CODE_EXEC: u8 = 5;
const EXE_SLOT: u32 = 10;

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
    format!("BVISOR-NET-MARKER-{pid}-{nanos}")
}

// ── Channel A: the HOST-SIDE /proc/<child_pid>/net/dev oracle ────────────────────────

/// Scan `/proc/*/cmdline` for the EXEC'd target — the process whose command line contains
/// `marker` — polling until `deadline`. Returns its pid. `None` if it never appears (so the
/// caller can fail the test honestly rather than panic on a race).
fn host_find_child(marker: &str, deadline: Instant) -> Option<RawFd> {
    while Instant::now() < deadline {
        if let Some(pid) = scan_proc_cmdline(marker) {
            return Some(pid);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// One pass over `/proc/<pid>/cmdline`, returning the pid of the process whose command line
/// (NUL-separated argv) contains `marker`.
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
        if String::from_utf8_lossy(&bytes).contains(marker) {
            return Some(pid);
        }
    }
    None
}

/// Read the CHILD's netns interface names from the KERNEL (`/proc/<pid>/net/dev` — the
/// kernel's own per-netns interface table), independent of any workload claim. Each data
/// line after the two header lines is `<iface>: <stats...>`; the name is the token before
/// the first `:`. Returns the sorted interface names. `None` if the file is unreadable (the
/// child already exited / a race) so the caller can retry within the deadline.
fn host_read_child_interfaces(pid: RawFd) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/net/dev")).ok()?;
    let mut ifaces: Vec<String> = Vec::new();
    // The first two lines are column headers; data lines look like "  lo: 0 0 ...".
    for line in text.lines().skip(2) {
        if let Some((name, _rest)) = line.split_once(':') {
            let name = name.trim();
            if !name.is_empty() {
                ifaces.push(name.to_owned());
            }
        }
    }
    ifaces.sort();
    Some(ifaces)
}

// ── Launcher plan plumbing (the empty netns is the REAL S9 lowering) ──────────────────

fn entry(id: &str, phase_code: u8) -> LoweringWireEntryV1 {
    LoweringWireEntryV1 {
        id: id.to_owned(),
        version: 1,
        phase_code,
        param_digest: [0u8; 32],
        decl_digest: [0u8; 32],
    }
}

/// A scrub+exec plan running `argv` via `/bin/sh -c`. `deny_network` engages BOTH the userns
/// rendezvous AND the empty netns (S9 requires the S8 userns); `false` is the unchanged path.
fn plan(argv: Vec<String>, deny_network: bool) -> LinuxLaunchPlanV1 {
    let lowering = LoweringWireV1 {
        entries: vec![
            entry(ID_AMBIENT_SCRUB, PHASE_CODE_SCRUB),
            entry(ID_EXEC, PHASE_CODE_EXEC),
        ],
    };
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
            attempt_id: AttemptId([7u8; 32]),
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
                // S9 / D3: the empty netns requires the S8 userns rendezvous, so deny_network
                // engages BOTH together (off ⇒ both None ⇒ the no-netns path is unchanged).
                user_namespace: deny_network.then(UserNsRequest::new),
                network_namespace: deny_network.then(NetworkNsRequest::new),
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

// ── THE HOST-SIDE GUARANTEE-HOLDS ORACLE (channel A: zero external interfaces) ────────

#[test]
fn host_sees_only_loopback_in_the_child_netns_no_external_interface_or_skip() {
    let mut sink = std::io::stderr();
    if !unprivileged_userns_available() {
        let _ = writeln!(
            sink,
            "SKIP NetworkDenyAll host-side oracle: this host forbids unprivileged user+network \
             namespaces (sysctl) — the empty-netns cell is FAIL_CLOSED here, never a silent pass"
        );
        return;
    }

    let marker = unique_marker();
    // The workload: carry the unique marker IN THE SCRIPT (so the host finds it via
    // /proc/<pid>/cmdline), then stay RESIDENT (`sleep`) so the host can read the child's
    // /proc/<pid>/net/dev while it is alive. The trailing `true` keeps the shell resident.
    let script = format!(": {marker}; sleep 3; true");
    let argv = vec!["sh".to_string(), "-c".to_string(), script];
    let launcher = test_launcher_path();
    let p = plan(argv, true);
    let deadline = Instant::now() + Duration::from_millis(2500);

    let handle = std::thread::Builder::new()
        .name("net-oracle-launcher".to_string())
        .spawn(move || {
            launch::run_launcher(&launcher, &p, vec![sh_authority()])
                .expect("the launcher runs the empty-netns workload to a verdict")
        })
        .expect("spawn the launcher driver thread");

    // CHANNEL A: find the child, then read its netns interface list from the kernel.
    let mut host_ifaces: Option<Vec<String>> = None;
    if let Some(pid) = host_find_child(&marker, deadline) {
        while Instant::now() < deadline {
            if let Some(ifaces) = host_read_child_interfaces(pid) {
                host_ifaces = Some(ifaces);
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    let obs = handle.join().expect("net-oracle launcher thread joins");
    let _ = writeln!(
        sink,
        "NetworkDenyAll host-side: child interfaces={host_ifaces:?}; transcript={:?} notes={:?}",
        obs.transcript, obs.notes
    );

    // Collect-and-assert (panic! banned even in tests): gather every failure, assert once.
    let mut failures: Vec<String> = Vec::new();

    // HOSTCONTROL CARVE-OUT: the launcher's own control channel still works through the empty
    // netns (fd-passed sockets are unaffected), so the workload RAN to a verdict inside it.
    if !obs.exec_succeeded() {
        failures.push(format!(
            "the workload must run to ExecSucceeded INSIDE the empty netns (HostControl carve-out: \
             the launcher's fd-passed control channel is unaffected by netns); terminal={:?} \
             notes={:?}",
            obs.terminal, obs.notes
        ));
    }
    // The launcher attested the empty-netns mechanism on its honest transcript.
    if !obs.notes.iter().any(|n| n.contains("empty_netns")) {
        failures.push(format!(
            "the launcher must attest the empty_netns mechanism; notes={:?}",
            obs.notes
        ));
    }

    match host_ifaces {
        None => failures.push(
            "CHANNEL A: the host must observe the child's /proc/<pid>/net/dev while it is alive"
                .to_string(),
        ),
        Some(ifaces) => {
            // THE INDEPENDENT WITNESS: the child's netns has ONLY `lo` — NO external interface.
            // An empty netns the kernel creates contains exactly one interface, `lo` (reported
            // IFF_UP but with no address + no routes ⇒ unreachable; route_count=0 confirms it).
            if ifaces != vec!["lo".to_string()] {
                failures.push(format!(
                    "CHANNEL A: the child netns must contain ONLY loopback `lo` (zero external \
                     interfaces); got {ifaces:?}"
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "NetworkDenyAll host-side oracle failures: {failures:#?}"
    );
}

// ── THE WORKLOAD-SELF-REPORT ORACLE (channel B: cannot reach the network) ─────────────

#[test]
fn workload_cannot_reach_the_network_from_the_empty_netns_or_skip() {
    let mut sink = std::io::stderr();
    if !unprivileged_userns_available() {
        let _ = writeln!(
            sink,
            "SKIP NetworkDenyAll workload self-report: this host forbids unprivileged user+network \
             namespaces — FAIL_CLOSED, never a silent pass"
        );
        return;
    }

    // The workload enumerates its OWN netns interfaces (count the data lines of
    // /proc/self/net/dev) and checks it has NO route (the data lines of /proc/self/net/route
    // beyond the header) — in an empty netns there is exactly ONE interface (`lo`) and ZERO
    // routes, so the workload reports it CANNOT reach the network. Each value is prefixed so
    // the host can parse it from the launcher-captured stdout regardless of formatting.
    let script = "ifaces=$(awk 'NR>2 && NF {print $1}' /proc/self/net/dev | wc -l); \
         printf 'iface_count=%s\\n' \"$ifaces\"; \
         printf 'iface_names=%s\\n' \"$(awk 'NR>2 && NF {sub(/:.*/,\"\",$1); print $1}' /proc/self/net/dev | sort | tr '\\n' ',')\"; \
         routes=$(awk 'NR>1 && NF {print}' /proc/self/net/route | wc -l); \
         printf 'route_count=%s\\n' \"$routes\"; \
         if [ \"$routes\" -eq 0 ]; then printf 'network=UNREACHABLE\\n'; else printf 'network=REACHABLE\\n'; fi";
    let argv = vec!["sh".to_string(), "-c".to_string(), script.to_string()];
    let launcher = test_launcher_path();
    let obs = launch::run_launcher(&launcher, &plan(argv, true), vec![sh_authority()])
        .expect("the launcher runs the empty-netns self-report workload to a verdict");
    let stdout = String::from_utf8_lossy(&obs.captured_stdout).into_owned();
    let _ = writeln!(
        sink,
        "NetworkDenyAll workload self-report stdout:\n{stdout}\ntranscript={:?}",
        obs.transcript
    );

    let mut failures: Vec<String> = Vec::new();

    if !obs.exec_succeeded() {
        failures.push(format!(
            "the self-report workload must run to ExecSucceeded; terminal={:?}",
            obs.terminal
        ));
    }

    let field = |key: &str| -> Option<String> {
        stdout.lines().find_map(|l| {
            l.strip_prefix(&format!("{key}="))
                .map(|r| r.trim().to_owned())
        })
    };

    // Exactly ONE interface, named `lo` — the workload sees ONLY loopback.
    match field("iface_count") {
        Some(c) if c == "1" => {}
        other => failures.push(format!(
            "workload must see exactly 1 interface (lo) in its empty netns, got iface_count={other:?}"
        )),
    }
    match field("iface_names") {
        // The awk trims `lo:` to `lo`; tr appends a trailing comma.
        Some(n) if n == "lo," => {}
        other => failures.push(format!(
            "workload's only interface must be `lo`, got iface_names={other:?}"
        )),
    }
    // ZERO routes — no default route, so no externally-routable destination is reachable.
    match field("route_count") {
        Some(c) if c == "0" => {}
        other => failures.push(format!(
            "workload must have ZERO routes in its empty netns (cannot route externally), got \
             route_count={other:?}"
        )),
    }
    match field("network") {
        Some(v) if v == "UNREACHABLE" => {}
        other => failures.push(format!(
            "workload must report network=UNREACHABLE from the empty netns, got {other:?}"
        )),
    }

    assert!(
        failures.is_empty(),
        "NetworkDenyAll workload-self-report failures: {failures:#?}"
    );
}

// ── The full-execute()-path witness + the contract-level fail-closed branch ───────────

/// A spec whose ONLY capability is `Network { policy }`, plus launch + capture + an empty
/// explicit env. The LinuxBackend admits `DenyAll` ONLY when its ceiling backs
/// NetworkDenyAll=Enforced (the host permits unprivileged userns+netns).
fn net_spec(policy: NetPolicy) -> BoundarySpec {
    BoundarySpec {
        workload: Workload::Process {
            exe: "/bin/sh".to_string(),
            // Read the netns interface count as the workload's observable behavior.
            args: vec![
                "-c".to_string(),
                "awk 'NR>2 && NF {print $1}' /proc/self/net/dev | wc -l; exit 0".to_string(),
            ],
        },
        capabilities: vec![
            Capability::Network { policy },
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
fn a_deny_all_spec_runs_through_the_execute_path_or_skip() {
    let mut sink = std::io::stderr();
    // FAIL-CLOSED SKIP: without unprivileged userns+netns the cell is absent from the
    // production ceiling, so a DenyAll spec REFUSES at admission (never a silent pass). We
    // assert exactly that refusal here, then SKIP the positive path.
    if !unprivileged_userns_available() {
        let refused = run_execute(&net_spec(NetPolicy::DenyAll));
        assert!(
            refused.is_none(),
            "FAIL_CLOSED: with no unprivileged userns+netns, a NetworkDenyAll spec must REFUSE at \
             admission (the cell is Unsupported) — the target never runs; got {refused:?}"
        );
        let _ = writeln!(
            sink,
            "SKIP NetworkDenyAll execute-path positive: no unprivileged userns+netns; the \
             fail-closed admission refusal was asserted instead (never a silent pass)"
        );
        return;
    }

    // POSITIVE: a DenyAll spec ADMITS (the cell is Enforced) and runs to a clean verdict on
    // the FULL execute()/BoundaryRunner contract path, with the network lowering fact recorded
    // — the empty netns rides the production contract, not only a run_launcher-direct plan.
    let report = run_execute(&net_spec(NetPolicy::DenyAll))
        .expect("a NetworkDenyAll spec must ADMIT (the cell is Enforced on this host)");

    let mut failures: Vec<String> = Vec::new();
    if report.outcome != Outcome::Completed {
        failures.push(format!(
            "the DenyAll workload must run to Completed inside the empty netns: {:?} / {:?}",
            report.outcome, report.observed
        ));
    }
    if !report.observed.iter().any(|f| f.kind == "network_lowered") {
        failures.push(format!(
            "the execute() path must record the network lowering: {:?}",
            report.observed
        ));
    }
    // The workload ran INSIDE the empty netns and the host captured its stdout cleanly (the
    // byte-count fact backs CaptureStreams). The /proc/<pid>/net/dev CONTENT witness is the
    // dedicated host-side + self-report oracles above; here we prove the lowering rides the
    // production execute()/BoundaryRunner contract (admission + Completed + the fact), not
    // only a run_launcher-direct plan.
    if !report.observed.iter().any(|f| f.kind == "stream_captured") {
        failures.push(format!(
            "the execute() path must capture the workload's streams: {:?}",
            report.observed
        ));
    }

    assert!(
        failures.is_empty(),
        "NetworkDenyAll execute()-path witness failures: {failures:#?}"
    );
}

#[test]
fn network_allow_list_fails_closed_at_admission_the_target_never_runs() {
    // CONTRACT-LEVEL FAIL-CLOSED: `NetPolicy::AllowList` is NOT realized by this backend (no
    // broker in v1; only DenyAll is lowered, via an empty netns). It is absent from the
    // ceiling, so it must REFUSE before execution — the target NEVER runs. This holds on EVERY
    // host (independent of userns support), proving the fail-closed branch on the full
    // contract path (admission), not only a launcher-direct mechanism.
    let report = run_execute(&net_spec(NetPolicy::AllowList(vec![NetDest {
        host: "example".to_string(),
        port: 443,
    }])));
    assert!(
        report.is_none(),
        "a NetworkAllowList spec must FAIL CLOSED at admission (the cell is Unsupported — no \
         broker in v1) — the target never runs; got a sealed report {report:?}"
    );
}
