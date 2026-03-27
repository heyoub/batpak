# Free-B (batteries) — Airgapped Build Spec

One document. No overrides. No layers. Every decision is final. A cloud agent with zero prior context builds the entire library from this file.

---

## INVARIANTS (check every decision against these — they override everything)

```
1. NO TOKIO IN REQUIRED DEPS. The library is runtime-agnostic.
   tokio stays in dev-deps only. Fan-out uses Vec<flume::Sender>.

2. STORE API IS SYNC. One set of methods. Bisync is a CHANNEL property,
   not an API property. The Store doesn't know if its caller is sync or async.

3. NO PRODUCT CONCEPTS IN LIBRARY CODE. Library vocabulary:
   coordinate, entity, event, outcome, gate, region, transition.
   build.rs checks type declarations for: "trajectory", "artifact", "tenant".
   ("scope", "agent", "turn", "note" are common English substrings —
   "return" contains "turn", "annotation" contains "note" — so they are
   NOT auto-checked. Use judgment for these in code review.)

4. NO SPECULATIVE ABSTRACTIONS. No trait until there's a second impl.
   No generic param until there's a second type. No module until there's
   enough code. Test: "Does removing this break working code? If no, skip."

5. BLAKE3 IS THE ONLY HASH. No HashAlgorithm enum. No EventVerifier trait.
   One function: compute_hash(bytes) -> [u8; 32], behind feature = "blake3".
```

---

## RED FLAGS (if you do any of these, you violated the design)

```
✗ DO NOT transmute, mem::read, or pointer-cast EventHeader from raw bytes.
  repr(C) is for deterministic FIELD ORDERING in Rust, not a wire format.
  All serialization goes through MessagePack (rmp-serde). Always.

✗ DO NOT add async fn to any public Store method.
  If you wrote ".await" inside store/mod.rs, you broke Invariant 2.
  Async lives in flume channels and product code. Not here.

✗ DO NOT add a third RestartPolicy variant.
  Once and Bounded. That's it. If you're reaching for exponential backoff
  curves or jitter configs, the product should wrap Store in a supervisor.

✗ DO NOT store product domain types in library code.
  If your type name contains a business noun (trajectory, agent, artifact,
  tenant, scope, note, turn), it goes in the product, not the library.

✗ DO NOT auto-store Denials in the event log.
  The library returns Denial to the caller. The caller decides whether to
  persist it. Auto-storing creates a DoS vector (spam invalid requests →
  fill the log with rejections).

✗ DO NOT add a trait for what has one implementation.
  Store is a struct. ProjectionCache is a trait because it has NoCache +
  RedbCache + LmdbCache. If your new thing has one impl, it's a struct.

✗ DO NOT bypass the Receipt to commit.
  If you can call store.append() without a Receipt<T> from the pipeline,
  you broke the TOCTOU guarantee. The ONLY bypass path is Pipeline::bypass()
  which produces a BypassReceipt with an auditable reason.

✗ DO NOT use std::time::Duration in serializable types.
  All durations are u64 milliseconds. Duration doesn't implement Serialize
  without custom serde impls. u64 millis is portable across languages.

✗ DO NOT assume the index fits in memory forever.
  10M events ≈ 2-3GB RAM. This is a declared trade-off, not an accident.
  If your test creates 100K events without a tempdir cleanup, you're
  leaking real memory. global_sequence exists for future partial loading.

✗ DO NOT put uuid::Uuid in the public API.
  All IDs are u128. The uuid crate is used internally by define_entity_id!
  for now_v7() generation. The public surface never exposes Uuid.
```

---

## ANTI-ALMOST-CORRECTNESS PROTOCOL

The most dangerous code is code that *looks* correct but doesn't compile — or compiles
but doesn't do what you think. AI-generated code is especially prone to this: hallucinated
APIs, missing trait bounds, dead logic branches, wrong feature flags. The fix isn't more
review — it's **making the toolchain the reviewer**.

### Rules

```
1. EVERY COMPILATION FIX GENERATES A REGRESSION TEST.
   The test must fail without the fix. If you fix a bug and don't write
   a test that would have caught it, the fix is incomplete.

2. cargo test --all-features IS THE ONLY ACCEPTANCE GATE.
   If it doesn't pass, the code doesn't exist. There is no "it compiles
   so it's probably fine" — compilation is necessary but not sufficient.

3. THE LIBRARY DOGFOODS ITS OWN GATE SYSTEM.
   tests/self_benchmark.rs uses a Gate<ColdStartContext> to validate
   cold-start performance. If gates work, the test passes. If the test
   passes, gates work. This is the quadratic feedback loop.

4. HALLUCINATED APIs ARE CAUGHT BY INTEGRATION TESTS.
   Every trait method on ProjectionCache is exercised against every backend
   (NoCache, RedbCache, LmdbCache). If an API doesn't exist, the test
   fails before it reaches CI. See: LmdbCache::delete_prefix (never existed).

5. AI-GENERATED CODE FOLLOWS THE SAME SPEC AS HUMAN CODE.
   The spec IS the compiler after first build. If an AI produces code
   that references [FILE:tests/monad_laws.rs] but doesn't create the file,
   the tests will catch it. The file either exists and passes, or it doesn't.

6. DEAD LOGIC IS A BUG, NOT A STYLE ISSUE.
   A condition that can never be true (e.g., `x.is_none()` guarding
   `if let Some(v) = x`) means the code doesn't do what the author intended.
   Tests must exercise all query paths to surface these.
```

### Feedback Loop Topology

```
                    ┌─── build.rs invariant checks ──────┐
                    │                                     │
   SPEC.md ──────→ Code ──→ cargo check ──→ clippy ──→ tests
     │               │                                    │
     │               └─── benchmarks ──────────────────→  │
     │                                                    │
     └──── self_benchmark.rs (dogfood Gate) ──────────────┘
                         ↑
                    quadratic: the test uses the system
                    being tested to validate the test
```

### Test-to-Fix Traceability (Initial Audit)

| Fix | Root Cause | Test That Catches It |
|-----|-----------|---------------------|
| serde `"rc"` feature | Arc<str> not Serialize | wire_format::coordinate_msgpack_round_trip |
| `use redb::ReadableTable` | trait method not in scope | store_integration (any RedbCache test) |
| LmdbCache::delete_prefix | hallucinated API | store_integration (any LmdbCache test) |
| `T: Clone` on join_all | Outcome::map needs Clone | monad_laws::join_all_all_ok (Batch cases) |
| DashMap Ref lifetime | flat_map escapes guard | store_integration::query_by_scope |
| Dead logic in query() | unreachable branch | store_integration::query_by_entity_prefix |
| `.unwrap()` → `.expect()` | clippy::unwrap_used deny | cargo clippy --all-features |
| `///` → `//` | doc comment on non-item | cargo check (warning → error in CI) |

---

## WHAT V1 IS

```
batpak v1 is a coordinate-addressed append-only causal log with
typestate-aware transitions and projection replay.

It IS:  a library for building event-sourced state machines over coordinate spaces.
It is NOT:  multi-lane, distributed, a transformer-gate algebra, or a generic
            storage trait ecosystem. It is DAG-ready, not DAG-complete.
```

---

## SIX PRINCIPLES

```
1. Everything is somewhere.        -> coordinate/
2. Everything has an outcome.      -> outcome/
3. Allowed = constructible.        -> guard/
4. Writing is executing.           -> store/
5. Phase tells you what you can do -> pipeline/
6. Structure survives transform.   -> (functor laws on Outcome, tested via proptest)
```

---

## FILE TREE

