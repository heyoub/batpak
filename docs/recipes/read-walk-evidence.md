# Read Walk Evidence

Agent surface tasks: `read_with_evidence`, `schema_snapshot`, `chain_walk_evidence`.

Problem: read data and carry deterministic evidence about selector, freshness
intent, limits, and proof refs.

Correct API: `ReadWalkRequest`, `Store::query_with_read_walk_evidence`,
`ReadWalkEvidenceReport`, `ChainWalkRequest`, `Store::chain_walk_evidence`,
`compare_schema_snapshot`.

Minimal code is mirrored by `templates/audit-read-report`.

Wrong tempting move: return read rows and later infer which boundary or frontier
was used.

Test command: `cargo test -p batpak --test read_walk_evidence_report --all-features`.

Invariant protected: evidence bodies are deterministic and do not fake certainty.
