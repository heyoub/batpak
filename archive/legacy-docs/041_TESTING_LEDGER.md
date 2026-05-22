# Testing Ledger

This ledger records the current doctrine-bearing suites and their primary
harness pattern.

Coverage is treated as a consequence of harness density, not as the target
itself. Where a delta is unmeasured, the ledger says so explicitly
instead of pretending.

## Runtime And Boundary Harness

### Invariant: syncbat checkout emits honest runtime receipts

- Harness pattern: `Runtime And Boundary Harness`
- Status: unmeasured
- Catalog invariants: `INV-SYNCBAT-DISPATCH-RECEIPTS`
- Location:
  - `bpk-lib/crates/syncbat/tests/runtime.rs`
  - `bpk-lib/crates/syncbat/tests/store_sink.rs`
  - `bpk-lib/crates/syncbat/tests/receipt_namespace.rs`
  - `bpk-lib/crates/syncbat/tests/wire_stability.rs`
- Command used:
  - `cargo test -p syncbat --test runtime`
  - `cargo test -p syncbat --test store_sink`
  - `cargo test -p syncbat --test receipt_namespace`
  - `cargo test -p syncbat --test wire_stability`
- Line/function coverage delta: unmeasured
- Mutation delta: `syncbat-runtime-dispatch` lane is defined in
  `bpk-lib/tools/xtask/src/commands/mutants/lanes.rs`; no run receipt is
  recorded.
- Remaining known blind spots:
  - receipt hashing policy has deterministic unit coverage; arbitrary-byte
    proptest coverage has no recorded run.

### Invariant: syncbat durable catalog rebuild is deterministic

- Harness pattern: `Runtime And Boundary Harness`
- Status: unmeasured
- Catalog invariants: `INV-SYNCBAT-REGISTER-CATALOG-DETERMINISTIC`
- Location:
  - `bpk-lib/crates/syncbat/tests/register_store_catalog.rs`
  - `bpk-lib/crates/syncbat/tests/register_properties.rs`
  - `bpk-lib/crates/syncbat/tests/descriptor_validation.rs`
  - `bpk-lib/crates/syncbat/tests/operation_macro.rs`
  - `bpk-lib/crates/syncbat/tests/operation_macro_errors.rs`
  - `bpk-lib/crates/syncbat/tests/wire_stability.rs`
- Command used:
  - `cargo test -p syncbat --test register_store_catalog`
  - `cargo test -p syncbat --test register_properties`
  - `cargo test -p syncbat --test descriptor_validation`
  - `cargo test -p syncbat --test operation_macro`
  - `cargo test -p syncbat --test operation_macro_errors`
  - `cargo test -p syncbat --test wire_stability`
- Line/function coverage delta: unmeasured
- Mutation delta: `syncbat-register-catalog` lane is defined in
  `bpk-lib/tools/xtask/src/commands/mutants/lanes.rs`; no run receipt is
  recorded.
- Remaining known blind spots:
  - catalog row order is covered through store sequence order, but no separate
    mutation score is recorded for lifecycle conflict branches.

### Invariant: netbat line protocol stays stable and thin

- Harness pattern: `Runtime And Boundary Harness`
- Status: unmeasured
- Catalog invariants: `INV-NETBAT-LINE-PROTOCOL-STABLE`, `INV-NETBAT-BOUNDARY-THIN`
- Location:
  - `bpk-lib/crates/netbat/tests/boundary.rs`
  - `bpk-lib/crates/netbat/tests/route_validation.rs`
  - `bpk-lib/crates/netbat/tests/tcp_transport.rs`
  - `bpk-lib/crates/netbat/tests/golden/request_call_v1.hex`
- Command used:
  - `cargo test -p netbat --test boundary`
  - `cargo test -p netbat --test route_validation`
  - `cargo test -p netbat --test tcp_transport`
- Line/function coverage delta: unmeasured
- Mutation delta: `netbat-boundary-protocol` lane is defined in
  `bpk-lib/tools/xtask/src/commands/mutants/lanes.rs`; no run receipt is
  recorded.
- Remaining known blind spots:
  - NETBAT/1 is request/response only; streaming over Canal requires a separate
    protocol contract rather than an implicit extension.

## Fault-Injection Harness

### Invariant: Invalid derive input fails structurally

- Harness pattern: `Fault-Injection Harness`
- Status: unmeasured
- Catalog invariants: `INV-BUILD-FAIL-FAST`, `INV-GENERATED-WITNESS-PIN`
- Location:
  - `bpk-lib/crates/core/tests/derive_eventpayload_errors.rs`
  - `bpk-lib/crates/core/tests/derive_event_sourced_errors.rs`
  - `bpk-lib/crates/core/tests/derive_multi_event_reactor_errors.rs`
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
- Status: unmeasured
- Catalog invariants: `INV-FAULT-INJECT-GATED`, `INV-BATCH-CRASH-RECOVERY`
- Location:
  - `bpk-lib/crates/core/tests/chaos_testing.rs`
  - `bpk-lib/crates/core/tests/cold_start_recovery.rs`
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
- Status: unmeasured
- Catalog invariants: `INV-BATCH-CRASH-RECOVERY`
- Location:
  - `bpk-lib/crates/core/tests/segment_scan_hardening.rs`
