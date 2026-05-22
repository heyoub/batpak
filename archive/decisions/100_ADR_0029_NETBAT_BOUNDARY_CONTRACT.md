# ADR-0029: Netbat Boundary Contract

## Status

Accepted for the 0.7.6 correction cut.

## Context

`netbat` is the thin boundary layer above `syncbat`. It exposes already-built
sync runtimes through route metadata and bounded line-protocol frames. It does
not own handler execution, operation meaning, durable records, or admission
policy.

R4 closed the immediate request-encoder asymmetry. This ADR locks the full
boundary contract so protocol changes are explicit instead of appearing as ad
hoc test byte strings or transport shortcuts.

## Decision

`netbat` owns:

- route metadata and endpoint validation
- server/module introspection
- bounded request/response frame encoding
- `NETBAT/1 CALL <operation-name> <hex-input>` request decoding
- stable response frames: `OK <hex-output>` and `ERR <code> <hex-message>`
- sequential blocking TCP listener behavior
- stable error-code mapping for malformed, limit, protocol, and runtime failures

`netbat` does not own:

- handler execution semantics
- receipt emission
- durable writes
- application protocol envelopes
- async runtimes or thread-pool policy

## Protocol Contract

The versioned request frame is:

```text
NETBAT/1 CALL <operation-name> <hex-input>\n
```

The first-rung legacy frame remains accepted for 0.7.6 compatibility:

```text
CALL <operation-name> <hex-input>\n
```

Unsupported `NETBAT/*` versions fail with
`ERR unsupported_protocol_version <hex-message>`.

Decoders enter the legacy branch when and only when the first
whitespace-separated token does not start with `NETBAT/`.

Operation names are non-empty ASCII names. Valid bytes are `A-Z`, `a-z`,
`0-9`, `.`, `_`, and `-`. Names cannot start or end with `.`, and cannot
contain `..`.

Encoders write lowercase ASCII hex (`0-9`, `a-f`). Decoders accept lowercase
and uppercase ASCII hex.

All request decoding is bounded by `Limits`:

- maximum line bytes
- maximum operation-name bytes
- maximum decoded input bytes
- maximum encoded output bytes

The TCP listener is sequential and sync-first. The default TCP listener serves
one request per accepted connection (`DEFAULT_MAX_REQUESTS_PER_CONNECTION = 1`).
Owners may raise this value only for sequential connection reuse within
`NETBAT/1` request/response semantics.

Listener owners pass `IoTimeouts` hints and a `ShutdownHandle` to bound blocking
I/O and coordinate shutdown. `netbat` does not enforce timeouts for generic
non-`TcpStream` transports.

## Error Codes

`ERR <code> <hex-message>` uses these stable code strings:

- `io`: transport I/O failure
- `empty_stream`: no request bytes were available
- `line_too_long`: request line exceeded `Limits::max_line_bytes`
- `malformed_request`: frame shape, verb, field count, or hex syntax failed
- `unsupported_protocol_version`: first token named an unsupported `NETBAT/*`
  version
- `operation_name_too_long`: operation name exceeded
  `Limits::max_operation_name_bytes`
- `input_too_large`: decoded input exceeded `Limits::max_input_bytes`
- `output_too_large`: encoded success output exceeded
  `Limits::max_output_bytes`
- `unknown_operation`: syncbat runtime did not know the requested operation
- `missing_handler`: syncbat runtime had a descriptor without a handler
- `handler`: syncbat handler returned a classified runtime failure
- `receipt_sink`: syncbat receipt sink rejected a runtime receipt

## Streaming

R4 added batpak's `Canal` trait, but `NETBAT/1` remains request/response only.
ADR-0030 defines the separate `NETBAT/2 STREAM` contract shape. `CALL` does not
gain chunked responses.

## Consequences

- `netbat` receives a checked public API baseline.
- protocol fixtures and response shapes become traceable artifacts.
- downstream clients can encode requests against one owner function.
- streaming changes have a named decision point instead of accidental drift.

## References

- `003_NETBAT_NETWORK.md`
- `100_ADR_0030_NETBAT_STREAMING_CONTRACT.md`
- `bpk-lib/crates/netbat/src/`
- `bpk-lib/crates/netbat/tests/`
- `bpk-lib/traceability/public_api/netbat.txt`