```
batpak/
├── .cargo/config.toml
├── .config/nextest.toml
├── .github/workflows/ci.yml
├── .gitignore
├── build.rs                    # pre-flight invariant enforcement (runs every cargo command)
├── Cargo.toml
├── CHANGELOG.md
├── LICENSE-APACHE
├── LICENSE-MIT
├── README.md
├── clippy.toml
├── justfile
├── rust-toolchain.toml
│
├── src/
│   ├── lib.rs
│   ├── wire.rs               # serde helpers: u128 as [u8;16] BE (see WIRE FORMAT DECISIONS)
│   ├── prelude.rs
│   │
│   ├── coordinate/
│   │   ├── mod.rs            # Coordinate (Arc<str>), Region, CoordinateError, KindFilter
│   │   └── position.rs       # DagPosition (wall_ms, counter, depth, lane, sequence)
│   │
│   ├── outcome/
│   │   ├── mod.rs            # Outcome<T> — 6 variants + all combinators
│   │   ├── error.rs          # OutcomeError, ErrorKind (9 + Custom(u16))
│   │   ├── combine.rs        # zip, join_all, join_any
│   │   └── wait.rs           # WaitCondition, CompensationAction
│   │
│   ├── event/
│   │   ├── mod.rs            # Event<P>, StoredEvent<P>
│   │   ├── header.rs         # EventHeader (repr(C), deterministic layout)
│   │   ├── kind.rs           # EventKind (private u16)
│   │   ├── hash.rs           # HashChain + compute_hash() + verify_chain() (NO trait)
│   │   └── sourcing.rs       # EventSourced<P> + Reactive<P>
│   │
│   ├── guard/
│   │   ├── mod.rs            # Gate<Ctx> trait, GateSet<Ctx>
│   │   ├── denial.rs         # Denial struct
│   │   └── receipt.rs        # Receipt<T> (sealed, consumed once)
│   │
│   ├── pipeline/
│   │   ├── mod.rs            # Pipeline<Ctx>, Proposal<T>, Committed<T>
│   │   └── bypass.rs         # BypassReason trait, BypassReceipt<T>
│   │
│   ├── store/
│   │   ├── mod.rs            # Store, StoreConfig, StoreError, AppendReceipt, StoredEvent
│   │   ├── segment.rs        # SegmentHeader, frame_encode/decode, FramePayload<P>
│   │   ├── writer.rs         # WriterHandle, WriterCommand, SubscriberList, 10-step commit
│   │   ├── reader.rs         # Reader (LRU FD cache, pread, CRC32 verify)
│   │   ├── index.rs          # StoreIndex, IndexEntry, ClockKey, DiskPos
│   │   ├── projection.rs     # ProjectionCache trait, NoCache, RedbCache, Freshness
│   │   ├── cursor.rs         # Cursor (pull-based, guaranteed delivery)
│   │   └── subscription.rs   # Subscription (push-based, per-subscriber flume channels)
│   │
│   ├── typestate/
│   │   ├── mod.rs            # define_state_machine!, define_typestate!
│   │   └── transition.rs     # Transition<From, To, P>
│   │
│   └── id/
│       └── mod.rs            # EntityIdType trait (Layer 0) + define_entity_id! macro (Layer 1+)
│
├── tests/
│   ├── monad_laws.rs         # proptest: left/right identity, associativity, Batch distribution
│   ├── hash_chain.rs         # proptest: chain verification, tamper detection, genesis
│   ├── store_integration.rs  # tempdir: append/get/query, rotation, cold start, concurrent r/w
│   ├── gate_pipeline.rs      # registration order, fail-fast, receipt TOCTOU, consumed once
│   ├── typestate_safety.rs   # trybuild: compile-fail for invalid transitions + forged receipts
│   ├── wire_format.rs        # golden file comparison for MessagePack serialization
│   ├── self_benchmark.rs     # gate that validates cold start < 200ms (library tests itself)
│   ├── ui/                   # trybuild compile-fail test cases
│   │   ├── forge_receipt.rs
│   │   └── invalid_transition.rs
│   └── golden/               # wire format golden files (msgpack bytes, hex-encoded)
│       └── event_header_v1.hex
│
└── benches/
    ├── write_throughput.rs   # criterion: events/sec for 1K/10K/100K appends
    ├── cold_start.rs         # criterion: index rebuild for 1K/10K/100K/1M events
    └── projection_latency.rs # criterion: cache hit vs miss for EventSourced projection
```

---

## Cargo.toml

```toml
[package]
name = "batpak"
version = "0.1.0"
edition = "2021"
rust-version = "1.75"
license = "MIT OR Apache-2.0"
description = "Event-sourced state machines over coordinate spaces"

[features]
default = ["blake3"]
blake3 = ["dep:blake3"]
redb = ["dep:redb"]
lmdb = ["dep:heed"]

[dependencies]
uuid = { version = "1", features = ["v7"] }
serde = { version = "1", features = ["derive", "rc"] }
serde_json = "1"
blake3 = { version = "1", optional = true }
flume = "0.11"
crc32fast = "1"
rmp-serde = "1"
dashmap = "5"
parking_lot = "0.12"
tracing = "0.1"
redb = { version = "2", optional = true }
heed = { version = "0.20", optional = true }
# NO TOKIO. Invariant 1.

[dev-dependencies]
proptest = "1"
criterion = { version = "0.5", features = ["html_reports"] }
tempfile = "3"
trybuild = "1"
tokio = { version = "1", features = ["rt", "macros"] }

[lints.clippy]
dbg_macro = "deny"
todo = "deny"
unimplemented = "deny"
unwrap_used = "deny"
panic = "deny"
print_stdout = "deny"
print_stderr = "deny"
large_enum_variant = "warn"
clone_on_ref_ptr = "warn"
needless_pass_by_value = "warn"
module_name_repetitions = "allow"
must_use_candidate = "allow"
missing_errors_doc = "allow"

[[bench]]
name = "write_throughput"
harness = false

[[bench]]
name = "cold_start"
harness = false

[[bench]]
name = "projection_latency"
harness = false
```

---

## WIRE FORMAT DECISIONS

```
MessagePack (rmp-serde) is the ONLY wire format for segments. These decisions
are load-bearing for golden file tests and cross-language readers.

1. ALL segment serialization uses rmp_serde::to_vec_named() (NOT to_vec()).
   Named mode preserves field names as map keys. Positional arrays break
   silently when fields are added or reordered. to_vec_named() survives.

2. u128 fields serialize as [u8; 16] big-endian via a shared serde helper.
   MessagePack has no native u128. Bare u128 fields cause rmp-serde errors.

   Helper module: src/wire.rs (~50 LOC, zero internal dependencies)
     pub fn serialize<S: Serializer>(val: &u128, ser: S) -> Result<S::Ok, S::Error>
       → val.to_be_bytes(), serialize as bytes
     pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<u128, D::Error>
       → deserialize bytes, u128::from_be_bytes

   For Option<u128>:
     pub mod option_u128_bytes  (same file, ~15 LOC)
       serialize: None → serialize_none, Some(v) → v.to_be_bytes()
       deserialize: visit_none → None, visit_bytes → Some(u128::from_be_bytes)

   Annotated fields (every u128 in a serializable type):
     EventHeader: event_id, correlation_id
     EventHeader: causation_id (uses option_u128_bytes)
     Notification: event_id, correlation_id
     Notification: causation_id (uses option_u128_bytes)
     Committed<T>: event_id
     WaitCondition::Event: event_id
     CompensationAction::Notify: target_id
     CompensationAction::Rollback: event_ids (Vec<u128> — use a vec_u128_bytes helper)
     CompensationAction::Release: resource_ids (Vec<u128> — same helper)
     Outcome::Pending: resume_token

   Big-endian preserves sort order and is the standard network byte order.

3. Option<T> serializes as msgpack nil for None (default serde behavior).
   Use #[serde(skip_serializing_if = "Option::is_none", default)] on optional
   fields for clean forward compatibility.
```

---

## TOOLCHAIN ENFORCEMENT

The spec becomes the compiler after first build. Every invariant and red flag
that can be statically detected gets a build-time or test-time enforcement.
If it fails, the message IS the documentation — it names the invariant, the
files to check, and the fix.

```
build.rs — runs before every cargo build/check/test. Cannot be skipped.
  CHECKS:
    Invariant 1: tokio not in [dependencies] (only [dev-dependencies])
    Invariant 3: no banned product nouns (trajectory, scope, artifact, agent,
      tenant, turn, note) in struct/enum/fn names in src/**/*.rs
    Red flag: no transmute, mem::read, or pointer_cast in src/
    Red flag: no async fn in src/store/
    Red flag: no std::time::Duration in types with #[derive(Serialize)]
  ON FAILURE: panic!() with invariant number + explanation + file path

compile_error!() — placed at top of modules agents try to "improve":
    src/store/mod.rs:
      #[cfg(feature = "async-store")]
      compile_error!("Invariant 2: Store API is sync. Async callers use \
        spawn_blocking() or flume recv_async(). See store/subscription.rs.");
    src/event/hash.rs:
      #[cfg(feature = "sha256")]
      compile_error!("Invariant 5: blake3 is the only hash. No HashAlgorithm \
        enum. One function: compute_hash(bytes) -> [u8; 32].");
    src/store/writer.rs:
      #[cfg(feature = "exponential-backoff")]
      compile_error!("Red flag: only Once and Bounded restart policies. \
        Exponential backoff belongs in the product's supervisor, not here.");

clippy denials — per-module guardrails beyond Cargo.toml lints:
    src/store/mod.rs:     #![deny(clippy::future_not_send)]
    src/guard/receipt.rs:  // Receipt must NOT derive Clone (enforced by not deriving it;
                           // trybuild test verifies cloning fails to compile)

Test diagnostic messages — every test failure includes:
    1. Which invariant/property broke
    2. Which files to investigate
    3. Common causes
    4. Next command to run
  Example (self_benchmark.rs cold start test):
    assert!(elapsed < Duration::from_millis(200),
      "COLD START REGRESSION: {elapsed:?} > 200ms.\n\
       Check: store/index.rs scan_segment(), store/reader.rs.\n\
       Common causes: unnecessary deserialization, missing BTreeMap pre-alloc.\n\
       Run: cargo bench --bench cold_start");

Golden test data — tests/golden/*.hex:
    Hex-encoded MessagePack bytes for known structs.
    wire_format.rs serializes known values, compares to golden files.
    To update: GOLDEN_UPDATE=1 cargo test wire_format

justfile dep-doc targets:
    deps-doc:
        cargo doc --document-private-items --no-deps --open
    lib-doc:
        cargo doc --all-features --open  # includes all dependency docs
```

---

## PER-FILE PRDs

Every PRD below is the FINAL version. No overrides exist. Implement exactly what's described.

---

### `build.rs`

