use crate::repo_surface::{
    core_benches_root, core_examples_root, core_tests_root, ensure, production_rust_roots,
    relative, repo_root, rust_files, tracked_repo_files,
};
use crate::shared_checks::{collect_dead_code_silencer_sites, load_dead_code_silencer_allowlist};
use crate::source_cache::SourceCache;
use crate::{
    agent_surface, architecture_lints, chaos_contract, ci_container_contract, ci_parity,
    complexity, dangerous_hooks_contract, docs_catalog, glob_coverage, harness_lints,
    invariant_bridge, literal_regex_contract, public_surface, scope_exclusion_contract,
    store_pub_fn_coverage, wallclock,
};
use anyhow::{anyhow, bail, Result};
use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;
use syn::visit::Visit;

#[cfg(test)]
#[path = "structural_tests.rs"]
mod structural_tests;

pub(crate) fn run() -> Result<()> {
    let repo_root = repo_root()?;
    let tracked_files = tracked_repo_files(&repo_root)?;
    let mut source_cache = SourceCache::new(&repo_root);
    architecture_lints::check(&repo_root, &tracked_files, &mut source_cache)?;
    agent_surface::check(&repo_root)?;
    harness_lints::check(&repo_root, &tracked_files, &mut source_cache)?;

    // invariant-bridge: receipt over the tracked-file surface it scans.
    crate::receipts::run_gate("invariant-bridge", || {
        invariant_bridge::check(&repo_root, &tracked_files)?;
        Ok(crate::receipts::GateWork::new(
            tracked_files.len(),
            tracked_files.len(),
            tracked_files.iter().cloned().collect(),
        ))
    })?;

    // GAUNTLET-DOCS-CURRENCY: the INVARIANTS.md catalog block must be a current
    // view of traceability/invariants.yaml (drift => fail), and every declared
    // witness_test must resolve to a real test. `check=true` => never rewrite.
    docs_catalog::run(&repo_root, true)?;

    // The structural source lints share one receipt: they all walk the
    // production-Rust surface and each file is the unit of work + the assertion.
    let started = crate::receipts::iso8601_now();
    let source_files = production_rust_files(&repo_root)?;
    check_no_dead_code_silencers(&repo_root, &mut source_cache)?;
    check_no_placeholder_runtime_macros(&repo_root, &mut source_cache)?;
    check_canonical_encoding_boundary(&repo_root, &mut source_cache)?;
    check_no_store_read_dir_entry_error_swallowing(&repo_root, &mut source_cache)?;
    check_store_segment_classification_boundary(&repo_root, &mut source_cache)?;
    check_allow_justifications(&repo_root, &mut source_cache)?;
    check_rust_file_size_pressure(&repo_root, &mut source_cache)?;
    check_inline_test_island_pressure(&repo_root, &mut source_cache)?;
    check_event_payload_frozen_fixtures(&repo_root, &mut source_cache)?;
    crate::test_assertions::check(&repo_root, &mut source_cache)?;
    complexity::check(&repo_root, &mut source_cache)?;
    wallclock::check(&repo_root, &mut source_cache)?;
    glob_coverage::check(&repo_root)?;
    crate::mutation_debt::check(&repo_root)?;
    crate::dst_corpus::check(&repo_root)?;
    public_surface::check(&repo_root, &mut source_cache)?;
    store_pub_fn_coverage::check(&repo_root, &mut source_cache)?;
    // Twelve blocking source lints ran over every production file; record the
    // real files-examined count and assertions (one structural lint per file).
    crate::receipts::record_pass(
        "structural-source-lints",
        &source_files.iter().cloned().collect(),
        source_files.len(),
        source_files.len().saturating_mul(12),
        started,
    )?;

    crate::receipts::run_gate("examples-observable-output", || {
        let inputs = check_examples_observable_output(&repo_root, &mut source_cache)?;
        let files = inputs.len().max(1);
        Ok(crate::receipts::GateWork::new(files, files, inputs))
    })?;

    crate::receipts::run_gate("perf-gates-contract", || {
        let inputs = crate::perf_gates_contract::check(&repo_root, &mut source_cache)?;
        let files = inputs.len().max(1);
        Ok(crate::receipts::GateWork::new(files, files, inputs))
    })?;

    crate::receipts::run_gate("dangerous-hooks-contract", || {
        let inputs = dangerous_hooks_contract::check(&repo_root, &mut source_cache)?;
        let files = inputs.len().max(1);
        Ok(crate::receipts::GateWork::new(files, files, inputs))
    })?;

    crate::receipts::run_gate("chaos-linux-only-contract", || {
        let inputs = chaos_contract::check(&repo_root, &mut source_cache)?;
        let files = inputs.len().max(1);
        Ok(crate::receipts::GateWork::new(files, files, inputs))
    })?;

    crate::receipts::run_gate("literal-regex-contract", || {
        let inputs = literal_regex_contract::check(&repo_root, &mut source_cache)?;
        let files = inputs.len().max(1);
        Ok(crate::receipts::GateWork::new(files, files, inputs))
    })?;

    crate::receipts::run_gate("canonical-container-ci", || {
        let inputs = ci_container_contract::check(&repo_root)?;
        let files = inputs.len().max(1);
        Ok(crate::receipts::GateWork::new(files, files, inputs))
    })?;

    crate::receipts::run_gate("cross-directory-scope-contract", || {
        let inputs = scope_exclusion_contract::check(&repo_root, &mut source_cache)?;
        let files = inputs.len().max(1);
        Ok(crate::receipts::GateWork::new(files, files, inputs))
    })?;

    // ci-parity: receipt over the ci.yml + xtask source surface it cross-checks.
    crate::receipts::run_gate("ci-parity", || {
        ci_parity::check(&repo_root)?;
        let inputs: BTreeSet<PathBuf> = ci_parity_inputs(&repo_root);
        let files = inputs.len();
        Ok(crate::receipts::GateWork::new(files, files.max(1), inputs))
    })?;

    // triangulation: cross-oracle check of non-type facts (GAUNTLET-TRIANGULATION).
    // The wired fact is workspace crate-graph acyclicity, cross-checked by two
    // independent graph derivations; a disagreement OR an agreed cycle fails.
    crate::receipts::run_gate("triangulation", || {
        crate::triangulation::check(&repo_root)?;
        // The declarative rule surface must stay in lockstep with the code roster.
        crate::fitness_functions::check(&repo_root)?;
        let mut inputs: BTreeSet<PathBuf> = workspace_manifest_inputs(&repo_root);
        inputs.insert(repo_root.join("Cargo.toml"));
        inputs.insert(crate::triangulation::dependency_direction_path(&repo_root));
        inputs.insert(crate::fitness_functions::fitness_functions_path(&repo_root));
        let files = inputs.len().max(1);
        Ok(crate::receipts::GateWork::new(files, files, inputs))
    })?;

    crate::receipts::run_gate("overclaim", || crate::overclaim::check(&repo_root))?;

    crate::receipts::run_gate("release-status", || {
        crate::release_status::check(
            &repo_root,
            &crate::release_status::ReleaseCheckOptions::structural(),
        )
    })?;

    // repo-ir-fitness (D9): fold the BLOCKING fitnesses over the live repo-IR and
    // additionally assert every seam glob PARSED from seam_registry.yaml resolves
    // to a tracked file. A finding fails the run — the IR is a real gate now, not
    // an advisory skeleton; its seam column is parsed (not mirrored).
    crate::receipts::run_gate("repo-ir-fitness", || crate::repo_ir::check(&repo_root))?;

    // assurance-level-check: receipt over the manifest + the production files it
    // resolves to assurance levels.
    crate::receipts::run_gate("assurance-level-check", || {
        crate::assurance::check(&repo_root)?;
        let manifest = crate::assurance::manifest_path(&repo_root);
        let mut inputs: BTreeSet<PathBuf> =
            production_rust_files(&repo_root)?.into_iter().collect();
        inputs.insert(manifest);
        let files = inputs.len();
        Ok(crate::receipts::GateWork::new(files, files, inputs))
    })?;

    // typed-waivers: receipt over the waiver file (always examined, even when
    // empty — the gate parses + validates it on every run).
    crate::receipts::run_gate("typed-waivers", || {
        crate::typed_waivers::check(&repo_root)?;
        let mut inputs = BTreeSet::new();
        inputs.insert(crate::typed_waivers::waivers_path(&repo_root));
        Ok(crate::receipts::GateWork::new(1, 1, inputs))
    })?;

    // no-runtime-dep-graph (D10): the PRODUCTION dependency graph of every runtime
    // crate must contain NO async runtime (tokio/async-std/smol/async-executor),
    // including renamed/optional/target-specific/transitive forms a Cargo.toml grep
    // misses. The SAME shared scanner is the build.rs early sentinel. Input is the
    // workspace manifest surface (the resolved graph cargo metadata reads).
    crate::receipts::run_gate("no-runtime-dep-graph", || {
        crate::no_runtime_gate::check(&repo_root)?;
        let inputs = workspace_manifest_inputs(&repo_root);
        let files = inputs.len().max(1);
        Ok(crate::receipts::GateWork::new(files, files, inputs))
    })?;

    // store-sync-only (D11): the STRUCTURAL (AST) half — no public async Store API,
    // no impl-Future/boxed-Future return, no #[async_trait], no .await/async block
    // in production store code — PLUS the dep-graph half (no async executor in the
    // store's production graph). Replaces the old `async fn` substring grep.
    crate::receipts::run_gate("store-sync-only", || {
        crate::store_sync_gate::check(&repo_root, &mut source_cache)?;
        crate::no_runtime_gate::check_store(&repo_root)?;
        let inputs: BTreeSet<PathBuf> = crate::store_sync_gate::store_production_files(&repo_root)
            .into_iter()
            .collect();
        let files = inputs.len().max(1);
        Ok(crate::receipts::GateWork::new(files, files, inputs))
    })?;

    // capability-snapshot (GAUNT-CAPSNAP): the committed capability FLOOR must be
    // an exact mirror of every backend's `support_matrix()` best-case table + the
    // witnessed-invariant set. Drift => fail+regenerate; a downgrade diff is
    // blocked by the meta-gate. Inputs: the snapshot + the four backend tables.
    crate::receipts::run_gate("capability-snapshot", || {
        crate::capability_snapshot::check(&repo_root)?;
        let mut inputs = BTreeSet::new();
        inputs.insert(repo_root.join(crate::capability_snapshot::SNAPSHOT_REL));
        for backend in ["linux", "wasm", "windows", "macos"] {
            inputs.insert(repo_root.join(format!("crates/bvisor/src/backend/{backend}/mod.rs")));
        }
        let files = inputs.len();
        Ok(crate::receipts::GateWork::new(files, files, inputs))
    })?;

    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "structural-check: ok");
    Ok(())
}

