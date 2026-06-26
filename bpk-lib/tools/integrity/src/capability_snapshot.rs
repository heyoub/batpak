//! GAUNT-CAPSNAP — the anti-nerf / anti-hollow capability FLOOR ("honest =
//! invariant"). The keystone that makes every other "this capability is real"
//! claim NON-REGRESSABLE.
//!
//! THE LIABILITY THIS CLOSES. A backend's `support_matrix()` is the FAMILY
//! ASPIRATION table: per `RequirementKind` it advertises an `Enforcement`
//! grade (Enforced/Mediated/Unsupported) and the evidence it can witness. A
//! consumer targeting a regulated industry reads that table as a CONTRACT. The
//! catastrophic, owner-undetectable failure is a SILENT NERF: a "made it honest"
//! edit that quietly flips a cell Enforced->Mediated->Unsupported, drops an
//! evidence claim, deletes a (backend,kind) row, or un-proves a witnessed
//! invariant — agreeing doc-and-code so no drift detector fires, discovered only
//! by a downstream consumer in production. This module + its two partners make
//! that path loud, typed, and approval-gated.
//!
//! THREE COOPERATING PIECES (no single one is sufficient; together airtight —
//! the same shape as `mutation_debt` + `meta_gate`):
//!
//! 1. THIS structural check (every run, fresh-checkout, no git): the committed
//!    `traceability/capability_snapshot.yaml` MUST be an EXACT MIRROR of the
//!    source-derived truth — the literal best-case verdicts written in each
//!    backend's `support_matrix()` (AST, not the probe-dependent runtime
//!    `profile()`), plus the witnessed-invariant set from the docs catalog. Any
//!    drift FAILS with a regenerate instruction. This guarantees the snapshot
//!    ALWAYS tells the truth about what the family advertises — so a source nerf
//!    cannot desync from the snapshot, and a hand-edited snapshot cannot desync
//!    from source. HARDENING: a `support_matrix()` that exists but yields ZERO
//!    rows is RED (a refactor-to-helper or a parser break is itself the silent
//!    nerf vector this gate exists to stop — never let it pass as "empty").
//!
//! 2. `meta_gate::detect_capability_downgrade` (PR diff): the ANTI-NERF
//!    authority. On the committed snapshot's diff, an enforcement rank that
//!    DECREASED, a removed (backend,kind) row, a removed evidence claim, or a
//!    witnessed `true->false` is a [`crate::meta_gate::WeakeningKind::CapabilityDowngraded`]
//!    weakening — cleared by the meta-gate's standard two-person
//!    `GAUNTLET-WEAKEN-OK` approval (L4 for a drop to `Unsupported` / a removed
//!    row). (Mirror-check #1 keeps the diff truthful; the approval lives here.)
//!
//! 3. `WaiverKind::CapabilityDowngrade` (typed, expiring, owned, ADR-anchored):
//!    the DURABLE justification record a sanctioned downgrade takes — owner +
//!    expiry + ADR + (L4) independent sign-off, validated like every waiver.
//!    ADDING such a waiver is itself an approval-gated weakening (meta_gate's
//!    `detect_waiver_additions`), and [`check`] validates each waiver target names
//!    a real `backend:kind` cell (anti-rot). It forces "complete the capability OR
//!    justify the gap with an expiry"; it is never a silent permanent carve-out.
//!    (The meta-gate does NOT special-case the waiver as a same-diff downgrade
//!    clearance — it stays pure-diff; the approval path above is what clears the
//!    `CapabilityDowngraded` finding.)
//!
//! WHY MIRROR-AND-NOT-RATCHET HERE. Monotonic "floor only moves up" cannot be
//! enforced on a stateless fresh checkout without git history (a hand-lowered
//! floor is indistinguishable from an always-low floor). So the stateless layer
//! keeps the snapshot a faithful mirror (no desync possible), and the
//! git/diff-aware `meta_gate` is where weakening is judged — exactly how the
//! repo's mutation ratchet splits its baseline check from its meta-gate.

use crate::repo_surface::ensure;
use crate::source_cache::SourceCache;
use anyhow::{bail, Context, Result};
use std::path::Path;
use syn::visit::Visit;

#[cfg(test)]
#[path = "capability_snapshot_tests.rs"]
mod capability_snapshot_tests;

/// Repo-relative path to the committed capability floor.
pub(crate) const SNAPSHOT_REL: &str = "traceability/capability_snapshot.yaml";

