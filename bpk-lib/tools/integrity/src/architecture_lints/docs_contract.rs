use super::{ensure, relative};
use anyhow::{Context, Result};
use pulldown_cmark::{Event, Options, Parser, Tag};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path};

pub(super) fn check(repo_root: &Path) -> Result<()> {
    check_portable_context_links(repo_root)?;
    check_live_docs_do_not_link_archives(repo_root)?;
    check_root_doc_site_contract(repo_root)?;
    check_reference_doc_completeness(repo_root)?;
    Ok(())
}

fn check_portable_context_links(repo_root: &Path) -> Result<()> {
    let doc_root = project_root(repo_root);
    let repo_map = doc_root.join("000_REPO_MAP.md");
    let readme = doc_root.join("README.md");
    let guide = doc_root.join("010_USER_GUIDE.md");
    let reference = doc_root.join("020_TECHNICAL_REFERENCE.md");

    let readme_links = markdown_links(doc_root, &readme)?;
    ensure(
        readme_links.contains("000_REPO_MAP.md"),
        "README.md must link to 000_REPO_MAP.md",
    )?;
    ensure(
        readme_links.contains("010_USER_GUIDE.md"),
        "README.md must link to 010_USER_GUIDE.md",
    )?;
    ensure(
        readme_links.contains("020_TECHNICAL_REFERENCE.md"),
        "README.md must link to 020_TECHNICAL_REFERENCE.md",
    )?;

    let guide_links = markdown_links(doc_root, &guide)?;
    ensure(
        guide_links.contains("README.md"),
        "GUIDE.md must link back to README.md",
    )?;
    ensure(
        guide_links.contains("020_TECHNICAL_REFERENCE.md"),
        "010_USER_GUIDE.md must link to 020_TECHNICAL_REFERENCE.md",
    )?;

    let reference_links = markdown_links(doc_root, &reference)?;
    ensure(
        reference_links.contains("README.md"),
        "REFERENCE.md must link back to README.md",
    )?;
    ensure(
        reference_links.contains("010_USER_GUIDE.md"),
        "020_TECHNICAL_REFERENCE.md must link to 010_USER_GUIDE.md",
    )?;

    let repo_map_links = markdown_links(doc_root, &repo_map)?;
    ensure(
        repo_map_links.contains("cookbook"),
        "000_REPO_MAP.md must point agents at cookbook/",
    )?;

    Ok(())
}

fn check_live_docs_do_not_link_archives(repo_root: &Path) -> Result<()> {
    let doc_root = project_root(repo_root);
    let files = vec![
        doc_root.join("000_REPO_MAP.md"),
        doc_root.join("README.md"),
        doc_root.join("010_USER_GUIDE.md"),
        doc_root.join("020_TECHNICAL_REFERENCE.md"),
        doc_root.join("060_CONTRIBUTING.md"),
    ];
    for path in files {
        let rel = relative(doc_root, &path);
        for link in markdown_links(doc_root, &path)? {
            ensure(
                !link.starts_with("docs/"),
                format!("live doc {rel} links archive material as if it were live: {link}"),
            )?;
        }
    }
    Ok(())
}

fn check_root_doc_site_contract(repo_root: &Path) -> Result<()> {
    let docs_rs = repo_root.join("tools/xtask/src/docs.rs");
    let content = fs::read_to_string(&docs_rs).context("read tools/xtask/src/docs.rs")?;
    for (source, rendered) in [
        ("000_REPO_MAP.md", "REPO_MAP.html"),
        ("README.md", "README.html"),
        ("010_USER_GUIDE.md", "GUIDE.html"),
        ("020_TECHNICAL_REFERENCE.md", "REFERENCE.html"),
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
    let reference = project_root(repo_root).join("020_TECHNICAL_REFERENCE.md");
    let content = fs::read_to_string(&reference).context("read 020_TECHNICAL_REFERENCE.md")?;
    for heading in [
        "## Storage And Cold Start",
        "## Public Surface Witnesses",
        "## Tuning Highlights",
        "## Benchmark Surfaces",
        "## Invariants",
        "## Authoritative Paths",
        "Key tradeoffs:",
        "Canonical commands:",
    ] {
        ensure(
            content.contains(heading),
            format!("020_TECHNICAL_REFERENCE.md is missing required section or anchor `{heading}`"),
        )?;
    }
    Ok(())
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