/// The files `ci_parity::check` cross-checks: the CI workflow, the Dockerfile,
/// the justfile, and the xtask command surface. Used only to give the ci-parity
/// receipt a real (non-zero) `files_examined` count + `inputs_hash`; missing
/// optional files are simply omitted.
fn ci_parity_inputs(repo_root: &Path) -> BTreeSet<PathBuf> {
    let mut inputs = BTreeSet::new();
    for rel in [
        ".github/workflows/ci.yml",
        "Dockerfile",
        ".devcontainer/Dockerfile",
        "justfile",
        "tools/xtask/src/main.rs",
    ] {
        let path = repo_root.join(rel);
        if path.exists() {
            inputs.insert(path);
        }
    }
    inputs
}

/// The member `Cargo.toml` manifests the triangulation crate-graph oracles read
/// (every workspace member's manifest, plus the root). Used to give the
/// triangulation receipt a real `files_examined` count + `inputs_hash`.
fn workspace_manifest_inputs(repo_root: &Path) -> BTreeSet<PathBuf> {
    let mut inputs = BTreeSet::new();
    for rel in [
        "crates/core",
        "crates/syncbat",
        "crates/netbat",
        "crates/bvisor",
        "crates/macros",
        "crates/macros-support",
        "crates/bench-support",
        "tools/integrity",
        "tools/xtask",
    ] {
        let manifest = repo_root.join(rel).join("Cargo.toml");
        if manifest.exists() {
            inputs.insert(manifest);
        }
    }
    inputs
}

