# Integration

batpak owns a local truth boundary. It does not own the whole application.

## Enforced boundaries

| Invariant | Proof surface |
| --- | --- |
| Production family crates stay sync-first (no async runtime dependencies) | `just boundary` |
| `batpak` core does not own network wire surfaces (`netbat` sits above `syncbat`) | `just boundary` |
| Store machine contact routes through `store/platform` rather than ad hoc filesystem calls | `just boundary` |
| `authority_required` is receiver policy input, never granted authority | traceability + substrate docs; no runtime grant path in core |
| PCP-Core wire validation ships only with explicit codecs, tests, and traceability | absence of undeclared PCP codecs in core; ADR/traceability when added |

batpak ships as an embedded event substrate, not as a hosted database, queue, ORM, or workflow product. Callers own process model, disk placement, and integration boundaries.

## Async Hosts

Async hosts may integrate with batpak by moving blocking work to their own runtime boundary. The substrate remains sync-first.

## Bidirectional Terminal Lane

The reference NETBAT terminal is a loop, not just a mailbox:

| Direction | Operation | Meaning |
| --- | --- | --- |
| Write | `bank.commit` | append a substrate event and receive a commit receipt |
| Point | `event.get` | read a known event id and its canonical payload bytes |
| Walk | `event.query` | page bounded substrate summaries for replay and audit |

`event.query` is domain-neutral. It filters on substrate coordinates, kind
category/type, and `global_sequence`; it does not know Moonwalker missions,
workflows, movement graphs, or receipt-body taxonomies.

`entity` filters use `Region::entity`, which is prefix-based. Supplying both
`entity` and `scope` gives the normal coordinate-level replay shape.

## Platform Contact

Filesystem, clock, lock, sync, and mmap contact should route through the platform boundary where the store owns that behavior.

## Larger Systems

Use circuits and terminals to connect batteries. Do not hide ownership by letting one battery mutate another battery's state through an unmodeled route.
