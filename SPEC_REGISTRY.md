# SPEC_REGISTRY — Portable Context for Parallel Agent Execution

Status note (2026-03-30): this registry now sits beside the live code and the
machine-readable traceability registry. The store implementation has been split
across focused modules, the canonical Linux environment is the checked-in
devcontainer, CI runs Linux in-container plus Windows native, and mutation
testing now has smoke and scheduled full-shard lanes. Where older file-local
sections still describe the pre-split store monolith, treat them as API intent
only and prefer the current file graph in `batpak/src/store/`.

```
WHAT THIS IS:
  Self-contained file-level build contexts. Each H2 section is ONE file.
  An agent loads this section + the DEPENDENCY SURFACE below, writes the file, done.
  No compiler access. No discovery. No guessing. Execute from context.

HOW TO USE:
  1. Read the DEPENDENCY SURFACE section (always load this — it's the shared type atlas)
  2. Grep for the file you're assigned: ## src/wire.rs
  3. Read from that H2 to the next H2 (or EOF)
  4. Everything you need is in that section:
     - CONSTRAINTS: what NOT to do (read these FIRST, before any code)
     - IMPORTS: exact use statements (copy verbatim, do NOT substitute from memory)
     - SHAPES: type signatures for every cross-file type this file touches
     - TYPES: exact struct/enum/trait definitions for this file
     - IMPL: pseudo-code with ///why comments for every method
     - TESTS: what the test suite asserts about this file
  5. Write the file. Do not run cargo. Do not compile. Do not test.
  6. The /// comments are prompts. They explain WHY. Code blocks explain WHAT.

FORMAT:
  ## path/to/file.rs            ← H2 = file path (grepable)
  >[filename.rs]                ← end marker (grep from ## to >[] = full context)

CROSS-REFERENCES:
  [SPEC:section_name]          ← refers to SPEC.md ## section_name
  [DEP:crate::path::fn]        ← refers to DEPENDENCY SURFACE section below (NOT the web)
  [FILE:path/to/other.rs]      ← refers to another file section in this registry

ANTI-DRIFT PRINCIPLES (read these if you feel yourself improvising):
  - If a type isn't in SHAPES or DEPENDENCY SURFACE, you don't know its shape. Stop.
  - If an API isn't in DEPENDENCY SURFACE, you don't know if it exists. Stop.
  - If you're about to write a struct/enum that isn't in TYPES, you're inventing. Stop.
  - If your code compiles in your head but contradicts a CONSTRAINT, the constraint wins.
  - "Seems right" is not "is right." Check SHAPES. Check DEPENDENCY SURFACE.
```

---

## DEPENDENCY SURFACE

Exact API signatures for every external crate function/type we call.
This section is GROUND TRUTH. If it contradicts your training data, this wins.
Verified against cargo doc output for the pinned versions in Cargo.toml.

```
=== flume 0.12 ===

flume::bounded<T>(cap: usize) -> (Sender<T>, Receiver<T>)
flume::unbounded<T>() -> (Sender<T>, Receiver<T>)

Sender<T>: Clone + Send + Sync (where T: Send)
  fn send(&self, msg: T) -> Result<(), SendError<T>>       // blocks if bounded+full
  fn try_send(&self, msg: T) -> Result<(), TrySendError<T>> // never blocks
  fn is_disconnected(&self) -> bool

  TrySendError<T>:
    Full(T)           — channel at capacity, message returned
    Disconnected(T)   — all receivers dropped, message returned

  SendError<T>(pub T) — all receivers dropped

Receiver<T>: Clone + Send + Sync (where T: Send)
  fn recv(&self) -> Result<T, RecvError>                    // blocks until message
  fn recv_async(&self) -> RecvFut<'_, T>                 // async, runtime-agnostic
  fn try_recv(&self) -> Result<T, TryRecvError>
  fn iter(&self) -> Iter<'_, T>                             // blocking iterator

  RecvError — unit struct, all senders dropped + channel empty
  TryRecvError: Empty | Disconnected

  RecvFut<'a, T>: Future<Output = Result<T, RecvError>>
    // works with any async runtime (tokio, async-std, smol)
    // no tokio dependency — uses std::task::Waker

=== rmp_serde 1 ===

rmp_serde::to_vec_named<T: Serialize>(val: &T) -> Result<Vec<u8>, rmp_serde::encode::Error>
  // ALWAYS use to_vec_named, NEVER to_vec. [SPEC:WIRE FORMAT DECISIONS]
  // Named mode preserves field names as msgpack map keys.
  // to_vec serializes as positional arrays — breaks on field reorder.

rmp_serde::from_slice<T: Deserialize>(buf: &[u8]) -> Result<T, rmp_serde::decode::Error>
  // Handles both named and positional input.

=== crc32fast 1 ===

crc32fast::hash(data: &[u8]) -> u32                // one-shot CRC32
crc32fast::Hasher::new() -> Hasher                  // incremental
  fn update(&mut self, data: &[u8])
  fn finalize(self) -> u32

  // Uses CRC-32/ISO-HDLC (standard zlib/gzip/PNG CRC32)
  // Hardware-accelerated on x86_64 (SSE4.2 PCLMULQDQ)

=== blake3 1 (behind feature = "blake3") ===

blake3::hash(input: &[u8]) -> blake3::Hash
  Hash::into() -> [u8; 32]                          // via From<Hash> for [u8; 32]
  // Usage: let bytes: [u8; 32] = blake3::hash(data).into();

=== uuid 1 (features = ["v7"]) ===

uuid::Uuid::now_v7() -> Uuid                        // timestamp + random
  fn as_u128(&self) -> u128                          // big-endian u128
  // Used ONLY inside define_entity_id! macro. Never in public API.

=== parking_lot 0.12 ===

parking_lot::Mutex<T>: Send + Sync (where T: Send)
  fn lock(&self) -> MutexGuard<'_, T>                // NO Result, NO poisoning
  fn try_lock(&self) -> Option<MutexGuard<'_, T>>
  // 1 byte overhead (vs ~40 bytes for std::sync::Mutex)

=== dashmap 5 ===

DashMap<K, V>: Send + Sync (where K: Eq + Hash + Send + Sync, V: Send + Sync)
  fn insert(&self, key: K, value: V) -> Option<V>
  fn get(&self, key: &Q) -> Option<Ref<'_, K, V>>   // where K: Borrow<Q>
  fn get_mut(&self, key: &Q) -> Option<RefMut<'_, K, V>>
  fn entry(&self, key: K) -> Entry<'_, K, V>         // holds WRITE lock on shard
  fn contains_key(&self, key: &Q) -> bool
  fn remove(&self, key: &Q) -> Option<(K, V)>
  fn iter(&self) -> Iter<'_, K, V>                   // NOT a consistent snapshot
  fn len(&self) -> usize

  Ref<'a, K, V>: Deref<Target = V>
    fn key(&self) -> &K
    fn value(&self) -> &V
    // DROPS the shard read-lock when Ref is dropped.
    // DEADLOCK RISK: do not hold Ref while calling insert/remove/entry on same map.

  Entry<'a, K, V>:
    fn or_insert(self, default: V) -> RefMut<'_, K, V>
    fn or_insert_with(self, f: impl FnOnce() -> V) -> RefMut<'_, K, V>
    // Holds WRITE lock for its entire lifetime. Release fast.

  Arc<str> as key: works. Eq + Hash delegate to inner str.
  Lookup with &str: works via K: Borrow<Q> where Arc<str>: Borrow<str>.

=== tracing 0.1 ===

tracing::trace!(field = value, "message")            // most verbose
tracing::debug!(field = value, "message")
tracing::info!(field = value, "message")
tracing::warn!(field = value, "message")
tracing::error!(field = value, "message")            // least verbose
  // %value = Display, ?value = Debug, value = native
  // No-op when no subscriber installed. Near-zero overhead.

=== serde 1 ===

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]             // adjacent tagging for enums
#[serde(with = "module_path")]                       // custom ser/de
#[serde(skip_serializing_if = "Option::is_none")]    // omit None fields
#[serde(default)]                                    // fill missing fields with Default

  Serializer trait — we use: serialize_bytes, serialize_none, serialize_seq
  Deserializer trait — we use: deserialize_bytes, deserialize_option, deserialize_seq
  Visitor trait — we implement: visit_bytes, visit_seq, visit_none, visit_some

=== std (Rust 1.75 MSRV) ===

Arc<str>: Clone + Send + Sync + Eq + Hash + Deref<Target = str>
  Arc::from("string_literal") -> Arc<str>
  // Borrow<str> implemented — can use &str for HashMap/DashMap lookups

AtomicU64: Send + Sync
  fn fetch_add(&self, val: u64, order: Ordering) -> u64
  fn load(&self, order: Ordering) -> u64
  fn store(&self, val: u64, order: Ordering)

std::os::unix::fs::FileExt (stable since 1.15):
  fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize>  // pread
  fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<usize>     // pwrite
  // Do NOT use File::create_new() — requires 1.77. Use OpenOptions instead.

std::sync::OnceLock<T> (stable since 1.70):
  fn get_or_init(&self, f: impl FnOnce() -> T) -> &T
  // Do NOT use LazyLock — requires 1.80.
  // Do NOT use get_or_try_init — requires 1.82.
```

---

## INTERNAL TYPE ATLAS

Shapes of every cross-file type. When your file imports a type from another
module, its shape is here. If you need a field or method, check this section.
Do NOT invent fields. Do NOT guess method signatures.

```
EventKind(u16)                     — PRIVATE inner. No .0 access from outside event/.
  ::custom(category: u8, type_id: u16) -> Self
  .category() -> u8
  .type_id() -> u16
  .is_system() -> bool
  .is_effect() -> bool
  Constants: DATA, SYSTEM_INIT, SYSTEM_SHUTDOWN, SYSTEM_HEARTBEAT,
    SYSTEM_CONFIG_CHANGE, SYSTEM_CHECKPOINT, EFFECT_ERROR, EFFECT_RETRY,
    EFFECT_ACK, EFFECT_BACKPRESSURE, EFFECT_CANCEL, EFFECT_CONFLICT

Coordinate { entity: Arc<str>, scope: Arc<str> }   — fields PRIVATE
  ::new(entity: impl AsRef<str>, scope: impl AsRef<str>) -> Result<Self, CoordinateError>
  .entity() -> &str
  .scope() -> &str
  pub(crate) .entity_arc() -> Arc<str>
  pub(crate) .scope_arc() -> Arc<str>
  Display: "entity@scope"

CoordinateError: EmptyEntity | EmptyScope          — impl Display, Error

DagPosition { pub wall_ms: u64, pub counter: u16, pub depth: u32, pub lane: u32, pub sequence: u32 }  — repr(C)
  ::new(depth, lane, sequence) -> Self              // wall_ms=0, counter=0
  ::with_hlc(wall_ms, counter, depth, lane, sequence) -> Self
  ::root() -> Self                    // all zeros
  ::child(sequence: u32) -> Self      // depth=0, lane=0, wall_ms=0
  ::child_at(sequence, wall_ms, counter) -> Self    // v1 with HLC
  ::fork(parent_depth, new_lane) -> Self
  .is_root() -> bool
  .is_ancestor_of(&DagPosition) -> bool
  Display: "depth:lane:sequence@wall_ms.counter"

Region { entity_prefix, scope, fact, clock_range } — all Option, all pub
  ::all() -> Self (Default)
  ::entity(prefix) -> Self
  ::scope(scope) -> Self
  ::coordinate(&Coordinate) -> Self
  .with_scope(s) -> Self
  .with_fact(KindFilter) -> Self
  .with_fact_category(u8) -> Self
  .with_clock_range((u32, u32)) -> Self
  .matches_event(entity: &str, scope: &str, kind: EventKind) -> bool

KindFilter: Exact(EventKind) | Category(u8) | Any

HashChain { pub prev_hash: [u8; 32], pub event_hash: [u8; 32] }
  Default: all zeros (genesis convention)

EventHeader { event_id, correlation_id, causation_id, timestamp_us: i64,
              position, payload_size, event_kind, flags: u8, content_hash: [u8; 32] }
  All fields pub. Serde annotations on u128 fields. content_hash has #[serde(default)].
  ::new(event_id, correlation_id, causation_id, timestamp_us, position, payload_size, event_kind) -> Self
    // flags defaults to 0, content_hash defaults to [0u8; 32]
  .with_flags(u8) -> Self
  .requires_ack() -> bool    // flag bit 0
  .is_transactional() -> bool // flag bit 1
  .is_replay() -> bool       // flag bit 3
  .age_us(now_us: i64) -> u64

Event<P> { pub header: EventHeader, pub payload: P, pub hash_chain: Option<HashChain> }
  ::new(header, payload) -> Self
  .with_hash_chain(HashChain) -> Self
  .event_id() -> u128
  .event_kind() -> EventKind
  .position() -> &DagPosition
  .is_genesis() -> bool
  .map_payload(f) -> Event<U>

StoredEvent<P> { pub coordinate: Coordinate, pub event: Event<P> }

Outcome<T>: Ok(T) | Err(OutcomeError) | Retry{..} | Pending{..} | Cancelled{..} | Batch(Vec<Outcome<T>>)
  .map(f) .and_then(f) .map_err(f) .or_else(f) .flatten()
  .is_ok() .is_err() .is_retry() .is_pending() .is_cancelled() .is_batch() .is_terminal()
  .into_result() -> Result<T, OutcomeError>

OutcomeError { pub kind: ErrorKind, pub message: String, pub compensation: Option<CompensationAction>, pub retryable: bool }
ErrorKind: NotFound | Conflict | Validation | PolicyRejection | StorageError | Timeout | Serialization | Internal | Custom(u16)

Gate<Ctx>: trait, Send + Sync
  .name() -> &'static str
  .evaluate(&Ctx) -> Result<(), Denial>

GateSet<Ctx> { gates: Vec<Box<dyn Gate<Ctx>>> }
  .push(gate)
  .evaluate(ctx, proposal) -> Result<Receipt<T>, Denial>  // fail-fast
  .evaluate_all(ctx) -> Vec<Denial>                        // no fail-fast

Denial { pub gate: &'static str, pub code: String, pub message: String, pub context: Vec<(String, String)> }
  Serialize only — NOT Deserialize (&'static str can't deser from owned data)
  ::new(gate, message) .with_code(code) .with_context(key, value)

Receipt<T> { _seal: seal::Token, gates_passed: Vec<&'static str>, payload: T }
  NOT Clone. NOT Copy. NOT Serialize. Consumed once.
  .payload() -> &T
  .gates_passed() -> &[&'static str]
  .into_parts() -> (T, Vec<&'static str>)           // consuming
  Only constructible via GateSet::evaluate(). seal::Token is pub(crate).

Proposal<T>(pub T)
  ::new(payload) .payload() .map(f)

Committed<T> { pub payload: T, pub event_id: u128, pub sequence: u64, pub hash: [u8; 32] }

Pipeline<Ctx> { gates: GateSet<Ctx> }
  ::new(gates)
  .evaluate(ctx, proposal) -> Result<Receipt<T>, Denial>
  .commit<E>(receipt, f: impl FnOnce(T) -> Result<Committed<T>, E>) -> Result<Committed<T>, E>

Transition<From, To, P> { kind: EventKind, payload: P, _from: PhantomData, _to: PhantomData }
  ::new(kind, payload) .kind() .payload() .into_payload()

EntityIdType: trait (Copy + Clone + Eq + Hash + Debug + Display + FromStr + Send + Sync)
  ::new(u128) .as_u128() .now_v7() .nil()

=== STORE TYPES ===

Store { index: Arc<StoreIndex>, reader: Arc<Reader>, cache: Box<dyn ProjectionCache>,
        writer: WriterHandle, config: Arc<StoreConfig> }
  Store: Send + Sync (all fields are Send + Sync)
  ::open(config: StoreConfig) -> Result<Self, StoreError>
  ::open_default() -> Result<Self, StoreError>     // ./batpak-data/
  .append(&self, coord, kind, payload: &impl Serialize) -> Result<AppendReceipt, StoreError>
  .append_reaction(&self, coord, kind, payload, correlation_id: u128, causation_id: u128) -> Result<AppendReceipt, StoreError>
  .get(event_id: u128) -> Result<StoredEvent<serde_json::Value>, StoreError>
  .query(region: &Region) -> Vec<IndexEntry>
  .walk_ancestors(event_id: u128, limit: usize) -> Vec<StoredEvent<Value>>
  .project<T: EventSourced<Value>>(entity: &str, freshness: Freshness) -> Result<Option<T>, StoreError>
  .subscribe(region: &Region) -> Subscription
  .cursor(region: &Region) -> Cursor
  .stream(entity) .by_scope(scope) .by_fact(kind)  // convenience sugar
  .sync() .close(self) .stats() -> StoreStats

StoreConfig { data_dir: PathBuf, segment_max_bytes: u64, sync_every_n_events: u32,
              fd_budget: usize, writer_channel_capacity: usize, broadcast_capacity: usize,
              cache_map_size_bytes: usize, restart_policy: RestartPolicy, shutdown_drain_limit: usize,
              writer_stack_size: Option<usize>, clock: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
              sync_mode: SyncMode }
  All pub. No Default — use StoreConfig::new(data_dir) with sane defaults, then override fields.
  Manual Clone and Debug impls because `clock` is `Arc<dyn Fn>`.
  SyncMode: SyncAll (default) | SyncData.

StoreError: Io(io::Error) | Coordinate(CoordinateError) | Serialization(String)
  | CrcMismatch{segment_id,offset} | CorruptSegment{segment_id,detail}
  | NotFound(u128) | SequenceMismatch{entity,expected,actual}
  | DuplicateEvent(u128) | WriterCrashed | ShuttingDown | CacheFailed(String)
  impl Display, Error, From<CoordinateError>, From<io::Error>

AppendReceipt { pub event_id: u128, pub sequence: u64, pub disk_pos: DiskPos }

AppendOptions { pub expected_sequence: Option<u32>, pub idempotency_key: Option<u128>,
                pub correlation_id: Option<u128>, pub causation_id: Option<u128> }
  impl Default (all None)

RestartPolicy: Once | Bounded { max_restarts: u32, within_ms: u64 }
  impl Default → Once. EXACTLY two variants. [SPEC:RED FLAGS]

StoreIndex — pub(crate). Fields: streams, scope_entities, by_fact, by_id, latest,
  global_sequence: AtomicU64, len: AtomicUsize, entity_locks: DashMap
  .insert(entry) .get_by_id(u128) .get_latest(&str) .stream(&str) .query(&Region)
  .global_sequence() -> u64 .len() -> usize

IndexEntry { pub event_id: u128, pub correlation_id: u128, pub causation_id: Option<u128>,
             pub coord: Coordinate, pub kind: EventKind, pub wall_ms: u64, pub clock: u32,
             pub hash_chain: HashChain, pub disk_pos: DiskPos, pub global_sequence: u64 }
  .is_correlated() -> bool    (event_id != correlation_id)
  .is_caused_by(u128) -> bool (causation_id == Some(id))
  .is_root_cause() -> bool    (causation_id.is_none())

ClockKey { pub wall_ms: u64, pub clock: u32, pub uuid: u128 }
  impl Ord: wall_ms first, then clock, then uuid tiebreak. [SPEC:IMPLEMENTATION NOTES item 1]

DiskPos { pub segment_id: u64, pub offset: u64, pub length: u32 }

WriterHandle — pub(crate) { pub tx: Sender<WriterCommand>, pub subscribers: Arc<SubscriberList>, thread }
  ::spawn(config, index, subscribers) -> Result<Self, StoreError>
  .tx is pub(crate) — Store sends commands directly, no wrapper method.

WriterCommand: Append{entity,scope,event,kind,correlation_id,causation_id,respond}
  | Sync{respond} | Shutdown{respond}

SubscriberList { senders: Mutex<Vec<Sender<Notification>>> }
  .subscribe(capacity) -> Receiver<Notification>
  .broadcast(Notification)  // try_send, retain on Ok|Full, prune on Disconnected

Notification: Clone + Debug
  { pub event_id: u128, pub correlation_id: u128, pub causation_id: Option<u128>,
    pub coord: Coordinate, pub kind: EventKind, pub sequence: u64 }

Cursor { region: Region, position: u64, index: Arc<StoreIndex> }
  .poll() -> Option<IndexEntry>
  .poll_batch(max: usize) -> Vec<IndexEntry>

Subscription { rx: Receiver<Notification>, region: Region }
  .recv() -> Option<Notification>               // sync, blocks, filters by region
  .receiver() -> &Receiver<Notification>         // for async: rx.recv_async().await

ProjectionCache: trait (Send + Sync + 'static)
  .get(key: &[u8]) -> Result<Option<(Vec<u8>, CacheMeta)>, StoreError>
  .put(key: &[u8], value: &[u8], meta: CacheMeta) -> Result<(), StoreError>
  .delete_prefix(prefix: &[u8]) -> Result<u64, StoreError>
  .sync() -> Result<(), StoreError>

CacheMeta { pub watermark: u64, pub cached_at_us: i64 }
Freshness: Consistent | BestEffort { max_stale_ms: u64 }
NoCache — default, always miss, forces replay from segments

SegmentHeader { pub version: u16, pub flags: u16, pub created_ns: i64, pub segment_id: u64 }
FramePayload<P> { pub event: Event<P>, pub entity: String, pub scope: String }
Segment<Active> — writable. Segment<Sealed> — immutable. Typestate transition via .seal()

StoreStats { pub event_count: usize, pub global_sequence: u64 }
```