/// Absolute, non-overridable production file cap. There is no per-file escape
/// hatch and no per-file ceiling: a file over this cap must be split, never
/// bumped ("split, don't bump"); a file under the cap passes regardless of its
/// prior size.
const DEFAULT_LINE_BUDGET: usize = 850;

fn check_rust_file_size_pressure(repo_root: &Path, source_cache: &mut SourceCache) -> Result<()> {
    check_rust_file_size_pressure_over(repo_root, &production_rust_files(repo_root)?, source_cache)
}

/// Testable core of [`check_rust_file_size_pressure`]: assert every file in
/// `paths` is within the absolute production line budget. Split out so a RED
/// fixture can run the gate over a planted temp tree without depending on the
/// live production-Rust surface.
fn check_rust_file_size_pressure_over(
    repo_root: &Path,
    paths: &[PathBuf],
    source_cache: &mut SourceCache,
) -> Result<()> {
    for path in paths {
        let rel = relative(repo_root, path);
        let content = source_cache.read_to_string(path)?;
        let line_count = nonblank_line_count(&content);
        ensure(
            line_count <= DEFAULT_LINE_BUDGET,
            format!(
                "structural-check: production Rust file size pressure in {rel}: {line_count} lines exceeds the absolute cap {DEFAULT_LINE_BUDGET}.\n\
                 The cap is non-overridable: split the file, do not bump a budget. \
                 There is no per-file size allowlist anymore."
            ),
        )?;
    }
    Ok(())
}

/// Absolute, non-overridable cap on the nonblank line count of a single inline
/// `#[cfg(test)] mod tests` island in a production source file.
const DEFAULT_TEST_ISLAND_BUDGET: usize = 200;

fn check_inline_test_island_pressure(
    repo_root: &Path,
    source_cache: &mut SourceCache,
) -> Result<()> {
    check_inline_test_island_pressure_over(
        repo_root,
        &production_rust_files(repo_root)?,
        source_cache,
    )
}

/// Testable core of [`check_inline_test_island_pressure`]: assert every inline
/// `mod tests` island in `paths` is within the absolute island budget. Split out
/// so a RED fixture can run the gate over a planted temp tree.
fn check_inline_test_island_pressure_over(
    repo_root: &Path,
    paths: &[PathBuf],
    source_cache: &mut SourceCache,
) -> Result<()> {
    for path in paths {
        let rel = relative(repo_root, path);
        let content = source_cache.read_to_string(path)?;
        let file = source_cache
            .parse_rust(path)
            .map_err(|err| anyhow!("parse inline test islands in {rel}: {err}"))?;
        let lines = content.lines().collect::<Vec<_>>();
        for island in inline_test_islands(&file, &lines) {
            ensure(
                island.nonblank_lines <= DEFAULT_TEST_ISLAND_BUDGET,
                format!(
                    "structural-check: oversized inline `mod tests` island in {rel}:{}-{} has {} nonblank lines, exceeding the absolute cap {DEFAULT_TEST_ISLAND_BUDGET}.\n\
                     The cap is non-overridable: extract growth into integration tests or focused test modules. \
                     There is no per-island size allowlist anymore.",
                    island.start_line,
                    island.end_line,
                    island.nonblank_lines
                ),
            )?;
        }
    }
    Ok(())
}

