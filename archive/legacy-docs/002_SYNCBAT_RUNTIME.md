# 002 Syncbat Runtime

`syncbat` is the sync-first runtime layer over batpak.

It owns the operation catalog, runtime builder, checkout dispatch, handler
registry, invocation context, receipt envelope, and store-backed receipt sink.

Short form:

```text
sb runs.
```

## Boundary

`syncbat` depends on `batpak`. It does not depend on `netbat`,
LiteShip, or PCP semantics. It stays synchronous and does not introduce a
production async runtime.

## Main Types

- `Core`
- `CoreBuilder`
- `Ctx`
- `Module`
- `OperationDescriptor`
- `OperationRegisterItem`
- `Register`
- `CacheRegister`
- `CheckoutFrame`
- `Checkout`
- `ReceiptEnvelope`
- `StoreReceiptSink`
- `StoreRegisterCatalog`
- `RegisterOperationRowV1`
- `RegisterOperationActionV1`

## Durable Register

`Register` is the catalog of operation descriptors. `CacheRegister` is a hot
lookup projection over a register and is not truth.

`StoreRegisterCatalog` persists register rows into batpak as typed catalog
events, and `rebuild_register_from_store` rebuilds a `Register` from those rows.
Rows are folded in store sequence order.

| Action | Writer API | Rebuild behavior |
| --- | --- | --- |
| `put` | `persist_operation` / `persist_register` | inserts a new descriptor or repeats the same descriptor idempotently |
| `update` | `update_operation` | explicitly replaces fields for an active descriptor |
| `delete` | `delete_operation` | writes a terminal tombstone; deleted descriptors are omitted |
| `supersede` | `supersede_operation` | tombstones one operation name and activates a replacement descriptor |

Writer APIs preflight the current catalog state before appending lifecycle rows;
raw malformed lifecycle rows and invalid transitions still fail closed during
rebuild. `syncbat` keeps these as typed runtime-catalog rows; Lane B registry
rows remain an evidence projection boundary rather than the live runtime storage
format.

Delete and supersede tombstones are distinct terminal states. A duplicate
delete is idempotent, and a duplicate exact supersede is idempotent, but changing
which terminal state a name entered is rejected during rebuild.

## Layer Contract

Macros may declare descriptors and register items, but callers still assemble a
runtime explicitly. Runtime dispatch emits receipts only through the configured
sink. Batpak records the durable facts.

The normative runtime contract is ADR-0028. The public API is locked by
`bpk-lib/traceability/public_api/syncbat.txt`.
