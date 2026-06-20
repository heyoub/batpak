# Fork Clone

Agent surface task: `fork_clone`.

Problem: materialize a store directory that can be opened independently without
replaying or exporting through an application format.

Correct API: `Store::fork_with_evidence` when the caller needs proof,
`Store::fork` when the report is intentionally discarded.

Minimal shape:

```rust
let report = store.fork_with_evidence(dest.path(), ForkOptions::default())?;
let forked = Store::open(StoreConfig::new(dest.path()))?;
```

Fork does not open the destination. Opening appends lifecycle events and starts
a writer, so it stays caller-owned.

Wrong tempting move: hardlink everything. Sealed segments may be shared because
they are immutable; the active segment, visibility ranges, idempotency sidecar,
and pending compaction marker must be copied.

Test command: `cargo test -p batpak --test store_fork`.

Invariant protected: INV-FORK-ISOLATION.
