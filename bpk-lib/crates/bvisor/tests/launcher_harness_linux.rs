// REAL landlock FS confinement through the HOST-SIDE LAUNCHER HARNESS (kernel plan
// §10.8, backend→launcher rewire step 7a), re-proving G1 (secret-read-denied) + G3
// (escape-write-denied) THROUGH the new reusable harness — NOT a hand-rolled spawn.
// Real-OS: the harness seals a plan into a memfd and spawns the `bvisor-linux-launcher`
// bin, so it is gated to Linux + the backend-linux feature. The G1/G3 oracle is gated on
// the LIVE landlock ABI probe: below the floor (or sandbox-blocked) the confinement
// assertions are SKIPPED with an explicit message — never silently passed.
#![cfg(all(target_os = "linux", feature = "backend-linux"))]
//! THE HARNESS NEVER GRADES ITSELF. An INDEPENDENT [`FsGroundTruth`] reads the REAL
//! on-disk effect (did the escape file appear? did the secret bytes leak into the in-root
//! sink?), NEVER the harness's collected transcript. The transcript/terminal is asserted
//! SEPARATELY (the harness's `LaunchObservation` must report `ConfinementPhaseResolved` +
//! `confinement_installed` + terminal `ExecSucceeded`), but the SAFETY verdict is the disk.
//!
//! This proves the host-side harness DRIVES real confinement — the foundation step 7b
//! cuts `execute()` onto. (The hand-rolled-spawn variant lives in
//! `launcher_landlock_linux.rs`; this file proves the SAME confinement through the
//! production harness API `bvisor::linux::launch::run_launcher`.)

use bvisor::linux::launch::{run_launcher, AuthorityFd};
use bvisor::linux::protocol::{
    DescriptorKind, DescriptorRole, DescriptorShape, DescriptorSlotV1, LauncherState,
    LinuxLaunchBodyV1, LinuxLaunchPlanV1, LoweringWireEntryV1, LoweringWireV1, TargetSpecV1,
};
use bvisor::{AdmissionProgramHash, AttemptId, BackendProfileHash, BoundaryPlanHash};
use std::io::Write;
use std::os::fd::{OwnedFd, RawFd};
use std::path::{Path, PathBuf};

// Frozen ids/phase-codes the launcher serves (mirror the launcher's constants).
const ID_AMBIENT_SCRUB: &str = "linux.ambient.scrub.v1";
const ID_LANDLOCK_APPLY: &str = "linux.landlock.apply.v1";
const ID_EXEC: &str = "linux.exec.v1";
const PHASE_CODE_SCRUB: u8 = 3; // LoweringPhase::FdHygiene.code()
const PHASE_CODE_CONFINE: u8 = 4; // LoweringPhase::PolicyInstall.code()
const PHASE_CODE_EXEC: u8 = 5; // LoweringPhase::Launch.code()

// The landlock ABI floor the launcher confines at (mirrors the launcher sys floor).
const LANDLOCK_ABI_FLOOR: i64 = 3;

// Fixed authority slot indices == the fd numbers the launcher reads each handle at. The
// harness places the launcher's OWN channel fds (plan/control/error) ABOVE these, so they
// never collide with a declared descriptor slot.
const SLOT_EXE: RawFd = 10;
const SLOT_READ_ROOT: RawFd = 15;
const SLOT_WRITE_ROOT: RawFd = 16;
const SLOT_SYS_ROOT_BASE: RawFd = 20;

/// System dirs a dynamically-linked workload must READ (loader + shared libs), declared
/// as additional ReadRoot slots. None hold the secret/escape targets, so confinement of
/// those is unaffected. Only dirs that EXIST on the host are wired.
const SYSTEM_EXEC_ROOTS: &[&str] = &["/usr", "/lib", "/lib64", "/bin", "/sbin", "/etc"];

// ── Live landlock probe (the SKIP gate) ─────────────────────────────────────────

/// Probe the LIVE landlock ABI exactly as the launcher does (`>=1`, or `0` when
/// unavailable). The G1/G3 confinement assertions run ONLY at/above the floor; otherwise
/// the test SKIPS them with an explicit message (never a silent pass).
fn live_landlock_abi() -> i64 {
    const LANDLOCK_CREATE_RULESET_VERSION: libc::c_uint = 1;
    // SAFETY: documented version-query form (NULL attr, 0 size); reads no user memory,
    // creates no fd, mutates nothing. Test-only probe.
    let raw = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if raw < 0 {
        0
    } else {
        raw
    }
}

