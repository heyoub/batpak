use crate::repo_surface::{project_root, resolve_repo_or_core_path};
use anyhow::{bail, Context, Result};
use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn check(repo_root: &Path) -> Result<()> {
    check_docs_for_source_citations(repo_root, &root_source_citation_docs(repo_root)?)?;
    Ok(())
}

fn root_source_citation_docs(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let root = project_root(repo_root);
    let mut docs = Vec::new();
    for entry in fs::read_dir(root).with_context(|| format!("read {}", root.display()))? {
        let entry = entry.with_context(|| format!("read entry under {}", root.display()))?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if is_root_doc(file_name) {
            docs.push(path);
        }
    }
    let archive = root.join("archive/decisions");
    if archive.exists() {
        for entry in
            fs::read_dir(&archive).with_context(|| format!("read {}", archive.display()))?
        {
            let entry = entry.with_context(|| format!("read entry under {}", archive.display()))?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
                docs.push(path);
            }
        }
    }
    docs.sort();
    Ok(docs)
}

fn is_root_doc(file_name: &str) -> bool {
    matches!(
        file_name,
        "README.md"
            | "FACTORY.md"
            | "MODEL.md"
            | "INVARIANTS.md"
            | "BATTERIES.md"
            | "TERMINALS.md"
            | "EVENTS.md"
            | "RECEIPTS.md"
            | "CIRCUITS.md"
            | "REPLAY.md"
            | "PROJECTIONS.md"
            | "INTEGRATION.md"
            | "CONFORMANCE.md"
            | "CONTRIBUTING.md"
            | "AGENTS.md"
            | "CHANGELOG.md"
    )
}

fn check_docs_for_source_citations(repo_root: &Path, docs: &[PathBuf]) -> Result<()> {
    // justifies: INV-LITERAL-REGEX-UNWRAP-SAFE; literal regex pattern is compile-time-constant in tools/integrity/src/architecture_lints/source_citations.rs, unwrap safe by construction
    let citation = Regex::new(
        r"(?P<path>(?:[A-Za-z0-9_.-]+/)*[A-Za-z0-9_.-]+\.rs):(?P<start>[1-9][0-9]*)(?:-(?P<end>[1-9][0-9]*))?",
    )
    .expect("internal regex is a compile-time constant and will compile");

    for doc in docs {
        let content = fs::read_to_string(doc).with_context(|| format!("read {}", doc.display()))?;
        for (line_index, line) in content.lines().enumerate() {
            if is_cargo_mutants_record(line) {
                continue;
            }
            for cap in citation.captures_iter(line) {
                let cited = &cap["path"];
                let start = parse_line_number(&cap["start"], doc, line_index + 1)?;
                let end = cap
                    .name("end")
                    .map(|end| parse_line_number(end.as_str(), doc, line_index + 1))
                    .transpose()?
                    .unwrap_or(start);
                if end < start {
                    bail!(
                        "source citation range runs backward in {}:{}: `{}`",
                        doc_label(repo_root, doc),
                        line_index + 1,
                        cap.get(0).map(|m| m.as_str()).unwrap_or(cited)
                    );
                }

                let source = resolve_source_citation(repo_root, cited);
                if !source.is_file() {
                    bail!(
                        "source citation points at missing file in {}:{}: `{}` resolved to `{}`",
                        doc_label(repo_root, doc),
                        line_index + 1,
                        cited,
                        source.display()
                    );
                }
                let line_count = count_lines(&source)?;
                if end > line_count {
                    bail!(
                        "source citation line beyond EOF in {}:{}: `{}` cites line {} but `{}` has {} line(s)",
                        doc_label(repo_root, doc),
                        line_index + 1,
                        cap.get(0).map(|m| m.as_str()).unwrap_or(cited),
                        end,
                        source_label(repo_root, &source),
                        line_count
                    );
                }
            }
        }
    }
    Ok(())
}

fn is_cargo_mutants_record(line: &str) -> bool {
    line.contains(".rs:")
        && (line.contains("mutant `")
            || line.contains(" replace ")
            || line.contains(" delete ")
            || line.contains("delete field "))
}

fn parse_line_number(value: &str, doc: &Path, doc_line: usize) -> Result<usize> {
    value.parse::<usize>().with_context(|| {
        format!(
            "parse source citation line number `{value}` in {}:{}",
            doc.display(),
            doc_line
        )
    })
}

