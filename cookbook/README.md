# Cookbook

These recipes are transcription rails: intent to API to proof surface. The
machine-readable source of truth is `bpk-lib/traceability/agent_surface.yaml`; each
recipe below should stay backed by a compiling example, template, or test.

- [`200_BUILD_WITH_BATPAK.md`](200_BUILD_WITH_BATPAK.md) - shortest end-to-end path.
- [`200_TYPED_EVENT_STORE.md`](200_TYPED_EVENT_STORE.md) - typed event append.
- [`200_IDEMPOTENT_PASS.md`](200_IDEMPOTENT_PASS.md) - durable re-runnable idempotent append.
- [`200_REGION_READ.md`](200_REGION_READ.md) - bounded reads.
- [`200_READ_WALK_EVIDENCE.md`](200_READ_WALK_EVIDENCE.md) - read evidence reports.
- [`200_PROJECTION.md`](200_PROJECTION.md) - projection and projection-run evidence.
- [`200_CURSOR_REPLAY.md`](200_CURSOR_REPLAY.md) - at-least-once cursor replay.
- [`200_LOSSY_SUBSCRIPTION.md`](200_LOSSY_SUBSCRIPTION.md) - lossy live push.
- [`200_ARTIFACT_ENVELOPE.md`](200_ARTIFACT_ENVELOPE.md) - canonical artifact envelopes.
- [`200_ATTESTED_REGISTRY.md`](200_ATTESTED_REGISTRY.md) - generic registry rows.
- [`200_BACKUP_ENVELOPE.md`](200_BACKUP_ENVELOPE.md) - backup manifest evidence.
- [`200_STATE_TRANSITION.md`](200_STATE_TRANSITION.md) - generic state transition evidence.
- [`200_RESERVATION_LEDGER.md`](200_RESERVATION_LEDGER.md) - abstract reservation ledgers.
- [`200_PLATFORM_EVIDENCE.md`](200_PLATFORM_EVIDENCE.md) - platform profile probes.
- [`200_ANTI_PATTERNS.md`](200_ANTI_PATTERNS.md) - wrong moves and replacements.

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