---

## build.rs

IMPORTS:
```rust
use std::fs;
use std::path::Path;
```

TYPES: none (build script, not a library module)

IMPL:
```rust
/// build.rs runs before every cargo build/check/test. Cannot be skipped.
/// It enforces SPEC invariants at build time so agents get English errors
/// instead of cryptic compiler failures. [SPEC:INVARIANTS]

fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=src/");

    check_no_tokio_in_deps();
    check_no_banned_patterns();
}

fn check_no_tokio_in_deps() {
    /// Invariant 1: tokio must not appear in [dependencies].
    /// Only [dev-dependencies] is allowed. [SPEC:INVARIANTS item 1]
    let cargo = fs::read_to_string("Cargo.toml").expect("read Cargo.toml");

    /// Strategy: find the [dependencies] section, take text until the next
    /// section header (line starting with [), check for "tokio".
    /// This is deliberately simple string matching — no toml parser dep.
    if let Some(deps_section) = cargo.split("[dependencies]").nth(1) {
        let deps_only = deps_section.split("\n[").next().unwrap_or("");
        if deps_only.contains("tokio") {
            panic!(
                "INVARIANT 1 VIOLATED: tokio found in [dependencies].\n\
                 tokio belongs in [dev-dependencies] only.\n\
                 The library is runtime-agnostic. Fan-out uses Vec<flume::Sender>.\n\
                 See: SPEC.md ## INVARIANTS, item 1."
            );
        }
    }
}

fn check_no_banned_patterns() {
    /// Walk src/**/*.rs, read each file, check for patterns that violate
    /// invariants or red flags. [SPEC:RED FLAGS]
    walk_rs_files(Path::new("src"), &|path, contents| {
        let path_str = path.display().to_string();

        /// Red flag: no transmute/mem::read/pointer_cast in any src file.
        /// All serialization goes through MessagePack. [SPEC:RED FLAGS item 1]
        for banned in ["transmute", "mem::read", "pointer_cast"] {
            if contents.contains(banned) {
                panic!(
                    "RED FLAG VIOLATED in {path_str}: found `{banned}`.\n\
                     repr(C) is for field ordering, not a wire format.\n\
                     All serialization goes through rmp-serde. Always.\n\
                     See: SPEC.md ## RED FLAGS, item 1."
                );
            }
        }

        /// Invariant 2: no async fn in store module.
        /// Store API is sync. Async lives in flume channels. [SPEC:INVARIANTS item 2]
        if path_str.contains("store") && contents.contains("async fn") {
            panic!(
                "INVARIANT 2 VIOLATED in {path_str}: found `async fn`.\n\
                 Store API is sync. Async callers use spawn_blocking()\n\
                 or flume's recv_async(). See: store/subscription.rs.\n\
                 See: SPEC.md ## INVARIANTS, item 2."
            );
        }

        /// Invariant 3: no product concepts in library code.
        /// Check struct/enum/fn/type declarations for banned nouns.
        /// Skip string literals and comments. [SPEC:INVARIANTS item 3]
        let banned_nouns = [
            "trajectory", "artifact", "tenant",
        ];
        /// NOTE: "scope" and "agent" are common English words.
        /// "turn" and "note" are substrings of "return" and "annotation" —
        /// substring matching would false-positive on legitimate Rust code.
        /// Only check nouns that are unambiguous product concepts.
        /// Strategy: check lines starting with pub/fn/struct/enum/type
        /// for WORD-BOUNDARY matches of banned nouns.
        for line in contents.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") || trimmed.starts_with("///") {
                continue; // skip comments
            }
            let is_decl = trimmed.starts_with("pub ")
                || trimmed.starts_with("fn ")
                || trimmed.starts_with("struct ")
                || trimmed.starts_with("enum ")
                || trimmed.starts_with("type ");
            if is_decl {
                let lower = trimmed.to_lowercase();
                for noun in &banned_nouns {
                    /// Word boundary check: noun must be preceded by start/underscore/space
                    /// and followed by end/underscore/space/(/>. Prevents "return" matching "turn".
                    let has_match = lower.split(|c: char| !c.is_alphanumeric() && c != '_')
                        .any(|word| {
                            word == *noun
                            || word.starts_with(&format!("{noun}_"))
                            || word.ends_with(&format!("_{noun}"))
                            || word.contains(&format!("_{noun}_"))
                        });
                    if has_match {
                        panic!(
                            "INVARIANT 3 VIOLATED in {path_str}: \
                             product concept `{noun}` in declaration:\n  {trimmed}\n\
                             Library vocabulary: coordinate, entity, event, outcome, \
                             gate, region, transition.\n\
                             See: SPEC.md ## INVARIANTS, item 3."
                        );
                    }
                }
            }
        }
    });
}

fn walk_rs_files(dir: &Path, check: &dyn Fn(&Path, &str)) {
    /// Recursive directory walk. Only reads .rs files.
    /// Uses std::fs only — no external deps allowed in build scripts
    /// unless declared in [build-dependencies].
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk_rs_files(&path, check);
            } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
                if let Ok(contents) = fs::read_to_string(&path) {
                    check(&path, &contents);
                }
            }
        }
    }
}
```

TESTS: build.rs is tested implicitly — if invariants are violated, cargo build fails.

>[build.rs]

---

## src/wire.rs

IMPORTS:
```rust
use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::ser::Serializer;
use std::fmt;
```

TYPES: three submodules, no structs.

IMPL:
```rust
/// Serde helpers for u128 serialization as [u8; 16] big-endian.
/// MessagePack has no native u128 type. Bare u128 causes rmp-serde errors.
/// Big-endian preserves sort order and is standard network byte order.
/// [SPEC:WIRE FORMAT DECISIONS item 2]
///
/// ZERO internal dependencies. This module is declared FIRST in lib.rs.
/// Every serializable type with a u128 field uses these helpers.
/// [SPEC:BUILD ORDER STEP 4 — wire.rs is FIRST]

pub mod u128_bytes {
    /// Usage: #[serde(with = "crate::wire::u128_bytes")]
    /// Annotated on: EventHeader.event_id, EventHeader.correlation_id,
    ///   Notification.event_id, Notification.correlation_id,
    ///   Committed.event_id, WaitCondition::Event.event_id,
    ///   CompensationAction::Notify.target_id, Outcome::Pending.resume_token

    pub fn serialize<S: Serializer>(val: &u128, ser: S) -> Result<S::Ok, S::Error> {
        /// Convert to 16-byte big-endian array, serialize as bytes.
        /// [DEP:serde::Serializer::serialize_bytes]
        ser.serialize_bytes(&val.to_be_bytes())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<u128, D::Error> {
        /// Accept bytes, convert from big-endian to u128.
        /// Use a Visitor that handles both byte arrays and sequences.
        /// [DEP:serde::de::Visitor]
        struct U128Visitor;
        impl<'de> Visitor<'de> for U128Visitor {
            type Value = u128;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("16 bytes for u128")
            }
            fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<u128, E> {
                /// v must be exactly 16 bytes. Convert via from_be_bytes.
                let arr: [u8; 16] = v.try_into().map_err(|_| {
                    E::invalid_length(v.len(), &"16 bytes")
                })?;
                Ok(u128::from_be_bytes(arr))
            }
            /// Also handle seq format (some deserializers emit sequences not bytes)
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<u128, A::Error> {
                let mut bytes = [0u8; 16];
                for (i, byte) in bytes.iter_mut().enumerate() {
                    *byte = seq.next_element()?
                        .ok_or_else(|| de::Error::invalid_length(i, &"16 bytes"))?;
                }
                Ok(u128::from_be_bytes(bytes))
            }
        }
        de.deserialize_bytes(U128Visitor)
    }
}

pub mod option_u128_bytes {
    /// Usage: #[serde(with = "crate::wire::option_u128_bytes")]
    /// Annotated on: EventHeader.causation_id, Notification.causation_id

    pub fn serialize<S: Serializer>(val: &Option<u128>, ser: S) -> Result<S::Ok, S::Error> {
        match val {
            Some(v) => ser.serialize_bytes(&v.to_be_bytes()),
            None => ser.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Option<u128>, D::Error> {
        /// Visitor that handles None (nil) and Some(bytes).
        struct OptU128Visitor;
        impl<'de> Visitor<'de> for OptU128Visitor {
            type Value = Option<u128>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("null or 16 bytes for u128")
            }
            fn visit_none<E: de::Error>(self) -> Result<Option<u128>, E> {
                Ok(None)
            }
            fn visit_some<D2: Deserializer<'de>>(self, de: D2) -> Result<Option<u128>, D2::Error> {
                super::u128_bytes::deserialize(de).map(Some)
            }
            fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Option<u128>, E> {
                let arr: [u8; 16] = v.try_into().map_err(|_| {
                    E::invalid_length(v.len(), &"16 bytes")
                })?;
                Ok(Some(u128::from_be_bytes(arr)))
            }
        }
        de.deserialize_option(OptU128Visitor)
    }
}

pub mod vec_u128_bytes {
    /// Usage: #[serde(with = "crate::wire::vec_u128_bytes")]
    /// Annotated on: CompensationAction::Rollback.event_ids,
    ///   CompensationAction::Release.resource_ids

    pub fn serialize<S: Serializer>(val: &[u128], ser: S) -> Result<S::Ok, S::Error> {
        /// Serialize as a sequence of [u8; 16] fixed-size arrays (NOT bytes).
        /// Using arrays ensures serialize and deserialize use the same msgpack
        /// format (array of arrays, not array of bin). Avoids format mismatch.
        use serde::ser::SerializeSeq;
        let mut seq = ser.serialize_seq(Some(val.len()))?;
        for v in val {
            seq.serialize_element(&v.to_be_bytes())?; // [u8; 16], serialized as array
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<u128>, D::Error> {
        /// Deserialize a sequence of [u8; 16] arrays back to Vec<u128>.
        struct VecU128Visitor;
        impl<'de> Visitor<'de> for VecU128Visitor {
            type Value = Vec<u128>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("sequence of 16-byte u128 values")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<u128>, A::Error> {
                let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0));
                while let Some(arr) = seq.next_element::<[u8; 16]>()? {
                    out.push(u128::from_be_bytes(arr));
                }
                Ok(out)
            }
        }
        de.deserialize_seq(VecU128Visitor)
    }
}
```

TESTS: [FILE:tests/wire_format.rs] golden file comparison verifies round-trip.

>[wire.rs]

---

## src/event/kind.rs

IMPORTS:
```rust
use serde::{Deserialize, Serialize};
use std::fmt;
```

TYPES:
```rust
/// EventKind wraps a private u16. Products cannot construct arbitrary system kinds.
/// Products use EventKind::custom(category, type_id) which validates the range.
/// [SPEC:src/event/kind.rs]

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventKind(u16); // PRIVATE inner field — not pub
```

IMPL:
```rust
impl EventKind {
    /// category:type encoding. Upper 4 bits = category, lower 12 = type.
    /// Products use categories 0x1-0xF. System uses 0x0 and 0xD.
    pub const fn custom(category: u8, type_id: u16) -> Self {
        /// Combine: (category as u16) << 12 | (type_id & 0x0FFF)
        Self(((category as u16) << 12) | (type_id & 0x0FFF))
    }

    pub const fn category(self) -> u8 {
        (self.0 >> 12) as u8
    }

    pub const fn type_id(self) -> u16 {
        self.0 & 0x0FFF
    }

    pub const fn is_system(self) -> bool {
        self.category() == 0x0
    }

    pub const fn is_effect(self) -> bool {
        self.category() == 0xD
    }

    /// Library constants. Products NEVER define these — they use custom().
    pub const DATA: Self = Self(0x0000);
    pub const SYSTEM_INIT: Self = Self(0x0001);
    pub const SYSTEM_SHUTDOWN: Self = Self(0x0002);
    pub const SYSTEM_HEARTBEAT: Self = Self(0x0003);
    pub const SYSTEM_CONFIG_CHANGE: Self = Self(0x0004);
    pub const SYSTEM_CHECKPOINT: Self = Self(0x0005);
    pub const EFFECT_ERROR: Self = Self(0xD001);
    pub const EFFECT_RETRY: Self = Self(0xD002);
    pub const EFFECT_ACK: Self = Self(0xD004);
    pub const EFFECT_BACKPRESSURE: Self = Self(0xD005);
    pub const EFFECT_CANCEL: Self = Self(0xD006);
    pub const EFFECT_CONFLICT: Self = Self(0xD007);
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:04X}", self.0)
    }
}
```

TESTS: [FILE:tests/gate_pipeline.rs] verifies custom() range. [FILE:tests/wire_format.rs] verifies serde round-trip.

>[kind.rs]

---

## src/coordinate/mod.rs

IMPORTS:
```rust
pub mod position;
pub use position::DagPosition;

use crate::event::EventKind;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;
```

TYPES:
```rust
/// Coordinate: WHO (entity) + WHERE (scope). The address of an event stream.
/// [SPEC:src/coordinate/mod.rs]

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Coordinate {
    entity: Arc<str>,   // WHO — stream key, hash chain anchor
    scope: Arc<str>,    // WHERE — isolation boundary
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoordinateError {
    EmptyEntity,
    EmptyScope,
}

/// Region: the ONE predicate type for query, subscription, cursor, traversal.
/// [SPEC:src/coordinate/mod.rs — Region replaces SubscriptionPattern]
#[derive(Clone, Debug, Default)]
pub struct Region {
    pub entity_prefix: Option<Arc<str>>,
    pub scope: Option<Arc<str>>,
    pub fact: Option<KindFilter>,
    pub clock_range: Option<(u32, u32)>, // per-entity clock, NOT global_sequence [SPEC:IMPLEMENTATION NOTES item 12]
}

#[derive(Clone, Debug)]
pub enum KindFilter {
    Exact(EventKind),
    Category(u8),    // matches any EventKind in this 4-bit category
    Any,
}
```

