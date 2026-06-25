// REAL unprivileged USER-NAMESPACE RENDEZVOUS proof for the single-threaded Linux
// confinement launcher (proof-spine S8). Real-OS: spawns `bvisor-linux-launcher` and a
// real workload that reports its OWN uid/gid + namespace maps, so it is gated to Linux +
// backend-linux. S8 is INFRASTRUCTURE (the prerequisite for unprivileged netns creation,
// S9) — it mints NO new PROVEN ledger row; its teeth are this independent observation.
#![cfg(all(target_os = "linux", feature = "backend-linux"))]
//! THE LAUNCHER NEVER GRADES ITSELF. The witness is the KERNEL's own state, captured
//! INDEPENDENTLY of the launcher's transcript: the workload `cat`s its `/proc/self/
//! uid_map`, `/proc/self/gid_map`, `/proc/self/setgroups`, and prints `id -u`/`id -g`.
//! The kernel WROTE those maps (the parent could only request them); the workload merely
//! reads them back, so a launcher that lied in its transcript cannot fake them.
//!
//! RENDEZVOUS (the happy path): a plan with `user_namespace = Some(..)` engages the
//! rendezvous. The child is born in a NEW userns, BLOCKS, the parent writes
//! `uid_map = "0 <euid> 1"` / `setgroups = deny` / `gid_map = "0 <egid> 1"`, then releases
//! it. The workload — now uid 0 INSIDE the userns — reports uid 0/gid 0 AND the maps read
//! back EXACTLY `0 <euid> 1` / `0 <egid> 1`, with `setgroups = deny`.
//!
//! OFF-PATH (non-vacuous control): the SAME workload with NO userns request runs in the
//! launcher's OWN userns — its `id -u` is the launcher's real uid (NOT 0) and its
//! `/proc/self/uid_map` is the host's identity map (NOT `0 <euid> 1`). This proves the
//! test distinguishes "mapped into a new userns" from "shared the parent's", AND that the
//! opt-in did not disturb the no-userns path (the launcher still execs to success).
//!
//! FAIL-CLOSED: a plan that requests a userns but whose map-write the launcher cannot
//! complete (we force this by pointing the rendezvous at a kernel that refuses the map —
//! a userns that is ALREADY mapped cannot be remapped, so a SECOND CLONE_NEWUSER nested
//! by the workload is not how we trigger it; instead we engage on a host WITHOUT
//! unprivileged userns support, where clone3(CLONE_NEWUSER) itself fails ⇒ no child runs).
//! The launcher fails closed: the target NEVER runs (no ExecSucceeded).
//!
//! SKIP: if the host forbids unprivileged user namespaces (sysctl), the rendezvous
//! assertions SKIP with an explicit message — never a silent pass (mirrors the
//! landlock-ABI-floor SKIP).

use bvisor::linux::launch::{resolve_launcher_path, unprivileged_userns_available, AuthorityFd};
use bvisor::linux::protocol::{
    DescriptorKind, DescriptorRole, DescriptorShape, DescriptorSlotV1, LinuxLaunchBodyV1,
    LinuxLaunchPlanV1, LoweringWireEntryV1, LoweringWireV1, TargetSpecV1, UserNsRequest,
};
use bvisor::{AdmissionProgramHash, AttemptId, BackendProfileHash, BoundaryPlanHash};
use std::io::Write;
use std::os::fd::{OwnedFd, RawFd};

// Frozen ids/phase-codes the launcher serves (mirror its constants).
const ID_AMBIENT_SCRUB: &str = "linux.ambient.scrub.v1";
const ID_EXEC: &str = "linux.exec.v1";
const PHASE_CODE_SCRUB: u8 = 3; // LoweringPhase::FdHygiene.code()
const PHASE_CODE_EXEC: u8 = 5; // LoweringPhase::Launch.code()

// The exe rides this slot fd (well above stdio); the launcher reads it at the slot index.
const EXE_SLOT: u32 = 10;

// The workload: report the kernel's view of who-am-i + the userns maps to stdout. Each
// line is prefixed so the host can parse exactly one value regardless of `id`/`cat`
// formatting. `/bin/sh` is the exe (a real binary on every test host).
const WORKLOAD: &str = "printf 'uid=%s\\n' \"$(id -u)\"; \
     printf 'gid=%s\\n' \"$(id -g)\"; \
     printf 'uid_map=%s\\n' \"$(cat /proc/self/uid_map)\"; \
     printf 'gid_map=%s\\n' \"$(cat /proc/self/gid_map)\"; \
     printf 'setgroups=%s\\n' \"$(cat /proc/self/setgroups 2>/dev/null)\"";

fn entry(id: &str, phase_code: u8) -> LoweringWireEntryV1 {
    LoweringWireEntryV1 {
        id: id.to_owned(),
        version: 1,
        phase_code,
        param_digest: [0u8; 32],
        decl_digest: [0u8; 32],
    }
}