- Command used:
  - `cargo test --test segment_scan_hardening`
  - `cargo test --test segment_scan_hardening corruption_inside_committed_batch_fails_closed`
  - `cargo test --test segment_scan_hardening sidx_footer_entry_count_disagreement_falls_back_to_frame_scan`
  - `cargo test --test segment_scan_hardening valid_crc_unreadable_frame_metadata_fails_closed`
  - `cargo test --test segment_scan_hardening orphan_commit_marker_is_ignored_without_stopping_scan`
- Line/function coverage delta: targeted rise in `bpk-lib/crates/core/src/store/segment/scan.rs`; exact JSON delta not recorded
- Mutation delta: unmeasured
- Covered tests:
  - `invalid_batch_begin_count_fails_closed_on_reopen` pins `BEGIN` markers
    with invalid item counts as fail-closed corruption.
  - `missing_hash_chain_for_data_frame_fails_closed_on_reopen` pins ordinary
    data frames with missing hash-chain metadata as fail-closed corruption.
  - `corruption_inside_committed_batch_fails_closed` pins CRC
    corruption inside a staged batch as discarding the whole in-flight batch.
  - `sidx_footer_entry_count_disagreement_falls_back_to_frame_scan` pins SIDX
    footer count disagreement as accelerator corruption that must fall back to
    the authoritative frame scan.
  - `valid_crc_unreadable_frame_metadata_fails_closed` pins
    CRC-valid/non-CRC metadata decode failures in committed data frames as
    fail-closed segment corruption.
  - `orphan_commit_marker_is_ignored_without_stopping_scan` pins COMMIT without
    prior BEGIN as ignored batch metadata that does not stop the scan.
- Remaining known blind spots:
  - low-level unit coverage still owns some byte-range helper boundaries, but
    the black-box slow-path corruption shapes currently called out in this
    ledger are covered.

### Invariant: Durable frontier observations stay honest under writer faults

- Harness pattern: `Fault-Injection Harness`
- Status: unmeasured
- Catalog invariants: `INV-FRONTIER-DURABLE-COVERS-RECOVERED`
- Location:
  - `bpk-lib/crates/core/tests/durable_frontier_semantics.rs`
  - `bpk-lib/crates/core/tests/durable_frontier_chaos.rs`
- Command used:
  - `cargo test --test durable_frontier_semantics --features dangerous-test-hooks`
  - `cargo test --test durable_frontier_chaos --features dangerous-test-hooks`
- Line/function coverage delta: added in the Phase 0 durable-frontier wave;
  exact JSON delta not recorded in this ledger
- Mutation delta:
  - writer commit protocol and projection replay/freshness are policy-owned
    critical seams; current thresholds are printed by `cargo xtask mutants policy`.
- Remaining known blind spots:
  - `writer_panic_at_single_append_written_is_not_durable_on_reopen` is
    intentionally ignored because an in-process writer panic leaves the complete
    unsynced frame recoverable from host page cache; it is superseded by the
    dm-flakey block-layer proof in
    `bpk-lib/crates/core/tests/chaos/scenarios/single_append_written.rs`
  - the public frontier exposes observation truth; durability-gated read and
    wait semantics are represented by the explicit wait APIs and durability
    gates.

### Invariant: Explicit close lifecycle frontiers survive restart

- Harness pattern: `Fault-Injection Harness`
- Status: unmeasured
- Catalog invariants: `INV-STORE-LIFECYCLE-HONESTY`, `INV-FRONTIER-OPEN-MONOTONIC`
- Location:
  - `bpk-lib/crates/core/tests/durable_frontier_semantics.rs`
- Command used:
  - `cargo test --test durable_frontier_semantics --features dangerous-test-hooks`
- Line/function coverage delta: Phase 1A adds explicit-close bootstrap coverage;
  exact JSON delta not recorded in this ledger
- Mutation delta:
  - no dedicated lifecycle seam; regressions route through the writer commit
    protocol and projection replay/freshness critical seams.
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

## State-Machine Harness

### Invariant: Platform profile mismatch fails open before lifecycle success

- Harness pattern: `State-Machine Harness`
- Status: unmeasured
- Catalog invariants: `INV-PLATFORM-EVIDENCE-NOT-MEANING`
- Location:
  - `bpk-lib/crates/core/tests/platform_backend.rs`
- Command used:
  - `cargo test --test platform_backend platform_profile_match_allows_open_and_mismatch_fails_before_lifecycle`
  - `cargo test platform_profile_mismatch_fails_closed`
- Line/function coverage delta: unmeasured
- Mutation delta:
  - `platform-backend` critical seam is registered at the 85% smoke threshold.
- Covered tests:
  - profile fingerprint round-trip pins the private JSON + CRC32 shape.
  - profile mismatch rejects store open before writer spawn or
    `SYSTEM_OPEN_COMPLETED` lifecycle append.
- Remaining known blind spots:
  - profile command and crates/core/build.rs env-var validation are covered by structural
    and compile checks; mutation-specific fixtures have no recorded run.

### Invariant: Durable frontier wait API surfaces honest blocking semantics

