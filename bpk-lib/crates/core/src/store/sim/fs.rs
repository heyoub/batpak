//! Fault-injecting, real-file-backed filesystem seam for the real-`Store`
//! crash-recovery simulation.
//!
//! [`SimFs`] is the production [`StoreFs`] seam wired so a deterministic
//! simulation can compose the REAL [`crate::store::Store`] over it: every op
//! operates on REAL files under the store's data directory (so a reopened store
//! cold-starts over exactly the bytes left behind), but the durability seam
//! ([`StoreFs::sync_file_with_mode`] / [`StoreFs::sync_file_all`]) is interposed:
//!
//!   * On each fsync, SimFs consults a seeded PRNG ([`fastrand`]) once. Most
//!     fsyncs are **honored**: the file's current real length becomes its durable
//!     length. Under the seed's schedule an fsync is **dropped**: the call still
//!     returns `Ok` to the store (a silently-lying disk), but the durable length
//!     is NOT advanced, so the most recent bytes are lost on the next crash.
//!   * [`StoreFs::crash`] truncates every tracked real file to its last durable
//!     length, discarding the write-but-unsynced (and fsync-dropped) tail. This
//!     models power loss losing the OS page-cache tail.
//!
//! Reopening a real [`crate::store::Store`] over the same data directory after a
//! [`StoreFs::crash`] then exercises the genuine cold-start recovery path over
//! the truncated files. The model-only determinism witness (no real `Store`)
//! lives in [`super::fault_model::InMemFaultFs`].
//!
//! Determinism: every fsync-drop decision is a single draw from one seeded
//! [`fastrand::Rng`], advanced in the order the store reaches its fsync seam.
//! Same seed ⇒ same drop schedule ⇒ same durable prefix ⇒ same recovered state.

use crate::store::platform::fs::{PositionedReadError, StoreFs};
use crate::store::{StoreError, SyncMode};
use std::collections::BTreeMap;
use std::fs::{File, ReadDir};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Per-file durability bookkeeping: the byte length that has survived an fsync.
#[derive(Default, Clone, Copy)]
struct DurableState {
    /// Length (bytes) of the file prefix that an honored fsync has made durable.
    durable_len: u64,
}

/// Deterministic, fault-injecting filesystem over real files.
///
/// State lives behind [`Mutex`]es so the type is legitimately `Send + Sync`
/// (required by the [`StoreFs`] supertrait) without any `unsafe`; the simulation
/// drives the store request/response per op, so the locks are effectively
/// uncontended.
pub(crate) struct SimFs {
    /// Seeded PRNG; advanced once per fsync to decide honor-vs-drop.
    rng: Mutex<fastrand::Rng>,
    /// Durable-length table keyed by the real file path. Only files created
    /// through [`SimFs::create_new_file`] are tracked (the segment + data files
    /// whose torn tail a crash must discard).
    durable: Mutex<BTreeMap<PathBuf, DurableState>>,
    /// 1-in-N fsync-drop rate. A value of `0` disables drops entirely (every
    /// fsync is honored), so the crash boundary is purely the unsynced tail.
    fsync_drop_one_in: u32,
    /// ENOSPC injection schedule for file-materialization ops
    /// ([`StoreFs::cow_copy_file`] / [`StoreFs::copy`]). `None` disables it.
    /// When `Some`, the materialize counter is advanced once per such op and
    /// the op whose 1-based index EQUALS the threshold fails with `ENOSPC`
    /// (`io::ErrorKind::StorageFull`) — modelling a disk that fills mid-fork.
    /// Deterministic: the same threshold fails the same op every run.
    enospc_on_copy: Mutex<EnospcSchedule>,
    /// Deterministic atomic-op fault schedule for the W3-routed crash-sensitive
    /// ops ([`StoreFs::rename`] / [`StoreFs::remove_file`] /
    /// [`StoreFs::persist_temp_with_parent_sync`]). `None`/unarmed disables it.
    op_fault: Mutex<OpFaultSchedule>,
    /// Deterministic positioned-read fault schedule for the routed
    /// [`StoreFs::read_exact_at`] primitive (the active-segment frame read).
    /// DISTINCT from the [`CrashOp`] atomic-op schedule above: a read is not an
    /// atomic-rename / persist, so it carries its own targeted-Nth counter and
    /// its own fault taxonomy ([`ReadFaultKind`], D1 = model BOTH a hard I/O
    /// error and a short read). `None`/unarmed disables it. Test-only: the
    /// production build never faults a read, so the whole read-fault subsystem is
    /// compiled out of the non-test `Store`-over-`SimFs` fixtures.
    #[cfg(test)]
    read_fault: Mutex<ReadFaultSchedule>,
}

