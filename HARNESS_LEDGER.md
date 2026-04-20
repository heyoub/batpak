# Harness Ledger

This ledger records the current doctrine-bearing suites and their primary
harness pattern.

Coverage is treated as a consequence of harness density, not as the target
itself. Where a delta is not yet measured, the ledger says so explicitly
instead of pretending.

## Fault-Injection Harness

### Invariant: Invalid derive input fails structurally

- Harness pattern: `Fault-Injection Harness`
- Location:
  - `tests/derive_eventpayload_errors.rs`
  - `tests/derive_event_sourced_errors.rs`
  - `tests/derive_multi_event_reactor_errors.rs`
- Command used:
  - `cargo test --test derive_eventpayload_errors`
  - `cargo test --test derive_event_sourced_errors`
  - `cargo test --test derive_multi_event_reactor_errors`
- Line/function coverage delta: unmeasured in this wave
- Mutation delta: unmeasured in this wave
- Remaining known blind spots:
  - compile-fail suites prove invalid macro shapes and error quality, but they
    do not prove successful derived runtime behaviour by themselves

### Invariant: Corruption and stress fail closed

- Harness pattern: `Fault-Injection Harness`
- Location:
  - `tests/chaos_testing.rs`
  - `tests/cold_start_recovery.rs`
- Command used:
  - `cargo test --test chaos_testing --all-features`
  - `cargo test --test cold_start_recovery`
- Line/function coverage delta: unmeasured in this wave
- Mutation delta: unmeasured in this wave
- Remaining known blind spots:
  - broad chaos coverage exists, but not every low-level segment-scan defensive
    branch is table-driven yet

### Invariant: Slow-path segment recovery fails closed on corrupt batch metadata

- Harness pattern: `Fault-Injection Harness`
- Location:
  - `tests/segment_scan_hardening.rs`
- Command used:
  - `cargo test --test segment_scan_hardening`
  - `cargo test --test segment_scan_hardening corruption_inside_staged_batch_discards_the_whole_batch`
- Line/function coverage delta: targeted rise in `src/store/segment/scan.rs`; exact JSON delta not recorded in this wave
- Mutation delta: unmeasured in this wave
- Remaining known blind spots:
  - this wave now proves invalid BEGIN counts, missing `hash_chain`, and CRC corruption on the second staged batch item all fail closed on reopen
  - remaining uncovered defensive branches are mostly other corrupt-frame shapes such as footer disagreement and non-CRC decode failures deeper in the slow path

## Equivalence Harness

### Invariant: Derived projections stay equivalent to the hand-written target

- Harness pattern: `Equivalence Harness`
- Location:
  - `tests/derive_event_sourced_parity.rs`
  - `tests/derive_event_sourced_generic.rs`
- Command used:
  - `cargo test --test derive_event_sourced_parity`
  - `cargo test --test derive_event_sourced_generic`
- Line/function coverage delta: unmeasured in this wave
- Mutation delta: unmeasured in this wave
- Remaining known blind spots:
  - these suites pin behavioural equivalence, not compile-fail diagnostics

### Invariant: Live, reopen, and replay paths converge on the same visible truth

- Harness pattern: `Equivalence Harness`
- Location:
  - `tests/replay_consistency.rs`
  - `tests/mmap_cold_start.rs`
- Command used:
  - `cargo test --test replay_consistency`
  - `cargo test --test mmap_cold_start`
- Line/function coverage delta: unmeasured in this wave
- Mutation delta: unmeasured in this wave
- Remaining known blind spots:
  - parity across all artifact paths is strong, but some corruption-only
    branches still live in separate fault-injection suites

### Invariant: Projection flow surfaces stay observationally equivalent

- Harness pattern: `Equivalence Harness`
- Location:
  - `tests/raw_projection_mode.rs`
- Command used:
  - `cargo test --test raw_projection_mode`
- Line/function coverage delta: targeted rise in `src/store/projection/flow.rs`
    and watcher-adjacent paths; exact JSON delta not recorded in this wave
- Mutation delta: unmeasured in this wave
- Remaining known blind spots:
  - current matrix covers relevant and irrelevant appends across the two replay
    lanes; cache-state and `Freshness::MaybeStale` matrix expansion still remains

## Property Harness

### Invariant: Fuzz and chaos probe outputs stay within explicit policy gates

- Harness pattern: `Property Harness`
- Location:
  - `tests/fuzz_chaos_feedback.rs`
- Command used:
  - `cargo test --test fuzz_chaos_feedback --all-features --release`
- Line/function coverage delta: unmeasured in this wave
- Mutation delta: unmeasured in this wave
- Remaining known blind spots:
  - feedback policy is explicit, but it does not replace direct seam-level
    fault-injection or state-machine proofs