/// The four platform backends whose `support_matrix()` best-case tables are the
/// family aspiration surface. `inert` (the no-op test backend) is deliberately
/// excluded: it is not a platform contract a consumer reads.
const BACKENDS: &[&str] = &["linux", "wasm", "windows", "macos"];

/// One advertised capability cell: a `(backend, kind)` best-case verdict.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CeilingRow {
    pub(crate) backend: String,
    pub(crate) kind: String,
    pub(crate) enforcement: String,
    /// Evidence claims, sorted, deduplicated.
    pub(crate) evidence: Vec<String>,
}

/// One witnessed-invariant cell: whether the catalog invariant carries a
/// resolved `witness_test`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct WitnessRow {
    pub(crate) id: String,
    pub(crate) witnessed: bool,
}

/// The full committed snapshot: the capability floor + the witnessed-invariant
/// floor. Both lists are kept sorted so the on-disk form is deterministic and
/// line-stable (each cell is exactly one diff-able line).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Snapshot {
    pub(crate) ceilings: Vec<CeilingRow>,
    pub(crate) witnesses: Vec<WitnessRow>,
}

impl Snapshot {
    fn sorted(mut self) -> Self {
        self.ceilings.sort();
        self.witnesses.sort();
        self
    }
}

/// The security-order rank of an enforcement grade (Enforced strongest). Used by
/// `meta_gate` to decide whether a diff lowered a cell. Unknown strings rank
/// below `Unsupported` so a typo/garbage value never reads as "stronger".
#[must_use]
pub(crate) fn enforcement_rank(enforcement: &str) -> u8 {
    match enforcement {
        "Enforced" => 3,
        "Mediated" => 2,
        "Unsupported" => 1,
        _ => 0,
    }
}

/// Top-level entry: derive the snapshot from source, then either WRITE it to
/// `capability_snapshot.yaml` (`check == false`) or assert the committed file
/// already matches (`check == true`, folded into `structural-check`).
pub(crate) fn run(repo_root: &Path, check: bool) -> Result<()> {
    let derived = derive_snapshot(repo_root)?.sorted();
    let rendered = render(&derived);
    let path = repo_root.join(SNAPSHOT_REL);

    if check {
        return check_gate(repo_root, &derived);
    }
    if std::fs::read_to_string(&path).ok().as_deref() != Some(rendered.as_str()) {
        std::fs::write(&path, &rendered).with_context(|| format!("write {SNAPSHOT_REL}"))?;
        outln!(
            "capability-snapshot: regenerated {SNAPSHOT_REL} ({} ceiling cell(s), {} witness cell(s))",
            derived.ceilings.len(),
            derived.witnesses.len()
        );
    } else {
        outln!(
            "capability-snapshot: {SNAPSHOT_REL} already current ({} ceiling cell(s))",
            derived.ceilings.len()
        );
    }
    Ok(())
}

/// Production gate entry, folded into `structural::run`.
pub(crate) fn check(repo_root: &Path) -> Result<()> {
    let derived = derive_snapshot(repo_root)?.sorted();
    check_gate(repo_root, &derived)
}

/// The full gate: assert the committed floor mirrors source AND validate that
/// every `CapabilityDowngrade` waiver targets a real `backend:kind` cell (a
/// waiver for a vanished cell is stale debt). Takes a pre-derived snapshot so the
/// `run`/`check` paths don't re-walk the AST.
fn check_gate(repo_root: &Path, derived: &Snapshot) -> Result<()> {
    let committed = std::fs::read_to_string(repo_root.join(SNAPSHOT_REL))
        .with_context(|| format!("read {SNAPSHOT_REL}"))?;
    assert_mirror(&committed, derived)?;
    validate_downgrade_waiver_targets(repo_root, derived)?;
    outln!(
        "capability-snapshot: ok ({} ceiling cell(s), {} witness cell(s), mirror current)",
        derived.ceilings.len(),
        derived.witnesses.len()
    );
    Ok(())
}