/// One `#[derive(EventPayload)]` type that does not yet have a frozen-decode
/// fixture. Mirrors `harness_lints::HeaderDebt`: a pre-seeded
/// allowlist so the warn-first lint lands green while the fixture backlog
/// (Phase 2, `ART-EVENT-PAYLOAD-FROZEN-GOLDENS`) is burned down.
struct FrozenFixtureDebt {
    /// The payload struct's identifier (matched against the AST-discovered name).
    type_name: &'static str,
    /// Why this payload has no frozen fixture yet.
    reason: &'static str,
    /// What closes the debt.
    target: &'static str,
}

/// `BoundaryStartedEvent` (0xE/0x001) is intentionally fixture-less while its
/// payload is PROVISIONAL: it embeds the full `BoundaryPlan`, whose budget surface
/// is being widened to the seven-dimensional model. Its provisional golden was
/// deleted (not version-bumped) and is regenerated once the final admission surface
/// is integrated — see `crates/bvisor/tests/frozen_goldens.rs` (`PAYLOAD_MANIFEST`).
/// The anti-rot check below forces this entry's removal once `e_001__v1.hex` is
/// re-frozen. The other 0xE payloads keep their v1 goldens.
const FROZEN_FIXTURE_DEBT: &[FrozenFixtureDebt] = &[FrozenFixtureDebt {
    type_name: "BoundaryStartedEvent",
    reason: "provisional: embeds BoundaryPlan whose 7-dimensional budget surface is mid-widening",
    target: "regenerate e_001__v1.hex after the final admission surface + plan identity land",
}];

/// Warn-first frozen-fixture lint (`ART-EVENT-PAYLOAD-FROZEN-GOLDENS`).
///
/// Every `#[derive(EventPayload)]` type in production source must have a frozen
/// payload fixture (`tests/golden/payloads/<cat>_<type_id>__v*.hex`) so the
/// decode seam's back-compat is proven against historical bytes
/// (`INV-EVENT-PAYLOAD-DECODE-BACKCOMPAT`). Types still in `FROZEN_FIXTURE_DEBT`
/// are skipped. Phase 1 is **warn-first** (prints, does not fail) — it flips to
/// hard-fail next minor once the debt is burned down. A debt entry whose fixture
/// now exists, or that names a type no longer present, is reported so the
/// allowlist cannot rot.
fn check_event_payload_frozen_fixtures(
    repo_root: &Path,
    source_cache: &mut SourceCache,
) -> Result<()> {
    let payloads_dir = core_tests_root(repo_root).join("golden").join("payloads");

    let mut discovered: Vec<(String, Option<(u8, u16)>)> = Vec::new();
    for path in production_rust_files(repo_root)? {
        let rel = relative(repo_root, &path);
        let file = source_cache
            .parse_rust(&path)
            .map_err(|err| anyhow!("parse EventPayload derives in {rel}: {err}"))?;
        collect_event_payload_structs(&file.items, &mut discovered);
    }

    let debt: std::collections::HashMap<&str, &FrozenFixtureDebt> = FROZEN_FIXTURE_DEBT
        .iter()
        .map(|d| (d.type_name, d))
        .collect();

    let mut warnings: Vec<String> = Vec::new();
    let mut satisfied_debt: BTreeSet<&str> = BTreeSet::new();
    let discovered_names: BTreeSet<&str> = discovered.iter().map(|(n, _)| n.as_str()).collect();

    for (name, kind) in &discovered {
        let has_fixture = kind
            .map(|(cat, type_id)| frozen_fixture_exists(&payloads_dir, cat, type_id))
            .unwrap_or(false);
        let is_debted = debt.contains_key(name.as_str());
        if has_fixture {
            if is_debted {
                satisfied_debt.insert(name.as_str());
            }
            continue;
        }
        if is_debted {
            continue;
        }
        warnings.push(format!(
            "structural-check (warn): EventPayload type `{name}` has no frozen payload fixture under \
             {} and is not in FROZEN_FIXTURE_DEBT. Freeze a v1 fixture + a frozen_decode test \
             (see EVENTS.md -> Schema Evolution), or add a debt entry. \
             [ART-EVENT-PAYLOAD-FROZEN-GOLDENS; warn-first this minor]",
            relative(repo_root, &payloads_dir)
        ));
    }

    // Anti-rot: a debt entry whose fixture now exists must be removed; a debt
    // entry naming a vanished type is stale.
    for d in FROZEN_FIXTURE_DEBT {
        if satisfied_debt.contains(d.type_name) {
            warnings.push(format!(
                "structural-check (warn): FROZEN_FIXTURE_DEBT entry `{}` now has a fixture; remove \
                 the debt entry (reason was: {}; target: {}).",
                d.type_name, d.reason, d.target
            ));
        } else if !discovered_names.contains(d.type_name) {
            warnings.push(format!(
                "structural-check (warn): FROZEN_FIXTURE_DEBT entry `{}` names no live \
                 EventPayload type; remove the stale debt entry.",
                d.type_name
            ));
        }
    }

    let mut out = std::io::stdout().lock();
    for warning in &warnings {
        let _ = writeln!(out, "{warning}");
    }
    Ok(())
}

