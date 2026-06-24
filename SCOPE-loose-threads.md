# SCOPE — Loose Threads to 0.9.0 (implementation-ready)

> Read-only mapping pass. Hand this to Cursor. Every section is implementation-ready: scope, AL, files (`path:line`),
> exact changes, **red + green fixtures**, gate wiring, sequencing, done-when. No source was edited producing this.
>
> Repo facts pinned at authoring (branch `feat/0.9.0-integration`, 120 ahead of `main`):
> - Current version is **0.8.3** (`bpk-lib/crates/core/Cargo.toml:3`). PR #121 already did the `0.8.2→0.8.3` narration bump; the milestone-end jump is **0.8.3 → 0.9.0**.
> - Gauntlet law (enforced, not aspirational): every blocking gate names an **existing, anti-vacuous red fixture**; gates emit **non-vacuous receipts** (`files_examined>0`, `assertions_run>0`); criticality is **L0–L4**; the AL manifest's L3∪L4 glob set must **equal exactly** the union of `*_MUTANT_FILES` in `lanes.rs` (lockstep test).
> - **Hard rule:** mutation/fuzz/coverage are **cloud-only**. Do NOT add a mutation seam and run it locally; seams are declared in `lanes.rs` and exercised on the cloud lane.
> - **Zero `#[allow]`** in non-test source (build.rs + structural enforce). **Zero typed waivers** today (`typed_waivers.yaml` empty). `panic`/`unwrap_used` denied workspace-wide; non-test uses `Result`+`map_err`/`expect`-banned.

---

## Prioritized index

| # | Thread | Why it matters | Size | Blocks the cut? |
|---|--------|----------------|------|-----------------|
| **48** | Cure #121 fork/import under the gauntlet, then bump 0.8.3→0.9.0 | The cut itself. fork/import are L4 (durability/idempotency/hash-chain) surfaces that no offensive family currently attacks; they ship behind 2 cloud mutation seams + happy-path proof tests only. | **L** | **YES — it *is* the cut** |
| **64** | Mutation-as-divergence + DST corpus (hybrid AL-graded engine) | Today mutation (score%) and DST (determinism digest) are disjoint; surviving mutants go to a manual markdown debt log; DST runs single seeds with no corpus. Closes the "a survivor is a finding" loop and turns DST into a growing oracle. | **L** | **YES — no-deferral (was post-cut)** |
| **67** | Claim-vs-reality / "half-baked-intent" hunter | A new detector family that catches over-claims (gate claimed blocking-but-never-red, fn named X that doesn't do X, doc property with no witness). This is the *meta* defense against exactly the deferral-debt this scope itself documents. | **M** | **YES — no-deferral (was "should")** |
| **68** | Dedup examples/templates/cookbook → one canonical runnable per concept | 23 examples + 18 cookbook docs + 10 templates with real duplication (typed-append ×3, durability ×5) AND real gaps (fork/import/lane have docs+templates but **no runnable example**). | **M** | **YES — no-deferral (full dedup, not just gap-fill)** |

> ## ⛔ OWNER DIRECTIVE (supersedes every "post-cut" / "No" / "deferred" label below)
> **Nothing is deferred. Everything is pre-cut.** Per the standing kitchen-sink rule: 0.9.0 = build-complete first → **one** heavy-validation batch → cut only on explicit say-so. So **#48, #64 (A–D), #67 (full, incl. the doc-ratchet + name-vs-behavior fixture), #68 (full dedup, not just the gap-fill), and the discovered threads D3–D11 are ALL cut blockers.** The per-thread "Blocks the cut?" / "post-cut" notes below are VOID — read them as scope, not sequencing.
>
> ### The cascade this triggers (the non-obvious part)
> "Nothing deferred" + "no over-claim" means **you cannot ship a thread *honestly bounded* — the bound itself is a deferral.** So the bounds become work:
> - **SIM-2c: the scary part is ALREADY DONE (verified 2026-06-23) — D6's framing is stale.** The writer is already a state machine (`write/writer.rs` `WriterCore::drive_command` → `DriveStep`) behind a `WriterDrive::{Threaded, Cooperative}` seam (#63 Stage 1-2). The deadlock concern is gone: `pump()` (writer.rs:158) drains via **`while let Ok(cmd) = self.rx.try_recv()`** — non-blocking, NOT `rx.iter()` — and `pump()` is already wired through the **entire** writer surface incl. the **append path** (`write_api.rs:41`), sync, visibility-fence, fork, snapshot, open. So "make the writer cooperative" is **complete**, not a multi-week refactor. SIM-2c's *actual* remnant is bounded: (a) wire the **SimFs fault schedule** (fsync-drop / torn / ENOSPC) into the writer's routed disk ops so the recovery oracle can run `fsync_drop_one_in > 0` (today 0 = honest disk only — this is really SIM-2b for the writer's segment/sync cluster), and (b) *optionally* order the pump from the `SimScheduler` FIFO (the explicit-pump-at-callsite already gives deterministic single-thread ordering, so this may be unnecessary). Re-estimate SIM-2c as **bounded fault-routing**, not a writer rewrite.
> - **Fork is quiesced-by-construction in the DST world — SIM-2c does NOT gate fork.** Fork = `lifecycle_gate.lock()` (serialize lifecycle ops) → `begin_visibility_fence()` → `sync()` (drains the writer via `pump()`, flushing all queued appends to disk) → `idemp.flush()` → record watermark → copy on-disk files → `fence.cancel()`. In cooperative/DST mode the writer **cannot advance without a `pump()` call, and fork issues none during its copy loop** (lifecycle.rs:213–231) — so no append can interleave with the copy. Fork's only hazard is **I/O faults during the sequential copy = SIM-2b** (route fork's `cow_copy_file`/`reject_symlink_leaf`/`canonicalize`/`symlink_metadata` through `StoreFs` → `SimFs`). Concurrent-append-during-fork is not a real DST race here. Same logic for import (drains + idempotent re-apply).
> - **D8 typed `*FutureVersion` errors must be created** so the 4 deferred compat-matrix store-format rows (idempotency/visibility/checkpoint/segment) can land — they're blocked on those errors existing.
> - **D5 witness-test burndown** (79 prose/weak INVs → strong tier + MODEL.md→symbol bindings) lands, so #67's doc-property-without-witness check runs without a known-debt carve-out.
> - **D7 (4 missing triangulation oracles + DEPENDENCY-DIRECTION gate)**, **D9 (Repo-IR fitness → blocking + thin columns)**, **D10 (react_loop → opaque handle, in the pre-1.0 API-break window)**, **D11 (SimClock core-export)** all land.
> - **D4** (`RecordOnly` dead variant) gets wired by #64-A.
>
> ### Two things "no deferral" canNOT make instant — plan around them
> - **D3 is irreducibly cloud-dependent:** the repo-wide mutation confirm + #127 survivor cure needs the cloud lane (no local heavy lanes). It's part of the single heavy-validation batch, not something to "finish" locally.
> - **The kernel (bvisor track A) stays DECOUPLED from this cut.** `bvisor` is `publish = false`, so it is NOT part of the 0.9.0 release surface; its shadow→promote→bind-identity work is a separate track on the same branch, gated on its own cloud QF_BV proof. "Everything pre-cut" = everything in the **0.9.0 release surface (core/store + the gauntlet that gates it)**, which `publish=false` bvisor is not in. (Confirm if you actually want the kernel in 0.9.0 — default reading: no.)
>
> ### D1 and D2 status corrections (both resolved/reclassified since the doc was written)
> - **D1 is RESOLVED, not a blocker.** Verified: `cargo clippy -p batpak --tests` is **clean** — the `single_element_loop` is gone (compat formats expanded). No commits are being bypassed on this account.
> - **D2 is a TASK-LABEL over-claim, not a code one.** The bvisor code is honestly status-marked (`shadow.rs` says H_A binds only post-promotion); task #71's tracker label was corrected. **#67 will NOT fire on it** (it scans code/docs/gates, all honest here), so D2 does not couple this cut to the cloud proof.
>
> ### Unified pre-cut sequence (one ordering, build-complete → validate → cut)
> 1. **Foundations:** `lifecycle.rs` function-split (unblocks the ratchet) → SIM-2b (route fork through StoreFs) → **SIM-2c (writer on SimScheduler — the hard one)** → D10 react_loop opaque handle → D11 SimClock export.
> 2. **Detectors/infra:** #67 full (gate-over-claim + name-vs-behavior + doc-witness) → D7 triangulation oracles + dependency-direction gate → D9 Repo-IR blocking → D5 witness-test burndown.
> 3. **Offensive + features:** #48 fork/import offensive fixtures (now on the full SimFs+scheduler seam) → D8 typed FutureVersion errors → all 6 compat-matrix rows → #64 (A–D, AL-graded, on the now-unbounded corpus) → #68 full dedup + fork/import/lane examples.
> 4. **One heavy-validation batch (cloud):** repo-wide mutation smoke (confirm Phase4 or drop), seam floors ≥85%, equivalence-mutant audit, D3 #127 survivor cure, fuzz/DST sweeps.
> 5. **Cut:** bump 0.9.0, seal `public_api/batpak.txt`, CHANGELOG, chain-publish, tag — only on explicit say-so.

