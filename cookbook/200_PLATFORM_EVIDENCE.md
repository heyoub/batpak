# Platform Evidence

Agent surface task: `platform_evidence`.

Problem: probe filesystem and machine-contact assumptions at the store path
boundary.

Correct API (from `bpk-lib/`): `cargo xtask platform doctor`, `probe`, `verify`,
`bless`, and `audit`.

Minimal code is mirrored by `bpk-lib/templates/minimal-store`.

Wrong tempting move: hard-code host assumptions into public substrate APIs.

Test command: `cd bpk-lib && cargo xtask platform audit`.

Invariant protected: platform evidence is collected at the machine-contact
boundary, not inferred by docs.
