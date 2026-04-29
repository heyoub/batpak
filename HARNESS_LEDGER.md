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

### Invariant: Durable frontier observations stay honest under writer faults

- Harness pattern: `Fault-Injection Harness`
- Location:
  - `tests/durable_frontier_semantics.rs`
  - `tests/durable_frontier_chaos.rs`
- Command used:
  - `cargo test --test durable_frontier_semantics --features dangerous-test-hooks`
  - `cargo test --test durable_frontier_chaos --features dangerous-test-hooks`
- Line/function coverage delta: added in the Phase 0 durable-frontier wave;
  exact JSON delta not recorded in this ledger
- Mutation delta:
  - writer commit protocol smoke held at 5/5 caught = 100%
  - projection replay/freshness smoke held at 7/7 caught = 100%
- Remaining known blind spots:
  - `writer_panic_at_single_append_written_is_not_durable_on_reopen` is
    intentionally ignored because an in-process writer panic leaves the complete
    unsynced frame recoverable from host page cache; proving true torn-tail
    non-durability needs a VM/block-device harness
  - the public frontier exposes observation truth, not durability-gated read or
    wait semantics; those are later policy work

### Invariant: Explicit close lifecycle frontiers survive restart

- Harness pattern: `Fault-Injection Harness`
- Location:
  - `tests/durable_frontier_semantics.rs`
- Command used:
  - `cargo test --test durable_frontier_semantics --features dangerous-test-hooks`
- Line/function coverage delta: Phase 1A adds explicit-close bootstrap coverage;
  exact JSON delta not recorded in this ledger
- Mutation delta:
  - writer commit protocol smoke must remain at 5/5 caught = 100%
  - projection replay/freshness smoke must remain at 7/7 caught = 100%
- Covered tests:
  - `explicit_close_emits_system_close_completed_event` defends that explicit
    `Store::close()` emits one `SYSTEM_CLOSE_COMPLETED` covering the visible
    frontier at close time.
  - `drop_without_explicit_close_emits_no_close_event` defends that `Drop`
    never emits `SYSTEM_CLOSE_COMPLETED`, preserving the explicit-close-only
    lifecycle contract.
  - `bootstrap_open_hlc_consumes_recorded_close_hlc` defends repeated
    graceful open/close cycles by consuming the latest recovered close
    lifecycle HLC.
  - `close_hlc_monotonicity_violation_surfaces_invariant_violation` records
    the corruption shape that must fail closed once a segment-forging helper
    exists.
  - `ops_take_limit_returns_none_immediately_while_store_is_open` and
    `subscription_ops_take_limits_count` are fast mutation-smoke pins for
    exhausted `SubscriptionOps::take` behavior while the store remains open;
    `subscription_ops_filter_chains_correctly` applies the same fail-fast
    pattern to filtered take chains.
  - `cursor_all_region_first_poll_includes_global_sequence_zero` and the
    bounded cursor loop in `index_filter_composition::assert_cursor_matches`
    are fast mutation-smoke pins added with Phase 1A so repo-wide cursor
    progression mutants fail quickly instead of exhausting the smoke-lane
    timeout.
- Remaining known blind spots:
  - `close_hlc_monotonicity_violation_surfaces_invariant_violation` is ignored
    until the Phase 1B chaos/forging helper can construct a later-written close
    event whose HLC regresses below a prior close event.

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
  - `cargo test --test raw_projection_mode projection_flow_maybe_stale_keeps_replay_lanes_equivalent`
  - `cargo test --test raw_projection_mode projection_flow_incremental_group_local_keeps_lanes_equivalent`
  - `cargo test --test raw_projection_mode projection_flow_incremental_external_cache_keeps_lanes_equivalent`
- Line/function coverage delta: targeted rise in `src/store/projection/flow.rs`
    and watcher-adjacent paths; exact JSON delta not recorded in this wave
- Mutation delta: unmeasured in this wave
- Remaining known blind spots:
  - current matrix now covers relevant and irrelevant appends across the two replay
    lanes, the cache-enabled `Freshness::MaybeStale` stale-hit vs forced-replay branch,
    and both incremental branches (group-local and external-cache replay)
  - remaining blind spots are cache-get-error handling, exact age-boundary behavior,
    and the empty/no-replay-plan public surface

