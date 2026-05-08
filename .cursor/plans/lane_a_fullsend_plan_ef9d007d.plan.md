---
name: Lane A Fullsend Plan
overview: "Freeze the closure matrix as the anti-vibes contract, then run a two-step Lane A implementation arc: CanonicalArtifactEnvelope first, followed by CompactionReport plus prove-or-build decision gates for IdempotencyLedger and RegionBoundQuery."
todos:
  - id: freeze-matrix
    content: Finalize closure matrix dispositions and blockers in docs/extraction/evidence-substrate-audit.md with a lane kickoff note.
    status: pending
  - id: lane-a1-envelope
    content: Design and land CanonicalArtifactEnvelope with deterministic identity tests and docs.
    status: pending
  - id: lane-a2-compaction
    content: Design and land CompactionReport over existing compaction mechanics with structural proof tests.
    status: pending
  - id: decision-gate-ledger-query
    content: Resolve IdempotencyLedger and RegionBoundQuery via explicit prove-or-build gates and update matrix outcomes.
    status: pending
  - id: closure-gates
    content: Run full closure gates and confirm lane completion criteria.
    status: pending
isProject: false
---

# Lane A Fullsend Plan

## Arc Contract
- Treat [docs/extraction/evidence-substrate-audit.md](/home/heyoub/Documents/code/new_ver_sot/batpak/batpak/docs/extraction/evidence-substrate-audit.md) as the closure map source of truth.
- Every remaining noun must have: owner layer, proof path, next arc, and blocker.
- Keep the hard boundary: batpak owns generic substrate physics; higher layers own domain law.

## Phase 0: Matrix Freeze (No New Primitives Yet)
- Validate and finalize the refreshed Consumer Closure Matrix in [docs/extraction/evidence-substrate-audit.md](/home/heyoub/Documents/code/new_ver_sot/batpak/batpak/docs/extraction/evidence-substrate-audit.md):
  - dispositions only from the allowed set
  - no vague parked/future state
  - blocker text is concrete and falsifiable
- Add one short changelog note in the same doc tying matrix version to the next Lane A arc kickoff.

## Step 1: CanonicalArtifactEnvelope (Lane A-1)
- Add a minimal generic envelope surface in `src/store` (new module + re-exports through [src/store/mod.rs](/home/heyoub/Documents/code/new_ver_sot/batpak/batpak/src/store/mod.rs) and [src/prelude.rs](/home/heyoub/Documents/code/new_ver_sot/batpak/batpak/src/prelude.rs) only if pattern-matching by callers is required).
- Reuse the existing canonical identity contract (canonical bytes + deterministic hash) and avoid introducing protocol/domain vocabulary.
- Add deterministic tests for:
  - body-hash stability
  - metadata outside deterministic identity
  - reopen/readback invariants when persisted material is involved

## Step 2: CompactionReport + Decision Gates (Lane A-2)
- Implement deterministic compaction proof object based on existing compaction mechanics in [src/store/lifecycle.rs](/home/heyoub/Documents/code/new_ver_sot/batpak/batpak/src/store/lifecycle.rs) and existing compaction contract tests in [tests/store_snapshot_compaction.rs](/home/heyoub/Documents/code/new_ver_sot/batpak/batpak/tests/store_snapshot_compaction.rs).
- Keep scope structural: input range/source refs/outcome identity; no policy semantics.
- In the same arc, run explicit prove-or-build gates:
  - `IdempotencyLedger`: prove existing append/idempotency surfaces fully satisfy the requirement, or implement a minimal generic primitive.
  - `RegionBoundQuery`: prove existing query/read-walk surfaces already enforce bound discipline, or implement a minimal generic primitive.
- For each gate, produce one crisp disposition outcome in the matrix (implemented core vs rejected as redundant), with tests/doc evidence.

## Cross-Cutting Quality Gates Per Step
- Tests and docs travel with each primitive/change; no placeholder APIs.
- No speculative public enum states; no serde laundering; no fake uncertainty.
- Keep public API names domain-neutral.
- Run closure gates for each step before merge:
  - `cargo fmt --all --check`
  - targeted tests for new/changed surfaces
  - `cargo test --workspace --all-features`
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  - `cargo xtask docs`
  - `cargo xtask structural`
  - `cargo xtask ci`

## Done Criteria
- Matrix has no vague entries and every remaining noun has owner+proof+arc+blocker.
- Lane A-1 and Lane A-2 each land with deterministic tests and docs.
- `IdempotencyLedger` and `RegionBoundQuery` are each resolved by proof or implementation (not deferred wording).
- Repository passes full closure gates at the end of Lane A-2.