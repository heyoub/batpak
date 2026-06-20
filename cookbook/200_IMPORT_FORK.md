# Import Fork

Agent surface task: `import_fork`.

Problem: copy selected events from one store into another without pretending the
two histories have the same event identity.

Correct API: `Store::import_events`, `ImportSelector`, `ImportOptions`.

Minimal shape:

```rust
let options = ImportOptions::new("source-alpha")?.with_chunk_size(256);
let report = destination.import_events(&source, &ImportSelector::all(), &options)?;
```

Import is re-application, not merge. Destination ids, global sequences, and
predecessors are regenerated. Raw MessagePack payload bytes and content hashes
are preserved. Correlation is copied as opaque metadata; causation is cleared.
The caller-owned `source_namespace` is part of the deterministic import key.

Wrong tempting move: derive the namespace from a path by default. Paths move;
logical source identity is caller policy. Path-derived namespaces are explicit
opt-in convenience only.

Test command: `cargo test -p batpak --test import_events --test isomorphism_laws`.

Invariant protected: INV-IMPORT-CONTENT-ISOMORPHISM.
