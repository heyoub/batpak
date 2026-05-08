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
    content_hash_impl(bytes)
}

pub(crate) fn sort_findings<T: Ord>(findings: &mut [T]) {
    findings.sort();
}

#[cfg(feature = "blake3")]
fn content_hash_impl(bytes: &[u8]) -> EvidenceHash {
    crate::event::hash::compute_hash(bytes)
}

#[cfg(not(feature = "blake3"))]
fn content_hash_impl(bytes: &[u8]) -> EvidenceHash {
    let crc = crc32fast::hash(bytes).to_be_bytes();
    let mut out = [0_u8; 32];
    out[..4].copy_from_slice(&crc);
    out
}
