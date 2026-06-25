//! HOST-SIDE launcher-plan construction for the Linux backend (split out of
//! `backend_impl.rs` to keep each production file under the structural-check size cap).
//!
//! This module assembles the [`LinuxLaunchPlanV1`] + the pre-opened authority handles
//! the launcher inherits: the descriptor table (authority handles keyed to their slot fd
//! numbers), the lowering schedule the launcher SERVES (scrub + optional landlock-apply +
//! exec), and the launch body's identity binding (`h_l = blake3(canonical(lowering))`).
//! All of it is SAFE std (`File::open`) — authority rides an OWNED handle, never a
//! reopened path (CVE-2019-5736 / Leaky-Vessels class). The OS spawn/confinement lives in
//! the `sys` basement + the launcher itself; nothing here is `unsafe`.

use crate::backend::linux::launch::AuthorityFd;
use crate::backend::linux::protocol::{
    DescriptorKind, DescriptorRole, DescriptorShape, DescriptorSlotV1, LinuxLaunchBodyV1,
    LinuxLaunchPlanV1, LoweringWireEntryV1, LoweringWireV1, TargetSpecV1,
};
use crate::contract::capability::{FsAccess, PathSet};
use crate::contract::ids::{
    AdmissionProgramHash, AttemptId, BackendProfileHash, BoundaryPlanHash, Digest32,
};
use crate::contract::plan::BoundaryPlan;
use std::os::fd::{OwnedFd, RawFd};

/// System paths a confined workload needs READ+EXECUTE to run at all (the loader,
/// shared libraries, the binary's usual locations). These are granted READ-ONLY
/// (never write), IN ADDITION to the declared data roots — a workload must be able
/// to load its own image, but the confinement of its DATA access to the declared
/// roots is unaffected (these dirs hold no secret/quarantine target). Only dirs that
/// EXIST on the host are wired as ReadRoot slots (opening a missing dir would fail
/// the launch); the rest are skipped.
const SYSTEM_EXEC_ROOTS: &[&str] = &["/usr", "/lib", "/lib64", "/bin", "/sbin", "/etc"];

// ── Frozen launcher-wire constants (mirror `launcher/linux/imp.rs`) ────────────
// The backend MUST mirror exactly the primitive ids + phase codes the launcher
// SERVES, or the launcher refuses `MissingPrimitive`. These are the launcher's own
// frozen constants, restated here so the two sides agree without a shared module
// (the protocol carries the wire shapes, not these string/code literals).

/// The fd-scrub primitive (AmbientAuthority phase) — the launcher's MANDATORY
/// ambient-authority action. Every launch schedules exactly one.
const ID_AMBIENT_SCRUB: &str = "linux.ambient.scrub.v1";
/// The landlock-apply primitive (Confinement phase). Scheduled ONLY when the plan
/// carries a Filesystem capability; its absence ⇒ Confinement resolves NotRequired.
const ID_LANDLOCK_APPLY: &str = "linux.landlock.apply.v1";
/// The launch primitive (marks the `fexecve` step). Always scheduled.
const ID_EXEC: &str = "linux.exec.v1";
/// `LoweringPhase::FdHygiene.code()` — the scrub action's wire phase.
const PHASE_CODE_SCRUB: u8 = 3;
/// `LoweringPhase::PolicyInstall.code()` — the landlock-apply action's wire phase.
const PHASE_CODE_CONFINE: u8 = 4;
/// `LoweringPhase::Launch.code()` — the exec action's wire phase.
const PHASE_CODE_EXEC: u8 = 5;

// ── Descriptor-table slot fd numbers (slot_index == the fd the launcher reads) ──
// The launcher reads each authority handle at the fd number equal to its slot
// index, and the harness places its OWN channel fds (plan/control/error) strictly
// ABOVE every authority slot, so these fixed low numbers never collide. They must
// stay > 2 (the stdio floor the harness reserves) and dense enough to be distinct.