/// ENOSPC-mid-copy injection bookkeeping. `fail_at` is the 1-based
/// materialize-op index that fails; `seen` counts materialize ops reached.
#[derive(Default)]
struct EnospcSchedule {
    fail_at: Option<u32>,
    seen: u32,
}

/// A crash-sensitive [`StoreFs`] op a SimFs schedule can fault. These are the
/// W3-routed atomic-rename / persist primitives the compaction swap/rollback,
/// the visibility-range persist, and the cursor-checkpoint persist now reach
/// through the seam — so a seeded schedule can tear them where, as free fns,
/// they were unfaultable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CrashOp {
    /// [`StoreFs::rename`] — the compaction relocate/rollback swap point.
    Rename,
    /// [`StoreFs::remove_file`] (and the provided `remove_file_if_present`) —
    /// the post-swap segment reclaim.
    RemoveFile,
    /// [`StoreFs::persist_temp_with_parent_sync`] — the visibility-range and
    /// cursor-checkpoint atomic publish point.
    PersistTemp,
}

/// Deterministic atomic-op fault bookkeeping. `target` names the op kind and the
/// 1-based occurrence of THAT kind to fail; `seen` counts occurrences of the
/// targeted kind reached so far. The same target fails the same op every run.
#[derive(Default)]
struct OpFaultSchedule {
    target: Option<(CrashOp, u32)>,
    seen: u32,
}

/// How a scheduled [`StoreFs::read_exact_at`] fault manifests (DECISION D1 =
/// support BOTH). These map onto the two failure shapes the reader's
/// active-frame read already distinguishes:
///
///   * [`ReadFaultKind::Io`] — a hard positioned-read error
///     ([`PositionedReadError::Io`]); surfaces as [`StoreError::Io`].
///   * [`ReadFaultKind::ShortRead`] — the read ended before the requested slice
///     was filled ([`PositionedReadError::ShortRead`]). `bytes_read == 0` is an
///     EOF at the frame boundary (reader maps it to `corrupt_eof`); a non-zero
///     partial is a torn frame (reader maps it to a corrupt-segment error).
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadFaultKind {
    /// Inject a hard I/O error on the positioned read.
    Io,
    /// Inject a short read that stops after `bytes_read` bytes.
    ShortRead {
        /// Bytes "read" before the short read stops (`0` ⇒ EOF at the boundary).
        bytes_read: usize,
    },
}

/// Deterministic positioned-read fault bookkeeping. `target` is the 1-based
/// occurrence of [`StoreFs::read_exact_at`] to fault and the [`ReadFaultKind`] to
/// inject; `seen` counts positioned reads reached so far. Same target ⇒ the same
/// read faults every run. Kept distinct from [`OpFaultSchedule`] so a read fault
/// and an atomic-op fault can be armed independently in one run.
#[cfg(test)]
#[derive(Default)]
struct ReadFaultSchedule {
    target: Option<(u32, ReadFaultKind)>,
    seen: u32,
}