```
Pre-flight invariant enforcement. Runs before every cargo build/check/test.
~80 LOC. No external deps (build script uses only std).

fn main():
  1. Read Cargo.toml as string
     - Split on "[dependencies]", take the section before next "[" header
     - If that section contains "tokio": panic with Invariant 1 message
  2. Walk src/**/*.rs files (use std::fs::read_dir recursively)
     - For each file, read contents as string
     - Check for banned patterns:
       a. transmute|mem::read|pointer_cast → panic with Red Flag message
       b. In src/store/*.rs: async fn → panic with Invariant 2 message
       c. Struct/enum/fn names containing: trajectory|scope|artifact|agent|
          tenant|turn|note → panic with Invariant 3 message
          (Match on: "struct ", "enum ", "fn ", "type " followed by a name
          containing a banned word. NOT string literals or comments.)
  3. println!("cargo:rerun-if-changed=Cargo.toml");
     println!("cargo:rerun-if-changed=src/");

Panic messages follow the pattern:
  "INVARIANT {N} VIOLATED: {what happened}.\n\
   {why this is wrong}.\n\
   {what to do instead}.\n\
   See: SPEC.md ## INVARIANTS, item {N}."
```

---

### `src/lib.rs`

```
Crate root. Module declarations + getting-started guide in doc comments.

Doc comment structure:
  Paragraph 1: "batpak is a library for building event-sourced
    state machines over user-defined coordinate spaces."
  Paragraph 2: Four concepts — Coordinate (where), Outcome (what happened),
    Gate (who decides), Store (the runtime).
  Paragraph 3: 12-line hello world (pure sync, fn main, no tokio).
  Paragraph 4: Reading order: coordinate → outcome → event → guard →
    pipeline → store → typestate.

Module declarations in DEPENDENCY ORDER:
  pub mod wire;        // serde helpers — no deps, must come first
  pub mod coordinate;
  pub mod outcome;
  pub mod event;
  pub mod guard;
  pub mod pipeline;
  pub mod store;
  pub mod typestate;
  pub mod id;
  pub mod prelude;

Each module's doc comment teaches the concept it implements:
  P1: What this module is (one sentence)
  P2: The concept (2-3 sentences, no jargon)
  P3: Minimum example (3-5 lines)
  P4: "Next: read [next module] to learn [next concept]"
```

---

### `src/prelude.rs`

```
15 types for 90% of usage. `use batpak::prelude::*`

Re-exports:
  Coordinate, Region, EventKind, DagPosition
  Outcome, OutcomeError, ErrorKind
  Event, EventHeader, HashChain, StoredEvent
  Gate, GateSet, Denial, Receipt
  Proposal, Committed
  Store
  EventSourced
```

---

### `src/coordinate/mod.rs`

```
Coordinate struct + Region struct + CoordinateError + KindFilter.

pub struct Coordinate {
    entity: Arc<str>,   // WHO — stream key, hash chain anchor
    scope: Arc<str>,    // WHERE — isolation boundary
}
Coordinate::new(entity: impl AsRef<str>, scope: impl AsRef<str>) -> Result<Self, CoordinateError>
  Validates both non-empty.
entity() -> &str, scope() -> &str
entity_arc() -> Arc<str>, scope_arc() -> Arc<str>  (pub(crate))
Display: "entity@scope"

pub enum CoordinateError { EmptyEntity, EmptyScope }
  impl Display, Error. Coordinate does NOT depend on StoreError.
  StoreError has From<CoordinateError> in the store layer.

pub struct Region {
    pub entity_prefix: Option<Arc<str>>,
    pub scope: Option<Arc<str>>,
    pub fact: Option<KindFilter>,
    pub clock_range: Option<(u32, u32)>,  // per-entity clock (IndexEntry.clock), NOT global_sequence
}

pub enum KindFilter {
    Exact(EventKind),
    Category(u8),    // matches any EventKind in this 4-bit category
    Any,
}

Region builder (method chaining):
  Region::all()
  Region::entity("player:alice")
  Region::scope("room:dungeon")
  Region::coordinate(&coord)
  .with_scope("x").with_fact(KindFilter::Exact(k)).with_fact_category(0xF)

Region replaces SubscriptionPattern. It is the ONE predicate type for:
  Applied to history  = query
  Applied to future   = subscription (push)
  Applied to cursor   = consumption (pull, guaranteed)
  Applied to chain    = traversal (walk_ancestors is the exception — see store)

Region::matches_event(&self, entity: &str, scope: &str, kind: EventKind) -> bool
  Used by Subscription to filter incoming events. Takes individual fields
  instead of Notification to avoid circular dep (coordinate → store).
```

---

### `src/coordinate/position.rs`

```
DagPosition. Graph position with hybrid logical clock + depth + lane + sequence.
wall_ms + counter provide global causal ordering (HLC-style) across entities.
depth/lane/sequence provide per-entity chain ordering.

#[repr(C)]
pub struct DagPosition {
    pub wall_ms: u64,    // Wall-clock milliseconds at event creation (HLC layer 1)
    pub counter: u16,    // HLC counter for same-millisecond tiebreaking
    pub depth: u32,
    pub lane: u32,
    pub sequence: u32,
}

const fn: new (depth/lane/seq, wall_ms=0), with_hlc (all fields),
          root, child (seq only), child_at (seq + HLC), fork, is_root, is_ancestor_of
Display: "depth:lane:sequence@wall_ms.counter"
PartialOrd for causal ordering (different lanes are incomparable).

v1: depth=0, lane=0 always. Sequence is per-entity monotonic counter.
wall_ms set by writer. Batched events get sequential positions (N, N+1, N+2...) on lane 0.
Lane/depth vocabulary is for future distributed fan-out/fan-in.
```

---

### `src/outcome/mod.rs`

```
Outcome<T>. The core algebraic type. Named "Outcome" not "Effect" to
eliminate Effect/Event confusion. The algebra is identical — Outcome IS the
effect functor at different commitment phases.

pub enum Outcome<T> {
    Ok(T),
    Err(OutcomeError),
    Retry { after_ms: u64, attempt: u32, max_attempts: u32, reason: String },
    Pending { condition: WaitCondition, resume_token: u128 },
    Cancelled { reason: String },
    Batch(Vec<Outcome<T>>),
}

6 variants. Compensate was folded into OutcomeError.compensation.
Join is join_all/join_any free functions in combine.rs.

Combinators (all distribute over Batch via F: Clone bound):
  map, and_then, map_err, or_else, flatten, inspect, inspect_err,
  and_then_if, into_result, unwrap_or, unwrap_or_else

Predicates: is_ok, is_err, is_retry, is_pending, is_cancelled, is_batch, is_terminal
Construction: ok, err, cancelled, retry, pending

The and_then monad fix:
  pub fn and_then<U, F: FnOnce(T) -> Outcome<U> + Clone>(self, f: F) -> Outcome<U>
  Distributes over Batch (recurses into each element).

Serde: #[derive(Serialize, Deserialize)]  (serde is always available)
  Adjacent tagging: #[serde(tag = "type", content = "data")]
  Durations as u64 millis.
  All u128 fields use #[serde(with = "crate::wire::u128_bytes")] — see WIRE FORMAT DECISIONS.
```

---

### `src/outcome/error.rs`

```
OutcomeError + ErrorKind.

pub struct OutcomeError {
    pub kind: ErrorKind,
    pub message: String,
    pub compensation: Option<CompensationAction>,
    pub retryable: bool,
}
impl Display, Error, Clone, PartialEq.

pub enum ErrorKind {
    NotFound, Conflict, Validation, PolicyRejection,
    StorageError, Timeout, Serialization, Internal,
    Custom(u16),
}
ErrorKind::is_retryable(), is_domain(), is_operational()

Products extend via Custom(u16) — same category:type encoding as EventKind.
```

---

### `src/outcome/combine.rs`

```
pub fn zip<A, B>(a: Outcome<A>, b: Outcome<B>) -> Outcome<(A, B)>
pub fn join_all<T>(batch: Vec<Outcome<T>>) -> Outcome<Vec<T>>
pub fn join_any<T>(batch: Vec<Outcome<T>>) -> Outcome<T>
```

---

### `src/outcome/wait.rs`

```
pub enum WaitCondition {
    Timeout { resume_at_ms: u64 },
    Event { event_id: u128 },
    All(Vec<WaitCondition>),
    Any(Vec<WaitCondition>),
    Custom { tag: u16, data: Vec<u8> },
}

pub enum CompensationAction {
    Rollback { event_ids: Vec<u128> },
    Notify { target_id: u128, message: String },
    Release { resource_ids: Vec<u128> },
    Custom { action_type: String, data: Vec<u8> },
}
```

---

### `src/event/mod.rs`

```
Event<P> + StoredEvent<P>.

pub struct Event<P> {
    pub header: EventHeader,
    pub payload: P,
    pub hash_chain: Option<HashChain>,
}
Event::new, with_hash_chain, event_id, event_kind, position, map_payload, is_genesis

pub struct StoredEvent<P> {
    pub coordinate: Coordinate,
    pub event: Event<P>,
}
This is what store.get() returns and what segments persist.
The coordinate is part of the stored fact, not separate metadata.

store.get() returns StoredEvent<serde_json::Value> because segments are
schema-free MessagePack. Round-trip: MyStruct → msgpack → serde_json::Value.
Use project<T: EventSourced>() for typed reconstruction.
```

---

### `src/wire.rs`