fn landlock_available() -> bool {
    live_landlock_abi() >= LANDLOCK_ABI_FLOOR
}

// ── Scratch tree + independent oracle ───────────────────────────────────────────

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!("bvisor-harness-fs-{tag}-{pid}-{nanos}"));
        std::fs::create_dir_all(&root).expect("scratch root");
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The INDEPENDENT oracle: reconstructs what actually happened by reading the REAL disk,
/// never the harness's transcript. `true` ⇒ the marker is present on disk ⇒ the dangerous
/// effect actually landed (confinement FAILED or never ran).
struct FsGroundTruth {
    marker: String,
    witness_path: PathBuf,
}

impl FsGroundTruth {
    fn danger_occurred(&self) -> bool {
        match std::fs::read(&self.witness_path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).contains(&self.marker),
            Err(_) => false,
        }
    }

    fn effect_landed(&self) -> bool {
        self.danger_occurred()
    }
}

// ── Wire helpers ─────────────────────────────────────────────────────────────────

fn entry(id: &str, phase_code: u8) -> LoweringWireEntryV1 {
    LoweringWireEntryV1 {
        id: id.to_owned(),
        version: 1,
        phase_code,
        param_digest: [0u8; 32],
        decl_digest: [0u8; 32],
    }
}

/// A body whose `h_l` is `blake3(canonical(lowering))` so the launcher's schedule-digest
/// binding passes (the REAL H_L binding is #75 — noted).
fn body_with(
    lowering: LoweringWireV1,
    table: Vec<DescriptorSlotV1>,
    argv: Vec<String>,
) -> LinuxLaunchBodyV1 {
    let bytes = batpak::canonical::to_bytes(&lowering).expect("encode lowering");
    let h_l = batpak::event::hash::compute_hash(&bytes);
    LinuxLaunchBodyV1 {
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
            exe_slot: u32::try_from(SLOT_EXE).expect("fd fits u32"),
        },
    }
}

fn exe_slot() -> DescriptorSlotV1 {
    DescriptorSlotV1 {
        slot_index: u32::try_from(SLOT_EXE).expect("fd"),
        role: DescriptorRole::TargetExe,
        expected: DescriptorShape {
            kind: DescriptorKind::Regular,
            writable: false,
        },
    }
}

/// A confinement-root slot declaration (a directory fd is never writable per `O_ACCMODE`,
/// so the declared shape is `writable:false`; the landlock WRITE grant is driven by the
/// `role`, not the fd's open mode).
fn root_slot(fd: RawFd, role: DescriptorRole) -> DescriptorSlotV1 {
    DescriptorSlotV1 {
        slot_index: u32::try_from(fd).expect("fd"),
        role,
        expected: DescriptorShape {
            kind: DescriptorKind::Directory,
            writable: false,
        },
    }
}

fn present_system_roots() -> Vec<&'static str> {
    SYSTEM_EXEC_ROOTS
        .iter()
        .copied()
        .filter(|p| Path::new(p).is_dir())
        .collect()
}

/// A scrub + landlock-apply + exec plan confining FS to: the read root + write root, PLUS
/// one ReadRoot per present system-exec dir (loader/libs). `n_sys` system roots at
/// `SLOT_SYS_ROOT_BASE..`.
fn confined_plan(argv: Vec<String>, n_sys: usize) -> LinuxLaunchPlanV1 {
    let lowering = LoweringWireV1 {
        entries: vec![
            entry(ID_AMBIENT_SCRUB, PHASE_CODE_SCRUB),
            entry(ID_LANDLOCK_APPLY, PHASE_CODE_CONFINE),
            entry(ID_EXEC, PHASE_CODE_EXEC),
        ],
    };
    let mut table = vec![
        exe_slot(),
        root_slot(SLOT_READ_ROOT, DescriptorRole::ReadRoot),
        root_slot(SLOT_WRITE_ROOT, DescriptorRole::WriteRoot),
    ];
    for i in 0..n_sys {
        let fd = SLOT_SYS_ROOT_BASE + RawFd::try_from(i).expect("fd");
        table.push(root_slot(fd, DescriptorRole::ReadRoot));
    }
    LinuxLaunchPlanV1 {
        body: body_with(lowering, table, argv),
    }
}

