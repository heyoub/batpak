# Replay

Replay reconstructs state from events.

Replay is deterministic when the replay function, event order, payload decoding, and dependency versions are deterministic.

batpak can preserve event history and receipt evidence. It cannot make non-deterministic user code deterministic.

## What Replay Does

- rebuilds projections
- verifies event history
- supports debugging
- supports migration and import paths
- makes derived state disposable

## What Replay Does Not Do

- hide user-code nondeterminism
- invent missing dependencies
- turn projection state into source truth
- own the host runtime

## Replay Evidence

Replay paths should produce evidence where user-visible trust depends on the result. Silent fallback is a smell; typed load status, report fields, and receipts are the preferred shape.

## External Traversal

In-process Rust replay should use `Store::query_entries_after` for bounded
commit-order pages and projection replay for derived state. `Store::query` is a
small-region snapshot convenience, and delivery cursors are ordered pull
mechanics rather than query pagination. Non-Rust terminals use the bounded
NETBAT lane:

1. `event.query` pages substrate summaries by coordinate/region/kind in
   ascending `global_sequence` order.
2. `event.get` fetches the canonical payload bytes for selected event ids.
3. Domain code decodes the payload envelope and dispatches on its own taxonomy.

`event.walk` is a separate axis: bounded hash-chain ancestry from a starting
`event_id`, returned in relation order (anchor first). It is not commit-order
pagination and must not be sorted by `global_sequence`. Use `event.query` when
you need commit-order pages; use `event.walk` when you need ancestor summaries
along the hash chain.

The `evidence.*` ops expose batpak's own substrate evidence reports over the
wire, so an external consumer can fetch chain-walk, read-walk, projection-run,
and store-resource evidence directly instead of approximating it from
`event.walk` + `receipt.verify` + `event.query`. They surface evidence and
metadata only — never decoded payload bytes — and resume points stay on the
`global_sequence` axis, never a stream cursor.

Pagination uses `after_global_sequence`, an exclusive resume point on global
commit order. It is not a stream cursor or server-held session: the next request
sets `after_global_sequence` to the prior response's
`next_after_global_sequence`. Existing `bank.commit` and `event.get` ack fields
named `sequence` are legacy wire spellings for that same global commit
sequence, not per-entity clock order.

Sidecar indexes may exist as caches or projections. They are not source truth:
authoritative external replay must be reconstructable from `event.query` plus
`event.get`.