### Invariant: MaybeStale never serves corrupt cache bytes as a “fresh enough” success

- Harness pattern: `Fault-Injection Harness`
- Location:
  - `tests/projection_cache.rs`
- Command used:
  - `cargo test --test projection_cache freshness_maybe_stale_replays_when_stale_cache_bytes_are_corrupt`
  - `cargo test --test projection_cache freshness_maybe_stale_replays_when_fresh_cache_bytes_are_corrupt`
  - `cargo test --test projection_cache projection_replays_when_cache_get_errors`
  - `cargo test --test projection_cache freshness_maybe_stale_replays_at_exact_age_boundary`
- Line/function coverage delta: targeted rise in `src/store/projection/flow.rs`; exact JSON delta not recorded in this wave
- Mutation delta: unmeasured in this wave
- Remaining known blind spots:
  - this seam now proves that stale-but-young corrupt rows, fresh-but-corrupt rows, cache-get failures, and exact age-boundary rows all fall back to honest replay under `Freshness::MaybeStale`
  - remaining cache-edge blind spots are mostly around alternate corruption shapes and the empty/no-replay-plan public surface rather than stale-byte honesty itself

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
  - `cargo test --test control_plane_surface fenced_root_submit_stays_hidden_until_commit_and_cancel_discards_it`
  - `cargo test --test control_plane_surface fenced_batch_submit_stays_hidden_until_commit_and_cancel_discards_it`
  - `cargo test --test control_plane_surface fenced_reaction_submit_stays_hidden_until_commit_and_cancel_discards_it`
  - `cargo test --test control_plane_surface fenced_reaction_commit_preserves_reaction_metadata`
  - `cargo test --test control_plane_surface try_submit_batch_returns_retry_under_pressure`
  - `CARGO_INCREMENTAL=0 cargo mutants --output tools/xtask/target/mutants/writer-commit-ticket-try-check-none --in-place --baseline run --file 'src/store/write/*.rs' --exclude src/store/ancestry/by_clock.rs --all-features --cargo-arg --locked --test-tool cargo --shard 1/8 --sharding round-robin --build-timeout 300 --timeout 300 --minimum-test-timeout 120 -F 'Ticket<T>::try_check.*with None'`
  - `CARGO_INCREMENTAL=0 cargo mutants --output tools/xtask/target/mutants/fence-token-root-under-fence-4 --in-place --baseline run --file 'src/store/write/control.rs' --all-features --cargo-arg --locked --test-tool cargo --build-timeout 300 --timeout 300 --minimum-test-timeout 120 -F 'delete field fence_token from struct Self expression in AppendSubmission::root_under_fence'`
- Line/function coverage delta: targeted rise in `src/store/write/control.rs`; exact JSON delta not recorded in this wave
- Mutation delta:
  - exact mutant `src/store/write/control.rs:29:9 replace Ticket<T>::try_check -> Option<Result<T, StoreError>> with None` is now caught by the ready-path proof lane
  - the exact default-receipt mutants for `AppendTicket::try_check` and `BatchAppendTicket::try_check` are now characterized as unviable at build time:
    - `src/store/write/control.rs:64:9 replace AppendTicket::try_check -> Option<AppendReply> with Some(Default::default())`
    - `src/store/write/control.rs:96:9 replace BatchAppendTicket::try_check -> Option<BatchAppendReply> with Some(Default::default())`
  - exact field-deletion mutants in the fence/reaction submission constructors are now caught:
    - `src/store/write/control.rs:551:13 delete field fence_token from struct Self expression in AppendSubmission::root_under_fence`
    - `src/store/write/control.rs:562:17 delete field causation_id from struct AppendOptions expression in AppendSubmission::reaction`
    - `src/store/write/control.rs:575:13 delete field fence_token from struct Self expression in AppendSubmission::reaction_under_fence`
- Remaining known blind spots:
  - this closes the positive-ready edge for append and batch tickets and adds direct root-under-fence, batch-under-fence, reaction-under-fence visibility/cancel, and reaction metadata-preservation proofs
  - batch pressure-retry symmetry is now pinned alongside append pressure-retry, but the wider writer commit protocol still needs broader mutation pressure across `writer.rs`, `staging.rs`, and `fanout.rs`

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