/// The SAME plan WITHOUT the landlock action (scrub + exec only) — the anti-vacuous
/// control: the launcher runs it UNCONFINED, so the escape WOULD land. No root slots.
fn unconfined_plan(argv: Vec<String>) -> LinuxLaunchPlanV1 {
    let lowering = LoweringWireV1 {
        entries: vec![
            entry(ID_AMBIENT_SCRUB, PHASE_CODE_SCRUB),
            entry(ID_EXEC, PHASE_CODE_EXEC),
        ],
    };
    LinuxLaunchPlanV1 {
        body: body_with(lowering, vec![exe_slot()], argv),
    }
}

// ── fd helpers (SAFE: std::fs only — the harness owns ALL the unsafe) ─────────────

fn open_sh() -> OwnedFd {
    OwnedFd::from(std::fs::File::open("/bin/sh").expect("open /bin/sh"))
}

fn open_dir(path: &Path) -> OwnedFd {
    OwnedFd::from(std::fs::File::open(path).expect("open dir"))
}

/// The compile-time path to the launcher bin (the harness honors a `BVISOR_LAUNCHER_BIN`
/// override on top of this; content-addressed identity is step 12).
fn launcher_bin() -> PathBuf {
    bvisor::linux::launch::resolve_launcher_path(env!("CARGO_BIN_EXE_bvisor-linux-launcher"))
}

// ── Tests ────────────────────────────────────────────────────────────────────────

/// G1 + G3 + CONTROL through the HARNESS, all in one confined run (one sh, three effects),
/// graded by the INDEPENDENT on-disk oracle. The harness's collected terminal/transcript
/// is asserted separately. Skipped (explicitly) if landlock is unavailable at the floor.
#[test]
fn harness_landlock_denies_secret_read_and_escape_write_allows_in_root() {
    if !landlock_available() {
        let mut sink = std::io::stderr();
        let _ = writeln!(
            sink,
            "SKIP harness G1/G3: live landlock ABI {} < floor {} \
             (kernel lacks landlock or the sandbox blocks it)",
            live_landlock_abi(),
            LANDLOCK_ABI_FLOOR
        );
        return;
    }

    let scratch = Scratch::new("g1g3");
    let quarantine = scratch.path("quarantine"); // the declared WRITE root
    std::fs::create_dir_all(&quarantine).expect("quarantine dir");
    let readable = scratch.path("readable"); // a READ root (CONTROL source lives here)
    std::fs::create_dir_all(&readable).expect("readable dir");
    let in_root_src = readable.join("inside.txt");
    let control_marker = "CONTROL-IN-ROOT-MARKER".to_string();
    std::fs::write(&in_root_src, &control_marker).expect("plant in-root source");

    // The secret lives OUTSIDE every declared root (G1 target).
    let secret = scratch.path("secret.key");
    let secret_marker = "TOPSECRET-HARNESS-G1".to_string();
    std::fs::write(&secret, &secret_marker).expect("plant secret");

    // Sinks INSIDE the writable quarantine (where the workload reports outcomes).
    let exfil = quarantine.join("exfil.txt"); // G1: gets the secret IF the read leaks
    let control_sink = quarantine.join("control.txt"); // CONTROL: the in-root copy

    // The escape target is OUTSIDE the write root (G3 target).
    let escape = scratch.path("escape.txt");
    let escape_marker = "ESCAPED-HARNESS-G3".to_string();

    let cmd = format!(
        "cat {secret} > {exfil} ; echo {escape_marker} > {escape} ; cat {src} > {control_sink}",
        secret = secret.to_string_lossy(),
        exfil = exfil.to_string_lossy(),
        escape_marker = escape_marker,
        escape = escape.to_string_lossy(),
        src = in_root_src.to_string_lossy(),
        control_sink = control_sink.to_string_lossy(),
    );

    let sys_dirs = present_system_roots();
    let plan = confined_plan(vec!["sh".into(), "-c".into(), cmd], sys_dirs.len());

    // The pre-opened authority handles, keyed to their declared slot fd numbers.
    let mut authority = vec![
        AuthorityFd {
            slot_index: SLOT_EXE,
            handle: open_sh(),
        },
        AuthorityFd {
            slot_index: SLOT_READ_ROOT,
            handle: open_dir(&readable),
        },
        AuthorityFd {
            slot_index: SLOT_WRITE_ROOT,
            handle: open_dir(&quarantine),
        },
    ];
    for (i, p) in sys_dirs.iter().enumerate() {
        authority.push(AuthorityFd {
            slot_index: SLOT_SYS_ROOT_BASE + RawFd::try_from(i).expect("fd"),
            handle: open_dir(Path::new(p)),
        });
    }

    let obs = run_launcher(&launcher_bin(), &plan, authority).expect("run launcher harness");

    // ── GroundTruth (REAL disk, NOT the transcript) ──────────────────────────────
    let g1 = FsGroundTruth {
        marker: secret_marker,
        witness_path: exfil,
    };
    assert!(
        !g1.danger_occurred(),
        "G1: landlock must block the out-of-root secret READ; the secret leaked into the \
         in-root exfil sink on disk. observation:\n{obs:?}"
    );

    let g3 = FsGroundTruth {
        marker: escape_marker,
        witness_path: escape,
    };
    assert!(
        !g3.danger_occurred(),
        "G3: landlock must block the out-of-quarantine WRITE; the escape file exists on \
         disk. observation:\n{obs:?}"
    );

    let control = FsGroundTruth {
        marker: control_marker,
        witness_path: control_sink,
    };
    assert!(
        control.effect_landed(),
        "CONTROL: an in-root read→in-root write must be ALLOWED through landlock (the deny \
         tests above are otherwise vacuous). observation:\n{obs:?}"
    );

    // ── The harness's collected observation (asserted SEPARATELY from the disk) ───
    assert_eq!(
        obs.terminal,
        Some(LauncherState::ExecSucceeded),
        "the harness must collect terminal ExecSucceeded (workload ran, confined): {obs:?}"
    );
    assert!(
        obs.exec_succeeded(),
        "exec_succeeded() must agree with the terminal: {obs:?}"
    );
    assert!(
        obs.transcript
            .contains(&LauncherState::ConfinementPhaseResolved),
        "the harness transcript must resolve the Confinement phase: {obs:?}"
    );
    assert!(
        obs.confinement_installed,
        "the harness must collect REAL confinement evidence (installed=true): {obs:?}"
    );
    assert_eq!(
        obs.outcome(),
        Some(bvisor::Outcome::Completed),
        "ExecSucceeded maps to Outcome::Completed (the honest 7b mapping): {obs:?}"
    );
}

