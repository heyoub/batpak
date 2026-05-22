use super::{ensure, relative};
use anyhow::{Context, Result};
use pulldown_cmark::{Event, Options, Parser, Tag};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path};

pub(super) fn check(repo_root: &Path) -> Result<()> {
    check_portable_context_links(repo_root)?;
    check_live_docs_do_not_link_archives(repo_root)?;
    check_factory_docs_use_just_commands(repo_root)?;
    check_root_doc_site_contract(repo_root)?;
    check_reference_doc_completeness(repo_root)?;
    check_changelog_migration_contract(repo_root)?;
    Ok(())
}

fn check_portable_context_links(repo_root: &Path) -> Result<()> {
    let doc_root = project_root(repo_root);
    let readme = doc_root.join("README.md");
    let factory = doc_root.join("FACTORY.md");
    let model = doc_root.join("MODEL.md");
    let invariants = doc_root.join("INVARIANTS.md");
    let conformance = doc_root.join("CONFORMANCE.md");
    let cookbook = doc_root.join("COOKBOOK.md");

    let readme_links = markdown_links(doc_root, &readme)?;
    for target in [
        "FACTORY.md",
        "MODEL.md",
        "INVARIANTS.md",
        "BATTERIES.md",
        "TERMINALS.md",
        "EVENTS.md",
        "RECEIPTS.md",
        "CIRCUITS.md",
        "REPLAY.md",
        "PROJECTIONS.md",
        "INTEGRATION.md",
        "CONFORMANCE.md",
        "COOKBOOK.md",
    ] {
        ensure(
            readme_links.contains(target),
            format!("README.md must link to canonical factory doc {target}"),
        )?;
    }

    for (label, path) in [
        ("FACTORY.md", factory),
        ("MODEL.md", model),
        ("INVARIANTS.md", invariants),
        ("CONFORMANCE.md", conformance),
        ("COOKBOOK.md", cookbook),
    ] {
        let links = markdown_links(doc_root, &path)?;
        ensure(
            !links.contains("099_DECISION_INDEX.md")
                && !links.iter().any(|link| link.contains("100_ADR_")),
            format!("{label} must not route readers through ADR lineage"),
        )?;
    }

    Ok(())
}

fn check_live_docs_do_not_link_archives(repo_root: &Path) -> Result<()> {
    let doc_root = project_root(repo_root);
    let files = vec![
        doc_root.join("README.md"),
        doc_root.join("FACTORY.md"),
        doc_root.join("MODEL.md"),
        doc_root.join("INVARIANTS.md"),
        doc_root.join("BATTERIES.md"),
        doc_root.join("TERMINALS.md"),
        doc_root.join("EVENTS.md"),
        doc_root.join("RECEIPTS.md"),
        doc_root.join("CIRCUITS.md"),
        doc_root.join("REPLAY.md"),
        doc_root.join("PROJECTIONS.md"),
        doc_root.join("INTEGRATION.md"),
        doc_root.join("CONFORMANCE.md"),
        doc_root.join("COOKBOOK.md"),
        doc_root.join("CONTRIBUTING.md"),
    ];
    for path in files {
        let rel = relative(doc_root, &path);
        for link in markdown_links(doc_root, &path)? {
            ensure(
                !link.starts_with("docs/")
                    && !link.starts_with("archive/")
                    && !link.contains("100_ADR_")
                    && link != "099_DECISION_INDEX.md",
                format!("live doc {rel} links archive material as if it were live: {link}"),
            )?;
        }
    }
    Ok(())
}

fn check_factory_docs_use_just_commands(repo_root: &Path) -> Result<()> {
    let doc_root = project_root(repo_root);
    for doc in [
        "README.md",
        "FACTORY.md",
        "MODEL.md",
        "INVARIANTS.md",
        "BATTERIES.md",
        "TERMINALS.md",
        "EVENTS.md",
        "RECEIPTS.md",
        "CIRCUITS.md",
        "REPLAY.md",
        "PROJECTIONS.md",
        "INTEGRATION.md",
        "CONFORMANCE.md",
        "COOKBOOK.md",
    ] {
        let content =
            fs::read_to_string(doc_root.join(doc)).with_context(|| format!("read {doc}"))?;
        for banned in ["cargo xtask", "pnpm test", "npm run"] {
            ensure(
                !content.contains(banned),
                format!(
                    "{doc} must route repeatable command examples through `just`, not `{banned}`"
                ),
            )?;
        }
    }
    Ok(())
}

