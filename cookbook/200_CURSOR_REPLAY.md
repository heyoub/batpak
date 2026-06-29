# Cursor Replay

Agent surface task: `cursor_replay`.

Problem: process events with at-least-once replay and optional durable
checkpoints.

Correct API: `Store::cursor_guaranteed`, cursor checkpoint config, typed reactor
helpers when routing by payload type.

Minimal code is mirrored by `bpk-lib/templates/typed-reactor` and
`bpk-lib/crates/batpak-examples/src/bin/cursor_worker.rs`.

Wrong tempting move: use a lossy subscription as a replay queue.

Test command: `cargo test -p batpak --test cursor_at_least_once_witness --all-features`.

Invariant protected: replay delivery exposes the at-least-once witness instead
of pretending to be exactly-once.
