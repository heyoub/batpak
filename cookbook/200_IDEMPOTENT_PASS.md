# Idempotent Pass

Agent surface task: `idempotent_pass`.

Problem: make a keyed append a durable, re-runnable no-op so a retry (after a
crash, a redelivery, a replayed request) commits the operation exactly once —
and stays a no-op even after retention compaction has evicted the event or the
store has cold-started.

Correct API: `IdempotencyKey::for_operation`, `AppendOptions::with_idempotency`,
`StoreConfig::with_idempotency_retention`.

Minimal code is mirrored by `bpk-lib/crates/core/examples/idempotent_pass.rs`.

Derive the key from OPERATION IDENTITY, not payload bytes:
`IdempotencyKey::for_operation("account.credit", &[account, request_id])` hashes
a length-delimited `(domain, components)` tuple, so `["ab","c"]` and `["a","bc"]`
never collide. Re-running the same operation recomputes the same key, and the
second append returns the original receipt without writing a duplicate.

Growth is bounded by the window-priority hybrid (`IdempotencyRetention::Hybrid`,
the default): the window is the inviolable guarantee — a key whose original
commit is within `keep_sequences` of the frontier is never evicted by the
`max_keys` soft cap. A within-window retry is always a no-op regardless of load.

Wrong tempting move: treating `for_operation` as a content hash (it certifies
the OPERATION, not the payload), or assuming dedup only survives the event
retention window (the durable `index.idemp` sidecar outlives event eviction).

Test command: `cargo test -p batpak --test idempotency_durable_store --test idempotency_window_priority --all-features`.

Invariant protected: INV-IDEMPOTENCY-DURABLE-WINDOW — a within-window keyed
retry is always a no-op across compaction, cold-start, and snapshot.