/// ANTI-VACUOUS DETECTOR through the harness: the SAME escape workload through a plan with
/// NO landlock action runs UNCONFINED, so GroundTruth SEES the escape land — proving the
/// oracle distinguishes confinement from non-confinement. Not landlock-gated: it proves the
/// escape WOULD happen absent confinement, regardless of kernel support, AND that the
/// harness honestly reports NO confinement install.
#[test]
fn harness_without_landlock_lets_the_escape_land() {
    let scratch = Scratch::new("novacuous");
    let escape = scratch.path("escape.txt");
    let marker = "ESCAPED-HARNESS-NOLANDLOCK".to_string();
    let cmd = format!(
        "echo {marker} > {escape}",
        marker = marker,
        escape = escape.to_string_lossy(),
    );

    let plan = unconfined_plan(vec!["sh".into(), "-c".into(), cmd]);
    let authority = vec![AuthorityFd {
        slot_index: SLOT_EXE,
        handle: open_sh(),
    }];
    let obs = run_launcher(&launcher_bin(), &plan, authority).expect("run launcher harness");

    let gt = FsGroundTruth {
        marker,
        witness_path: escape,
    };
    assert!(
        gt.danger_occurred(),
        "NON-VACUOUS: an UNCONFINED launch must let the escape write land on disk (so the \
         confined G3 deny is meaningful). observation:\n{obs:?}"
    );
    assert_eq!(
        obs.terminal,
        Some(LauncherState::ExecSucceeded),
        "the unconfined workload still runs to success: {obs:?}"
    );
    assert!(
        !obs.confinement_installed,
        "an exec-only plan must report NO confinement install (no over-claim): {obs:?}"
    );
}
