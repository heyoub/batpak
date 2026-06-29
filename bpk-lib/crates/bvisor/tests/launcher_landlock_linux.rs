// REAL landlock FS confinement THROUGH the single-threaded Linux confinement
// LAUNCHER (kernel plan §10.8), wired as G1 (secret-read-denied) and G3
// (escape-write-denied). Real-OS: spawns the `bvisor-linux-launcher` bin with
// inherited root fds + a landlock-apply lowering action, so it is gated to Linux +
// the backend-linux feature. The G1/G3 oracle is gated on the LIVE landlock ABI
// probe: if the kernel lacks landlock at the required floor (or the sandbox blocks
// it), the confinement assertions are SKIPPED with an explicit message — never
// silently passed.
#![cfg(all(target_os = "linux", feature = "backend-linux"))]
//! THE LAUNCHER NEVER GRADES ITSELF. An INDEPENDENT [`FsGroundTruth`] reads the REAL
//! on-disk effect (did the escape file appear? did the secret bytes leak into the
//! in-root sink?), NEVER the launcher's control-fd transcript. The transcript is
//! asserted SEPARATELY (it must report `ConfinementPhaseResolved` + `installed=true`
//! + `ExecSucceeded`), but the SAFETY verdict is the disk.
//!
//! G1 (secret-read-denied): the workload `cat`s a secret OUTSIDE the declared root,
//! redirecting into a sink INSIDE the writable quarantine. If landlock blocks the
//! READ, the sink never gets the secret bytes. GroundTruth reads that sink on disk.
//!
//! G3 (escape-write-denied): the workload writes a file OUTSIDE the write root.
//! GroundTruth stats the REAL disk: the escape file existing ⇒ the write escaped.
//!
//! CONTROL (non-vacuous): the workload reads a file INSIDE the root and copies it to
//! an in-root sink; that copy MUST land — proving the sandbox is not a blanket deny.
//!
//! ANTI-VACUOUS DETECTOR (the "lying launcher" analogue): a SEPARATE plan with NO
//! landlock action runs the SAME escape — and GroundTruth then SEES the escape land,
//! proving the test can distinguish confinement from non-confinement. (This is the
//! launcher-path mirror of grid_linux_fs.rs's red fixture, kept inline since it is a
//! cheap second spawn.)

use bvisor::linux::launch::transcript_confinement_unavailable;
use bvisor::linux::protocol::{
    DescriptorKind, DescriptorRole, DescriptorShape, DescriptorSlotV1, LinuxLaunchBodyV1,
    LinuxLaunchPlanV1, LoweringWireEntryV1, LoweringWireV1, TargetSpecV1,
};
use bvisor::{AdmissionProgramHash, AttemptId, BackendProfileHash, BoundaryPlanHash};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

// Frozen ids/phase-codes the launcher serves (mirror the launcher's constants).
const ID_AMBIENT_SCRUB: &str = "linux.ambient.scrub.v1";
const ID_LANDLOCK_APPLY: &str = "linux.landlock.apply.v1";
const ID_EXEC: &str = "linux.exec.v1";
const PHASE_CODE_SCRUB: u8 = 3; // LoweringPhase::FdHygiene.code()
const PHASE_CODE_CONFINE: u8 = 4; // LoweringPhase::PolicyInstall.code()
const PHASE_CODE_EXEC: u8 = 5; // LoweringPhase::Launch.code()

// The landlock ABI floor the launcher confines at (mirrors sys::LANDLOCK_ABI_FLOOR).
const LANDLOCK_ABI_FLOOR: i64 = 3;

// Fixed fd numbers the launcher inherits (chosen well above stdio).
const FIXED_EXE_FD: RawFd = 10;
const FIXED_CONTROL_FD: RawFd = 11;
const FIXED_ERROR_WRITE_FD: RawFd = 12;
const FIXED_PLAN_FD: RawFd = 13;
const FIXED_ERROR_READ_FD: RawFd = 14;
const FIXED_READ_ROOT_FD: RawFd = 15; // the test's own readable root (CONTROL source)
const FIXED_WRITE_ROOT_FD: RawFd = 16; // the writable quarantine (declared WRITE root)
                                       // The system-exec READ roots start here (one fd per dir). A real plan's planner adds
                                       // these so a dynamically-linked workload can read its loader + shared libraries; the
                                       // confinement of the SECRET / ESCAPE targets is unaffected (they live elsewhere).
