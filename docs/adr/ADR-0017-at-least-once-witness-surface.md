# ADR-0017: At-Least-Once Witness Surface

## Status
Accepted

## Context
`ObservedOnce` is the exactly-once composition witness: it combines a
substrate-supplied at-least-once delivery witness with a caller-supplied
idempotency key. Before this decision, `ObservedOnce` was public, but the
substrate witness was only constructible in crate tests and was not delivered
to cursor or typed-reactor handlers.

That made the public witness surface half-built. Downstream code could see the
exactly-once composition type, but had no substrate-minted proof to pass into
it. The remaining workaround was to reason from checkpoint configuration in
prose, outside the type system.

## Decision
Plumb `Option<&AtLeastOnce>` through the cursor and typed-reactor handler
surfaces:

- `cursor_worker` handlers receive `Option<&AtLeastOnce>` as their third
  parameter.
- `TypedReactive::react` receives the same witness parameter.
- `MultiReactive::dispatch`, and therefore `react_loop_multi` /
  `react_loop_multi_raw` generated handlers, receive the same witness
  parameter.

The worker mints the witness once from `CursorWorkerConfig::checkpoint_id` at
startup. A durable checkpoint-backed worker receives `Some(&AtLeastOnce)` on
every delivered batch. An ephemeral worker with no checkpoint receives `None`.

`AtLeastOnce::new` and `AtLeastOnce::from_cursor_callback` remain
`pub(crate)`: the substrate is the only minter. External callers receive the
witness from handler parameters and may inspect it through
`AtLeastOnce::checkpoint_id(&self) -> &CheckpointId`.

## Alternatives Considered
- Make `AtLeastOnce` publicly constructible. Rejected because it turns the
  witness into a caller promise rather than substrate proof.
- Add a witness accessor to cursor/reactor configuration. Rejected because it
  proves only configuration intent, not actual delivery through the substrate
  handler path.
- Defer until a future major release. Rejected because the project is pre-1.0
  and the existing public exactly-once type was not useful without a real
  witness source.

## Consequences
This is a breaking handler-signature change. Existing `cursor_worker`,
`react_loop_typed`, `react_loop_multi`, and `react_loop_multi_raw` callers must
accept a new third parameter. Callers that do not need exactly-once composition
can name it `_witness` and ignore it.

The witness asymmetry is intentional: `Some` means the worker is backed by a
durable checkpoint id and can prove at-least-once delivery for that cursor
identity; `None` means the worker is process-local only.

Exactly-once composition now has a type-level path:

1. handler receives `Some(witness)`;
2. handler supplies an `IdempotencyKey`;
3. handler calls `ObservedOnce::new(witness.clone(), key)`.

## References
- [ADR-0001: Sync-Only Store API](ADR-0001-sync-only-store.md)
- [ADR-0011: Reactor Canal](ADR-0011-reactor-canal.md)
- `traceability/invariants.yaml`: `INV-DELIVERY-AT-LEAST-ONCE-WITNESS`
- `traceability/flows.yaml`: `FLOW-DELIVERY-AT-LEAST-ONCE-WITNESS`
- `tests/cursor_at_least_once_witness.rs`