/// Walk items recursively, collecting `(struct_name, Option<(category, type_id)>)`
/// for every struct carrying `#[derive(..EventPayload..)]`. The kind tuple is
/// `None` when the `#[batpak(category, type_id)]` attribute cannot be parsed
/// (the derive itself would reject that, so it only happens on malformed input).
fn collect_event_payload_structs(items: &[syn::Item], out: &mut Vec<(String, Option<(u8, u16)>)>) {
    for item in items {
        if let syn::Item::Struct(s) = item {
            if has_event_payload_derive(&s.attrs) {
                out.push((s.ident.to_string(), parse_batpak_kind(&s.attrs)));
            }
        } else if let syn::Item::Mod(m) = item {
            // Skip `#[cfg(test)]` modules: payload types defined there are test
            // fixtures, not shippable wire kinds, so they need no frozen-decode
            // proof of historical compatibility.
            if is_cfg_test(&m.attrs) {
                continue;
            }
            if let Some((_, nested)) = &m.content {
                collect_event_payload_structs(nested, out);
            }
        }
    }
}

/// True when `attrs` carries `#[cfg(test)]` (the literal test-gate form).
fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("cfg") {
            return false;
        }
        let mut found = false;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("test") {
                found = true;
            }
            Ok(())
        });
        found
    })
}

fn has_event_payload_derive(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("derive") {
            return false;
        }
        let mut found = false;
        let _ = attr.parse_nested_meta(|meta| {
            if meta
                .path
                .segments
                .last()
                .is_some_and(|seg| seg.ident == "EventPayload")
            {
                found = true;
            }
            Ok(())
        });
        found
    })
}

fn parse_batpak_kind(attrs: &[syn::Attribute]) -> Option<(u8, u16)> {
    let attr = attrs.iter().find(|a| a.path().is_ident("batpak"))?;
    let mut category: Option<u64> = None;
    let mut type_id: Option<u64> = None;
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("category") {
            let lit: syn::LitInt = meta.value()?.parse()?;
            category = lit.base10_parse::<u64>().ok();
        } else if meta.path.is_ident("type_id") {
            let lit: syn::LitInt = meta.value()?.parse()?;
            type_id = lit.base10_parse::<u64>().ok();
        } else if meta.path.is_ident("version") {
            let _: syn::LitInt = meta.value()?.parse()?;
        }
        Ok(())
    })
    .ok()?;
    // crates/macros/src/event_payload.rs already validates category fits 4 bits
    // and type_id fits 12 bits; try_from mirrors that bounded narrowing and
    // drops the value (None) if a malformed attribute somehow exceeds it.
    let category = u8::try_from(category?).ok()?;
    let type_id = u16::try_from(type_id?).ok()?;
    Some((category, type_id))
}

/// A fixture for `(category, type_id)` exists when any `<cat>_<type_id>__v*.hex`
/// is present. Fixture base name matches the test helper's
/// `<kind>__v<N>.hex` convention: kind == `{cat:x}_{type_id:03x}`.
fn frozen_fixture_exists(payloads_dir: &Path, category: u8, type_id: u16) -> bool {
    let prefix = format!("{category:x}_{type_id:03x}__v");
    let Ok(entries) = std::fs::read_dir(payloads_dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix) && name.ends_with(".hex") {
            return true;
        }
    }
    false
}

fn check_no_placeholder_runtime_macros(
    repo_root: &Path,
    source_cache: &mut SourceCache,
) -> Result<()> {
    let mut paths = production_rust_files(repo_root)?;
    paths.extend(rust_files(&repo_root.join("tools/xtask/src")));
    paths.extend(rust_files(&repo_root.join("tools/integrity/src")));
    paths.push(repo_root.join("crates/core/build.rs"));

    for path in paths {
        let rel = relative(repo_root, &path);
        let content = source_cache.read_to_string(&path)?;
        for (line_no, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") {
                continue;
            }
            for needle in [
                "to".to_owned() + "do!(",
                "unimplemented".to_owned() + "!(",
                "d".to_owned() + "bg!(",
            ] {
                if line.contains(&needle) {
                    bail!(
                        "structural-check: disallowed placeholder/debug macro `{}` in {}:{}.\n\
                         Remove the macro and implement explicit behavior or diagnostics.\n\
                         See INV-BUILD-FAIL-FAST and INV-TRACEABILITY-COMPLETE.",
                        needle,
                        rel,
                        line_no + 1
                    );
                }
            }
        }
    }
    Ok(())
}

