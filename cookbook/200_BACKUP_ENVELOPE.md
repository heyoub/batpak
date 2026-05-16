# Backup Envelope

Agent surface task: `backup_envelope`.

Problem: describe backup manifest segment bytes with deterministic identity and
restore proof evidence.

Correct API: `BackupManifestBody`, `BackupManifestEnvelope`,
`restore_proof_report_body`.

Minimal code is mirrored by `bpk-lib/templates/backup-envelope`.

Wrong tempting move: let segment vector order define manifest identity.

Test command: `cargo test -p batpak --test lane_b2_backup_envelope_substrate --all-features`.

Invariant protected: backup segment refs are sorted before manifest and restore
proof identity.
