# Gauntlet Mutation Debt

Surviving / hard-to-kill mutants from cloud mutation runs, with the cure status of
each. Curing (a behavioral test that asserts the exact observable the mutant flips)
is always preferred; a mutant lands in the *debt* sections below only when a kill is
genuinely unreachable without changing production logic beyond a small testability
seam, or when the difference is non-deterministic (timing) rather than a state
difference a single targeted test can pin. Cloud confirms every claimed kill.

---

## 2026-06-21 — smoke shard 0/48 cure pass (cloud run 27888953019)

Source: the always-on `mutants-smoke-repo-wide` lane (shard 0 of 48) on
`feat/gauntlet-phase-2-fuzz` — **82% (42 caught / 51 scored)**; `76` mutants in the
shard, `9 missed + 1 timeout + 24 unviable`. Cures landed on
`feat/0.9.0-integration` (tests-only; verified passing on real code; cloud confirms
the kills). This is a representative shard, NOT the full repo — the true repo-wide
floor still needs a `mutants-full proof=heavy` cloud run on the post-gauntlet
feature repo.

### Cured (test added, passes on real code)

| Mutant | Killing test |
|---|---|
| `registry.rs:166` `+=`→`*=` (sorted drift-merge cursor advance) | `registry::tests::drift_merge_advances_both_cursors_across_every_branch_kind` + `…_drains_observed_tail_as_extra_rows` |
| `reservation.rs:78` `partial_cmp`→`None` | `reservation::tests::partial_cmp_orders_by_namespace_then_key_bytes` |
| `lifecycle.rs:101` `> 0`→`>= 0` (snapshot DestinationCleared) | `tests/store_snapshot_compaction.rs::snapshot_into_fresh_destination_reports_no_destination_cleared` |
| `segment/mod.rs:407` `<`→`==` (trusted compaction-copy corruption guard) | `store::segment::boundary_tests::append_frames_from_segment_accepts_trusted_empty_frame_region` |
| `platform/clock.rs:136` `now_wall_ns` body→`1` | `store::platform::clock::tests::system_clock_now_wall_ns_reports_real_wall_time` |
| `sim/workload.rs:43` `usize_token` body→`0` | `store::sim::workload::tests::usize_token_widens_value_losslessly` |
| `delivery/cursor/worker.rs:76` match-guard `load_saved_checkpoint`→`true` | `store::delivery::cursor::worker::tests::build_worker_cursor_honors_the_load_saved_checkpoint_flag` |

### Debt — hard to kill without a production change (filed, not cured)

#### `batch.rs:443` — `prepared.len() - 1` → `prepared.len() + 1` (COMMIT-marker last-item index)