/// The target executable handle (`DescriptorRole::TargetExe`).
const SLOT_EXE: RawFd = 10;
/// The single declared read root (`DescriptorRole::ReadRoot`), when the access is
/// read-only. With a write grant the data root rides [`SLOT_WRITE_ROOT`] instead.
const SLOT_READ_ROOT: RawFd = 15;
/// The single declared write root (`DescriptorRole::WriteRoot`), when the access
/// grants writing.
const SLOT_WRITE_ROOT: RawFd = 16;
/// The cgroup leaf directory (`DescriptorRole::CgroupDir`), when the host created a
/// per-run leaf — the launcher births the child INTO it via `CLONE_INTO_CGROUP`.
const SLOT_CGROUP: RawFd = 17;
/// The base fd for the read-only system-exec roots (loader/libs), one per present
/// dir at `SLOT_SYS_ROOT_BASE + i`.
const SLOT_SYS_ROOT_BASE: RawFd = 20;

/// Everything the host built for one launcher run: the sealed-later plan, the
/// pre-opened authority handles keyed to their slot fds, the human-readable root
/// lists (honest evidence), and whether a landlock action was scheduled.
pub(super) struct Prepared {
    pub(super) launch_plan: LinuxLaunchPlanV1,
    pub(super) authority: Vec<AuthorityFd>,
    pub(super) read_roots: Vec<String>,
    pub(super) write_roots: Vec<String>,
    pub(super) confined: bool,
}

/// Build the [`LinuxLaunchPlanV1`] + pre-opened authority handles from the admitted
/// plan, host-side with SAFE std (`File::open`). Returns a human-readable error
/// string on any wiring fault (the caller fails closed). The descriptor table, the
/// lowering schedule, and the authority handles are all assembled here so the
/// launcher reads each handle at its declared slot fd number.
pub(super) fn prepare_launch(
    exe: &str,
    args: &[String],
    plan: &BoundaryPlan,
    fs: Option<&(FsAccess, PathSet)>,
    cgroup_dir_fd: Option<OwnedFd>,
    envp: Vec<(String, String)>,
) -> Result<Prepared, String> {
    let mut table: Vec<DescriptorSlotV1> = Vec::new();
    let mut authority: Vec<AuthorityFd> = Vec::new();
    let mut read_roots: Vec<String> = Vec::new();
    let mut write_roots: Vec<String> = Vec::new();

    // 1. The target executable rides a handle, never a path (exec is `fexecve` on
    //    the inherited fd in the launcher child).
    let exe_handle = open_handle(exe)?;
    authority.push(AuthorityFd {
        slot_index: SLOT_EXE,
        handle: exe_handle,
    });
    table.push(exe_slot());

    // 2. The declared data root (read-only, or read+write when the grant writes),
    //    plus the read-only system-exec roots so the workload image can load.
    let confined = fs.is_some();
    if let Some((access, scope)) = fs {
        let writable = matches!(access, FsAccess::Write | FsAccess::ReadWrite);
        for path in &scope.roots {
            let handle = open_handle(path)?;
            let (slot, role) = if writable {
                write_roots.push(path.clone());
                (SLOT_WRITE_ROOT, DescriptorRole::WriteRoot)
            } else {
                read_roots.push(path.clone());
                (SLOT_READ_ROOT, DescriptorRole::ReadRoot)
            };
            authority.push(AuthorityFd {
                slot_index: slot,
                handle,
            });
            table.push(root_slot(slot, role));
        }
        // System-exec roots: one ReadRoot slot per dir that EXISTS on the host.
        let mut sys_i: RawFd = 0;
        for sys_root in SYSTEM_EXEC_ROOTS {
            if !std::path::Path::new(sys_root).is_dir() {
                continue;
            }
            let handle = open_handle(sys_root)?;
            let slot = SLOT_SYS_ROOT_BASE
                .checked_add(sys_i)
                .ok_or_else(|| "system-exec root slot overflow".to_string())?;
            authority.push(AuthorityFd {
                slot_index: slot,
                handle,
            });
            table.push(root_slot(slot, DescriptorRole::ReadRoot));
            read_roots.push((*sys_root).to_string());
            sys_i += 1;
        }
    }

    // 2b. The cgroup leaf directory, when the host created a per-run leaf: the launcher
    //     resolves this singleton CgroupDir slot and births the workload child INSIDE the
    //     leaf via CLONE_INTO_CGROUP (no post-fork migration race). The fd is a
    //     non-writable directory (File::open is O_RDONLY); it is NOT a lowering action, so
    //     it does NOT enter the schedule / H_L — it is driven purely by the slot's
    //     presence.
    if let Some(fd) = cgroup_dir_fd {
        authority.push(AuthorityFd {
            slot_index: SLOT_CGROUP,
            handle: fd,
        });
        table.push(cgroup_slot());
    }

    // 3. The lowering schedule the launcher SERVES: scrub (mandatory) + landlock-apply
    //    (only when confining) + exec. Mirrors the launcher's served ids/phase codes
    //    exactly, else the launcher refuses MissingPrimitive. (If the BoundaryPlan
    //    later carries a real lowering schedule we project it; today the confinement
    //    model is exactly this minimal schedule — see the H_L note below.)
    let mut entries = vec![entry(ID_AMBIENT_SCRUB, PHASE_CODE_SCRUB)];
    if confined {
        entries.push(entry(ID_LANDLOCK_APPLY, PHASE_CODE_CONFINE));
    }
    entries.push(entry(ID_EXEC, PHASE_CODE_EXEC));
    let lowering = LoweringWireV1 { entries };

    let body = build_body(plan, lowering, table, exe, args, envp)?;
    Ok(Prepared {
        launch_plan: LinuxLaunchPlanV1 { body },
        authority,
        read_roots,
        write_roots,
        confined,
    })
}

