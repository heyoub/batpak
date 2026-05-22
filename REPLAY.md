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

