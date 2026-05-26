# Cookbook

The cookbook is the task-shaped field guide. Canonical model and invariant docs stay small; recipes live here and in `cookbook/200_*.md`.

## Recipe Index

- [Build with batpak](cookbook/200_BUILD_WITH_BATPAK.md)
- [Typed event store](cookbook/200_TYPED_EVENT_STORE.md)
- [State transition](cookbook/200_STATE_TRANSITION.md)
- [Cursor replay](cookbook/200_CURSOR_REPLAY.md)
- [Lossy subscription](cookbook/200_LOSSY_SUBSCRIPTION.md)
- [Projection](cookbook/200_PROJECTION.md)
- [Region read](cookbook/200_REGION_READ.md)
- [Read-walk evidence](cookbook/200_READ_WALK_EVIDENCE.md)
- [Artifact envelope](cookbook/200_ARTIFACT_ENVELOPE.md)
- [Backup envelope](cookbook/200_BACKUP_ENVELOPE.md)
- [Attested registry](cookbook/200_ATTESTED_REGISTRY.md)
- [Reservation ledger](cookbook/200_RESERVATION_LEDGER.md)
- [Platform evidence](cookbook/200_PLATFORM_EVIDENCE.md)
- [Anti-patterns](cookbook/200_ANTI_PATTERNS.md)

## Factory Checks

Use the root command surface:

```sh
just inspect
just verify
just perf-gates
just loom
just seal
```

`perf-gates` and `loom` are manual proof tiers; they are not part of `just verify`.

If a repeated workflow needs raw `cargo`, `npm`, or `pnpm`, promote it to a named `just` recipe.
