// REAL landlock filesystem confinement on the live Linux backend, wired into the
// G-grid as G1 (secret-read-denied) and G3 (write-only-in-quarantine). Compiles
// only with BOTH the real Linux backend and the dangerous-test-hooks harness.
#![cfg(all(
    feature = "backend-linux",
    feature = "dangerous-test-hooks",
    target_os = "linux"
))]
//! GAUNTLET bvisor — REAL landlock FS confinement THROUGH `execute()`→the host-side
//! LAUNCHER (backend→launcher rewire step 7b), lie-caught by an INDEPENDENT
//! GroundTruth.
//!
//! THE BACKEND NEVER GRADES ITSELF. The harness owns a [`FsGroundTruth`] that
//! determines what ACTUALLY happened by observing the REAL filesystem on disk —
//! NEVER by trusting the backend's self-reported [`BoundaryReportBody`]. The two
//! are diffed; a divergence is a lie. INVERSION RULE: a backend may DENY MORE than
//! asked (landlock blocking an access the test never recorded is fine), but it may
//! NEVER REPORT LESS DANGER THAN OCCURRED (a leak/escape the report hides).
//!
//! WHAT CHANGED AT 7b: `execute()` no longer self-spawns + confines via a backend
//! `pre_exec`. It builds a launch plan and runs the `bvisor-linux-launcher`, which
//! applies the landlock ruleset in its single-threaded child window
//! (`restrict_self`, after the fd scrub, before `fexecve`). So:
//!   - the SAFETY verdict is STILL the independent on-disk oracle (the secret did
//!     not leak / the escape did not land);
//!   - the launcher does NOT capture the workload's stdout/stderr back to the
//!     backend (the workload inherits the launcher's stdio — captured-stdio slot
//!     wiring is a later step), so the report no longer carries a stderr-derived
//!     `denied`. A held confinement is an HONEST `Outcome::Completed` with the
//!     launcher's `confinement_installed` mechanism attestation; the DENIAL is
//!     proven by the disk (no danger to report because confinement held).
//!
//! G1 (secret-read-denied): the workload `cat`s a secret OUTSIDE the declared root
//! and redirects into a file INSIDE the (writable) root. If landlock blocks the
//! READ, the exfil file is empty / never gets the secret bytes. GroundTruth reads
//! that exfil file on disk: secret bytes present ⇒ the read leaked.
//!
//! G3 (write-only-in-quarantine): the workload writes a file OUTSIDE the write
//! root. GroundTruth stats the REAL disk: the escape file existing ⇒ the write
//! escaped quarantine.
//!
//! LIVE-ABI GATE: the confinement assertions run ONLY at/above the landlock floor;
//! below it (kernel lacks landlock, or the sandbox blocks it) they are SKIPPED with
//! an explicit message — never silently passed.
//!
//! RED FIXTURE (`--cfg gauntlet_red_fixture`): runs the SAME G3 workload UNCONFINED
//! (the backend lying that it confined), then asserts GroundTruth is clean — which
//! is FALSE because the escape really landed, so the red half FAILS, proving the
//! oracle is anti-vacuous.

use bvisor::{
    Backend, BackendId, BackendRegistry, BoundaryPlanner, BoundaryReportBody, BoundaryRunner,
    BoundarySpec, BudgetRequirements, Capability, EvidenceRequirements, FsAccess, FsConfinement,
    HostControl, LinuxBackend, MinGuarantee, Outcome, PathSet, StdStreams, Workload,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The test-compiled launcher bin `execute()` must run. `execute()` resolves the
/// launcher via the backend's INJECTED path (constructor injection — see
/// [`LinuxBackend::with_launcher_path`]) FIRST, so each test injects this path rather
/// than mutating the process environment (`std::env::set_var` is banned as thread-unsafe,
/// BANNED-003; concurrent tests in this binary would race it).
fn test_launcher_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_bvisor-linux-launcher"))
}

/// Probe the LIVE landlock ABI exactly as the backend/launcher do (`>=1`, or `0`
/// when unavailable). The G1/G3 confinement assertions run ONLY at/above the floor;
/// otherwise the test SKIPS them with an explicit message (never a silent pass).
const LANDLOCK_ABI_FLOOR: i64 = 1;

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

fn landlock_available() -> bool {
    live_landlock_abi() >= LANDLOCK_ABI_FLOOR
}

/// Emit an explicit skip line (never a silent pass) and return.
fn skip(test: &str) {
    use std::io::Write;
    let mut sink = std::io::stderr();
    let _ = writeln!(
        sink,
        "SKIP {test}: live landlock ABI {} < floor {LANDLOCK_ABI_FLOOR} \
         (kernel lacks landlock or the sandbox blocks it)",
        live_landlock_abi()
    );
}

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
        let root = std::env::temp_dir().join(format!("bvisor-fs-{tag}-{pid}-{nanos}"));
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

