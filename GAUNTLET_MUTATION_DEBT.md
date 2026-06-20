# Mutation debt — pulled from cloud run 27836115610 (#121 branch, PRE-cure)

Source: Integrity CI run 27836115610 on `feat/0.9.0-fork-import` (2026-06-19, before
the `0667bbc` cure commit). These are the *known* survivors that run identified.
Cross-check every one against the cures now on `feat/0.9.0-fork-import@0667bbc`
("kill 24 surviving mutants") + the inline cures on `feat/gauntlet-phase-2-fuzz@d6e6205`.
Anything not provably killed by an existing test is a remaining gap to cure (by
reasoning; cloud confirms the kill).

## projection-fusion — 78% (4 missed)
- fusion.rs:113:5  replace `fused_relevant_kinds  -> Vec<EventKind>` with `vec![]`
- fusion.rs:124:5  replace `fused_relevant_kinds3 -> Vec<EventKind>` with `vec![]`
- fusion.rs:132:5  replace `collect_relevant_kinds -> Vec<EventKind>` with `vec![]`
- fusion.rs:142:16 delete `!` in `collect_relevant_kinds`

## projection-flow — 75% (5 missed)
- registry.rs:87:81 replace `!=` with `==` in `ProjectionRegistry::unregister`
- fusion.rs:113 / 124 / 132 / 142 (same four as projection-fusion)

## writer-commit — 67% (14 missed + 1 timeout)
- publish.rs:39:34 replace `>` with `>=` in `lane_publish_points_from_notifications`
- watermark.rs:50:9   `LaneWatermarks::for_bootstrap -> Self` => `Default::default()`
- watermark.rs:92:17  replace `-` with `+` in `LaneWatermarks::view`
- watermark.rs:142/150/174/183/192/201/210/228:9  `WatermarkAdvanceHandle::wait_for_* -> Result` => `Ok(())` (8 handles)
- watermark.rs:321:24 replace `>=` with `<` in `wait_for_watermark_on_lane`
- watermark.rs:431:9  `WatermarkState::for_bootstrap -> Self` => `Default::default()`
- watermark.rs:533:9  `WatermarkState::advance_visible_and_emitted` => `()`
- (+1 timeout — see timeout.txt; investigate separately)

## lane-branch — 87% (7 missed + 1 timeout)  [score already >= 85; failed on the timeout]
- coordinate/mod.rs:339:9   `Region::matches_event -> bool` => `true`
- index/mod.rs:615:76       replace `<` with `==`, `>`, `<=` in `cancel_visibility_fence` (3 mutants)
- index/query.rs:281:39     replace `!=` with `==` in `query_any_hits_after`
- index/query.rs:282:25     replace `||` with `&&` in `query_any_hits_after`
- index/query.rs:282:28     delete `!` in `query_any_hits_after`

## NEVER MEASURED (cancelled / timed out — no missed list exists yet)
- lane-frontier  — cancelled (6h budget). Needs cloud re-run, likely sharded.
- repo-wide ratchet — cancelled. This is the decision-1 baseline measurement; must run in cloud.
- "Mutation full (shard)" — skipped.

## Confirmed GREEN in that run (no action)
segment-scan, cursor-delivery, hash-chain-replay, frontier-wait-durable,
event-payload-registry-validator, netbat-boundary-protocol, frontier-append-gate,
testing-ledger-structural-lint, syncbat-runtime-dispatch, platform-backend,
syncbat-register-catalog, fork-isolation, import-reapply.
