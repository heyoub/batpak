---
name: Lane A Fullsend Plan
overview: "Preflight: refresh evidence-substrate-audit.md (fullsend matrix). Lane A delivers CanonicalArtifactEnvelope (crate-level), CompactionReport (store), then mandatory prove-or-build gates for IdempotencyLedger and RegionBoundQuery—with doctrine tests, HARNESS_LEDGER, and structural. Full xtask CI deferred until all scopes land."
todos:
  - id: preflight-audit-doc
    content: "Update docs/extraction/evidence-substrate-audit.md: matrix version/changelog, allowed dispositions only, Lane A1–A4 + gates, remove any stale 'parked' / bench-gap / lint-debt language; mark A1/A2 implement core, A3/A4 prove-or-build."
    status: pending
  - id: lane-a1-artifact
    content: "A1: src/artifact.rs CanonicalArtifactEnvelope + verification types/tests; no store dep; intentional prelude re-exports only."
    status: pending
  - id: lane-a2-compaction
    content: "A2: store-owned CompactionReport + wiring from lifecycle.compact; tests aligned with store_snapshot_compaction."
    status: pending
  - id: lane-a3-idempotency-gate
    content: "A3: prove IdempotencyLedger redundant vs append idempotency + index, or implement minimal core primitive; matrix disposition + tests/docs."
    status: pending
  - id: lane-a4-region-gate
    content: "A4: prove RegionBoundQuery redundant vs public query/cursor discipline, or implement minimal helper; matrix disposition + tests/docs."
    status: pending
  - id: harness-structural
    content: "New doctrine tests get PROVES/CATCHES/SEEDED; update HARNESS_LEDGER.md; cargo xtask structural must pass."
    status: pending
  - id: closure-commands
    content: "Run fmt, targeted tests, workspace test, clippy, xtask docs, structural, evidence_reports bench --no-run if touched. Skip cargo xtask ci / mutants until all Lane A scopes done (long runs)."
    status: pending
isProject: false
---

# Lane A Fullsend Plan (Composer Execution Rail)

## Mode (how to work)

- No PM slicing, no vague parking, no “maybe later.”
- Every noun in the matrix gets **owner + invariant + proof path**.
- Do **not** stop between substeps for permission unless: an invariant conflicts with existing code; a public API needs a breaking shape; a test disproves the design; or implementation requires non-generic / domain vocabulary.

## Arc contract

- Source of truth: [docs/extraction/evidence-substrate-audit.md](docs/extraction/evidence-substrate-audit.md) (closure matrix + changelog).
- Evidence family stays **sealed** (no new report families this arc).
- Boundary: batpak owns **generic substrate physics**; higher layers own domain law.

---

## Preflight (before code): audit doc

1. Confirm [docs/extraction/evidence-substrate-audit.md](docs/extraction/evidence-substrate-audit.md) matches the **fullsend** matrix (no vague “parked” final states).
2. Remove or rewrite **stale** language if present:
   - “Parked-Item Promotion Gate”
   - old “known bench gap” for topology-param evidence benches (if closed: say landed)
   - old platform lint debt (if closed: say evidence-debt-zero closed)
   - macro rows using “Park” as final status — replace with allowed dispositions below
3. **Allowed disposition labels only:**
   - `already covered`
   - `implement in batpak core`
   - `implement in batpak tooling/helper`
   - `implement above batpak`
   - `reject / not needed`
4. Add **matrix version / changelog** line for Lane A kickoff.
5. Implementation waves: **A1** → **A2** → **A3** (prove-or-build) → **A4** (prove-or-build).

---

## Hard invariants (all Lane A work)

- No fake uncertainty; no serde laundering; no placeholder APIs.
- No speculative public enum variants.
- No domain / protocol / deployment / product vocabulary in public names.
- **Metadata never changes canonical body identity** for artifact/body hashing.
- Report/envelope findings **deterministic and sorted** where the type promises ordering.
- Public API names: **generic substrate** vocabulary only.
- Doctrine-bearing tests: **PROVES / CATCHES / SEEDED** headers + **HARNESS_LEDGER.md** entry when proving a named invariant.
- **`cargo xtask structural` must pass** (includes harness ledger lint).

---

## A1 — `CanonicalArtifactEnvelope` (crate-level)

**Placement**

- **Must** be crate-level generic substrate: default **[src/artifact.rs](src/artifact.rs)** (or split only if a strong reason appears).
- **Do not** place under [src/store](src/store) unless the implementation genuinely depends on store internals (it must not for v1).
- Re-export from [src/lib.rs](src/lib.rs) / [src/prelude.rs](src/prelude.rs) **only if intentionally** caller-facing.

**Public contract**

- `body_hash` = hash(canonical **body** bytes only).
- `envelope_hash` = hash(canonical envelope: metadata + signatures + attestations; includes `body_hash` anchor, not raw body twice).
- Adding signature/attestation metadata changes `envelope_hash`, **not** `body_hash`.
- Verification artifact/report is **deterministic**.
- `generated_at` / diagnostics / non-identity metadata **outside** body hash (envelope-only).
- v1: **no** COSE / SLSA / protocol-registry coupling.
- Signature/attestation: **generic refs** or **opaque byte envelopes** only.

**Roles (names may vary)**

- `CanonicalArtifactEnvelope<T>`
- `ArtifactVerificationReport` (or `ArtifactVerificationEvidenceReport`)
- `SignatureEnvelope` / `SignatureRef`
- `AttestationRef`
- `ArtifactEnvelopeFinding`

**Tests (minimum)**