const FIXED_SYS_ROOT_BASE: RawFd = 20;

/// System dirs a dynamically-linked workload must READ (loader + shared libs). These
/// are declared as additional ReadRoot slots — the launcher-path analogue of the
/// backend's `SYSTEM_EXEC_ROOTS`. None hold the secret/escape targets, so confinement
/// of those is unaffected. Only dirs that EXIST on the host are wired.
const SYSTEM_EXEC_ROOTS: &[&str] = &["/usr", "/lib", "/lib64", "/bin", "/sbin", "/etc"];

// ── Live landlock probe (the SKIP gate) ─────────────────────────────────────────

/// Probe the LIVE landlock ABI exactly as the launcher does. Returns the supported
/// version (`>=1`) or `0` when landlock is unavailable. The G1/G3 confinement
/// assertions run ONLY when this is `>= LANDLOCK_ABI_FLOOR`; otherwise the test
/// SKIPS them with an explicit message (never a silent pass).
fn live_landlock_abi() -> i64 {
    const LANDLOCK_CREATE_RULESET_VERSION: libc::c_uint = 1;
    // SAFETY: documented version-query form (NULL attr, 0 size); reads no user
    // memory, creates no fd, mutates nothing. Test-only probe.
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

/// Whether landlock is usable at the launcher's floor. When false the confinement
/// assertions are skipped (printed) rather than silently passed.
fn landlock_available() -> bool {
    live_landlock_abi() >= LANDLOCK_ABI_FLOOR
}

// ── Scratch tree ─────────────────────────────────────────────────────────────────

/// Per-test scratch dir under a unique path so concurrent tests never collide.
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
        let root = std::env::temp_dir().join(format!("bvisor-launcher-fs-{tag}-{pid}-{nanos}"));
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

/// The INDEPENDENT oracle: reconstructs what actually happened by reading the REAL
/// disk, never the launcher's transcript. `true` ⇒ the marker is present on disk ⇒
/// the dangerous effect actually landed (confinement FAILED or never ran).
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

    /// Whether the (allowed) in-root effect landed — used for the non-vacuous control.
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

/// A body whose `h_l` is `blake3(canonical(lowering))` so the launcher's
/// schedule-digest binding passes (the REAL H_L binding is #75 — note it).
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
            exe_slot: u32::try_from(FIXED_EXE_FD).expect("fd fits u32"),
            user_namespace: None,
            network_namespace: None,
            seccomp: None,
        },
    }
}

fn exe_slot() -> DescriptorSlotV1 {
    DescriptorSlotV1 {
        slot_index: u32::try_from(FIXED_EXE_FD).expect("fd"),
        role: DescriptorRole::TargetExe,
        expected: DescriptorShape {
            kind: DescriptorKind::Regular,
            writable: false,
        },
    }
}

/// A confinement-root slot declaration. A directory fd is never writable per
/// `O_ACCMODE` (a dir cannot be opened O_WRONLY/O_RDWR), so the DECLARED shape is
/// always `writable:false`; the landlock WRITE grant is driven by the `role`
/// (WriteRoot vs ReadRoot), not the fd's open mode.
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

/// The system-exec dirs that exist on this host (so we only wire fds that open).
fn present_system_roots() -> Vec<&'static str> {
    SYSTEM_EXEC_ROOTS
        .iter()
        .copied()
        .filter(|p| Path::new(p).is_dir())
        .collect()
}