impl SimFs {
    /// Construct a filesystem model seeded from `seed`, dropping roughly one in
    /// `fsync_drop_one_in` fsyncs (`0` ⇒ never drop; the crash boundary is then
    /// exactly the bytes not yet fsynced).
    pub(crate) fn new(seed: u64, fsync_drop_one_in: u32) -> Self {
        Self {
            rng: Mutex::new(fastrand::Rng::with_seed(seed)),
            durable: Mutex::new(BTreeMap::new()),
            fsync_drop_one_in,
            enospc_on_copy: Mutex::new(EnospcSchedule::default()),
            op_fault: Mutex::new(OpFaultSchedule::default()),
            #[cfg(test)]
            read_fault: Mutex::new(ReadFaultSchedule::default()),
        }
    }

    /// Arm a deterministic fault on the `fail_at`-th occurrence (1-based) of
    /// crash-sensitive op `op`, consuming `self` (builder form for a `SimFs` not
    /// yet shared). See [`SimFs::arm_fault_on`].
    #[cfg(test)]
    pub(crate) fn with_fault_on(self, op: CrashOp, fail_at: u32) -> Self {
        self.arm_fault_on(op, fail_at);
        self
    }

    /// Arm (or re-arm) a deterministic fault on the `fail_at`-th occurrence
    /// (1-based) of crash-sensitive op `op`. The faulted op returns an injected
    /// I/O error instead of performing, modelling a torn atomic-rename / persist
    /// on the compaction swap, visibility-range persist, or cursor-checkpoint
    /// persist path. Same `(op, fail_at)` ⇒ the same op fails every run.
    ///
    /// Interior-mutable (takes `&self`) so a test can build a `Store` over a
    /// shared `Arc<SimFs>` FIRST and arm the fault only once the store is open —
    /// the occurrence counter resets here, so the crash-sensitive ops the build
    /// itself performed are not counted toward `fail_at`.
    #[cfg(test)]
    pub(crate) fn arm_fault_on(&self, op: CrashOp, fail_at: u32) {
        let mut sched = self
            .op_fault
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sched.target = Some((op, fail_at));
        sched.seen = 0;
    }

    /// Advance the counter for `op` and return `true` when THIS occurrence must
    /// fault. A no-op (returns `false`) when no schedule targets `op`.
    fn op_fault_strikes(&self, op: CrashOp) -> bool {
        let mut sched = self
            .op_fault
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some((target, fail_at)) = sched.target else {
            return false;
        };
        if target != op {
            return false;
        }
        sched.seen = sched.seen.saturating_add(1);
        sched.seen == fail_at
    }

    /// The injected-fault I/O error a faulted atomic op returns.
    fn injected_op_fault(op: CrashOp) -> io::Error {
        io::Error::other(format!("SimFs: injected fault on {op:?}"))
    }

    /// Arm a deterministic positioned-read fault on the `fail_at`-th (1-based)
    /// [`StoreFs::read_exact_at`], consuming `self` (builder form for a `SimFs`
    /// not yet shared). See [`SimFs::arm_read_fault_on`].
    #[cfg(test)]
    pub(crate) fn with_read_fault_on(self, fail_at: u32, kind: ReadFaultKind) -> Self {
        self.arm_read_fault_on(fail_at, kind);
        self
    }

    /// Arm (or re-arm) a deterministic positioned-read fault: the `fail_at`-th
    /// (1-based) [`StoreFs::read_exact_at`] injects `kind` instead of reading,
    /// modelling a torn/short active-segment frame read. Same `(fail_at, kind)` ⇒
    /// the same read faults every run.
    ///
    /// Interior-mutable (takes `&self`) so a test can build a `Store` over a
    /// shared `Arc<SimFs>` FIRST and arm the fault only once the store is open —
    /// the occurrence counter resets here, so any reads the build itself
    /// performed are not counted toward `fail_at`.
    #[cfg(test)]
    pub(crate) fn arm_read_fault_on(&self, fail_at: u32, kind: ReadFaultKind) {
        let mut sched = self
            .read_fault
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sched.target = Some((fail_at, kind));
        sched.seen = 0;
    }

