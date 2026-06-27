# Projection

Agent surface tasks: `build_projection`, `watch_projection`, `projection_run_evidence`.

Problem: build and observe projection state from store replay without inventing a
parallel ordering authority.

Correct API: `Projection`, projection cache traits, projection watch/wait
helpers, and `ProjectionRunEvidenceReport`.

Minimal code is mirrored by `bpk-lib/templates/projection-cache` and
`bpk-lib/crates/batpak-examples/src/bin/raw_projection_counter.rs`.

Wrong tempting move: call a cache hit fresh without binding it to replay
watermarks and cache capability evidence.

Test command: `cargo test -p batpak --test projection_run_evidence_report --all-features`.

Invariant protected: projection truth follows replay/frontier mechanics, not
ambient sampling.
