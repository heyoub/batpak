# 002 Syncbat

`syncbat` is the sync-first runtime layer over batpak.

It owns the operation catalog, runtime builder, checkout dispatch, handler
registry, invocation context, receipt envelope, and store-backed receipt sink.

Short form:

```text
sb runs.
```

## Boundary

`syncbat` depends on `batpak`. It does not depend on `downstream-kit`, `netbat`,
DownstreamFrontend, or ExtProfile semantics. It stays synchronous and does not introduce a
production async runtime.

## Main Types

- `Core`
- `CoreBuilder`
- `Cx`
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

## Durable Register

`Register` is the catalog of operation descriptors. `CacheRegister` is a hot
lookup projection over a register and is not truth.

`StoreRegisterCatalog` persists register rows into batpak as typed catalog
events, and `rebuild_register_from_store` rebuilds a `Register` from those rows.
Identical duplicate rows are ignored; conflicting rows for the same operation
name fail closed.

## Layer Contract

Macros may declare descriptors and register items, but callers still assemble a
runtime explicitly. Runtime dispatch emits receipts only through the configured
sink. Batpak records the durable facts.