The 4th arg to `write_batch_marker_frame(… SYSTEM_BATCH_COMMIT, 0, prepared.len()-1, false)`
is `item_index_for_error` — a **purely diagnostic** field stamped onto
`StoreError::BatchFailed { item_index, .. }` *iff* writing the COMMIT marker frame
fails. It drives no control flow, recovery, or durable state (confirmed: the only
consumers are `BatchFailed`'s `Display` + a `tracing::debug!`). For the COMMIT
marker the error is unreachable from a test: Validation is pre-empted by the BEGIN
marker under `MonotonicClock` clamping; Encoding can't fail for a zero-payload
marker; Syncing is skipped (`allow_rotation=false`); Writing goes straight to the
segment's raw `std::fs::File` with **no `StoreFs`/`InjectionPoint` fault seam**.
**Kill requires** a new `InjectionPoint::BatchCommitMarkerWriting` before the COMMIT
`write_frame` + a `dangerous-test-hooks` test asserting `item_index == len-1` — a
production change for a diagnostic-only field. Low severity; deferred.

#### `writer.rs:250` — `WriterHandle::close_channel_and_join` body → `()`

`close_channel_and_join(self)` drops `tx` then `thread.join()`s to quiescence. The
`()` mutant still drops `tx` and the `JoinHandle` (via end-of-scope drop of the
moved-in `self`) — so the channel still closes, but the thread is **detached, not
joined**. Identical *eventual* durable state; the only difference is whether the
call blocks until the writer thread fully stops before `SimFs::crash()`. That is a
**timing race**, not a state difference — the recovery_matrix oracle
(`recovered_visible >= durable_acked`, rare syncs) can pass or fail by scheduling.
Not equivalent (it weakens crash-consistency), but not deterministically killable by
one targeted test; forcing a fake assertion would be dishonest. **Net:** the
multi-seed `recovery_matrix` cloud lanes are the right probabilistic net; a
deterministic kill needs a `dangerous-test-hooks` barrier blocking the writer
mid-fsync.

### Timeout (already counts as caught — no action required)

#### `runtime.rs:104` — `if !budget_ok` → `if budget_ok` (writer restart-budget guard)

Reported **TIMED OUT** by the cloud run, which cargo-mutants counts as **caught**.
Inverting the guard makes a budget-exhausted panic restart instead of terminally
exit, so `tests/store_restart_policy.rs::writer_restart_once_gives_up_after_second_panic`
spins its 5s append deadline and fails — a real (slow) kill. Optional speed-up (not
done, to avoid a flaky tight-timing assertion): poll `fail_if_exited()` with a short
bound after budget exhaustion.

---

## 2026-06-19 — #121 survivors (cloud run 27836115610, PRE-cure)

Source: Integrity CI run 27836115610 on `feat/0.9.0-fork-import` (before the
`0667bbc` cure commit). The *known* survivors that run identified; cross-checked
against the cures on `feat/0.9.0-fork-import@0667bbc` ("kill 24 surviving mutants")
+ the inline cures on `feat/gauntlet-phase-2-fuzz@d6e6205`. All 30 confirmed cured.

### projection-fusion — 78% (4 missed)
- fusion.rs:113:5  replace `fused_relevant_kinds  -> Vec<EventKind>` with `vec![]`
- fusion.rs:124:5  replace `fused_relevant_kinds3 -> Vec<EventKind>` with `vec![]`
- fusion.rs:132:5  replace `collect_relevant_kinds -> Vec<EventKind>` with `vec![]`
- fusion.rs:142:16 delete `!` in `collect_relevant_kinds`

### projection-flow — 75% (5 missed)
- registry.rs:87:81 replace `!=` with `==` in `ProjectionRegistry::unregister`
- fusion.rs:113 / 124 / 132 / 142 (same four as projection-fusion)

### writer-commit — 67% (14 missed + 1 timeout)
- publish.rs:39:34 replace `>` with `>=` in `lane_publish_points_from_notifications`
- watermark.rs:50:9   `LaneWatermarks::for_bootstrap -> Self` => `Default::default()`
- watermark.rs:92:17  replace `-` with `+` in `LaneWatermarks::view`
- watermark.rs:142/150/174/183/192/201/210/228:9  `WatermarkAdvanceHandle::wait_for_* -> Result` => `Ok(())` (8 handles)
- watermark.rs:321:24 replace `>=` with `<` in `wait_for_watermark_on_lane`
- watermark.rs:431:9  `WatermarkState::for_bootstrap -> Self` => `Default::default()`
- watermark.rs:533:9  `WatermarkState::advance_visible_and_emitted` => `()`
- (+1 timeout — see timeout.txt; investigate separately)

### lane-branch — 87% (7 missed + 1 timeout)  [score already >= 85; failed on the timeout]
- coordinate/mod.rs:339:9   `Region::matches_event -> bool` => `true`
- index/mod.rs:615:76       replace `<` with `==`, `>`, `<=` in `cancel_visibility_fence` (3 mutants)
- index/query.rs:281:39     replace `!=` with `==` in `query_any_hits_after`
- index/query.rs:282:25     replace `||` with `&&` in `query_any_hits_after`
- index/query.rs:282:28     delete `!` in `query_any_hits_after`

### NEVER MEASURED (cancelled / timed out — no missed list exists yet)
- lane-frontier  — cancelled (6h budget). Needs cloud re-run, likely sharded.
- repo-wide ratchet — cancelled. This is the decision-1 baseline measurement; must run in cloud.
- "Mutation full (shard)" — skipped.

### Confirmed GREEN in that run (no action)
segment-scan, cursor-delivery, hash-chain-replay, frontier-wait-durable,
event-payload-registry-validator, netbat-boundary-protocol, frontier-append-gate,
testing-ledger-structural-lint, syncbat-runtime-dispatch, platform-backend,
syncbat-register-catalog, fork-isolation, import-reapply.