- Harness pattern: `State-Machine Harness`
- Status: unmeasured
- Catalog invariants: `INV-FRONTIER-WAIT-MONOTONIC`, `INV-FRONTIER-APPEND-GATE-HONORED`
- Location:
  - `bpk-lib/crates/core/tests/durable_frontier_waits.rs`
- Command used:
  - `cargo test --test durable_frontier_waits --features dangerous-test-hooks`
- Line/function coverage delta: not measured; the suite covers durable wait API
  semantics, applied/visible wait surfaces, and append-time gate behavior.
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
    blocking until a later explicit sync advances the durable frontier.
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
  - Precise waiter lists are not implemented; wake-all remains the current wait
    strategy.

## Fault-Injection Harness

### Invariant: Linux block-layer chaos harness fails writes after device flip

- Harness pattern: `Fault-Injection Harness`
- Status: unmeasured
- Catalog invariants: `INV-CHAOS-LINUX-ONLY`
- Location:
  - `bpk-lib/crates/core/tests/chaos.rs`
  - `bpk-lib/crates/core/tests/chaos/dm_flakey.rs`
  - `bpk-lib/crates/core/tests/chaos/scenarios/smoke.rs`
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
- Status: unmeasured
- Catalog invariants: `INV-FRONTIER-DURABLE-COVERS-RECOVERED`, `INV-CHAOS-LINUX-ONLY`
- Location:
  - `bpk-lib/crates/core/tests/chaos.rs`
  - `bpk-lib/crates/core/tests/chaos/dm_flakey.rs`
  - `bpk-lib/crates/core/tests/chaos/scenarios/single_append_written.rs`
  - `bpk-lib/crates/core/tests/chaos/scenarios/batch_commit_written.rs`
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
- Status: unmeasured
- Catalog invariants: `INV-REPLAY-LANE-SELECTION`
- Location:
  - `bpk-lib/crates/core/tests/derive_event_sourced_parity.rs`
  - `bpk-lib/crates/core/tests/derive_event_sourced_generic.rs`
- Command used:
  - `cargo test --test derive_event_sourced_parity`
  - `cargo test --test derive_event_sourced_generic`
- Line/function coverage delta: unmeasured
- Mutation delta: unmeasured
- Remaining known blind spots:
  - these suites pin behavioural equivalence, not compile-fail diagnostics

### Invariant: Live, reopen, and replay paths converge on the same visible truth

- Harness pattern: `Equivalence Harness`
- Status: unmeasured
- Catalog invariants: `INV-REPLAY-LANE-SELECTION`, `INV-MMAP-SEALED-READS`
- Location:
  - `bpk-lib/crates/core/tests/replay_consistency.rs`
  - `bpk-lib/crates/core/tests/mmap_cold_start.rs`
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
- Status: unmeasured
- Catalog invariants: `INV-CACHE-CAPABILITIES-EXPLICIT`, `INV-REPLAY-LANE-SELECTION`
- Location:
  - `bpk-lib/crates/core/tests/raw_projection_mode.rs`
- Command used:
  - `cargo test --test raw_projection_mode`
  - `cargo test --test raw_projection_mode projection_flow_maybe_stale_keeps_replay_lanes_equivalent`
  - `cargo test --test raw_projection_mode projection_flow_incremental_group_local_keeps_lanes_equivalent`
  - `cargo test --test raw_projection_mode projection_flow_incremental_external_cache_keeps_lanes_equivalent`
- Line/function coverage delta: targeted rise in `bpk-lib/crates/core/src/store/projection/flow.rs`
    and watcher-adjacent paths; exact JSON delta not recorded
- Mutation delta: unmeasured
- Remaining known blind spots:
  - current matrix now covers relevant and irrelevant appends across the two replay
    lanes, the cache-enabled `Freshness::MaybeStale` stale-hit vs forced-replay branch,
    and both incremental branches (group-local and external-cache replay)
  - remaining blind spots are cache-get-error handling, exact age-boundary behavior,
    and the empty/no-replay-plan public surface

## Fault-Injection Harness

### Invariant: MaybeStale never serves corrupt cache bytes as a “fresh enough” success

- Harness pattern: `Fault-Injection Harness`
- Status: unmeasured
- Catalog invariants: `INV-CACHE-CAPABILITIES-EXPLICIT`, `INV-CLOCK-NOW-US-LIVE`
- Location:
  - `bpk-lib/crates/core/tests/projection_cache.rs`
- Command used:
  - `cargo test --test projection_cache freshness_maybe_stale_replays_when_stale_cache_bytes_are_corrupt`
  - `cargo test --test projection_cache freshness_maybe_stale_replays_when_fresh_cache_bytes_are_corrupt`
  - `cargo test --test projection_cache projection_replays_when_cache_get_errors`
  - `cargo test --test projection_cache freshness_maybe_stale_replays_at_exact_age_boundary`
  - `cargo test --test projection_cache empty_projection_surface_skips_cache_for_no_replay_plan`
  - `cargo test --test projection_cache consistent_replays_when_reopened_native_cache_row_is_stale`
  - `cargo test --test projection_cache maybe_stale_replays_when_cache_row_has_valid_metadata_but_empty_payload`
  - `cargo test --test projection_cache consistent_replays_when_cache_row_has_valid_metadata_but_truncated_payload`