/// Anti-rot: every `CapabilityDowngrade` waiver's `target` must name a real
/// `backend:kind` cell present in the floor. A waiver for a non-existent cell is
/// stale debt masking a moved/renamed capability — fail so the justification
/// ledger stays honest. This is the waiver kind's structural consumer.
fn validate_downgrade_waiver_targets(repo_root: &Path, derived: &Snapshot) -> Result<()> {
    let waivers = crate::typed_waivers::load_waivers(repo_root)?;
    let targets = crate::typed_waivers::targets_for(
        &waivers,
        crate::typed_waivers::WaiverKind::CapabilityDowngrade,
    );
    for target in &targets {
        let (backend, kind) = target.split_once(':').with_context(|| {
            format!(
                "capability-snapshot: CapabilityDowngrade waiver target `{target}` must be \
                 `backend:kind` (e.g. `linux:ExposePath`)"
            )
        })?;
        let exists = derived
            .ceilings
            .iter()
            .any(|c| c.backend == backend && c.kind == kind);
        ensure(
            exists,
            format!(
                "capability-snapshot: CapabilityDowngrade waiver target `{target}` names no real \
                 cell in the floor — the capability was renamed/removed; fix or retire the waiver."
            ),
        )?;
    }
    Ok(())
}

/// The testable mirror assertion: the committed snapshot TEXT must parse to the
/// same (sorted) [`Snapshot`] the source derives. Split out so a red fixture can
/// exercise the real failure path with a planted-downgrade committed text rather
/// than a proxy for it.
pub(crate) fn assert_mirror(committed_text: &str, derived: &Snapshot) -> Result<()> {
    let committed = parse(committed_text)
        .with_context(|| format!("parse committed {SNAPSHOT_REL}"))?
        .sorted();
    let derived = derived.clone().sorted();
    ensure(
        committed == derived,
        format!(
            "capability-snapshot: {SNAPSHOT_REL} is STALE — it no longer mirrors the source \
             `support_matrix()` best-case tables (or the witnessed-invariant set).\n\
             Regenerate it with `cargo run -p batpak-integrity -- capability-snapshot` (or \
             `cargo xtask capability-snapshot`).\n\
             If the change LOWERED a capability (enforcement weakened, a row/evidence claim \
             removed, or a witness un-proved), that is a CAPABILITY DOWNGRADE: complete the \
             capability so the matrix becomes true again, or file a typed CapabilityDowngrade \
             waiver (owner + expiry + ADR). The meta-gate blocks an unapproved downgrade diff."
        ),
    )
}

/// Derive the live snapshot from source: AST-walk each backend's
/// `support_matrix()` for its best-case `insert(...)` cells, and read the
/// witnessed-invariant set from the docs catalog.
pub(crate) fn derive_snapshot(repo_root: &Path) -> Result<Snapshot> {
    let mut cache = SourceCache::new(repo_root);
    let mut ceilings = Vec::new();
    for backend in BACKENDS {
        ceilings.extend(derive_backend_ceilings(repo_root, backend, &mut cache)?);
    }
    let witnesses = derive_witnesses(repo_root)?;
    Ok(Snapshot {
        ceilings,
        witnesses,
    })
}

/// Walk one backend's `support_matrix()` and extract its best-case cells.
/// HARDENING: an existing `support_matrix()` that yields zero cells is RED — a
/// refactor that moved the inserts behind a helper/loop, or a parse failure,
/// would silently empty the floor and defeat the whole gate.
fn derive_backend_ceilings(
    repo_root: &Path,
    backend: &str,
    cache: &mut SourceCache,
) -> Result<Vec<CeilingRow>> {
    let rel = format!("crates/bvisor/src/backend/{backend}/mod.rs");
    let abs = repo_root.join(&rel);
    let file = cache
        .parse_rust(&abs)
        .with_context(|| format!("parse backend support matrix {rel}"))?;
    extract_ceilings(backend, &file)
}

/// Pure extractor: walk a parsed backend file's `support_matrix()` for its
/// best-case cells. HARDENING: a `support_matrix()` that yields ZERO cells is RED
/// — a refactor that moved the inserts behind a helper/loop (or a parse that lost
/// them) would silently empty the floor and defeat the whole gate. Split from IO
/// so the empty-guard is directly testable.
fn extract_ceilings(backend: &str, file: &syn::File) -> Result<Vec<CeilingRow>> {
    let mut visitor = SupportMatrixVisitor {
        in_support_matrix: false,
        cells: Vec::new(),
    };
    visitor.visit_file(file);

    ensure(
        !visitor.cells.is_empty(),
        format!(
            "capability-snapshot: backend `{backend}` has a `support_matrix()` but the AST walk \
             extracted ZERO best-case cells. The literal `insert(&mut best, RequirementKind::X, \
             Enforcement::Y, &[..])` shape was refactored away (or the file failed to parse) — the \
             capability floor cannot be derived, which would silently empty the anti-nerf snapshot. \
             Restore the literal inserts or extend the extractor."
        ),
    )?;

    Ok(visitor
        .cells
        .into_iter()
        .map(|(kind, enforcement, mut evidence)| {
            evidence.sort();
            evidence.dedup();
            CeilingRow {
                backend: backend.to_string(),
                kind,
                enforcement,
                evidence,
            }
        })
        .collect())
}