- Same body, no signatures → same `body_hash`.
- Same body, added signature → same `body_hash`, different `envelope_hash`.
- Metadata order does not change canonical identity (envelope canonicalization sorts / fixed struct).
- Invalid signature → **structured deterministic** finding/report.
- Verification report `body_hash` deterministic.
- `generated_at` / diagnostics do not affect `body_hash`.
- **No store import** in artifact module.

---

## A2 — `CompactionReport` (store-owned)

**Placement**

- Store-owned; live near compaction ownership ([src/store/lifecycle.rs](src/store/lifecycle.rs), new module e.g. `compaction_report.rs`).
- Align with [tests/store_snapshot_compaction.rs](tests/store_snapshot_compaction.rs).

**Public contract**

- Structural evidence only: **no** retention policy, legal deletion, or domain semantics.
- Explicit **input range** / **source refs** (sorted deterministically).
- Explicit **output** segment/hash identity where available.
- **Findings** structural, sorted deterministically.
- Same logical compaction inputs → **same** report body (for a defined “logical” tuple documented in code).

**Fields (shape-flexible)**

- `schema_version`
- stable report id (`compaction_id` or hash-of-structural-inputs)
- input range / frontier / `source_refs` as appropriate to segment model
- output ref / output hash / segment refs
- `findings`
- `body_hash` via existing evidence/canonical pattern ([src/evidence.rs](src/evidence.rs) helpers)

**Tests (minimum)**

- Deterministic `body_hash` for same compaction evidence.
- Input refs sorted.
- Output hash stable when segment bytes stable.
- Same logical compaction → same report body.
- Corruption/incomplete compaction path → structured finding(s).
- No policy/domain vocabulary in **public** names.

**API note**

- Prefer `compact_with_report` or equivalent that returns report alongside [CompactionResult](src/store/segment/mod.rs) without breaking existing `Store::compact` callers (or extend in a non-breaking way).

---

## A3 — `IdempotencyLedger` (prove-or-build — **mandatory** closure)

Must end as either **`already covered` / `reject / not needed`** with proof **or** **`implement in batpak core`**.

**Discovery**

- [AppendOptions::idempotency_key](src/store/append.rs), writer append path, batch idempotency, duplicate/prior receipt lookup, [StoreError](src/store/error.rs) idempotency variants, existing tests.

**Proof questions (answer in docs/matrix)**

- Stable key exists? Scope/coordinate context? First receipt ref observable? Duplicate observable? TTL/expiry in contract or explicitly absent? Replay lookup evidence? Deterministic inspection/report surface?

**If redundant**

- Update matrix; add/extend tests + short doc proving the existing append path is the substrate.

**If implement**

- Minimal generic ledger/report types; **no** operation/protocol semantics, replay-response policy, TTL (unless already in store semantics), domain nouns.

**Tests either way**

- First-key append stores/returns first receipt path; duplicate key returns prior receipt deterministically; batch behavior pinned; no fake success on mismatch; deterministic evidence if a report type exists.

---

## A4 — `RegionBoundQuery` (prove-or-build — **mandatory** closure)

Must end as either **`already covered` / `reject / not needed`** with proof **or** minimal **`implement in batpak core`** helper/type.

**Discovery**

- Public `query` / `by_scope` / `stream` / `by_fact`, [cursor_guaranteed](src/store/mod.rs), read-walk source refs, internal-only scans, topology tests.

**Proof questions**

- Do public query/cursor/replay paths **structurally** require `Region` or other explicit bounds (entity/scope/kind)?
- Are truly unbounded scans internal-only?
- Can evidence name region/predicate/limit deterministically?
- Ordering independent of topology/storage iteration for comparable queries?

**If redundant**

- Matrix + focused tests/docs.

**If implement**

- Minimal `RegionBoundQuery` (or equivalent): `region`, predicate hash/id, optional `limit`, ordering mode; no tenant/capability vocabulary; canonical deterministic form.

**Tests**

- Public bulk-read discipline documented; cursor/replay bound explicit; read-walk source refs deterministic where applicable; topology-independent ordering where promised; no accidental public unbounded-scan helper without explicit contract.

---

## Harness / QA

- Doctrine-bearing tests: **PROVES**, **CATCHES**, **SEEDED** (per repo harness directive).
- Update [HARNESS_LEDGER.md](HARNESS_LEDGER.md) for new suites.
- `cargo xtask structural` **must** pass.

---

## Commands (order)

1. `cargo fmt --all --check`
2. Targeted tests for A1–A4
3. `cargo test --workspace --all-features`
4. `cargo clippy --workspace --all-targets --all-features -- -D warnings`
5. `cargo xtask docs`
6. `cargo xtask structural`
7. `cargo bench --bench evidence_reports --no-run` **if** benches touched
8. **`cargo xtask ci`** — **defer** until all Lane A scopes complete (long/mutants runs). Run once at end of fullsend rail unless policy changes.

---

## Stop conditions (do not merge if)

- Speculative public enum variants or placeholder APIs.
- API names import domain/protocol/deployment vocabulary.
- `body_hash` changes when **only** non-body metadata changes (artifact contract violated).
- Report/envelope ordering depends on map/storage/layout iteration without documented canonical order.
- Idempotency or query discipline cannot be proven **and** no implementation is added.
- `cargo xtask structural` fails.

---

## Done criteria

- Matrix: no vague entries; every row has owner + proof path + arc + blocker.
- A1–A2 shipped with tests + docs; A3–A4 each **resolved** (proof or implementation).
- HARNESS_LEDGER + structural green for new doctrine tests.
- fmt / workspace tests / clippy / docs pass; bench compile if applicable.
- Full `cargo xtask ci` run when deliberately closing the rail (not per micro-step if timeboxed).
