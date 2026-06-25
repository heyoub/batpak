//! SAFE host-side cgroup v2 manager for the Linux backend (kernel plan §10.8,
//! step 8a — resource confinement, HOST half).
//!
//! cgroup v2 is a FILESYSTEM interface (a `cgroup2` mount, typically at
//! `/sys/fs/cgroup`): creating a leaf is `mkdir`, setting a limit is writing a
//! decimal to `pids.max`/`memory.max`, reading membership is reading
//! `cgroup.procs`, and an atomic recursive teardown is writing `"1"` to
//! `cgroup.kill` (cgroup v2 ≥ 5.14). EVERYTHING here is therefore plain
//! `std::fs` — there is NO `unsafe` and there can be none: if a syscall feels
//! necessary, the abstraction is wrong. This module is consequently FULLY
//! runtime-shape-checked (not a `sys.rs` basement) and unit-testable against a
//! FAKE cgroup tree on any host without privileges.
//!
//! ## What this step (8a) builds, and what it deliberately does NOT
//! 8a is the HOST half only:
//!   - [`probe_cgroup_delegation`] — find a writable, delegated cgroup v2 base
//!     where the backend may create a leaf (honest `None` when there is none);
//!   - [`CgroupLeaf`] — create/configure a leaf, set `pids.max`/`memory.max`
//!     (ONLY for controllers actually delegated — an un-delegated limit is a
//!     typed error, never a silent no-op), read `cgroup.procs`, open the dir as
//!     an [`OwnedFd`] (for 8b's `CLONE_INTO_CGROUP` descriptor slot), and tear
//!     the leaf down atomically via `cgroup.kill` then `rmdir`.
//!
//! 8b (NOT here) adds the launcher's `clone3(CLONE_INTO_CGROUP)` placement of
//! the child INTO the prepared leaf, and the `profile()` Budget/Kill honesty
//! cells (`Enforced` ONLY when a real delegated controller backs them). NO
//! launcher change, NO `profile()`/ceiling change, NO `unsafe` lives here.
//!
//! ## systemd user-delegation expectation (the realistic deployment)
//! An unprivileged process cannot write the system-root cgroup. Under systemd a
//! user session gets a DELEGATED subtree
//! (`/sys/fs/cgroup/user.slice/user-<uid>.slice/user@<uid>.service/...`) whose
//! controllers systemd has enabled in `cgroup.subtree_control`; inside that
//! subtree the unprivileged user may freely `mkdir` leaves and write the
//! delegated controller files. [`probe_cgroup_delegation`] discovers that base
//! from the process's OWN cgroup (`/proc/self/cgroup`) and PROVES writability by
//! a create-then-remove round-trip of a probe subdirectory — it never assumes.
//!
//! ## HONESTY (the cardinal rule, feeding 8b's profile())
//! A limit the environment cannot actually back is NEVER silently treated as
//! set: [`CgroupLeaf::create`] returns the [`CgroupSetup`] record of which
//! controllers were delegated, and asking for a limit on an un-delegated
//! controller is an [`io::ErrorKind::Unsupported`] error. `cgroup.kill` on a
//! pre-5.14 kernel (file absent) is likewise a typed error — we NEVER pretend a
//! kill happened. This is what lets 8b mark Budget/Kill `Enforced` ONLY when a
//! real controller/kill is proven present.

use std::fs;
use std::io;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The conventional cgroup v2 mount point. A `cgroup.controllers` file directly
/// under this path is the marker of a unified (v2) hierarchy.
const CGROUP_V2_ROOT: &str = "/sys/fs/cgroup";

/// The filename whose presence marks a cgroup v2 directory (it lists the
/// controllers available to be enabled in this cgroup's subtree).
const CONTROLLERS_FILE: &str = "cgroup.controllers";

/// The file listing the controllers a cgroup has enabled for its CHILDREN
/// (space-separated). A child leaf can only set a controller's interface files
/// if that controller appears in its PARENT's `cgroup.subtree_control`.
const SUBTREE_CONTROL_FILE: &str = "cgroup.subtree_control";

/// The leaf's process-membership file (one pid per line).
const PROCS_FILE: &str = "cgroup.procs";

