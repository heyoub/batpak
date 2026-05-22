# ADR-0030: Netbat Streaming Contract

## Status

Draft contract for a post-`NETBAT/1` streaming boundary.

## Context

ADR-0029 locks `NETBAT/1` as request/response:

```text
NETBAT/1 CALL <operation-name> <hex-input>\n
```

R4 added batpak's `Canal` trait, and downstream callers need a boundary shape
that can carry ordered delivery items without smuggling streams through a
single `CALL` response. Streaming must therefore be a protocol contract with a
new verb/version, not an interpretation of `CALL`.

## Decision

`NETBAT/1` stays request/response only.

The streaming boundary uses a distinct protocol rung:

```text
NETBAT/2 STREAM <operation-name> <hex-input>\n
```

The response is an ordered stream of frames. The frame vocabulary is:

```text
ITEM <hex-payload>\n
WATERMARK <hex-watermark-ref>\n
END <hex-summary>\n
ERR <code> <hex-message>\n
```

`STREAM` is bound to batpak's delivery contract. Netbat may expose it only after
the substrate side commits the item vocabulary for the Canal-backed stream.
Before that substrate vocabulary exists, netbat rejects `NETBAT/2` frames
through the existing `unsupported_protocol_version` path.

## Contract

The streaming contract is:

- `CALL` and `STREAM` are separate verbs with separate response grammars.
- `NETBAT/1` never gains chunked responses.
- `NETBAT/2 STREAM` carries the same operation-name grammar and hex rules as
  `NETBAT/1 CALL`.
- `ITEM` frames preserve substrate ordering.
- `WATERMARK` frames expose progress witnesses, not application meaning.
- `END` is terminal for a successful stream.
- `ERR` is terminal for a failed stream.
- A stream error cannot be followed by more `ITEM` frames.
- Netbat frames bytes; syncbat still owns runtime dispatch; batpak still owns
  durable records, receipts, evidence, and delivery witnesses.

## API Gate

This ADR does not add a public netbat streaming API by itself. The public API
appears only when the batpak-side stream item contract is present and tested.

## Consequences

- `NETBAT/1` clients stay source-compatible and semantic-compatible.
- Streaming clients negotiate a new protocol rung explicitly.
- WebSocket, SSE, TCP, Unix socket, and stdio transports may carry the same
  streaming frame grammar; transport choice does not define stream semantics.
- Canal-backed delivery can cross the network boundary without turning netbat
  into a runtime owner.

## References

- `003_NETBAT_NETWORK.md`
- `100_ADR_0011_REACTOR_CANAL.md`
- `100_ADR_0029_NETBAT_BOUNDARY_CONTRACT.md`
