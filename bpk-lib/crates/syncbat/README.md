# syncbat

Sync-first runtime layer for batpak-family operation surfaces.

```text
sb runs.
```

`syncbat` owns operation descriptors, handler registration, checkout dispatch,
runtime receipts, and durable operation-catalog rows through batpak public APIs.
It does not own network framing, async runtimes, application kit vocabulary, or
batpak store internals.

Safety defaults fail closed: `CoreBuilder::build` refuses to build without a
receipt sink (opt out with `without_receipts`), receipt hashing defaults to
`ReceiptHashPolicy::Blake3` (opt out with `Deferred`), and capability tokens are
enforced at checkout (grant them with `grant_capability` / `grant_capabilities`).

Every effect axis is an enforced boundary, not a cooperative audit trail: an
operation reaches an effect only through the matching `Ctx` capability handle,
which records the effect into the observed row in the same step, and `checkout`
fails closed when the observed row is not a subset of the declared row.
`use_host_control` is a declared + subset-checked target axis like the others
(observed host controls must be a subset of the declared `uses_host_control`
targets), and `emit_receipt` stamps its opaque payload as observed evidence into
the invocation's single banked receipt (under the runtime-owned
`syncbat.emit_receipt.{kind}` drawer key) only after the backend mediates the
emission.

The runtime contract is documented in repository ADR-0028.

Live terminals: [05_TERMINALS.md](../../../05_TERMINALS.md). Composition:
[11_INTEGRATION.md](../../../11_INTEGRATION.md).