IMPL:
```rust
impl Coordinate {
    pub fn new(
        entity: impl AsRef<str>,
        scope: impl AsRef<str>,
    ) -> Result<Self, CoordinateError> {
        let entity = entity.as_ref();
        let scope = scope.as_ref();
        if entity.is_empty() { return Err(CoordinateError::EmptyEntity); }
        if scope.is_empty() { return Err(CoordinateError::EmptyScope); }
        Ok(Self {
            entity: Arc::from(entity),
            scope: Arc::from(scope),
        })
    }

    pub fn entity(&self) -> &str { &self.entity }
    pub fn scope(&self) -> &str { &self.scope }
    pub(crate) fn entity_arc(&self) -> Arc<str> { Arc::clone(&self.entity) }
    pub(crate) fn scope_arc(&self) -> Arc<str> { Arc::clone(&self.scope) }
}

impl fmt::Display for Coordinate {
    /// "entity@scope"
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.entity, self.scope)
    }
}

impl fmt::Display for CoordinateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyEntity => write!(f, "entity cannot be empty"),
            Self::EmptyScope => write!(f, "scope cannot be empty"),
        }
    }
}
impl std::error::Error for CoordinateError {}

/// Region builder — method chaining. [SPEC:src/coordinate/mod.rs — Region builder]
impl Region {
    pub fn all() -> Self { Self::default() }

    pub fn entity(prefix: impl AsRef<str>) -> Self {
        Self { entity_prefix: Some(Arc::from(prefix.as_ref())), ..Self::default() }
    }

    pub fn scope(scope: impl AsRef<str>) -> Self {
        Self { scope: Some(Arc::from(scope.as_ref())), ..Self::default() }
    }

    pub fn coordinate(coord: &Coordinate) -> Self {
        Self {
            entity_prefix: Some(coord.entity_arc()),
            scope: Some(coord.scope_arc()),
            ..Self::default()
        }
    }

    /// Chainable setters
    pub fn with_scope(mut self, scope: impl AsRef<str>) -> Self {
        self.scope = Some(Arc::from(scope.as_ref()));
        self
    }

    pub fn with_fact(mut self, filter: KindFilter) -> Self {
        self.fact = Some(filter);
        self
    }

    pub fn with_fact_category(mut self, cat: u8) -> Self {
        self.fact = Some(KindFilter::Category(cat));
        self
    }

    pub fn with_clock_range(mut self, range: (u32, u32)) -> Self {
        self.clock_range = Some(range);
        self
    }

    /// Match against individual fields — avoids circular dep on store::Notification.
    /// Called by Subscription::recv() to filter events. [FILE:src/store/subscription.rs]
    pub fn matches_event(&self, entity: &str, scope: &str, kind: EventKind) -> bool {
        if let Some(ref prefix) = self.entity_prefix {
            if !entity.starts_with(prefix.as_ref()) {
                return false;
            }
        }
        if let Some(ref s) = self.scope {
            if scope != s.as_ref() {
                return false;
            }
        }
        if let Some(ref fact) = self.fact {
            match fact {
                KindFilter::Exact(k) => if kind != *k { return false; },
                KindFilter::Category(c) => if kind.category() != *c { return false; },
                KindFilter::Any => {},
            }
        }
        /// clock_range is not checked here — it's for index queries, not live filtering.
        true
    }
}
```

TESTS: [FILE:tests/store_integration.rs] tests Region query matching. [FILE:tests/gate_pipeline.rs] tests Coordinate construction.

>[mod.rs]

---

## src/coordinate/position.rs

IMPORTS:
```rust
use serde::{Deserialize, Serialize};
use std::fmt;
```

TYPES:
```rust
/// DagPosition: graph position with hybrid logical clock + depth + lane + sequence.
/// wall_ms + counter provide global causal ordering (HLC-style) across entities.
/// depth/lane/sequence provide per-entity chain ordering.
/// v1: depth=0, lane=0 always. Sequence is per-entity monotonic counter.
/// Lane/depth vocabulary is scaffolding for future distributed fan-out.
/// [SPEC:src/coordinate/position.rs]
/// [CROSS-POLLINATION:czap/hlc.ts — HLC adds wall-clock causality to event ordering]

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DagPosition {
    /// Wall-clock milliseconds at event creation. HLC layer 1.
    pub wall_ms: u64,
    /// HLC counter for same-millisecond tiebreaking.
    pub counter: u16,
    pub depth: u32,
    pub lane: u32,
    pub sequence: u32,
}
```

IMPL:
```rust
impl DagPosition {
    pub const fn new(depth: u32, lane: u32, sequence: u32) -> Self {
        Self { wall_ms: 0, counter: 0, depth, lane, sequence }
    }

    /// Full constructor with HLC fields.
    pub const fn with_hlc(wall_ms: u64, counter: u16, depth: u32, lane: u32, sequence: u32) -> Self {
        Self { wall_ms, counter, depth, lane, sequence }
    }

    pub const fn root() -> Self {
        Self { wall_ms: 0, counter: 0, depth: 0, lane: 0, sequence: 0 }
    }

    /// v1: always depth=0, lane=0, sequence=N. wall_ms set by writer.
    pub const fn child(sequence: u32) -> Self {
        Self { wall_ms: 0, counter: 0, depth: 0, lane: 0, sequence }
    }

    /// v1 with HLC: same as child but with wall clock context.
    pub const fn child_at(sequence: u32, wall_ms: u64, counter: u16) -> Self {
        Self { wall_ms, counter, depth: 0, lane: 0, sequence }
    }

    /// Future: fork creates a new lane at depth+1
    pub const fn fork(parent_depth: u32, new_lane: u32) -> Self {
        Self { wall_ms: 0, counter: 0, depth: parent_depth + 1, lane: new_lane, sequence: 0 }
    }

    pub const fn is_root(&self) -> bool {
        self.depth == 0 && self.lane == 0 && self.sequence == 0
    }

    /// Causal ordering: ancestor if same lane, same depth, and lower sequence.
    /// v1: depth is always 0, lane always 0, so just compare sequence.
    /// DAG-ready: different depths means different branches — not ancestor.
    pub const fn is_ancestor_of(&self, other: &DagPosition) -> bool {
        self.lane == other.lane
            && self.depth == other.depth
            && self.sequence < other.sequence
    }
}

impl fmt::Display for DagPosition {
    /// "depth:lane:sequence@wall_ms.counter"
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}@{}.{}", self.depth, self.lane, self.sequence, self.wall_ms, self.counter)
    }
}

/// PartialOrd for causal ordering — not total because different lanes
/// are incomparable. [SPEC:src/coordinate/position.rs — PartialOrd]
impl PartialOrd for DagPosition {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        if self.lane != other.lane {
            return None; // different lanes are incomparable
        }
        Some(self.sequence.cmp(&other.sequence))
    }
}
```

TESTS: unit tests inline. [FILE:tests/store_integration.rs] verifies position assignment.

>[position.rs]

---

## src/outcome/error.rs

IMPORTS:
```rust
use serde::{Deserialize, Serialize};
use std::fmt;
```

TYPES:
```rust
/// OutcomeError: structured error with kind, message, optional compensation.
/// [SPEC:src/outcome/error.rs]

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutcomeError {
    pub kind: ErrorKind,
    pub message: String,
    pub compensation: Option<super::wait::CompensationAction>,
    pub retryable: bool,
}

/// ErrorKind: 8 domain kinds + Custom(u16) for product extension.
/// Products extend via Custom(u16) — same category:type encoding as EventKind.
/// [SPEC:src/outcome/error.rs — ErrorKind]

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ErrorKind {
    NotFound,
    Conflict,
    Validation,
    PolicyRejection,
    StorageError,
    Timeout,
    Serialization,
    Internal,
    Custom(u16),
}
```

IMPL:
```rust
impl ErrorKind {
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::StorageError | Self::Timeout)
    }

    pub fn is_domain(&self) -> bool {
        matches!(self, Self::NotFound | Self::Conflict | Self::Validation | Self::PolicyRejection)
    }

    pub fn is_operational(&self) -> bool {
        matches!(self, Self::StorageError | Self::Timeout | Self::Serialization | Self::Internal)
    }
}

impl fmt::Display for OutcomeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{:?}] {}", self.kind, self.message)
    }
}
impl std::error::Error for OutcomeError {}
```

TESTS: [FILE:tests/monad_laws.rs] constructs OutcomeErrors in Outcome::Err variants.

>[error.rs]

---

## src/outcome/wait.rs

IMPORTS:
```rust
use serde::{Deserialize, Serialize};
// NOTE: No `use crate::wire::*` needed here. The #[serde(with = "crate::wire::...")]
// annotations are string literal paths — serde resolves them at compile time, not
// through Rust's `use` mechanism. The wire module just needs to exist in the crate.
```

TYPES:
```rust
/// WaitCondition: what an Outcome::Pending is waiting for.
/// [SPEC:src/outcome/wait.rs]

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum WaitCondition {
    Timeout { resume_at_ms: u64 },
    Event {
        #[serde(with = "crate::wire::u128_bytes")]
        event_id: u128,
    },
    All(Vec<WaitCondition>),
    Any(Vec<WaitCondition>),
    Custom { tag: u16, data: Vec<u8> },
}

/// CompensationAction: what to do when an error needs compensation.
/// The writer persists this as data. Products implement the handler.
/// [SPEC:src/outcome/wait.rs — CompensationAction]

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum CompensationAction {
    Rollback {
        #[serde(with = "crate::wire::vec_u128_bytes")]
        event_ids: Vec<u128>,
    },
    Notify {
        #[serde(with = "crate::wire::u128_bytes")]
        target_id: u128,
        message: String,
    },
    Release {
        #[serde(with = "crate::wire::vec_u128_bytes")]
        resource_ids: Vec<u128>,
    },
    Custom { action_type: String, data: Vec<u8> },
}
```

IMPL: no methods — pure data types.

TESTS: [FILE:tests/wire_format.rs] verifies u128 round-trip through msgpack.

>[wait.rs]

---

## src/outcome/combine.rs

IMPORTS:
```rust
use super::{Outcome, OutcomeError};
use crate::outcome::error::ErrorKind;
```

TYPES: no types — free functions only.

IMPL:
```rust
/// zip: combine two outcomes into a tuple outcome.
/// If either is Err, the first Err wins.
/// [SPEC:src/outcome/combine.rs]

pub fn zip<A: Clone, B: Clone>(a: Outcome<A>, b: Outcome<B>) -> Outcome<(A, B)> {
    /// Priority order for non-Ok variants (highest wins):
    ///   Err > Cancelled > Retry > Pending > Batch > Ok
    /// When both are non-Ok, the FIRST (a) argument's variant wins at equal priority.
    match (a, b) {
        // Both Ok → combine
        (Outcome::Ok(a), Outcome::Ok(b)) => Outcome::Ok((a, b)),

        // Either Err → first Err wins
        (Outcome::Err(e), _) | (_, Outcome::Err(e)) => Outcome::Err(e),

        // Either Cancelled → first Cancelled wins
        (Outcome::Cancelled { reason }, _) | (_, Outcome::Cancelled { reason }) => {
            Outcome::Cancelled { reason }
        }

        // Either Retry → first Retry wins
        (Outcome::Retry { after_ms, attempt, max_attempts, reason }, _)
        | (_, Outcome::Retry { after_ms, attempt, max_attempts, reason }) => {
            Outcome::Retry { after_ms, attempt, max_attempts, reason }
        }

        // Either Pending → first Pending wins
        (Outcome::Pending { condition, resume_token }, _)
        | (_, Outcome::Pending { condition, resume_token }) => {
            Outcome::Pending { condition, resume_token }
        }

        // Both Batch → zip elements pairwise (truncate to shorter)
        (Outcome::Batch(a_items), Outcome::Batch(b_items)) => {
            Outcome::Batch(
                a_items.into_iter().zip(b_items).map(|(a, b)| zip(a, b)).collect()
            )
        }

        // One Batch, one Ok → map the Ok into each Batch element
        (Outcome::Batch(items), Outcome::Ok(b)) => {
            Outcome::Batch(items.into_iter().map(|a| zip(a, Outcome::Ok(b.clone()))).collect())
        }
        (Outcome::Ok(a), Outcome::Batch(items)) => {
            Outcome::Batch(items.into_iter().map(|b| zip(Outcome::Ok(a.clone()), b)).collect())
        }
    }
}
/// A: Clone and B: Clone required for the Batch+Ok distribution cases above.

/// join_all: collect a Vec of outcomes into an outcome of Vec.
/// All must be Ok for the result to be Ok. First Err short-circuits.
/// [SPEC:src/outcome/combine.rs]

pub fn join_all<T>(outcomes: Vec<Outcome<T>>) -> Outcome<Vec<T>> {
    let mut results = Vec::with_capacity(outcomes.len());
    for outcome in outcomes {
        match outcome {
            Outcome::Ok(v) => results.push(v),
            Outcome::Err(e) => return Outcome::Err(e),
            Outcome::Cancelled { reason } => return Outcome::Cancelled { reason },
            Outcome::Retry { after_ms, attempt, max_attempts, reason } => {
                return Outcome::Retry { after_ms, attempt, max_attempts, reason };
            }
            Outcome::Pending { condition, resume_token } => {
                return Outcome::Pending { condition, resume_token };
            }
            Outcome::Batch(inner) => {
                /// Flatten: join_all on the inner batch, then continue collecting.
                match join_all(inner) {
                    Outcome::Ok(vs) => results.extend(vs),
                    other => return other.map(|mut vs| { let mut r = results; r.append(&mut vs); r }),
                }
            }
        }
    }
    Outcome::Ok(results)
}

/// join_any: first Ok wins. If all fail, last Err wins.
/// [SPEC:src/outcome/combine.rs]

pub fn join_any<T>(outcomes: Vec<Outcome<T>>) -> Outcome<T> {
    let mut last_err = None;
    for outcome in outcomes {
        match outcome {
            Outcome::Ok(v) => return Outcome::Ok(v),
            Outcome::Err(e) => last_err = Some(e),
            other => return other, // Retry/Pending/Cancelled propagate immediately
        }
    }
    match last_err {
        Some(e) => Outcome::Err(e),
        None => Outcome::Err(OutcomeError {
            kind: ErrorKind::Internal,
            message: "join_any called with empty vec".into(),
            compensation: None,
            retryable: false,
        }),
    }
}
```

TESTS: [FILE:tests/monad_laws.rs] verifies zip/join_all/join_any properties.

>[combine.rs]

---

## src/outcome/mod.rs

IMPORTS:
```rust
use serde::{Deserialize, Serialize};
// NOTE: No `use crate::wire::*` needed. serde(with = "crate::wire::...") resolves via string path.

pub mod error;
pub mod combine;
pub mod wait;

pub use error::{OutcomeError, ErrorKind};
pub use wait::{WaitCondition, CompensationAction};
pub use combine::{zip, join_all, join_any};
```

TYPES:
```rust
/// Outcome<T>: the core algebraic type. 6 variants.
/// Named "Outcome" not "Effect" to eliminate Effect/Event confusion.
/// [SPEC:src/outcome/mod.rs]

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum Outcome<T> {
    Ok(T),
    Err(OutcomeError),
    Retry {
        after_ms: u64,
        attempt: u32,
        max_attempts: u32,
        reason: String,
    },
    Pending {
        condition: WaitCondition,
        #[serde(with = "crate::wire::u128_bytes")]
        resume_token: u128,
    },
    Cancelled { reason: String },
    Batch(Vec<Outcome<T>>),
}
```