fn check_no_dead_code_silencers(repo_root: &Path, source_cache: &mut SourceCache) -> Result<()> {
    let mut paths = production_rust_files(repo_root)?;
    paths.extend(rust_files(&repo_root.join("tools/xtask/src")));
    paths.extend(rust_files(&repo_root.join("tools/integrity/src")));
    paths.extend(rust_files(&core_tests_root(repo_root)));
    paths.extend(rust_files(&core_examples_root(repo_root)));
    paths.extend(rust_files(&core_benches_root(repo_root)));
    paths.push(repo_root.join("crates/core/build.rs"));
    check_no_dead_code_silencers_over(repo_root, &paths, source_cache)
}

fn check_examples_observable_output(
    repo_root: &Path,
    source_cache: &mut SourceCache,
) -> Result<BTreeSet<PathBuf>> {
    let paths = rust_files(&core_examples_root(repo_root));
    let inputs = paths.iter().cloned().collect::<BTreeSet<_>>();
    check_examples_observable_output_over(repo_root, &paths, source_cache)?;
    Ok(inputs)
}

/// Testable core for INV-EXAMPLES-OBSERVABLE-OUTPUT. Examples must use the
/// explicit `Write` API (`writeln!(stdout().lock(), ..)` or an equivalent locked
/// handle), not `print!`/`println!`, so observable output remains visible to
/// static review and cannot be hidden behind the formatting macros' global lock.
fn check_examples_observable_output_over(
    repo_root: &Path,
    paths: &[PathBuf],
    source_cache: &mut SourceCache,
) -> Result<()> {
    for path in paths {
        let rel = relative(repo_root, path);
        let file = source_cache
            .parse_rust(path)
            .map_err(|err| anyhow!("parse example output gate target {rel}: {err}"))?;
        let offenders = collect_print_macro_sites(&file);
        if let Some(offender) = offenders.first() {
            bail!(
                "structural-check (INV-EXAMPLES-OBSERVABLE-OUTPUT): `{}` macro in {}:{}.\n\
                 Examples must emit observable output through the explicit Write API, e.g. \
                 `let mut out = std::io::stdout().lock(); let _ = writeln!(out, ...)`.",
                offender.macro_name,
                rel,
                offender.line,
            );
        }
    }
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
struct PrintMacroSite {
    line: usize,
    macro_name: &'static str,
}

fn collect_print_macro_sites(file: &syn::File) -> Vec<PrintMacroSite> {
    let mut visitor = PrintMacroVisitor::default();
    visitor.visit_file(file);
    visitor.sites.sort_by_key(|site| site.line);
    visitor.sites
}

#[derive(Default)]
struct PrintMacroVisitor {
    sites: Vec<PrintMacroSite>,
}

impl<'ast> Visit<'ast> for PrintMacroVisitor {
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if macro_ends_with(&node.path, "println") {
            self.sites.push(PrintMacroSite {
                line: node.span().start().line,
                macro_name: "println!",
            });
        }
        if macro_ends_with(&node.path, "print") {
            self.sites.push(PrintMacroSite {
                line: node.span().start().line,
                macro_name: "print!",
            });
        }
        syn::visit::visit_macro(self, node);
    }
}

fn macro_ends_with(path: &syn::Path, ident: &str) -> bool {
    path.segments
        .last()
        .is_some_and(|segment| segment.ident == ident)
}

/// Testable core of [`check_no_dead_code_silencers`]: scan `paths` for
/// `dead_code`/`unused` silencer attributes not present in the repo's allowlist.
/// Split out so a RED fixture can run the gate over a planted temp tree.
fn check_no_dead_code_silencers_over(
    repo_root: &Path,
    paths: &[PathBuf],
    source_cache: &mut SourceCache,
) -> Result<()> {
    let allowlisted = load_dead_code_silencer_allowlist(repo_root).map_err(|err| anyhow!(err))?;
    for path in paths {
        let content = source_cache.read_to_string(path)?;
        let sites = collect_dead_code_silencer_sites(&content)
            .map_err(|err| anyhow!("parse {}: {}", relative(repo_root, path), err))?;
        for site in sites {
            let allowlist_site = format!("{}:{}", relative(repo_root, path), site.line);
            if allowlisted.contains(&allowlist_site) {
                continue;
            }
            bail!(
                "zero-allow policy (INV-ALLOW-IS-DESIGN): remove the #[allow]; fix the lint instead — see the INV.\n\
                 Found `{}` in {}:{}:{}.\n\
                 The repo permits NO #[allow(...)]/#![allow(...)]/#[expect(...)] attributes.\n\
                 If code is test-only, use #[cfg(test)]. If it is unused, delete it.\n\
                 If it is shared infrastructure, restructure it so the compiler sees the real ownership surface.",
                site.rendered,
                relative(repo_root, path),
                site.line,
                site.column,
            );
        }
    }
    Ok(())
}

