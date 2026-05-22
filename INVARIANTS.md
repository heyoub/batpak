# Invariants

This file is narrative ordnance: short, human-readable rules for the current substrate.

Machine law lives in `bpk-lib/traceability/invariants.yaml` and the integrity checks under `bpk-lib/tools/integrity/`. On conflict, traceability and executable checks win.

## Batteries Do Not Own The Machine

A battery may power or store part of a system. It does not become the application, runtime, server, queue, workflow engine, or framework.

## Terminals Are Explicit

All host interaction crosses named terminals. Hidden wires are bugs.

## Events Are Source Truth

An accepted event is immutable. Corrections are represented by later events, not mutation of old events.

## Receipts Describe Outcomes

A receipt records what the system accepted, denied, replayed, verified, projected, imported, exported, or inspected. A receipt is structured evidence, not a debug log.

## Projections Are Disposable

A projection may be rebuilt from the log. If a projection cannot be rebuilt, it is application state outside batpak's projection model.

## Sync-First Means No Hidden Runtime

batpak does not require an async runtime. Async hosts may integrate by moving blocking work to their own runtime boundary.

## Canonical Bytes Matter

When batpak hashes structured content, the same logical content must produce the same canonical bytes.

## Escape Hatches Are Labeled

Low-level access is allowed when necessary, but it must be named, visible, and non-default.

## Current Docs, Not Lineage

Canonical docs describe the current system. Historical notes belong only where compatibility, migration, or security requires them.

