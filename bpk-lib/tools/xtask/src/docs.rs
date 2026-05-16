use crate::util::{cargo_target_dir, copy_dir, open_in_browser};
use crate::DocsArgs;
use anyhow::{Context, Result};
use pulldown_cmark::{html, Options, Parser};
use std::fs;
use std::path::{Path, PathBuf};

struct RootDoc<'a> {
    source_path: &'a str,
    output_name: &'a str,
    title: &'a str,
}

const ROOT_DOCS: &[RootDoc<'_>] = &[
    RootDoc {
        source_path: "000_REPO_MAP.md",
        output_name: "REPO_MAP.html",
        title: "Repository Map",
    },
    RootDoc {
        source_path: "README.md",
        output_name: "README.html",
        title: "README",
    },
    RootDoc {
        source_path: "010_USER_GUIDE.md",
        output_name: "GUIDE.html",
        title: "Guide",
    },
    RootDoc {
        source_path: "020_TECHNICAL_REFERENCE.md",
        output_name: "REFERENCE.html",
        title: "Reference",
    },
    RootDoc {
        source_path: "040_TESTING_DOCTRINE.md",
        output_name: "HARNESS_DIRECTIVE.html",
        title: "Harness Directive",
    },
    RootDoc {
        source_path: "041_TESTING_LEDGER.md",
        output_name: "HARNESS_LEDGER.html",
        title: "Harness Ledger",
    },
    RootDoc {
        source_path: "060_CONTRIBUTING.md",
        output_name: "CONTRIBUTING.html",
        title: "Contributing",
    },
    RootDoc {
        source_path: "CHANGELOG.md",
        output_name: "CHANGELOG.html",
        title: "Changelog",
    },
    RootDoc {
        source_path: "AGENTS.md",
        output_name: "AGENTS.html",
        title: "Agents",
    },
];

const REQUIRED_DOC_NAV: &[(&str, &str)] = &[
    ("000_REPO_MAP.md", "REPO_MAP.html"),
    ("README.md", "README.html"),
    ("010_USER_GUIDE.md", "GUIDE.html"),
    ("020_TECHNICAL_REFERENCE.md", "REFERENCE.html"),
];

pub(crate) fn docs(args: DocsArgs) -> Result<()> {
    let target_dir = cargo_target_dir()?;
    let site_dir = target_dir.join("site");
    if site_dir.exists() {
        fs::remove_dir_all(&site_dir).with_context(|| format!("clear {}", site_dir.display()))?;
    }
    fs::create_dir_all(&site_dir).with_context(|| format!("create {}", site_dir.display()))?;

    let mut cargo_doc = std::process::Command::new("cargo");
    cargo_doc.env(
        "RUSTDOCFLAGS",
        "--cfg docsrs --cfg batpak_stable_docs -D warnings",
    );
    cargo_doc.args([
        "doc",
        "-p",
        "batpak",
        "-p",
        "syncbat",
        "-p",
        "clawbat",
        "-p",
        "netbat",
        "--all-features",
        "--no-deps",
    ]);
    crate::util::run(cargo_doc)?;
    copy_dir(&target_dir.join("doc"), &site_dir.join("api"))?;

    for doc in ROOT_DOCS {
        let source = project_root().join(doc.source_path);
        let markdown =
            fs::read_to_string(&source).with_context(|| format!("read {}", doc.source_path))?;
        let html = render_markdown_page(doc.title, &rewrite_root_doc_links(&markdown));
        fs::write(site_dir.join(doc.output_name), html)
            .with_context(|| format!("write {}", site_dir.join(doc.output_name).display()))?;
    }

    fs::write(site_dir.join("index.html"), index_page())?;
    scrub_site_dir(&site_dir)?;

    if args.open {
        open_in_browser(site_dir.join("index.html"))?;
    }
    Ok(())
}

fn project_root() -> PathBuf {
    PathBuf::from("..")
}

fn render_markdown_page(title: &str, markdown: &str) -> String {
    let mut rendered = String::new();
    let parser = Parser::new_ext(markdown, Options::all());
    html::push_html(&mut rendered, parser);
    let nav_links = root_doc_nav_links();
    format!(
        "<!doctype html>\
         <html lang=\"en\">\
         <head>\
         <meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>batpak {title}</title>\
         <style>\
         body{{font-family:system-ui,-apple-system,BlinkMacSystemFont,\"Segoe UI\",sans-serif;max-width:900px;margin:0 auto;padding:2rem;line-height:1.6;}}\
         nav{{margin-bottom:2rem;display:flex;gap:1rem;flex-wrap:wrap;}}\
         pre{{background:#f5f5f5;padding:1rem;overflow:auto;}}\
         code{{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;}}\
         a{{color:#0b5fff;text-decoration:none;}}\
         a:hover{{text-decoration:underline;}}\
         </style>\
         </head>\
         <body>\
         <nav><a href=\"index.html\">Home</a>{nav_links}<a href=\"api/batpak/\">API</a></nav>\
         <main>{rendered}</main>\
         </body>\
         </html>"
    )
}

