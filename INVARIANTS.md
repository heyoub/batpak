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

## Forks Do Not Alias Mutable Authorities

A fork may share immutable sealed segments, but it must own its active segment,
visibility ranges, idempotency sidecar, and pending compaction state. Parent
writes or cancellations after the fork do not affect the fork.

## Import Preserves Content, Not Identity

Import re-applies source events. Payload bytes and content hashes survive
unchanged, but event ids, global sequences, predecessors, and causation are
destination-local.

## Lane Frontiers Are Logical

Lane ids are opaque `u32` substrate data. Each lane has its own logical frontier
for accepted, written, durable, visible, applied, and emitted HLC points, while
the segment log remains one physically interleaved file sequence. A successful
fsync advances one global physical durability point; a lane's logical durable
point may advance only to events on that lane that are at or below the physical
durable point on the `global_sequence` axis, not by wall-clock HLC ordering.
Visibility is a separate per-lane publish cursor over the single global sequence
space; hidden/cancelled fence ranges are interpreted on that same axis and then
scoped by lane.

For every lane: accepted >= written >= durable, accepted >= visible >= applied,
and emitted >= visible. The global frontier remains the max view used by
legacy APIs; lane-scoped APIs must read from the lane view.

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

## Invariant Catalog

The narrative sections above are authored. The table below is a generated VIEW
of `bpk-lib/traceability/invariants.yaml` — the machine catalog. `just docs`
regenerates it; `structural-check` fails CI if it drifts from the catalog
(INV-DOCS-CATALOG-VIEW-CURRENT). Every catalog id may also carry a
`witness_test` naming the `#[test]` that exercises it (INV-INVARIANT-WITNESS-TEST).

<!-- BEGIN INV-CATALOG -->

_Generated from `bpk-lib/traceability/invariants.yaml` by `just docs`. Do not edit by hand; run the generator._