```
Serde helpers for types that MessagePack can't handle natively.
ZERO internal dependencies. This module is declared first in lib.rs
and available to every other module via crate::wire::.

See WIRE FORMAT DECISIONS for rationale.

pub mod u128_bytes {
    // #[serde(with = "crate::wire::u128_bytes")]
    pub fn serialize<S: Serializer>(val: &u128, ser: S) -> Result<S::Ok, S::Error>
    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<u128, D::Error>
}

pub mod option_u128_bytes {
    // #[serde(with = "crate::wire::option_u128_bytes")]
    pub fn serialize<S: Serializer>(val: &Option<u128>, ser: S) -> Result<S::Ok, S::Error>
    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Option<u128>, D::Error>
}

pub mod vec_u128_bytes {
    // #[serde(with = "crate::wire::vec_u128_bytes")]
    pub fn serialize<S: Serializer>(val: &[u128], ser: S) -> Result<S::Ok, S::Error>
    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<u128>, D::Error>
}

~50 LOC total. All #[inline]. Big-endian byte order.
```

---

### `src/event/header.rs`

```
#[repr(C)]
#[derive(Serialize, Deserialize)]
pub struct EventHeader {
    #[serde(with = "crate::wire::u128_bytes")]
    pub event_id: u128,
    #[serde(with = "crate::wire::u128_bytes")]
    pub correlation_id: u128,
    #[serde(with = "crate::wire::option_u128_bytes")]
    pub causation_id: Option<u128>,   // which event CAUSED this one (None = root cause)
    pub timestamp_us: i64,
    pub position: DagPosition,
    pub payload_size: u32,
    pub event_kind: EventKind,
    pub flags: u8,
    /// Content hash of the serialized payload. Enables automatic projection cache
    /// invalidation when event schemas evolve. [0u8; 32] when blake3 is off.
    #[serde(default)]
    pub content_hash: [u8; 32],
}
// No align(64) — causation_id pushes the struct past one cache line.
// Cache-line alignment isn't load-bearing for an append-only log
// (headers are read once from disk, not scanned in tight loops).
// #[repr(C)] kept for deterministic field layout in segment serialization.

THE STORE GENERATES THIS. Users never call EventHeader::new directly.
store.append(coord, kind, payload) fills in event_id (UUIDv7), timestamp,
position (from index), payload_size (from serialization). Constructor is
pub for testing and advanced use only.

  EventHeader::new(event_id, correlation_id, causation_id, timestamp_us, position, payload_size, event_kind)

  // correlation_id vs causation_id:
  //
  //   correlation_id: "these events are RELATED" — same workflow, same request,
  //     same logical operation. Many events share one correlation_id.
  //     Example: all events in a delegation chain share the delegation's correlation_id.
  //
  //   causation_id: "this event was CAUSED BY that specific event" — direct parent
  //     in the causal graph. Each event has at most one causation_id (its direct cause).
  //     Example: DELEGATION_ACCEPTED was caused by DELEGATION_CREATED.
  //     None means this event is a root cause (user action, external trigger).
  //
  //   Together they give you:
  //     correlation_id → "show me everything related to this workflow"
  //     causation_id chain → "trace the exact causal path that led to this event"
  //
  //   IndexEntry has both + helper methods:
  //     is_correlated() → event_id != correlation_id
  //     is_caused_by(id) → causation_id == Some(id)
  //     is_root_cause() → causation_id.is_none()
  with_flags(u8) -> Self            // builder
  requires_ack() -> bool            // flag bit 0
  is_transactional() -> bool        // flag bit 1
  is_replay() -> bool               // flag bit 3
  age_us(now_us: i64) -> u64        // convenience: now_us - timestamp_us
```

---

### `src/event/kind.rs`

```
pub struct EventKind(u16);  // PRIVATE inner field

EventKind::custom(category: u8, type_id: u16) -> Self
category() -> u8, type_id() -> u16, is_system(), is_effect()

Library constants ONLY:
  DATA=0x0000, SYSTEM_INIT=0x0001, SYSTEM_SHUTDOWN=0x0002,
  SYSTEM_HEARTBEAT=0x0003, SYSTEM_CONFIG_CHANGE=0x0004,
  SYSTEM_CHECKPOINT=0x0005,
  EFFECT_ERROR=0xD001, EFFECT_RETRY=0xD002, EFFECT_ACK=0xD004,
  EFFECT_BACKPRESSURE=0xD005, EFFECT_CANCEL=0xD006, EFFECT_CONFLICT=0xD007

Products: pub const PLAYER_MOVED: EventKind = EventKind::custom(0xF, 1);
```

---

### `src/event/hash.rs`

```
NO TRAIT. NO ENUM. Blake3 only (Invariant 5).

pub struct HashChain {
    pub prev_hash: [u8; 32],
    pub event_hash: [u8; 32],
}
Default: all zeros (genesis convention).

#[cfg(feature = "blake3")]
pub fn compute_hash(content_bytes: &[u8]) -> [u8; 32]

#[cfg(feature = "blake3")]
pub fn verify_chain(content_bytes: &[u8], chain: &HashChain, expected_prev: &[u8; 32]) -> bool

When blake3 feature is off, Committed.hash is [0u8; 32] (genesis convention).
~30 LOC total.
```

---

### `src/event/sourcing.rs`

```
EventSourced<P> + Reactive<P>.

pub trait EventSourced<P>: Sized {
    fn from_events(events: &[Event<P>]) -> Option<Self>;
    fn apply_event(&mut self, event: &Event<P>);
    fn relevant_event_kinds() -> &'static [EventKind];
}
P is generic. NO serde_json dependency in the trait definition.
The store uses EventSourced<serde_json::Value> (serde is always available).

pub trait Reactive<P> {
    fn react(&self, event: &Event<P>) -> Vec<(Coordinate, EventKind, P)>;
}
Forward-looking counterpart to EventSourced (backward-looking fold).
See event → maybe emit derived events. ~15 LOC. Same file.
Products compose: subscribe + react + append (7 lines of glue).
```

---

### `src/guard/mod.rs`

```
pub trait Gate<Ctx>: Send + Sync {
    fn name(&self) -> &'static str;
    fn evaluate(&self, ctx: &Ctx) -> Result<(), Denial>;
    fn description(&self) -> &'static str { "" }
}
Gates are PREDICATES, not transformers. No I/O, no mutation, pure.
Ctx is product-defined. Library is generic over it.

pub struct GateSet<Ctx> { gates: Vec<Box<dyn Gate<Ctx>>> }
  push(gate), evaluate(ctx, proposal) -> Result<Receipt<T>, Denial>,
  evaluate_all (no fail-fast, for observability), len, is_empty, names
```

---

### `src/guard/denial.rs`

```
pub struct Denial {
    pub gate: &'static str,
    pub code: String,
    pub message: String,
    pub context: Vec<(String, String)>,
}
Denial::new(gate, message), with_code, with_context
Display: "[gate] message"
Separate from OutcomeError. Library does NOT auto-store denials.
Products decide whether to persist denials as events.
Serde: #[derive(Serialize)] only — NOT Deserialize.
  gate is &'static str which can't be deserialized from owned data.
  Library never persists Denials (returns them to callers).
  Products serialize denials into their own event payloads if needed.
```

---

### `src/guard/receipt.rs`

```
pub struct Receipt<T> { _seal: seal::Token, gates_passed: Vec<&'static str>, payload: T }

NOT Clone. NOT Copy. NOT Serialize. Consumed exactly once.

mod seal { pub(crate) struct Token; }  // prevents external construction

Receipt::payload() -> &T
Receipt::gates_passed() -> &[&'static str]
Receipt::into_parts() -> (T, Vec<&'static str>)   // consuming extraction

TOCTOU fix: payload is INSIDE the receipt. Cannot mutate after gate evaluation.
Only constructible via GateSet::evaluate().
```

---

### `src/pipeline/mod.rs`

```
pub struct Proposal<T>(pub T);
  Proposal::new, payload, map

pub struct Committed<T> {
    pub payload: T,
    pub event_id: u128,
    pub sequence: u64,
    pub hash: [u8; 32],   // blake3, or [0u8;32] if feature off
}

pub struct Pipeline<Ctx> { gates: GateSet<Ctx> }
  Pipeline::new(gates)
  evaluate(ctx, proposal) -> Result<Receipt<T>, Denial>
  commit<E>(receipt: Receipt<T>, f: impl FnOnce(T) -> Result<Committed<T>, E>)
    -> Result<Committed<T>, E>
  // E is generic. The pipeline doesn't know about StoreError.
  // Products pass a closure that calls store.append() and wraps the result.

Library owns 2 stages: evaluate + commit.
Products wrap with assembly (before) and receipt generation (after).
```

---

### `src/pipeline/bypass.rs`

```
pub trait BypassReason: Send + Sync {
    fn name(&self) -> &'static str;
    fn justification(&self) -> &'static str;
}

pub struct BypassReceipt<T> {
    pub payload: T,
    pub reason: &'static str,
    pub justification: &'static str,
}

Pipeline::bypass(proposal, reason) -> BypassReceipt<T>
Audit trails show "bypassed: {reason}" with empty gate list.
```

---

### `src/store/mod.rs`

