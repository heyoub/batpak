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

## Oracle Harness

### Status: Planned, not yet ledger-seeded

- Harness pattern: `Oracle Harness`
- Location:
  - planned first-class target: `src/store/index/columnar.rs`
- Command used:
  - pending
- Line/function coverage delta: pending
- Mutation delta: pending
- Remaining known blind spots:
  - the repo still needs a dedicated simple-scan oracle harness for the columnar
    topology family rather than relying only on narrower topology-specific tests
