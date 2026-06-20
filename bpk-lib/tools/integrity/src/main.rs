//! Integrity tool - executable invariants for the batpak repo.
//!
//! # `// justifies:` comment convention
//!
//! Every `#[allow(...)]` in the repo's runtime, tool, and build-script
//! surfaces must carry a `// justifies: ...` comment either on the same line
//! or on the line immediately preceding the attribute. The comment body must:
//!
//! 1. Start with the literal prefix `justifies:` (after an optional `//` and
//!    whitespace).
//! 2. Contain at least five whitespace-separated words after the prefix,
//!    naming the design decision that makes the silencer safe.
//! 3. Cite at least one resolvable anchor - an `INV-<NAME>` from
//!    `traceability/invariants.yaml`, an `ADR-NNNN` whose file exists as a
//!    root ADR file, or a concrete in-repo path (`src/...`, `tests/...`,
//!    `examples/...`, `crates/macros/...`, `crates/macros-support/...`,
//!    `build.rs`) whose file exists. Multiple anchors are fine; at least one
//!    must resolve.
//!
//! Narrative prose without that structure counts as silence, not design.
//! INV-ALLOW-IS-DESIGN is the meta-invariant. This tool lints itself; every
//! justification below is load-bearing and is checked by
//! `structural::check_allow_justifications` on every run.
//! dead_code silencers are not tolerated in this repo; test-only code uses
//! `cfg(test)`, unused code is deleted, and shared helpers get restructured.
// justifies: INV-ALLOW-IS-DESIGN; batpak-integrity is a repository command-line tool and its check subcommands intentionally report human and CI status messages from tools/integrity/src/main.rs.
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod agent_doctor;
mod agent_surface;
mod architecture_ir;
mod architecture_lints;
mod assurance;
mod ci_parity;
mod complexity;
mod docs_catalog;
mod doctor;
mod evidence_audit;
mod gate_registry;
mod glob_coverage;
mod harness_lints;
mod invariant_bridge;
mod meta_gate;
mod public_surface;
mod receipts;
mod repo_ir;
mod repo_surface;
mod rust_ast;
mod source_cache;
mod store_pub_fn_coverage;
mod structural;
mod traceability;
mod triangulation;
mod typed_waivers;
mod wallclock;

#[path = "../../shared/shared_checks.rs"]
mod shared_checks;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(author, version, about = "Executable integrity checks for batpak")]
struct Cli {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Subcommand)]
enum CommandKind {
    Doctor {
        #[arg(long)]
        strict: bool,
    },
    TraceabilityCheck,
    StructuralCheck,
    /// Verify every registered gate emitted a non-vacuous execution receipt
    /// (`target/gauntlet-receipts/*.json`). Fails on a missing or zero-count
    /// (vacuous-pass) receipt; `SKIPPED_PACKAGED` receipts may carry zero counts.
    GauntletReceiptsPresent,
    /// Enforce the DO-178B tool-qualification law: no gate may have blocking
    /// authority without naming an existing red-fixture test. Reports any
    /// blocking gate that lacks a red fixture (a finding, not a failure path).
    GateRegistryCheck,
    /// Agent-safety meta-gate (P1-4): classify a `base..HEAD` diff and FAIL if it
    /// WEAKENS the assurance machinery without the required human approval. The
    /// pure classifier lives in `meta_gate.rs`; this subcommand is the
    /// integrity-side entry that the `cargo xtask meta-gate` shell calls.
    MetaGateCheck {
        /// Path to a file containing the unified diff (`git diff base..HEAD`).
        /// When omitted, the diff is read from stdin.
        #[arg(long)]
        diff_file: Option<std::path::PathBuf>,
        /// A PR label (repeatable). The human-applied `gauntlet-weaken-approved`
        /// label authorizes a weakening; CI cannot self-apply it.
        #[arg(long = "label")]
        labels: Vec<String>,
        /// The PR author login (for the L4 two-person rule).
        #[arg(long)]
        pr_author: Option<String>,
        /// Path to a file containing the PR's commit messages (e.g.
        /// `git log base..HEAD`). `GAUNTLET-WEAKEN-OK:` trailers and their commit
        /// authors are parsed from it. Optional; absent => no trailers.
        #[arg(long)]
        commits_file: Option<std::path::PathBuf>,
    },
    /// Triangulation harness (GAUNTLET-TRIANGULATION): cross-check independent
    /// oracles over non-type repo facts; a disagreement is a hard finding. The
    /// wired fact is workspace crate-graph acyclicity (cargo-metadata + Tarjan
    /// vs. manifest-scan). Also folded into `structural-check`.
    TriangulationCheck,
    /// Static checks for evidence report bodies and public export vocabulary.
    EvidenceAudit,
    /// Validate the machine-readable agent intent/API/test surface map.
    AgentSurfaceCheck,
    /// Fast agent-oriented repository doctor with stable repair IDs.
    AgentDoctor,
    /// Emit the repo architecture IR used by docs, agents, and drift queries.
    ArchitectureIr {
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        #[arg(long)]
        check: bool,
    },
    /// GAUNTLET-DOCS-CURRENCY: regenerate (or `--check`) the auto-generated INV
    /// catalog block in `INVARIANTS.md` from `traceability/invariants.yaml`, and
    /// enforce the per-INV `witness_test` strong-tier citation gate. `--check`
    /// fails on drift instead of rewriting; it is folded into `structural-check`.
    DocsCatalog {
        #[arg(long)]
        check: bool,
    },
    /// GAUNTLET-REPO-IR (Phase 3, item 6): emit the minimal queryable repo-IR as
    /// JSON. ONE column-store binding AL assignments + gate ownership + waiver
    /// ownership + public-surface map + mutation-seam map + docs traceability,
    /// over which fitness functions fold (banana-split-fused: one traversal, N
    /// checks). `--out` writes to a file; default prints to stdout.
    RepoIr {
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        CommandKind::Doctor { strict } => doctor::run(strict),
        CommandKind::TraceabilityCheck => traceability::run(),
        CommandKind::StructuralCheck => structural::run(),
        CommandKind::GauntletReceiptsPresent => {
            let validated = receipts::check_present(gate_registry::RECEIPT_REQUIRED_GATES)?;
            println!(
                "gauntlet-receipts-present: ok ({} non-vacuous receipt(s) validated)",
                validated.len()
            );
            Ok(())
        }
        CommandKind::GateRegistryCheck => {
            let repo_root = repo_surface::repo_root()?;
            gate_registry::check(&repo_root)?;
            gate_registry::report(&repo_root);
            Ok(())
        }
        CommandKind::MetaGateCheck {
            diff_file,
            labels,
            pr_author,
            commits_file,
        } => run_meta_gate(diff_file, labels, pr_author, commits_file),
        CommandKind::TriangulationCheck => triangulation::check(&repo_surface::repo_root()?),
        CommandKind::EvidenceAudit => evidence_audit::run(&repo_surface::repo_root()?),
        CommandKind::AgentSurfaceCheck => agent_surface::run(&repo_surface::repo_root()?),
        CommandKind::AgentDoctor => agent_doctor::run(&repo_surface::repo_root()?),
        CommandKind::ArchitectureIr { out, check } => {
            architecture_ir::run(&repo_surface::repo_root()?, out, check)
        }
        CommandKind::DocsCatalog { check } => docs_catalog::run(&repo_surface::repo_root()?, check),
        CommandKind::RepoIr { out } => repo_ir::run(&repo_surface::repo_root()?, out),
    }
}