/// The atomic recursive-kill control file (cgroup v2 ≥ 5.14): writing `"1"`
/// SIGKILLs every process in the cgroup and its descendants atomically.
const KILL_FILE: &str = "cgroup.kill";

/// The pids-controller limit file (max number of pids in the cgroup subtree).
const PIDS_MAX_FILE: &str = "pids.max";

/// The memory-controller hard-limit file (max resident bytes before the cgroup
/// OOM-kills; `"max"` means unlimited).
const MEMORY_MAX_FILE: &str = "memory.max";

/// A monotonic counter making each probe subdirectory name unique WITHIN this
/// process even when the pid is reused across runs, without any wall clock or
/// RNG (neither is dependable in every embedding). Combined with the pid it
/// yields a collision-free probe-dir suffix.
static PROBE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The resource limits to install on a leaf. A `None` field means "do not set
/// this controller's limit" (leave the kernel default, typically `max`). A
/// `Some` value on a controller that is NOT delegated is a HARD error at
/// [`CgroupLeaf::create`] — never a silent no-op.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct CgroupLimits {
    /// `pids.max` — the maximum number of processes/threads in the leaf subtree.
    pub pids_max: Option<u64>,
    /// `memory.max` — the hard memory limit in bytes for the leaf subtree.
    pub memory_max: Option<u64>,
}

impl CgroupLimits {
    /// A limit set requesting `pids.max` only (the common confinement floor).
    #[must_use]
    pub fn with_pids_max(pids_max: u64) -> Self {
        Self {
            pids_max: Some(pids_max),
            memory_max: None,
        }
    }

    /// This same limit set with `memory.max` (bytes) added.
    #[must_use]
    pub fn and_memory_max(self, memory_max: u64) -> Self {
        Self {
            memory_max: Some(memory_max),
            ..self
        }
    }
}

/// The HONEST record of which controllers were actually delegated (and thus
/// whose limits were genuinely written) when a leaf was created. A limit a
/// caller requested on an UN-delegated controller never reaches this struct —
/// [`CgroupLeaf::create`] fails closed instead — so a `true` here means the
/// interface file was really written, the backing for 8b's `Budget=Enforced`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct CgroupSetup {
    /// The `pids` controller was delegated AND a requested `pids.max` was written.
    pub pids_enforced: bool,
    /// The `memory` controller was delegated AND a requested `memory.max` was written.
    pub memory_enforced: bool,
}

/// A created cgroup v2 leaf with kill-then-remove teardown. Dropping a leaf
/// best-effort removes the (empty) directory; a leaf with LIVE members cannot be
/// removed, so the correct teardown order is [`CgroupLeaf::kill`] (atomically
/// SIGKILL every member) THEN [`CgroupLeaf::remove`] (rmdir the now-empty leaf).
/// `Drop` performs only the rmdir — it does NOT kill, because a silent kill on
/// drop would hide a still-running workload; the caller kills explicitly.
#[derive(Debug)]
pub struct CgroupLeaf {
    /// The absolute path of the leaf directory under the delegated base.
    dir: PathBuf,
    /// Which controllers were genuinely delegated + written at create time.
    setup: CgroupSetup,
    /// Whether the leaf directory still exists (cleared by [`Self::remove`] so
    /// `Drop` does not double-remove / error on an already-removed leaf).
    present: bool,
}