### Invariant: Representative store errors keep stable handling, display, and source contracts

- Harness pattern: `Property Harness`
- Location:
  - `tests/store_error_contract.rs`
- Command used:
  - `cargo test --test store_error_contract`
- Line/function coverage delta: targeted rise in `src/store/error.rs`; exact JSON delta not recorded in this wave
- Mutation delta: unmeasured in this wave
- Remaining known blind spots:
  - this table owns representative variant handling, `Display`, `source()`, and conversion routing, but it does not yet exercise the internal helper constructors that only unit tests inside `src/store/error.rs` can reach

### Invariant: Catastrophic performance regressions trip explicit thresholds

- Harness pattern: `Property Harness`
- Location:
  - `tests/perf_gates.rs`
- Command used:
  - `cargo xtask perf-gates`
- Line/function coverage delta: not applicable
- Mutation delta: not applicable
- Remaining known blind spots:
  - these are intentionally loose catastrophic guards, not precise benchmark
    baselines; stable trend authority belongs to `cargo xtask bench`

## State-Machine Harness

### Invariant: Bounded schedules preserve concurrency protocol truth

- Harness pattern: `State-Machine Harness`
- Location:
  - `tests/deterministic_concurrency.rs`
- Command used:
  - `cargo test --test deterministic_concurrency`
- Line/function coverage delta: unmeasured in this wave
- Mutation delta: unmeasured in this wave
- Remaining known blind spots:
  - loom proofs cover bounded interleavings, not unbounded stress or real I/O

### Invariant: Durable cursor checkpoints only commit honest progress

- Harness pattern: `State-Machine Harness`
- Location:
  - `tests/cursor_durability.rs`
- Command used:
  - `cargo test --test cursor_durability`
- Line/function coverage delta: targeted rise in `src/store/delivery/cursor.rs`;
    exact JSON delta not recorded in this wave
- Mutation delta: unmeasured in this wave
- Remaining known blind spots:
  - this wave proves committed progress vs rollback/restart semantics, but does
    not yet replace the broader cursor lifecycle tests in `tests/store_advanced.rs`

### Invariant: Ready writer tickets surface observable completion through `try_check`

- Harness pattern: `State-Machine Harness`
- Location:
  - `tests/control_plane_surface.rs`
- Command used:
  - `cargo test --test control_plane_surface try_check_surfaces_ready_append_and_batch_tickets`
  - `CARGO_INCREMENTAL=0 cargo mutants --output tools/xtask/target/mutants/writer-commit-ticket-try-check-none --in-place --baseline run --file 'src/store/write/*.rs' --exclude src/store/ancestry/by_clock.rs --all-features --cargo-arg --locked --test-tool cargo --shard 1/8 --sharding round-robin --build-timeout 300 --timeout 300 --minimum-test-timeout 120 -F 'Ticket<T>::try_check.*with None'`
- Line/function coverage delta: targeted rise in `src/store/write/control.rs`; exact JSON delta not recorded in this wave
- Mutation delta:
  - exact mutant `src/store/write/control.rs:29:9 replace Ticket<T>::try_check -> Option<Result<T, StoreError>> with None` is now caught by the ready-path proof lane
  - the exact default-receipt mutants for `AppendTicket::try_check` and `BatchAppendTicket::try_check` are now characterized as unviable at build time:
    - `src/store/write/control.rs:64:9 replace AppendTicket::try_check -> Option<AppendReply> with Some(Default::default())`
    - `src/store/write/control.rs:96:9 replace BatchAppendTicket::try_check -> Option<BatchAppendReply> with Some(Default::default())`
- Remaining known blind spots:
  - this closes the positive-ready edge for append and batch tickets, but the wider writer commit protocol still needs broader mutation pressure across `writer.rs`, `staging.rs`, and `fanout.rs`

## Oracle Harness

### Invariant: Public query and cursor surfaces match a linear reference scan across topologies

- Harness pattern: `Oracle Harness`
- Location:
  - `tests/index_filter_composition.rs`
- Command used:
  - `cargo test --test index_filter_composition`
  - `cargo test --test index_filter_composition reopen_matches_live_oracle_across_topologies`
- Line/function coverage delta: targeted rise in `src/store/index/columnar.rs` and `src/store/index/mod.rs`; exact JSON delta not recorded in this wave
- Mutation delta: unmeasured in this wave
- Remaining known blind spots:
  - the oracle now owns filter composition, cursor batch ordering, and live-vs-reopen parity across topologies
  - remaining blind spots are deeper restore-artifact mismatches outside this pure query surface, which still belong to cold-start parity suites rather than the overlay oracle itself