```
pub struct Store { index, reader, cache, writer, config }

Store::open(config: StoreConfig) -> Result<Self, StoreError>
Store::open_default() -> Result<Self, StoreError>  // ./batpak-data/

ALL METHODS ARE SYNC (Invariant 2):

WRITE:
  append(&self, coord: &Coordinate, kind: EventKind, payload: &impl Serialize)
    -> Result<AppendReceipt, StoreError>
    3 params. Store generates event_id, timestamp, position, hash chain.
    correlation_id defaults to event_id (self-correlated). causation_id = None (root cause).

  append_reaction(&self, coord: &Coordinate, kind: EventKind, payload: &impl Serialize,
                    correlation_id: u128, causation_id: u128)
    -> Result<AppendReceipt, StoreError>
    For events caused by another event. Sets both correlation and causation.
    Example: DELEGATION_ACCEPTED caused by DELEGATION_CREATED.

  append_with_options(..., opts: AppendOptions) — CAS, idempotency, custom correlation/causation
  apply_transition(coord, transition) — extracts kind+payload, delegates to append

READ:
  get(event_id: u128) -> Result<StoredEvent<serde_json::Value>, StoreError>
  query(region: &Region) -> Vec<IndexEntry>
  walk_ancestors(event_id: u128, limit: usize) -> Vec<StoredEvent<Value>>
    (special case — NOT Region-based; chain traversal is point-to-chain)

PROJECT:
  project<T: EventSourced<serde_json::Value>>(entity: &str, freshness: Freshness)
    -> Result<Option<T>, StoreError>

SUBSCRIBE:
  subscribe(region: &Region) -> Subscription     // push, per-subscriber flume channel
  cursor(region: &Region) -> Cursor              // pull, guaranteed delivery

CONVENIENCE (sugar over Region):
  stream(entity) = query(&Region::entity(entity))
  by_scope(scope) = query(&Region::scope(scope))
  by_fact(kind) = query(&Region::all().fact(KindFilter::Exact(kind)))

LIFECYCLE:
  sync(), snapshot(dest), compact(), close(self)

DIAGNOSTICS:
  stats() -> StoreStats, diagnostics() -> StoreDiagnostics

Async callers: use tokio::task::spawn_blocking(|| store.append(...)).await
Or use flume's async API on the channels directly.

Store: Send + Sync. Reader's LRU FD cache behind parking_lot::Mutex.
  Index is DashMap (Send + Sync). Writer communicates via flume (Send).
  Config is immutable after open().

Types owned by this module:
  Store, StoreConfig, StoreError (with From<CoordinateError>),
  StoreStats, StoreDiagnostics, StoredEvent<P>, AppendReceipt, AppendOptions,
  RestartPolicy

pub struct AppendOptions {
    pub expected_sequence: Option<u32>,     // CAS: reject if entity's latest clock != this
    pub idempotency_key: Option<u128>,      // dedup: skip if key already seen, return original receipt
    pub correlation_id: Option<u128>,       // override default (self-correlated)
    pub causation_id: Option<u128>,         // override default (root cause)
}

pub enum StoreError {
    Io(std::io::Error),
    Coordinate(CoordinateError),
    Serialization(String),
    CrcMismatch { segment_id: u64, offset: u64 },
    CorruptSegment { segment_id: u64, detail: String },
    NotFound(u128),                         // event_id not in index
    SequenceMismatch { entity: String, expected: u32, actual: u32 }, // CAS failure
    DuplicateEvent(u128),                   // idempotency_key already seen
    WriterCrashed,
    ShuttingDown,
    CacheFailed(String),
}
impl Display, Error, From<CoordinateError>, From<std::io::Error>.

StoreConfig includes:
  data_dir: PathBuf              (default: "./batpak-data")
  segment_max_bytes: u64         (default: 256MB)
  sync_every_n_events: u32       (default: 1000)
  fd_budget: usize               (default: 64)
  writer_channel_capacity: usize (default: 4096)
  broadcast_capacity: usize      (default: 8192)
  cache_map_size_bytes: usize    (default: 64MB, for LMDB)
  restart_policy: RestartPolicy  (default: Once)
  shutdown_drain_limit: usize    (default: 1024)
  writer_stack_size: Option<usize>  (default: None = OS default ~8MB on Linux)
  clock: Option<Arc<dyn Fn() -> i64 + Send + Sync>>  (default: None = SystemTime::now())
    Injectable clock for deterministic testing. Returns microseconds since epoch.
  sync_mode: SyncMode            (default: SyncAll)
    SyncAll = data+metadata (safest). SyncData = data only (faster).

In production, run under a process supervisor (systemd, k8s restart policy).
The library's RestartPolicy handles transient writer panics. Process-level
crashes require external restart. This is by design — a library is not a
process supervisor.
```

---

### `src/store/segment.rs`

```
Magic: b"FBAT", Header: 32 bytes, Frame: [len:u32 BE][crc32:u32 BE][msgpack]
Segment files named: {segment_id:06}.fbat (e.g., 000001.fbat). Sequential u64.
Cold start scans data_dir alphabetically (zero-padded = chronological order).

SegmentHeader { version: u16, flags: u16, created_ns: i64, segment_id: u64 }
frame_encode(data) -> Vec<u8>
  Serialization: rmp_serde::to_vec_named() — ALWAYS named mode (see WIRE FORMAT DECISIONS).
frame_decode(buf) -> Result<(&[u8], usize), StoreError>
FramePayload<P> { event: Event<P>, entity: String, scope: String }

Typestate: Segment<Active> (writable) vs Segment<Sealed> (immutable).
Rotation: seal + create new when segment exceeds config.segment_max_bytes.
Types: SegmentHeader, FramePayload<P>, CompactionResult
```

---

### `src/store/writer.rs`

```
Background OS thread ("batpak-writer-{hash}" where hash is FNV-1a of data_dir). Sync-first. flume channels.

WriterCommand { Append{entity,scope,event,respond}, Sync{respond}, Shutdown{respond} }
  All respond channels: flume::Sender (sync send from writer, async recv from caller)

WriterHandle { tx: flume::Sender<WriterCommand>, subscribers: Arc<SubscriberList>, thread }

struct SubscriberList {
    senders: parking_lot::Mutex<Vec<flume::Sender<Notification>>>,
}
  broadcast(notif): iterate with try_send(), retain on success or Full,
    prune on Disconnected. NEVER blocking send() — one slow subscriber must
    not block the writer thread. NO tokio::broadcast.
    Pattern:
      senders.retain(|tx| match tx.try_send(notif.clone()) {
          Ok(()) => true,
          Err(TrySendError::Full(_)) => true,         // keep, just slow
          Err(TrySendError::Disconnected(_)) => false, // prune
      });
  subscribe(capacity) -> flume::Receiver<Notification>

Notification {
    event_id: u128,
    correlation_id: u128,
    causation_id: Option<u128>,
    coord: Coordinate,
    kind: EventKind,
    sequence: u64,
}
// Reactive<P> consumers need correlation/causation to decide whether to react.

The 10-step commit protocol (handle_append):
  1. Acquire per-entity lock (DashMap<Arc<str>, Arc<Mutex<()>>>)
  2. Get prev_hash from index (or [0u8;32] for genesis)
  3. Compute sequence (latest.clock + 1, or 0)
  4. Set event header position
  5. Compute blake3 hash, set hash chain (or skip if feature off)
  6. Serialize to MessagePack + CRC32 frame
  7. Check segment rotation
  8. Write frame to segment file
  9. Update index
  10. Broadcast notification to subscribers

Backpressure: bounded channel (default 4096). Callers block when full.
Entity locks: grow without pruning (acceptable <100K entities).

Crash recovery via RestartPolicy (on StoreConfig):
  pub enum RestartPolicy {
      Once,                                          // default
      Bounded { max_restarts: u32, within_ms: u64 }, // production
  }
  Writer tracks restart count + timestamps. If count exceeds max within
  the window → StoreError::WriterCrashed. Otherwise restart + reset counter.
  Detection: WriterHandle sees flume send fail (receiver dropped on panic).
  ~20 LOC. Passes Invariant 4: Once and Bounded are two real impls.

Shutdown drain semantics:
  Writer receives Shutdown → drains up to config.shutdown_drain_limit
  (default 1024) queued Append commands → processes each → fsync →
  responds to Shutdown. Commands beyond the cap get StoreError::ShuttingDown
  on their respond channel. Producers that send after Shutdown get flume
  SendError because the channel is dropped after drain completes.
  This prevents silent data loss on close(). Products that call
  store.close() after a burst of appends get all queued events persisted
  (up to the cap). ~10 LOC.

  In production, set shutdown_drain_limit high enough to cover your
  burst size. In tests, default 1024 is fine.

// NOTE: CompensationAction exists on OutcomeError but the writer
// ignores it. Compensation handling is deferred — products implement it.

Tracing spans:
  warn!  — CRC mismatch, writer panic, segment corruption
  info!  — segment rotation, cold start complete, cache miss
  debug! — append committed, entity lock acquired, fsync
  trace! — frame written
```

---

### `src/store/reader.rs`

```
Reader with LRU file descriptor cache.
Reader::new(data_dir, fd_budget)
read_entry(disk_pos) -> Result<StoredEvent<serde_json::Value>, StoreError>
scan_segment(path) -> Result<Vec<ScannedEntry>, StoreError>
CRC32 verified on every read.
```

---

### `src/store/index.rs`

