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
- Line/function coverage delta: unmeasured
- Mutation delta: unmeasured
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
- Line/function coverage delta: unmeasured
- Mutation delta: unmeasured
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
  - `cargo test --test segment_scan_hardening sidx_footer_entry_count_disagreement_falls_back_to_frame_scan`
  - `cargo test --test segment_scan_hardening valid_crc_unreadable_frame_metadata_skips_only_that_frame`
  - `cargo test --test segment_scan_hardening orphan_commit_marker_is_ignored_without_stopping_scan`
- Line/function coverage delta: targeted rise in `src/store/segment/scan.rs`; exact JSON delta not recorded
- Mutation delta: unmeasured
- Covered tests:
  - `invalid_batch_begin_count_fails_closed_on_reopen` pins `BEGIN` markers
    with invalid item counts as fail-closed corruption.
  - `missing_hash_chain_for_data_frame_fails_closed_on_reopen` pins ordinary
    data frames with missing hash-chain metadata as fail-closed corruption.
  - `corruption_inside_staged_batch_discards_the_whole_batch` pins CRC
    corruption inside a staged batch as discarding the whole in-flight batch.
  - `sidx_footer_entry_count_disagreement_falls_back_to_frame_scan` pins SIDX
    footer count disagreement as accelerator corruption that must fall back to
    the authoritative frame scan.
  - `valid_crc_unreadable_frame_metadata_skips_only_that_frame` pins
    CRC-valid/non-CRC metadata decode failures as skipping the bad frame while
    preserving later independent frames.
  - `orphan_commit_marker_is_ignored_without_stopping_scan` pins COMMIT without
    prior BEGIN as ignored batch metadata that does not stop the scan.
- Remaining known blind spots:
  - low-level unit coverage still owns some byte-range helper boundaries, but
    the black-box slow-path corruption shapes currently called out in this
    ledger are covered.

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
    unsynced frame recoverable from host page cache; it is superseded by the
    dm-flakey block-layer proof in
    `tests/chaos/scenarios/single_append_written.rs`
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
    the corruption shape that must fail closed by forging a later-written
    `SYSTEM_CLOSE_COMPLETED` frame whose HLC regresses below a prior close
    event while preserving frame CRC validity.
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
  - none for the explicit-close lifecycle frontier shape currently in scope.

### Invariant: Durable frontier wait API surfaces honest blocking semantics

- Harness pattern: `State-Machine Harness`
- Location:
  - `tests/durable_frontier_waits.rs`
- Command used:
  - `cargo test --test durable_frontier_waits --features dangerous-test-hooks`
- Line/function coverage delta: not measured; Phase 2.1 adds durable wait API
  coverage, Phase 2.2 extends the same surface to applied and visible waits,
  and Phase 2.3 adds append-time gate coverage.
- Mutation delta:
  - `frontier-wait-durable` critical seam is registered at the 85% smoke
    threshold.
  - `frontier-append-gate` critical seam is registered at the 85% smoke
    threshold for gate kind matching, timeout propagation, receipt HLC
    conversion, and batch per-item gate ignore.
- Covered tests:
  - `wait_for_durable_returns_immediately_when_already_past` defends the fast
    path where the durable frontier already covers the target.
  - `wait_for_durable_blocks_then_returns_after_advance` defends that a
    waiter blocks until a later sync advances `durable_hlc`.
  - `wait_for_durable_returns_timeout_when_target_unreachable` defends
    mandatory timeout reporting through `StoreError::WaitTimeout`.
  - `wait_for_durable_surfaces_writer_crash` defends writer-crash poison and
    wakeup of blocked waiters.
  - `wait_for_durable_spurious_wakeup_safe` defends that condvar wakeups alone
    never satisfy the target predicate.
  - `wait_for_durable_mandatory_timeout_compiles_only_with_duration` defends
    the sync API shape by pinning a `Duration` parameter.
  - `wait_for_durable_zero_timeout_observes_current_state` defends the
    zero-timeout boundary for both uncovered and already-covered targets.
  - `wait_for_durable_origin_returns_immediately` defends the origin lower
    bound.
  - `wait_for_applied_returns_immediately_when_already_past` defends the
    applied fast path where the projection frontier already covers the target.
  - `wait_for_applied_returns_min_across_projections` defends that a single
    lagging registered projection keeps `applied_hlc` behind the target.
  - `wait_for_applied_blocks_until_lagging_projection_advances` defends that
    `wait_for_applied` wakes only after the lagging projection advances.
  - `wait_for_visible_returns_immediately_when_already_past` defends the
    visible fast path after publish.
  - `wait_for_visible_advances_under_cadence_gt_1_without_durable` defends the
    documented cadence>1 no-gate skew: visible can advance while durable does
    not.
  - `mixed_wait_for_durable_applied_visible_converge_in_order` defends that the
    three public wait surfaces share the same condvar/poison machinery and can
    converge on the same target.
  - `append_without_gate_returns_immediately` defends the default no-gate
    append behavior.
  - `append_with_durable_gate_blocks_until_synced` defends durable gate
    blocking until a later cadence sync.
  - `append_with_applied_gate_blocks_until_min_projection_advances` defends
    append-time applied gates honoring the min across registered projections.
  - `append_with_visible_gate_returns_after_publish` defends visible gate
    success under cadence>1 without waiting for durable sync.
  - `append_with_gate_surfaces_wait_timeout_when_unreachable` defends that
    gate timeout is surfaced while the committed event remains queryable.
  - `batch_append_with_durable_gate_covers_entire_batch` defends that a
    batch-level gate on the last event covers earlier batch items.
  - `batch_per_item_gate_ignored` defends the documented per-item gate ignore
    behavior for batches.
