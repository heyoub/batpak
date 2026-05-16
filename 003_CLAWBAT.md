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
- `GateContext`
- `RequirementEvidence`
- `RequiredPassGate`
- `RequiredCapabilityGate`
- `MISSING_PASS_CODE`
- `MISSING_CAPABILITY_CODE`
- `OperationKitItem`
- `OperationRegisterItem`
- `OperationDescriptor`
- `ReceiptEnvelope`

## Requirement Compiler

`OperationKitItem::compile_gate_set` and
`OperationKitItem::compile_pipeline` turn declared pass and capability refs into
batpak `GateSet` / `Pipeline` values over a caller-provided context. The default
`RequirementEvidence` is a concrete context for callers that already know which refs
are satisfied.

The compiler emits one gate per requirement class, not one duplicate-named gate
per ref. Missing refs deny with stable gate names and codes while caller policy
still owns what makes a pass or capability true.

```rust
let gates = item.compile_gate_set::<cb::RequirementEvidence>();
let pipeline = item.compile_pipeline::<cb::RequirementEvidence>();
```

## Layer Contract

Use downstream-kit to declare what can be run and what references are attached to that
declaration. Use syncbat to run it. Use batpak to record it.
