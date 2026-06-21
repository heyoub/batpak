# Integration

batpak owns a **local truth boundary** per journal: one `Store`, one `data_dir`,
one exclusive writer. That scopes source truth to a single append-only journal;
it does not mean distributed systems are out of scope. Larger hosts compose
**multiple journals** through explicit circuits — `netbat` routes, `syncbat`
dispatch, reference `refbat`, and cross-store observations — without one battery
mutating another's state through a hidden path. See [CIRCUITS.md](CIRCUITS.md)
for journal composition rules and [README.md](README.md) for the scale-out
model. batpak does not own the whole application.

## Enforced boundaries

| Invariant | Proof surface |
| --- | --- |
| Production family crates stay sync-first (no async runtime dependencies) | `just boundary` |
| `batpak` core does not own network wire surfaces (`netbat` sits above `syncbat`) | `just boundary` |
| Store machine contact routes through `store/platform` rather than ad hoc filesystem calls | `just boundary` |
| `authority_required` is receiver policy input, never granted authority | traceability + substrate docs; no runtime grant path in core |
| External-Profile wire validation ships only with explicit codecs, tests, and traceability | absence of undeclared ExtProfile codecs in core; ADR/traceability when added |
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
know Downstream missions, workflows, movement graphs, or receipt-body
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

**Calibration pulse:** `just host-dev` mirrors the CI ts-parity lane: export
manifest, codegen, build and test the workspace, boot refbat on an ephemeral
store, run heartbeat-spike (heartbeat, commit, query, get), and verify committed
generated sources stay deterministic. heartbeat-spike proves the live heartbeat
+ commit/query/get + ERR calibration path; `receipt.verify`, `event.walk`, and
the four `evidence.*` ops round out the ten-op host profile and are covered by
manifest/parity and refbat tests.

**Living loop:** `just host-loop` runs the audit-loop example against a
persistent store under `target/host-loop/store/`. It seeds app-owned events
(`kind_category = 0x01`), rebuilds the rendered audit view from `event.query` +
`event.get` (not commit acks), kills refbat, restarts on the same store, and
runs `--replay-only` to prove substrate replay.