```
2D primary index + auxiliaries (NOT "4D" — fact and clock are event metadata).

pub(crate) struct StoreIndex {
    streams: DashMap<Arc<str>, BTreeMap<ClockKey, IndexEntry>>,     // primary
    scope_entities: DashMap<Arc<str>, HashSet<Arc<str>>>,           // scope dim
    by_fact: DashMap<EventKind, BTreeMap<ClockKey, IndexEntry>>,    // fact dim
    by_id: DashMap<u128, IndexEntry>,                               // point lookup
    latest: DashMap<Arc<str>, IndexEntry>,                          // chain head
    global_sequence: AtomicU64,  // monotonic counter (foundation for:
                                 //   1. cursors — track position
                                 //   2. checkpoints — record sequence at snapshot
                                 //   3. exactly-once — consumers track high-water mark)
    len: AtomicUsize,
}

pub struct IndexEntry {
    pub event_id: u128,
    pub correlation_id: u128,          // for O(1) correlation checks without disk read
    pub causation_id: Option<u128>,    // direct causal parent (None = root cause)
    pub coord: Coordinate,
    pub kind: EventKind,
    pub wall_ms: u64,                  // HLC wall clock milliseconds — for global causal ordering
    pub clock: u32,
    pub hash_chain: HashChain,
    pub disk_pos: DiskPos,
    pub global_sequence: u64,
}

impl IndexEntry {
    /// This event is part of a multi-event workflow (shares correlation_id with others).
    pub fn is_correlated(&self) -> bool {
        self.event_id != self.correlation_id
    }

    /// This event was directly caused by the given event.
    pub fn is_caused_by(&self, event_id: u128) -> bool {
        self.causation_id == Some(event_id)
    }

    /// This event is a root cause — not caused by any other event.
    pub fn is_root_cause(&self) -> bool {
        self.causation_id.is_none()
    }
}
// Memory: ~32 bytes added per IndexEntry (correlation_id u128 + causation_id Option<u128>).
// Total per entry: ~230-330 bytes. Worth it for O(1) causal queries without disk reads.

pub struct DiskPos { pub segment_id: u64, pub offset: u64, pub length: u32 }
pub struct ClockKey { pub wall_ms: u64, pub clock: u32, pub uuid: u128 }
// Ord: wall_ms-first, then clock, then uuid tiebreak.

Memory: ~200-300 bytes per IndexEntry. 10M events ≈ 2-3GB RAM. No eviction.
```

---

### `src/store/projection.rs`

```
pub trait ProjectionCache: Send + Sync + 'static {
    fn get(&self, key: &[u8]) -> Result<Option<(Vec<u8>, CacheMeta)>, StoreError>;
    fn put(&self, key: &[u8], value: &[u8], meta: CacheMeta) -> Result<(), StoreError>;
    fn delete_prefix(&self, prefix: &[u8]) -> Result<u64, StoreError>;
    fn sync(&self) -> Result<(), StoreError>;
}

pub struct CacheMeta { pub watermark: u64, pub cached_at_us: i64 }
pub enum Freshness { Consistent, BestEffort { max_stale_ms: u64 } }
pub struct NoCache;   // default: every read replays from segments

#[cfg(feature = "redb")] pub struct RedbCache { ... }
#[cfg(feature = "lmdb")] pub struct LmdbCache { ... }
```

---

### `src/store/cursor.rs`

```
Pull-based event consumption with guaranteed delivery.

pub struct Cursor { region: Region, position: u64, store: Arc<StoreIndex> }
  poll() -> Option<IndexEntry>         // next matching event after position
  poll_batch(max: usize) -> Vec<IndexEntry>

Reads from index, not channels. Cannot lose events.
```

---

### `src/store/subscription.rs`

```
Push-based per-subscriber flume channels. NO tokio::broadcast.

pub struct Subscription { rx: flume::Receiver<Notification>, region: Region }
  recv() -> Option<Notification>              // sync: blocks
  receiver() -> &flume::Receiver<Notification>  // for async: rx.recv_async().await

Lossy: if subscriber is slow, bounded channel fills. Writer's retain()
prunes dropped senders. For guaranteed delivery, use Cursor instead.

ASYNC NOTE: For async event consumption, use receiver().recv_async().await
directly on the flume channel. spawn_blocking is for Store read/write
methods only (append, get, query, project). These are two different async
patterns for two different things — don't conflate them.
```

---

### `src/typestate/mod.rs`

```
macro_rules! define_state_machine! { ... }
  Generates: sealed marker trait + zero-sized state structs.

macro_rules! define_typestate! { ... }
  Generates: PhantomData wrapper with data(), into_data(), new().

99 LOC of macros. Zero deps. Zero runtime code.
```

---

### `src/typestate/transition.rs`

```
pub struct Transition<From, To, P> {
    kind: EventKind,
    payload: P,
    _from: PhantomData<From>,
    _to: PhantomData<To>,
}
Transition::new(kind, payload), kind(), payload(), into_payload()

Usage:
  impl Lock<Acquired> {
      pub fn release(self) -> Transition<Acquired, Released, ()> {
          Transition::new(LOCK_RELEASED, ())
      }
  }
  store.apply_transition(&coord, lock.release())?;

Store extracts EventKind + payload, builds Event, appends.
Compiler ensures you can only call release() on Lock<Acquired>.
EventSourced replays by matching on EventKind.
Forward (Transition) and backward (EventSourced) share EventKind as common language.
```

---

### `src/id/mod.rs`

```
Layer 0 (trait, no uuid dep):
  pub trait EntityIdType:
      Copy + Clone + Eq + Hash + Debug + Display + FromStr + Send + Sync + 'static
  {
      const ENTITY_NAME: &'static str;
      fn new(id: u128) -> Self;
      fn as_u128(&self) -> u128;
      fn now_v7() -> Self;
      fn nil() -> Self;
  }

Layer 1+ (macro, uses uuid):
  macro_rules! define_entity_id! { ($name:ident, $entity:literal) => { ... } }

Library defines ONE id: define_entity_id!(EventId, "event");
Products: define_entity_id!(PlayerId, "player");

All IDs use u128 internally. No Uuid type in public API.
uuid crate used only inside the macro's now_v7() implementation.
```

---

## CONTROL FLOW

### Write Path

```
User ── store.append(coord, kind, payload) ──> Store
  Store builds Event (header, payload, no hash yet)
  Store sends WriterCommand::Append via flume::send() [blocks if full]
  ──> Writer thread:
      1. entity lock
      2. prev_hash from index
      3. sequence = latest+1
      4. set position
      5. blake3 hash + hash chain
      6. msgpack + crc32 frame
      7. rotation check
      8. write frame to segment
      9. update index
      10. broadcast to subscribers (Vec<flume::Sender>)
  <── flume::recv() response
User gets AppendReceipt { event_id, sequence, disk_pos }
```

### Pipeline Flow

```
Product assembles (Ctx, Proposal<T>) via I/O
  ──> pipeline.evaluate(ctx, proposal)
      GateSet runs each Gate<Ctx>::evaluate(&ctx)
      All pass → Receipt<T> (sealed, wraps payload)
      Any deny → Err(Denial)
  ──> pipeline.commit(receipt, commit_fn)
      receipt.into_parts() → (payload, gate_names)
      commit_fn(payload) → store.append(...)
  ──> Committed<T> { payload, event_id, sequence, hash }
```

### Projection Flow

```
store.project::<Player>("player:1", Consistent)
  ──> index.latest("player:1") → watermark
  ──> cache.get(key) → HIT? return cached. MISS? continue:
  ──> stream all events for "player:1"
  ──> reader.read_entry(disk_pos) for each → Event<Value>
  ──> Player::from_events(&events) → Option<Player>
  ──> cache.put(key, serialized, meta)
  ──> return Player
```

### Subscription vs Cursor

```
PUSH (Subscription — lossy, real-time):
  Writer ──broadcast──> per-subscriber flume channel ──Region filter──> Consumer
  Dropped subscribers pruned on next broadcast (send returns Err)

PULL (Cursor — guaranteed, batch):
  Consumer ──poll()──> Cursor reads index from global_sequence position
  Never loses events. Catches up on next poll.
```

---

## DEVOPS

### rust-toolchain.toml
```toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy", "rust-src"]
```

### clippy.toml
```toml
msrv = "1.75"
cognitive-complexity-threshold = 25
too-many-arguments-threshold = 7
```

### .config/nextest.toml
```toml
[profile.default]
retries = 0
slow-timeout = { period = "30s", terminate-after = 2 }
fail-fast = true

[profile.ci]
retries = 2
fail-fast = false
test-threads = "num-cpus"

[profile.default.junit]
path = "target/nextest/default/junit.xml"
```

### .cargo/config.toml
```toml
# NOTE: .cargo/config.toml is for build settings, NOT profile settings.
# Profile settings ([profile.*]) belong in Cargo.toml.
# This file is intentionally minimal.
[build]
# rustflags set in CI via env var, not here
```

### Profile settings (in Cargo.toml, NOT .cargo/config.toml)
```toml
# Append these to Cargo.toml after [[bench]] sections:
[profile.dev]
opt-level = 0
debug = true
incremental = true

[profile.dev.package."*"]
opt-level = 2

[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 16
strip = "symbols"

[profile.test]
opt-level = 1

[profile.bench]
inherits = "release"
debug = true
```

### .github/workflows/ci.yml
```yaml
name: CI
on: [push, pull_request]
env:
  CARGO_TERM_COLOR: always
  RUSTFLAGS: "-D warnings"
  PROPTEST_CASES: 1000

jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - run: cargo check --all-features
      - run: cargo check --no-default-features
      - run: cargo check --features blake3
      - run: cargo check --features redb

  test:
    runs-on: ubuntu-latest
    needs: check
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: taiki-e/install-action@nextest
      - uses: Swatinem/rust-cache@v2
      - run: cargo nextest run --profile ci --all-features
      - run: cargo test --doc --all-features

  clippy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { components: clippy }
      - uses: Swatinem/rust-cache@v2
      - run: cargo clippy --all-features -- -D warnings

  fmt:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { components: rustfmt }
      - run: cargo fmt --check

  msrv:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@1.75.0
      - run: cargo check --all-features

  semver:
    runs-on: ubuntu-latest
    if: github.event_name == 'pull_request'
    steps:
      - uses: actions/checkout@v4
      - uses: obi1kenobi/cargo-semver-checks-action@v2

  bench-compile:
    runs-on: ubuntu-latest
    needs: check
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - run: cargo bench --no-run --all-features
```

