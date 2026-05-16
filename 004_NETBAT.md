# 004 Netbat

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

## Layer Contract

Use netbat to expose already-assembled syncbat runtimes at process or network
boundaries. Netbat validates and frames requests, then calls syncbat. Syncbat
dispatches. Batpak records.