IMPL:
```rust
impl<T> Outcome<T> {
    // --- Construction ---
    pub fn ok(val: T) -> Self { Self::Ok(val) }
    pub fn err(e: OutcomeError) -> Self { Self::Err(e) }
    pub fn cancelled(reason: impl Into<String>) -> Self {
        Self::Cancelled { reason: reason.into() }
    }
    pub fn retry(after_ms: u64, attempt: u32, max_attempts: u32, reason: impl Into<String>) -> Self {
        Self::Retry { after_ms, attempt, max_attempts, reason: reason.into() }
    }
    pub fn pending(condition: WaitCondition, resume_token: u128) -> Self {
        Self::Pending { condition, resume_token }
    }

    // --- Predicates ---
    pub fn is_ok(&self) -> bool { matches!(self, Self::Ok(_)) }
    pub fn is_err(&self) -> bool { matches!(self, Self::Err(_)) }
    pub fn is_retry(&self) -> bool { matches!(self, Self::Retry { .. }) }
    pub fn is_pending(&self) -> bool { matches!(self, Self::Pending { .. }) }
    pub fn is_cancelled(&self) -> bool { matches!(self, Self::Cancelled { .. }) }
    pub fn is_batch(&self) -> bool { matches!(self, Self::Batch(_)) }
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Ok(_) | Self::Err(_) | Self::Cancelled { .. })
    }

    // --- Combinators ---

    /// map: transform the Ok value. Distributes over Batch.
    /// [SPEC:src/outcome/mod.rs — combinators distribute over Batch via F: Clone]
    pub fn map<U, F: FnOnce(T) -> U + Clone>(self, f: F) -> Outcome<U> {
        match self {
            Self::Ok(v) => Outcome::Ok(f(v)),
            Self::Err(e) => Outcome::Err(e),
            Self::Retry { after_ms, attempt, max_attempts, reason } =>
                Outcome::Retry { after_ms, attempt, max_attempts, reason },
            Self::Pending { condition, resume_token } =>
                Outcome::Pending { condition, resume_token },
            Self::Cancelled { reason } => Outcome::Cancelled { reason },
            Self::Batch(items) => Outcome::Batch(
                items.into_iter().map(|o| o.map(f.clone())).collect()
            ),
        }
    }

    /// and_then: the monad bind. Distributes over Batch.
    /// F: Clone is required for Batch distribution (called once per element).
    /// [SPEC:src/outcome/mod.rs — The and_then monad fix]
    /// This is THE critical method. Monad laws are verified by proptest.
    /// [FILE:tests/monad_laws.rs]
    pub fn and_then<U, F: FnOnce(T) -> Outcome<U> + Clone>(self, f: F) -> Outcome<U> {
        match self {
            Self::Ok(v) => f(v),
            Self::Err(e) => Outcome::Err(e),
            Self::Retry { after_ms, attempt, max_attempts, reason } =>
                Outcome::Retry { after_ms, attempt, max_attempts, reason },
            Self::Pending { condition, resume_token } =>
                Outcome::Pending { condition, resume_token },
            Self::Cancelled { reason } => Outcome::Cancelled { reason },
            Self::Batch(items) => Outcome::Batch(
                items.into_iter().map(|o| o.and_then(f.clone())).collect()
            ),
        }
    }

    pub fn map_err<F: FnOnce(OutcomeError) -> OutcomeError + Clone>(self, f: F) -> Self {
        match self {
            Self::Err(e) => Self::Err(f(e)),
            Self::Batch(items) => Self::Batch(
                items.into_iter().map(|o| o.map_err(f.clone())).collect()
            ),
            other => other,
        }
    }

    pub fn or_else<F: FnOnce(OutcomeError) -> Outcome<T> + Clone>(self, f: F) -> Outcome<T> {
        match self {
            Self::Err(e) => f(e),
            Self::Batch(items) => Self::Batch(
                items.into_iter().map(|o| o.or_else(f.clone())).collect()
            ),
            other => other,
        }
    }

    pub fn flatten(self) -> Outcome<T>
    where T: Into<Outcome<T>> {
        /// Unwrap one layer: Outcome<Outcome<T>> → Outcome<T>
        self.and_then(|v| v.into())
    }

    pub fn inspect<F: FnOnce(&T) + Clone>(self, f: F) -> Self {
        match &self {
            Self::Ok(v) => { f(v); self }
            Self::Batch(_) => self.map(|v| { f(&v); v }),
            _ => self,
        }
    }

    pub fn inspect_err<F: FnOnce(&OutcomeError) + Clone>(self, f: F) -> Self {
        match &self {
            Self::Err(e) => { f(e); self }
            Self::Batch(_) => {
                /// Walk batch, inspect errors, return unchanged
                match self {
                    Self::Batch(items) => Self::Batch(
                        items.into_iter().map(|o| o.inspect_err(f.clone())).collect()
                    ),
                    _ => unreachable!(),
                }
            }
            _ => self,
        }
    }

    pub fn and_then_if<F: FnOnce(&T) -> bool, G: FnOnce(T) -> Outcome<T>>(
        self, pred: F, f: G,
    ) -> Outcome<T> {
        match self {
            Self::Ok(v) => if pred(&v) { f(v) } else { Self::Ok(v) },
            other => other,
        }
    }

    pub fn into_result(self) -> Result<T, OutcomeError> {
        match self {
            Self::Ok(v) => Ok(v),
            Self::Err(e) => Err(e),
            Self::Cancelled { reason } => Err(OutcomeError {
                kind: ErrorKind::Internal,
                message: format!("cancelled: {reason}"),
                compensation: None,
                retryable: false,
            }),
            _ => Err(OutcomeError {
                kind: ErrorKind::Internal,
                message: "outcome is not terminal".into(),
                compensation: None,
                retryable: false,
            }),
        }
    }

    pub fn unwrap_or(self, default: T) -> T {
        match self {
            Self::Ok(v) => v,
            _ => default,
        }
    }

    pub fn unwrap_or_else<F: FnOnce() -> T>(self, f: F) -> T {
        match self {
            Self::Ok(v) => v,
            _ => f(),
        }
    }
}
```

TESTS:
  [FILE:tests/monad_laws.rs] — proptest: left/right identity, associativity, Batch distribution.
  Failure message includes: "LEFT IDENTITY VIOLATED: Outcome::ok(x).and_then(f) != f(x). Check: outcome/mod.rs and_then implementation. The F: Clone bound must recurse into Batch elements."

>[mod.rs]

---

## src/event/hash.rs

IMPORTS:
```rust
use serde::{Deserialize, Serialize};
```

TYPES:
```rust
/// HashChain: prev_hash + event_hash. Per-entity linear chain.
/// Default (all zeros) = genesis convention.
/// [SPEC:src/event/hash.rs — NO TRAIT. NO ENUM.]

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HashChain {
    pub prev_hash: [u8; 32],
    pub event_hash: [u8; 32],
}
```

IMPL:
```rust
impl Default for HashChain {
    fn default() -> Self {
        Self { prev_hash: [0u8; 32], event_hash: [0u8; 32] }
    }
}

/// compute_hash: blake3 hash of content bytes.
/// Behind feature = "blake3". When off, Committed.hash is [0u8; 32].
/// [SPEC:INVARIANTS item 5 — blake3 is the only hash]
/// [DEP:blake3::hash] → returns blake3::Hash, .into() gives [u8; 32]

#[cfg(feature = "blake3")]
pub fn compute_hash(content_bytes: &[u8]) -> [u8; 32] {
    blake3::hash(content_bytes).into()
}

/// verify_chain: check that event_hash matches content AND prev_hash matches expected.
/// [SPEC:src/event/hash.rs — verify_chain]

#[cfg(feature = "blake3")]
pub fn verify_chain(
    content_bytes: &[u8],
    chain: &HashChain,
    expected_prev: &[u8; 32],
) -> bool {
    chain.prev_hash == *expected_prev
        && chain.event_hash == compute_hash(content_bytes)
}
```

TESTS: [FILE:tests/hash_chain.rs] — proptest: chain verification, tamper detection, genesis.

>[hash.rs]

---

## src/guard/denial.rs

IMPORTS:
```rust
use serde::Serialize;
use std::fmt;
```

TYPES:
```rust
/// Denial: returned by a Gate when it rejects a proposal.
/// Separate from OutcomeError. Library does NOT auto-store denials.
/// Products decide whether to persist denials as events.
/// [SPEC:src/guard/denial.rs]

#[derive(Clone, Debug, PartialEq, Serialize)]
// NOTE: Denial does NOT derive Deserialize. The gate field is &'static str which
// cannot be deserialized from owned data (no 'static lifetime at deser time).
// The library never persists Denials — it returns them to callers.
// Products that want to persist denials serialize them into event payloads.
pub struct Denial {
    pub gate: &'static str,
    pub code: String,
    pub message: String,
    pub context: Vec<(String, String)>,
}
```

IMPL:
```rust
impl Denial {
    pub fn new(gate: &'static str, message: impl Into<String>) -> Self {
        Self { gate, code: String::new(), message: message.into(), context: vec![] }
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = code.into();
        self
    }

    pub fn with_context(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.context.push((key.into(), value.into()));
        self
    }
}

impl fmt::Display for Denial {
    /// "[gate] message"
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.gate, self.message)
    }
}
impl std::error::Error for Denial {}
```

TESTS: [FILE:tests/gate_pipeline.rs] tests Denial construction and Display.

>[denial.rs]

---

## src/guard/receipt.rs

IMPORTS:
```rust
// NO serde imports. Receipt is NOT Serialize, NOT Clone, NOT Copy.
```

TYPES:
```rust
/// Receipt<T>: proof that all gates passed. Consumed exactly once.
/// The seal module prevents external construction. Only GateSet::evaluate() creates these.
/// [SPEC:src/guard/receipt.rs — TOCTOU fix]

pub struct Receipt<T> {
    _seal: seal::Token,
    gates_passed: Vec<&'static str>,
    payload: T,
}

mod seal {
    /// Private module. Token cannot be constructed outside guard/.
    pub(crate) struct Token;
}
```

IMPL:
```rust
/// Receipt is NOT Clone, NOT Copy, NOT Serialize.
/// It wraps the payload INSIDE so it can't be mutated after gate evaluation.
/// Consumed via into_parts().

impl<T> Receipt<T> {
    /// Only callable from within the crate (seal::Token is pub(crate)).
    /// [FILE:src/guard/mod.rs — GateSet::evaluate() is the only caller]
    pub(crate) fn new(payload: T, gates_passed: Vec<&'static str>) -> Self {
        Self { _seal: seal::Token, gates_passed, payload }
    }

    pub fn payload(&self) -> &T { &self.payload }
    pub fn gates_passed(&self) -> &[&'static str] { &self.gates_passed }

    /// Consuming extraction. After this, the receipt is gone.
    pub fn into_parts(self) -> (T, Vec<&'static str>) {
        (self.payload, self.gates_passed)
    }
}
```

TESTS:
  [FILE:tests/gate_pipeline.rs] — receipt TOCTOU, consumed once.
  [FILE:tests/typestate_safety.rs] — trybuild: forge_receipt.rs must NOT compile.
  [FILE:tests/ui/forge_receipt.rs] — attempts to construct Receipt directly, expects E0603.

>[receipt.rs]

---

## src/guard/mod.rs

IMPORTS:
```rust
pub mod denial;
pub mod receipt;

pub use denial::Denial;
pub use receipt::Receipt;
```

TYPES:
```rust
/// Gate<Ctx>: a predicate that evaluates a context and either permits or denies.
/// Gates are PREDICATES, not transformers. No I/O, no mutation, pure.
/// Ctx is product-defined. Library is generic over it.
/// [SPEC:src/guard/mod.rs]

pub trait Gate<Ctx>: Send + Sync {
    fn name(&self) -> &'static str;
    fn evaluate(&self, ctx: &Ctx) -> Result<(), Denial>;
    fn description(&self) -> &'static str { "" }
}

/// GateSet<Ctx>: ordered collection of gates. Fail-fast by default.
pub struct GateSet<Ctx> {
    gates: Vec<Box<dyn Gate<Ctx>>>,
}
```

IMPL:
```rust
impl<Ctx> GateSet<Ctx> {
    pub fn new() -> Self { Self { gates: vec![] } }

    pub fn push(&mut self, gate: impl Gate<Ctx> + 'static) {
        self.gates.push(Box::new(gate));
    }

    /// Fail-fast evaluation. First denial stops.
    /// Returns Receipt<T> wrapping the proposal payload on success.
    pub fn evaluate<T>(&self, ctx: &Ctx, proposal: crate::pipeline::Proposal<T>)
        -> Result<Receipt<T>, Denial>
    {
        for gate in &self.gates {
            gate.evaluate(ctx)?;
        }
        let names: Vec<&'static str> = self.gates.iter().map(|g| g.name()).collect();
        Ok(Receipt::new(proposal.0, names))
    }

    /// Evaluate ALL gates (no fail-fast). For observability — collect all denials.
    pub fn evaluate_all(&self, ctx: &Ctx) -> Vec<Denial> {
        self.gates.iter()
            .filter_map(|g| g.evaluate(ctx).err())
            .collect()
    }

    pub fn len(&self) -> usize { self.gates.len() }
    pub fn is_empty(&self) -> bool { self.gates.is_empty() }
    pub fn names(&self) -> Vec<&'static str> {
        self.gates.iter().map(|g| g.name()).collect()
    }
}

impl<Ctx> Default for GateSet<Ctx> {
    fn default() -> Self { Self::new() }
}
```

TESTS: [FILE:tests/gate_pipeline.rs] — registration order, fail-fast, receipt TOCTOU, consumed once.

>[mod.rs]

---

## src/id/mod.rs

IMPORTS:
```rust
use std::fmt;
use std::hash::Hash;
use std::str::FromStr;
```

TYPES:
```rust
/// EntityIdType: Layer 0 trait. No uuid dep.
/// All IDs are u128 internally. No Uuid in public API. [SPEC:src/id/mod.rs]
/// [SPEC:RED FLAGS — DO NOT put uuid::Uuid in the public API]

pub trait EntityIdType:
    Copy + Clone + Eq + Hash + fmt::Debug + fmt::Display + FromStr + Send + Sync + 'static
{
    const ENTITY_NAME: &'static str;
    fn new(id: u128) -> Self;
    fn as_u128(&self) -> u128;
    fn now_v7() -> Self;
    fn nil() -> Self;
}
```

IMPL:
```rust
/// Helper function: generates a UUIDv7 as u128. Used by the macro below.
/// This keeps `uuid` as a private dependency — downstream crates calling
/// define_entity_id! don't need uuid in their own Cargo.toml.
/// [DEP:uuid::Uuid::now_v7] → generates UUIDv7, .as_u128() → u128
pub fn generate_v7_id() -> u128 {
    uuid::Uuid::now_v7().as_u128()
}

/// define_entity_id!: Layer 1+ macro. Uses generate_v7_id() helper.
/// Downstream crates do NOT need uuid as a direct dependency.

#[macro_export]
macro_rules! define_entity_id {
    ($name:ident, $entity:literal) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub struct $name(u128);

        impl $crate::id::EntityIdType for $name {
            const ENTITY_NAME: &'static str = $entity;

            fn new(id: u128) -> Self { Self(id) }

            fn as_u128(&self) -> u128 { self.0 }

            fn now_v7() -> Self {
                Self($crate::id::generate_v7_id())
            }

            fn nil() -> Self { Self(0) }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                /// Display as "entity_name:hex" e.g. "event:0a1b2c..."
                write!(f, "{}:{:032x}", $entity, self.0)
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = String;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                /// Parse "entity_name:hex" or bare hex
                let hex = s.strip_prefix(concat!($entity, ":")).unwrap_or(s);
                u128::from_str_radix(hex, 16)
                    .map(Self)
                    .map_err(|e| format!("invalid {}: {e}", $entity))
            }
        }
    };
}

/// Library defines ONE id type.
define_entity_id!(EventId, "event");
```

TESTS: [FILE:tests/gate_pipeline.rs] tests EventId round-trip Display/FromStr.

>[mod.rs]

---

## src/typestate/mod.rs

IMPORTS:
```rust
// No imports needed — pure macro_rules!, zero deps, zero runtime code.
```

TYPES: generated by macros.

IMPL:
```rust
/// define_state_machine!: generates a sealed marker trait + zero-sized state structs.
/// [SPEC:src/typestate/mod.rs — 99 LOC of macros]
///
/// Usage:
///   define_state_machine!(LockState { Acquired, Released });
///   // Generates:
///   //   pub trait LockState: private::Sealed {}
///   //   pub struct Acquired;
///   //   pub struct Released;
///   //   impl LockState for Acquired {}
///   //   impl LockState for Released {}

#[macro_export]
macro_rules! define_state_machine {
    ($trait_name:ident { $($state:ident),+ $(,)? }) => {
        mod private {
            pub trait Sealed {}
        }

        pub trait $trait_name: private::Sealed {}

        $(
            #[derive(Debug, Clone, Copy, PartialEq, Eq)]
            pub struct $state;

            impl private::Sealed for $state {}
            impl $trait_name for $state {}
        )+
    };
}

/// define_typestate!: generates a PhantomData wrapper for typed state machines.
/// [SPEC:src/typestate/mod.rs]
///
/// Usage:
///   define_typestate!(Lock<S: LockState> { holder: String });
///   // Generates Lock<S> with PhantomData<S>, data(), into_data(), new()

#[macro_export]
macro_rules! define_typestate {
    ($name:ident<$param:ident: $bound:ident> { $($field:ident: $ftype:ty),* $(,)? }) => {
        pub struct $name<$param: $bound> {
            $( pub $field: $ftype, )*
            _state: ::std::marker::PhantomData<$param>,
        }

        impl<$param: $bound> $name<$param> {
            pub fn new($($field: $ftype),*) -> Self {
                Self { $($field,)* _state: ::std::marker::PhantomData }
            }

            pub fn data(&self) -> ($(&$ftype,)*) {
                ($(&self.$field,)*)
            }

            pub fn into_data(self) -> ($($ftype,)*) {
                ($(self.$field,)*)
            }
        }
    };
}
```

TESTS: [FILE:tests/typestate_safety.rs] — trybuild compile-fail tests for invalid transitions.

>[mod.rs]

---

## src/typestate/transition.rs

IMPORTS:
```rust
use crate::event::EventKind;
use std::marker::PhantomData;
```

TYPES:
```rust
/// Transition<From, To, P>: a typed state change with an EventKind and payload.
/// The compiler ensures you can only create transitions from valid source states.
/// [SPEC:src/typestate/transition.rs]

pub struct Transition<From, To, P> {
    kind: EventKind,
    payload: P,
    _from: PhantomData<From>,
    _to: PhantomData<To>,
}
```

IMPL:
```rust
impl<From, To, P> Transition<From, To, P> {
    pub fn new(kind: EventKind, payload: P) -> Self {
        Self { kind, payload, _from: PhantomData, _to: PhantomData }
    }

    pub fn kind(&self) -> EventKind { self.kind }
    pub fn payload(&self) -> &P { &self.payload }
    pub fn into_payload(self) -> P { self.payload }
}
```

TESTS: [FILE:tests/typestate_safety.rs] — verifies transition type safety.

>[transition.rs]

---

## src/event/header.rs