/// A scrub + landlock-apply + exec plan confining FS to: the test's read root +
/// write root, PLUS one ReadRoot per present system-exec dir (so a dynamically-linked
/// workload can read its loader/libs). `n_sys` system roots are wired at
/// `FIXED_SYS_ROOT_BASE..`.
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
        root_slot(FIXED_READ_ROOT_FD, DescriptorRole::ReadRoot),
        root_slot(FIXED_WRITE_ROOT_FD, DescriptorRole::WriteRoot),
    ];
    for i in 0..n_sys {
        let fd = FIXED_SYS_ROOT_BASE + RawFd::try_from(i).expect("fd");
        table.push(root_slot(fd, DescriptorRole::ReadRoot));
    }
    LinuxLaunchPlanV1 {
        body: body_with(lowering, table, argv),
    }
}

/// The SAME plan WITHOUT the landlock action (scrub + exec only) — the anti-vacuous
/// control: the launcher runs it UNCONFINED, so the escape WOULD land. No root slots
/// are declared (no landlock action references them).
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

// ── fd setup ─────────────────────────────────────────────────────────────────────

fn open_sh() -> OwnedFd {
    OwnedFd::from(std::fs::File::open("/bin/sh").expect("open /bin/sh"))
}

/// Open a directory read-only as an OwnedFd (a landlock root rides this fd).
fn open_dir(path: &Path) -> OwnedFd {
    OwnedFd::from(std::fs::File::open(path).expect("open dir"))
}

fn socketpair() -> (OwnedFd, OwnedFd) {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: test-only fd setup; `fds` is a valid 2-element out-array.
    let rc = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    };
    assert_eq!(rc, 0, "socketpair");
    // SAFETY: socketpair just produced two fresh, owned fds.
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}

fn error_pipe() -> (OwnedFd, OwnedFd) {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: test-only; `fds` is a valid 2-element out-array for pipe2.
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    assert_eq!(rc, 0, "pipe2");
    // SAFETY: pipe2 just produced two fresh, owned fds.
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}

fn plan_fd(plan: &LinuxLaunchPlanV1) -> OwnedFd {
    use std::io::{Seek, SeekFrom, Write};
    let bytes = plan.encode().expect("encode plan");
    let mut f = tempfile::tempfile().expect("tempfile");
    f.write_all(&bytes).expect("write plan");
    f.seek(SeekFrom::Start(0)).expect("rewind");
    OwnedFd::from(f)
}

/// Relocate an owned fd to a HIGH number (>= `FD_RELOCATE_BASE`) in the PARENT via
/// `F_DUPFD_CLOEXEC`, returning the new OwnedFd and consuming the original. This keeps
/// every dup-FROM source above the fixed dup-TO targets (10..=~25), so the pre_exec
/// dup2 sequence can never clobber a not-yet-consumed source. CLOEXEC on the high copy
/// is fine — the pre_exec dup2 onto the final fixed fd clears CLOEXEC there.
const FD_RELOCATE_BASE: RawFd = 100;
fn relocate_high(fd: OwnedFd) -> OwnedFd {
    // SAFETY: test-only; F_DUPFD_CLOEXEC returns a fresh owned fd >= the base, or -1.
    let new = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, FD_RELOCATE_BASE) };
    assert!(new >= FD_RELOCATE_BASE, "F_DUPFD_CLOEXEC relocate");
    // SAFETY: `new` is a fresh owned fd from F_DUPFD_CLOEXEC.
    let relocated = unsafe { OwnedFd::from_raw_fd(new) };
    drop(fd); // close the low original; only the high copy survives.
    relocated
}