- Observation notes:
  - `OBS-CADENCE-GT-1-VISIBLE-EXCEEDS-DURABLE` is narrowed, not retired:
    cadence>1 without a `DurabilityGate` still exhibits the skew, and the gate
    is the opt-in escape hatch.
- Remaining known blind spots:
  - No precise waiter lists yet; wake-all remains the v1 wait strategy.

### Invariant: Linux block-layer chaos harness fails writes after device flip

- Harness pattern: `Fault-Injection Harness`
- Location:
  - `tests/chaos.rs`
  - `tests/chaos/dm_flakey.rs`
  - `tests/chaos/scenarios/smoke.rs`
- Command used:
  - `BATPAK_RUN_CHAOS=1 cargo test --features dangerous-test-hooks --test chaos smoke -- --test-threads=1`
- Line/function coverage delta: not measured; this scaffold proves harness
  viability rather than batpak runtime code coverage.
- Mutation delta: not applicable yet; batpak-specific torn-tail scenarios land
  in later Phase 1B stops.
- Covered tests:
  - `dm_flakey_wrapper_create_flip_teardown_round_trip` defends
    `INV-CHAOS-LINUX-ONLY` by proving the privileged Linux device-mapper
    wrapper can create a mapped ext4 device, write before flip, flip the mapper
    to an error target, and observe synchronous write failure afterward.
- Remaining known blind spots:
  - This scaffold entry proves wrapper viability only; batpak-specific
    durability claims are recorded in the torn-tail scenario entry below.

### Invariant: Durable frontier covers recovered state after device failure

- Harness pattern: `Fault-Injection Harness`
- Location:
  - `tests/chaos.rs`
  - `tests/chaos/dm_flakey.rs`
  - `tests/chaos/scenarios/single_append_written.rs`
  - `tests/chaos/scenarios/batch_commit_written.rs`
- Command used:
  - `BATPAK_RUN_CHAOS=1 cargo test --features dangerous-test-hooks --test chaos -- --test-threads=1`
- Activation gate:
  - `BATPAK_RUN_CHAOS=1`
- Line/function coverage delta: not measured; this is a privileged block-layer
  proof rather than a coverage-density suite.
- Mutation delta: not applicable; the scenario exercises real device-mapper
  failure semantics outside cargo-mutants' in-process model.
- Covered tests:
  - `durable_frontier_covers_recovered_state_after_device_failure_cadence_1000`
    defends `INV-FRONTIER-DURABLE-COVERS-RECOVERED` by capturing the
    pre-failure `durable_hlc` after a successful explicit sync, appending an
    in-flight `SingleAppendWritten` event without another batpak fsync, failing
    the mapper, remounting the same backing file, and asserting that recovered
    `durable_hlc` covers every recovered event and remains monotonic across the
    crash boundary. OS-level page-cache or ext4 write-back may preserve or lose
    the in-flight frame; batpak's contract is honest Meaning-2 durable frontier
    classification of whatever was preserved, not fsync-history tracking or
    guaranteed physical disappearance.
  - `single_append_written_surfaces_io_error_cadence_1` defends that an append
    after the mapper is flipped to an error target returns a caller-visible
    storage failure instead of a false success receipt.
  - `post_fsync_events_survive_device_failure_durability_floor` defends the
    lower bound: events fsynced through the block device remain recoverable
    after a later mapper failure.
  - `durable_frontier_covers_recovered_state_after_batch_device_failure_cadence_1000`
    defends `INV-FRONTIER-DURABLE-COVERS-RECOVERED` for unsynced batch
    commit windows by asserting `durable_hlc` covers every recovered batch
    entry and remains monotonic across the dm-flakey failure boundary, while
    making no claim about the exact recovered count.
  - `batch_append_surfaces_io_error_after_device_failure_cadence_1000`
    defends that a batch append after the mapper is flipped to an error target
    returns a caller-visible storage failure instead of a false success receipt.
  - `post_fsync_batches_survive_device_failure_durability_floor` defends the
    batch lower bound: batches fsynced through the block device remain
    recoverable after a later mapper failure.
  - `mixed_single_and_batch_durable_floor_survives_device_failure` defends that
    durable frontier monotonicity and coverage hold across interleaved single
    and batch sync boundaries.
  - `partial_batch_writeback_durable_hlc_remains_monotonic` defends the larger
    unsynced batch surface where OS write-back may preserve zero, some, or all
    batch entries; batpak's guarantee remains recovered-state classification.
  - `batch_append_surfaces_io_error_after_device_failure_cadence_1` defends
    that cadence=1 batch appends surface device failure on the first batch
    attempt after the mapper flips.
