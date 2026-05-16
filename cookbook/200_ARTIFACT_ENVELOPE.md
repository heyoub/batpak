# Artifact Envelope

Agent surface task: `artifact_envelope`.

Problem: hash a canonical body separately from envelope metadata, signatures,
and attestations.

Correct API: `CanonicalArtifactEnvelope`, `artifact_body_hash_from_body`,
`verify_canonical_artifact_envelope`.

Minimal code is mirrored by `bpk-lib/templates/artifact-envelope`.

Wrong tempting move: include timestamps, diagnostics, or signatures in body
identity.

Test command: `cargo test -p batpak --test lane_a_artifact_substrate --all-features`.

Invariant protected: metadata never changes canonical body identity.
