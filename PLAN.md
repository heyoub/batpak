# Round 2: Audit Loop Hardening Plan

## Context

Round 1 implemented Big Bang Protocol compliance: build-time guards, lint hardening,
8 compliance tests, EventKind category guards. Four audit agents completed with findings.
This plan addresses the remaining gaps from both the Big Bang Protocol and the
Codebase Audit Loop, organized by impact.

## Current State (Post Round 1)

- 267 tests pass, 0 clippy warnings, 5 build.rs guards, 5 bench suites
- Big Bang compliance: 11 tests covering LAW-003/007, FM-007/022/023, INV-TYPE/TEMP/CONC/SEC
- Gaps identified by 4 audit agents remain unaddressed

---

## Phase 1: High-Impact Structural Fixes (from audit findings)

### 1A. Self-Benchmark Gate Expansion (INV-PERF gap — audit found only cold_start uses gates)
**File:** `tests/self_benchmark.rs`
**What:** Add GateSet-based performance gates for write throughput and projection replay,
not just cold_start. The self_benchmark file already has the pattern — extend it.
- Add `append_throughput_gate` that measures single-entity append rate, gates at reasonable floor
- Add `projection_replay_gate` that measures replay latency, gates at threshold
- These use the library's own Gate system (dogfooding per LAW-007)

### 1B. DagPosition PartialOrd Fix (from previous audit — still pending)
**File:** `src/coordinate/position.rs`
**What:** The current PartialOrd for DagPosition ignores depth, only comparing lane+sequence.
Two positions on the same lane but different depths should be comparable.
- Fix PartialOrd to include depth in comparison
- Add test verifying depth comparison works

### 1C. Store Drop Improvement (from previous audit — Drop doesn't wait for writer)
**File:** `src/store/mod.rs`
**What:** Store::drop sends Shutdown but doesn't wait. Data loss possible if writer
has pending events. Add a bounded wait (e.g., 100ms) in Drop to give the writer
time to drain.

---

## Phase 2: Feedback Loop & Audit Loop Compliance

### 2A. Traceability Comments (LAW-006 gap — tests exist but aren't mapped to laws)
**Files:** All test files
**What:** Add header comments to each test file mapping tests to the laws/invariants/FMs
they prove. Not a separate doc — inline in the test files themselves. Example:
```rust
// PROVES: LAW-003 (No Orphan Infrastructure), FM-007 (Island Syndrome)
// INVARIANTS: INV-TYPE-001 (Coordinate round-trip)
```
This closes the LAW-006 (Bidirectional Traceability) gap without creating a separate
doc that goes stale.

### 2B. Audit Pass Detectors in build.rs (Audit Loop Layer 2 enforcement)
**File:** `build.rs`
**What:** Add compile-time detectors for the most dangerous AI smells:
- **Stub detector**: Scan for `todo!()`, `unimplemented!()` in non-test src/ files
  (already denied by clippy, but build.rs gives better error messages with file:line)
- **Orphan pub fn detector**: Scan for `pub fn` and `pub struct` in src/ and verify
  each appears in at least one test file (lightweight Pass 5 proxy)

### 2C. Benchmark Regression Signal (Audit Loop feedback loop)
**File:** `tests/self_benchmark.rs`
**What:** Add a meta-test that runs after the benchmark gates. If any gate produces
a Denial, emit a structured diagnostic with investigation path. This closes the
"findings -> work items" loop from the Audit Loop spec.

---

## Phase 3: Algebraic Property Tests (from invariant taxonomy audit)

### 3A. Commutativity Test (audit found only 40% coverage)
**File:** `tests/bigbang_compliance.rs`
**What:** Add test verifying that appending events A then B to different entities
produces the same index state as appending B then A. This is the commutativity
property for independent entity streams.

### 3B. Closure Test (audit found 70% coverage)
**File:** `tests/bigbang_compliance.rs`
**What:** Add test verifying that Outcome combinators always produce valid Outcome
values — map, and_then, zip never escape the Outcome type. This is already partially
tested in monad_laws.rs but deserves an explicit algebraic identity test.

### 3C. Totality Hardening (audit found EventSourced not tested for unhandled kinds)
**File:** `tests/bigbang_compliance.rs`
**What:** Add test that feeds an unknown EventKind through a projection and verifies
it doesn't panic — the projection should ignore unknown kinds gracefully.

---

## Phase 4: Quick Wins (low effort, high signal)

### 4A. Wire Format Golden File (audit recommended, prevents silent serde drift)
**File:** `tests/wire_format.rs` (already has golden tests — verify SHA-256 hashes)
**What:** Add content-hash assertions to existing golden tests. If the msgpack encoding
changes, the hash changes, and the test screams. Currently the golden tests check
structure but not byte-level determinism.

### 4B. Disallowed Methods Expansion (clippy.toml)
**File:** `clippy.toml`
**What:** Add more disallowed methods based on post-mortem learnings:
- `std::process::exit` → "Use proper error propagation, not process::exit"
- `std::mem::forget` → "Leaking resources is a bug, not a feature"

### 4C. Error Variant Coverage Test (FM-011 Error Path Hollowing defense)
**File:** `tests/bigbang_compliance.rs`
**What:** Add test that verifies every StoreError variant can be constructed and has
a non-empty Display message. This ensures no error variant is hollow.

---

## Execution Order

1. Phase 1A (self_benchmark gates) — highest impact, closes INV-PERF gap
2. Phase 1B (DagPosition fix) — correctness bug from prior audit
3. Phase 1C (Store Drop wait) — data safety improvement
4. Phase 2A (traceability comments) — closes LAW-006 gap
5. Phase 2B (build.rs detectors) — audit loop enforcement
6. Phase 3A-3C (algebraic tests) — closes invariant coverage gaps
7. Phase 4A-4C (quick wins) — low-hanging fruit
8. Phase 2C (benchmark regression signal) — feedback loop closure

## Estimated Scope
- ~10 files modified
- ~200 lines of new tests
- ~50 lines of build.rs additions
- ~30 lines of production code fixes (DagPosition, Store Drop)
- No new dependencies

## Validation
After all changes: `cargo test`, `cargo clippy --all-features -- -D warnings`,
`cargo bench` (spot-check). All 267+ tests must pass, zero warnings, zero new
clippy issues.