IMPORTS:
```rust
use crate::coordinate::DagPosition;
use crate::event::EventKind;
use serde::{Deserialize, Serialize};
use std::fmt;
// NOTE: No `use crate::wire::*` needed. serde(with = "crate::wire::...") resolves via string path.
```

TYPES:
```rust
/// EventHeader: metadata for every event. Store generates this — users don't call new directly.
/// repr(C) for deterministic field ordering (NOT a wire format — msgpack handles serialization).
/// [SPEC:src/event/header.rs]

#[repr(C)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventHeader {
    #[serde(with = "crate::wire::u128_bytes")]
    pub event_id: u128,
    #[serde(with = "crate::wire::u128_bytes")]
    pub correlation_id: u128,
    #[serde(with = "crate::wire::option_u128_bytes")]
    pub causation_id: Option<u128>,
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
```

Flag bit constants:
```rust
pub const FLAG_REQUIRES_ACK: u8 = 0x01;
pub const FLAG_TRANSACTIONAL: u8 = 0x02;
pub const FLAG_REPLAY: u8 = 0x08;
```

IMPL:
```rust
impl EventHeader {
    pub fn new(
        event_id: u128,
        correlation_id: u128,
        causation_id: Option<u128>,
        timestamp_us: i64,
        position: DagPosition,
        payload_size: u32,
        event_kind: EventKind,
    ) -> Self {
        Self {
            event_id, correlation_id, causation_id, timestamp_us,
            position, payload_size, event_kind, flags: 0,
            content_hash: [0u8; 32],
        }
    }

    pub fn with_flags(mut self, flags: u8) -> Self {
        self.flags = flags;
        self
    }

    pub fn requires_ack(&self) -> bool { self.flags & FLAG_REQUIRES_ACK != 0 }
    pub fn is_transactional(&self) -> bool { self.flags & FLAG_TRANSACTIONAL != 0 }
    pub fn is_replay(&self) -> bool { self.flags & FLAG_REPLAY != 0 }
    pub fn age_us(&self, now_us: i64) -> u64 { now_us.saturating_sub(self.timestamp_us).max(0) as u64 }
}
```

TESTS: [FILE:tests/wire_format.rs] — golden file round-trip for EventHeader msgpack encoding.

>[header.rs]

---

## src/event/sourcing.rs

IMPORTS:
```rust
use crate::coordinate::Coordinate;
use crate::event::{Event, EventKind};
```

TYPES:
```rust
/// EventSourced<P>: backward-looking fold. Replay events to reconstruct state.
/// P is generic — NO serde_json dependency in the trait.
/// Store uses EventSourced<serde_json::Value>. [SPEC:src/event/sourcing.rs]

pub trait EventSourced<P>: Sized {
    fn from_events(events: &[Event<P>]) -> Option<Self>;
    fn apply_event(&mut self, event: &Event<P>);
    fn relevant_event_kinds() -> &'static [EventKind];
}

/// Reactive<P>: forward-looking counterpart. See event → maybe emit derived events.
/// Products compose: subscribe + react + append (7 lines of glue).
/// [SPEC:src/event/sourcing.rs]

pub trait Reactive<P> {
    fn react(&self, event: &Event<P>) -> Vec<(Coordinate, EventKind, P)>;
}
```

IMPL: traits only — no default implementations. ~15 LOC total.

>[sourcing.rs]

---

## src/event/mod.rs

IMPORTS:
```rust
pub mod kind;
pub mod header;
pub mod hash;
pub mod sourcing;

pub use kind::EventKind;
pub use header::EventHeader;
pub use hash::HashChain;
pub use sourcing::{EventSourced, Reactive};

use crate::coordinate::Coordinate;
use serde::{Deserialize, Serialize};
```

TYPES:
```rust
/// Event<P>: header + payload + optional hash chain.
/// [SPEC:src/event/mod.rs]

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event<P> {
    pub header: EventHeader,
    pub payload: P,
    pub hash_chain: Option<HashChain>,
}

/// StoredEvent<P>: what store.get() returns. Coordinate + Event.
/// store.get() returns StoredEvent<serde_json::Value> because segments are
/// schema-free MessagePack. [SPEC:src/event/mod.rs]

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredEvent<P> {
    pub coordinate: Coordinate,
    pub event: Event<P>,
}
```

IMPL:
```rust
impl<P> Event<P> {
    pub fn new(header: EventHeader, payload: P) -> Self {
        Self { header, payload, hash_chain: None }
    }

    pub fn with_hash_chain(mut self, chain: HashChain) -> Self {
        self.hash_chain = Some(chain);
        self
    }

    pub fn event_id(&self) -> u128 { self.header.event_id }
    pub fn event_kind(&self) -> EventKind { self.header.event_kind }
    pub fn position(&self) -> &crate::coordinate::DagPosition { &self.header.position }

    pub fn is_genesis(&self) -> bool {
        self.hash_chain.as_ref()
            .map(|c| c.prev_hash == [0u8; 32])
            .unwrap_or(true)
    }

    pub fn map_payload<U, F: FnOnce(P) -> U>(self, f: F) -> Event<U> {
        Event {
            header: self.header,
            payload: f(self.payload),
            hash_chain: self.hash_chain,
        }
    }
}
```

TESTS: [FILE:tests/store_integration.rs] [FILE:tests/wire_format.rs]

>[mod.rs]

---

## src/pipeline/mod.rs

IMPORTS:
```rust
use crate::guard::{Denial, GateSet, Receipt};
use serde::{Deserialize, Serialize};
// NOTE: No `use crate::wire::*` needed. serde(with = "crate::wire::...") resolves via string path.

pub mod bypass;
pub use bypass::{BypassReason, BypassReceipt};
```

TYPES:
```rust
/// Proposal<T>: wraps a value for gate evaluation.
/// [SPEC:src/pipeline/mod.rs]
pub struct Proposal<T>(pub T);

/// Committed<T>: proof that an event was persisted.
/// [SPEC:src/pipeline/mod.rs]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Committed<T> {
    pub payload: T,
    #[serde(with = "crate::wire::u128_bytes")]
    pub event_id: u128,
    pub sequence: u64,
    pub hash: [u8; 32], // blake3, or [0u8;32] if feature off
}

/// Pipeline<Ctx>: evaluate gates then commit.
/// [SPEC:src/pipeline/mod.rs]
pub struct Pipeline<Ctx> {
    gates: GateSet<Ctx>,
}
```

IMPL:
```rust
impl<T> Proposal<T> {
    pub fn new(payload: T) -> Self { Self(payload) }
    pub fn payload(&self) -> &T { &self.0 }
    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> Proposal<U> {
        Proposal(f(self.0))
    }
}

impl<Ctx> Pipeline<Ctx> {
    pub fn new(gates: GateSet<Ctx>) -> Self { Self { gates } }

    pub fn evaluate<T>(&self, ctx: &Ctx, proposal: Proposal<T>)
        -> Result<Receipt<T>, Denial>
    {
        self.gates.evaluate(ctx, proposal)
    }

    /// commit: generic over error type E. Pipeline doesn't know about StoreError.
    /// Products pass a closure that calls store.append() and wraps the result.
    /// [SPEC:IMPLEMENTATION NOTES item 9 — Pipeline::commit() E is generic]
    pub fn commit<T, E>(
        &self,
        receipt: Receipt<T>,
        commit_fn: impl FnOnce(T) -> Result<Committed<T>, E>,
    ) -> Result<Committed<T>, E> {
        let (payload, _gate_names) = receipt.into_parts();
        commit_fn(payload)
    }

    /// bypass: skip gates with an auditable reason.
    /// [FILE:src/pipeline/bypass.rs]
    pub fn bypass<T>(
        proposal: Proposal<T>,
        reason: &'static dyn BypassReason,
    ) -> BypassReceipt<T> {
        BypassReceipt {
            payload: proposal.0,
            reason: reason.name(),
            justification: reason.justification(),
        }
    }
}
```

TESTS: [FILE:tests/gate_pipeline.rs]

>[mod.rs]

---

## src/pipeline/bypass.rs

IMPORTS:
```rust
// No imports needed.
```

TYPES:
```rust
/// BypassReason: products implement this to justify skipping gates.
/// [SPEC:src/pipeline/bypass.rs]

pub trait BypassReason: Send + Sync {
    fn name(&self) -> &'static str;
    fn justification(&self) -> &'static str;
}

/// BypassReceipt<T>: audit trail shows "bypassed: {reason}".
pub struct BypassReceipt<T> {
    pub payload: T,
    pub reason: &'static str,
    pub justification: &'static str,
}
```

IMPL: none — pure types.

>[bypass.rs]

---

## src/prelude.rs

IMPORTS:
```rust
pub use crate::coordinate::{Coordinate, Region, KindFilter, CoordinateError};
pub use crate::coordinate::DagPosition;
pub use crate::event::{Event, EventHeader, EventKind, HashChain, StoredEvent, EventSourced};
pub use crate::guard::{Gate, GateSet, Denial, Receipt};
pub use crate::outcome::{Outcome, OutcomeError, ErrorKind};
pub use crate::pipeline::{Proposal, Committed};
pub use crate::store::Store;
```

TYPES: none — re-exports only.

IMPL: none.

>[prelude.rs]

---

## src/lib.rs

IMPORTS:
```rust
pub mod wire;
pub mod coordinate;
pub mod outcome;
pub mod event;
pub mod guard;
pub mod pipeline;
pub mod store;
pub mod typestate;
pub mod id;
pub mod prelude;
```

IMPL:
```rust
/// Module declarations in DEPENDENCY ORDER. wire first (zero deps).
/// [SPEC:src/lib.rs — Module declarations in DEPENDENCY ORDER]

/// compile_error guards for impossible configurations:
#[cfg(feature = "async-store")]
compile_error!(
    "INVARIANT 2: batpak does not have an async Store API. \
     Async callers use spawn_blocking() or flume recv_async(). \
     See: src/store/subscription.rs for the async pattern."
);

#[cfg(feature = "sha256")]
compile_error!(
    "INVARIANT 5: blake3 is the only hash. No HashAlgorithm enum. \
     One function: compute_hash(bytes) -> [u8; 32], behind feature = blake3."
);

/// Crate-level doc comment: see [SPEC:src/lib.rs] for structure.
/// P1: what it is. P2: four concepts. P3: hello world. P4: reading order.
```

>[lib.rs]

---

## src/store/index.rs

IMPORTS:
```rust
use crate::coordinate::Coordinate;
use crate::event::{EventKind, HashChain};
use dashmap::DashMap;
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
```

TYPES:
```rust
/// StoreIndex: in-memory 2D index + auxiliaries. NOT persisted — rebuilt from segments on cold start.
/// [SPEC:src/store/index.rs]
/// [DEP:dashmap::DashMap] — see DEPENDENCY SURFACE for deadlock warnings

pub(crate) struct StoreIndex {
    /// Primary: entity -> ordered events. [DEP:dashmap::DashMap::get_mut] for insert.
    streams: DashMap<Arc<str>, BTreeMap<ClockKey, IndexEntry>>,
    /// Scope dimension: scope -> set of entities in that scope.
    scope_entities: DashMap<Arc<str>, HashSet<Arc<str>>>,
    /// Fact dimension: event kind -> ordered events of that kind.
    by_fact: DashMap<EventKind, BTreeMap<ClockKey, IndexEntry>>,
    /// Point lookup: event_id -> entry. O(1) get by ID.
    by_id: DashMap<u128, IndexEntry>,
    /// Chain head: entity -> latest IndexEntry. For prev_hash in writer step 2.
    latest: DashMap<Arc<str>, IndexEntry>,
    /// Monotonic counter. Foundation for cursors, checkpoints, exactly-once.
    global_sequence: AtomicU64,
    /// Total event count.
    len: AtomicUsize,
    /// Per-entity write locks. Writer step 1 acquires these.
    /// [SPEC:IMPLEMENTATION NOTES item 5 — DashMap guard lifetimes]
    pub(crate) entity_locks: DashMap<Arc<str>, Arc<parking_lot::Mutex<()>>>,
}

/// ClockKey: BTreeMap key. Ord: wall_ms-first, then clock, then uuid tiebreak.
/// [SPEC:IMPLEMENTATION NOTES item 1]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClockKey {
    /// HLC wall clock milliseconds — global ordering across entities.
    pub wall_ms: u64,
    pub clock: u32,
    pub uuid: u128,
}

/// IndexEntry: everything needed for index queries without disk reads.
#[derive(Clone, Debug)]
pub struct IndexEntry {
    pub event_id: u128,
    pub correlation_id: u128,
    pub causation_id: Option<u128>,
    pub coord: Coordinate,
    pub kind: EventKind,
    /// HLC wall clock milliseconds — for global causal ordering.
    pub wall_ms: u64,
    pub clock: u32,
    pub hash_chain: HashChain,
    pub disk_pos: DiskPos,
    pub global_sequence: u64,
}

/// DiskPos: where to find this event on disk.
#[derive(Clone, Debug)]
pub struct DiskPos {
    pub segment_id: u64,
    pub offset: u64,
    pub length: u32,
}
```

IMPL:
```rust
impl Ord for ClockKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.wall_ms.cmp(&other.wall_ms)
            .then(self.clock.cmp(&other.clock))
            .then(self.uuid.cmp(&other.uuid))
    }
}
impl PartialOrd for ClockKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl IndexEntry {
    pub fn is_correlated(&self) -> bool { self.event_id != self.correlation_id }
    pub fn is_caused_by(&self, event_id: u128) -> bool { self.causation_id == Some(event_id) }
    pub fn is_root_cause(&self) -> bool { self.causation_id.is_none() }
}

impl StoreIndex {
    pub(crate) fn new() -> Self {
        Self {
            streams: DashMap::new(),
            scope_entities: DashMap::new(),
            by_fact: DashMap::new(),
            by_id: DashMap::new(),
            latest: DashMap::new(),
            global_sequence: AtomicU64::new(0),
            len: AtomicUsize::new(0),
            entity_locks: DashMap::new(),
        }
    }

    /// Called by writer step 9. Inserts into ALL indexes atomically.
    /// CONSTRAINT: caller MUST hold the entity lock before calling this.
    /// [SPEC:IMPLEMENTATION NOTES item 5]
    pub(crate) fn insert(&self, entry: IndexEntry) {
        let entity = entry.coord.entity_arc();
        let scope = entry.coord.scope_arc();
        let key = ClockKey { wall_ms: entry.wall_ms, clock: entry.clock, uuid: entry.event_id };

        /// Primary index: entity -> BTreeMap
        /// [DEP:dashmap::DashMap::entry] — holds write lock, release fast
        self.streams.entry(entity.clone())
            .or_insert_with(BTreeMap::new)
            .insert(key.clone(), entry.clone());

        /// Scope index
        self.scope_entities.entry(scope)
            .or_insert_with(HashSet::new)
            .insert(entity.clone());

        /// Fact index
        self.by_fact.entry(entry.kind)
            .or_insert_with(BTreeMap::new)
            .insert(key, entry.clone());

        /// Point lookup
        self.by_id.insert(entry.event_id, entry.clone());

        /// Chain head
        self.latest.insert(entity, entry);

        /// Counters
        self.global_sequence.fetch_add(1, Ordering::SeqCst);
        self.len.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn get_by_id(&self, event_id: u128) -> Option<IndexEntry> {
        self.by_id.get(&event_id).map(|r| r.value().clone())
    }

    pub(crate) fn get_latest(&self, entity: &str) -> Option<IndexEntry> {
        self.latest.get(entity).map(|r| r.value().clone())
    }

    pub(crate) fn stream(&self, entity: &str) -> Vec<IndexEntry> {
        self.streams.get(entity)
            .map(|r| r.value().values().cloned().collect())
            .unwrap_or_default()
    }

    pub(crate) fn query(&self, region: &crate::coordinate::Region) -> Vec<IndexEntry> {
        /// Region query strategy:
        /// 1. Determine candidate set based on most selective filter
        /// 2. Apply remaining filters to narrow results
        /// 3. Apply clock_range last (it's per-entity, cheap)
        use crate::coordinate::KindFilter;

        let mut candidates: Vec<IndexEntry> = if let Some(ref prefix) = region.entity_prefix {
            /// Entity prefix → scan streams map for matching keys
            /// [DEP:dashmap::DashMap::iter] — NOT a consistent snapshot, fine for queries
            self.streams.iter()
                .filter(|r| r.key().as_ref().starts_with(prefix.as_ref()))
                .flat_map(|r| r.value().values().cloned())
                .collect()
        } else if let Some(ref scope) = region.scope {
            /// Scope → look up entities in scope, collect their streams
            if let Some(entities) = self.scope_entities.get(scope.as_ref()) {
                entities.value().iter()
                    .flat_map(|entity| {
                        self.streams.get(entity.as_ref())
                            .map(|r| r.value().values().cloned().collect::<Vec<_>>())
                            .unwrap_or_default()
                    })
                    .collect()
            } else {
                Vec::new()
            }
        } else if let Some(ref fact) = region.fact {
            /// Fact filter without entity/scope → scan by_fact index
            match fact {
                KindFilter::Exact(k) => {
                    self.by_fact.get(k)
                        .map(|r| r.value().values().cloned().collect())
                        .unwrap_or_default()
                }
                KindFilter::Category(c) => {
                    let cat = *c;
                    self.by_fact.iter()
                        .filter(|r| r.key().category() == cat)
                        .flat_map(|r| r.value().values().cloned())
                        .collect()
                }
                KindFilter::Any => {
                    /// No filter at all — return everything (expensive, use sparingly)
                    self.streams.iter()
                        .flat_map(|r| r.value().values().cloned())
                        .collect()
                }
            }
        } else {
            /// Region::all() with no filters — return everything
            self.streams.iter()
                .flat_map(|r| r.value().values().cloned())
                .collect()
        };

        /// Apply remaining filters that weren't used for the initial candidate set.

        /// Scope filter (if entity_prefix was the primary selector)
        if region.entity_prefix.is_some() {
            if let Some(ref scope) = region.scope {
                candidates.retain(|e| e.coord.scope() == scope.as_ref());
            }
        }

        /// Entity prefix filter (if scope was the primary selector)
        if region.scope.is_some() && region.entity_prefix.is_none() {
            if let Some(ref prefix) = region.entity_prefix {
                candidates.retain(|e| e.coord.entity().starts_with(prefix.as_ref()));
            }
        }

        /// Fact filter (if not already applied)
        if region.entity_prefix.is_some() || region.scope.is_some() {
            if let Some(ref fact) = region.fact {
                candidates.retain(|e| match fact {
                    KindFilter::Exact(k) => e.kind == *k,
                    KindFilter::Category(c) => e.kind.category() == *c,
                    KindFilter::Any => true,
                });
            }
        }

        /// Clock range filter (always per-entity clock, not global_sequence)
        if let Some((min, max)) = region.clock_range {
            candidates.retain(|e| e.clock >= min && e.clock <= max);
        }

        /// Sort by global_sequence for consistent ordering
        candidates.sort_by_key(|e| e.global_sequence);
        candidates
    }

    pub(crate) fn global_sequence(&self) -> u64 {
        self.global_sequence.load(Ordering::SeqCst)
    }

    pub(crate) fn len(&self) -> usize {
        self.len.load(Ordering::Relaxed)
    }
}
```