fn dup_to(src: RawFd, target: RawFd, keep_cloexec: bool) -> std::io::Result<()> {
    // SAFETY: runs in the forked child via pre_exec; dup2/fcntl are async-signal-safe
    // and operate on inherited fds. Test-only.
    unsafe {
        if libc::dup2(src, target) < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if !keep_cloexec {
            let flags = libc::fcntl(target, libc::F_GETFD);
            if flags >= 0 {
                let _ = libc::fcntl(target, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
        }
    }
    Ok(())
}

/// The optional confinement roots to wire (read root, write root, system read roots).
/// `None` ⇒ the unconfined plan, which declares no root slots.
struct Roots {
    read: OwnedFd,
    write: OwnedFd,
    /// One open dir fd per present system-exec root (loader/libs), in the same order
    /// as [`confined_plan`]'s system-root slots.
    system: Vec<OwnedFd>,
}

/// Spawn the launcher with the given plan, exe fd, and optional roots; return the
/// child handle + the test's control-read end (the transcript).
fn spawn_launcher(
    plan: &LinuxLaunchPlanV1,
    exe_fd: OwnedFd,
    roots: Option<Roots>,
) -> (std::process::Child, OwnedFd) {
    // Serialise the fd-setup + fork window across parallel test threads: the pre_exec
    // dup2 sequence targets FIXED fd numbers, so concurrent spawns must not race on
    // them. (The launcher no longer refuses on an undeclared inherited fd — the child
    // scrub closes it — so a sibling's fd leaking into the fork is not a flake source.)
    static SPAWN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = SPAWN_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    // Relocate EVERY dup-source to a high fd (>= 100) so the fixed dup-TO targets
    // (10..=~25) never collide with a not-yet-consumed source during the pre_exec
    // dup2 sequence.
    let exe_fd = relocate_high(exe_fd);
    let pfd = relocate_high(plan_fd(plan));
    let (control_launcher, control_test) = socketpair();
    let (error_read, error_write) = error_pipe();
    let control_launcher = relocate_high(control_launcher);
    let error_write = relocate_high(error_write);
    let error_read = relocate_high(error_read);
    let roots = roots.map(|r| Roots {
        read: relocate_high(r.read),
        write: relocate_high(r.write),
        system: r.system.into_iter().map(relocate_high).collect(),
    });

    let exe_raw = exe_fd.as_raw_fd();
    let control_raw = control_launcher.as_raw_fd();
    let error_w_raw = error_write.as_raw_fd();
    let error_r_raw = error_read.as_raw_fd();
    let plan_raw = pfd.as_raw_fd();
    let (read_raw, write_raw, sys_raw): (Option<RawFd>, Option<RawFd>, Vec<RawFd>) = match &roots {
        Some(r) => (
            Some(r.read.as_raw_fd()),
            Some(r.write.as_raw_fd()),
            r.system.iter().map(|f| f.as_raw_fd()).collect(),
        ),
        None => (None, None, Vec::new()),
    };

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bvisor-linux-launcher"));
    cmd.env_clear()
        .env("BVISOR_LAUNCH_PLAN_FD", FIXED_PLAN_FD.to_string())
        .env("BVISOR_CONTROL_FD", FIXED_CONTROL_FD.to_string())
        .env("BVISOR_ERROR_FD", FIXED_ERROR_WRITE_FD.to_string())
        .env("BVISOR_ERROR_READ_FD", FIXED_ERROR_READ_FD.to_string());

    // SAFETY: test-only pre_exec — dup the inherited fds to fixed numbers. The
    // error-WRITE fd is re-set O_CLOEXEC (so a successful target execve auto-closes it
    // → coordinator read end sees EOF); the rest clear CLOEXEC so the launcher
    // inherits them.
    unsafe {
        cmd.pre_exec(move || {
            dup_to(exe_raw, FIXED_EXE_FD, false)?;
            dup_to(control_raw, FIXED_CONTROL_FD, false)?;
            dup_to(plan_raw, FIXED_PLAN_FD, false)?;
            dup_to(error_r_raw, FIXED_ERROR_READ_FD, false)?;
            if let Some(rr) = read_raw {
                dup_to(rr, FIXED_READ_ROOT_FD, false)?;
            }
            if let Some(wr) = write_raw {
                dup_to(wr, FIXED_WRITE_ROOT_FD, false)?;
            }
            let mut i = 0;
            while i < sys_raw.len() {
                let offset = RawFd::try_from(i).unwrap_or(RawFd::MAX);
                dup_to(sys_raw[i], FIXED_SYS_ROOT_BASE + offset, false)?;
                i += 1;
            }
            if libc::dup2(error_w_raw, FIXED_ERROR_WRITE_FD) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let flags = libc::fcntl(FIXED_ERROR_WRITE_FD, libc::F_GETFD);
            if flags >= 0 {
                let _ = libc::fcntl(
                    FIXED_ERROR_WRITE_FD,
                    libc::F_SETFD,
                    flags | libc::FD_CLOEXEC,
                );
            }
            Ok(())
        });
    }

    let child = cmd.spawn().expect("spawn launcher");
    drop(control_launcher);
    drop(error_write);
    drop(error_read);
    drop(pfd);
    drop(exe_fd);
    drop(roots);
    (child, control_test)
}

fn read_all(fd: OwnedFd) -> String {
    let mut f = std::fs::File::from(fd);
    let mut s = String::new();
    let _ = f.read_to_string(&mut s);
    s
}

// ── Tests ────────────────────────────────────────────────────────────────────────

/// G1 + G3 + CONTROL through the launcher, all in one confined run (one sh, three
/// effects), graded by the INDEPENDENT on-disk oracle. The transcript is asserted
/// separately. Skipped (explicitly) if landlock is unavailable at the floor.
#[test]
fn launcher_landlock_denies_secret_read_and_escape_write_allows_in_root() {
    if !landlock_available() {
        // EXPLICIT skip (never a silent pass): the workspace lints deny the print
        // macros, so write the notice through the stderr handle directly.
        let mut sink = std::io::stderr();
        let _ = writeln!(
            sink,
            "SKIP launcher_landlock G1/G3: live landlock ABI {} < floor {} \
             (kernel lacks landlock or the sandbox blocks it)",
            live_landlock_abi(),
            LANDLOCK_ABI_FLOOR
        );
        return;
    }

    let scratch = Scratch::new("g1g3");
    // The writable quarantine (the declared WRITE root). The workload may read+write.
    let quarantine = scratch.path("quarantine");
    std::fs::create_dir_all(&quarantine).expect("quarantine dir");
    // A readable root: a directory the workload may READ under (the CONTROL source).
    let readable = scratch.path("readable");
    std::fs::create_dir_all(&readable).expect("readable dir");
    let in_root_src = readable.join("inside.txt");
    let control_marker = "CONTROL-IN-ROOT-MARKER".to_string();
    std::fs::write(&in_root_src, &control_marker).expect("plant in-root source");

    // The secret lives OUTSIDE every declared root (G1 target).
    let secret = scratch.path("secret.key");
    let secret_marker = "TOPSECRET-LAUNCHER-G1".to_string();
    std::fs::write(&secret, &secret_marker).expect("plant secret");

    // Sinks INSIDE the writable quarantine (where the workload reports outcomes).
    let exfil = quarantine.join("exfil.txt"); // G1: gets the secret IF the read leaks
    let control_sink = quarantine.join("control.txt"); // CONTROL: the in-root copy

    // The escape target is OUTSIDE the write root (G3 target).
    let escape = scratch.path("escape.txt");
    let escape_marker = "ESCAPED-LAUNCHER-G3".to_string();

    // One workload, three effects: (G1) read the out-of-root secret into the in-root
    // exfil sink; (G3) write the escape file outside quarantine; (CONTROL) copy the
    // in-root source into the in-root control sink. The `;`-chain runs all three
    // regardless of which the landlock sandbox denies.
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
    let roots = Roots {
        read: open_dir(&readable),
        write: open_dir(&quarantine),
        system: sys_dirs.iter().map(|p| open_dir(Path::new(p))).collect(),
    };
    let (mut child, control) = spawn_launcher(&plan, open_sh(), Some(roots));
    let transcript = read_all(control);
    let _ = child.wait();
    if transcript_confinement_unavailable(&transcript) {
        let mut sink = std::io::stderr();
        let _ = writeln!(
            sink,
            "SKIP launcher_landlock_denies_secret_read_and_escape_write_allows_in_root: \
             kernel/container lacks landlock/userns/seccomp (ENOSYS); the launcher faulted before \
             exec — exercised on capable kernels + the bvisor-linux CI lane"
        );
        return;
    }

    // ── GroundTruth (REAL disk, NOT the transcript) ──────────────────────────────
    let g1 = FsGroundTruth {
        marker: secret_marker,
        witness_path: exfil,
    };
    assert!(
        !g1.danger_occurred(),
        "G1: landlock must block the out-of-root secret READ; the secret leaked into \
         the in-root exfil sink on disk. transcript:\n{transcript}"
    );

    let g3 = FsGroundTruth {
        marker: escape_marker,
        witness_path: escape,
    };
    assert!(
        !g3.danger_occurred(),
        "G3: landlock must block the out-of-quarantine WRITE; the escape file exists \
         on disk. transcript:\n{transcript}"
    );

    // CONTROL: the in-root read+write DID land (non-vacuous — the deny tests are not a
    // blanket deny). This reads the in-root sink on disk.
    let control = FsGroundTruth {
        marker: control_marker,
        witness_path: control_sink,
    };
    assert!(
        control.effect_landed(),
        "CONTROL: an in-root read→in-root write must be ALLOWED through landlock (the \
         deny tests above are otherwise vacuous). transcript:\n{transcript}"
    );

    // ── Transcript (asserted SEPARATELY from the disk verdict) ───────────────────
    assert!(
        transcript.contains("ConfinementPhaseResolved"),
        "transcript must resolve the Confinement phase: {transcript}"
    );
    assert!(
        transcript.contains("installed=true"),
        "transcript must record REAL confinement evidence (installed=true): {transcript}"
    );
    assert!(
        transcript.trim_end().ends_with("ExecSucceeded"),
        "transcript must end ExecSucceeded (the workload ran, confined): {transcript}"
    );
}

/// ANTI-VACUOUS DETECTOR: the SAME escape workload through a plan with NO landlock
/// action runs UNCONFINED, so GroundTruth SEES the escape land — proving the oracle
/// distinguishes confinement from non-confinement (the launcher-path analogue of
/// grid_linux_fs.rs's lying-backend red fixture). Not landlock-gated: it proves the
/// escape WOULD happen absent confinement, regardless of kernel support.
#[test]
fn launcher_without_landlock_lets_the_escape_land() {
    let scratch = Scratch::new("novacuous");
    let escape = scratch.path("escape.txt");
    let marker = "ESCAPED-NOLANDLOCK-MARKER".to_string();
    let cmd = format!(
        "echo {marker} > {escape}",
        marker = marker,
        escape = escape.to_string_lossy(),
    );

    // The unconfined plan: scrub + exec, NO landlock action (Confinement NotRequired).
    let plan = unconfined_plan(vec!["sh".into(), "-c".into(), cmd]);
    let (mut child, control) = spawn_launcher(&plan, open_sh(), None);
    let transcript = read_all(control);
    let _ = child.wait();
    if transcript_confinement_unavailable(&transcript) {
        let mut sink = std::io::stderr();
        let _ = writeln!(
            sink,
            "SKIP launcher_without_landlock_lets_the_escape_land: kernel/container lacks \
             landlock/userns/seccomp (ENOSYS); the launcher faulted before exec — exercised on \
             capable kernels + the bvisor-linux CI lane"
        );
        return;
    }

    let gt = FsGroundTruth {
        marker,
        witness_path: escape,
    };
    // The escape DID land (no confinement) — this is what makes the G3 deny non-vacuous.
    assert!(
        gt.danger_occurred(),
        "NON-VACUOUS: an UNCONFINED launch must let the escape write land on disk \
         (so the confined G3 deny is meaningful). transcript:\n{transcript}"
    );
    // And the launcher honestly reports NO confinement install for this plan.
    assert!(
        transcript.contains("installed=false"),
        "an exec-only plan must report installed=false (no over-claim): {transcript}"
    );
}
