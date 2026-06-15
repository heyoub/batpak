# syncbat

Sync-first runtime layer for batpak-family operation surfaces.

```text
sb runs.
```

`syncbat` owns operation descriptors, handler registration, checkout dispatch,
runtime receipts, and durable operation-catalog rows through batpak public APIs.
It does not own network framing, async runtimes, application kit vocabulary, or
batpak store internals.

The runtime contract is documented in repository ADR-0028.

Live terminals: [TERMINALS.md](../../../TERMINALS.md). Composition:
[INTEGRATION.md](../../../INTEGRATION.md).