/// Read the unified diff (from `--diff-file` or stdin), assemble the approval
/// context from the labels / PR author / commit messages, and evaluate the
/// meta-gate. The classification and approval logic live in `meta_gate`; this is
/// the thin I/O shell.
fn run_meta_gate(
    diff_file: Option<std::path::PathBuf>,
    labels: Vec<String>,
    pr_author: Option<String>,
    commits_file: Option<std::path::PathBuf>,
) -> Result<()> {
    use std::io::Read;
    let diff = match diff_file {
        Some(path) => std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("read diff file {}: {e}", path.display()))?,
        None => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| anyhow::anyhow!("read diff from stdin: {e}"))?;
            buf
        }
    };
    let weaken_ok_trailers = match commits_file {
        Some(path) => {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| anyhow::anyhow!("read commits file {}: {e}", path.display()))?;
            meta_gate::parse_weaken_trailers(&text)
        }
        None => Vec::new(),
    };
    let ctx = meta_gate::ApprovalContext {
        labels,
        pr_author,
        weaken_ok_trailers,
    };
    let repo_root = repo_surface::repo_root()?;
    let l4_entries = meta_gate::load_l4_entries(&repo_root);
    meta_gate::evaluate(&diff, &l4_entries, &ctx)?;
    println!("meta-gate: ok (no unapproved weakening detected)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::ci_parity::{dockerfile_tool_pins, workflow_list_values};
    use crate::shared_checks::{
        extract_anchors, justification_body, line_carries_justification, load_known_invariants,
        public_item_names, JustifiesAnchor,
    };
    use crate::traceability::validate_observation_evidence;
    use std::path::Path;

    #[test]
    fn public_item_names_collects_async_use_const_type_and_reexports() {
        let source = r#"
            pub const FLAG: u8 = 1;
            pub type Alias = u64;
            pub mod nested {}
            pub use crate::store::StoreError as PublicStoreError;

            pub struct Thing;
            impl Thing {
                pub async fn subscribe(&self) {}
            }
        "#;

        // justifies: INV-TEST-PANIC-AS-ASSERTION; setup panics signal fixture breakage, see tools/integrity/src/main.rs
        let file = syn::parse_file(source).expect("parse source");
        let names = public_item_names(&file);

        assert!(names.contains("FLAG"));
        assert!(names.contains("Alias"));
        assert!(!names.contains("nested"));
        assert!(names.contains("PublicStoreError"));
        assert!(names.contains("subscribe"));
    }

    #[test]
    fn workflow_list_values_parses_feature_matrix_strings() {
        let workflow = r#"
matrix:
  features:
    - ""
    - "--features dangerous-test-hooks"
    - "--all-features"
"#;

        // justifies: INV-TEST-PANIC-AS-ASSERTION; setup panics signal fixture breakage in tools/integrity/src/main.rs
        let values = workflow_list_values(workflow, "features").expect("parse values");
        assert_eq!(
            values,
            vec![
                "".to_string(),
                "--features dangerous-test-hooks".to_string(),
                "--all-features".to_string()
            ]
        );
    }

    #[test]
    fn dockerfile_tool_pins_are_collected_dynamically() {
        let pins = dockerfile_tool_pins(
            r#"
RUN cargo binstall --no-confirm cargo-deny@0.19.0 || cargo install --locked cargo-deny@0.19.0
RUN cargo install --locked cargo-mutants@27.0.0
"#,
        )
        .expect("parse pins");
        assert_eq!(pins.get("cargo-deny").map(String::as_str), Some("0.19.0"));
        assert_eq!(
            pins.get("cargo-mutants").map(String::as_str),
            Some("27.0.0")
        );
    }

    #[test]
    fn observation_evidence_requires_named_rust_function() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("crate lives under tools/integrity - two parents is the repo root")
            .to_path_buf();

        assert!(validate_observation_evidence(
            &repo_root,
            "OBS-TEST",
            "tests/durable_frontier_waits_append_gate.rs :: append_with_visible_gate_returns_after_publish",
        )
        .is_ok());

        let err = validate_observation_evidence(
            &repo_root,
            "OBS-TEST",
            "tests/durable_frontier_waits.rs :: missing_observation_evidence_function",
        )
        .expect_err("missing function must fail");
        assert!(
            err.to_string().contains("no Rust function"),
            "wrong error: {err:#}"
        );
    }

    #[test]
    fn justification_body_returns_prose_after_prefix() {
        assert_eq!(
            justification_body("// justifies: INV-FOO; narrow cast bounds checked above here"),
            Some("INV-FOO; narrow cast bounds checked above here".to_string()),
        );
        assert_eq!(
            justification_body("    let _ = 1; // justifies: INV-BAR; inline anchored rationale"),
            Some("INV-BAR; inline anchored rationale".to_string()),
        );
        assert_eq!(
            justification_body("// this is not a justifies comment"),
            None
        );
        assert_eq!(justification_body("let x = 1;"), None);
    }

    #[test]
    fn extract_anchors_finds_inv_adr_and_path_tokens() {
        let body =
            "INV-MACRO-BOUNDED-CAST and ADR-0010 plus tests/coordinate_hardening.rs:42 cover this";
        let anchors = extract_anchors(body);
        assert!(anchors.contains(&JustifiesAnchor::Invariant("INV-MACRO-BOUNDED-CAST".into())));
        assert!(anchors.contains(&JustifiesAnchor::Adr(10)));
        assert!(anchors.iter().any(
            |a| matches!(a, JustifiesAnchor::Path(p) if p.as_os_str() == "tests/coordinate_hardening.rs")
        ));
    }

    #[test]
    fn extract_anchors_rejects_non_inv_tokens() {
        let body = "TODO-MAYBE-LATER AND some-random-words and INV- by itself are not anchors";
        let anchors = extract_anchors(body);
        assert!(
            anchors.is_empty(),
            "bare words must not produce anchors; got {:?}",
            anchors
        );
    }

    #[test]
    fn line_carries_justification_requires_body_plus_anchor() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("crate lives under tools/integrity - two parents is the repo root")
            .to_path_buf();
        // justifies: INV-TEST-PANIC-AS-ASSERTION; test-only setup failure is the assertion signal in tools/integrity/src/main.rs
        let known = load_known_invariants(&repo_root).expect("load catalog");

        // good - real invariant anchor, prose is long enough
        assert!(line_carries_justification(
            "// justifies: INV-MACRO-BOUNDED-CAST; narrowing cast bounds checked in crates/macros/src/lib.rs",
            &repo_root,
            &known,
        ));

        // bad - prose but no resolvable anchor
        assert!(!line_carries_justification(
            "// justifies: this is a narrative justification with no catalog anchor or path",
            &repo_root,
            &known,
        ));

        // bad - INV-id that is not in the catalog
        assert!(!line_carries_justification(
            "// justifies: INV-NOT-A-REAL-INVARIANT-IDENTIFIER-HERE covers this site",
            &repo_root,
            &known,
        ));

        // bad - too short (< 5 words) even with an anchor
        assert!(!line_carries_justification(
            "// justifies: INV-MACRO-BOUNDED-CAST ok",
            &repo_root,
            &known,
        ));
    }
}