impl CgroupLeaf {
    /// Create a leaf cgroup `name` directly under the delegated `base`, then set
    /// the requested limits — but ONLY for controllers `base` actually delegates
    /// to its children (read from `base/cgroup.subtree_control`). A limit
    /// requested on an UN-delegated controller is an [`io::ErrorKind::Unsupported`]
    /// error (the leaf is removed first, so no half-configured leaf leaks):
    /// pretending an unbacked limit was set would be the exact lie 8b's
    /// `profile()` must never tell.
    ///
    /// # Errors
    /// - the `mkdir` of the leaf (e.g. `base` not writable / leaf already exists);
    /// - a requested limit whose controller is NOT in `base`'s
    ///   `cgroup.subtree_control` (`Unsupported`);
    /// - the write of a delegated controller's interface file.
    pub fn create(base: &Path, name: &str, limits: CgroupLimits) -> io::Result<Self> {
        if name.is_empty() || name.contains('/') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cgroup leaf name must be a single non-empty path component",
            ));
        }
        let dir = base.join(name);
        fs::create_dir(&dir)?;

        // Build the leaf, then configure it. If configuration fails, remove the
        // freshly-created (empty) leaf so a failed create leaves NO half-leaf.
        let mut leaf = Self {
            dir,
            setup: CgroupSetup::default(),
            present: true,
        };
        match leaf.apply_limits(base, limits) {
            Ok(setup) => {
                leaf.setup = setup;
                Ok(leaf)
            }
            Err(e) => {
                // Best-effort cleanup of the empty leaf; surface the ORIGINAL error.
                let _ = fs::remove_dir(&leaf.dir);
                leaf.present = false;
                Err(e)
            }
        }
    }

    /// Write each requested limit, but ONLY for a controller the parent `base`
    /// delegates (present in `base/cgroup.subtree_control`). An un-delegated
    /// requested limit is a fail-closed `Unsupported` error.
    fn apply_limits(&self, base: &Path, limits: CgroupLimits) -> io::Result<CgroupSetup> {
        let delegated = read_subtree_control(base)?;
        // VALIDATE every requested controller's delegation BEFORE writing ANY
        // interface file, so an un-delegated limit fails the whole create with NO
        // partial write — the freshly-created leaf is still empty and the
        // error-path cleanup rmdir succeeds (a partial write would otherwise leave
        // a half-configured leaf, exactly the dishonest state this forbids).
        if limits.pids_max.is_some() {
            require_delegated(&delegated, "pids")?;
        }
        if limits.memory_max.is_some() {
            require_delegated(&delegated, "memory")?;
        }
        let mut setup = CgroupSetup::default();
        if let Some(pids_max) = limits.pids_max {
            write_limit(&self.dir, PIDS_MAX_FILE, pids_max)?;
            setup.pids_enforced = true;
        }
        if let Some(memory_max) = limits.memory_max {
            write_limit(&self.dir, MEMORY_MAX_FILE, memory_max)?;
            setup.memory_enforced = true;
        }
        Ok(setup)
    }

    /// The leaf directory path. `Err` (`NotFound`) once the leaf has been
    /// [`Self::remove`]d, so a stale handle cannot hand out a dangling path.
    ///
    /// # Errors
    /// [`io::ErrorKind::NotFound`] if the leaf was already removed.
    pub fn dir(&self) -> io::Result<&Path> {
        if self.present {
            Ok(&self.dir)
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "cgroup leaf has already been removed",
            ))
        }
    }

    /// Which controllers were genuinely delegated + had their limit written. The
    /// HONEST backing for 8b's `Budget=Enforced` cell.
    #[must_use]
    pub fn setup(&self) -> CgroupSetup {
        self.setup
    }

    /// Open the leaf DIRECTORY as a read-only [`OwnedFd`], for 8b's
    /// `clone3(CLONE_INTO_CGROUP)` — which takes a fd of the destination cgroup
    /// directory. `File::open` on a directory is SAFE std (it `open(2)`s with
    /// `O_RDONLY|O_CLOEXEC`); no `unsafe` is involved.
    ///
    /// # Errors
    /// Any `io::Error` from opening the leaf directory (e.g. already removed).
    pub fn dir_fd(&self) -> io::Result<OwnedFd> {
        let dir = self.dir()?;
        let file = fs::File::open(dir)?;
        Ok(OwnedFd::from(file))
    }

    /// Parse the leaf's `cgroup.procs` into the member pids (one pid per line).
    /// An empty/whitespace line is skipped; a non-numeric line is a typed
    /// `InvalidData` error (the kernel never writes one, so a malformed line
    /// means a corrupt read we must not silently drop).
    ///
    /// # Errors
    /// The read of `cgroup.procs`, or a non-numeric membership line.
    pub fn member_pids(&self) -> io::Result<Vec<i32>> {
        let path = self.dir()?.join(PROCS_FILE);
        let text = fs::read_to_string(&path)?;
        parse_procs(&text)
    }

    /// Atomically SIGKILL every process in the leaf and its descendants by
    /// writing `"1"` to `cgroup.kill` (cgroup v2 ≥ 5.14). This is the recursive,
    /// race-free teardown the launcher's child subtree needs.
    ///
    /// HONESTY: if `cgroup.kill` is ABSENT (a pre-5.14 kernel), this is a typed
    /// [`io::ErrorKind::Unsupported`] error — we do NOT silently pretend the
    /// subtree was killed. (8b marks `Kill=Enforced` ONLY when this file is
    /// present.) Killing does NOT remove the directory; call [`Self::remove`]
    /// AFTER kill (a leaf with live members cannot be rmdir'd).
    ///
    /// # Errors
    /// `Unsupported` if `cgroup.kill` is absent; otherwise the write error.
    pub fn kill(&self) -> io::Result<()> {
        let path = self.dir()?.join(KILL_FILE);
        if !path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "cgroup.kill absent (kernel < 5.14): cannot atomically kill the subtree",
            ));
        }
        fs::write(&path, b"1")
    }

    /// Remove the leaf directory (rmdir). MUST follow a successful [`Self::kill`]
    /// (and the members' actual exit) — the kernel refuses to remove a cgroup
    /// with live members (`EBUSY`), so kill-then-remove ordering is load-bearing.
    /// Idempotent: a second call (or a `Drop` after this) is a no-op.
    ///
    /// # Errors
    /// The `rmdir`, e.g. `EBUSY` if the leaf still has live members.
    pub fn remove(&mut self) -> io::Result<()> {
        if !self.present {
            return Ok(());
        }
        fs::remove_dir(&self.dir)?;
        self.present = false;
        Ok(())
    }
}