fn check_allow_justifications(repo_root: &Path, source_cache: &mut SourceCache) -> Result<()> {
    let mut paths = production_rust_files(repo_root)?;
    paths.extend(rust_files(&repo_root.join("tools/xtask/src")));
    paths.extend(rust_files(&repo_root.join("tools/integrity/src")));
    paths.extend(rust_files(&core_tests_root(repo_root)));
    paths.extend(rust_files(&core_examples_root(repo_root)));
    paths.extend(rust_files(&core_benches_root(repo_root)));
    paths.push(repo_root.join("crates/core/build.rs"));
    check_allow_justifications_over(repo_root, &paths, source_cache)
}

/// Testable core of [`check_allow_justifications`]: under the zero-allow
/// doctrine (INV-ALLOW-IS-DESIGN) there are no allows to justify. This is now a
/// HARD BAN — any `#[allow(...)]`/`#![allow(...)]`/`#[expect(...)]` attribute in
/// `paths` (including cfg_attr-wrapped) fails. It routes through the same
/// AST-based detector as the dead-code gate, so raw-string fixtures are
/// correctly excluded and multi-line attributes are caught. The gate name and
/// signature are kept stable for build.rs `run_surface_lint` and the RED
/// fixtures. Split out so a RED fixture can run the gate over a planted temp tree.
fn check_allow_justifications_over(
    repo_root: &Path,
    paths: &[PathBuf],
    source_cache: &mut SourceCache,
) -> Result<()> {
    for path in paths {
        let content = source_cache.read_to_string(path)?;
        let sites = collect_dead_code_silencer_sites(&content)
            .map_err(|err| anyhow!("parse {}: {}", relative(repo_root, path), err))?;
        if let Some(site) = sites.into_iter().next() {
            ensure(
                false,
                format!(
                    "zero-allow policy (INV-ALLOW-IS-DESIGN): remove the #[allow]; fix the lint instead — see the INV. \
                     Found `{}` in {}:{}:{}. The repo permits NO #[allow(...)]/#![allow(...)]/#[expect(...)] attributes.",
                    site.rendered,
                    relative(repo_root, path),
                    site.line,
                    site.column,
                ),
            )?;
        }
    }
    Ok(())
}

fn check_canonical_encoding_boundary(
    repo_root: &Path,
    source_cache: &mut SourceCache,
) -> Result<()> {
    for path in production_rust_files(repo_root)? {
        let rel = relative(repo_root, &path);
        if rel == "crates/core/src/encoding.rs" {
            continue;
        }
        let content = source_cache.read_to_string(&path)?;
        for (line_no, line) in content.lines().enumerate() {
            if line.contains("rmp_serde::from_slice")
                || line.contains("rmp_serde::to_vec")
                || line.contains("rmp_serde::to_vec_named")
            {
                bail!(
                    "structural-check: raw rmp_serde call in {}:{}.\n\
                     Route production MessagePack through crate::encoding so ADR-0019 has one enforceable boundary.",
                    rel,
                    line_no + 1
                );
            }
        }
    }
    Ok(())
}

fn check_no_store_read_dir_entry_error_swallowing(
    repo_root: &Path,
    source_cache: &mut SourceCache,
) -> Result<()> {
    for path in production_rust_files(repo_root)? {
        let rel = relative(repo_root, &path);
        if !rel.starts_with("crates/core/src/store/") {
            continue;
        }
        let content = source_cache.read_to_string(&path)?;
        let lines = content.lines().collect::<Vec<_>>();
        for line_no in 0..lines.len() {
            if read_dir_entry_error_is_swallowed(&lines, line_no) {
                bail!(
                    "structural-check: read_dir entry errors must not be swallowed in {rel}:{}.\n\
                     Collect or iterate directory entries as Result values and propagate DirEntry errors through StoreError.",
                    line_no + 1
                );
            }
        }
    }
    Ok(())
}

fn check_store_segment_classification_boundary(
    repo_root: &Path,
    source_cache: &mut SourceCache,
) -> Result<()> {
    for path in production_rust_files(repo_root)? {
        let rel = relative(repo_root, &path);
        if !rel.starts_with("crates/core/src/store/")
            || rel == "crates/core/src/store/file_classification.rs"
            || rel == "crates/core/src/store/segment/mod.rs"
        {
            continue;
        }
        let content = source_cache.read_to_string(&path)?;
        let lines = content.lines().collect::<Vec<_>>();
        for line_no in 0..lines.len() {
            if local_segment_extension_classification(&lines, line_no) {
                bail!(
                    "structural-check: local segment filename classification in {rel}:{}.\n\
                     Use StoreFileKind::from_path so malformed segment names, cold-start artifacts, snapshots, and lifecycle cleanup share one store-owned classifier.",
                    line_no + 1
                );
            }
        }
    }
    Ok(())
}

fn read_dir_entry_error_is_swallowed(lines: &[&str], line_no: usize) -> bool {
    if !line_swallows_iterator_error(lines[line_no]) {
        return false;
    }

    let start = line_no.saturating_sub(4);
    let end = (line_no + 5).min(lines.len());
    lines[start..end]
        .iter()
        .any(|line| code_line_contains(line, "read_dir("))
}

