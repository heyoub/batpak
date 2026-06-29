# ADR-0028: Syncbat Runtime Contract

## Status

Accepted for the 0.7.6 correction cut.

## Context

R4 hardened `batpak` core into the substrate boundary: durable event storage,
visibility, receipts, evidence, clocks, and delivery canals are owned by core.
After retiring the in-workspace Rust downstream kit crate, `syncbat` is the
runtime layer between application code and the substrate.

`syncbat` is intentionally small, sync, and explicit. Callers register
operations, dispatch byte-oriented checkouts, and may persist operation receipts
through batpak-owned append surfaces.

## Decision

`syncbat` owns these runtime semantics:

- five composition entry points on `CoreBuilder`: `mount`,
  `register_operation`, `register_handler`, `register`, and `register_item`
- operation descriptor validation and stable operation-name grammar
- descriptor-to-handler bijection before a `Core` is built
- checkout dispatch from operation name plus input bytes to handler output bytes
- handler failure classification through `RuntimeError::Handler`
- optional receipt emission through a configured `ReceiptSink`
- durable operation-catalog rows through `StoreRegisterCatalog`
- deterministic catalog rebuild from batpak store sequence order

`syncbat` does not own network framing, async runtimes, application kit
vocabulary, an external context-profile spec, MCP, browser, agent semantics, or native batpak store internals
beyond public API calls.

## Build Contract

`CoreBuilder::build` validates the complete runtime shape before it returns a
`Core`.

The build contract is:

- each descriptor name is valid under the syncbat operation-name grammar
- each handler name is valid under the same grammar
- descriptors are unique by name
- handlers are unique by name
- every descriptor has exactly one handler
- every handler has exactly one descriptor
- mounted modules must supply valid descriptors and matching handlers

`BuildError` is the closed diagnostic surface for these failures:

- `DuplicateOperation`
- `DuplicateHandler`
- `MissingDescriptor`
- `MissingHandler`
- `InvalidModule`
- `InvalidOperation`
- `InvalidHandler`

## Register Types

`syncbat` uses three register forms with separate jobs:

- `Register` is an in-memory, in-process descriptor table used while composing
  runtime surfaces.
- `CacheRegister` is a hot projection for fast descriptor lookup. It is never a
  source of truth.
- `StoreRegisterCatalog` is the durable catalog writer and rebuild source of
  truth. It folds catalog rows from batpak store sequence order.

The durable catalog is append-shaped because runtime registration history is
substrate state. Rebuild is deterministic because row order is store order.

## Receipt Contract

When a checkout resolves to a registered operation and handler:

- a successful handler emits `ReceiptOutcome::Completed` when a receipt sink is
  configured
- a failed handler emits `ReceiptOutcome::Failed` when a receipt sink is
  configured
- `ReceiptOutcome::Denied` is reserved for direct receipt sinks, policy gates,
  and network guards that reject a call before handler execution
- receipt-sink failure is fail-closed and surfaces as
  `RuntimeError::ReceiptSink`
- when the sink fails while recording a failed-handler receipt,
  `RuntimeError::ReceiptSink.caused_by_handler` carries the handler class and
  message that produced the failed receipt
- unknown operations do not emit receipts

Handler failure classes are wire commitments:

- `invalid_input`
- `failed`

Receipt hashes are controlled by `ReceiptHashPolicy`. The default policy leaves
hash fields empty. `RawBytes` hashes input/output bytes with a deterministic
caller-owned hasher and does not change handler behavior.

`StoreReceiptSink` owns these signed batpak receipt-extension keys under the
`syncbat` namespace:

- `syncbat.descriptor`
- `syncbat.kind`
- `syncbat.outcome`
- `syncbat.input`
- `syncbat.output`
- `syncbat.signed`

## Dispatch Lifecycle

`Core::invoke`, `Core::checkout_frame`, and `Core::checkout` share the same
dispatch semantics.

The dispatch order is:

1. Resolve the descriptor by operation name.
2. Resolve the handler by the same operation name.
3. Run the handler synchronously on the caller thread.
4. Build a runtime receipt when a receipt sink is configured.
5. Return handler output only after receipt recording succeeds.

If the handler fails, runtime dispatch builds a failed receipt first. If that
receipt cannot be recorded, dispatch discards the handler output path and
returns `RuntimeError::ReceiptSink` with the handler cause attached. If the
receipt is recorded, dispatch returns `RuntimeError::Handler`.

`Core::checkout` re-resolves the descriptor by name against the runtime's
mounted descriptor table. The descriptor carried by the incoming `Checkout`
proves caller intent; it is not trusted as the runtime descriptor.

## Dispatch Errors

`RuntimeError` is the closed diagnostic surface for checkout failures:

- `UnknownOperation`: no descriptor is mounted for the requested name
- `MissingHandler`: the descriptor exists but no handler is mounted
- `Handler`: the handler returned `HandlerError`
- `ReceiptSink`: the configured receipt sink rejected a runtime-emitted receipt

`ReceiptSink.caused_by_handler` is `None` for completed-receipt sink failures.
It is `Some(ReceiptSinkHandlerCause)` when a handler failed first and the sink
then rejected the failed receipt.

## Catalog Contract

`StoreRegisterCatalog` persists `RegisterOperationRowV1` events at one
caller-chosen coordinate. Rows fold in store sequence order.

Valid lifecycle actions are:

- `put`
- `update`
- `delete`
- `supersede`

The writer API is online-strict: it rejects duplicate deletes, repeated
supersession, malformed rows, invalid descriptors, invalid effects, unsupported
schema versions, and conflicting lifecycle transitions before appending new
catalog state.

Rebuild is recovery-permissive: it folds already-durable rows deterministically
from store sequence order and fails closed when the durable sequence cannot
produce one coherent catalog state.

Delete and supersede tombstones are terminal. A name cannot be reused after a
tombstone. A supersession replacement may exist only if its descriptor is
identical to the incoming replacement descriptor.

## Consequences

- `syncbat` has a checked public API baseline.
- `syncbat` tests cite catalog and dispatch invariants directly.
- Application kits consume `syncbat`; they do not live inside `batpak`.
- `netbat` may expose `syncbat`, but it cannot reinterpret runtime semantics.

## References

- `002_SYNCBAT_RUNTIME.md`
- `bpk-lib/crates/syncbat/src/`
- `bpk-lib/crates/syncbat/tests/`
- `bpk-lib/traceability/public_api/syncbat.txt`