| Invariant | Statement |
| --- | --- |
| `INV-ALLOW-IS-DESIGN` | Every `#[allow(...)]` in the repo is a deliberate design decision, not a silencer. The allow carries a `// justifies <prose>` comment on the same or preceding line that cites at least one resolvable anchor — an INV-id from this catalog, an `ADR-NNNN` root ADR file, or a concrete repo path (`crates/core/src/...`, `crates/core/tests/...`, `crates/core/examples/...`, `crates/macros/...`, `crates/macros-support/...`) — so the rationale is traceable, not narrative-only. |
| `INV-BATCH-ATOMIC-VISIBILITY` | Batch append events are invisible to queries, cursors, and subscriptions until the entire batch is committed, fsynced, and atomically published via SequenceGate::publish(). All read methods filter by visible_sequence. No partial batch is ever observable. Proved by loom model and concurrent-read integration test. |
| `INV-BATCH-CRASH-RECOVERY` | Incomplete batches (BEGIN without matching COMMIT) are discarded during cold-start segment scan. Cross-segment batch state persists across segment boundaries to handle batches spanning rotation points. |
| `INV-BIDIRECTIONAL-SUBSTRATE-LANE` | Any non-Rust terminal that can commit events must expose bounded domain-neutral enumeration by coordinate, region, and global commit order, or docs must explicitly state replay requires caller-supplied event IDs and is not substrate-complete. |
| `INV-BUILD-FAIL-FAST` | crates/core/build.rs halts the build on invariant violations with a cargo-parseable explanation. Panic sites are consolidated through the crate-private fail helper so the policy is expressed in one place; the file-level #![allow(clippy::panic)] attribute covers that single documented exit path. |
| `INV-CACHE-CAPABILITIES-EXPLICIT` | Cache prefetch behavior must be discoverable through an explicit capability surface. |
| `INV-CANONICAL-CONTAINER-CI` | The canonical Linux CI path executes inside the checked-in devcontainer image rather than a separately hand-installed host environment. |
| `INV-CANONICAL-PATCH-STABILITY` | Schema-versioned evidence report bodies keep stable canonical bytes across patch releases; changing a v1 body shape requires an intentional golden fixture update or a new versioned body. |
| `INV-CHAOS-LINUX-ONLY` | The chaos harness uses Linux device-mapper primitives and is unconditionally cfg'd off on non-Linux targets; durability proofs derived from this harness apply to Linux kernels only. |
| `INV-CHECKPOINT-V2-INTERNED` | Index checkpoint v2 and later persist an interner snapshot and use InternId u32s instead of raw strings, saving ~22 bytes per entry and enabling fast cold-start restore. SIDX footers provide per-segment fast path. |
| `INV-CLOCK-NOW-US-LIVE` | The store wall-clock helper must produce positive microsecond timestamps that advance with real time. |
| `INV-COLD-START-ARTIFACTS` | Clean close and restart paths produce and consume the expected checkpoint, mmap, and segment artifacts without trusting stale fast-start metadata. |
| `INV-COLUMNAR-REPLACES-DASHMAP` | The scan index always keeps the base AoS by_fact and scope_entities maps, and may additionally fan out into SoA, SoAoS, and AoSoA64 overlays. IndexTopology is the live public model for enabling those overlays. |
| `INV-COMPLEXITY-EXPONENT-BOUNDED` | A real Store read operation's cost does not silently regress into a worse asymptotic class: the ALLOCATION COUNT (measured by the process-wide CountingAlloc, never wall-clock nanoseconds) of Store::query(Region::all()) is sampled at geometrically increasing input sizes, a least-squares slope of log(cost) vs log(n) is fit, and that fitted complexity exponent must stay under a linear-ish budget; additionally the worst observed per-op allocation COUNT over the declared input distribution stays under a fixed count-based WCET budget. The slope is a ratio of logs and so is hardware-independent and deterministic, and the gate is anti-vacuous because a planted quadratic dataset is rejected by the same pure check_complexity logic. |
| `INV-CONCURRENCY-SCHEDULE-PROOF` | Idempotency, compare-and-set, restart-budget, and compaction-exclusivity semantics remain valid under deterministic schedule exploration. |
| `INV-CONTEXT-VIEWS-DERIVED-FROM-HISTORY` | Projection and cache outputs are derived context views over append history; they are not an alternate source of truth and must not bypass journal receipts or frontier witnesses when proof is required. |
| `INV-COORDINATE-IS-LOGICAL-STREAM` | Coordinate names a logical context stream inside one journal; it provides logical ordering and addressing, not physical sharding or cross-directory consistency. |
| `INV-CROSS-DIRECTORY-CONSISTENCY-PRODUCT-OWNED` | Cross-data_dir consistency is outside Store invariants; multi-journal systems compose by product-owned routing, observations, receipts, and local policy. |
| `INV-DANGEROUS-TEST-HOOKS-NONDEFAULT` | Dangerous test hooks are not exposed in default production builds. |
| `INV-DELIVERY-AT-LEAST-ONCE-WITNESS` | cursor_worker and typed reactor handlers receive Option<&AtLeastOnce> on every delivered batch; Some is produced iff the worker config declares a checkpoint_id, and the witness CheckpointId matches that config. |
| `INV-DOCS-CATALOG-VIEW-CURRENT` | INVARIANTS.md is a generated VIEW of traceability/invariants.yaml: the auto-generated catalog block between the BEGIN/END INV-CATALOG markers lists every catalog id and its one-line statement, the authored human prose stays above the block, and the docs-catalog gate folded into structural-check fails on any drift so the published docs can never silently rot away from machine law while the generator regenerates the block deterministically. |
| `INV-DST-RECOVERY-LEGAL` | A real Store opened over the fault-injecting SimFs filesystem backend, driven through the real public append/append_batch/sync API, crashed at the durability boundary (writer abandoned without a clean shutdown, then the unsynced tail truncated via SimFs::crash), and reopened over the persisted (truncated) tree must recover a LEGAL state: a prefix of the appended op-log (no invented or undead events), containing every acknowledged-durable commit (nothing lost that an honored sync confirmed durable), with an intact hash chain across the recovered visible events; and the same BATPAK_SEED must recover the identical state and op-trace digest (determinism). A typed corruption refusal on reopen is a legal recovery outcome; an untyped failure is not. |
| `INV-EVENT-PAYLOAD-DECODE-BACKCOMPAT` | The single typed-decode seam keeps decoding historical payload bytes as schemas evolve - additive-with-default is absorbed by serde, an older stored payload_version is lifted to the current shape by the registered Upcast chain (in-memory only, stored bytes never rewritten), and a stored version newer than the decoder is a hard FutureVersion error; adding the payload_version header field moves no event content hash or signature. |
| `INV-EVENTKIND-PARSE-ROUNDTRIP` | Every value built by the fallible EventKind try_custom parser decomposes back to its originating category and type id, and try_custom accepts exactly the inputs the strict const constructor would accept without panicking. |
| `INV-EXAMPLES-OBSERVABLE-OUTPUT` | Examples in crates/core/examples/*.rs prove shipped behaviour by producing observable stdout/stderr output that a reader can see; print_stdout, print_stderr, wildcard match arms over enums, and bounded integer narrowing in teaching fixtures are expected for that role. The Teaches-header convention keeps each example focused on a single pattern. |
| `INV-EXTERNAL-REPLAY-NO-SIDECAR-TRUTH` | External replay may use caches or indexes, but authoritative reconstruction must be possible through event.query plus event.get plus envelope decoding above batpak; sidecar indexes are projections, not source truth. |
| `INV-FAULT-INJECT-GATED` | Fault injection infrastructure (InjectionPoint, FaultInjector trait, maybe_inject hooks, writer panic test hooks, and dangerous frontier mutation APIs) is compiled only under the opt-in dangerous-test-hooks feature gate; default downstream builds must not expose that surface. |
| `INV-FENCE-CANCELLED-STAYS-HIDDEN` | Cancelling a public visibility fence leaves its writes durable but hidden, and that hidden-range state survives reopen, snapshot export, and fast-start restore. |
| `INV-FORK-ISOLATION` | Store fork materializes a self-contained directory at a drained visibility boundary: immutable sealed segments may be shared by reflink or hardlink, but the active segment and mutable correctness authorities such as index.idemp and visibility_ranges.fbv are deep-copied, regenerable caches are excluded by default, symlink leaves are rejected, and writes or visibility cancellations after the fork cannot mutate the fork view. |
| `INV-FRONTIER-APPEND-GATE-HONORED` | When AppendOptions::gate is Some(DurabilityGate { kind, timeout }), Store::append_with_options and Store::append_batch_with_options return only after the corresponding watermark crosses the appended event's HLC, or return StoreError::WaitTimeout. The committed event is queryable in both cases; the timeout reflects the gate guarantee, not the commit. |
| `INV-FRONTIER-APPLIED-MIN` | applied_hlc equals the minimum HLC across all registered projections; with zero registered projections it remains at the bootstrap open_hlc, never drifting forward. wait_for_applied honors this min so a single lagging projection blocks the wait until it catches up. |
| `INV-FRONTIER-DURABLE-COVERS-RECOVERED` | On reopen after any crash or device failure, durable_hlc covers every event observable via query, and durable_hlc is monotonic across crash boundaries (recovered_durable_hlc >= pre_failure_durable_hlc). batpak makes no claim about which specific in-flight events survive a particular crash; it only guarantees that the durable frontier honestly classifies whatever was preserved. |
| `INV-FRONTIER-FAULT-ORDINALS` | SingleAppendStart fires before any watermark advance, SingleAppendWritten fires after written advance and before durable, and SingleAppendPublished fires after visible+emitted advance and before the receipt is returned. |
| `INV-FRONTIER-MONOTONIC` | Each frontier watermark (accepted, written, durable, visible, emitted, applied) advances monotonically; advance methods take max with the proposed point and never permit backward motion. |
| `INV-FRONTIER-OPEN-MONOTONIC` | On Store::open and Store::open_read_only, the bootstrap open_hlc satisfies open_hlc >= max_recovered_hlc and open_hlc >= last_close_hlc, where last_close_hlc is the highest HLC carried by recovered SYSTEM_CLOSE_COMPLETED lifecycle events. This monotonicity is enforced post-emit and surfaces as StoreError::InvariantViolation if violated. |
| `INV-FRONTIER-ORDERING` | At every torn-free observation, accepted_hlc >= written_hlc >= durable_hlc, accepted_hlc >= visible_hlc >= applied_hlc, and emitted_hlc >= visible_hlc. |
| `INV-FRONTIER-TORN-FREE` | FrontierView is composed under a single watermark mutex acquisition; visible+emitted advances at commit time use a composite advance helper that holds the lock across both field updates. |
| `INV-FRONTIER-WAIT-MONOTONIC` | wait_for_durable, wait_for_applied, and wait_for_visible return Ok(()) only after observing the corresponding watermark >= target on a state mutation under the same lock that issued the notify; spurious wakeups never satisfy the wait. |
| `INV-GAUNTLET-FOLD-FUSION` | The gauntlet's own fitness functions fold over ONE shared repo-IR column-store (binding AL assignments, gate ownership, waiver ownership, the public-surface map, the mutation-seam map, and docs traceability), and the fused single-traversal runner produces the identical finding set as running each fitness in its own separate pass, so the integrity engine obeys the same banana-split fold-fusion equivalence law it tests for event projections. |
| `INV-GENERATED-WITNESS-PIN` | Derive-generated handler-signature pin consts (for example _HANDLER_PIN_<i>, __batpak_kind_collision_check_<Ident>) are type-check witnesses — they exist to make a mismatched handler signature fail compilation, have no runtime role, and embed the user's ident (often CamelCase or snake). non_upper_case_globals, non_snake_case, and dead_code are suppressed on those specific generated items only. |
| `INV-GROUP-COMMIT-IDEMPOTENCY` | Group commit (batch > 1) structurally requires idempotency keys on every append, enforced at append time via StoreError::IdempotencyRequired. |
| `INV-HASH-CHAIN-INTEGRITY` | Hash-chain tests preserve append-order integrity by detecting reordered, missing, or mutated event payload links. |
| `INV-HLC-JOIN-SEMILATTICE` | The hybrid logical clock merge forms a bounded join-semilattice that is commutative, associative, idempotent, and bottomed at ORIGIN, with the dual meet behaving symmetrically and the lexicographic tiebreak preserved. |
| `INV-IDEMPOTENCY-DURABLE-WINDOW` | A keyed append is deduplicated as a true no-op via a durable sidecar (index.idemp - magic FBATID, versioned, crc32fast CRC, atomic write) that survives retention compaction, cold-start, and snapshot independent of event eviction and is restored unconditionally (never rebuilt from segment scan); growth is bounded by a window-priority hybrid where the window is inviolable (a key whose recorded global sequence is within keep_sequences of the frontier is never evicted by the soft max_keys cap, which may only trim out-of-window keys), so a retry of a within-window key is always a no-op regardless of load while a corrupt or missing sidecar degrades to empty (logged, never crashing) and a future on-disk version is a hard error; IdempotencyKey::for_operation derives operation identity from a length-delimited blake3 over domain plus components and is not a payload content hash. |
| `INV-IMPORT-CONTENT-ISOMORPHISM` | Store import is re-application rather than merge: source event identity, sequence, and causation are regenerated or cleared, while raw MessagePack payload bytes, content hashes, correlation metadata, deterministic source-namespace idempotency keys, and provenance receipt extensions are preserved across chunking, replay, and compaction. |
| `INV-INDEX-FILTER-COMPOSES` | Index filter composition over kind, scope, entity, and topology overlays returns the same logical result set as the authoritative scan. |
| `INV-INVARIANT-WITNESS-TEST` | Every catalog invariant that declares a witness_test names a path::fn that resolves to a real declared test (a #[test] function or a proptest!-defined test) in the tree; the docs-catalog gate fails when a declared witness names a missing file, a missing function, or a plain non-test function, upgrading the weak header-string citation to a strong named-test citation for the invariants that carry it. |
| `INV-JOURNAL-SINGLE-LIVE-OWNER` | A journal is one Store root at one data_dir with one live owner; mutable and read-only opens both fail with StoreLocked while another owner holds the directory lock. |
| `INV-JOURNAL-WRITER-SERIALIZES-COMMITS` | All committed events in one journal are serialized through the writer-owned commit path; Coordinate does not create an independent physical writer or global_sequence timeline. |
| `INV-LANE-BRANCH-ISOLATION` | DAG lanes are independent branch heads for the same entity: writer prev_hash, per-lane clock/CAS state, batch staging, cold-start latest-head reconstruction, query pagination, cursor delivery, and push fanout all route by `(entity,lane)`, while a Region with no lane filter observes the historical all-lane timeline and the lane-0 default path remains the compatibility path. |
| `INV-LINEARIZABILITY-SINGLE-WRITER` | Because batpak commits through a single writer behind a gated visibility watermark, the ordered visible history a reader returns is the single-writer linearization order: a dense, strictly increasing global_sequence prefix (no gap below the visible high-water and no premature visibility), reads are monotonic (a re-query never drops or reorders a previously-visible event), two independent readers of the same store converge on identical history, and there is no real-time/sequence inversion (if an append returns before another is issued, its global_sequence is the smaller one); a seeded operation stream against a real Store under a fixed clock witnesses this, and a pure checker rejects inverted/gapped/duplicate histories so the property is not vacuous. |
| `INV-LITERAL-REGEX-UNWRAP-SAFE` | unwrap / expect on regex::Regex::new with a literal string pattern inside tool code is safe because the pattern is a compile-time constant string with known-valid regex syntax; the call cannot fire in any reachable execution of the tool. |
| `INV-MACRO-BOUNDED-CAST` | Narrowing integer casts inside batpak-macros (under crates/macros) are preceded by an explicit u8::MAX / u16::MAX comparison that returns an error before the cast, so clippy::cast_possible_truncation at the cast site cannot fire. |
| `INV-MMAP-SEALED-READS` | Sealed segment reads use zero-copy memory-mapped I/O via memmap2. Active segment uses FD cache with pread (Unix) or seek+read (Windows). |
| `INV-MULTI-VIEW-PUBLISH-AFTER-VIEW-SYNC` | Writer visibility publish happens only after every active index view has been populated; readers must observe either all view updates for a sequence range or none of them. |
| `INV-NATIVE-DELETE-IDEMPOTENT` | NativeCache prefix deletion remains safe and idempotent — repeating a delete_prefix call after entries are already removed must return 0 with no errors. |
| `INV-NETBAT-BOUNDARY-THIN` | netbat remains a sync-first boundary over syncbat; it validates and frames calls but does not own handler execution, receipt emission, durable writes, async runtime policy, or application protocol semantics. |
| `INV-NETBAT-LINE-PROTOCOL-STABLE` | netbat request and response frames keep stable NETBAT/1 CALL and OK/ERR line shapes, bounded decode behavior, and deterministic error-code mapping. |
| `INV-NO-DEAD-CODE-SILENCERS` | dead_code lint suppressions are banned in tracked Rust source. The AST walker in shared_checks::collect_dead_code_silencer_sites rejects #[allow(dead_code)], #[expect(dead_code)], crate-inner forms, lint-list forms containing dead_code, #[allow(unused)] and #[expect(unused)] (the `unused` group subsumes dead_code), and cfg_attr wrappers around any of those, including multi-line wrappers. The only escape hatch is an exact-site entry in traceability/dead_code_silencer_allowlist.yaml with non-empty reason + adr fields that resolve to a real ADR. The honest responses to a dead_code warning are #[cfg(test)] for test-only code, deletion for truly unused code, or restructuring (workspace crate, narrower module, finer-grained #[path] include) so the compilation unit matches actual ownership. See ADR-0012. |
| `INV-NO-TOKIO-PROD` | Production dependencies remain runtime-agnostic; Tokio stays out of non-dev dependencies. |
| `INV-OBSERVABILITY-FAILURE-PATHS` | Named flows emit enough telemetry to distinguish successful execution from cache-degraded or causal-reaction execution paths. |
| `INV-ONDISK-FORWARD-COMPAT-CANONICAL` | Every on-disk format declared in the compat matrix opens or fails with exactly one canonical typed error per (writer_version, reader_version) pair - never silent corruption and never a silent rebuild-from-scan downgrade. A reader opening an artifact written at its own live version succeeds (the OpensOK self-row), and an artifact written at a strictly-newer version is a canonical typed refusal. The governed formats and their future-version refusals are - mmap-index (index.fbati, StoreError MmapFutureVersion), checkpoint (index.ckpt, StoreError CheckpointFutureVersion), idempotency-index (index.idemp, StoreError IdempotencyFutureVersion), and visibility-ranges (visibility_ranges.fbv, StoreError HiddenRangesFutureVersion); each refusal is propagated out of cold-start rather than degrading to a scan, while corrupt or older artifacts keep their graceful-rebuild path. The matrix is the live contract, so a bump to an on-disk version const with no matching matrix row is caught because the self-row reader_version is cross-checked against the live supported version. The segment/.fbat format is intentionally excluded - its version is msgpack-encoded (no fixed-offset forge) and a future-version segment already fails closed via CorruptSegment with no silent-degrade path. |
| `INV-OPEN-REPORT-RECEIPT` | Cold-start observability is a real receipt surface: diagnostics().open_report and the open-report observer expose current reopen fallback totals/histograms plus cumulative fallback totals/histograms persisted through mmap/checkpoint artifacts; mutable opens also append one durable SYSTEM_OPEN_COMPLETED lifecycle event while read-only opens remain side-effect free. |
| `INV-OUTBOX-DROP-SAFETY` | Dropping a producer outbox without flush cannot publish buffered events; only explicit flush/submit paths cross the writer boundary. |
| `INV-OUTCOME-FUNCTOR-COMPOSITION` | Outcome map distributes over function composition through every variant including the Batch arm, Batch concatenation is an associative monoid with the empty batch as unit, and zip selects the maximum-priority variant of its two inputs. |
| `INV-PAYLOAD-LENGTH-EXACT` | Serialized payload lengths must be computed exactly and rejected when they exceed the wire contract. |
| `INV-PAYLOAD-VERSION-NONZERO` | A typed event payload always carries a non-zero PAYLOAD_VERSION in the event header; the reserved legacy sentinel 0 marks an untyped/legacy frame, so the typed-append seam rejects any payload that forges PAYLOAD_VERSION = 0 (the derive macro forbids it at compile time and the runtime seam forbids a hand-written impl), keeping typed frames distinguishable from legacy frames on read. |
| `INV-PER-LANE-FRONTIER` | Frontier state is a lattice with one global physical durability axis and per-lane logical accepted, written, durable, visible, applied, and emitted HLC tracks. The lattice ordering for lane durability and visibility is the shared global_sequence axis, not wall-clock HLC ordering. For every lane, accepted >= written >= durable, accepted >= visible >= applied, emitted >= visible, and logical durable never exceeds the global physical durable point. Lane-scoped reads and waits use the lane cursor; cancelled visibility ranges are persisted with per-lane ranges over the shared global_sequence axis; legacy global reads and frontier fields remain the max compatibility view. |
| `INV-PERFORMANCE-GATES-ENFORCED` | Hardware-dependent performance gates remain explicit ignored tests and are runnable only through the repo-owned perf-gates command surface. |
| `INV-PLATFORM-EVIDENCE-NOT-MEANING` | store::platform may perform target-sensitive machine-contact operations and emit descriptive evidence/profile records, but it must not define batpak durability, replay, visibility, or admission semantics. Store, cold-start, segment, and frontier logic own those guarantees; configured profile mismatch is a hard open-time admission failure. |
| `INV-POSITION-HINT-PERSISTENCE` | Append position hints may supply only DAG lane/depth; writer-owned HLC wall/counter and sequence remain authoritative, and non-root lane/depth must survive live commit, mmap reopen, checkpoint reopen, SIDX-backed reconstruction, and full rebuild. |
| `INV-PREPARED-BATCH-STAGING-EQUIVALENCE` | Prepared-batch staging and fresh batch construction must commit, reopen, and publish the same runtime-visible outcomes; reused staged packets may optimize allocation but must not alter persisted or replayed semantics. |
| `INV-PROJECTION-FUSION-EQUIVALENCE` | Folding multiple catamorphisms in one fused banana-split traversal equals folding each fold separately, for any generated event stream and any fold pair or triple, while reading the stream exactly once. |
| `INV-PROJECTION-FUSION-EQUIVALENT` | Fused projection replay over an entity is equivalent to the tuple of separate consistent projections: one shared direct replay stream is read once, each projection folds only events matching its declared relevant_event_kinds, projection-applied notifications use each projection's own per-lane relevant input maxima even when the projection returns None, and cache watermarks remain projection-specific because the fused path does not write cache rows. |
| `INV-RECEIPT-SEALED` | Receipts are only constructible by gate evaluation, not by external callers. |
| `INV-RECOVERY-ORACLE-LEGAL` | A real Store opened over the fault-injecting SimFs backend (plus the durability-boundary fault injector), driven through the real append/append_batch/sync API, crashed under EACH hostile-fs fault mode SimFs can model — honest-disk crash, lying-disk fsync-drop, and crash-before-fsync at each durability boundary (single-append frame write, batch-commit marker, post-fsync-before-publish, segment-rotation create) — and reopened over the persisted (truncated) tree must recover a state that is EXACTLY one of {CommittedPrefix \| RolledBack \| CanonicalRefusal} and LEGAL: a prefix of the appended op-log (no invented or undead events) with an intact hash chain across the recovered visible events; HONEST-disk modes never lose an acknowledged-durable commit (the sacred rule); LYING-disk modes may legally lose a dropped-fsync commit but the result must STILL be a prefix, undead-free, and chain-intact (losing the dropped commit is the FS's fault, exposing an undead/corrupt one is not); a typed corruption refusal on reopen is a legal outcome, an untyped failure/panic is not. The same (BATPAK_SEED, fault mode) recovers the identical classification and op-trace digest (determinism). |
| `INV-REPLAY-LANE-SELECTION` | Projection replay lane selection stays sealed to the built-in `ProjectionInput` modes, with `ReplayLane` naming the live default-vs-raw split and `RawMsgpackInput` remaining a real throughput lane rather than doc-only decoration. |
| `INV-SCHEMA-VERSION-ISOLATION` | Cached projections are keyed by entity + schema_version. Different versions get separate cache slots, preventing stale cached values from being served to new code. |
| `INV-SEMANTIC-DIFF-EQUIVALENCE` | Every pair of store configurations that claim to expose identical visible truth (mmap<->scan, checkpoint<->rebuilt, fused<->unfused projection, cached<->uncached, and reopened cold-start across those representations) produces byte-identical observables — query results across every region shape, the visible HLC frontier, and the global sequence — when fed the same seeded operation stream under a fixed clock; any divergence between an equivalence-claiming pair is a hard finding. |
| `INV-SIDX-TIMESTAMP-US-APPROXIMATION` | SIDX-accelerated cold start reconstructs timestamp_us from persisted wall_ms to the nearest millisecond; this best-effort value is suitable for age/display APIs but is not a sub-millisecond ordering contract. |
| `INV-STORE-ERROR-TAXONOMY` | Store failure paths must preserve structured error variants instead of laundering failures into defaults or strings. |
| `INV-STORE-ISOMORPHISM-LAWS` | Serialization seams that define durable store identity round-trip generated values without loss: DagPosition MessagePack encoding, SIDX entry encoding, lane-neutral JSON-to-MessagePack upcast, and raw imported payload hashing all preserve their intended value or byte identity. |
| `INV-STORE-LIFECYCLE-HONESTY` | Store sync and drop paths must surface writer failures honestly and send best-effort shutdown rather than silently succeeding. |
| `INV-STORE-SYNC-ONLY` | The Store public API remains synchronous. |
| `INV-SUBSCRIPTION-STATE-MACHINE` | Subscription delivery follows the open, receive, close state machine without fabricating events or hiding a closed producer. |
| `INV-SUBSTRATE-TRAVERSAL-DOMAIN-NEUTRAL` | Traversal terminals return substrate metadata only; they must not expose Downstream missions, workflow verbs, movement-graph semantics, receipt_kind dispatch, decoded envelope bodies, or domain replay names. |
| `INV-SYNCBAT-DISPATCH-RECEIPTS` | syncbat checkout dispatch emits at most one completed or failed runtime receipt for resolved operations, emits no receipt for unknown operations, and fails closed when the configured receipt sink fails. |
| `INV-SYNCBAT-REGISTER-CATALOG-DETERMINISTIC` | syncbat durable register catalog rows fold in store sequence order, reject malformed or conflicting lifecycle transitions, and rebuild the same active register after reopen. |
| `INV-TEST-PANIC-AS-ASSERTION` | Test bodies under crates/core/tests/ use panic!, unwrap, and expect as the assertion style — when a test precondition, fixture setup, or invariant check fails, the panic is the test failure signal. File-level #![allow(clippy::unwrap_used, clippy::panic)] is the acceptable scope for this idiom. Tests using panic! for invariant failures and test-local FaultInjector impls that panic in writer threads must opt out of clippy::panic at module scope and document the exception in a leading "justifies:" comment. |
| `INV-TRACEABILITY-COMPLETE` | Each important requirement, invariant, and flow links to concrete proving artifacts, and every artifact participates in that graph. |
| `INV-TYPELEVEL-COMBINATOR-LAWS` | Public outcome and monadic combinators preserve their type-level success, retry, cancelled, and error law shapes under property tests. |
| `INV-TYPESTATE-OPEN-HAS-WRITER` | Store<Open> is only constructible by Store::open after the writer thread is initialized; Store::<Open>::writer_ref() is the single crate-private access point for the writer handle, and its expect is a typestate invariant assertion that cannot fail in reachable code. |
| `INV-WIRE-ROUNDTRIP-TOTALITY` | Public wire/event/id surfaces round-trip every valid generated value and reject malformed bytes or identifiers without partial decoding. |
| `INV-WORKSPACE-DAG-ACYCLIC` | The intra-workspace crate dependency graph is a directed acyclic graph; the triangulation gate cross-checks two independent edge derivations (cargo-metadata path dependencies and a direct manifest scan) and fails on either an inter-oracle disagreement or an agreed dependency cycle, so layering and incremental builds are never broken by a reverse or circular crate edge. |

<!-- END INV-CATALOG -->
