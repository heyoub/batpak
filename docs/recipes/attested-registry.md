# Attested Registry

Agent surface task: `attested_registry`.

Problem: represent generic registry rows with deterministic row identity and
attestation checks.

Correct API: `RegistryRowBody`, `registry_row_body_hash`,
`verify_registry_attested_row`.

Minimal code is mirrored by `templates/registry-row`.

Wrong tempting move: move protocol semantic field classes into the generic
registry substrate.

Test command: `cargo test -p batpak --test lane_b1_registry_substrate --all-features`.

Invariant protected: registry rows normalize named digests before hashing and
signing checks.
