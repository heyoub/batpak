use batpak::registry::{
    registry_row_body_hash, NamedDigest, RegistryRowBody, RegistryRowId,
    REGISTRY_LIFECYCLE_LIVE, REGISTRY_ROW_BODY_SCHEMA_VERSION,
};

pub fn run() -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let body = RegistryRowBody {
        schema_version: REGISTRY_ROW_BODY_SCHEMA_VERSION,
        row_id: RegistryRowId([1; 32]),
        row_kind: 1,
        row_layout_version: 1,
        opaque_payload: vec![1, 2, 3],
        named_digests: vec![NamedDigest {
            name: "body".to_string(),
            digest: [2; 32],
        }],
        lifecycle: REGISTRY_LIFECYCLE_LIVE,
        supersedes: None,
    };
    Ok(registry_row_body_hash(&body)?)
}