**(Superseded — historical:** Cut-readiness summary: #48 is the only true blocker; #67/#64/#68 post-cut-acceptable.)**

---

## Thread #48 — Cure #121 fork/import under the gauntlet, then bump 0.8.3 → 0.9.0

### Scope / why
PR #121 adds `Store::fork` / `Store::fork_with_evidence` and `Store::import_events`. These are **L4** surfaces (durability, idempotency via `for_operation`, per-entity hash-chain regeneration, crash boundaries). Current coverage:
- Two **cloud mutation seams**: `fork-isolation`, `import-reapply` (`lanes.rs:647-662`).
- Happy-path + isolation proof tests (`tests/store_fork.rs`, `store_fork_isolation.rs`, `import_events.rs`, `import_events_accessors.rs`, `isomorphism_laws.rs`).

**The gap:** none of the *offensive* families (hostile `SimFs`/`StoreFs`, recovery oracle, compat-matrix, semantic-diff, fault injection) is pointed at fork or import. fork does multi-file CoW copy + symlink rejection + self-fork canonicalization + visibility-fence cancel; import does paginated re-apply with a same-store ceiling guard and durable dedup. Each of those is a crash/fault/adversarial surface with **zero hostile coverage**. Per the gauntlet's "real failure → cure → survives N seeded hostile runs" loop, the cut is not earned until fork/import survive the offensive tier.

### Assurance level
**L4** (both surfaces). They must appear in `assurance_levels.yaml` at L4 with their seam slugs — verify `fork-isolation` and `import-reapply` are present and that the lockstep test (`assurance.rs`) passes (the L3∪L4 glob set == `FORK_MUTANT_FILES ∪ IMPORT_MUTANT_FILES`).SCOPE-loose-threads.md (1-365)

- Fork API: `bpk-lib/crates/core/src/store/lifecycle_api.rs:284-300`; algebra entry `lifecycle_fork.rs:53-130`
- Import core loop: `bpk-lib/crates/core/src/store/import.rs:239-339`; **same-store ceiling guard `:255` + `:268-273`**; idempotency key `:341-347`; provenance `:196-217`
- Import API: `bpk-lib/crates/core/src/store/import_api.rs:17-24`
- Seams: `bpk-lib/tools/xtask/src/commands/mutants/lanes.rs:647-662` (`fork-isolation`, `import-reapply`); equivalence exclusions `:186-191` (import) and `:220-222` (fork)
- Offensive families to point at fork/import:
  - `SimFs` (real-file fault FS w/ `crash()`): `bpk-lib/crates/core/src/store/sim/fs.rs`
  - In-mem fault model: `sim/fault_model.rs` (`InMemFaultFs`, torn-write/short-read/fsync-drop, 18 `InjectionPoint` variants)
  - Recovery oracle: `sim/recovery.rs`; matrix `sim/recovery_matrix.rs` (`FaultMode`, 4 boundaries)
  - Fault trait: `bpk-lib/crates/core/src/store/fault.rs:391` (`maybe_inject`), injection points `:36-182`
  - Recovery-oracle test: `bpk-lib/crates/core/tests/recovery_oracle.rs` (`INV-RECOVERY-ORACLE-LEGAL`)
  - Semantic-diff: `bpk-lib/crates/core/tests/semantic_diff.rs` (`INV-SEMANTIC-DIFF-EQUIVALENCE`)
  - Compat-matrix: `bpk-lib/crates/core/tests/compat_matrix.rs` + `traceability/compat_matrix.yaml`SCOPE-loose-threads.md (1-365)
sserting `Err` or canonical refusal.
3. `tests/import_under_fault.rs` — **`INV-IMPORT-CRASH-IDEMPOTENT`**: drive `import_events` over `SimFs`, crash mid-pagination, reopen destination, **re-run the same import**, assert: (a) no duplicate events (durable dedup via `import_key` survives crash), (b) per-entity `prev_hash` chain intact, (c) payload bytes + content-hash byte-identical to source. This is the "survives N seeded hostile runs" loop made concrete.
4. `tests/import_same_store_ceiling.rs` — **`INV-IMPORT-NO-RUNAWAY`**: source == destination; assert import terminates at the call-time `import_ceiling` (`import.rs:255`) and never paginates into its own appends (the runaway it guards against, `:268-273`). Seed a store at the boundary where the active segment rotates *during* import.
5. `tests/import_semantic_diff.rs` — extend `semantic_diff` axis set: a store reached via `import_events` from another store must produce **byte-identical observables** to a store built by direct append of the same logical stream (mmap on/off × checkpoint on/off × cold-start). This proves import is observationally pure re-application, not a merge.

**B. Compat-matrix rows for fork/import on-disk shapes.** `traceability/compat_matrix.yaml` + `tests/compat_matrix.rs`: add rows for the **`import.provenance` receipt-extension schema** (`IMPORT_PROVENANCE_SCHEMA_VERSION = 1`) and the **fork evidence report body** — forge a future `schema_version` and assert canonical refusal (mirrors the existing `MmapFutureVersion` self-row at `compat_matrix.rs`). NOTE: this is **also** the cure for the standing pre-commit RED (`single_element_loop` placeholder, see Discovery #D1) — adding rows replaces the single-element placeholder loop.

**C. Equivalence-mutant audit.** Confirm the 5 registered equivalent mutants (`lanes.rs:186-191`, `:220-222`) still hold after the new fixtures land (the new tests may *kill* a previously-equivalent mutant, which is good — de-register it). Update `GAUNTLET_MUTATION_DEBT.md` if cloud smoke surfaces new survivors in `import.rs`/`file_classification.rs`/`fork_report.rs`.

**D. The bump/seal/tag.** Only after A–C are green on cloud:
1. `0.8.3 → 0.9.0` in every member `Cargo.toml` + `bpk-ts` manifests.
2. `CHANGELOG.md`: promote `[Unreleased]` → `[0.9.0] - <date>`.
3. Re-seal public surface: regenerate `traceability/public_api/batpak.txt` (the fork/import/lane APIs are the headline of the cut; the seal must match).
4. Per memory `batpak-release-flow`: manual chain-order `cargo publish`; npm needs the bypass-2FA granular token. Tag after publish.

### Red + green fixtures
For each new `INV-*` above, the gauntlet discipline is non-negotiable:
- **Red:** plant the violation and prove the harness catches it. Two mechanisms available (`gate_registry.rs:43-51`):
  - `GateNegativePath` — a green test whose body contains a failure-asserting token (`is_err`/`expect_err`/`Err(`/`is_none`/`is_empty`/`assert_ne!`). E.g. `fork_under_fault`'s red plants a *non-atomic* fork (publish before fsync) and asserts reopen rejects it.
  - `ProductionFlip` — a `#[cfg(gauntlet_red_fixture)]` test the `gauntlet-red-fixtures-bite` lane (`ci.yml:600`, `xtask/commands/prove_gates_bite.rs:22` `--cfg gauntlet_red_fixture`) builds with the cfg ON and asserts FAILS. **Use ProductionFlip for the crash-atomicity invariants** (mirrors the existing `dst-recovery` gate which is a proven ProductionFlip).
- **Green:** the live-tree test passes and the gate emits a non-vacuous receipt.

### Gate / enforcement
- Wire each `INV-*` witness test through `invariants.yaml` (strong-tier `witness_test:`) so `docs_catalog`/`invariant_bridge` enforce the doc↔test link (`structural.rs:30,42`).
- If you add a *blocking gate* (vs. just a proptest), register it in `gate_registry.rs` (the `GATES` array, struct-literal style, no helpers — `:74-408`) with `has_blocking_authority: true` + a named `red_fixture_test`, add to `RECEIPT_REQUIRED_GATES`, and wrap its check in `crate::receipts::run_gate("...", ...)` inside `structural.rs:run()` (pattern at `:84-90`).
- The seams already wire to CI (`ci.yml:396-397`); cloud `run-mutants` label on the PR exercises them.

### Sequencing / dependencies
1. Land #121 onto the gauntlet'd integration branch first (per GAUNTLET_ISSUES.md: #121 rebase has ~14 conflict files — delicate).
2. A (offensive fixtures) → B (compat rows, which *also* clears the pre-commit RED) → C (equivalence audit) → **cloud smoke green** → D (bump/seal/tag).
3. Depends on nothing from #64/#67/#68, but #67 landing first would *certify* the cut isn't over-claimed (recommended).

### Done-when
- All 5 offensive `INV-*` fixtures green (live) + red-proven; receipts emitted.
- Compat-matrix has fork-evidence + import-provenance rows; `single_element_loop` pre-commit RED is gone.
- Cloud `fork-isolation` + `import-reapply` smoke at/above the seam floor (85%); any survivor cured or registered with a proof in `mutation_debt.yaml`.
- Version is `0.9.0` everywhere; `public_api/batpak.txt` re-sealed and matching; CHANGELOG cut; published + tagged.

---

## Thread #64 — Mutation-as-divergence + DST corpus (hybrid AL-graded engine)

### Scope / why
Two mature-but-disjoint systems exist. **(1) Mutation:** `critical_mutation_seams()` (`lanes.rs:525-701`, 22 seams), repo phase `REPO_MUTATION_PHASE = Phase4` (75%, `policy.rs:19`), survivor ledger `mutation_debt.yaml` (typed, currently empty) + narrative `GAUNTLET_MUTATION_DEBT.md`. **(2) DST:** `sim/` — `SimFs` (real-file fault FS + `crash()`), `SimScheduler` (cooperative, deterministic), `workload.rs` (seeded op mix, FNV-1a op-trace digest), `recovery.rs`/`recovery_matrix.rs` (legality oracle over `(fault_mode, boundary, seed)`). Plus a third precedent: bvisor's `GroundTruth` oracle (`crates/bvisor/src/sim/ground_truth.rs:104-131`) where a *divergence between two independently-produced records IS a finding* (lie catalogue G1–G11).

**The thesis:** a **surviving mutant is a divergence finding** (the test suite agreed with a mutated reality), and **DST seeds that exercise rich behavior should graduate into a reusable corpus**, both **graded by the AL of the seam they touch**. Today neither exists: survivors are hand-logged; DST runs single env-seeds (`BATPAK_SEED`) with no corpus, no graduation, no AL coupling.

### Assurance level
The **engine code is L3** (deterministic tooling). But its *output* is AL-graded: a survivor/divergence in an **L4** seam is a hard finding (blocking); in **L1/L2** it is debt. The grading function is the deliverable.

### Files (`path:line`)
- Seam registry: `bpk-lib/tools/xtask/src/commands/mutants/lanes.rs:525-701`; `CriticalMutationSeam` struct `:300`; surface/paths fields
- Lane run/score: `mutants/run.rs:93-157` (`mutation_score` reads `caught/missed/timeout/unviable.txt`); policy `mutants/policy.rs:19-79`
- Debt ledger gate: `bpk-lib/tools/integrity/src/mutation_debt.rs:45` (`check`), `DebtEntry` `:28`; YAML `traceability/mutation_debt.yaml`
- DST: `crates/core/src/store/sim/mod.rs` (`run_seeded_workload` `:123`, `seed_from_env`/`replay_seed`), `scheduler.rs:150`, `fault_model.rs:82`, `workload.rs:57`, `recovery.rs`, `recovery_matrix.rs:64,125`
- GroundTruth divergence precedent: `crates/bvisor/src/sim/ground_truth.rs:35-78` (Lie catalogue), `:104-131`
- AL grading: `traceability/assurance_levels.yaml`; enum + lockstep `tools/integrity/src/assurance.rs:24-31,60-162` (`CRITICAL_SEAM_MUTANT_GLOBS` mirror)
- Repo-IR (already binds mutation-seam map as a fact column): `tools/integrity/src/repo_ir.rs`

### Exact changes

**A. Surviving-mutant → divergence finding (AL-graded).** Extend `mutation_debt.rs`:
- Add `seam_assurance_level(seam_slug) -> AssuranceLevel` by joining `lanes.rs` seam slug → `assurance_levels.yaml` entry (the lockstep test already guarantees this mapping is total for L3/L4).
- New rule in `check`: a survivor recorded against an **L4** seam without a `proof:` field (proven-equivalent) is a **hard fail** (blocking), not just ledger debt. L3 → warn-with-budget. L1/L2 → debt. Add `proof: <equivalence-argument>` optional field to `DebtEntry` (`:28`).
- Red fixture: plant a `mutation_debt.yaml` row with an L4 seam and no proof; assert `check` returns `Err`. Green: an L4 row *with* proof, or an L1 row, passes.

**B. DST corpus + graduation.** New `traceability/dst_corpus.yaml` (typed ledger) + a corpus module (`crates/core/src/store/sim/corpus.rs`, gated `dangerous-test-hooks`):
- Schema per entry: `{ seed, fault_mode, boundary, seam_touched, assurance_level, op_trace_digest, outcome }` where outcome ∈ {CommittedPrefix | RolledBack | CanonicalRefusal}.
- **Graduation criterion:** a seed graduates iff it (a) is deterministic (digest stable across 2 runs), (b) reaches a target seam, and (c) the legality oracle PASSES. Store the digest as the corpus identity.
- A seeded sweep harness `run_corpus_sweep(seeds: &[u64])` that runs `run_seeded_recovery` per seed and emits graduation candidates. Cloud lane runs the sweep; the YAML is the durable corpus (replayable via `BATPAK_SEED`).

**C. Mutation × DST interplay (the "hybrid").** The high-value join: run the **DST recovery oracle as the test that must kill the mutant**. Add a mutation-lane mode where, for L4 seams (`writer-commit`, `segment-scan`, `hash-chain-replay`, `frontier-wait-durable`, plus `fork-isolation`/`import-reapply`), the kill-test is `run_corpus_sweep` over the graduated corpus — a mutant that survives the *whole corpus* is a true divergence (the implementation diverged from spec and no seeded reality caught it). This makes the corpus the oracle and surviving-against-corpus the hardest finding tier.

**D. Public/stable seam registry.** Currently `critical_mutation_seams()` is `pub(super)` in xtask, mirrored as `CRITICAL_SEAM_MUTANT_GLOBS` in integrity (drift risk). Emit a `traceability/seam_registry.yaml` (slug → globs → AL → DST-coverage flag) as the single authoritative source; have both xtask and integrity *read* it; add a lockstep test that the YAML == the in-code arrays (extend the existing `assurance.rs` lockstep). This removes the two-copy drift surface and lets the corpus engine query "which files are in seam X".

### Red + green fixtures
- Grading (A): red = L4-survivor-without-proof → `Err`; green = proven L4 / L1 debt passes.
- Corpus (B): red = a seed whose two runs disagree (non-deterministic) must be **refused** entry to the corpus; green = a deterministic legality-passing seed graduates.
- Hybrid (C): red = `ProductionFlip` planting a hash-chain mutation that the unit suite misses but the corpus sweep catches → under `gauntlet_red_fixture` the sweep FAILS (proves the corpus bites); green = clean store survives the sweep.

### Gate / enforcement
- A's hard-fail wires through the existing `mutation_debt::check` already in `structural.rs:60`.
- B/C: cloud-only lanes (mutation/sweep are heavy — hard rule). Register a `dst-corpus-currency` gate (blocking, ProductionFlip) in `gate_registry.rs` that asserts the corpus YAML is non-empty and every entry's digest replays — this is the *local-cheap* half (replay one corpus seed, assert digest), the *cloud-heavy* sweep stays on the cloud lane.
- Seam-registry lockstep (D) extends `assurance.rs` lockstep test.

### Sequencing / dependencies
- A (grading) is independent and cheap — land first; it immediately hardens the existing ledger.
- D (seam registry YAML) should precede B/C (they query it).
- B → C (the hybrid needs a corpus to sweep).
- DST breadth is gated on the **SIM-2b/2c deferred StoreFs durability ops** (see Discovery #D6): the full Store-over-SimScheduler composition is NOT wired; the writer runs on `ThreadSpawn` and would deadlock a cooperative scheduler. **Honest sizing: the corpus today can only cover what `recovery.rs` already routes (segment durability cluster, honest-disk).** Lying-disk (`fsync_drop_one_in>0`) and read-path faults are out until SIM-2b lands. Scope the corpus to the *currently-routed* surface and grow it as SIM-2b/2c land — do not claim full-store DST coverage.

### Done-when
- Survivor grading is AL-aware: L4-without-proof blocks; cheap local replay gate green.
- `dst_corpus.yaml` exists, non-empty, every entry replays deterministically; graduation criterion enforced (non-deterministic seeds refused).
- At least the L4 fork/import + writer/segment seams have a corpus sweep as a cloud kill-test.
- `seam_registry.yaml` is authoritative; xtask + integrity read it; two-copy drift removed; lockstep green.
- README/INVARIANTS honestly state DST corpus covers the **routed** surface only (no over-claim).

---

## Thread #67 — Claim-vs-reality / "half-baked-intent" hunter

### Scope / why
A new gauntlet **detector family** that finds code/docs claiming a property the implementation doesn't deliver. The lesson is live in this very repo: the vacuous-glob killer found seam globs matching 0 files → 0 mutants → **vacuous PASS** (`GAUNTLET_ISSUES.md:4`); the kernel task #71 over-claim (admission circuit still a shadow, H_A/H_L unbound — per memory). The detector's job is to make over-claims *mechanically* impossible to merge. This is the meta-gate that certifies the 0.9.0 cut (and everything in *this scope doc's* deferral ledger) is honest.

Three claim→reality classes to cover:
1. **Gate over-claim:** a gate with `has_blocking_authority: true` whose red fixture is vacuous (green-only, no failure-asserting token) or whose ProductionFlip file lacks the `gauntlet_red_fixture` token. *(Partially covered already by `gate_registry.rs:554-593` — extend, don't duplicate.)*
2. **Doc over-claim:** an `INV-*` / MODEL.md / INVARIANTS.md property with no resolvable `witness_test`, OR a witness_test that is a plain `fn` not a `#[test]` (precedent: `docs_catalog.rs:144-185`, `invariant_bridge.rs:122-149`). Today only **3 of 94** catalog INVs carry `witness_test` (`invariants.yaml`); 79 ride weak header/ledger citation — that gap is the over-claim surface.
3. **Name-vs-behavior over-claim:** a `pub fn`/seam/module named/aimed at X with no test exercising X (e.g. a seam glob matching 0 files; a `*_evidence`/`*_verify`/`*_proof` fn with no assertion-bearing caller test). This is the genuinely *new* detector.

### Assurance level
**L3** (deterministic tooling that grades other gates). Its own red fixture must be a `ProductionFlip` so the `gauntlet-red-fixtures-bite` lane proves it bites — a claim-detector that can't be proven to catch a planted claim would itself be an over-claim.

### Files (`path:line`)
- New module: `bpk-lib/tools/integrity/src/overclaim.rs` (+ `overclaim_tests.rs`)
- Reuse — gate registry + self-proof law: `gate_registry.rs:43-66` (`Gate`, `RedFixtureKind`), validation `:554-593`, law test `no_blocking_gate_without_a_red_fixture` `:662-664`
- Reuse — triangulation oracle pattern (claim vs reality = two oracles, disagreement = finding): `triangulation.rs:38-43` (`Claim`), `:73-76` (`Oracle` trait), `:107-162` (`TriangulationEngine` — never picks a winner); red fixtures `triangulation_tests.rs:28-101`
- Reuse — receipts: `receipts.rs:156-169` (`run_gate`), `:175-193` (`GateWork`), anti-vacuity `:229-255`
- Reuse — doc↔code: `docs_catalog.rs:144-185` (witness_test must be a real `#[test]`), `invariant_bridge.rs:122-149` (anchor resolution)
- Reuse — AST/source: `source_cache.rs`, `rust_ast.rs`, `repo_surface.rs`
- Wire-in: `structural.rs:21-115` (add a `run_gate("overclaim", ...)` block), `main.rs:65-187` (add `CommandKind` for standalone `overclaim-check`)
- Source data: `invariants.yaml` (94 INV, 10 witness_test), `assurance_levels.yaml`, `lanes.rs` (seam globs — for the 0-file check, though `glob_coverage.rs` already covers stale globs — extend, don't duplicate)

### Exact changes
Model it on the **triangulation engine** (two independent derivations; disagreement is the finding), reusing `Claim`/`Oracle`:
- `ClaimOracle` — derives CLAIMS: parse gate-registry blocking flags, INV `witness_test` declarations, doc property assertions, `pub fn` names matching aspirational patterns (`*_evidence`, `*_proof`, `*_verify`, `*_attested`).
- `RealityOracle` — derives REALITY: does the witness test exist + carry `#[test]`? does the ProductionFlip file contain the token? does a seam glob match >0 files? does the aspirational fn have an assertion-bearing test in the citation set?
- The engine flags any `(subject, predicate)` where claim says "delivered" and reality says "absent". The detector **never** auto-resolves — the disagreement IS the hard finding (same doctrine as triangulation `:107-162`).
- A `check(repo_root) -> Result<GateWork>` that `bail!`s with the specific over-claim (subject + which oracle disagreed), returning real `files_examined`/`assertions_run` counts.

Sketch:
```rust
// overclaim.rs
fn check(repo_root: &Path) -> Result<GateWork> {
    let claims  = ClaimOracle.claims(repo_root)?;   // "INV-X claims witness Y", "fn fork_with_evidence claims evidence"
    let reality = RealityOracle.claims(repo_root)?;  // "witness Y is #[test]? no", "fn has assertion-bearing test? no"
    let findings = TriangulationEngine::new()
        .add(claims).add(reality).disagreements();   // claim=delivered vs reality=absent
    if !findings.is_empty() { bail!("over-claim: {findings:#?}"); }
    Ok(GateWork::new(files, assertions, inputs))
}
```

### Red + green fixtures
- **Red (ProductionFlip):** under `--cfg gauntlet_red_fixture`, register a synthetic over-claim — e.g. an INV declaring `witness_test: "foo::bar"` where `bar` is a plain `fn` (not `#[test]`), and assert `check` returns `Err` naming it. The bite-lane proves it FAILS.
- **Red (GateNegativePath):** a unit test that plants a synthetic gate-registry entry claiming `has_blocking_authority: true` with a green-only red fixture body and asserts the detector flags it (`expect_err`).
- **Green:** the live tree passes — every blocking gate has a non-vacuous red fixture, every declared witness_test is a real `#[test]`, every aspirational fn has an assertion-bearing test.

### Gate / enforcement
- Register in `gate_registry.rs` `GATES`: `Gate { slug: "overclaim", red_fixture_test: Some("tools/integrity/src/overclaim.rs::detector_rejects_planted_overclaim"), red_fixture_kind: Some(RedFixtureKind::ProductionFlip), has_blocking_authority: true }`.
- Add `"overclaim"` to `RECEIPT_REQUIRED_GATES`.
- Wrap in `crate::receipts::run_gate("overclaim", ...)` in `structural.rs:run()`.
- Add a `CommandKind::OverclaimCheck` for standalone runs in `main.rs`.

### Sequencing / dependencies
- Independent of #48/#64/#68. **Recommended to land before the 0.9.0 cut** so the cut is certified non-over-claimed (it would, today, flag the 79 prose-only INVs and any vacuous seam — scope the *first* version to gate class (1) gate-over-claim + (3) name-vs-behavior, and treat class (2) doc-witness as a ratchet so the existing 79-INV backlog doesn't block-fail the cut on day one).

### Done-when
- `overclaim` gate registered, blocking, ProductionFlip red-proven via the bite-lane, emits a non-vacuous receipt.
- Detector catches all three planted classes (gate / doc / name-vs-behavior) in fixtures.
- Live tree green; the detector flags zero real over-claims OR each is ratcheted with an explicit budget (no silent pass).

---

## Thread #68 — Dedup examples/templates/cookbook → one canonical runnable per concept

### Scope / why
Three parallel artifact families teach overlapping concepts with both **duplication** and **gaps**:
- **23 examples** (`crates/examples/examples/`), **18 cookbook docs** (`cookbook/`), **10 templates** (`bpk-lib/templates/`), **2 TS examples** (`bpk-ts/examples/`).
- **Duplication:** typed-append taught 3× (`quickstart.rs`, `cross_crate_payloads.rs`, `eight_jobs.rs`); durability/gates/receipts taught 5× (`append_with_gate.rs`, `wait_for_durable.rs`, `signed_receipts.rs`, `visibility_fence.rs`, `lifecycle_observer.rs`).
- **Gaps (cookbook+template but NO runnable example):** fork-clone, import-fork, lane-branch, artifact-envelope, attested-registry, backup-envelope, state-transition, reservation-ledger, platform-evidence, read-evidence. The headline 0.9.0 features (**fork/import/lane**) have docs but **no runnable example** — embarrassing for the cut.

Goal: **one canonical, lock-gated runnable per concept**, with cookbook + template referencing the canonical example rather than re-implementing.

### Assurance level
**L1** (examples are illustrative), but the **lock-gate is L2-ish** (the registry that guarantees every concept has exactly one runnable canonical).

### Files (`path:line`)
- Examples: `bpk-lib/crates/examples/examples/*.rs` (23 files), manifest `crates/examples/Cargo.toml`
- Cookbook: `/home/heyoub/Code/batpak/cookbook/*.md` (18 files), index `cookbook/README.md`
- Templates: `bpk-lib/templates/*/` (10 dirs)
- TS: `bpk-ts/examples/{audit-loop,heartbeat-spike}/`
- Lock-gate today (three layers):
  - `ART-EXAMPLES` registry: `traceability/artifacts.yaml:768-793` (every `.rs` must be listed)
  - Gate: `tools/integrity/src/traceability.rs:230-257` (`check_examples_artifact_complete` — fails on drift either direction)
  - Template smoke: `tools/xtask/src/commands/templates.rs` (copies each template, rewrites path dep, `cargo test`) invoked by `commands/ci.rs:65`
  - Release smoke: `tools/xtask/src/commands/release.rs:7-8` (`quickstart` must run)
- Concept↔task source of truth: `traceability/agent_surface.yaml` (NOT auto-coupled to cookbook today — weak link)

### Exact changes

**A. Concept registry (the canonical map).** New `traceability/concept_catalog.yaml`: one row per concept = `{ concept_id, canonical_example: <path>, cookbook: <path>, template: <dir|null>, agent_surface_task }`. This becomes the single authority for "one canonical runnable per concept."

**B. New gate `concept-canonical`** in `tools/integrity/` (extend `traceability.rs`):
- Every `concept_catalog` row's `canonical_example` exists, is in `ART-EXAMPLES`, and compiles (already smoke-tested via examples build).
- Every cookbook doc maps to exactly one concept row; every example maps to ≤1 concept (no two examples claim the same `concept_id` as canonical).
- Every `agent_surface.yaml` task has a concept row (closes the weak hand-synced link).

**C. Consolidate duplicates** (per concept_catalog):
- Typed-append: **`eight_jobs.rs` = canonical** (the full 0.8 path). `quickstart.rs` stays (marketing/release smoke) but is documented as a thin entry pointing to `eight_jobs`. **Retire `cross_crate_payloads.rs`** (fold its cross-crate-allocation assertion into a test, not an example).
- Durability cluster: collapse the 5 into **2 canonical** — one "durability gates + visibility" (`append_with_gate` absorbing `visibility_fence` + `wait_for_durable`) and one "receipts" (`signed_receipts`). `lifecycle_observer` folds into the canonical append example as a step.
- Keep the *progressions* as-is (they teach distinct things): cursor/reactor (basic→single→multi), projection (raw→derived-raw→fully-derived), typestate.

**D. Fill the gaps** — add canonical runnable examples for the 0.9.0 headline features and template-only concepts: **`fork_clone.rs`, `import_events.rs`, `lane_branch.rs`** (highest priority — they're the cut), then `read_evidence.rs`, and one runnable per template-only concept (artifact-envelope, attested-registry, backup-envelope, state-transition, reservation-ledger, platform-evidence). Each registered in `ART-EXAMPLES` + `concept_catalog`.

### Red + green fixtures
- Red: a `concept_catalog` row pointing at a missing/non-existent canonical example → gate `Err`; two examples claiming the same `concept_id` → gate `Err`; an `agent_surface` task with no concept row → gate `Err`.
- Green: live tree — every concept has exactly one canonical example, all compile, full cookbook/template/task coverage.

### Gate / enforcement
- Extend `traceability.rs` with `check_concept_canonical()`, wire into the existing `traceability-check` (runs in `ci-fast` per PR).
- The existing example-compile smoke (`templates.rs`/examples build) already guarantees runnability; the new gate guarantees *uniqueness + completeness*.

### Sequencing / dependencies
- D (fork/import/lane examples) is the only slice that should land **with the cut** (#48) — the 0.9.0 headline features must have runnable examples. The rest (A/B/C + remaining gaps) is post-cut cleanup.
- A → B → C/D.

### Done-when
- `concept_catalog.yaml` exists; `concept-canonical` gate green.
- fork/import/lane each have a canonical runnable example, in the catalog, compiling.
- No two examples are canonical for one concept; every cookbook doc + agent_surface task maps to a concept; duplicates retired/folded.

---

## Newly discovered threads (ranked)

> Sourced primarily from `GAUNTLET_ISSUES.md` (a rich, honest deferral ledger) cross-checked against code. The repo is *unusually clean* on the classic smells: **zero `#[allow]` in non-test source, zero `#[ignore]`d tests, zero typed waivers, zero GAP/PARTIAL rows, zero stub/`todo!`/`unimplemented!` in production.** The real debt is *deferred-but-tracked* gauntlet build-out, not rot.

### D1 — **Standing pre-commit RED (clippy `single_element_loop`)** — HIGH, trivial, cut-adjacent
`crates/core/tests/compat_matrix.rs:222` `for format in ["mmap-index"]` trips `clippy::single_element_loop`; multiple commits landed with `--no-verify` (`GAUNTLET_ISSUES.md` P3-PRECOMMIT ×2). The repo's pre-commit is **currently red** and being bypassed. **Cure = Thread #48 step B** (add the deferred compat-matrix rows → loop is no longer single-element). Until then every commit needs `--no-verify`, which masks *real* future failures. **Fix before the cut.**

### D2 — **Kernel #71 over-claim: admission circuit is a shadow, H_A/H_L unbound** — HIGH, out-of-scope-but-flag
Per memory (`gauntlet-program-state`, `bvisor-track-a-lowering-membrane`): task #71 "PROMOTE + bind identity" was claimed done but the admission circuit remains a **shadow** and H_A/H_L are **unbound**. The bvisor seams (`bvisor-admission`, `bvisor-report-seal`) are L3 in `assurance_levels.yaml` but the GroundTruth lie-catalogue (`ground_truth.rs:35-78`) is the *only* binding force. **This is exactly what Thread #67 should catch** — a circuit claimed promoted that's still shadow is the canonical name-vs-behavior over-claim. Flag for #67's fixture set; not in the 0.9.0 cut's path but the over-claim detector should be pointed here.

### D3 — **Mutation Phase4 (75%) set PROVISIONALLY** — HIGH, cloud-confirm
`policy.rs:19` `REPO_MUTATION_PHASE = Phase4`, set "PROVISIONALLY — confirm against first cloud repo-wide smoke; drop one line if it overshoots" (`GAUNTLET_ISSUES.md`). The post-merge repo-wide mutation on #127 **failed** with debt in new integrity code (`meta_gate`/`gate_registry`/`receipts`) — uncured. **Before the cut:** run cloud repo-wide smoke, confirm Phase4 holds or drop to Phase3, cure or register the #127 survivors in `mutation_debt.yaml`. Couples to Thread #64-A (grading).

### D4 — **`RepoMutationPhase::RecordOnly` never constructed (dead_code)** — LOW
`tools/xtask/src/commands/mutants/policy.rs:59` — a `RecordOnly` variant never constructed (a clippy `dead_code` warning, currently tolerated). Either wire it (it's the natural "L1/L2 survivor → record not block" tier for Thread #64-A) or remove it. **#64-A gives it a purpose** — likely keep + use.

### D5 — **79 of 94 INVs are prose/weak-tier (no witness_test)** — MEDIUM, ratchet
`invariants.yaml`: 94 `INV-*`, only 10 carry `witness_test` (strong tier). 79 ride weak header/ledger citation; 24 are prose-only with a deferred burndown ratchet (`GAUNTLET_ISSUES.md` P3-DOCS items 5.2/5.4). This is the **doc-over-claim surface** Thread #67 class (2) targets. Don't block the cut on it — ratchet. But MODEL.md→symbol bindings (5.2) and the prose-only burndown (5.4) are real, named, deferred docs-currency gaps.

### D6 — **SIM-2b/2c — CORRECTED 2026-06-23 (the cooperative writer is DONE; remnant is bounded fault-routing)** — MEDIUM, bounds #64 only
Original framing ("writer would deadlock on `rx.iter()`, needs full Store-over-SimScheduler") is **stale**. Verified against `write/writer.rs`:
- **DONE (#63):** writer is a state machine (`WriterCore::drive_command`→`DriveStep`) behind `WriterDrive::{Threaded, Cooperative}`; `pump()` (writer.rs:158) drains with **non-blocking `try_recv()`** (no `iter()`/deadlock) and is wired through the **whole** writer surface including the **append path** (`write_api.rs:41`), sync, fence, fork, snapshot, open. The cooperative writer exists end-to-end.
- **REMNANT (SIM-2b for the writer + #64):** the `StoreFs` trait still routes only ~6 ops; the writer's segment/sync durability cluster + fork's copy ops (`cow_copy_file`/`reject_symlink_leaf`/`canonicalize`/`symlink_metadata`) aren't on the trait, so the recovery oracle runs honest-disk (`fsync_drop_one_in=0`). Route them → enable `fsync_drop_one_in>0` / torn / ENOSPC. **Gated by the complexity ratchet** (`complexity_ratchet.yaml` pins `lifecycle.rs::snapshot=84`/`compact=167`, zero headroom) → a `lifecycle.rs` function-split comes first.
- **SIM-2c (scheduler-ordered pump) is OPTIONAL:** explicit-pump-at-callsite already gives deterministic single-thread ordering; only needed if you want scheduler-interleaved pump steps. Not a fork/import blocker.
- **Bounds #64's corpus, NOT #48's fork/import crash-atomicity** — fork/import drain the writer (`sync()`→`pump()`) and copy/re-apply with the writer quiesced-by-construction in DST, so their only hazard is I/O faults during the sequential op = the SIM-2b routing above.

### D7 — **Triangulation: only 2 of 6 oracles; DEPENDENCY-DIRECTION gate not built** — MEDIUM
`triangulation.rs` ships the engine + 2 crate-graph oracles (cargo-metadata, manifest-scan). Deferred (`GAUNTLET_ISSUES.md` P3-triangulation): rustc/clippy, syn-AST, traceability-ledger, runtime-receipts oracles; the `DEPENDENCY-DIRECTION` allowed-edge gate (item 3.3, `INV-DEPENDENCY-DIRECTION` not added); fitness_functions YAML registry. **Thread #67 reuses this engine** — adding oracles is additive, so #67 and this are synergistic.

### D8 — **Compat-matrix: 4 of 6 format rows deferred** — MEDIUM, partly in #48
`compat_matrix.yaml` has only the mmap-index self-row + forged-future-version. Deferred rows: idempotency-index, visibility-ranges, checkpoint, segment — **blocked on typed `*FutureVersion` errors not existing yet** (`GAUNTLET_ISSUES.md` P3 COMPAT-MATRIX, item 4.1). Thread #48-B adds fork-evidence + import-provenance rows on top of this; the 4 store-format rows are separate debt needing the typed errors first.

### D9 — **Repo-IR fitness pass is advisory, not blocking; 4 of 6 columns thin** — LOW/MEDIUM
`repo_ir.rs` binds 6 fact families but the fitness runner is advisory (not yet re-hosting the serial structural checks); syn-derived symbol/call/type columns, dep-edge column, format-version column deferred (`GAUNTLET_ISSUES.md` P3-REPO-IR). Mutation-seam map is mirrored from `assurance::CRITICAL_SEAM_MUTANT_GLOBS` rather than parsing `lanes.rs` — **Thread #64-D's `seam_registry.yaml` would fix this mirror.**

### D10 — **`react_loop` public return type is concrete `std::thread::JoinHandle`** — LOW, pre-1.0 break candidate
`GAUNTLET_ISSUES.md` SIM-2a: `react_loop`'s public return is a sealed concrete `JoinHandle<()>` (`public_api/batpak.txt`), blocking it from routing through the `Spawn` seam. Since API may break pre-1.0, the 0.9.0 cut is the *right time* to evolve it to an opaque handle — enabling the Sim scheduler to intercept it (unblocks part of D6). Consider folding into #48's API-break window.

### D11 — **SimClock TODO(core-export)** — LOW
`crates/bvisor/src/sim/clock.rs:11` — `SimClock` is `pub(crate)` in core; bvisor carries a duplicate to avoid reach-in. The only real `TODO()` in production. Promote core's `SimClock` to public → delete the duplicate. Cosmetic.

### D12 — **Dependabot #129–135** — note only (per instructions, not deep-scoped)
#129 taiki-e/install-action, #130 actions/checkout v7 (major), #133 @types/node v26 (major), #134 ed25519-compact 2.3.1 (a crypto dep — review carefully), #131 effect beta, #132 syn patch, #135 typescript-eslint. The two majors (#130, #133) and the crypto bump (#134) warrant a human glance before the cut; the rest are routine.

---

### Honest sizing notes
- **#48 is bigger than "add a few tests."** The offensive families largely target the *append/segment* path; fork (multi-file CoW + symlink + canonicalize) and import (paginated re-apply + same-store ceiling) are new fault surfaces. D6 means some fork fault-injection can't use `SimFs` yet (its `copy`/`metadata`/`reject_symlink_leaf` aren't routed) — expect to inject at the real-fs boundary or extend the `StoreFs` trait first. Budget for the SIM-2b function-split prerequisite if you want full `SimFs` coverage.
- **#64 is the largest** and is *gated* by D6 — scope the corpus to the routed surface, or you'll over-claim DST coverage (the exact sin #67 hunts).
- **#67 is the cleanest win** — the infrastructure (triangulation engine, gate-registry law, receipts, doc↔code linkage) all exists; it's mostly *composition*. Land it first to certify the rest.
- **#68's gap-fill (fork/import/lane examples) is the only #68 slice that's cut-blocking** by social contract (headline features need runnable examples).