- Remaining known blind spots:
  - the legacy in-process `FaultInjector` test remains ignored as a documented
    contrast with host page-cache behavior; the privileged dm-flakey scenario
    now pins the recovered-state accounting contract. Batpak deliberately does
    not expose or persist a fsync-history marker that distinguishes "batpak
    deliberately fsynced" from "the OS preserved this data"; this is recorded
    as `OBS-DURABLE-HLC-INCLUDES-OS-PRESERVED-DATA`.

## Equivalence Harness

### Invariant: Derived projections stay equivalent to the hand-written target

- Harness pattern: `Equivalence Harness`
- Location:
  - `tests/derive_event_sourced_parity.rs`
  - `tests/derive_event_sourced_generic.rs`
- Command used:
  - `cargo test --test derive_event_sourced_parity`
  - `cargo test --test derive_event_sourced_generic`
- Line/function coverage delta: unmeasured
- Mutation delta: unmeasured
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
- Line/function coverage delta: unmeasured
- Mutation delta: unmeasured
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
    and watcher-adjacent paths; exact JSON delta not recorded
- Mutation delta: unmeasured
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
  - `cargo test --test projection_cache empty_projection_surface_skips_cache_for_no_replay_plan`
  - `cargo test --test projection_cache consistent_replays_when_reopened_native_cache_row_is_stale`
  - `cargo test --test projection_cache maybe_stale_replays_when_cache_row_has_valid_metadata_but_empty_payload`
  - `cargo test --test projection_cache consistent_replays_when_cache_row_has_valid_metadata_but_truncated_payload`
- Line/function coverage delta: targeted rise in `src/store/projection/flow.rs`; exact JSON delta not recorded
- Mutation delta: unmeasured
- Remaining known blind spots:
  - this seam now proves that stale-but-young corrupt rows, fresh-but-corrupt rows, cache-get failures, and exact age-boundary rows all fall back to honest replay under `Freshness::MaybeStale`
  - coverage-closure sweep also pins empty/no-replay-plan behavior, reopened stale external-cache replay under `Freshness::Consistent`, and valid-metadata/undecodable-payload cache rows that bypass metadata corruption but still must replay honestly
  - remaining cache-edge blind spots are now limited to backend-specific OS error shapes that are difficult to force portably without changing production behavior

## Property Harness

### Invariant: Fuzz and chaos probe outputs stay within explicit policy gates

- Harness pattern: `Property Harness`
- Location:
  - `tests/fuzz_chaos_feedback.rs`
- Command used:
  - `cargo test --test fuzz_chaos_feedback --all-features --release`
- Line/function coverage delta: unmeasured
- Mutation delta: unmeasured
- Remaining known blind spots:
  - feedback policy is explicit, but it does not replace direct seam-level
    fault-injection or state-machine proofs

### Invariant: Representative store errors keep stable handling, display, and source contracts

- Harness pattern: `Property Harness`
- Location:
  - `tests/store_error_contract.rs`
  - `src/store/error.rs`
- Command used:
  - `cargo test --test store_error_contract`
  - `cargo test store::error::tests`
- Line/function coverage delta: targeted rise in `src/store/error.rs`; exact JSON delta not recorded
- Mutation delta: unmeasured
- Covered tests:
  - `store_error_contract_table_stays_stable` now includes direct public
    contract rows for helper-shaped `CorruptSegment` construction and
    fail-closed `InvariantViolation` display/classification.
  - `src/store/error.rs::tests::*_helper_*` directly exercises every
    `pub(crate)` `StoreError` helper constructor, including source-bearing
    batch/cache/serialization helpers and segment-corruption helpers.
- Remaining known blind spots:
  - none for the representative `StoreError` handling, `Display`, `source()`,
    conversion routing, and internal helper-constructor surface currently in
    scope.

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
- Line/function coverage delta: unmeasured
- Mutation delta: unmeasured
- Remaining known blind spots:
  - loom proofs cover bounded interleavings, not unbounded stress or real I/O

### Invariant: Durable cursor checkpoints only commit honest progress

- Harness pattern: `State-Machine Harness`
- Location:
  - `tests/cursor_durability.rs`
- Command used:
  - `cargo test --test cursor_durability`
- Line/function coverage delta: targeted rise in `src/store/delivery/cursor.rs`;
    exact JSON delta not recorded
- Mutation delta: unmeasured
- Remaining known blind spots:
  - committed progress vs rollback/restart semantics are covered, but this does
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
- Line/function coverage delta: targeted rise in `src/store/write/control.rs`; exact JSON delta not recorded
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
- Line/function coverage delta: targeted rise in `src/store/index/columnar.rs` and `src/store/index/mod.rs`; exact JSON delta not recorded
- Mutation delta: unmeasured
- Remaining known blind spots:
  - the oracle now owns filter composition, cursor batch ordering, and live-vs-reopen parity across topologies
  - remaining blind spots are deeper restore-artifact mismatches outside this pure query surface, which still belong to cold-start parity suites rather than the overlay oracle itself