/// A scrub+exec plan running [`WORKLOAD`] via `/bin/sh -c`. `user_namespace` is the
/// opt-in: `Some` engages the rendezvous, `None` is the byte-for-byte-unchanged path.
fn plan(user_namespace: Option<UserNsRequest>) -> LinuxLaunchPlanV1 {
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
                argv: vec!["sh".to_owned(), "-c".to_owned(), WORKLOAD.to_owned()],
                envp: vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())],
                exe_slot: EXE_SLOT,
                user_namespace,
            },
        },
    }
}

/// Run [`plan`] through the harness with `/bin/sh` as the exe authority handle, with
/// `extra_env` forwarded into the launcher's (otherwise cleared) environment.
fn run_with_env(
    user_namespace: Option<UserNsRequest>,
    extra_env: &[(&str, String)],
) -> bvisor::linux::launch::LaunchObservation {
    let sh = OwnedFd::from(std::fs::File::open("/bin/sh").expect("open /bin/sh"));
    let authority = vec![AuthorityFd {
        slot_index: RawFd::try_from(EXE_SLOT).expect("exe slot fits RawFd"),
        handle: sh,
    }];
    let launcher = resolve_launcher_path(env!("CARGO_BIN_EXE_bvisor-linux-launcher"));
    bvisor::linux::launch::run_launcher_with_env(
        &launcher,
        &plan(user_namespace),
        authority,
        extra_env,
    )
    .expect("harness ran the launcher")
}

/// Run [`plan`] with no extra env (the happy / off-path launches).
fn run(user_namespace: Option<UserNsRequest>) -> bvisor::linux::launch::LaunchObservation {
    run_with_env(user_namespace, &[])
}

/// Parse a `key=value` line out of the workload's captured stdout.
fn field(stdout: &str, key: &str) -> Option<String> {
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix(&format!("{key}=")) {
            return Some(rest.trim().to_owned());
        }
    }
    None
}

/// The test process's EFFECTIVE uid/gid, read from `/proc/self/status` (SAFE — no
/// `libc`). The `Uid:`/`Gid:` lines are `real effective saved fs`; we take EFFECTIVE
/// (index 1). The launcher inherits this identity, so the userns maps to exactly it.
fn effective_ids() -> (u32, u32) {
    let status = std::fs::read_to_string("/proc/self/status").expect("read /proc/self/status");
    let read = |prefix: &str| -> u32 {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix(prefix) {
                let cols: Vec<&str> = rest.split_whitespace().collect();
                if let Some(eff) = cols.get(1) {
                    if let Ok(v) = eff.parse::<u32>() {
                        return v;
                    }
                }
            }
        }
        u32::MAX
    };
    (read("Uid:"), read("Gid:"))
}