impl Drop for CgroupLeaf {
    fn drop(&mut self) {
        // Best-effort rmdir ONLY (no kill on drop — a silent kill would hide a
        // still-running workload; the caller kills explicitly). A leaf with live
        // members fails here with EBUSY, which we deliberately swallow: Drop must
        // not panic, and an un-removable leaf is the caller's kill-ordering bug to
        // surface via the explicit `kill`/`remove` Results, not a drop crash.
        if self.present {
            let _ = fs::remove_dir(&self.dir);
        }
    }
}

/// Probe for a writable, delegated cgroup v2 base where the backend may create a
/// leaf, returning it or an honest `None` (no v2 / no delegation / not writable).
///
/// Strategy (no assumptions — every step is verified):
///   1. confirm a unified (v2) hierarchy: `<root>/cgroup.controllers` exists;
///   2. read the process's OWN v2 cgroup from `/proc/self/cgroup` (the `0::<path>`
///      line — controller field empty marks the unified hierarchy) and map it to
///      a directory under `<root>`;
///   3. PROVE writability by a create-then-remove round-trip of a unique probe
///      subdirectory (`<base>/.bvisor-probe-<pid>-<counter>`): only a base where
///      that round-trips is returned. Under systemd this base is the user's
///      DELEGATED subtree
///      (`/sys/fs/cgroup/user.slice/user-<uid>.slice/user@<uid>.service/...`).
///
/// A non-writable own-cgroup falls back to PROBING the parent directory (systemd
/// often delegates the slice while the process sits in a managed leaf within it).
/// Returns `None` — an HONEST "no delegation" — when no candidate round-trips,
/// rather than guessing a base the backend cannot actually use.
#[must_use]
pub fn probe_cgroup_delegation() -> Option<PathBuf> {
    let root = Path::new(CGROUP_V2_ROOT);
    // (1) cgroup v2 present?
    if !root.join(CONTROLLERS_FILE).exists() {
        return None;
    }
    // (2) the process's own v2 cgroup directory.
    let own = own_v2_cgroup_dir(root)?;
    // (3) prove writability of the own dir, else its parent (delegated slice).
    if base_is_writable(&own) {
        return Some(own);
    }
    let parent = own.parent()?;
    if parent.starts_with(root) && base_is_writable(parent) {
        return Some(parent.to_path_buf());
    }
    None
}

