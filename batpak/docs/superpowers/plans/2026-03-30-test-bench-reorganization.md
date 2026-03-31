# Test And Bench Reorganization Record

## Status

This reorganization wave is complete. The repo now uses the clearer test and benchmark layout introduced on 2026-03-30, and this document records the resulting structure instead of the historical rename plan.

## Final Test Layout

- `tests/store_properties.rs`: algebraic properties, replay determinism, idempotency, commutativity, and builder-surface behavior.
- `tests/store_edge_cases.rs`: edge conditions, lifecycle, concurrent append checks, and drop/config behavior.
- `tests/perf_gates.rs`: self-dogfooding performance gates.
- `tests/event_api.rs`: event, ID, header, kind, and coordinate-adjacent API witnesses.
- `tests/store_advanced.rs`: advanced integration behavior that still benefits from grouped coverage.
- `tests/store_projection_wiring.rs`: projection-cache wiring and prefetch capability checks.
- `tests/store_restart_policy.rs`: restart-policy behavior behind the `test-support` feature.
- `tests/observability_flows.rs`: trace-emission proofs for named store flows.
- `tests/replay_consistency.rs`: replay-divergence and checkpoint-equivalence coverage.

## Final Benchmark Layout

- `benches/write_throughput.rs`
- `benches/cold_start.rs`
- `benches/projection_latency.rs`
- `benches/compaction.rs`
- `benches/subscription_fanout.rs`

`projection_latency.rs` now includes cache-backed benchmark groups for `redb` and `lmdb` in addition to the baseline replay measurements.

## Integrity Notes

- The renamed and split tests are now reflected in `SPEC.md`, `SPEC_REGISTRY.md`, CI, and the machine-readable traceability registry.
- Historical file names are intentionally omitted here so structural drift checks can treat them as stale references everywhere else in the repo.
- The current canonical verification path is `just ci` from `batpak/`, backed by `doctor`, `traceability-check`, and `structural-check`.
