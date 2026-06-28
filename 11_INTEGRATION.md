# Integration

batpak owns a **local truth boundary** per journal: one `Store`, one `data_dir`,
one exclusive writer. That scopes source truth to a single append-only journal;
it does not mean distributed systems are out of scope. Larger hosts compose
**multiple journals** through explicit circuits — `netbat` routes, `syncbat`
dispatch, a `hostbat` reference host, and cross-store observations — without one battery
mutating another's state through a hidden path. See [08_CIRCUITS.md](08_CIRCUITS.md)
for journal composition rules and [README.md](README.md) for the scale-out
model. batpak does not own the whole application.

## Enforced boundaries

| Invariant | Proof surface |
| --- | --- |
| Production family crates stay sync-first (no async runtime dependencies) | `just boundary` |
| `batpak` core does not own network wire surfaces (`netbat` sits above `syncbat`) | `just boundary` |
| Store machine contact routes through `store/platform` rather than ad hoc filesystem calls | `just boundary` |
| `authority_required` is receiver policy input, never granted authority | traceability + substrate docs; no runtime grant path in core |
| PCP-Core wire validation ships only with explicit codecs, tests, and traceability | absence of undeclared PCP codecs in core; ADR/traceability when added |
| Downstream product doctrine maps to existing substrate terminals, not new BatPAK product APIs | traceability/product_doctrine_audit.yaml + `just inspect` |

batpak ships as an embedded event substrate, not as a hosted database, queue, ORM, or workflow product. Callers own process model, disk placement, and integration boundaries.

## Product Projection Boundary

Downstream product and agent frameworks may translate substrate truth into docs,
apps, dashboards, timelines, reports, CLIs, context packets, or delegated action
loops. BatPAK's job is to keep the source truth bounded and replayable through
`bank.commit`, `event.get`, `event.query`, `event.walk`, receipts, regions, and
projection mechanisms. The semantic payloads, role policies, UI surfaces,
workflow meaning, and representation routing live above BatPAK.

## Async Hosts

Async hosts may integrate with batpak by moving blocking work to their own runtime boundary. The substrate remains sync-first.

## Bidirectional Terminal Lane

The reference NETBAT terminal is a loop, not just a mailbox:

| Direction | Operation | Meaning |
| --- | --- | --- |
| Write | `bank.commit` | append a substrate event and receive a commit receipt |
| Point | `event.get` | read a known event id and its canonical payload bytes |
| Page | `event.query` | page bounded substrate summaries by `global_sequence` for replay and audit |
| Walk | `event.walk` | bounded hash-chain ancestry from a starting event id |
| Evidence | `evidence.*` | fetch batpak's own substrate evidence reports over the wire |

`event.query` is domain-neutral commit-order pagination. It filters on
substrate coordinates, kind category/type, and `global_sequence`; it does not
know Moonwalker missions, workflows, movement graphs, or receipt-body
taxonomies. `after_global_sequence` is the strict resume point for the next
page, not a server-held stream cursor.

The `evidence.*` family (`evidence.chain_walk`, `evidence.store_resource`,
`evidence.read_walk`, `evidence.projection_run`) gives a wire consumer direct
access to the substrate evidence reports `Store` already produces, instead of
re-deriving them from `event.walk` + `receipt.verify` + `event.query`. Each ack
carries the canonical report body (`report_hex`) plus its `body_hash` identity
and a `truncated` flag, so the consumer can verify the report by re-hashing the
blob. Evidence requests use domain-neutral substrate selectors — entity/scope
prefixes, optional kind filters, optional per-entity clock range on
`evidence.read_walk`, projection ids on `evidence.projection_run`, and
event-id hex on `evidence.chain_walk` — and traversal returns evidence/metadata
only, never decoded domain payloads.
`evidence.projection_run` resolves a domain-neutral projection id through an
embedder-registered table and the reference host registers none.

`entity` filters use `Region::entity`, which is prefix-based. Supplying both
`entity` and `scope` gives the normal coordinate-level replay shape.
`evidence.read_walk` additionally accepts `kind_category`/`kind_type_id`,
`start_clock`/`end_clock`, `max_stale_ms`, and a positive-only `limit`.

## Platform Contact

Filesystem, clock, lock, sync, and mmap contact should route through the platform boundary where the store owns that behavior.

## Larger Systems

Use circuits and terminals to connect batteries. Do not hide ownership by letting one battery mutate another battery's state through an unmodeled route.

## Local Host Loop

**Calibration pulse:** `cargo test -p netbat` exercises NETBAT/1 wire goldens,
stream runtime sessions, and bounded request/response paths against `syncbat`.
`cargo test -p hostbat` proves the `ClientManifest` projection, schema golden
vectors, and subscription descriptor wiring stay aligned with the H-interface.

**Living loop:** use `event.query` + `event.get` replay from a persistent store
in your embedder or the workspace examples under `bpk-lib/crates/batpak-examples/src/bin/`.
Seed app-owned events, rebuild a rendered view from commit-order query pages,
restart on the same store, and prove substrate replay without relying on commit
acks alone.