fn rewrite_root_doc_links(markdown: &str) -> String {
    let mut rewritten = markdown.to_string();
    for doc in ROOT_DOCS {
        rewritten = rewritten.replace(
            &format!("]({})", doc.source_path),
            &format!("]({})", doc.output_name),
        );
        rewritten = rewritten.replace(
            &format!("]: {}", doc.source_path),
            &format!("]: {}", doc.output_name),
        );
        rewritten = rewritten.replace(
            &format!("]: <{}>", doc.source_path),
            &format!("]: <{}>", doc.output_name),
        );
    }
    rewritten
}

fn index_page() -> String {
    let canonical_links = root_doc_index_links();
    "<!doctype html>\
     <html lang=\"en\">\
     <head>\
     <meta charset=\"utf-8\">\
     <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
     <title>batpak docs</title>\
     <style>body{font-family:system-ui,-apple-system,BlinkMacSystemFont,\"Segoe UI\",sans-serif;max-width:900px;margin:0 auto;padding:2rem;line-height:1.6;}ul{line-height:1.9;}a{color:#0b5fff;text-decoration:none;}a:hover{text-decoration:underline;}</style>\
     </head>\
     <body>\
     <h1>batpak docs</h1>\
     <p>Root docs are canonical. API docs are generated from rustdoc.</p>\
     <ul>\
     "
        .to_string()
        + &canonical_links
        + "<li><a href=\"api/batpak/\">API</a></li>\
     </ul>\
     </body>\
     </html>"
}

fn root_doc_nav_links() -> String {
    REQUIRED_DOC_NAV
        .iter()
        .map(|(source_path, output_name)| {
            let label = source_path
                .trim_end_matches(".md")
                .trim_start_matches(|ch: char| ch.is_ascii_digit() || ch == '_');
            format!("<a href=\"{output_name}\">{label}</a>")
        })
        .collect::<Vec<_>>()
        .join("")
}

fn root_doc_index_links() -> String {
    ROOT_DOCS
        .iter()
        .map(|doc| {
            let source_path = doc.source_path;
            let output_name = doc.output_name;
            let label = source_path
                .trim_end_matches(".md")
                .trim_start_matches(|ch: char| ch.is_ascii_digit() || ch == '_');
            format!("<li><a href=\"{output_name}\">{label}</a></li>")
        })
        .collect::<Vec<_>>()
        .join("")
}

pub(crate) fn scrub_site_dir(site_dir: &Path) -> Result<()> {
    remove_problematic_files(site_dir)?;
    ensure_readable(site_dir)?;
    Ok(())
}

fn remove_problematic_files(root: &Path) -> Result<()> {
    for entry in walk_files(root)? {
        let file_name = entry
            .file_name()
            .to_str()
            .map(ToOwned::to_owned)
            .unwrap_or_default();
        if (file_name.contains('!') && file_name.ends_with(".html")) || file_name == ".lock" {
            fs::remove_file(entry.path())
                .with_context(|| format!("remove {}", entry.path().display()))?;
        }
    }
    Ok(())
}

fn walk_files(root: &Path) -> Result<Vec<std::fs::DirEntry>> {
    let mut stack = vec![root.to_path_buf()];
    let mut entries = Vec::new();
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                stack.push(entry.path());
            } else {
                entries.push(entry);
            }
        }
    }
    Ok(entries)
}

fn ensure_readable(root: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut stack = vec![root.to_path_buf()];
        while let Some(path) = stack.pop() {
            let metadata = fs::metadata(&path)?;
            let mut perms = metadata.permissions();
            if metadata.is_dir() {
                perms.set_mode(perms.mode() | 0o755);
                fs::set_permissions(&path, perms)?;
                for entry in fs::read_dir(path)? {
                    stack.push(entry?.path());
                }
            } else {
                perms.set_mode(perms.mode() | 0o644);
                fs::set_permissions(&path, perms)?;
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = root;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{index_page, rewrite_root_doc_links, REQUIRED_DOC_NAV, ROOT_DOCS};

    #[test]
    fn rewrite_root_doc_links_updates_canonical_docs() {
        let rewritten = rewrite_root_doc_links(
            "[map](000_REPO_MAP.md) [readme](README.md) [guide](010_USER_GUIDE.md) [reference](020_TECHNICAL_REFERENCE.md)",
        );
        assert!(rewritten.contains("README.html"));
        assert!(rewritten.contains("GUIDE.html"));
        assert!(rewritten.contains("REFERENCE.html"));
    }

    #[test]
    fn root_docs_include_required_nav_docs() {
        for (source_path, output_name) in REQUIRED_DOC_NAV {
            assert!(ROOT_DOCS
                .iter()
                .any(|doc| { doc.source_path == *source_path && doc.output_name == *output_name }));
        }
    }

    #[test]
    fn index_page_links_canonical_docs_and_api() {
        let page = index_page();
        for doc in ROOT_DOCS {
            assert!(page.contains(doc.output_name));
        }
        assert!(page.contains("api/batpak/"));
    }
}