- Line/function coverage delta: targeted rise in `bpk-lib/crates/core/src/store/projection/flow.rs`; exact JSON delta not recorded
- Mutation delta: unmeasured
- Remaining known blind spots:
  - this seam now proves that stale-but-young corrupt rows, fresh-but-corrupt rows, cache-get failures, and exact age-boundary rows all fall back to honest replay under `Freshness::MaybeStale`
  - coverage-closure sweep also pins empty/no-replay-plan behavior, reopened stale external-cache replay under `Freshness::Consistent`, and valid-metadata/undecodable-payload cache rows that bypass metadata corruption but still must replay honestly
  - remaining cache-edge blind spots are now limited to backend-specific OS error shapes that are difficult to force portably without changing production behavior

## Property Harness

### Invariant: Fuzz and chaos probe outputs stay within explicit policy gates

- Harness pattern: `Property Harness`
- Status: unmeasured
- Catalog invariants: `INV-CANONICAL-PATCH-STABILITY`, `INV-GROUP-COMMIT-IDEMPOTENCY`
- Catalog invariants: `INV-FAULT-INJECT-GATED`
- Location:
  - `bpk-lib/crates/core/tests/fuzz_chaos_feedback.rs`
- Command used:
  - `cargo test --test fuzz_chaos_feedback --all-features --release -- --ignored`
- Line/function coverage delta: unmeasured
- Mutation delta: unmeasured
- Remaining known blind spots:
  - feedback policy is explicit, but it does not replace direct seam-level
    fault-injection or state-machine proofs

### Invariant: Representative store errors keep stable handling, display, and source contracts

- Harness pattern: `Property Harness`
- Status: unmeasured
- Catalog invariants: `INV-CANONICAL-PATCH-STABILITY`
- Catalog invariants: `INV-STORE-ERROR-TAXONOMY`
- Location:
  - `bpk-lib/crates/core/tests/store_error_contract.rs`
  - `bpk-lib/crates/core/src/store/error.rs`
- Command used:
  - `cargo test --test store_error_contract`
  - `cargo test store::error::tests`
- Line/function coverage delta: targeted rise in `bpk-lib/crates/core/src/store/error.rs`; exact JSON delta not recorded
- Mutation delta: unmeasured
- Covered tests:
  - `store_error_contract_table_stays_stable` now includes direct public
    contract rows for helper-shaped `CorruptSegment` construction and
    fail-closed `InvariantViolation` display/classification.
  - `bpk-lib/crates/core/src/store/error.rs::tests::*_helper_*` directly exercises every
    `pub(crate)` `StoreError` helper constructor, including source-bearing
    batch/cache/serialization helpers and segment-corruption helpers.
- Remaining known blind spots:
  - none for the representative `StoreError` handling, `Display`, `source()`,
    conversion routing, and internal helper-constructor surface currently in
    scope.

### Invariant: Catastrophic performance regressions trip explicit thresholds

- Harness pattern: `Property Harness`
- Status: unmeasured
- Catalog invariants: `INV-CANONICAL-PATCH-STABILITY`
- Catalog invariants: `INV-PERFORMANCE-GATES-ENFORCED`
- Location:
  - `bpk-lib/crates/core/tests/perf_gates.rs`
- Command used:
  - `cargo xtask perf-gates`
- Line/function coverage delta: not applicable
- Mutation delta: not applicable
- Remaining known blind spots:
  - these are intentionally loose catastrophic guards, not precise benchmark
    baselines; stable trend authority belongs to `cargo xtask bench`

### Invariant: Typed payload kind allocation is binary-wide and collision-checked

- Harness pattern: `Property Harness`
- Status: unmeasured
- Catalog invariants: `INV-CANONICAL-PATCH-STABILITY`
- Catalog invariants: `INV-GENERATED-WITNESS-PIN`, `INV-MACRO-BOUNDED-CAST`
- Location:
  - `bpk-lib/crates/core/fixtures/downstream/src/lib.rs`
  - `bpk-lib/crates/core/tests/event_payload_registry_policy.rs`
  - `bpk-lib/crates/core/tests/event_payload_registry_downstream.rs`
- Command used:
  - `cargo test --manifest-path bpk-lib/crates/core/fixtures/downstream/Cargo.toml`
  - `cargo test --test event_payload_registry_policy`
  - `cargo test --test event_payload_registry_downstream`
- Line/function coverage delta: targeted rise in `bpk-lib/crates/core/src/event/payload.rs`,
  `bpk-lib/crates/core/src/store/config.rs`, and `bpk-lib/crates/core/src/store/mod.rs`; exact JSON delta not recorded
- Mutation delta:
  - `event-payload-registry-validator` critical seam is registered at the 85%
    smoke threshold for collision detection, open-time warn/fail-fast policy,
    and cache refresh.
- Covered tests:
  - `public_payload_registry_validator_reports_clean_registry` pins the clean
    registry path and explicit revalidation hook.
  - `store_open_accepts_explicit_payload_validation_policy_when_registry_is_clean`
    pins the public config policy surface.
  - `downstream_fixture_detects_dependency_event_kind_collision` pins
    dependency-crate collisions in a composing debug test binary.
  - `downstream_fixture_detects_dependency_event_kind_collision_in_release`
    pins the same inventory-registration behavior under release linkage.
