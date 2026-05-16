# Testing Doctrine

This file defines the operational testing doctrine for `batpak`.

This doctrine is enforced for doctrine-bearing harnesses listed in
`041_TESTING_LEDGER.md` by `cargo xtask structural` (and therefore by
`cargo xtask ci`). Legacy exceptions must appear in the integrity tool's
explicit debt allowlists with a line cap, reason, and shrink target.
Allowed `Command used:` entries are deliberately narrow: `cargo test`,
`BATPAK_RUN_CHAOS=1 cargo test`, `CARGO_INCREMENTAL=0 cargo mutants`, or
`cargo xtask ...`. Extending that prefix set is a structural-lint policy
change, not a per-entry escape hatch.

The goal is not to maximize test count or chase a vanity coverage number. The
goal is to increase proof density: more decisions covered per harness, clearer
receipts for what each harness proves, and fewer story tests that only exercise
one happy path.

## Non-Negotiables

Every new doctrine-bearing harness must satisfy all of these:

- deterministic
- no network, Docker, or external services
- fail-closed: if the harness cannot decide, it fails instead of skipping
- runnable under `cargo xtask ci` or `cargo xtask preflight`
- no production rewrites for testability without explicit approval
- no new test framework dependencies
- if a harness would exceed 500 lines, split it by seam or evidence shape

## Required Module Header

Every new doctrine-bearing harness module must declare:

- `PROVES:` the invariant it proves
- `CATCHES:` the failure mode it is meant to surface
- `SEEDED:` how it is seeded if random, or `deterministic / no randomness`

Keep these declarations at module scope so a future reader can understand why
the file exists without spelunking.

## The Five Allowed Harness Patterns

These are the only repo-owned harness patterns. When a suite is hard to place,
classify it by the **evidence shape**, not by the subsystem under test.

### 1. Oracle Harness

Use this when a simple, obviously-correct implementation can act as the source
of truth for a more optimized or specialized path.

Typical shape:

- simple scan/filter oracle
- optimized topology or index path
- compare outputs exactly

Use this for:

- index topology parity
- query engines
- optimized filtering paths

### 2. Property Harness

Use this when randomized or enumerated input families are checked against one
or more invariants, and the strength comes from breadth of cases rather than a
single narrative.

Typical shape:

- seeded randomness or a generated matrix
- invariant assertions over every case
- deterministic replay of failures

Use this for:

- fuzz-style invariants
- catastrophic regression thresholds
- generated edge-matrix coverage

### 3. State-Machine Harness

Use this when the thing being proved is a protocol, lifecycle, or bounded
schedule whose valid transitions matter as much as the final output.

Typical shape:

- explicit steps or actions
- asserted state after each transition
- lifecycle or interleaving invariants

Use this for:

- writer command flow
- cursor lifecycle
- loom schedule proofs
- restart / rollback / drain protocols

### 4. Equivalence Harness

Use this when multiple code paths claim to produce the same semantics and the
harness proves they stay aligned.

Typical shape:

- same semantic input
- different lanes, replay modes, or artifact paths
- assert identical visible result

Use this for:

- hand-written vs derived parity
- raw-msgpack vs json replay lanes
- live vs reopen / rebuild / mmap parity
- `project`, `project_if_changed`, and watcher convergence

### 5. Fault-Injection Harness

Use this when the harness injects bad input, corruption, environmental damage,
or illegal surface shapes and proves the runtime or macro fails structurally.

Typical shape:

- corrupt bytes, torn artifacts, broken contracts, or invalid derive input
- structured failure or safe fallback
- no phantom success

Use this for:

- compile-fail suites
- corruption recovery
- chaos testing
- truncated or CRC-mismatched artifacts

## Classification Rule

Classify each doctrine-bearing suite by its primary evidence shape:

- compile-fail suites are usually `Fault-Injection Harness`
- derive or path parity suites are usually `Equivalence Harness`
- loom and protocol lifecycle suites are usually `State-Machine Harness`
- fuzz and generated invariant matrices are usually `Property Harness`
- corruption and recovery probes are usually `Fault-Injection Harness`
- catastrophic perf gates are treated as a `Property Harness`, because they are
  threshold invariants, not precision benchmarking

One file gets one primary pattern in the ledger, even if it incidentally
touches a second one.

## Promotion Rule

A suite belongs in [`041_TESTING_LEDGER.md`](041_TESTING_LEDGER.md) when it:

- proves a named invariant or boundary contract
- would leave a real proof hole if deleted
- is stable enough to be run intentionally
- points back to a concrete runtime seam or public contract

## Maintenance Rule

When a doctrine-bearing suite changes:

1. keep its primary harness pattern unless the evidence shape truly changed
2. update the matching entry in [`041_TESTING_LEDGER.md`](041_TESTING_LEDGER.md)
3. update the module header if the invariant, failure mode, or seed policy changed

Do not invent a sixth harness pattern because a suite feels special. Tighten
the existing five instead.