>[index.rs]

---

## src/store/segment.rs

IMPORTS:
```rust
use crate::event::Event;
use crate::store::StoreError;
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
// NOTE: No `use crate::wire::*` needed. serde(with) resolves via string path.
```

TYPES:
```rust
/// Segment file format: magic + header + frames.
/// Magic: b"FBAT" (4 bytes). Header: 32 bytes. Frame: [len:u32 BE][crc32:u32 BE][msgpack]
/// Files named: {segment_id:06}.fbat (e.g., 000001.fbat). Sequential u64.
/// [SPEC:src/store/segment.rs]

pub const SEGMENT_MAGIC: &[u8; 4] = b"FBAT";
pub const SEGMENT_HEADER_SIZE: usize = 32;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SegmentHeader {
    pub version: u16,
    pub flags: u16,
    pub created_ns: i64,
    pub segment_id: u64,
}

/// FramePayload: what gets serialized into each frame.
/// entity and scope are stored as strings (not Coordinate) because segments
/// are the persistence layer — they don't depend on the Coordinate type.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FramePayload<P> {
    pub event: Event<P>,
    pub entity: String,
    pub scope: String,
}

/// Typestate for segment lifecycle.
pub struct Active;
pub struct Sealed;
pub struct Segment<State> {
    pub header: SegmentHeader,
    pub path: std::path::PathBuf,
    file: Option<std::fs::File>,
    written_bytes: u64,
    _state: std::marker::PhantomData<State>,
}

#[derive(Debug)]
pub struct CompactionResult {
    pub segments_removed: usize,
    pub bytes_reclaimed: u64,
}
```

IMPL:
```rust
/// frame_encode: serialize data to msgpack, wrap in [len:u32 BE][crc32:u32 BE][msgpack]
/// [SPEC:WIRE FORMAT DECISIONS — ALWAYS rmp_serde::to_vec_named()]
/// [DEP:rmp_serde::to_vec_named] → Result<Vec<u8>, encode::Error>
/// [DEP:crc32fast::hash] → u32

pub fn frame_encode<T: serde::Serialize>(data: &T) -> Result<Vec<u8>, StoreError> {
    let msgpack = rmp_serde::to_vec_named(data)
        .map_err(|e| StoreError::Serialization(e.to_string()))?;
    let crc = crc32fast::hash(&msgpack);
    let len = msgpack.len() as u32;

    let mut frame = Vec::with_capacity(8 + msgpack.len());
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&crc.to_be_bytes());
    frame.extend_from_slice(&msgpack);
    Ok(frame)
}

/// frame_decode: read [len][crc][msgpack], verify CRC, return msgpack bytes.
/// Returns (msgpack_bytes, total_frame_size_consumed).
pub fn frame_decode(buf: &[u8]) -> Result<(&[u8], usize), StoreError> {
    if buf.len() < 8 {
        return Err(StoreError::CorruptSegment {
            segment_id: 0, detail: "frame too short for header".into()
        });
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let expected_crc = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if buf.len() < 8 + len {
        return Err(StoreError::CorruptSegment {
            segment_id: 0, detail: "frame truncated".into()
        });
    }
    let msgpack = &buf[8..8 + len];
    let actual_crc = crc32fast::hash(msgpack);
    if actual_crc != expected_crc {
        return Err(StoreError::CrcMismatch { segment_id: 0, offset: 0 });
    }
    Ok((msgpack, 8 + len))
}

/// Segment naming helper.
pub fn segment_filename(segment_id: u64) -> String {
    format!("{:06}.fbat", segment_id)
}

impl Segment<Active> {
    /// Create new active segment.
    pub fn create(dir: &std::path::Path, segment_id: u64) -> Result<Self, StoreError> {
        let path = dir.join(segment_filename(segment_id));
        /// Use OpenOptions (NOT File::create_new — requires Rust 1.77, MSRV is 1.75)
        /// [SPEC:IMPLEMENTATION NOTES item 7 — MSRV workarounds]
        let mut file = std::fs::OpenOptions::new()
            .write(true).create_new(true).open(&path)
            .map_err(StoreError::Io)?;

        let header = SegmentHeader {
            version: 1, flags: 0,
            created_ns: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_nanos() as i64,
            segment_id,
        };

        /// Write magic + header
        file.write_all(SEGMENT_MAGIC).map_err(StoreError::Io)?;
        let header_bytes = rmp_serde::to_vec_named(&header)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;
        file.write_all(&header_bytes).map_err(StoreError::Io)?;

        Ok(Self {
            header, path, file: Some(file),
            written_bytes: (4 + header_bytes.len()) as u64,
            _state: std::marker::PhantomData,
        })
    }

    /// Write a frame. Returns offset where frame starts.
    pub fn write_frame(&mut self, frame: &[u8]) -> Result<u64, StoreError> {
        let offset = self.written_bytes;
        if let Some(ref mut f) = self.file {
            f.write_all(frame).map_err(StoreError::Io)?;
        }
        self.written_bytes += frame.len() as u64;
        Ok(offset)
    }

    pub fn needs_rotation(&self, max_bytes: u64) -> bool {
        self.written_bytes >= max_bytes
    }

    pub fn sync(&mut self) -> Result<(), StoreError> {
        if let Some(ref f) = self.file {
            f.sync_all().map_err(StoreError::Io)?;
        }
        Ok(())
    }

    /// Seal: close file handle, transition to Sealed.
    pub fn seal(mut self) -> Segment<Sealed> {
        drop(self.file.take());
        Segment {
            header: self.header, path: self.path, file: None,
            written_bytes: self.written_bytes, _state: std::marker::PhantomData,
        }
    }
}
```

>[segment.rs]

---

## src/store/reader.rs

IMPORTS:
```rust
use crate::coordinate::Coordinate;
use crate::event::{Event, StoredEvent};
use crate::store::segment::{self, FramePayload, SEGMENT_MAGIC};
use crate::store::{StoreError, DiskPos};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
```

TYPES:
```rust
/// Reader: reads events from segment files. LRU file descriptor cache.
/// Behind parking_lot::Mutex for Send + Sync. [SPEC:src/store/reader.rs]
/// [SPEC:IMPLEMENTATION NOTES item 6 — Store is Send + Sync]

pub(crate) struct Reader {
    data_dir: PathBuf,
    /// LRU FD cache: segment_id -> open File handle. Evicts oldest when full.
    /// [DEP:parking_lot::Mutex] — lock() returns guard directly, no poisoning
    fd_cache: Mutex<FdCache>,
}

struct FdCache {
    fds: HashMap<u64, File>,
    order: Vec<u64>,  // LRU order: most recent at end
    budget: usize,
}

/// ScannedEntry: what cold start produces per event in a segment.
pub(crate) struct ScannedEntry {
    pub event: Event<serde_json::Value>,
    pub entity: String,
    pub scope: String,
    pub segment_id: u64,
    pub offset: u64,
    pub length: u32,
}
```

IMPL:
```rust
impl Reader {
    pub(crate) fn new(data_dir: PathBuf, fd_budget: usize) -> Self {
        Self {
            data_dir,
            fd_cache: Mutex::new(FdCache {
                fds: HashMap::new(),
                order: Vec::new(),
                budget: fd_budget,
            }),
        }
    }

    /// Read a single event by disk position. CRC32 verified.
    /// [DEP:crc32fast::hash] verifies frame integrity on every read.
    pub(crate) fn read_entry(&self, pos: &DiskPos)
        -> Result<StoredEvent<serde_json::Value>, StoreError>
    {
        let file = self.get_fd(pos.segment_id)?;
        let mut buf = vec![0u8; pos.length as usize];

        /// Use pread (read_at) — doesn't modify file cursor. [SPEC:IMPLEMENTATION NOTES item 7]
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            file.read_at(&mut buf, pos.offset).map_err(StoreError::Io)?;
        }
        #[cfg(not(unix))]
        {
            /// Fallback: seek + read (holds the mutex so this is safe)
            use std::io::{Seek, SeekFrom};
            let mut file = file; // need mut for seek
            file.seek(SeekFrom::Start(pos.offset)).map_err(StoreError::Io)?;
            file.read_exact(&mut buf).map_err(StoreError::Io)?;
        }

        let (msgpack, _) = segment::frame_decode(&buf)?;
        let payload: FramePayload<serde_json::Value> = rmp_serde::from_slice(msgpack)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;

        let coord = Coordinate::new(&payload.entity, &payload.scope)
            .map_err(StoreError::Coordinate)?;
        Ok(StoredEvent { coordinate: coord, event: payload.event })
    }

    /// Scan an entire segment for cold start. Returns all events in order.
    pub(crate) fn scan_segment(&self, path: &Path)
        -> Result<Vec<ScannedEntry>, StoreError>
    {
        let mut file = File::open(path).map_err(StoreError::Io)?;
        let mut all_bytes = Vec::new();
        file.read_to_end(&mut all_bytes).map_err(StoreError::Io)?;

        /// Verify magic
        if all_bytes.len() < 4 || &all_bytes[..4] != SEGMENT_MAGIC {
            return Err(StoreError::CorruptSegment {
                segment_id: 0, detail: "bad magic".into()
            });
        }

        /// Extract segment_id from filename: "000042.fbat" → 42
        let segment_id = path.file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        /// Skip magic (4 bytes). Parse segment header from msgpack.
        /// [DEP:rmp_serde::from_slice] — deserialize SegmentHeader
        let after_magic = &all_bytes[4..];
        let _header: segment::SegmentHeader = rmp_serde::from_slice(after_magic)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;

        /// Find where header ends and frames begin.
        /// Re-encode header to measure its serialized size (simplest approach).
        let header_bytes = rmp_serde::to_vec_named(&_header)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;
        let mut cursor = 4 + header_bytes.len();

        /// Read frames until EOF. Each frame: [len:u32 BE][crc32:u32 BE][msgpack]
        let mut entries = Vec::new();
        while cursor < all_bytes.len() {
            let remaining = &all_bytes[cursor..];
            if remaining.len() < 8 { break; } // not enough for a frame header

            let frame_offset = cursor as u64;
            match segment::frame_decode(remaining) {
                Ok((msgpack, frame_size)) => {
                    /// Deserialize frame payload
                    match rmp_serde::from_slice::<FramePayload<serde_json::Value>>(msgpack) {
                        Ok(payload) => {
                            entries.push(ScannedEntry {
                                event: payload.event,
                                entity: payload.entity,
                                scope: payload.scope,
                                segment_id,
                                offset: frame_offset,
                                length: frame_size as u32,
                            });
                        }
                        Err(e) => {
                            tracing::warn!(
                                segment_id, offset = frame_offset,
                                "skipping unreadable frame: {e}"
                            );
                        }
                    }
                    cursor += frame_size;
                }
                Err(StoreError::CrcMismatch { .. }) => {
                    tracing::warn!(segment_id, offset = frame_offset, "CRC mismatch, skipping frame");
                    break; // CRC mismatch = stop scanning this segment
                }
                Err(_) => break, // truncated or corrupt — stop
            }
        }
        Ok(entries)
    }

    fn get_fd(&self, segment_id: u64) -> Result<File, StoreError> {
        let mut cache = self.fd_cache.lock();
        /// LRU logic: if in cache, move to end of order vec. If not, open file,
        /// evict oldest if over budget, insert.
        if let Some(pos) = cache.order.iter().position(|&id| id == segment_id) {
            cache.order.remove(pos);
            cache.order.push(segment_id);
            return Ok(cache.fds[&segment_id].try_clone().map_err(StoreError::Io)?);
        }

        let path = self.data_dir.join(segment::segment_filename(segment_id));
        let file = File::open(&path).map_err(StoreError::Io)?;

        if cache.fds.len() >= cache.budget {
            if let Some(oldest) = cache.order.first().copied() {
                cache.fds.remove(&oldest);
                cache.order.remove(0);
            }
        }

        cache.fds.insert(segment_id, file.try_clone().map_err(StoreError::Io)?);
        cache.order.push(segment_id);
        Ok(file)
    }
}
```

>[reader.rs]

---

## src/store/writer.rs

IMPORTS:
```rust
use crate::coordinate::{Coordinate, DagPosition};
use crate::event::{Event, EventHeader, EventKind, HashChain};
use crate::store::index::{StoreIndex, IndexEntry, ClockKey, DiskPos};
use crate::store::segment::{self, Segment, Active, FramePayload};
use crate::store::{StoreConfig, StoreError, AppendReceipt};
use flume::{Sender, Receiver, TrySendError};
use parking_lot::Mutex;
use std::sync::Arc;
use tracing::{debug, info, warn, trace};
```

TYPES:
```rust
/// WriterCommand: messages sent to the background writer thread via flume.
/// All respond channels: flume::Sender — sync send from writer, async recv from caller.
/// [SPEC:src/store/writer.rs]

pub(crate) enum WriterCommand {
    Append {
        entity: Arc<str>,
        scope: Arc<str>,
        event: Event<Vec<u8>>,  // pre-serialized payload as msgpack bytes
        kind: EventKind,
        correlation_id: u128,
        causation_id: Option<u128>,
        respond: Sender<Result<AppendReceipt, StoreError>>,
    },
    Sync {
        respond: Sender<Result<(), StoreError>>,
    },
    Shutdown {
        respond: Sender<Result<(), StoreError>>,
    },
}

/// WriterHandle: owned by Store. Communicates with the background thread.
pub(crate) struct WriterHandle {
    pub tx: Sender<WriterCommand>,
    pub subscribers: Arc<SubscriberList>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// SubscriberList: push-based notification fanout via flume channels.
/// [SPEC:src/store/writer.rs — try_send pattern]
pub(crate) struct SubscriberList {
    senders: Mutex<Vec<Sender<Notification>>>,
}

/// Notification: lightweight event summary pushed to subscribers.
/// Must derive Clone (used in try_send broadcast loop).
/// [SPEC:src/store/writer.rs — Notification struct]
#[derive(Clone, Debug)]
pub struct Notification {
    pub event_id: u128,
    pub correlation_id: u128,
    pub causation_id: Option<u128>,
    pub coord: Coordinate,
    pub kind: EventKind,
    pub sequence: u64,
}

/// RestartPolicy: how the writer recovers from panics.
/// [SPEC:src/store/writer.rs — RestartPolicy]
/// EXACTLY two variants. Adding a third violates the RED FLAGS.
#[derive(Clone, Debug)]
pub enum RestartPolicy {
    Once,
    Bounded { max_restarts: u32, within_ms: u64 },
}

impl Default for RestartPolicy {
    fn default() -> Self { Self::Once }
}
```

