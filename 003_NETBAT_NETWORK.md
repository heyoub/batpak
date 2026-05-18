# 003 Netbat Network

`netbat` is the server/network boundary layer.

It owns route metadata, boundary validation, bounded line-protocol frames, and a
blocking TCP listener that dispatches through syncbat.

Short form:

```text
nb exposes.
```

## Boundary

`netbat` depends on `syncbat`. It does not own runtime admission, handler
execution, durable records, or operation meaning.

The TCP listener is sequential and sync-first. It does not spawn worker threads
inside the crate and does not require syncbat handlers to be `Send`.

The line protocol accepts the versioned frame:

```text
NETBAT/1 CALL <operation-name> <hex-input>
```

The first-rung legacy `CALL <operation-name> <hex-input>` frame remains accepted.
Unsupported `NETBAT/*` versions return a stable error response. Listener stats
separate malformed frames, limit failures, and runtime failures.

Encoders write lowercase ASCII hex. Decoders accept lowercase and uppercase
ASCII hex.

## Main Types

- `Endpoint`
- `Route`
- `ServerModule`
- `Server`
- `Limits`
- `TcpServerConfig`
- `ShutdownHandle`
- `TcpServeStats`
- `RequestFrame`
- `ResponseFrame`

Helpers:

- `encode_request`
- `encode_response`
- `decode_line`
- `encode_hex_into`
- `decode_hex`

## Layer Contract

Use netbat to expose already-assembled syncbat runtimes at process or network
boundaries. Netbat validates and frames requests, then calls syncbat. Syncbat
dispatches. Batpak records.

The normative boundary contract is ADR-0029. The public API is locked by
`bpk-lib/traceability/public_api/netbat.txt`.

## Streaming Contract

`NETBAT/1` is request/response only. Streaming uses the separate
`NETBAT/2 STREAM` contract described in ADR-0030 once the batpak-side stream
item vocabulary is present. Netbat does not add chunked `CALL` responses.