/// The witnessed-invariant floor: one row per catalog invariant, `witnessed` =
/// it declares a `witness_test`. A previously-witnessed invariant losing its
/// witness is a downgrade the meta-gate catches.
fn derive_witnesses(repo_root: &Path) -> Result<Vec<WitnessRow>> {
    let invariants =
        crate::docs_catalog::load_catalog(repo_root).context("load invariants catalog")?;
    Ok(invariants
        .into_iter()
        .map(|inv| WitnessRow {
            id: inv.id,
            witnessed: inv.witness_test.is_some(),
        })
        .collect())
}

/// syn visitor that collects the `(kind, enforcement, evidence)` triples from the
/// `insert(&mut best, RequirementKind::K, Enforcement::E, &[EvidenceClaim::C, ..])`
/// calls inside the `support_matrix()` function body (and nowhere else — test
/// assertions in the same file also call methods named `insert`/use the enums).
struct SupportMatrixVisitor {
    in_support_matrix: bool,
    cells: Vec<(String, String, Vec<String>)>,
}

impl<'ast> Visit<'ast> for SupportMatrixVisitor {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if node.sig.ident == "support_matrix" {
            self.in_support_matrix = true;
            syn::visit::visit_block(self, &node.block);
            self.in_support_matrix = false;
        }
        // Do not descend into other top-level fns (e.g. the `insert` helper, unit
        // tests) — only `support_matrix`'s body authors the best-case table.
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if self.in_support_matrix {
            if let Some(cell) = parse_insert_call(node) {
                self.cells.push(cell);
            }
        }
        syn::visit::visit_expr_call(self, node);
    }
}

/// Match `insert(&mut best, RequirementKind::K, Enforcement::E, &[EvidenceClaim::C, ..])`
/// and return `(K, E, [C, ..])`. Returns `None` for any call that is not the
/// 4-argument best-case insert (so unrelated calls are ignored, not misread).
fn parse_insert_call(node: &syn::ExprCall) -> Option<(String, String, Vec<String>)> {
    let syn::Expr::Path(func) = node.func.as_ref() else {
        return None;
    };
    if !path_ends_with(&func.path, "insert") {
        return None;
    }
    let args: Vec<&syn::Expr> = node.args.iter().collect();
    let [_table, kind_arg, enforcement_arg, evidence_arg] = args.as_slice() else {
        return None;
    };
    let kind = enum_variant(kind_arg, "RequirementKind")?;
    let enforcement = enum_variant(enforcement_arg, "Enforcement")?;
    let evidence = evidence_slice(evidence_arg)?;
    Some((kind, enforcement, evidence))
}

/// Extract `Variant` from a `Path::To::Enum::Variant` expression, requiring the
/// path to mention `enum_name` so a stray identifier never reads as a verdict.
fn enum_variant(expr: &syn::Expr, enum_name: &str) -> Option<String> {
    let syn::Expr::Path(path_expr) = expr else {
        return None;
    };
    let segments: Vec<String> = path_expr
        .path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect();
    if !segments.iter().any(|s| s == enum_name) {
        return None;
    }
    segments.last().cloned()
}

/// Extract the evidence-claim variant names from a `&[EvidenceClaim::A, ..]` (or
/// an empty `&[]`) reference-to-array-literal expression.
fn evidence_slice(expr: &syn::Expr) -> Option<Vec<String>> {
    // `&[..]` is an `Expr::Reference`; an unparenthesized `[..]` is a bare
    // `Expr::Array`. Peel one reference layer if present (no wildcard match arm —
    // `syn::Expr` is foreign + `#[non_exhaustive]`).
    let inner = if let syn::Expr::Reference(reference) = expr {
        reference.expr.as_ref()
    } else {
        expr
    };
    let syn::Expr::Array(array) = inner else {
        return None;
    };
    let mut claims = Vec::new();
    for elem in &array.elems {
        claims.push(enum_variant(elem, "EvidenceClaim")?);
    }
    Some(claims)
}

fn path_ends_with(path: &syn::Path, ident: &str) -> bool {
    path.segments
        .last()
        .is_some_and(|segment| segment.ident == ident)
}