### justfile
```makefile
default:
    just --list

check:
    cargo check --all-features

test:
    cargo nextest run --all-features
    cargo test --doc --all-features

clip:
    cargo clippy --all-features -- -D warnings

fmt:
    cargo fmt

ci: fmt clip test
    cargo bench --no-run --all-features
    cargo check --no-default-features

bench:
    cargo bench --all-features

doc:
    cargo doc --all-features --no-deps --open

deps-doc:
    cargo doc --document-private-items --no-deps --open

lib-doc:
    cargo doc --all-features --open
```

### Version Policy
```
MSRV: 1.75. Edition: 2021. Semver: 0.x (breaking changes expected).
Use LATEST version of every dep UNLESS it creates duplicates.
Check: cargo tree --duplicates. If duplicates: pin to older.
```

---

## BUILD ORDER

Exact files per step. Dependencies flow DOWN — each step may use types from
steps above it. An implementing agent builds top to bottom, checking after
each numbered step.

```
1.  cargo init batpak --lib
2.  Create all config files (.cargo, clippy.toml, rust-toolchain.toml,
    nextest.toml, ci.yml, justfile, .gitignore, licenses, changelog, build.rs)
3.  Write Cargo.toml

STEP 4 — Foundation types
  FILES:
    src/wire.rs                    ← FIRST. Zero deps. Everything else uses it.
    src/event/kind.rs              ← EventKind (u16 newtype, no deps)
    src/event/mod.rs               ← STUB: just `pub mod kind;` for now
    src/coordinate/mod.rs          ← Coordinate, CoordinateError, Region, KindFilter
    src/coordinate/position.rs     ← DagPosition
    src/outcome/error.rs           ← OutcomeError, ErrorKind
    src/outcome/wait.rs            ← WaitCondition, CompensationAction (uses wire::)
    src/outcome/combine.rs         ← zip, join_all, join_any
    src/outcome/mod.rs             ← Outcome<T> + combinators (uses wait.rs, error.rs)
    src/guard/denial.rs            ← Denial
    src/guard/receipt.rs           ← Receipt<T> (sealed)
    src/guard/mod.rs               ← Gate<Ctx>, GateSet<Ctx>
    src/typestate/mod.rs           ← define_state_machine!, define_typestate!
    src/typestate/transition.rs    ← Transition<From,To,P> (uses EventKind)
    src/id/mod.rs                  ← EntityIdType trait + define_entity_id! macro
    src/lib.rs                     ← module declarations (wire, coordinate, outcome,
                                     event stub, guard, typestate, id)
  DEP NOTE: KindFilter in coordinate/mod.rs contains EventKind.
    Write event/kind.rs and event/mod.rs (stub with `pub mod kind;`) BEFORE
    coordinate/mod.rs so the import resolves. File order within this step matters.
  → cargo check --no-default-features MUST PASS

STEP 5 — Event types (EventHeader, Event<P>, StoredEvent, EventSourced, HashChain)
  FILES:
    src/event/hash.rs              ← HashChain struct (always), compute_hash/verify_chain (blake3 feature)
    src/event/header.rs            ← EventHeader (uses wire::, DagPosition, EventKind)
    src/event/mod.rs               ← COMPLETE: Event<P>, StoredEvent<P> (uses header, hash, kind)
    src/event/sourcing.rs          ← EventSourced<P>, Reactive<P> (uses Event<P>, EventKind, Coordinate)
    src/id/mod.rs                  ← ADD define_entity_id!(EventId, "event") (uses uuid)
  → cargo check --features blake3 MUST PASS

STEP 6 — Pipeline
  FILES:
    src/pipeline/mod.rs            ← Pipeline<Ctx>, Proposal<T>, Committed<T> (uses guard, wire::)
    src/pipeline/bypass.rs         ← BypassReason, BypassReceipt<T>
  → cargo check --features blake3 MUST PASS

STEP 7 — Store (all files, biggest step)
  FILES:
    src/store/index.rs             ← StoreIndex, IndexEntry, ClockKey, DiskPos
    src/store/segment.rs           ← SegmentHeader, frame_encode/decode, FramePayload
    src/store/reader.rs            ← Reader (LRU FD cache, pread, CRC32)
    src/store/writer.rs            ← WriterHandle, WriterCommand, SubscriberList, Notification
    src/store/projection.rs        ← ProjectionCache trait, NoCache, RedbCache, LmdbCache
    src/store/cursor.rs            ← Cursor
    src/store/subscription.rs      ← Subscription
    src/store/mod.rs               ← Store, StoreConfig, StoreError, AppendReceipt, AppendOptions
    src/prelude.rs                 ← re-exports from all modules
    src/lib.rs                     ← ADD remaining module declarations (pipeline, store, prelude)
  → cargo check --all-features MUST PASS

STEP 8 — Tests
  FILES: tests/monad_laws.rs, hash_chain.rs, store_integration.rs,
    gate_pipeline.rs, typestate_safety.rs, wire_format.rs, self_benchmark.rs
  → cargo nextest run MUST PASS

STEP 9 — Benches
  FILES: benches/write_throughput.rs, cold_start.rs, projection_latency.rs
  → cargo bench --no-run MUST COMPILE

STEP 10 — Hello world, verify it runs (fn main, no tokio)

STEP 11 — Final checks
  → cargo clippy --all-features -- -D warnings → ZERO WARNINGS
  → cargo fmt --check → PASSES
  → cargo doc --all-features --no-deps → CLEAN
```

---

## HELLO WORLD (pure sync, no tokio)

```rust
use batpak::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = Store::open_default()?;
    let coord = Coordinate::new("player:alice", "room:dungeon")?;
    let kind = EventKind::custom(0xF, 1);

    let receipt = store.append(&coord, kind, &serde_json::json!({"x": 10, "y": 20}))?;
    println!("Stored event {} at position {}", receipt.event_id, receipt.sequence);

    for entry in store.stream("player:alice") {
        let stored = store.get(entry.event_id)?;
        println!("{}: {:?}", stored.event.event_kind(), stored.event.payload);
    }
    Ok(())
}
```

---

## DESIGN BOUNDARIES

No v1/v2. No roadmap. The library does X. If pain demands Y, you add Y.
There's no version boundary — just responding to reality.

```
DECIDED (the design — not compromises, not simplifications, the right call):

  Outcome<T> with 1 type param
  Gates are predicates (enrichment goes in assembly, before gates)
  Coordinate is a struct (Arc<str> entity + scope)
  Store is a concrete struct (no storage trait)
  Per-entity linear chains (not a real DAG — DAG vocabulary is scaffolding)
  Lane = 0 always (fan-out is future scaffolding)
  NoCache is the default projection backend
  macro_rules! for typestate (not proc macro)
  Denial is separate from OutcomeError (library doesn't auto-store denials)
  Writer restart via RestartPolicy (Once default, Bounded for production)
  Writer drains queued commands on shutdown (bounded, no silent data loss)
  blake3 only (no enum, no trait, one function)
  flume for all channels (no tokio::broadcast)
  Store API is sync (bisync is a channel property)

SHIPPED (enforcement via tests, not types — equally valid, better ergonomics):

  Functor laws — tested via proptest (left/right identity, associativity,
    Batch distribution). The algebra IS the implementation. The 4-param
    Outcome<C,K,P,S> would encode these at compile time. The test suite
    enforces them at test time. Same guarantees, less type noise.

  Coordinate querying — Region ships as the coordinate-space query language.
    Region::entity().scope().fact_category() IS the coordinate algebra.
    A Coordinate trait would let users define custom dimensions. That arrives
    when someone has a coordinate space that isn't two strings.

  Compensation model — CompensationAction ships as data on OutcomeError.
    The writer persists it as part of error events. Products implement the
    handler. Library provides the data model, product provides the execution.

  Checkpoint foundation — global_sequence on every IndexEntry. SYSTEM_CHECKPOINT
    reserved. Checkpoint payload = serialized StoreIndex. Cold start scans
    segments. The ONLY thing that arrives later is ~50 LOC of emission +
    accelerated cold start. Foundation is complete.

ARRIVES WITH CONCRETE PAIN (not deferred — literally doesn't exist because
nobody has hit the wall that demands it):

  EventDag storage trait — when a second backend exists, extract the trait
    from the concrete Store. Until then, Invariant 4.
  derive(EventSourced) proc macro — when 10+ entities make hand-written
    impls painful. EventSourced<P> being generic is the prerequisite.
  Index memory eviction — when a store exceeds 10M events and 2-3GB RAM
    matters. global_sequence + checkpoints enable partial loading.
  Entity lock pruning — when unique entity count exceeds 100K.
    DashMap entry() API makes pruning safe when the time comes.

```

---

## ESTIMATED LOC