IMPL:
```rust
impl SubscriberList {
    pub(crate) fn new() -> Self {
        Self { senders: Mutex::new(Vec::new()) }
    }

    /// Subscribe: create a new bounded channel, store the sender, return the receiver.
    pub(crate) fn subscribe(&self, capacity: usize) -> Receiver<Notification> {
        let (tx, rx) = flume::bounded(capacity);
        self.senders.lock().push(tx);
        rx
    }

    /// Broadcast: try_send to all, retain on Ok or Full, prune on Disconnected.
    /// NEVER use blocking send() — one slow subscriber must not block the writer.
    /// [DEP:flume::Sender::try_send] → Result<(), TrySendError<T>>
    /// [DEP:flume::TrySendError::Full] / [DEP:flume::TrySendError::Disconnected]
    pub(crate) fn broadcast(&self, notif: Notification) {
        let mut guard = self.senders.lock();
        guard.retain(|tx| match tx.try_send(notif.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => false,
        });
    }
}

impl WriterHandle {
    /// Spawn the background writer thread.
    /// [SPEC:src/store/writer.rs — "batpak-writer-{hash}" thread]
    pub(crate) fn spawn(
        config: &Arc<StoreConfig>,
        index: &Arc<StoreIndex>,
        subscribers: &Arc<SubscriberList>,
    ) -> Result<Self, StoreError> {
        std::fs::create_dir_all(&config.data_dir).map_err(StoreError::Io)?;
        let initial_segment_id = find_latest_segment_id(&config.data_dir).unwrap_or(0) + 1;
        let initial_segment = Segment::<Active>::create(&config.data_dir, initial_segment_id)?;

        let (tx, rx) = flume::bounded::<WriterCommand>(config.writer_channel_capacity);
        let subs = Arc::clone(subscribers);
        let cfg = Arc::clone(config);
        let idx = Arc::clone(index);

        let thread_name = format!("batpak-writer-{:08x}", {
            let mut h: u64 = 0xcbf29ce484222325; // FNV-1a basis
            for b in config.data_dir.to_string_lossy().bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x100000001b3); // FNV-1a prime
            }
            h
        });

        let thread = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                writer_loop(&rx, &cfg, &idx, &subs, initial_segment, initial_segment_id);
            })
            .map_err(StoreError::Io)?;

        Ok(Self { tx, subscribers: Arc::clone(subscribers), _thread: Some(thread) })
    }

    /// NOTE: No send_append() method here. Store::append() and Store::append_reaction()
    /// in store/mod.rs create one-shot flume channels and send WriterCommands directly
    /// via self.writer.tx.send(). This avoids an unnecessary abstraction layer.
    /// WriterHandle.tx is pub(crate) for direct access. [SPEC:INVARIANTS item 4]
}

/// The writer's main loop. Runs on the background thread.
fn writer_loop(
    rx: Receiver<WriterCommand>,
    config: Arc<StoreConfig>,
    index: Arc<StoreIndex>,
    subscribers: Arc<SubscriberList>,
) {
    let data_dir = &config.data_dir;
    /// Initialize: create data_dir if not exists, find latest segment or create first.
    std::fs::create_dir_all(data_dir).expect("create data dir");
    let mut segment_id: u64 = find_latest_segment_id(data_dir).unwrap_or(0) + 1;
    let mut active_segment = Segment::<Active>::create(data_dir, segment_id)
        .expect("create initial segment");
    let mut events_since_sync: u32 = 0;

    /// Main loop: recv commands, dispatch.
    for cmd in rx.iter() {
        match cmd {
            WriterCommand::Append { entity, scope, event, kind,
                                    correlation_id, causation_id, respond } => {
                let result = handle_append(
                    &entity, &scope, event, kind, correlation_id, causation_id,
                    &index, &mut active_segment, &mut segment_id,
                    &config, &subscribers,
                );
                /// Respond to caller. Ignore send error (caller may have dropped).
                let _ = respond.send(result);

                events_since_sync += 1;
                if events_since_sync >= config.sync_every_n_events {
                    let _ = active_segment.sync_with_mode(&config.sync_mode);
                    events_since_sync = 0;
                }
            }
            WriterCommand::Sync { respond } => {
                let result = active_segment.sync_with_mode(&config.sync_mode);
                let _ = respond.send(result);
                events_since_sync = 0;
            }
            WriterCommand::Shutdown { respond } => {
                /// Drain up to shutdown_drain_limit queued commands.
                /// [SPEC:src/store/writer.rs — Shutdown drain semantics]
                let mut drained = 0;
                while drained < config.shutdown_drain_limit {
                    match rx.try_recv() {
                        Ok(WriterCommand::Append { entity, scope, event, kind,
                                                   correlation_id, causation_id, respond: r }) => {
                            let result = handle_append(
                                &entity, &scope, event, kind, correlation_id, causation_id,
                                &index, &mut active_segment, &mut segment_id,
                                &config, &subscribers,
                            );
                            let _ = r.send(result);
                            drained += 1;
                        }
                        Ok(WriterCommand::Shutdown { respond: r }) => {
                            let _ = r.send(Ok(()));
                        }
                        Ok(WriterCommand::Sync { respond: r }) => {
                            let _ = r.send(active_segment.sync_with_mode(&config.sync_mode));
                        }
                        Err(_) => break, // channel empty
                    }
                }
                let _ = active_segment.sync_with_mode(&config.sync_mode);
                let _ = respond.send(Ok(()));
                return; // exit writer loop
            }
        }
    }
}

/// The 10-step commit protocol.
/// [SPEC:src/store/writer.rs — handle_append]
fn handle_append(
    entity: &Arc<str>,
    scope: &Arc<str>,
    mut event: Event<Vec<u8>>,
    kind: EventKind,
    correlation_id: u128,
    causation_id: Option<u128>,
    index: &StoreIndex,
    active_segment: &mut Segment<Active>,
    segment_id: &mut u64,
    config: &StoreConfig,
    subscribers: &SubscriberList,
) -> Result<AppendReceipt, StoreError> {

    /// STEP 1: Acquire per-entity lock.
    /// [SPEC:IMPLEMENTATION NOTES item 5 — DashMap guard lifetimes]
    /// Clone the Arc<Mutex> OUT of DashMap, drop the DashMap entry guard,
    /// THEN lock the Mutex. Never hold DashMap Ref across the commit.
    let lock = index.entity_locks.entry(entity.clone())
        .or_insert_with(|| Arc::new(parking_lot::Mutex::new(())))
        .clone();
    let _entity_guard = lock.lock();
    debug!(entity = %entity, "entity lock acquired");

    /// STEP 2: Get prev_hash from index (or [0u8;32] for genesis).
    /// Clone the value out of the DashMap Ref immediately.
    let prev_hash = index.get_latest(entity)
        .map(|e| e.hash_chain.event_hash)
        .unwrap_or([0u8; 32]);

    /// STEP 3: Compute sequence (latest.clock + 1, or 0).
    let clock = index.get_latest(entity)
        .map(|e| e.clock + 1)
        .unwrap_or(0);

    /// STEP 4: Set event header position with HLC wall clock.
    /// Ensure wall_ms is monotonically non-decreasing per entity.
    let raw_ms = (event.header.timestamp_us / 1000) as u64;
    let last_ms = index.get_latest(entity).map(|e| e.wall_ms).unwrap_or(0);
    let now_ms = raw_ms.max(last_ms);
    let position = DagPosition::child_at(clock, now_ms, 0);
    event.header.position = position;
    event.header.event_kind = kind;
    event.header.correlation_id = correlation_id;
    event.header.causation_id = causation_id;

    /// STEP 5: Compute blake3 hash, set hash chain (or skip if feature off).
    /// [SPEC:INVARIANTS item 5 — blake3 only]
    let payload_for_hash = &event.payload; // pre-serialized bytes
    #[cfg(feature = "blake3")]
    let event_hash = crate::event::hash::compute_hash(payload_for_hash);
    #[cfg(not(feature = "blake3"))]
    let event_hash = [0u8; 32];

    event.hash_chain = Some(HashChain { prev_hash, event_hash });

    /// STEP 6: Serialize to MessagePack + CRC32 frame.
    /// [SPEC:WIRE FORMAT DECISIONS — rmp_serde::to_vec_named() ALWAYS]
    let frame_payload = FramePayload {
        event: event.clone(),
        entity: entity.to_string(),
        scope: scope.to_string(),
    };
    let frame = segment::frame_encode(&frame_payload)?;

    /// STEP 7: Check segment rotation.
    if active_segment.needs_rotation(config.segment_max_bytes) {
        active_segment.sync_with_mode(&config.sync_mode)?;
        let old = std::mem::replace(
            active_segment,
            Segment::<Active>::create(&config.data_dir, *segment_id + 1)?,
        );
        let _sealed = old.seal();
        *segment_id += 1;
        info!(segment_id = *segment_id, "segment rotated");
    }

    /// STEP 8: Write frame to segment file.
    let offset = active_segment.write_frame(&frame)?;
    trace!(offset = offset, len = frame.len(), "frame written");

    /// STEP 9: Update index.
    let global_seq = index.global_sequence();
    let disk_pos = DiskPos {
        segment_id: *segment_id,
        offset,
        length: frame.len() as u32,
    };
    let entry = IndexEntry {
        event_id: event.header.event_id,
        correlation_id,
        causation_id,
        coord: Coordinate::new(entity.as_ref(), scope.as_ref())
            .map_err(StoreError::Coordinate)?,
        kind,
        wall_ms: now_ms,
        clock,
        hash_chain: event.hash_chain.clone().unwrap_or_default(),
        disk_pos: disk_pos.clone(),
        global_sequence: global_seq,
    };
    index.insert(entry);
    debug!(event_id = %event.header.event_id, clock = clock, "append committed");

    /// STEP 10: Broadcast notification to subscribers.
    subscribers.broadcast(Notification {
        event_id: event.header.event_id,
        correlation_id,
        causation_id,
        coord: Coordinate::new(entity.as_ref(), scope.as_ref())
            .map_err(StoreError::Coordinate)?,
        kind,
        sequence: global_seq,
    });

    Ok(AppendReceipt {
        event_id: event.header.event_id,
        sequence: global_seq,
        disk_pos,
    })
}

/// Find the latest segment ID by scanning data_dir for .fbat files.
fn find_latest_segment_id(dir: &std::path::Path) -> Option<u64> {
    std::fs::read_dir(dir).ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_str()?;
            if name.ends_with(".fbat") {
                name.trim_end_matches(".fbat").parse::<u64>().ok()
            } else { None }
        })
        .max()
}
```

>[writer.rs]

---

## src/store/projection.rs

IMPORTS:
```rust
use crate::store::StoreError;
use serde::{Deserialize, Serialize};
```

TYPES:
```rust
/// ProjectionCache: trait for caching projected state.
/// Three impls: NoCache (default), RedbCache (optional), LmdbCache (optional).
/// [SPEC:src/store/projection.rs]

pub trait ProjectionCache: Send + Sync + 'static {
    fn get(&self, key: &[u8]) -> Result<Option<(Vec<u8>, CacheMeta)>, StoreError>;
    fn put(&self, key: &[u8], value: &[u8], meta: CacheMeta) -> Result<(), StoreError>;
    fn delete_prefix(&self, prefix: &[u8]) -> Result<u64, StoreError>;
    fn sync(&self) -> Result<(), StoreError>;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CacheMeta {
    pub watermark: u64,
    pub cached_at_us: i64,
}

#[derive(Clone, Debug)]
pub enum Freshness {
    Consistent,
    BestEffort { max_stale_ms: u64 },
}

/// NoCache: default. Every read replays from segments. No state.
pub struct NoCache;
```

IMPL:
```rust
impl ProjectionCache for NoCache {
    fn get(&self, _key: &[u8]) -> Result<Option<(Vec<u8>, CacheMeta)>, StoreError> {
        Ok(None) // always miss — forces replay
    }
    fn put(&self, _key: &[u8], _value: &[u8], _meta: CacheMeta) -> Result<(), StoreError> {
        Ok(()) // no-op
    }
    fn delete_prefix(&self, _prefix: &[u8]) -> Result<u64, StoreError> {
        Ok(0) // nothing to delete
    }
    fn sync(&self) -> Result<(), StoreError> {
        Ok(()) // nothing to sync
    }
}

/// RedbCache: backed by redb embedded database.
/// [DEP:redb::Database::create] [DEP:redb::TableDefinition]
#[cfg(feature = "redb")]
pub struct RedbCache {
    db: redb::Database,
}

#[cfg(feature = "redb")]
const CACHE_TABLE: redb::TableDefinition<&[u8], &[u8]> = redb::TableDefinition::new("projection_cache");

#[cfg(feature = "redb")]
impl RedbCache {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, StoreError> {
        let db = redb::Database::create(path.as_ref())
            .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        Ok(Self { db })
    }
}

#[cfg(feature = "redb")]
impl ProjectionCache for RedbCache {
    fn get(&self, key: &[u8]) -> Result<Option<(Vec<u8>, CacheMeta)>, StoreError> {
        let txn = self.db.begin_read().map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        let table = txn.open_table(CACHE_TABLE).map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        match table.get(key) {
            Ok(Some(guard)) => {
                let bytes = guard.value().to_vec();
                /// Last 16 bytes = CacheMeta (watermark u64 LE + cached_at_us i64 LE)
                if bytes.len() < 16 {
                    return Ok(None);
                }
                let (value, meta_bytes) = bytes.split_at(bytes.len() - 16);
                let watermark = u64::from_le_bytes(meta_bytes[..8].try_into().unwrap());
                let cached_at_us = i64::from_le_bytes(meta_bytes[8..16].try_into().unwrap());
                Ok(Some((value.to_vec(), CacheMeta { watermark, cached_at_us })))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(StoreError::CacheFailed(e.to_string())),
        }
    }

    fn put(&self, key: &[u8], value: &[u8], meta: CacheMeta) -> Result<(), StoreError> {
        /// Append CacheMeta as last 16 bytes of value
        let mut buf = Vec::with_capacity(value.len() + 16);
        buf.extend_from_slice(value);
        buf.extend_from_slice(&meta.watermark.to_le_bytes());
        buf.extend_from_slice(&meta.cached_at_us.to_le_bytes());

        let txn = self.db.begin_write().map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        {
            let mut table = txn.open_table(CACHE_TABLE).map_err(|e| StoreError::CacheFailed(e.to_string()))?;
            table.insert(key, buf.as_slice()).map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        }
        txn.commit().map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        Ok(())
    }

    fn delete_prefix(&self, prefix: &[u8]) -> Result<u64, StoreError> {
        /// redb has no built-in delete_prefix. Iterate range + collect keys + delete.
        let txn = self.db.begin_write().map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        let mut count = 0u64;
        {
            let mut table = txn.open_table(CACHE_TABLE).map_err(|e| StoreError::CacheFailed(e.to_string()))?;
            /// Range: prefix..prefix_with_ff_appended
            let mut end = prefix.to_vec();
            end.push(0xFF);
            let keys: Vec<Vec<u8>> = table.range(prefix..end.as_slice())
                .map_err(|e| StoreError::CacheFailed(e.to_string()))?
                .filter_map(|r| r.ok())
                .map(|(k, _)| k.value().to_vec())
                .collect();
            for key in &keys {
                table.remove(key.as_slice()).map_err(|e| StoreError::CacheFailed(e.to_string()))?;
                count += 1;
            }
        }
        txn.commit().map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        Ok(count)
    }

    fn sync(&self) -> Result<(), StoreError> {
        Ok(()) // redb commits are durable by default
    }
}

/// LmdbCache: backed by LMDB via heed.
/// [DEP:heed::EnvOpenOptions] — open() is unsafe (must not double-open same dir)
#[cfg(feature = "lmdb")]
pub struct LmdbCache {
    env: heed::Env,
    db: heed::Database<heed::types::Bytes, heed::types::Bytes>,
}

#[cfg(feature = "lmdb")]
impl LmdbCache {
    pub fn open(path: impl AsRef<std::path::Path>, map_size: usize) -> Result<Self, StoreError> {
        std::fs::create_dir_all(path.as_ref()).map_err(StoreError::Io)?;
        /// SAFETY: We guarantee this path is opened at most once per process.
        /// The Store owns the LmdbCache exclusively.
        let env = unsafe {
            heed::EnvOpenOptions::new()
                .map_size(map_size)
                .max_dbs(1)
                .open(path.as_ref())
                .map_err(|e| StoreError::CacheFailed(e.to_string()))?
        };
        let mut wtxn = env.write_txn().map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        let db = env.create_database(&mut wtxn, Some("projection_cache"))
            .map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        wtxn.commit().map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        Ok(Self { env, db })
    }
}

#[cfg(feature = "lmdb")]
impl ProjectionCache for LmdbCache {
    fn get(&self, key: &[u8]) -> Result<Option<(Vec<u8>, CacheMeta)>, StoreError> {
        let txn = self.env.read_txn().map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        match self.db.get(&txn, key).map_err(|e| StoreError::CacheFailed(e.to_string()))? {
            Some(bytes) if bytes.len() >= 16 => {
                let (value, meta_bytes) = bytes.split_at(bytes.len() - 16);
                let watermark = u64::from_le_bytes(meta_bytes[..8].try_into().unwrap());
                let cached_at_us = i64::from_le_bytes(meta_bytes[8..16].try_into().unwrap());
                Ok(Some((value.to_vec(), CacheMeta { watermark, cached_at_us })))
            }
            _ => Ok(None),
        }
    }

    fn put(&self, key: &[u8], value: &[u8], meta: CacheMeta) -> Result<(), StoreError> {
        let mut buf = Vec::with_capacity(value.len() + 16);
        buf.extend_from_slice(value);
        buf.extend_from_slice(&meta.watermark.to_le_bytes());
        buf.extend_from_slice(&meta.cached_at_us.to_le_bytes());

        let mut txn = self.env.write_txn().map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        self.db.put(&mut txn, key, &buf).map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        txn.commit().map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        Ok(())
    }

    fn delete_prefix(&self, prefix: &[u8]) -> Result<u64, StoreError> {
        /// heed has built-in delete_prefix! One line.
        let mut txn = self.env.write_txn().map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        let count = self.db.delete_prefix(&mut txn, prefix)
            .map_err(|e| StoreError::CacheFailed(e.to_string()))? as u64;
        txn.commit().map_err(|e| StoreError::CacheFailed(e.to_string()))?;
        Ok(count)
    }

    fn sync(&self) -> Result<(), StoreError> {
        self.env.force_sync().map_err(|e| StoreError::CacheFailed(e.to_string()))
    }
}
```

>[projection.rs]

---

## src/store/cursor.rs

IMPORTS:
```rust
use crate::coordinate::Region;
use crate::store::index::{StoreIndex, IndexEntry};
use std::sync::Arc;
```

TYPES:
```rust
/// Cursor: pull-based event consumption with guaranteed delivery.
/// Reads from index, not channels. Cannot lose events.
/// [SPEC:src/store/cursor.rs]

pub struct Cursor {
    region: Region,
    position: u64,      // tracks global_sequence — next poll starts after this
    index: Arc<StoreIndex>,
}
```

