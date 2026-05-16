# 001 Batpak

`batpak` is the substrate layer.

It owns coordinate-addressed `.fbat` logs, typed events, caller-defined gates,
pipelines, append receipts, denial receipts, projections, replay, and opaque
receipt extension cargo.

Short form:

```text
bp records.
```

## Boundary

`batpak` does not know operation kits, runtime checkouts, server routes, PCP
semantics, or rendering. Callers may store extension bytes whose keys belong to
other layers, but batpak validates and preserves those bytes without deciding
what they mean.

## Main Types

- `Store`
- `Coordinate`
- `EventPayload`
- `AppendOptions`
- `AppendReceipt`
- `DenialReceipt`
- `Gate`
- `GateSet`
- `Pipeline`
- projections, subscriptions, cursors, and evidence reports

## Layer Contract

Use batpak when you need durable, sync-first receipt/event storage in one Rust
process. Keep application meaning in typed payloads or opaque extension bytes.

Do not add runtime, network, product, or protocol semantics to this layer unless
they are substrate mechanics.