```
coordinate/    ~160  (Coordinate + Region + CoordinateError + KindFilter + DagPosition)
outcome/       ~450  (Outcome<T> + OutcomeError + combinators + wait conditions)
event/         ~400  (Event<P> + StoredEvent + EventHeader + EventKind + hash fns + EventSourced + Reactive)
guard/         ~250  (Gate + GateSet + Denial + Receipt)
pipeline/      ~150  (Pipeline + Proposal + Committed + Bypass)
store/        ~1800  (Store + segment + writer + reader + index + projection + cursor + subscription)
typestate/     ~160  (macros + Transition<From,To,P>)
id/             ~80  (EntityIdType + define_entity_id! + EventId)
wire            ~50  (u128_bytes, option_u128_bytes, vec_u128_bytes serde helpers)
build.rs        ~80  (pre-flight invariant enforcement)
lib+prelude     ~60

CODE:  ~3,640
TESTS: ~1,600  (diagnostic messages + trybuild ui/ + golden files add ~100)
TOTAL: ~5,240
```

---

## VERIFICATION TRACE (reviewed 2026-03-20)

All drift corrections verified against this document:

```
[✓] tokio::broadcast → Vec<flume::Sender>
    writer.rs has SubscriberList with Mutex<Vec<flume::Sender<Notification>>>.
    subscription.rs says "NO tokio::broadcast."
    Cargo.toml has "# NO TOKIO. Invariant 1." tokio only in dev-deps.

[✓] Store API sync
    Every method returns Result, not impl Future.
    store/mod.rs says "ALL METHODS ARE SYNC (Invariant 2)."
    Async callers use spawn_blocking or flume recv_async.

[✓] Watcher → Reactive<P> trait
    ~15 LOC in sourcing.rs next to EventSourced<P>.
    No watcher module. No watcher file.

[✓] Region builder — method chaining
    Region::entity().scope().fact_category()

[✓] blake3 only
    hash.rs: "NO TRAIT. NO ENUM." Two functions + one struct. ~30 LOC.

[✓] KindFilter in coordinate/mod.rs
    Region component, Region lives in coordinate. Internally consistent.

[✓] OutcomeError naming
    Consistent throughout. Prelude lists OutcomeError. No stale EffectError.

[✓] StoredEvent<P>
    store.get() returns StoredEvent<serde_json::Value>.
    Round-trip erasure documented.

[✓] CoordinateError
    Coordinate doesn't depend on StoreError.
    From<CoordinateError> lives in store layer.

[✓] Committed<T> hash field
    [u8; 32], always present. [0u8; 32] when blake3 off (genesis convention).

[✓] append signature
    payload: &impl Serialize. Generic.

[✓] Async pattern clarification
    subscription.rs distinguishes recv_async (for subscriptions) from
    spawn_blocking (for Store read/write methods).

[✓] RestartPolicy on StoreConfig
    writer.rs: RestartPolicy enum (Once, Bounded). StoreConfig has restart_policy field.
    Default: Once. Production: Bounded { max_restarts, within_ms }.
    Passes Invariant 4: two real impls, not speculative.

[✓] Shutdown drain semantics
    writer.rs: Shutdown drains up to shutdown_drain_limit queued commands,
    then fsync, then responds. Beyond cap → StoreError::ShuttingDown.
    No silent data loss on store.close() after burst of appends.

[✓] causation_id on EventHeader
    header.rs: causation_id: Option<u128> — which event CAUSED this one.
    Distinct from correlation_id (related vs caused-by).
    None = root cause (user action, external trigger).
    store.append() defaults to None. store.append_reaction() sets both.

[✓] IndexEntry causal fields + methods
    index.rs: correlation_id: u128 + causation_id: Option<u128> on IndexEntry.
    ~32 bytes added per entry for O(1) causal queries without disk reads.
    Three methods:
      is_correlated() → event_id != correlation_id
      is_caused_by(id) → causation_id == Some(id)
      is_root_cause() → causation_id.is_none()
    Products: query(region).filter(|e| e.is_caused_by(parent_id))
    Validated by FerrOx convergence — independent project needed same field.

[✓] correlation vs causation documented on EventHeader
    header.rs: explicit doc distinguishing the two concepts.
    correlation = "show me everything related to this workflow"
    causation chain = "trace the exact causal path to this event"

[✓] Production supervisor guidance
    store/mod.rs: "In production, run under a process supervisor."
    Library handles transient writer panics. Process crashes require external restart.
```

MCQ VALIDATION (10 questions, all answers verified against spec):
  Q1=B (sync API), Q2=C (Ctx is product-defined), Q3=B (seal is private),
  Q4=B (restart per policy), Q5=B (and_then distributes over Batch),
  Q6=B (push lossy / pull guaranteed), Q7=B (EventSourced in Layer 0),
  Q8=B (private u16 prevents reserved-range construction),
  Q9=C (no cross-entity atomicity, saga via compensation),
  Q10=B (writing IS executing — store is the runtime).

CASCADE ANALYSIS (4 cascading pairs, 3 independent, 0 contradictions):
  Q1+Q6: sync API + push/pull → NEVER async methods, products compose at boundary
  Q3+Q9: sealed receipt + no cross-entity atomicity → one coord per pipeline pass
  Q5+Q7: monad distributes + generic P → compose chains without serde
  Q8+Q2: private EventKind + product Ctx → system events invisible to product gates
  Independent: Q1/Q8, Q3/Q7, Q6/Q9

This document is FINAL. No override layers. No patches. No "THIS SECTION WINS."
An implementing agent reads top to bottom and builds exactly what's described.

---

## IMPLEMENTATION NOTES

These notes resolve ambiguities an implementing agent would otherwise have to
guess at. They do not change the architecture — they specify the HOW for
decisions the PRDs specify the WHAT.

```
1. ClockKey Ord implementation:
   impl Ord for ClockKey: compare wall_ms first (HLC global ordering),
   then clock, then uuid for deterministic tiebreaking. This is the sort
   order for BTreeMap entries in streams and by_fact indexes.

2. Segment file naming:
   {segment_id:06}.fbat (e.g., 000001.fbat). segment_id is a sequential u64.
   Cold start scans data_dir alphabetically — zero-padded means lexicographic
   sort matches creation order. No gaps assumed (compaction may remove segments).

3. walk_ancestors semantics:
   Follows the per-entity hash chain (prev_hash links), NOT the causation chain.
   Each entity's events form a linear hash chain. walk_ancestors starts at an
   event_id, looks up its HashChain.prev_hash, finds the IndexEntry whose
   event_hash matches, repeats. N index lookups for depth N. No disk reads
   needed — the index has hash_chain on every IndexEntry.
   Causation chain traversal (following causation_id across entities) is a
   product concern — products use query(region).filter(|e| e.is_caused_by(id)).

4. Writer panic → caller experience:
   When the writer thread panics, flume's receiver is dropped. The caller
   blocked on flume::recv() gets RecvError (channel disconnected). Store's
   append() method catches this and converts to StoreError::WriterCrashed
   before attempting restart per RestartPolicy. The caller NEVER sees a raw
   flume error. The in-flight append is lost — the caller must retry.

5. DashMap guard lifetimes in the writer:
   Extract and clone values from DashMap BEFORE acquiring the entity Mutex.
   Do NOT hold a DashMap Ref across the 10-step commit. Pattern:
     let lock = index.entity_locks.entry(entity.clone())
         .or_insert_with(|| Arc::new(Mutex::new(()))).clone();
     // DashMap entry guard dropped here (clone gives us the Arc)
     let _guard = lock.lock();
     // ... 10-step commit with _guard held ...
   Similarly for step 2 (prev_hash from latest): clone the IndexEntry out
   of the DashMap Ref immediately, drop the Ref, then use the cloned value.

6. Store is Send + Sync:
   - Reader: LRU FD cache behind parking_lot::Mutex → Send + Sync
   - Index: DashMap → Send + Sync
   - Writer: flume::Sender → Send + Sync
   - Config: immutable after open() → Send + Sync
   The compiler enforces this. If a field isn't Sync, Store won't compile
   as Sync. No manual unsafe impl needed.

7. MSRV 1.75 workarounds:
   - File::create_new() requires 1.77. Use OpenOptions::new().write(true)
     .create_new(true).open(path) instead.
   - LazyLock requires 1.80. Use OnceLock with get_or_init() instead.
   - OnceLock::get_or_try_init() requires 1.82. Pre-validate before
     get_or_init(), or use Once manually.
   - Unix pread: use std::os::unix::fs::FileExt::read_at() (stable since 1.15).
     For cross-platform: #[cfg(unix)] read_at, #[cfg(not(unix))] seek+read fallback.

8. serde is non-optional:
   serde + serde_json are required dependencies (not behind a feature flag).
   The store's primary API (append with &impl Serialize, get returning
   serde_json::Value, project) fundamentally requires serialization.
   Layer 0 types derive(Serialize, Deserialize) unconditionally.
   blake3, redb, lmdb remain optional features.
   --no-default-features builds everything except blake3 hashing (which
   falls back to [0u8; 32] genesis convention).

9. rmp-serde named mode:
   ALL MessagePack serialization uses rmp_serde::to_vec_named().
   NEVER use rmp_serde::to_vec() — it serializes structs as positional
   arrays which break silently when fields are added or reordered.
   This applies to frame_encode, cache serialization, and any other
   msgpack path. Deserialization uses rmp_serde::from_slice() (handles both
   named and positional input, but we always write named).

10. Broadcast is lossy by design:
    Subscription says "lossy: if subscriber is slow, bounded channel fills."
    The try_send() pattern in the writer preserves this: Full channels keep
    the subscriber (they catch up on next send), Disconnected channels prune.
    For guaranteed delivery, products use Cursor (pull-based, index-backed).
```