/// Open a directory/file path as an owned handle with SAFE std (`File::open`). The
/// path is opened HOST-SIDE so authority rides the handle, never a reopened path
/// (CVE-2019-5736 / Leaky-Vessels class). A failure is a host wiring fault string.
fn open_handle(path: &str) -> Result<OwnedFd, String> {
    std::fs::File::open(path)
        .map(OwnedFd::from)
        .map_err(|e| format!("cannot open authority path {path}: {e}"))
}

/// The `DescriptorRole::TargetExe` slot declaration (a regular file, read-only —
/// exec rides the fd; the launcher `fstat`-checks the shape).
fn exe_slot() -> DescriptorSlotV1 {
    DescriptorSlotV1 {
        slot_index: slot_u32(SLOT_EXE),
        role: DescriptorRole::TargetExe,
        expected: DescriptorShape {
            kind: DescriptorKind::Regular,
            writable: false,
        },
    }
}

/// A confinement-root slot declaration. A directory fd is never writable per
/// `O_ACCMODE`, so the declared shape is `writable:false`; the landlock WRITE grant
/// is driven by the `role` (WriteRoot), NOT the fd's open mode.
fn root_slot(fd: RawFd, role: DescriptorRole) -> DescriptorSlotV1 {
    DescriptorSlotV1 {
        slot_index: slot_u32(fd),
        role,
        expected: DescriptorShape {
            kind: DescriptorKind::Directory,
            writable: false,
        },
    }
}

/// The `DescriptorRole::CgroupDir` slot declaration (a directory, read-only — the
/// launcher passes it to `clone3(CLONE_INTO_CGROUP)`; the kernel consumes it at fork).
fn cgroup_slot() -> DescriptorSlotV1 {
    DescriptorSlotV1 {
        slot_index: slot_u32(SLOT_CGROUP),
        role: DescriptorRole::CgroupDir,
        expected: DescriptorShape {
            kind: DescriptorKind::Directory,
            writable: false,
        },
    }
}

/// One projected lowering entry. The param/decl digests are zeroed: the REAL
/// schedule binding (param/decl-addressed entries + the authoritative `H_L`) is the
/// track-A reconciliation in #75; today the launcher binds only
/// `h_l == blake3(canonical(lowering))`, so the zeroed digests are honest for this
/// minimal confinement schedule and the launcher's served-id check is what matters.
fn entry(id: &str, phase_code: u8) -> LoweringWireEntryV1 {
    LoweringWireEntryV1 {
        id: id.to_owned(),
        version: 1,
        phase_code,
        param_digest: [0u8; 32],
        decl_digest: [0u8; 32],
    }
}

