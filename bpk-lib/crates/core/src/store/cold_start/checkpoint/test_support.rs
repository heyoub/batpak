use super::format;
use serde::Serialize;
use std::path::Path;

pub(super) fn touch_segment(dir: &Path, segment_id: u64) {
    let name = format!("{segment_id:06}.fbat");
    std::fs::write(dir.join(name), vec![0u8; 8192]).expect("write dummy segment");
}

pub(super) fn write_legacy_checkpoint_body<T: Serialize>(dir: &Path, version: u16, body: &T) {
    let body = crate::encoding::to_bytes(body).expect("serialize legacy checkpoint");
    let crc = crc32fast::hash(&body);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(format::CHECKPOINT_MAGIC);
    bytes.extend_from_slice(&version.to_le_bytes());
    bytes.extend_from_slice(&crc.to_le_bytes());
    bytes.extend_from_slice(&body);
    std::fs::write(dir.join(format::CHECKPOINT_FILENAME), bytes).expect("write legacy checkpoint");
}
