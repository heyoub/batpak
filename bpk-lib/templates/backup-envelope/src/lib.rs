use batpak::store::{
    backup_manifest_body_hash, BackupManifestBody, BackupSegmentRef,
    BACKUP_MANIFEST_BODY_SCHEMA_VERSION,
};

pub fn run() -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let body = BackupManifestBody {
        schema_version: BACKUP_MANIFEST_BODY_SCHEMA_VERSION,
        backup_id: [1; 32],
        layout_revision: 1,
        tooling_revision: 1,
        segments: vec![BackupSegmentRef {
            segment_id: 1,
            bytes_digest: [2; 32],
        }],
    };
    Ok(backup_manifest_body_hash(&body)?)
}