- Remaining known blind spots:
  - the validator reports linked registrations, not whether a particular store
    will ever append every linked payload kind. Store open warns by default and
    can be made fail-fast, but explicit per-application allocation discipline
    remains the caller's responsibility.

### Invariant: Harness doctrine stays structurally enforceable

- Harness pattern: `Property Harness`
- Status: unmeasured
- Catalog invariants: `INV-CANONICAL-PATCH-STABILITY`
- Catalog invariants: `INV-ALLOW-IS-DESIGN`, `INV-NO-DEAD-CODE-SILENCERS`
- Location:
  - `bpk-lib/tools/integrity/src/harness_lints.rs`
- Command used:
  - `cargo test -p batpak-integrity harness_lints`
  - `cargo xtask structural`
- Line/function coverage delta: targeted rise in `bpk-lib/tools/integrity/src/harness_lints.rs`;
  exact JSON delta not recorded
- Mutation delta:
  - `testing-ledger-structural-lint` critical seam is registered at the 85%
    smoke threshold for ledger schema, command-prefix, location, module-header,
    and capped line-count enforcement.
- Covered tests:
  - `synthetic_well_formed_ledger_entry_is_accepted` pins a minimal valid
    doctrine-bearing ledger entry, tracked Rust file, header, and line cap.
  - `synthetic_malformed_ledger_entry_is_rejected` pins schema rejection for a
    missing required ledger field.
- Remaining known blind spots:
  - allowlist entries are explicit debt with reason and shrinkage target; new
    entries should be treated as review-visible debt, not routine bypasses.

### Invariant: Evidence report bodies keep deterministic structural identity

- Harness pattern: `Property Harness`
- Status: unmeasured
- Catalog invariants: `INV-CANONICAL-PATCH-STABILITY`
- Location:
  - `bpk-lib/crates/core/tests/evidence_report_family.rs`
  - `bpk-lib/crates/core/tests/lane_store_resource_evidence.rs`
  - `bpk-lib/crates/core/tests/schema_snapshot_report.rs`
  - `bpk-lib/crates/core/tests/chain_walk_evidence_report.rs`
  - `bpk-lib/crates/core/tests/subscriber_frontier_observations.rs`
  - `bpk-lib/crates/core/tests/projection_run_evidence_report.rs`
  - `bpk-lib/crates/core/tests/read_walk_evidence_report.rs`
- Command used:
  - `cargo test --test evidence_report_family`
  - `cargo test --test lane_store_resource_evidence`
  - `cargo xtask evidence-audit`
  - `cargo test --test schema_snapshot_report`
  - `cargo test --test chain_walk_evidence_report`
  - `cargo test --test subscriber_frontier_observations`
  - `cargo test --test projection_run_evidence_report`
  - `cargo test --test read_walk_evidence_report`
- Line/function coverage delta: unmeasured
- Mutation delta: unmeasured
- Covered tests:
  - family-wide tests pin canonical `body_hash`, metadata exclusion from body
    identity, deterministic finding order, no automatic append, domain-neutral
    public type names, close/reopen behavior, topology-independent
    read/projection evidence identity, and store resource evidence helpers plus
    reopen-stable configuration slice checks.
  - report-specific suites pin schema drift, chain continuity/corruption,
    subscriber loss/frontier precision, projection outcome frontier/cache/
    freshness/output truth, and read-walk visibility/proof-ref/count truth.
- Remaining known blind spots:
  - the v1 report family is covered as structural evidence; any new report body
    needs its own schema-version, canonical-body, findings-order, and
    append-boundary pins before promotion.

### Invariant: Lane A generic substrate — artifact envelope framing, compaction evidence, idempotency, and explicit read bounds

- Harness pattern: `Property Harness`
- Status: unmeasured
- Catalog invariants: `INV-CANONICAL-PATCH-STABILITY`, `INV-GROUP-COMMIT-IDEMPOTENCY`
- Location:
  - `bpk-lib/crates/core/tests/lane_a_artifact_substrate.rs`
  - `bpk-lib/crates/core/tests/lane_a_store_substrate.rs`
  - `bpk-lib/crates/core/tests/idempotent_batch_crash_recovery.rs` (close+reopen batch idempotency)
  - `bpk-lib/crates/core/src/artifact.rs`
  - `bpk-lib/crates/core/src/store/compaction_report.rs`
- Command used:
  - `cargo test --test lane_a_artifact_substrate`
  - `cargo test --test lane_a_store_substrate`
- Line/function coverage delta: targeted rise in `bpk-lib/crates/core/src/artifact.rs` and
  `bpk-lib/crates/core/src/store/compaction_report.rs`; exact JSON delta not recorded