fn check_root_doc_site_contract(repo_root: &Path) -> Result<()> {
    let docs_rs = repo_root.join("tools/xtask/src/docs.rs");
    let content = fs::read_to_string(&docs_rs).context("read tools/xtask/src/docs.rs")?;
    for (source, rendered) in [
        ("README.md", "README.html"),
        ("FACTORY.md", "FACTORY.html"),
        ("MODEL.md", "MODEL.html"),
        ("INVARIANTS.md", "INVARIANTS.html"),
        ("CONFORMANCE.md", "CONFORMANCE.html"),
        ("COOKBOOK.md", "COOKBOOK.html"),
    ] {
        ensure(
            content.contains(source),
            format!("xtask docs surface must render canonical root doc {source}"),
        )?;
        ensure(
            content.contains(rendered),
            format!("xtask docs surface must emit canonical page {rendered}"),
        )?;
    }
    ensure(
        content.contains("api/batpak/"),
        "xtask docs surface must expose rustdoc API under api/batpak/",
    )?;
    ensure(
        !content.contains("mdbook"),
        "xtask docs surface must not depend on mdbook",
    )?;
    Ok(())
}

fn check_reference_doc_completeness(repo_root: &Path) -> Result<()> {
    let doc_root = project_root(repo_root);
    for (path, heading) in [
        ("FACTORY.md", "## Factory Contract"),
        ("MODEL.md", "## Objects"),
        ("INVARIANTS.md", "## Batteries Do Not Own The Machine"),
        ("CONFORMANCE.md", "## Command Authority"),
        ("CONFORMANCE.md", "## Machine Law"),
    ] {
        let content =
            fs::read_to_string(doc_root.join(path)).with_context(|| format!("read {path}"))?;
        ensure(
            content.contains(heading),
            format!("{path} is missing required section or anchor `{heading}`"),
        )?;
    }
    Ok(())
}

fn check_changelog_migration_contract(repo_root: &Path) -> Result<()> {
    let changelog = project_root(repo_root).join("CHANGELOG.md");
    let content = fs::read_to_string(&changelog).context("read CHANGELOG.md")?;
    for section in changelog_release_sections(&content) {
        if !section_requires_migration(section) {
            continue;
        }
        ensure(
            section.contains("### Migration"),
            "CHANGELOG.md release sections with breaking/removed/rename language must include `### Migration`",
        )?;
    }
    Ok(())
}

fn changelog_release_sections(content: &str) -> Vec<&str> {
    let mut sections = Vec::new();
    let mut current_start: Option<usize> = None;
    for (offset, line) in content.match_indices('\n') {
        let line_start = offset + 1;
        let next_line = &content[line_start..];
        if next_line.starts_with("## ") {
            if let Some(start) = current_start.replace(line_start) {
                sections.push(&content[start..line_start]);
            }
        } else if current_start.is_none() && line.starts_with("## ") {
            current_start = Some(0);
        }
    }
    if let Some(start) = current_start {
        sections.push(&content[start..]);
    }
    sections
}

fn section_requires_migration(section: &str) -> bool {
    let lower = section.to_ascii_lowercase();
    section.contains("**Breaking**")
        || section.contains("### Removed")
        || lower.contains("rename")
        || lower.contains("renamed")
        || lower.contains("removed")
}

fn project_root(repo_root: &Path) -> &Path {
    repo_root.parent().unwrap_or(repo_root)
}

fn markdown_links(repo_root: &Path, path: &Path) -> Result<BTreeSet<String>> {
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let parser = Parser::new_ext(&content, Options::all());
    let mut links = BTreeSet::new();
    for event in parser {
        if let Event::Start(Tag::Link { dest_url, .. }) = event {
            let Some(link) = resolve_link(repo_root, path, dest_url.as_ref()) else {
                continue;
            };
            links.insert(link);
        }
    }
    Ok(links)
}

fn resolve_link(repo_root: &Path, source: &Path, raw_link: &str) -> Option<String> {
    if raw_link.starts_with("http://")
        || raw_link.starts_with("https://")
        || raw_link.starts_with("mailto:")
    {
        return None;
    }
    let path_part = raw_link.split('#').next()?.trim();
    if path_part.is_empty() {
        return None;
    }
    let source_rel = source.strip_prefix(repo_root).ok()?;
    let base = source_rel.parent().unwrap_or(Path::new(""));
    Some(normalize_repo_path(&base.join(path_part)))
}

fn normalize_repo_path(path: &Path) -> String {
    let mut normalized = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part.to_string_lossy().into_owned()),
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    normalized.join("/")
}