    /// Advance the positioned-read counter and return the [`ReadFaultKind`] when
    /// THIS read must fault. A no-op (returns `None`) when no schedule is armed.
    #[cfg(test)]
    fn read_fault_strikes(&self) -> Option<ReadFaultKind> {
        let mut sched = self
            .read_fault
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (fail_at, kind) = sched.target?;
        sched.seen = sched.seen.saturating_add(1);
        (sched.seen == fail_at).then_some(kind)
    }

    /// Arm deterministic ENOSPC injection: the `fail_at`-th file-materialization
    /// op (1-based; `cow_copy_file` or `copy`) fails with
    /// [`io::ErrorKind::StorageFull`]. Used by the offensive `fork_hostile_fs`
    /// fixture to force a disk-full mid-fork and prove the fork does not
    /// publish a partial copy.
    pub(crate) fn with_enospc_on_copy(self, fail_at: u32) -> Self {
        {
            let mut sched = self
                .enospc_on_copy
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            sched.fail_at = Some(fail_at);
            sched.seen = 0;
        }
        self
    }

    /// Advance the materialize counter and return `true` when THIS op must fail
    /// with ENOSPC. A no-op (returns `false`) when no schedule is armed.
    fn enospc_strikes_now(&self) -> bool {
        let mut sched = self
            .enospc_on_copy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(fail_at) = sched.fail_at else {
            return false;
        };
        sched.seen = sched.seen.saturating_add(1);
        sched.seen == fail_at
    }

    /// Decide whether THIS fsync is dropped, advancing the PRNG exactly once.
    /// A single draw per fsync keeps the drop schedule a pure function of the
    /// order in which the store reaches its fsync seam.
    fn fsync_dropped(&self) -> bool {
        let mut rng = self
            .rng
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let roll = rng.u32(..);
        self.fsync_drop_one_in != 0 && roll.is_multiple_of(self.fsync_drop_one_in)
    }

    /// Record an honored fsync: advance `path`'s durable length to the file's
    /// current real length. A dropped fsync skips this, so the tail stays
    /// lost-on-crash.
    fn record_durable(&self, file: &File, path: &Path) {
        let Ok(metadata) = file.metadata() else {
            return;
        };
        let len = metadata.len();
        let mut durable = self
            .durable
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        durable.entry(path.to_path_buf()).or_default().durable_len = len;
    }

    /// Durable byte length recorded for `path` (what survives a crash). `0` for
    /// an untracked path. Test-facing witness for the no-loss invariant.
    #[cfg(test)]
    pub(crate) fn durable_len(&self, path: &Path) -> u64 {
        self.durable
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(path)
            .map_or(0, |state| state.durable_len)
    }

    /// Simulate a crash: truncate every tracked real file to its last durable
    /// length, discarding the unsynced (and fsync-dropped) tail. Power-loss
    /// model. After this returns, reopening a real [`crate::store::Store`] over
    /// the same data directory cold-starts over the durable prefix only.
    ///
    /// Inherent (not a [`StoreFs`] trait method) because only the fault-injecting
    /// backend models a crash — the production [`crate::store::platform::fs::RealFs`]
    /// has no such concept, and adding a no-op trait method would leave a dead
    /// production vtable entry.
    pub(crate) fn crash(&self) {
        let durable = self
            .durable
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (path, state) in durable.iter() {
            let _truncated = crate::store::platform::fs::truncate_file_to(path, state.durable_len);
        }
    }

    /// Register a file written outside the segment fsync seam (fork copy / snapshot copy).
    fn track_materialized_file(&self, path: &Path) {
        let Ok(meta) = crate::store::platform::fs::metadata(path) else {
            return;
        };
        let mut durable = self
            .durable
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        durable.entry(path.to_path_buf()).or_default().durable_len = meta.len();
    }
}

impl StoreFs for SimFs {
    fn read_dir(&self, path: &Path) -> io::Result<ReadDir> {
        // Real directory: the sim composes over the store's actual data dir.
        crate::store::platform::fs::read_dir(path)
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        crate::store::platform::fs::create_dir_all(path)
    }

