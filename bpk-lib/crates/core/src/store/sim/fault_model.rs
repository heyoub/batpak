//! In-memory, fault-injecting durability model (model-only determinism witness).
//!
//! [`InMemFaultFs`] is a pure in-memory model of a faulty filesystem. It does
//! NOT implement [`crate::store::platform::fs::StoreFs`] and is NOT wired under a
//! real [`Store`]; it backs the model-only [`super::workload`] determinism
//! witness (`sim_is_deterministic`). The real-`Store` composition over the
//! filesystem seam lives in [`super::fs::SimFs`] (real-file-backed) and is driven
//! by [`super::recovery`].
//!
//! On top of an in-memory directory tree the model consults a seeded PRNG
//! ([`fastrand`]) to apply faults keyed off [`InjectionPoint`]:
//!
//!   * **torn-write** — only a deterministic prefix of the bytes lands.
//!   * **short-read** — a read returns fewer bytes than requested.
//!   * **fsync-drop** — `fsync` is silently skipped so the most recent unsynced
//!     bytes are lost on the next simulated crash.
//!
//! Determinism: every fault decision is drawn from a single seeded
//! [`fastrand::Rng`] advanced once per [`InjectionPoint`] consultation, in the
//! order the simulation reaches those points. Same seed ⇒ same fault sequence.
//!
//! [`Store`]: crate::store::Store

use crate::store::fault::InjectionPoint;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A fault drawn for one [`InjectionPoint`] consultation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Fault {
    /// Proceed normally — the full op succeeds.
    None,
    /// Only `prefix_len` bytes of a write land; the tail is dropped.
    TornWrite {
        /// Number of bytes that actually persisted before the simulated tear.
        prefix_len: usize,
    },
    /// A read returns fewer bytes than requested.
    ShortRead {
        /// Number of bytes actually returned.
        returned: usize,
    },
    /// `fsync` is dropped: unsynced bytes remain lost-on-crash.
    FsyncDrop,
}

/// In-memory file: a byte vector split into a durable (fsynced) region and an
/// unsynced tail. A simulated crash truncates the file to its durable length.
#[derive(Default, Clone)]
struct SimFile {
    /// All written bytes (durable prefix + unsynced tail).
    bytes: Vec<u8>,
    /// Length of the prefix that has survived an `fsync`.
    durable_len: usize,
}

/// Deterministic, fault-injecting in-memory filesystem model.
///
/// State lives behind [`Mutex`]es so the type is legitimately `Send + Sync`; the
/// simulation drives it single-threaded so the locks are always uncontended.
pub(crate) struct InMemFaultFs {
    /// Seeded PRNG; advanced once per injection-point consultation.
    rng: Mutex<fastrand::Rng>,
    /// In-memory file table keyed by logical path.
    files: Mutex<BTreeMap<PathBuf, SimFile>>,
}

impl InMemFaultFs {
    /// Construct a model seeded from `seed`. All fault decisions are a pure
    /// function of `seed` and the order of injection-point consultations.
    pub(crate) fn new(seed: u64) -> Self {
        Self {
            rng: Mutex::new(fastrand::Rng::with_seed(seed)),
            files: Mutex::new(BTreeMap::new()),
        }
    }

    /// Decide which fault (if any) fires at `point`, advancing the PRNG exactly
    /// once. The decision distribution is keyed off the injection-point family
    /// so that, e.g., fsync points can only ever drop an fsync.
    pub(crate) fn decide_fault(&self, point: &InjectionPoint, op_len: usize) -> Fault {
        let mut rng = self
            .rng
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let roll = rng.u32(..);
        // ~1-in-8 fault rate; the family of the point selects the fault kind.
        if !roll.is_multiple_of(8) {
            return Fault::None;
        }
        // Bounded modulo against the op length, never zero-divides.
        let bounded = |op_len: usize| -> usize {
            if op_len == 0 {
                0
            } else {
                (roll as usize) % op_len
            }
        };
        match point {
            InjectionPoint::BatchFsync { .. }
            | InjectionPoint::SingleAppendWritten { .. }
            | InjectionPoint::BatchCommitWritten { .. } => Fault::FsyncDrop,
            InjectionPoint::ReadAt { .. } | InjectionPoint::ColdStartScanFrame { .. } => {
                Fault::ShortRead {
                    returned: bounded(op_len),
                }
            }
            // Every other write-path injection point models a torn write. An
            // explicit fallthrough (not a bare `_`) keeps the match exhaustive
            // in intent: new write points get torn-write semantics by default.
            InjectionPoint::BatchStart { .. }
            | InjectionPoint::BatchBeginWritten { .. }
            | InjectionPoint::BatchItemWritten { .. }
            | InjectionPoint::BatchItemsComplete { .. }
            | InjectionPoint::BatchPrePublish { .. }
            | InjectionPoint::SingleAppendStart { .. }
            | InjectionPoint::SingleAppendPublished { .. }
            | InjectionPoint::SegmentRotationCreate { .. }
            | InjectionPoint::SegmentRotation { .. }
            | InjectionPoint::MmapIndexLoad
            | InjectionPoint::IndexFooterDecode { .. }
            | InjectionPoint::CheckpointDecode
            | InjectionPoint::HiddenRangesLoad => Fault::TornWrite {
                prefix_len: bounded(op_len),
            },
        }
    }