fn local_segment_extension_classification(lines: &[&str], line_no: usize) -> bool {
    let start = line_no.saturating_sub(3);
    let end = (line_no + 4).min(lines.len());
    let window = &lines[start..end];
    window
        .iter()
        .any(|line| code_line_contains(line, ".extension()"))
        && window
            .iter()
            .any(|line| code_line_contains(line, "SEGMENT_EXTENSION"))
}

fn line_swallows_iterator_error(line: &str) -> bool {
    code_line_contains(line, ".filter_map(Result::ok)")
        || (code_line_contains(line, ".filter_map(|") && code_line_contains(line, ".ok())"))
        || code_line_contains(line, ".flatten()")
}

fn code_line_contains(line: &str, needle: &str) -> bool {
    let trimmed = line.trim_start();
    !(trimmed.starts_with("//") || trimmed.starts_with("///") || trimmed.starts_with("//!"))
        && line.contains(needle)
}

#[derive(Debug, Eq, PartialEq)]
struct InlineTestIsland {
    start_line: usize,
    end_line: usize,
    nonblank_lines: usize,
}

fn inline_test_islands(file: &syn::File, source_lines: &[&str]) -> Vec<InlineTestIsland> {
    let mut islands = Vec::new();
    collect_inline_test_islands(&file.items, source_lines, &mut islands);
    islands
}

fn collect_inline_test_islands(
    items: &[syn::Item],
    source_lines: &[&str],
    islands: &mut Vec<InlineTestIsland>,
) {
    for item in items {
        let syn::Item::Mod(module) = item else {
            continue;
        };
        if module.ident == "tests" && module.content.is_some() {
            let span = module.span();
            let start_line = span.start().line;
            let end_line = span.end().line;
            islands.push(InlineTestIsland {
                start_line,
                end_line,
                nonblank_lines: nonblank_line_count_in_range(source_lines, start_line, end_line),
            });
        }
        if let Some((_, nested_items)) = &module.content {
            collect_inline_test_islands(nested_items, source_lines, islands);
        }
    }
}

fn nonblank_line_count(content: &str) -> usize {
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
}

fn nonblank_line_count_in_range(lines: &[&str], start_line: usize, end_line: usize) -> usize {
    if start_line == 0 || end_line < start_line {
        return 0;
    }
    lines
        .iter()
        .skip(start_line - 1)
        .take(end_line - start_line + 1)
        .filter(|line| !line.trim().is_empty())
        .count()
}

pub(crate) fn production_rust_files(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = BTreeSet::new();
    for root in production_rust_roots(repo_root)? {
        paths.extend(rust_files(&root));
    }
    Ok(paths.into_iter().collect())
}

/// True when `attrs` carries `#[cfg(test)]`. Shared with the complexity gate so
/// both detectors skip the same test-only module surface.
pub(crate) fn module_is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    is_cfg_test(attrs)
}

#[cfg(test)]
mod tests {
    use super::{
        inline_test_islands, local_segment_extension_classification, nonblank_line_count,
        read_dir_entry_error_is_swallowed,
    };

    #[test]
    fn file_size_pressure_counts_nonblank_lines() {
        assert_eq!(nonblank_line_count("one\n\n  \n two\n"), 2);
    }

    #[test]
    fn inline_test_island_detection_counts_inline_tests_only() {
        let source = r#"
mod production {
    fn helper() {}

    mod tests {
        #[test]
        fn nested_island() {}

    }
}

mod tests {
    #[test]
    fn top_level_island() {}

}

mod external_tests;
"#;
        let file = syn::parse_file(source).expect("parse fixture");
        let lines = source.lines().collect::<Vec<_>>();
        let islands = inline_test_islands(&file, &lines);

        assert_eq!(islands.len(), 2);
        assert_eq!(islands[0].nonblank_lines, 4);
        assert_eq!(islands[1].nonblank_lines, 4);
    }

    #[test]
    fn read_dir_swallow_gate_is_scoped_to_directory_entries() {
        let unrelated = [
            "fn helper(items: Vec<Option<u8>>) {",
            "    let _values = items.into_iter().flatten().collect::<Vec<_>>();",
            "}",
        ];
        assert!(!read_dir_entry_error_is_swallowed(&unrelated, 1));

        let directory = [
            "fn helper(path: &Path) {",
            "    let _paths = std::fs::read_dir(path)",
            "        .expect(\"read dir\")",
            "        .flatten()",
            "        .map(|entry| entry.path())",
            "        .collect::<Vec<_>>();",
            "}",
        ];
        assert!(read_dir_entry_error_is_swallowed(&directory, 3));
    }

    #[test]
    fn segment_classification_gate_allows_generation_but_rejects_extension_tests() {
        let generation = [
            "fn segment_path(segment_id: u64) -> String {",
            "    format!(\"{segment_id:06}.{}\", SEGMENT_EXTENSION)",
            "}",
        ];
        assert!(!local_segment_extension_classification(&generation, 1));

        let classification = [
            "fn helper(path: &Path) -> bool {",
            "    path",
            "        .extension()",
            "        .map(|ext| ext == segment::SEGMENT_EXTENSION)",
            "        .unwrap_or(false)",
            "}",
        ];
        assert!(local_segment_extension_classification(&classification, 2));
    }
}
