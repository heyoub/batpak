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
mod architecture_lints;
mod ci_parity;
mod doctor;
mod evidence_audit;
mod harness_lints;
mod invariant_bridge;
mod public_surface;
mod repo_surface;
mod store_pub_fn_coverage;
mod structural;
mod traceability;

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
    /// Static checks for evidence report bodies and public export vocabulary.
    EvidenceAudit,
    /// Validate the machine-readable agent intent/API/test surface map.
    AgentSurfaceCheck,
    /// Fast agent-oriented repository doctor with stable repair IDs.
    AgentDoctor,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        CommandKind::Doctor { strict } => doctor::run(strict),
        CommandKind::TraceabilityCheck => traceability::run(),
        CommandKind::StructuralCheck => structural::run(),
        CommandKind::EvidenceAudit => evidence_audit::run(&repo_surface::repo_root()?),
        CommandKind::AgentSurfaceCheck => agent_surface::run(&repo_surface::repo_root()?),
        CommandKind::AgentDoctor => agent_doctor::run(&repo_surface::repo_root()?),
    }
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
            "tests/durable_frontier_waits.rs :: append_with_visible_gate_returns_after_publish",
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