IMPL:
```rust
impl Cursor {
    pub(crate) fn new(region: Region, index: Arc<StoreIndex>) -> Self {
        Self { region, position: 0, index }
    }

    /// Poll for the next matching event after our current position.
    pub fn poll(&mut self) -> Option<IndexEntry> {
        /// Query the index for events matching our region with global_sequence > self.position.
        /// Return the first match, advance position.
        let results = self.index.query(&self.region);
        for entry in results {
            if entry.global_sequence > self.position {
                self.position = entry.global_sequence;
                return Some(entry);
            }
        }
        None
    }

    /// Poll for up to max matching events.
    pub fn poll_batch(&mut self, max: usize) -> Vec<IndexEntry> {
        let mut batch = Vec::with_capacity(max);
        let results = self.index.query(&self.region);
        for entry in results {
            if entry.global_sequence > self.position {
                self.position = entry.global_sequence;
                batch.push(entry);
                if batch.len() >= max { break; }
            }
        }
        batch
    }
}
```

>[cursor.rs]

---

## src/store/subscription.rs

IMPORTS:
```rust
use crate::coordinate::Region;
use crate::store::writer::Notification;
use flume::Receiver;
```

TYPES:
```rust
/// Subscription: push-based per-subscriber flume channel. Lossy.
/// If subscriber is slow, bounded channel fills. Writer's retain() prunes.
/// For guaranteed delivery, use Cursor instead.
/// [SPEC:src/store/subscription.rs]

pub struct Subscription {
    rx: Receiver<Notification>,
    region: Region,
}
```

IMPL:
```rust
impl Subscription {
    pub(crate) fn new(rx: Receiver<Notification>, region: Region) -> Self {
        Self { rx, region }
    }

    /// Blocking receive. Filters by region. Returns None if channel closed.
    pub fn recv(&self) -> Option<Notification> {
        loop {
            match self.rx.recv() {
                Ok(notif) => {
                    /// Filter: only return events matching our region.
                    /// [FILE:src/coordinate/mod.rs — Region::matches_event]
                    if self.region.matches_event(
                        notif.coord.entity(), notif.coord.scope(), notif.kind
                    ) {
                        return Some(notif);
                    }
                    /// Didn't match — keep receiving
                }
                Err(_) => return None, // channel closed
            }
        }
    }

    /// Expose the raw receiver for async usage.
    /// Caller uses: sub.receiver().recv_async().await
    /// [DEP:flume::Receiver::recv_async] → RecvFut<'_, T>: Future
    /// ASYNC NOTE: This is for async event consumption. For Store methods
    /// (append, get, query), use spawn_blocking instead. Two different patterns.
    /// [SPEC:src/store/subscription.rs — ASYNC NOTE]
    pub fn receiver(&self) -> &Receiver<Notification> {
        &self.rx
    }
}
```

>[subscription.rs]

---

## src/store/mod.rs

Current-state note (2026-03-30): the live repo no longer keeps every store
type in this file. `StoreConfig` lives in `src/store/config.rs`, `StoreError`
lives in `src/store/error.rs`, append/compaction contracts live in
`src/store/contracts.rs`, test-only runtime checks live in
`src/store/runtime_contracts.rs`, ancestor traversal is split into
`src/store/ancestors.rs` plus cfg-specific helper files, lifecycle helpers live
in `src/store/maintenance.rs`, projection orchestration lives in
`src/store/projection_flow.rs`, and test-only hooks live behind the
`test-support` feature in `src/store/test_support.rs`. Read the section below
as public API intent, not as the literal final file layout.

IMPORTS:
```rust
pub mod index;
pub mod segment;
pub mod writer;
pub mod reader;
pub mod projection;
pub mod cursor;
pub mod subscription;

pub use index::{IndexEntry, ClockKey, DiskPos};
pub use projection::{ProjectionCache, NoCache, CacheMeta, Freshness};
pub use cursor::Cursor;
pub use subscription::Subscription;
pub use writer::{Notification, RestartPolicy};

use crate::coordinate::{Coordinate, CoordinateError, Region, KindFilter};
use crate::event::{Event, EventHeader, EventKind, StoredEvent, EventSourced};
use crate::wire::u128_bytes;
use index::StoreIndex;
use reader::Reader;
use writer::{WriterHandle, WriterCommand, SubscriberList};
use projection::ProjectionCache;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
// NOTE: the `use crate::wire::u128_bytes` IS needed here — Store uses it
// for AppendOptions.idempotency_key serde annotation (if AppendOptions is serialized).
// If AppendOptions is never serialized, this can be removed.
```

TYPES:
```rust
/// Store: the runtime. Sync API. Send + Sync.
/// [SPEC:src/store/mod.rs]
/// Invariant 2: ALL METHODS ARE SYNC. No .await anywhere.
#[cfg(feature = "async-store")]
compile_error!("INVARIANT 2: Store API is sync. Use spawn_blocking or flume recv_async.");

pub struct Store {
    index: Arc<StoreIndex>,
    reader: Arc<Reader>,
    cache: Box<dyn ProjectionCache>,
    writer: WriterHandle,
    config: Arc<StoreConfig>,
}

/// StoreConfig: all settings for a Store instance.
/// No Default — callers must provide data_dir via `StoreConfig::new(path)`.
/// Manual Clone and Debug impls because `clock` field is `Arc<dyn Fn>`.
pub struct StoreConfig {
    pub data_dir: PathBuf,
    pub segment_max_bytes: u64,
    pub sync_every_n_events: u32,
    pub fd_budget: usize,
    pub writer_channel_capacity: usize,
    pub broadcast_capacity: usize,
    pub cache_map_size_bytes: usize,
    pub restart_policy: RestartPolicy,
    pub shutdown_drain_limit: usize,
    /// Optional writer thread stack size. None = OS default (~8MB on Linux).
    pub writer_stack_size: Option<usize>,
    /// Injectable clock for deterministic testing. Returns microseconds since epoch.
    /// None = std::time::SystemTime::now() (production default).
    pub clock: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    /// Sync mode: SyncAll (data+metadata, default) or SyncData (data only, faster).
    pub sync_mode: SyncMode,
}

/// Sync strategy for segment fsync.
#[derive(Clone, Debug, Default)]
pub enum SyncMode {
    #[default]
    SyncAll,
    SyncData,
}

impl StoreConfig {
    /// Create a StoreConfig with required data_dir and sensible defaults.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            segment_max_bytes: 256 * 1024 * 1024,  // 256MB
            sync_every_n_events: 1000,
            fd_budget: 64,
            writer_channel_capacity: 4096,
            broadcast_capacity: 8192,
            cache_map_size_bytes: 64 * 1024 * 1024, // 64MB
            restart_policy: RestartPolicy::default(),
            shutdown_drain_limit: 1024,
            writer_stack_size: None,
            clock: None,
            sync_mode: SyncMode::default(),
        }
    }
}

/// StoreError: every error the store can produce.
/// [SPEC:src/store/mod.rs — StoreError variants]
#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Coordinate(CoordinateError),
    Serialization(String),
    CrcMismatch { segment_id: u64, offset: u64 },
    CorruptSegment { segment_id: u64, detail: String },
    NotFound(u128),
    SequenceMismatch { entity: String, expected: u32, actual: u32 },
    DuplicateEvent(u128),
    WriterCrashed,
    ShuttingDown,
    CacheFailed(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {e}"),
            Self::Coordinate(e) => write!(f, "coordinate error: {e}"),
            Self::Serialization(s) => write!(f, "serialization error: {s}"),
            Self::CrcMismatch { segment_id, offset } =>
                write!(f, "CRC mismatch in segment {segment_id} at offset {offset}"),
            Self::CorruptSegment { segment_id, detail } =>
                write!(f, "corrupt segment {segment_id}: {detail}"),
            Self::NotFound(id) => write!(f, "event {id:032x} not found"),
            Self::SequenceMismatch { entity, expected, actual } =>
                write!(f, "CAS failed for {entity}: expected seq {expected}, got {actual}"),
            Self::DuplicateEvent(key) => write!(f, "duplicate idempotency key {key:032x}"),
            Self::WriterCrashed => write!(f, "writer thread crashed"),
            Self::ShuttingDown => write!(f, "store is shutting down"),
            Self::CacheFailed(s) => write!(f, "cache error: {s}"),
        }
    }
}
impl std::error::Error for StoreError {}
impl From<CoordinateError> for StoreError {
    fn from(e: CoordinateError) -> Self { Self::Coordinate(e) }
}
impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self { Self::Io(e) }
}

/// AppendReceipt: proof an event was persisted.
#[derive(Clone, Debug)]
pub struct AppendReceipt {
    pub event_id: u128,
    pub sequence: u64,
    pub disk_pos: DiskPos,
}

/// AppendOptions: CAS, idempotency, custom correlation/causation.
/// [SPEC:src/store/mod.rs — AppendOptions]
#[derive(Clone, Debug, Default)]
pub struct AppendOptions {
    pub expected_sequence: Option<u32>,
    pub idempotency_key: Option<u128>,
    pub correlation_id: Option<u128>,
    pub causation_id: Option<u128>,
}
```

IMPL:
```rust
impl Store {
    pub fn open(config: StoreConfig) -> Result<Self, StoreError> {
        std::fs::create_dir_all(&config.data_dir)?;
        let config = Arc::new(config);
        let index = Arc::new(StoreIndex::new());
        let reader = Arc::new(Reader::new(config.data_dir.clone(), config.fd_budget));

        /// Cold start: scan all segments, rebuild index.
        /// [SPEC:IMPLEMENTATION NOTES item 2 — segment naming, alphabetical scan]
        let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(&config.data_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|ext| ext == "fbat").unwrap_or(false))
            .collect();
        entries.sort_by_key(|e| e.file_name());

        for dir_entry in &entries {
            let scanned = reader.scan_segment(&dir_entry.path())?;
            for se in scanned {
                let coord = Coordinate::new(&se.entity, &se.scope)?;
                let clock = se.event.header.position.sequence;
                let entry = IndexEntry {
                    event_id: se.event.header.event_id,
                    correlation_id: se.event.header.correlation_id,
                    causation_id: se.event.header.causation_id,
                    coord,
                    kind: se.event.header.event_kind,
                    wall_ms: se.event.header.position.wall_ms,
                    clock,
                    hash_chain: se.event.hash_chain.clone().unwrap_or_default(),
                    disk_pos: DiskPos {
                        segment_id: se.segment_id,
                        offset: se.offset,
                        length: se.length,
                    },
                    global_sequence: index.global_sequence(),
                };
                index.insert(entry);
            }
        }

        let subscribers = Arc::new(SubscriberList::new());
        let writer = WriterHandle::spawn(
            Arc::clone(&config), Arc::clone(&index), Arc::clone(&subscribers),
        )?;

        Ok(Self {
            index, reader, cache: Box::new(NoCache), writer, config,
        })
    }

    pub fn open_default() -> Result<Self, StoreError> {
        Self::open(StoreConfig::new("./batpak-data"))
    }

    /// WRITE: append a new root-cause event.
    /// correlation_id defaults to event_id (self-correlated). causation_id = None.
    pub fn append(
        &self, coord: &Coordinate, kind: EventKind, payload: &impl Serialize,
    ) -> Result<AppendReceipt, StoreError> {
        let payload_bytes = rmp_serde::to_vec_named(payload)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;
        let event_id = crate::id::generate_v7_id();
        let header = EventHeader::new(
            event_id, event_id, None, // correlation = self, causation = root
            now_us(), crate::coordinate::DagPosition::root(),
            payload_bytes.len() as u32, kind,
        );
        let event = Event::new(header, payload_bytes);

        let (tx, rx) = flume::bounded(1);
        self.writer.tx.send(WriterCommand::Append {
            entity: coord.entity_arc(),
            scope: coord.scope_arc(),
            event, kind,
            correlation_id: event_id,
            causation_id: None,
            respond: tx,
        }).map_err(|_| StoreError::WriterCrashed)?;

        rx.recv().map_err(|_| StoreError::WriterCrashed)?
    }

    /// WRITE: append a reaction (caused by another event).
    pub fn append_reaction(
        &self, coord: &Coordinate, kind: EventKind, payload: &impl Serialize,
        correlation_id: u128, causation_id: u128,
    ) -> Result<AppendReceipt, StoreError> {
        let payload_bytes = rmp_serde::to_vec_named(payload)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;
        let event_id = crate::id::generate_v7_id();
        let header = EventHeader::new(
            event_id, correlation_id, Some(causation_id),
            now_us(), crate::coordinate::DagPosition::root(),
            payload_bytes.len() as u32, kind,
        );
        let event = Event::new(header, payload_bytes);

        let (tx, rx) = flume::bounded(1);
        self.writer.tx.send(WriterCommand::Append {
            entity: coord.entity_arc(), scope: coord.scope_arc(),
            event, kind, correlation_id, causation_id: Some(causation_id),
            respond: tx,
        }).map_err(|_| StoreError::WriterCrashed)?;

        rx.recv().map_err(|_| StoreError::WriterCrashed)?
    }

    /// READ: get a single event by ID.
    pub fn get(&self, event_id: u128) -> Result<StoredEvent<serde_json::Value>, StoreError> {
        let entry = self.index.get_by_id(event_id)
            .ok_or(StoreError::NotFound(event_id))?;
        self.reader.read_entry(&entry.disk_pos)
    }

    /// READ: query by Region.
    pub fn query(&self, region: &Region) -> Vec<IndexEntry> {
        self.index.query(region)
    }

    /// READ: walk hash chain ancestors. [SPEC:IMPLEMENTATION NOTES item 3]
    pub fn walk_ancestors(&self, event_id: u128, limit: usize)
        -> Vec<StoredEvent<serde_json::Value>>
    {
        let mut results = Vec::new();
        let mut current_id = Some(event_id);
        while let Some(id) = current_id {
            if results.len() >= limit { break; }
            if let Some(entry) = self.index.get_by_id(id) {
                if let Ok(stored) = self.reader.read_entry(&entry.disk_pos) {
                    results.push(stored);
                }
                /// Follow prev_hash: find the entry whose event_hash matches prev_hash
                let prev = entry.hash_chain.prev_hash;
                if prev == [0u8; 32] { break; } // genesis
                /// Linear scan is acceptable for ancestor walks (bounded by limit).
                current_id = self.index.stream(entry.coord.entity())
                    .iter()
                    .find(|e| e.hash_chain.event_hash == prev)
                    .map(|e| e.event_id);
            } else {
                break;
            }
        }
        results
    }

    /// PROJECT: reconstruct typed state from events.
    pub fn project<T: EventSourced<serde_json::Value>>(
        &self, entity: &str, _freshness: Freshness,
    ) -> Result<Option<T>, StoreError> {
        let entries = self.index.stream(entity);
        if entries.is_empty() { return Ok(None); }

        let mut events = Vec::with_capacity(entries.len());
        for entry in &entries {
            let stored = self.reader.read_entry(&entry.disk_pos)?;
            events.push(stored.event);
        }
        Ok(T::from_events(&events))
    }

    /// SUBSCRIBE: push-based, lossy.
    pub fn subscribe(&self, region: &Region) -> Subscription {
        let rx = self.writer.subscribers.subscribe(self.config.broadcast_capacity);
        Subscription::new(rx, region.clone())
    }

    /// CURSOR: pull-based, guaranteed delivery.
    pub fn cursor(&self, region: &Region) -> Cursor {
        Cursor::new(region.clone(), Arc::clone(&self.index))
    }

    /// CONVENIENCE: sugar over Region.
    pub fn stream(&self, entity: &str) -> Vec<IndexEntry> {
        self.query(&Region::entity(entity))
    }
    pub fn by_scope(&self, scope: &str) -> Vec<IndexEntry> {
        self.query(&Region::scope(scope))
    }
    pub fn by_fact(&self, kind: EventKind) -> Vec<IndexEntry> {
        self.query(&Region::all().with_fact(KindFilter::Exact(kind)))
    }

    /// LIFECYCLE
    pub fn sync(&self) -> Result<(), StoreError> {
        let (tx, rx) = flume::bounded(1);
        self.writer.tx.send(WriterCommand::Sync { respond: tx })
            .map_err(|_| StoreError::WriterCrashed)?;
        rx.recv().map_err(|_| StoreError::WriterCrashed)?
    }

    pub fn close(self) -> Result<(), StoreError> {
        let (tx, rx) = flume::bounded(1);
        self.writer.tx.send(WriterCommand::Shutdown { respond: tx })
            .map_err(|_| StoreError::WriterCrashed)?;
        rx.recv().map_err(|_| StoreError::WriterCrashed)?
    }

    /// DIAGNOSTICS
    pub fn stats(&self) -> StoreStats {
        StoreStats {
            event_count: self.index.len(),
            global_sequence: self.index.global_sequence(),
        }
    }
}

fn now_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as i64
}

#[derive(Clone, Debug)]
pub struct StoreStats {
    pub event_count: usize,
    pub global_sequence: u64,
}
```

>[mod.rs]

---

```
STORE MODULE REGISTRATION COMPLETE — 7 files registered.

Tests and benches pending registration:
  tests/monad_laws.rs, hash_chain.rs, store_integration.rs, gate_pipeline.rs,
  typestate_safety.rs, wire_format.rs, perf_gates.rs
  benches/write_throughput.rs, cold_start.rs, projection_latency.rs

Test/bench registration will follow the same pattern:
  IMPORTS → TYPES → IMPL → cross-references → end marker.
```