/// Map the process's own unified-hierarchy cgroup (the `0::<path>` line of
/// `/proc/self/cgroup`) to its directory under `root`. `None` if the file is
/// unreadable or carries no v2 line.
fn own_v2_cgroup_dir(root: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string("/proc/self/cgroup").ok()?;
    let rel = own_v2_relative_path(&text)?;
    // The v2 line path is rooted at the hierarchy root ("/...") — strip the
    // leading slash and join under the mount so an absolute path never escapes it.
    let rel = rel.trim_start_matches('/');
    Some(root.join(rel))
}

/// Extract the `<path>` from the unified-hierarchy line of `/proc/self/cgroup`.
/// That line has the form `0::<path>` (hierarchy-id `0`, EMPTY controller list);
/// returns the `<path>` (e.g. `/user.slice/user-1000.slice/...`). `None` if no
/// such line is present (a v1-only or hybrid layout without a v2 line).
fn own_v2_relative_path(proc_cgroup: &str) -> Option<String> {
    for line in proc_cgroup.lines() {
        // Format: `hierarchy-ID:controller-list:cgroup-path`. The v2 line is the
        // one whose hierarchy-ID is `0` AND whose controller-list is empty.
        let mut parts = line.splitn(3, ':');
        let hierarchy = parts.next()?;
        let controllers = parts.next()?;
        let path = parts.next()?;
        if hierarchy == "0" && controllers.is_empty() {
            return Some(path.to_string());
        }
    }
    None
}

/// Prove `base` is writable by creating then immediately removing a unique probe
/// subdirectory. Round-trip success ⇒ the backend may create leaves here. Any
/// failure (create or remove) ⇒ `false` (no delegation here). The probe dir is
/// always cleaned up on the success path; a create that succeeds but whose
/// remove fails still reports `false` (we could not fully round-trip).
fn base_is_writable(base: &Path) -> bool {
    let suffix = PROBE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let probe = base.join(format!(".bvisor-probe-{pid}-{suffix}"));
    match fs::create_dir(&probe) {
        Ok(()) => fs::remove_dir(&probe).is_ok(),
        Err(_) => false,
    }
}

/// Read the parent's `cgroup.subtree_control` (the controllers it delegates to
/// children) into a list of controller names. A missing file yields an EMPTY set
/// (nothing delegated) rather than an error, so the honest "no controller" path
/// is `require_delegated` failing the specific limit — not a create-time read
/// crash.
fn read_subtree_control(base: &Path) -> io::Result<Vec<String>> {
    let path = base.join(SUBTREE_CONTROL_FILE);
    match fs::read_to_string(&path) {
        Ok(text) => Ok(text.split_whitespace().map(str::to_string).collect()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// Fail closed unless `controller` is in the delegated set: a limit on an
/// un-delegated controller can NOT be enforced, so it is an `Unsupported` error
/// (never a silent no-op the profile would then over-claim).
fn require_delegated(delegated: &[String], controller: &str) -> io::Result<()> {
    if delegated.iter().any(|c| c == controller) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "the `{controller}` controller is not delegated (absent from \
                 cgroup.subtree_control); refusing to claim an unenforceable limit"
            ),
        ))
    }
}

/// Write a single decimal limit value to a controller interface file in `dir`.
fn write_limit(dir: &Path, file: &str, value: u64) -> io::Result<()> {
    fs::write(dir.join(file), value.to_string().as_bytes())
}