fn resolve_source_citation(repo_root: &Path, cited: &str) -> PathBuf {
    let normalized = cited.trim_start_matches("./");
    let project = project_root(repo_root);
    if let Some(rest) = normalized.strip_prefix("bpk-lib/") {
        return repo_root.join(rest);
    }

    let project_path = project.join(normalized);
    if project_path.exists() {
        return project_path;
    }

    resolve_repo_or_core_path(repo_root, normalized)
}

fn count_lines(path: &Path) -> Result<usize> {
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(content.lines().count())
}

fn doc_label(repo_root: &Path, doc: &Path) -> String {
    doc.strip_prefix(project_root(repo_root))
        .unwrap_or(doc)
        .to_string_lossy()
        .replace('\\', "/")
}

fn source_label(repo_root: &Path, source: &Path) -> String {
    source
        .strip_prefix(project_root(repo_root))
        .unwrap_or(source)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::check_docs_for_source_citations;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn source_citation_accepts_existing_file_and_range() -> anyhow::Result<()> {
        let sandbox = Sandbox::new("valid")?;
        sandbox.write("bpk-lib/crates/core/src/lib.rs", "one\ntwo\nthree\n")?;
        sandbox.write(
            "100_ADR_0099_SYNTHETIC.md",
            "`bpk-lib/crates/core/src/lib.rs:1-3` and `crates/core/src/lib.rs:2`",
        )?;

        check_docs_for_source_citations(
            &sandbox.repo_root(),
            &[sandbox.doc("100_ADR_0099_SYNTHETIC.md")],
        )
    }

    #[test]
    fn source_citation_rejects_missing_file() -> anyhow::Result<()> {
        let sandbox = Sandbox::new("missing")?;
        sandbox.write(
            "100_ADR_0099_SYNTHETIC.md",
            "`bpk-lib/crates/core/src/missing.rs:1`",
        )?;

        let error = check_docs_for_source_citations(
            &sandbox.repo_root(),
            &[sandbox.doc("100_ADR_0099_SYNTHETIC.md")],
        )
        .expect_err("missing source citation must fail");
        assert!(error.to_string().contains("missing file"));
        Ok(())
    }

    #[test]
    fn source_citation_rejects_line_beyond_eof() -> anyhow::Result<()> {
        let sandbox = Sandbox::new("eof")?;
        sandbox.write("bpk-lib/crates/core/src/lib.rs", "one\ntwo\n")?;
        sandbox.write(
            "100_ADR_0099_SYNTHETIC.md",
            "`bpk-lib/crates/core/src/lib.rs:1-3`",
        )?;

        let error = check_docs_for_source_citations(
            &sandbox.repo_root(),
            &[sandbox.doc("100_ADR_0099_SYNTHETIC.md")],
        )
        .expect_err("out-of-range source citation must fail");
        assert!(error.to_string().contains("beyond EOF"));
        Ok(())
    }

    #[test]
    fn source_citation_ignores_cargo_mutants_records() -> anyhow::Result<()> {
        let sandbox = Sandbox::new("mutants")?;
        sandbox.write(
            "041_TESTING_LEDGER.md",
            "exact mutant `bpk-lib/crates/core/src/store/write/control.rs:551:13 delete field fence_token from struct Self expression`",
        )?;

        check_docs_for_source_citations(
            &sandbox.repo_root(),
            &[sandbox.doc("041_TESTING_LEDGER.md")],
        )
    }

    struct Sandbox {
        root: PathBuf,
    }

    impl Sandbox {
        fn new(name: &str) -> anyhow::Result<Self> {
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
            let root = std::env::temp_dir().join(format!(
                "batpak-source-citation-{name}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(root.join("bpk-lib"))?;
            Ok(Self { root })
        }

        fn repo_root(&self) -> PathBuf {
            self.root.join("bpk-lib")
        }

        fn doc(&self, path: &str) -> PathBuf {
            self.root.join(path)
        }

        fn write(&self, relative: &str, content: &str) -> anyhow::Result<()> {
            let path = self.root.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, content)?;
            Ok(())
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            remove_dir_all_best_effort(&self.root);
        }
    }

    fn remove_dir_all_best_effort(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }
}
