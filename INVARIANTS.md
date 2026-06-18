# Invariants

This file is narrative ordnance: short, human-readable rules for the current substrate.

Machine law lives in `bpk-lib/traceability/invariants.yaml` and the integrity checks under `bpk-lib/tools/integrity/`. On conflict, traceability and executable checks win.

## Batteries Do Not Own The Machine

A battery may power or store part of a system. It does not become the application, runtime, server, queue, workflow engine, or framework.

## Terminals Are Explicit

All host interaction crosses named terminals. Hidden wires are bugs.

## Events Are Source Truth

An accepted event is immutable. Corrections are represented by later events, not mutation of old events.

## Payload Shape Evolves On Read

Stored payload bytes are never rewritten. A payload's `PAYLOAD_VERSION` rides in the event header, outside the hashed region; on read an older version is upcast in memory, an equal or legacy-`0` version decodes tolerantly, and a version newer than the reader understands is a hard error everywhere — including replay and cold-start.

## Idempotency Is Durable

A keyed append (`with_idempotency`) is a durable no-op: within its retention window a retry returns the original receipt regardless of compaction, cold-start, or load. The window is the inviolable guarantee; the size cap may only ever evict keys already outside it.

## Receipts Describe Outcomes

A receipt records what the system accepted, denied, replayed, verified, projected, imported, exported, or inspected. A receipt is structured evidence, not a debug log.

## Projections Are Disposable

A projection may be rebuilt from the log. If a projection cannot be rebuilt, it is application state outside batpak's projection model.

## Traversal Axes Stay Separate

Commit-order pagination uses `global_sequence` and the
`after_global_sequence` resume point. Hash-chain ancestry uses `event.walk` /
`walk_ancestors`. Delivery cursors are ordered pull mechanics. These names must
not collapse into one generic cursor story.

## Sync-First Means No Hidden Runtime

batpak does not require an async runtime. Async hosts may integrate by moving blocking work to their own runtime boundary.

## Canonical Bytes Matter

When batpak hashes structured content, the same logical content must produce the same canonical bytes.

## Escape Hatches Are Labeled

Low-level access is allowed when necessary, but it must be named, visible, and non-default.

## Advanced Surfaces Are Still Real

An API can be public without being beginner-hot. Evidence reports, reactors,
outbox writes, visibility fences, delivery cursors, and platform diagnostics are
expert surfaces unless the root docs explicitly promote them.

## Current Docs, Not Lineage

Canonical docs describe the current system. Historical notes belong only where compatibility, migration, or security requires them.
