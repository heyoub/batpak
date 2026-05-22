# Integration

batpak owns a local truth boundary. It does not own the whole application.

## Enforced boundaries

| Invariant | Proof surface |
| --- | --- |
| Production family crates stay sync-first (no async runtime dependencies) | `just boundary` |
| `batpak` core does not own network wire surfaces (`netbat` sits above `syncbat`) | `just boundary` |
| Store machine contact routes through `store/platform` rather than ad hoc filesystem calls | `just boundary` |
| `authority_required` is receiver policy input, never granted authority | traceability + substrate docs; no runtime grant path in core |
| External-Profile wire validation ships only with explicit codecs, tests, and traceability | absence of undeclared ExtProfile codecs in core; ADR/traceability when added |

batpak ships as an embedded event substrate, not as a hosted database, queue, ORM, or workflow product. Callers own process model, disk placement, and integration boundaries.

## Async Hosts

Async hosts may integrate with batpak by moving blocking work to their own runtime boundary. The substrate remains sync-first.

## Platform Contact

Filesystem, clock, lock, sync, and mmap contact should route through the platform boundary where the store owns that behavior.

## Larger Systems

Use circuits and terminals to connect batteries. Do not hide ownership by letting one battery mutate another battery's state through an unmodeled route.

