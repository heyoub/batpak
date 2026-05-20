//! Internal helpers shared by deterministic evidence report implementations.

use serde::Serialize;

pub(crate) type EvidenceHash = [u8; 32];

pub(crate) fn report_body_hash<T, E, F>(body: &T, map_error: F) -> Result<EvidenceHash, E>
where
    T: Serialize,
    F: FnOnce(String) -> E,
{
    let bytes = crate::canonical::to_bytes(body).map_err(|error| map_error(error.to_string()))?;
    Ok(content_hash(&bytes))
}

pub(crate) fn content_hash(bytes: &[u8]) -> EvidenceHash {
    crate::event::hash::compute_hash(bytes)
}

pub(crate) fn sort_findings<T: Ord>(findings: &mut [T]) {
    findings.sort();
}

pub(crate) fn sorted_findings<T: Clone + Ord>(findings: &[T]) -> Vec<T> {
    let mut sorted = findings.to_vec();
    sort_findings(&mut sorted);
    sorted
}