- Mutation delta: unmeasured
- Covered tests:
  - body digest stable while artifact envelope metadata/signatures/attestations move
    envelope digest; canonical sort makes signature vector order and attestation vector order
    immaterial to `envelope_hash`; verification findings deterministic; free-function vs
    inherent method envelope digest parity; compaction skipped report stability
    and schema echo; `compaction_id` excludes post-hoc findings; `PreSwapRollback`
    structural finding; compaction helper constructors; finding order
    canonicalization in compaction report `body_hash`; performed-merge `compact_with_report` exercise
    in `bpk-lib/crates/core/tests/lane_a_store_substrate.rs` (sorted sources, stable `body_hash`, output digest);
    append idempotency key aliasing event id with global replay; keyed batch replay in-process and
    `IdempotencyPartialBatch` on mixed cached/new keys; explicit `Region` / `by_scope` /
    `stream` / `by_fact` / cursor surfaces (no hidden public full-store cursor); close+reopen batch
    replay in `bpk-lib/crates/core/tests/idempotent_batch_crash_recovery.rs`.
- Remaining known blind spots:
  - deep cryptographic signature verification is represented by signing and
    verifier harnesses; these tests use structural echo verifiers only.

### Invariant: Lane B1 attested registry — canonical row body, envelope composition, drift, verification, supersession

- Harness pattern: `Property Harness`
- Status: unmeasured
- Catalog invariants: `INV-CANONICAL-PATCH-STABILITY`
- Location:
  - `bpk-lib/crates/core/tests/lane_b1_registry_substrate.rs`
  - `bpk-lib/crates/core/src/registry.rs`
- Command used:
  - `cargo test --test lane_b1_registry_substrate`
- Line/function coverage delta: unmeasured
- Mutation delta: unmeasured
- Covered tests:
  - `named_digests` permutation invariance for `registry_row_body_hash`; normalized row bytes align
    with `artifact_body_hash_from_body` / signing bytes; drift report `body_hash` sorts findings;
    `verify_registry_attested_row` composes `CanonicalArtifactEnvelope` with normalized verify plane;
    lifecycle / row-id / row-hash mismatch findings; supersession dangling target, removed row with
    `supersedes`, simple cycle, and duplicate `row_id` in sorted catalog inputs.
- Remaining known blind spots:
  - full graph-theoretic SCC classification for supersession is not exposed; only deterministic
    structural findings used by callers today.

### Invariant: Lane B2 backup envelope — manifest body hash, segment sort, restore proof, attested envelope

- Harness pattern: `Property Harness`
- Status: unmeasured
- Catalog invariants: `INV-CANONICAL-PATCH-STABILITY`
- Location:
  - `bpk-lib/crates/core/tests/lane_b2_backup_envelope_substrate.rs`
  - `bpk-lib/crates/core/src/store/backup_envelope.rs`
- Command used:
  - `cargo test --test lane_b2_backup_envelope_substrate`
- Line/function coverage delta: unmeasured
- Mutation delta: unmeasured
- Covered tests:
  - segment permutation invariance for `backup_manifest_body_hash`; digest change moves hash;
    normalized manifest body aligns with `artifact_body_hash_from_body`; restore proof stable under
    observed permutation and finding-order canonicalization for `restore_proof_report_body_hash`;
    missing / unexpected / digest-mismatch segments; duplicate and inconsistent `segment_id` rows;
    `verify_backup_manifest_envelope` with `ManifestBodyHashMismatch`; envelope-only metadata changes
    `envelope_hash` without changing manifest body hash; MessagePack disk round-trip preserves hash.
- Remaining known blind spots:
  - restore proof does not execute a real store open; it compares digest maps only.

### Invariant: Lane B3 transition substrate — canonical event digest, allowed-edge report, finding order

- Harness pattern: `Property Harness`
- Status: unmeasured
- Catalog invariants: `INV-CANONICAL-PATCH-STABILITY`
- Location:
  - `bpk-lib/crates/core/tests/lane_b3_transition_substrate.rs`
  - `bpk-lib/crates/core/src/transition.rs`
- Command used:
  - `cargo test --test lane_b3_transition_substrate`
- Line/function coverage delta: unmeasured
- Mutation delta: unmeasured
- Covered tests:
  - `causes` permutation invariance for `state_transition_event_digest`; state change moves digest;
    legal transitions yield stable empty-finding report hashes; disallowed edges emit
    `InvalidTransition` with [`TRANSITION_INVALID_DISALLOWED_EDGE`]; unsorted causes and unsorted
    allowed-edge inputs emit structural findings; `state_transition_report_body_hash` sorts findings;
    sort-order helpers; normalized event digest parity; MessagePack round-trip on
    `state_transition_event_bytes`; `StateTransitionReport` alias + schema constants.
- Remaining known blind spots:
  - allowed-edge membership is a flat sorted list only (no labeled edge families or weights).

### Invariant: Lane B4 reservation ledger — structural lanes, invalid transitions, reconciliation buckets, body hashes

- Harness pattern: `Property Harness`
- Status: unmeasured
- Catalog invariants: `INV-CANONICAL-PATCH-STABILITY`
- Location:
  - `bpk-lib/crates/core/tests/lane_b4_reservation_substrate.rs`
  - `bpk-lib/crates/core/src/reservation.rs`
- Command used:
  - `cargo test --test lane_b4_reservation_substrate`