    fn create_new_file(&self, path: &Path) -> Result<File, StoreError> {
        let file = crate::store::platform::fs::create_new_file(path)?;
        // Register the file with durable_len = 0; its bytes become durable only
        // as honored fsyncs advance the recorded length.
        self.durable
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(path.to_path_buf())
            .or_default();
        Ok(file)
    }

    fn sync_file_with_mode(
        &self,
        file: &File,
        path: &Path,
        _mode: &SyncMode,
    ) -> Result<(), StoreError> {
        // A dropped fsync returns Ok (the store believes it durable) but does NOT
        // advance the durable length — modelling a silently-lying disk. The bytes
        // are then lost on the next crash, which is precisely the violation the
        // recovery oracle must never observe for an acknowledged-durable commit.
        if self.fsync_dropped() {
            return Ok(());
        }
        self.record_durable(file, path);
        Ok(())
    }

    fn sync_file_all(&self, file: &File, path: &Path) -> io::Result<()> {
        if self.fsync_dropped() {
            return Ok(());
        }
        self.record_durable(file, path);
        Ok(())
    }

    fn sync_parent_dir(&self, _path: &Path) -> Result<(), StoreError> {
        // The directory entry is modelled as always durable once the file is
        // created: the crash truncates file CONTENTS, it does not unlink files.
        Ok(())
    }

    fn reject_symlink_leaf(&self, path: &Path, purpose: &str) -> Result<(), StoreError> {
        crate::store::platform::fs::reject_symlink_leaf(path, purpose)
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        crate::store::platform::fs::canonicalize(path)
    }

    fn symlink_metadata(&self, path: &Path) -> io::Result<std::fs::Metadata> {
        crate::store::platform::fs::symlink_metadata(path)
    }

