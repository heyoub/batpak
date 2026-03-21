# /raise-the-tide — Quadratic Audit & Fix Workflow

Find every quiet straggler, fix root causes not symptoms, improve the feedback
loop so the fix prevents recurrence, then verify the verification. Self-improving:
each run's findings become the next run's baseline.

## Arguments
- `$ARGUMENTS` — optional focus area (e.g. "concurrency", "error handling", "store module")

## Philosophy

1. **Don't just fix bugs — make fixing bugs create the immune system.**
   Every fix becomes a test. Every test validates a spec invariant. The test
   suite itself becomes a gate that prevents recurrence.

2. **Dogfood everything.** If the codebase has a Gate/Pipeline system, use it
   to test itself. If it has an event system, emit events about its own health.
   Quadratic feedback: the system tests itself with its own tools.

3. **Quiet stragglers won't make noise.** Look for pub functions with 0 tests,
   Display impls nobody exercises, flag systems nobody sets, classification
   predicates nobody calls, feature-gated code nobody runs, error paths that
   silently swallow, TOCTOU races hiding behind channels.

4. **Root cause over symptom.** When you find a bug, ask: why did this happen?
   Is the abstraction wrong (like flatten on Outcome<T> instead of
   Outcome<Outcome<T>>)? Can we compose behavior differently? Check the dev
   docs / spec — did someone design this and a lazy agent skip the wiring?

5. **The spec runs the spec.** If a SPEC.md exists, grep it for promises.
   Cross-reference against reality. Every unfulfilled promise is a straggler.

## Workflow

### Phase 1: Explore (parallel agents)

Launch 3 Explore agents in parallel:

**Agent 1 — Test Honesty Audit:**
Read every test file. For each test function rate it:
- STRONG: tests real behavior with content assertions
- WEAK: checks boolean/existence only, could pass with broken code
- BROKEN: proves nothing (e.g. tests arithmetic identity instead of actual behavior)
List all pub API methods with zero test coverage.

**Agent 2 — Feedback Loop Audit:**
Read benchmarks, CI config, coverage scripts, justfile.
- Are benchmarks measuring the right thing? Could they mislead?
- Are thresholds meaningful or "never fail"?
- Is the feedback loop fully wired? Where can regressions slip through?
- What does the spec promise that isn't delivered?

**Agent 3 — Quiet Straggler Hunt:**
Read every source file line by line.
- Silent error swallowing (unwrap_or_default, let _ =, filter_map ok)
- TOCTOU races (check-then-act across channel/thread boundaries)
- Feature-gated code with no test coverage
- Dead code (write-only fields, unused constants, unreachable paths)
- Invariants documented in comments but not enforced by types
- Trait impls that exist but are never exercised

### Phase 2: Triage

Classify findings into:
- **CRITICAL**: concurrency bugs, data corruption, silent data loss
- **HIGH**: dead/lying APIs, feature-gated untested code, misleading behavior
- **MEDIUM**: missing tests, weak assertions, ergonomic issues
- **LOW**: documentation drift, unused types, style

### Phase 3: Fix (run agents in parallel where independent)

For each finding, apply the fix hierarchy:
1. Can we **compose behavior better**? (e.g. move CAS check inside writer where the lock is)
2. Can we **make the type system prevent it**? (e.g. newtype wrapper, sealed trait)
3. Can we **add a test that would have caught it**?
4. Can we **add a gate that prevents recurrence**?

After fixing, add each fix to the self-benchmark correctness gates so the
feedback loop catches future regressions of the same class.

### Phase 4: Verify the verification

- Run `cargo test --all-features` (must pass)
- Run `cargo test --no-default-features` (feature isolation)
- Run `cargo test --features redb` and `--features lmdb` if applicable
- Run `cargo clippy --all-features` (zero warnings)
- Check: did we add any `#[allow]` annotations? If yes, justify or remove.
- Check: do all new tests assert real behavior, not just booleans?
- Check: are the self-benchmark gates still green?

### Phase 5: Update the feedback loop

- If we found a class of bug, add a gate that catches that class
- If we found untested code, add it to the coverage expectations
- If we found misleading benchmarks, fix the measurement
- Update dev docs / SPEC with what we learned
- This skill file itself: did we learn a new pattern? Add it to Phase 1 or 3.

## Lessons Learned (append after each run)

### PR session 01QB53p7zDAw5Fc2Wrh4zpBd

- `#[allow(clippy::*)]` is always a smell. Every one we removed revealed a
  real composition improvement (WriterState struct, closure-owns-Arc pattern).
- `flatten()` on `Outcome<T>` with `T: Into<Outcome<T>>` was wrong abstraction.
  Correct: `impl<T> Outcome<Outcome<T>> { fn flatten() }`. Category theory
  tells you where the method belongs: join = bind(id), on the nested type.
- `Reactive<P>` looked like dead code but was SPEC-mandated. Always check
  dev docs before deleting — a lazy agent may have skipped the wiring.
- Cursor off-by-one: `global_sequence` starts at 0, cursor started at
  position 0 with `> self.position`. Fix: `started` flag. Pattern: any
  "start after this position" with 0-based sequences needs a sentinel.
- CAS and idempotency checks outside the writer lock = TOCTOU. Pattern:
  check-then-act across a channel boundary is never atomic. Move the check
  to where the lock is held.
- `_cache` field with leading underscore = dead code hiding in plain sight.
  Pattern: search for `_` prefixed fields — they're telling you something.
- Tests that assert `<= N` instead of `== N` are weak. Tests that discard
  results (`let _ = results.len()`) are broken. Tests that check `len > 5`
  instead of content are weak. Always assert the strongest property you can.
- `walk_ancestors` with all-zero hashes (no blake3) matches every event.
  Pattern: when a feature flag degrades a discriminator to a constant,
  algorithms that depend on uniqueness break silently.
