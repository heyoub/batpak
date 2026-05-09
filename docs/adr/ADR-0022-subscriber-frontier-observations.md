# ADR-0022: Subscriber Frontier Observations

## Status
Accepted.

## Context
batpak push subscriptions are intentionally lossy: slow subscribers are pruned
instead of pacing the writer. Cursor-backed consumers can provide stronger
delivery evidence, but there was no small public observation surface that
expressed consumed frontier, available frontier, lag, and loss precision in a
deterministic report body.

## Decision
batpak adds `Store::subscriber_frontier_observation` and
`crates/core/src/store/subscriber_frontier.rs` as a minimal structural evidence surface.

### 1) Report-body shape

`SubscriberFrontierReportBody` carries:

- schema version
- source lane (`LossyPush` or `CursorBacked`)
- consumed frontier sequence (optional)
- available frontier sequence
- lag in events (optional)
- delivery state
- loss precision
- deterministic findings

`SubscriberFrontierEvidenceReport` wraps the body with metadata outside
deterministic identity (`generated_at_unix_ms`, `batpak_version`, `diagnostics`)
following ADR-0019.
Findings are deterministic and sorted in structural order.

### 2) Precision and honesty model

This surface is observation-first:

- exact dropped ranges are emitted only when supplied and precision is
  `ExactRange`
- unknown consumed frontier and delivery state are represented explicitly in
  findings
- unknown loss precision remains explicit in the report body; `LossObserved` is
  reserved for non-`Unknown` precision so the report does not claim observed
  loss when only the precision is unknown
- lossy push subscribers remain observation-only; no fake durability claims

### 3) Source-specific available frontier

- `LossyPush` compares consumed frontier against `frontier.emitted_hlc`
- `CursorBacked` compares consumed frontier against `frontier.current_visible_hlc`

This keeps the report anchored to substrate-observable frontiers without policy
interpretation.

## Non-goals

This slice does not add:

- retry or backpressure policy
- UI/agent semantics
- protocol/application law
- durable guarantees for lossy push channels

## Consequences

- downstream crates can inspect deterministic subscriber-frontier observations
  instead of guessing
- lossy/drop/disconnect outcomes become explicit findings when observed
- unknown precision is explicit in the body, not hidden behind implied exactness

## References

- `crates/core/src/store/subscriber_frontier.rs`
- `crates/core/src/store/write/fanout.rs`
- `crates/core/src/store/delivery/subscription.rs`
- `docs/adr/ADR-0019-canonical-encoding-contract.md`