// ── Deterministic on-disk form (flow-style, one cell per line) ──────────────

/// Render the snapshot to its canonical, line-stable YAML. Flow-style mappings
/// keep each cell on exactly ONE line so a capability downgrade shows up as a
/// clean one-line replacement in the diff the meta-gate reads.
pub(crate) fn render(snapshot: &Snapshot) -> String {
    let mut out = String::new();
    out.push_str(
        "# capability_snapshot.yaml — the anti-nerf capability FLOOR (GAUNT-CAPSNAP).\n\
         # GENERATED: an exact mirror of each backend's `support_matrix()` best-case table\n\
         # plus the witnessed-invariant set. Do NOT hand-edit; run\n\
         # `cargo xtask capability-snapshot` (or `cargo run -p batpak-integrity -- \
         capability-snapshot`).\n\
         # A weakening diff here (enforcement lowered, a row/evidence claim removed, a witness\n\
         # un-proved) is a CAPABILITY DOWNGRADE the meta-gate blocks without two-person approval\n\
         # or a typed CapabilityDowngrade waiver.\n",
    );
    out.push_str("ceilings:\n");
    for cell in &snapshot.ceilings {
        let evidence = cell.evidence.join(", ");
        out.push_str(&format!(
            "  - {{ backend: {}, kind: {}, enforcement: {}, evidence: [{}] }}\n",
            cell.backend, cell.kind, cell.enforcement, evidence
        ));
    }
    out.push_str("witnesses:\n");
    for row in &snapshot.witnesses {
        out.push_str(&format!(
            "  - {{ id: {}, witnessed: {} }}\n",
            row.id, row.witnessed
        ));
    }
    out
}

/// Parse a committed snapshot back into [`Snapshot`]. Tolerant of the canonical
/// flow-style form this module writes; rejects anything malformed so a corrupted
/// floor is a finding, not a silent empty mirror.
pub(crate) fn parse(text: &str) -> Result<Snapshot> {
    let mut snapshot = Snapshot::default();
    let mut section = Section::None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "ceilings:" {
            section = Section::Ceilings;
            continue;
        }
        if line == "witnesses:" {
            section = Section::Witnesses;
            continue;
        }
        let body = line
            .strip_prefix("- ")
            .and_then(|l| l.strip_prefix('{'))
            .and_then(|l| l.strip_suffix('}'))
            .map(str::trim)
            .with_context(|| format!("capability-snapshot: malformed row `{line}`"))?;
        match section {
            Section::Ceilings => snapshot.ceilings.push(parse_ceiling(body)?),
            Section::Witnesses => snapshot.witnesses.push(parse_witness(body)?),
            Section::None => bail!("capability-snapshot: row before any section header: `{line}`"),
        }
    }
    Ok(snapshot)
}

enum Section {
    None,
    Ceilings,
    Witnesses,
}

fn parse_ceiling(body: &str) -> Result<CeilingRow> {
    let fields = parse_fields(body);
    let evidence_raw = field(&fields, "evidence", body)?;
    let evidence: Vec<String> = evidence_raw
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    Ok(CeilingRow {
        backend: field(&fields, "backend", body)?,
        kind: field(&fields, "kind", body)?,
        enforcement: field(&fields, "enforcement", body)?,
        evidence,
    })
}

fn parse_witness(body: &str) -> Result<WitnessRow> {
    let fields = parse_fields(body);
    let witnessed = field(&fields, "witnessed", body)? == "true";
    Ok(WitnessRow {
        id: field(&fields, "id", body)?,
        witnessed,
    })
}

/// Split `k: v, k: v, evidence: [a, b]` into `(key, value)` pairs, treating a
/// bracketed list as a single value (commas inside `[..]` are not separators).
fn parse_fields(body: &str) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for ch in body.chars() {
        match ch {
            '[' => {
                depth += 1;
                current.push(ch);
            }
            ']' => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => {
                push_field(&mut fields, &current);
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    push_field(&mut fields, &current);
    fields
}

fn push_field(fields: &mut Vec<(String, String)>, raw: &str) {
    if let Some((k, v)) = raw.split_once(':') {
        fields.push((k.trim().to_string(), v.trim().to_string()));
    }
}

fn field(fields: &[(String, String)], key: &str, body: &str) -> Result<String> {
    fields
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
        .with_context(|| format!("capability-snapshot: row `{body}` is missing field `{key}`"))
}
