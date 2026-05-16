use batpak::artifact::{verify_canonical_artifact_envelope, CanonicalArtifactEnvelope};

#[derive(serde::Serialize)]
pub struct Body {
    pub value: u64,
}

pub fn run() -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let envelope = CanonicalArtifactEnvelope {
        body: Body { value: 1 },
        envelope_schema_version: 1,
        generated_at_wall_ms: Some(100),
        diagnostic_note: Some("template".to_string()),
        signatures: Vec::new(),
        attestations: Vec::new(),
    };
    let report = verify_canonical_artifact_envelope(&envelope, |_sig, _body| Ok(()))?;
    Ok(report.body_hash)
}