- Line/function coverage delta: unmeasured
- Mutation delta: unmeasured
- Covered tests:
  - reserve→commit / reserve→refund / reserve→expire happy paths; double-commit and refund-after-commit
    structural findings; duplicate reserve and invalid reserve units; commit without prior reserve;
    expire/refund/orphan without reserve; deterministic orphan + reconciliation report hashes; transition
    list permutation invariance after normalization; `cause_refs` sort stabilizes transition log digest;
    subject key sort normalizes subject bytes; ledger `body_hash` independent of finding vector order;
    `ReservationReconciliationReport` alias + schema constants exercised.
- Remaining known blind spots:
  - simulation is pure transition replay over an in-memory map; no store append or cross-session recovery.

## State-Machine Harness

### Invariant: Bounded schedules preserve concurrency protocol truth

- Harness pattern: `State-Machine Harness`
- Status: unmeasured
- Catalog invariants: `INV-CONCURRENCY-SCHEDULE-PROOF`, `INV-BATCH-ATOMIC-VISIBILITY`
- Location:
  - `bpk-lib/crates/core/tests/deterministic_concurrency.rs`
- Command used:
  - `cargo xtask loom`
- Line/function coverage delta: unmeasured
- Mutation delta: unmeasured
- Remaining known blind spots:
  - loom proofs cover bounded interleavings, not unbounded stress or real I/O

### Invariant: Durable cursor checkpoints only commit honest progress

- Harness pattern: `State-Machine Harness`
- Status: unmeasured
- Catalog invariants: `INV-DELIVERY-AT-LEAST-ONCE-WITNESS`
- Location:
  - `bpk-lib/crates/core/tests/cursor_durability.rs`
- Command used:
  - `cargo test --test cursor_durability`
- Line/function coverage delta: targeted rise in `bpk-lib/crates/core/src/store/delivery/cursor.rs`;
    exact JSON delta not recorded
- Mutation delta: unmeasured
- Remaining known blind spots:
  - committed progress vs rollback/restart semantics are covered, but this does
    not replace the broader cursor lifecycle tests in `bpk-lib/crates/core/tests/store_advanced.rs`

### Invariant: Ready writer tickets surface observable completion through `try_check`

- Harness pattern: `State-Machine Harness`
- Status: unmeasured
- Catalog invariants: `INV-JOURNAL-WRITER-SERIALIZES-COMMITS`, `INV-FENCE-CANCELLED-STAYS-HIDDEN`
- Location:
  - `bpk-lib/crates/core/tests/control_plane_surface.rs`
- Command used:
  - `cargo test --test control_plane_surface try_check_surfaces_ready_append_and_batch_tickets`
  - `cargo test --test control_plane_surface fenced_root_submit_stays_hidden_until_commit_and_cancel_discards_it`
  - `cargo test --test control_plane_surface fenced_batch_submit_stays_hidden_until_commit_and_cancel_discards_it`
  - `cargo test --test control_plane_surface fenced_reaction_submit_stays_hidden_until_commit_and_cancel_discards_it`
  - `cargo test --test control_plane_surface fenced_reaction_commit_preserves_reaction_metadata`
  - `cargo test --test control_plane_surface try_submit_batch_returns_retry_under_pressure`
  - `CARGO_INCREMENTAL=0 cargo mutants --output ../target/xtask-mutants/writer-commit-ticket-try-check-none --in-place --baseline run --file 'crates/core/src/store/write/*.rs' --exclude crates/core/src/store/ancestry/by_clock.rs --all-features --cargo-arg --locked --test-tool cargo --shard 1/8 --sharding round-robin --build-timeout 300 --timeout 300 --minimum-test-timeout 120 -F 'Ticket<T>::try_check.*with None'`
  - `CARGO_INCREMENTAL=0 cargo mutants --output ../target/xtask-mutants/fence-token-root-under-fence-4 --in-place --baseline run --file 'crates/core/src/store/write/control.rs' --all-features --cargo-arg --locked --test-tool cargo --build-timeout 300 --timeout 300 --minimum-test-timeout 120 -F 'delete field fence_token from struct Self expression in AppendSubmission::root_under_fence'`
- Line/function coverage delta: targeted rise in `bpk-lib/crates/core/src/store/write/control.rs`; exact JSON delta not recorded
- Mutation delta:
  - exact mutant `bpk-lib/crates/core/src/store/write/control.rs:29:9 replace Ticket<T>::try_check -> Option<Result<T, StoreError>> with None` is now caught by the ready-path proof lane
  - the exact default-receipt mutants for `AppendTicket::try_check` and `BatchAppendTicket::try_check` are now characterized as unviable at build time:
    - `bpk-lib/crates/core/src/store/write/control.rs:64:9 replace AppendTicket::try_check -> Option<AppendReply> with Some(Default::default())`
    - `bpk-lib/crates/core/src/store/write/control.rs:96:9 replace BatchAppendTicket::try_check -> Option<BatchAppendReply> with Some(Default::default())`
  - exact field-deletion mutants in the fence/reaction submission constructors are now caught:
    - `bpk-lib/crates/core/src/store/write/control.rs:551:13 delete field fence_token from struct Self expression in AppendSubmission::root_under_fence`
    - `bpk-lib/crates/core/src/store/write/control.rs:562:17 delete field causation_id from struct AppendOptions expression in AppendSubmission::reaction`
    - `bpk-lib/crates/core/src/store/write/control.rs:575:13 delete field fence_token from struct Self expression in AppendSubmission::reaction_under_fence`