    fn cow_copy_file(
        &self,
        from: &Path,
        to: &Path,
        preference: crate::store::CopyPreference,
    ) -> io::Result<crate::store::platform::fs::CowStrategyUsed> {
        if self.enospc_strikes_now() {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "SimFs: injected ENOSPC mid-fork on cow_copy_file",
            ));
        }
        let used = crate::store::platform::fs::cow_copy_file(from, to, preference)?;
        self.track_materialized_file(to);
        Ok(used)
    }

    fn copy(&self, from: &Path, to: &Path) -> io::Result<u64> {
        if self.enospc_strikes_now() {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "SimFs: injected ENOSPC mid-fork on copy",
            ));
        }
        let bytes = crate::store::platform::fs::copy(from, to)?;
        self.track_materialized_file(to);
        Ok(bytes)
    }

    fn metadata(&self, path: &Path) -> io::Result<std::fs::Metadata> {
        crate::store::platform::fs::metadata(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        if self.op_fault_strikes(CrashOp::Rename) {
            return Err(Self::injected_op_fault(CrashOp::Rename));
        }
        crate::store::platform::fs::rename(from, to)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        // `remove_file_if_present` is the provided default in terms of this
        // method, so both the direct reclaim and the if-present probes funnel
        // through one faultable primitive.
        if self.op_fault_strikes(CrashOp::RemoveFile) {
            return Err(Self::injected_op_fault(CrashOp::RemoveFile));
        }
        crate::store::platform::fs::remove_file(path)
    }

    fn named_temp_in(&self, dir: &Path) -> io::Result<tempfile::NamedTempFile> {
        // The temp file is the staging half; the publish (and its fault) is on
        // `persist_temp_with_parent_sync`, so staging is a faithful delegate.
        crate::store::platform::fs::named_temp_in(dir)
    }

    fn persist_temp_with_parent_sync(
        &self,
        named_temp: tempfile::NamedTempFile,
        final_path: &Path,
        admission: crate::store::platform::sync::ParentDirSyncAdmission,
    ) -> io::Result<()> {
        // A faulted persist drops the rename entirely: the staged temp is left
        // un-published, so the store's belief that the metadata is durable is
        // falsified — exactly the torn atomic publish the crash harness needs.
        if self.op_fault_strikes(CrashOp::PersistTemp) {
            return Err(Self::injected_op_fault(CrashOp::PersistTemp));
        }
        crate::store::platform::sync::persist_temp_with_parent_sync(
            named_temp, final_path, admission,
        )
    }

    fn read_exact_at(
        &self,
        file: &mut File,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<(), PositionedReadError> {
        // A faulted read never touches the real file: it returns the injected
        // positioned-read error directly, so the reader's active-frame read
        // surfaces the same StoreError it would on a genuinely torn read. No
        // `read_at` here — the raw pread contact stays in `platform::fs`. The
        // fault path is test-only; the production `Store`-over-`SimFs` fixtures
        // never fault a read, so they compile straight to the honest delegate.
        #[cfg(test)]
        if let Some(kind) = self.read_fault_strikes() {
            return Err(match kind {
                ReadFaultKind::Io => PositionedReadError::Io(io::Error::other(
                    "SimFs: injected positioned-read fault",
                )),
                ReadFaultKind::ShortRead { bytes_read } => {
                    PositionedReadError::ShortRead { bytes_read }
                }
            });
        }
        crate::store::platform::fs::read_exact_at(file, offset, buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn same_seed_same_fsync_drop_schedule() {
        let a = SimFs::new(99, 4);
        let b = SimFs::new(99, 4);
        let pa: Vec<_> = (0..64).map(|_| a.fsync_dropped()).collect();
        let pb: Vec<_> = (0..64).map(|_| b.fsync_dropped()).collect();
        assert_eq!(
            pa, pb,
            "PROPERTY: identical seeds produce identical fsync-drop schedules"
        );
    }

    #[test]
    fn crash_truncates_to_durable_length() {
        let dir = tempfile::tempdir().expect("tmpdir");
        // Never drop fsyncs here so the durability is purely the unsynced tail.
        let fs = SimFs::new(1, 0);
        let path = dir.path().join("seg.fbat");
        let mut file = fs.create_new_file(&path).expect("create");
        file.write_all(b"durable").expect("write durable");
        fs.sync_file_all(&file, &path).expect("honored fsync");
        let durable = fs.durable_len(&path);
        // Write more, do NOT route a sync: this tail must be lost on crash.
        // Flush the real bytes through the platform seam (the structural gate
        // forbids a bare `.sync_all()` outside src/store/platform) so the tail is
        // genuinely on disk before the crash truncates it back to durable_len.
        file.write_all(b"-and-lost-tail").expect("write tail");
        crate::store::platform::sync::sync_file_all_io(&file).expect("flush real bytes to disk");
        fs.crash();
        let recovered = crate::store::platform::fs::metadata(&path)
            .expect("stat")
            .len();
        assert_eq!(
            recovered, durable,
            "PROPERTY: a crash truncates the real file to its last durable (fsynced) length"
        );
        assert_eq!(
            recovered,
            b"durable".len() as u64,
            "PROPERTY: only the fsynced prefix survives the crash"
        );
    }

    #[test]
    fn dropped_fsync_does_not_advance_durable_length() {
        let dir = tempfile::tempdir().expect("tmpdir");
        // Always drop fsyncs (1-in-1): durable length must never advance.
        let fs = SimFs::new(7, 1);
        let path = dir.path().join("seg.fbat");
        let mut file = fs.create_new_file(&path).expect("create");
        file.write_all(b"unsynced").expect("write");
        crate::store::platform::sync::sync_file_all_io(&file).expect("flush real bytes");
        fs.sync_file_all(&file, &path)
            .expect("dropped fsync still returns Ok to the store");
        assert_eq!(
            fs.durable_len(&path),
            0,
            "PROPERTY: a dropped fsync returns Ok but never advances the durable length"
        );
        fs.crash();
        assert_eq!(
            crate::store::platform::fs::metadata(&path)
                .expect("stat")
                .len(),
            0,
            "PROPERTY: an all-dropped-fsync file loses its entire tail on crash"
        );
    }
}