/// The INDEPENDENT oracle: it reconstructs what actually happened by reading the
/// REAL disk, never the backend's report.
struct FsGroundTruth {
    /// A marker that, if present in `witness_path` on disk, proves the dangerous
    /// effect actually occurred (a leaked secret for G1, an escape write for G3).
    marker: String,
    /// The real on-disk path the oracle stats to decide whether the effect landed.
    witness_path: PathBuf,
}

impl FsGroundTruth {
    /// Whether the dangerous effect ACTUALLY landed on disk — read independently of
    /// any report. `true` = the confinement FAILED (or never ran).
    fn danger_occurred(&self) -> bool {
        match std::fs::read(&self.witness_path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).contains(&self.marker),
            Err(_) => false,
        }
    }
}

/// A budgeted FS spec the LinuxBackend admits: a Filesystem capability confined to
/// `root`, plus launch + stdout/stderr capture. Budgets are uniform/Mediated with
/// no evidence demand (matching the backend's honest observed-not-capped profile).
fn fs_spec(args: Vec<String>, access: FsAccess, root: &Path) -> BoundarySpec {
    BoundarySpec {
        workload: Workload::Process {
            exe: "/bin/sh".to_string(),
            args,
        },
        capabilities: vec![Capability::Filesystem {
            access,
            scope: PathSet {
                roots: vec![root.to_string_lossy().into_owned()],
            },
            recursive: true,
            confinement: FsConfinement::DeclaredRootsOnly,
        }],
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

/// Run the spec through the registered LinuxBackend (whose `execute()` now drives
/// the launcher), returning the sealed body.
fn run(spec: &BoundarySpec) -> BoundaryReportBody {
    let backend = Arc::new(LinuxBackend::with_launcher_path(test_launcher_path()));
    let id: BackendId = backend.id();
    let mut registry = BackendRegistry::new();
    registry.register(Arc::clone(&backend) as Arc<dyn Backend>);

    let plan = BoundaryPlanner::new(&registry)
        .plan(spec, &id)
        .expect("LinuxBackend admits a declared-roots FS spec");
    BoundaryRunner::new(&registry)
        .run(&plan)
        .expect("the confined run seals a terminal report")
        .body
}

/// Whether the report HONESTLY records that the launcher INSTALLED the landlock
/// confinement — the new honest confinement evidence (the launcher applies the
/// ruleset, so the backend's mechanism attestation reflects the launcher install).
/// The `filesystem_confined` fact names the launcher mechanism; the `launcher_note`
/// facts carry the launcher's own `confinement=Applied installed=true` attestation.
fn report_records_confinement_installed(body: &BoundaryReportBody) -> bool {
    let named = body
        .observed
        .iter()
        .any(|f| f.kind == "filesystem_confined" && f.detail.contains("launcher restrict_self"));
    let attested = body
        .observed
        .iter()
        .any(|f| f.kind == "launcher_note" && f.detail.contains("installed=true"));
    let not_under_claimed = !body
        .observed
        .iter()
        .any(|f| f.kind == "confinement_not_installed");
    named && attested && not_under_claimed
}

#[test]
fn g1_landlock_denies_secret_read_outside_declared_root() {
    if !landlock_available() {
        skip("g1");
        return;
    }
    let scratch = Scratch::new("g1");
    // Declared root: the writable quarantine the workload MAY read+write.
    let root = scratch.path("quarantine");
    std::fs::create_dir_all(&root).expect("quarantine dir");
    // The secret lives OUTSIDE the declared root.
    let secret = scratch.path("secret.key");
    let marker = "TOPSECRET-G1-MARKER".to_string();
    std::fs::write(&secret, &marker).expect("plant secret");
    // The exfil sink is INSIDE the root: if the READ is allowed, the secret bytes
    // land here; if landlock blocks the read, this stays empty.
    let exfil = root.join("exfil.txt");

    // Read the secret, redirect into the in-root sink. ReadWrite so the in-root
    // write is legal; the out-of-root READ is what landlock must block.
    let cmd = format!(
        "cat {secret} > {exfil}",
        secret = secret.to_string_lossy(),
        exfil = exfil.to_string_lossy()
    );
    let body = run(&fs_spec(vec!["-c".into(), cmd], FsAccess::ReadWrite, &root));

    let gt = FsGroundTruth {
        marker,
        witness_path: exfil,
    };
    assert!(
        !gt.danger_occurred(),
        "GROUNDTRUTH: landlock (launcher restrict_self) must block the out-of-root \
         secret read; the secret leaked into the in-root exfil sink on disk. report:\n{:?}",
        body.observed
    );
    // Inversion rule: confinement HELD ⇒ no danger to report, and the report must
    // HONESTLY attest the launcher installed the confinement (not under-claim it).
    assert!(
        report_records_confinement_installed(&body),
        "report must HONESTLY attest the launcher installed the confinement it ran \
         under: {:?}",
        body.observed
    );
    // The workload exec'd under confinement ⇒ the honest 7b terminal is Completed
    // (the launcher reports its setup terminal, not the workload's own exit code).
    assert_eq!(
        body.outcome,
        Outcome::Completed,
        "a confined run that exec'd is an honest Completed: {:?}",
        body.observed
    );
}

#[test]
fn g3_landlock_denies_write_outside_quarantine() {
    if !landlock_available() {
        skip("g3");
        return;
    }
    let scratch = Scratch::new("g3");
    // Declared WRITE root: the quarantine the workload MAY write into.
    let root = scratch.path("quarantine");
    std::fs::create_dir_all(&root).expect("quarantine dir");
    // The escape target is OUTSIDE the write root.
    let escape = scratch.path("escape.txt");
    let marker = "ESCAPED-G3-MARKER".to_string();
    let cmd = format!(
        "echo {marker} > {escape}",
        escape = escape.to_string_lossy()
    );

    let body = run(&fs_spec(vec!["-c".into(), cmd], FsAccess::Write, &root));

    let gt = FsGroundTruth {
        marker,
        witness_path: escape,
    };
    assert!(
        !gt.danger_occurred(),
        "GROUNDTRUTH: landlock (launcher restrict_self) must block the \
         out-of-quarantine write; the escape file exists on disk. report:\n{:?}",
        body.observed
    );
    assert!(
        report_records_confinement_installed(&body),
        "report must HONESTLY attest the launcher installed the confinement: {:?}",
        body.observed
    );
}

/// CONTROL: an IN-root write succeeds and is honestly reported as Completed — the
/// confinement denies MORE than asked is fine, but it must not deny what is allowed
/// (no false positive that would make the deny-tests vacuous).
#[test]
fn control_in_root_write_is_allowed_and_completes() {
    if !landlock_available() {
        skip("control");
        return;
    }
    let scratch = Scratch::new("ctl");
    let root = scratch.path("quarantine");
    std::fs::create_dir_all(&root).expect("quarantine dir");
    let inside = root.join("allowed.txt");
    let marker = "ALLOWED-MARKER".to_string();
    let cmd = format!(
        "echo {marker} > {inside}",
        inside = inside.to_string_lossy()
    );

    let body = run(&fs_spec(vec!["-c".into(), cmd], FsAccess::Write, &root));

    // GroundTruth: the in-root write DID land (proving the sandbox is not a blanket
    // deny — the deny-tests above are therefore non-vacuous).
    let landed = std::fs::read(&inside)
        .map(|b| String::from_utf8_lossy(&b).contains(&marker))
        .unwrap_or(false);
    assert!(
        landed,
        "an in-root write must be allowed through landlock (launcher restrict_self). \
         report:\n{:?}",
        body.observed
    );
    assert_eq!(
        body.outcome,
        Outcome::Completed,
        "an allowed in-root write completes cleanly: {:?}",
        body.observed
    );
    assert!(
        report_records_confinement_installed(&body),
        "the launcher installed the confinement the allowed write ran under: {:?}",
        body.observed
    );
}

/// RED FIXTURE: simulate a LYING backend that runs the SAME G3 workload UNCONFINED
/// (claiming confinement it did not apply). GroundTruth then sees the escape land,
/// so the "clean" assertion is FALSE and the red half FAILS — proving the oracle
/// catches a lying backend, not just a cooperative one.
#[cfg(gauntlet_red_fixture)]
#[test]
fn g3_red_fixture_unconfined_backend_is_caught() {
    let scratch = Scratch::new("g3-red");
    let escape = scratch.path("escape.txt");
    let marker = "ESCAPED-RED-MARKER".to_string();
    let cmd = format!(
        "echo {marker} > {escape}",
        escape = escape.to_string_lossy()
    );
    // The LYING backend: spawn the workload with NO confinement (NOT through the
    // launcher path — a backend that skipped confinement entirely).
    let _ = std::process::Command::new("/bin/sh")
        .args(["-c", &cmd])
        .output()
        .expect("unconfined spawn");

    let gt = FsGroundTruth {
        marker,
        witness_path: escape,
    };
    // A biting oracle SEES the escape landed; this assertion is false and the red
    // half FAILS — exactly what proves the gate is anti-vacuous.
    assert!(
        !gt.danger_occurred(),
        "RED FIXTURE: an unconfined backend lets the write ESCAPE; GroundTruth \
         catches it, so this assertion must fail"
    );
}
