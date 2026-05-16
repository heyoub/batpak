# 003 DownstreamKit

`downstream-kit` is the operation-kit facade.

It owns declaration vocabulary: operation kit metadata, pass references,
capability references, and the `cb::` import shape. It re-exports operation
macro sugar while runtime execution stays in syncbat.

Short form:

```text
cb declares.
```

## Boundary

`downstream-kit` depends on `syncbat`. It does not own dispatch, storage, network
transport, or receipt persistence.

Pass and capability vocabulary are userland declarations. They compile down to
runtime descriptors, register items, gates, context, and caller policy. They do
not become native batpak authority.

## Main Types

- `PassRef`
- `CapabilityRef`
- `PassDescriptor`
- `CapabilityDescriptor`
- `OperationKitItem`
- `OperationRegisterItem`
- `OperationDescriptor`
- `ReceiptEnvelope`

## Layer Contract

Use downstream-kit to declare what can be run and what references are attached to that
declaration. Use syncbat to run it. Use batpak to record it.