/// Collapse a `/proc/self/<x>_map` line's runs of whitespace to single spaces so it can
/// be compared to the canonical `"0 <id> 1"` the launcher writes (the kernel pads the map
/// columns with variable spacing).
fn normalize_map(map: &str) -> String {
    map.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn rendezvous_maps_the_child_to_uid0_inside_a_new_userns_or_skip() {
    let mut sink = std::io::stderr();
    if !unprivileged_userns_available() {
        let _ = writeln!(
            sink,
            "SKIP userns rendezvous: this host forbids unprivileged user namespaces (sysctl)"
        );
        return;
    }

    let (euid, egid) = effective_ids();
    let obs = run(Some(UserNsRequest::new()));
    let stdout = String::from_utf8_lossy(&obs.captured_stdout).into_owned();
    let _ = writeln!(
        sink,
        "userns rendezvous: euid={euid} egid={egid}; workload stdout:\n{stdout}\ntranscript: {:?}",
        obs.transcript
    );

    // Collect-and-assert (no panic!): gather every check, then assert the whole set.
    let mut failures: Vec<String> = Vec::new();

    if !obs.exec_succeeded() {
        failures.push(format!(
            "the mapped workload must run to success; transcript={:?}",
            obs.transcript
        ));
    }

    match field(&stdout, "uid") {
        Some(uid) if uid == "0" => {}
        other => failures.push(format!(
            "workload uid must be 0 inside the userns, got {other:?}"
        )),
    }
    match field(&stdout, "gid") {
        Some(gid) if gid == "0" => {}
        other => failures.push(format!(
            "workload gid must be 0 inside the userns, got {other:?}"
        )),
    }

    let want_uid_map = format!("0 {euid} 1");
    match field(&stdout, "uid_map").map(|m| normalize_map(&m)) {
        Some(m) if m == want_uid_map => {}
        other => failures.push(format!(
            "uid_map must be {want_uid_map:?} (child uid 0 -> launcher euid), got {other:?}"
        )),
    }
    let want_gid_map = format!("0 {egid} 1");
    match field(&stdout, "gid_map").map(|m| normalize_map(&m)) {
        Some(m) if m == want_gid_map => {}
        other => failures.push(format!(
            "gid_map must be {want_gid_map:?} (child gid 0 -> launcher egid), got {other:?}"
        )),
    }
    match field(&stdout, "setgroups") {
        Some(sg) if sg == "deny" => {}
        other => failures.push(format!(
            "setgroups must be 'deny' (mandatory before gid_map), got {other:?}"
        )),
    }

    assert!(
        failures.is_empty(),
        "userns rendezvous failed its independent kernel-state checks: {failures:#?}"
    );
}

#[test]
fn off_path_no_userns_shares_the_parent_namespace_and_is_unaffected() {
    let mut sink = std::io::stderr();
    let (euid, _egid) = effective_ids();
    let obs = run(None);
    let stdout = String::from_utf8_lossy(&obs.captured_stdout).into_owned();
    let _ = writeln!(
        sink,
        "off-path (no userns): euid={euid}; workload stdout:\n{stdout}\ntranscript: {:?}",
        obs.transcript
    );

    let mut failures: Vec<String> = Vec::new();

    // The opt-in did NOT disturb the no-userns path: the launcher still execs to success.
    if !obs.exec_succeeded() {
        failures.push(format!(
            "the no-userns workload must still run to success (opt-in undisturbed); transcript={:?}",
            obs.transcript
        ));
    }

    // NON-VACUOUS: with no userns the workload runs in the LAUNCHER's own userns — its uid
    // is the launcher's real uid (NOT remapped to 0), and its uid_map is the host identity
    // map, NOT the rendezvous `0 <euid> 1`.
    let uid = field(&stdout, "uid");
    if uid.as_deref() == Some("0") && euid != 0 {
        failures.push(format!(
            "without a userns request the workload must NOT be uid 0 (it shares our userns); got {uid:?}"
        ));
    }
    let uid_map = field(&stdout, "uid_map").map(|m| normalize_map(&m));
    let rendezvous_map = format!("0 {euid} 1");
    if uid_map.as_deref() == Some(rendezvous_map.as_str()) {
        failures.push(format!(
            "without a userns request the uid_map must NOT be the rendezvous map {rendezvous_map:?}; \
             got {uid_map:?} — the no-userns path engaged a userns it should not have"
        ));
    }

    assert!(
        failures.is_empty(),
        "off-path (no-userns) checks failed: {failures:#?}"
    );
}

#[test]
fn fail_closed_when_userns_unsupported_target_never_runs() {
    let mut sink = std::io::stderr();
    // Path A: a host that REFUSES unprivileged userns. There, clone3(CLONE_NEWUSER) itself
    // fails, so the launcher faults BEFORE any child runs — the target never runs.
    if !unprivileged_userns_available() {
        let obs = run(Some(UserNsRequest::new()));
        let _ = writeln!(
            sink,
            "fail-closed (no userns support): transcript={:?}",
            obs.transcript
        );
        assert!(
            !obs.exec_succeeded(),
            "fail-closed: with no userns support the target must NEVER run; transcript={:?}",
            obs.transcript
        );
        return;
    }

    // Path B: a host that SUPPORTS userns — so clone3 succeeds and we must force the
    // PARENT's map-write to fail to exercise the reap-and-fault branch. The
    // `dangerous-test-hooks` injection (`BVISOR_TEST_FORCE_USERNS_MAP_FAIL=1`) makes the
    // launcher's first map write target a non-existent `/proc/<pid>/` attribute, which the
    // kernel rejects. The launcher must then NOT release the child, reap it, and fault —
    // the target NEVER runs (no ExecSucceeded). This is a genuine fail-closed witness, not
    // a SKIP, on the exact hosts where the happy path also runs.
    //
    // The flag is forwarded for THIS launch ONLY via `run_launcher_with_env` (the launcher
    // spawns with `env_clear()`), so no process-global env is mutated and concurrent tests
    // are unaffected.
    let obs = run_with_env(
        Some(UserNsRequest::new()),
        &[(
            bvisor::linux::launch::ENV_FORCE_USERNS_MAP_FAIL,
            "1".to_owned(),
        )],
    );
    let _ = writeln!(
        sink,
        "fail-closed (forced map-write failure): transcript={:?}",
        obs.transcript
    );
    assert!(
        !obs.exec_succeeded(),
        "fail-closed: a broken userns map-write must reap the child and fault — the target \
         must NEVER run; transcript={:?}",
        obs.transcript
    );
    // Stronger: the launcher recorded the fail-closed reason on its transcript.
    assert!(
        obs.notes.iter().any(|n| n.contains("map_write_failed")),
        "the launcher must record the fail-closed map-write reason; notes={:?}",
        obs.notes
    );
}
