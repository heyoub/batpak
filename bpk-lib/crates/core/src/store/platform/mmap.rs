use crate::store::stats::MmapEvidence;
use crate::store::StoreError;
use memmap2::Mmap;
use std::fs::File;

#[derive(Clone, Copy, Debug)]
pub(crate) struct SealedSegmentMmapAdmission {
    _private: (),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MmapIndexAdmission {
    _private: (),
}

pub(crate) fn admit_mmap_index(evidence: MmapEvidence) -> Result<MmapIndexAdmission, StoreError> {
    match evidence {
        MmapEvidence::FileBacked => Ok(MmapIndexAdmission { _private: () }),
        MmapEvidence::Unknown | MmapEvidence::ObservedUnsupported | MmapEvidence::ProbeFailed => {
            Err(StoreError::PlatformAdmissionFailed {
                capability: "mmap index",
                reason: format!("mmap index evidence {evidence:?} is not admissible"),
            })
        }
    }
}

pub(crate) fn admit_sealed_segment_mmap(
    evidence: MmapEvidence,
) -> Result<SealedSegmentMmapAdmission, StoreError> {
    match evidence {
        MmapEvidence::FileBacked => Ok(SealedSegmentMmapAdmission { _private: () }),
        MmapEvidence::Unknown | MmapEvidence::ObservedUnsupported | MmapEvidence::ProbeFailed => {
            Err(StoreError::PlatformAdmissionFailed {
                capability: "sealed segment mmap",
                reason: format!("sealed segment mmap evidence {evidence:?} is not admissible"),
            })
        }
    }
}

/// Map a file using the target mmap primitive.
///
/// # Safety
/// Callers own the semantic proof that the mapped file will not be mutated in a
/// way that violates the store path using the mapping.
pub(crate) unsafe fn map_mmap_index_file(
    file: &File,
    _admission: MmapIndexAdmission,
) -> std::io::Result<Mmap> {
    unsafe { Mmap::map(file) }
}

/// Map a sealed segment using an admitted mmap token.
///
/// # Safety
/// The admission token proves only the target mmap mechanism. Callers still own
/// the store semantic proof that this segment is sealed and immutable.
pub(crate) unsafe fn map_sealed_segment_file(
    file: &File,
    _admission: SealedSegmentMmapAdmission,
) -> std::io::Result<Mmap> {
    unsafe { Mmap::map(file) }
}