- Remaining known blind spots:
  - this closes the positive-ready edge for append and batch tickets and adds direct root-under-fence, batch-under-fence, reaction-under-fence visibility/cancel, and reaction metadata-preservation proofs
  - batch pressure-retry symmetry is now pinned alongside append pressure-retry, but the wider writer commit protocol still needs broader mutation pressure across `writer.rs`, `staging.rs`, and `fanout.rs`

## Oracle Harness

### Invariant: Public query and cursor surfaces match a linear reference scan across topologies

- Harness pattern: `Oracle Harness`
- Status: unmeasured
- Catalog invariants: `INV-INDEX-FILTER-COMPOSES`, `INV-MULTI-VIEW-PUBLISH-AFTER-VIEW-SYNC`
- Location:
  - `bpk-lib/crates/core/tests/index_filter_composition.rs`
- Command used:
  - `cargo test --test index_filter_composition`
  - `cargo test --test index_filter_composition reopen_matches_live_oracle_across_topologies`
- Line/function coverage delta: targeted rise in `bpk-lib/crates/core/src/store/index/columnar.rs` and `bpk-lib/crates/core/src/store/index/mod.rs`; exact JSON delta not recorded
- Mutation delta: unmeasured
- Remaining known blind spots:
  - the oracle now owns filter composition, cursor batch ordering, and live-vs-reopen parity across topologies
  - remaining blind spots are deeper restore-artifact mismatches outside this pure query surface, which still belong to cold-start parity suites rather than the overlay oracle itself

### Invariant: Public topology diagnostics match configured overlay posture

- Harness pattern: `Oracle Harness`
- Status: unmeasured
- Catalog invariants: `INV-COLUMNAR-REPLACES-DASHMAP`
- Location:
  - `bpk-lib/crates/core/tests/index_topology.rs`
- Command used:
  - `cargo test --test index_topology`
- Line/function coverage delta: unmeasured
- Mutation delta: unmeasured
- Covered tests:
  - constructor checks pin the public presets to their intended overlay sets.
  - diagnostics checks pin `index_topology` labels and `tile_count` reporting
    for base, scan, entity-local, tiled, tiled-simd, and all-overlay postures.
- Remaining known blind spots:
  - this suite proves diagnostic truth for topology posture; query/cursor
    semantic equivalence remains owned by the linear-reference oracle above.

## Structural Harness

### Invariant: Structural gates own non-runtime policy and bridge coverage

- Harness pattern: `Structural Harness`
- Status: unmeasured
- Catalog invariants: `INV-NO-TOKIO-PROD`, `INV-STORE-SYNC-ONLY`, `INV-PAYLOAD-LENGTH-EXACT`, `INV-TRACEABILITY-COMPLETE`, `INV-CANONICAL-CONTAINER-CI`, `INV-CHECKPOINT-V2-INTERNED`, `INV-SIDX-TIMESTAMP-US-APPROXIMATION`, `INV-EXAMPLES-OBSERVABLE-OUTPUT`, `INV-LITERAL-REGEX-UNWRAP-SAFE`, `INV-TYPESTATE-OPEN-HAS-WRITER`, `INV-COORDINATE-IS-LOGICAL-STREAM`, `INV-CROSS-DIRECTORY-CONSISTENCY-PRODUCT-OWNED`, `INV-CONTEXT-VIEWS-DERIVED-FROM-HISTORY`
- Location:
  - `bpk-lib/tools/integrity/src/invariant_bridge.rs`
  - `bpk-lib/tools/integrity/src/structural.rs`
  - `bpk-lib/tools/integrity/src/ci_parity.rs`
  - `bpk-lib/tools/integrity/src/architecture_lints/syncbat_boundary.rs`
  - `bpk-lib/tools/xtask/src/main.rs`
  - `bpk-lib/crates/core/tests/typestate_safety.rs`
  - `bpk-lib/crates/core/tests/mmap_cold_start.rs`
  - `bpk-lib/crates/core/tests/wire_format.rs`
- Command used:
  - `cargo xtask structural`
  - `cargo xtask boundary`
  - `cargo xtask stale-paths`
  - `cargo xtask evidence-audit`
  - `cargo test --test typestate_safety`
  - `cargo test --test mmap_cold_start`
  - `cargo test --test wire_format`
- Line/function coverage delta: unmeasured
- Mutation delta: unmeasured
- Covered tests:
  - the invariant bridge now hard-fails any catalog invariant that has neither
    a direct test artifact nor a ledger/waiver witness, so structural policy
    drift cannot remain a green-but-noisy warning.
  - structural owns runtime-boundary policy, stale path checks, CI/devcontainer
    parity, allow/dead-code discipline, example-output doctrine, and traceability
    graph completeness.
  - targeted core suites pin typestate writer availability, cold-start/SIDX
    restore behavior, and canonical wire/payload length boundaries.
- Remaining known blind spots:
  - this entry covers structural and doctrine-shaped invariants; runtime behavior
    still belongs in direct test artifacts whenever a catalog invariant can be
    exercised as code.