/// Parse a `cgroup.procs` body (one pid per line) into pids. Blank lines are
/// skipped; a non-numeric line is an `InvalidData` error (the kernel never emits
/// one, so it signals a corrupt read we must not silently drop).
fn parse_procs(text: &str) -> io::Result<Vec<i32>> {
    let mut pids = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let pid = line.parse::<i32>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("cgroup.procs contained a non-numeric line: {line:?}"),
            )
        })?;
        pids.push(pid);
    }
    Ok(pids)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a FAKE cgroup v2 tree under a tempdir: a `base` dir whose
    /// `cgroup.subtree_control` delegates `delegated`, containing an EMPTY leaf
    /// dir (no interface files yet — `create` writes them). Returns
    /// `(tempdir, base_path)`; the tempdir is kept alive by the caller.
    fn fake_base(delegated: &str) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path().join("base");
        fs::create_dir(&base).expect("base dir");
        fs::write(base.join(CONTROLLERS_FILE), delegated).expect("controllers");
        fs::write(base.join(SUBTREE_CONTROL_FILE), delegated).expect("subtree_control");
        (tmp, base)
    }

    /// Pre-seed a leaf's kill+procs interface files (the kernel auto-creates
    /// these; the fake tree must materialise them for the write/read assertions).
    fn seed_leaf_interface(dir: &Path) {
        fs::write(dir.join(PROCS_FILE), "").expect("procs");
        fs::write(dir.join(KILL_FILE), "").expect("kill");
    }

    #[test]
    fn create_writes_pids_max_to_the_interface_file() {
        let (_tmp, base) = fake_base("pids memory");
        let leaf = CgroupLeaf::create(&base, "leaf", CgroupLimits::with_pids_max(64))
            .expect("create leaf");
        let written = fs::read_to_string(leaf.dir().expect("dir").join(PIDS_MAX_FILE))
            .expect("read pids.max");
        assert_eq!(
            written, "64",
            "pids.max must hold exactly the requested limit"
        );
        assert!(leaf.setup().pids_enforced, "pids was delegated ⇒ enforced");
        assert!(!leaf.setup().memory_enforced, "memory was not requested");
    }

    #[test]
    fn create_writes_memory_max_when_requested_and_delegated() {
        let (_tmp, base) = fake_base("pids memory");
        let limits = CgroupLimits::with_pids_max(8).and_memory_max(1_048_576);
        let leaf = CgroupLeaf::create(&base, "leaf", limits).expect("create leaf");
        let mem = fs::read_to_string(leaf.dir().expect("dir").join(MEMORY_MAX_FILE))
            .expect("read memory.max");
        assert_eq!(
            mem, "1048576",
            "memory.max must hold exactly the requested bytes"
        );
        assert!(
            leaf.setup().memory_enforced,
            "memory was delegated ⇒ enforced"
        );
    }

    #[test]
    fn limit_on_undelegated_controller_is_an_honest_error_not_silent() {
        // `base` delegates ONLY pids; requesting a memory limit must fail closed —
        // never silently leave memory unbounded while the caller believes it set.
        let (_tmp, base) = fake_base("pids");
        let limits = CgroupLimits::with_pids_max(4).and_memory_max(4096);
        let err = CgroupLeaf::create(&base, "leaf", limits)
            .expect_err("undelegated memory limit must error");
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        // The failed create must leave NO half-configured leaf behind.
        assert!(
            !base.join("leaf").exists(),
            "a failed create must remove the half-leaf"
        );
    }

    #[test]
    fn member_pids_parses_multiline_procs_into_a_vec() {
        let (_tmp, base) = fake_base("pids");
        let leaf = CgroupLeaf::create(&base, "leaf", CgroupLimits::with_pids_max(16))
            .expect("create leaf");
        // The kernel writes one pid per line; create did not seed procs, so write it.
        fs::write(leaf.dir().expect("dir").join(PROCS_FILE), "101\n202\n303\n")
            .expect("seed procs");
        let pids = leaf.member_pids().expect("member pids");
        assert_eq!(pids, vec![101, 202, 303]);
    }

    #[test]
    fn member_pids_on_empty_procs_is_empty() {
        let (_tmp, base) = fake_base("pids");
        let leaf =
            CgroupLeaf::create(&base, "leaf", CgroupLimits::with_pids_max(2)).expect("create leaf");
        fs::write(leaf.dir().expect("dir").join(PROCS_FILE), "").expect("empty procs");
        assert!(leaf.member_pids().expect("member pids").is_empty());
    }

    #[test]
    fn member_pids_rejects_a_nonnumeric_line() {
        let (_tmp, base) = fake_base("pids");
        let leaf =
            CgroupLeaf::create(&base, "leaf", CgroupLimits::with_pids_max(2)).expect("create leaf");
        fs::write(leaf.dir().expect("dir").join(PROCS_FILE), "101\nnope\n")
            .expect("seed bad procs");
        let err = leaf.member_pids().expect_err("non-numeric line must error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn kill_writes_one_to_the_kill_file_when_present() {
        let (_tmp, base) = fake_base("pids");
        let leaf =
            CgroupLeaf::create(&base, "leaf", CgroupLimits::with_pids_max(2)).expect("create leaf");
        seed_leaf_interface(leaf.dir().expect("dir"));
        leaf.kill().expect("kill writes 1");
        let killed =
            fs::read_to_string(leaf.dir().expect("dir").join(KILL_FILE)).expect("read kill");
        assert_eq!(killed, "1", "cgroup.kill must receive exactly \"1\"");
    }

    #[test]
    fn kill_without_kill_file_is_an_honest_unsupported_error() {
        // A pre-5.14 fake (no cgroup.kill) must NOT pretend it killed.
        let (_tmp, base) = fake_base("pids");
        let leaf =
            CgroupLeaf::create(&base, "leaf", CgroupLimits::with_pids_max(2)).expect("create leaf");
        // Deliberately do NOT seed cgroup.kill.
        let err = leaf.kill().expect_err("absent cgroup.kill must error");
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn dir_fd_opens_the_leaf_directory() {
        let (_tmp, base) = fake_base("pids");
        let leaf =
            CgroupLeaf::create(&base, "leaf", CgroupLimits::with_pids_max(2)).expect("create leaf");
        // Opening the leaf dir as an OwnedFd is the 8b CLONE_INTO_CGROUP handle.
        let fd = leaf.dir_fd().expect("open leaf dir fd");
        // A directory fd is non-negative and owned (closed on drop) — just prove
        // it materialised.
        use std::os::fd::AsRawFd;
        assert!(fd.as_raw_fd() >= 0);
    }

    #[test]
    fn remove_then_drop_does_not_double_remove() {
        // A leaf with NO limits, so the fake leaf dir is empty — mirroring a REAL
        // cgroup leaf, whose kernel interface files (pids.max, cgroup.kill, …) do
        // NOT block `rmdir` of a member-less leaf (verified on the live delegated
        // box: `mkdir` a leaf, then `rmdir` succeeds despite its interface files).
        // The fake tree can't reproduce kernel pseudo-files that rmdir ignores, so
        // it uses a limit-free (genuinely empty) leaf to exercise the same rmdir.
        let (_tmp, base) = fake_base("pids");
        let mut leaf =
            CgroupLeaf::create(&base, "leaf", CgroupLimits::default()).expect("create leaf");
        let dir = leaf.dir().expect("dir").to_path_buf();
        leaf.remove().expect("explicit remove");
        assert!(!dir.exists(), "remove rmdir'd the leaf");
        // dir() now errors (leaf gone) and Drop is a no-op (no panic / double-rmdir).
        assert_eq!(
            leaf.dir().expect_err("dir after remove").kind(),
            io::ErrorKind::NotFound
        );
        drop(leaf);
    }

    #[test]
    fn invalid_leaf_name_is_rejected() {
        let (_tmp, base) = fake_base("pids");
        for bad in ["", "a/b"] {
            let err = CgroupLeaf::create(&base, bad, CgroupLimits::default())
                .expect_err("invalid name must error");
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn parse_v2_relative_path_picks_the_unified_line() {
        // A hybrid /proc/self/cgroup: v1 controller lines + the v2 `0::` line.
        let sample = "12:pids:/user.slice\n0::/user.slice/user-1000.slice/session.scope\n";
        assert_eq!(
            own_v2_relative_path(sample).as_deref(),
            Some("/user.slice/user-1000.slice/session.scope")
        );
    }

    #[test]
    fn parse_v2_relative_path_none_when_no_unified_line() {
        // A v1-only layout has no `0::` line ⇒ no v2 path.
        assert_eq!(own_v2_relative_path("3:cpu:/foo\n2:memory:/bar\n"), None);
    }

    #[test]
    fn require_delegated_distinguishes_present_from_absent() {
        let delegated = vec!["pids".to_string(), "memory".to_string()];
        assert!(require_delegated(&delegated, "pids").is_ok());
        assert_eq!(
            require_delegated(&delegated, "io")
                .expect_err("io not delegated")
                .kind(),
            io::ErrorKind::Unsupported
        );
    }
}