/// Assemble the launcher body. Identity binding:
/// - `plan_id` is the REAL admitted-plan identity;
/// - `h_p` is the honest digest of the plan's bound profile snapshot
///   ([`BackendProfileHash::of`] over its canonical bytes);
/// - `h_l = blake3(canonical(lowering))` — the launcher re-derives + compares this
///   exact binding (the real `H_L`/schedule reconciliation is #75; do NOT invent a
///   different binding here);
/// - `attempt_id`/`h_a` are derived deterministically from the plan identity (the
///   `BoundaryPlan` carries neither, and the launcher does NOT verify them — it
///   checks ONLY `h_l`; the real attempt/admission-program threading is #75). They
///   are domain-separated so they never collide with each other or with `plan_id`.
fn build_body(
    plan: &BoundaryPlan,
    lowering: LoweringWireV1,
    table: Vec<DescriptorSlotV1>,
    exe: &str,
    args: &[String],
    envp: Vec<(String, String)>,
) -> Result<LinuxLaunchBodyV1, String> {
    let lowering_bytes = batpak::canonical::to_bytes(&lowering)
        .map_err(|e| format!("cannot canonically encode the lowering schedule: {e}"))?;
    let h_l: Digest32 = batpak::event::hash::compute_hash(&lowering_bytes);

    let profile_bytes = batpak::canonical::to_bytes(&plan.profile)
        .map_err(|e| format!("cannot canonically encode the profile snapshot: {e}"))?;
    let h_p = BackendProfileHash::of(&profile_bytes);

    // argv[0] is the conventional program name; the rest are the workload args.
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(exe.to_string());
    argv.extend(args.iter().cloned());

    Ok(LinuxLaunchBodyV1 {
        attempt_id: AttemptId(derive_id(plan.plan_id, b"bvisor.attempt.v1")),
        plan_id: plan.plan_id,
        h_a: AdmissionProgramHash(derive_id(plan.plan_id, b"bvisor.h_a.v1")),
        h_p,
        h_l,
        lowering,
        descriptor_table: table,
        target: TargetSpecV1 {
            argv,
            // The target environment is EXACTLY the lowered Environment::Exact table
            // (literals + parent-resolved leases) — nothing inherited. No implicit
            // PATH: the spec DECLARES every variable it needs (proof-spine §5 D2 —
            // platform-generated entries must be explicit, never invisible).
            envp,
            exe_slot: slot_u32(SLOT_EXE),
            // The production plan-build path does NOT request a user namespace (S8 is
            // opt-in infrastructure; the netns wiring that uses it is S9). `None` keeps
            // the canonical bytes byte-for-byte identical to the pre-S8 wire form.
            user_namespace: None,
        },
    })
}

/// Derive a domain-separated digest from the plan identity. Used for the launch
/// identity fields the `BoundaryPlan` does not carry (`attempt_id`/`h_a`) so they
/// are deterministic + bound to plan identity yet never alias each other or
/// `plan_id`. The launcher does not verify these (it checks only `h_l`); the real
/// attempt/admission-program threading is #75.
fn derive_id(plan_id: BoundaryPlanHash, domain: &[u8]) -> Digest32 {
    let mut framed = Vec::with_capacity(domain.len() + 1 + 32);
    framed.extend_from_slice(domain);
    framed.push(0u8); // length-unambiguous separator (domain is NUL-free)
    framed.extend_from_slice(&plan_id.0);
    batpak::event::hash::compute_hash(&framed)
}

/// A slot fd number as the `u32` the descriptor table declares. The slot constants
/// are small positive literals, so the conversion cannot fail; on the impossible
/// negative it saturates to `u32::MAX` (a fd the launcher will fail to `fstat`,
/// fail-closed — never a silent wrong slot).
fn slot_u32(fd: RawFd) -> u32 {
    u32::try_from(fd).unwrap_or(u32::MAX)
}