    /// Append `data` to the file at `path`, applying any fault decided for
    /// `point`. Returns the number of bytes that landed (may be short under a
    /// torn write). New bytes are unsynced until [`InMemFaultFs::fsync`].
    pub(crate) fn write_bytes(&self, path: &Path, point: &InjectionPoint, data: &[u8]) -> usize {
        let landed = match self.decide_fault(point, data.len()) {
            Fault::TornWrite { prefix_len } => prefix_len,
            Fault::None | Fault::ShortRead { .. } | Fault::FsyncDrop => data.len(),
        };
        let mut files = self
            .files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let file = files.entry(path.to_path_buf()).or_default();
        file.bytes.extend_from_slice(&data[..landed]);
        landed
    }

    /// Read up to `len` bytes from `offset` in `path`, applying short-read
    /// faults decided for `point`. Returns the bytes actually delivered.
    pub(crate) fn read_bytes(
        &self,
        path: &Path,
        point: &InjectionPoint,
        offset: usize,
        len: usize,
    ) -> Vec<u8> {
        let files = self
            .files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(file) = files.get(path) else {
            return Vec::new();
        };
        let available = file.bytes.len().saturating_sub(offset);
        let want = len.min(available);
        let deliver = match self.decide_fault(point, want) {
            Fault::ShortRead { returned } => returned.min(want),
            Fault::None | Fault::TornWrite { .. } | Fault::FsyncDrop => want,
        };
        file.bytes[offset..offset + deliver].to_vec()
    }

    /// Fsync `path`, marking its current length durable — unless the fault
    /// decided for `point` drops the sync. Returns whether the sync was honored.
    pub(crate) fn fsync(&self, path: &Path, point: &InjectionPoint) -> bool {
        let dropped = matches!(self.decide_fault(point, 0), Fault::FsyncDrop);
        if dropped {
            return false;
        }
        let mut files = self
            .files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(file) = files.get_mut(path) {
            file.durable_len = file.bytes.len();
        }
        true
    }

    /// Simulate a crash: every file is truncated to its last durable length,
    /// discarding unsynced (and fsync-dropped) tails. Models power loss.
    pub(crate) fn crash(&self) {
        let mut files = self
            .files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for file in files.values_mut() {
            file.bytes.truncate(file.durable_len);
        }
    }

    /// Durable byte length of `path` (what survives a crash).
    pub(crate) fn durable_len(&self, path: &Path) -> usize {
        self.files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(path)
            .map_or(0, |f| f.durable_len)
    }

    /// Current (possibly unsynced) byte length of `path`.
    pub(crate) fn len(&self, path: &Path) -> usize {
        self.files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(path)
            .map_or(0, |f| f.bytes.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point() -> InjectionPoint {
        InjectionPoint::SingleAppendStart {
            entity: "e".to_string(),
        }
    }

    #[test]
    fn same_seed_same_fault_sequence() {
        let a = InMemFaultFs::new(99);
        let b = InMemFaultFs::new(99);
        let pa: Vec<_> = (0..32).map(|_| a.decide_fault(&point(), 100)).collect();
        let pb: Vec<_> = (0..32).map(|_| b.decide_fault(&point(), 100)).collect();
        assert_eq!(
            pa, pb,
            "PROPERTY: identical seeds produce identical fault sequences"
        );
    }

    #[test]
    fn crash_truncates_to_durable_length() {
        let fs = InMemFaultFs::new(1);
        let p = Path::new("seg.fbat");
        let landed = fs.write_bytes(p, &point(), b"hello world");
        for _ in 0..16 {
            if fs.fsync(p, &point()) {
                break;
            }
        }
        let durable = fs.durable_len(p);
        fs.crash();
        assert_eq!(
            fs.len(p),
            durable,
            "PROPERTY: a crash truncates each file to its last durable length"
        );
        assert!(
            landed <= 11,
            "torn writes never exceed the requested length"
        );
    }
}
