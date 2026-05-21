use crate::util::{cargo_target_dir, run};
use anyhow::Result;
use std::process::Command;

pub(crate) fn loom() -> Result<()> {
    for test in ["deterministic_concurrency", "group_commit_crash"] {
        let mut command = Command::new("cargo");
        command
            .arg("test")
            .arg("--release")
            .arg("--test")
            .arg(test)
            .arg("--all-features")
            .arg("--target-dir")
            .arg(cargo_target_dir()?.join("loom"))
            .env("CARGO_TARGET_DIR", cargo_target_dir()?)
            .env("LOOM_MAX_PREEMPTIONS", "3")
            .env(
                "RUSTFLAGS",
                append_rustflags("--cfg loom", std::env::var("RUSTFLAGS").ok()),
            );
        run(command)?;
    }
    Ok(())
}

fn append_rustflags(required: &str, existing: Option<String>) -> String {
    match existing {
        Some(flags) if flags.split_whitespace().any(|flag| flag == "--cfg") => {
            format!("{flags} {required}")
        }
        Some(flags) if !flags.trim().is_empty() => format!("{flags} {required}"),
        _ => required.to_owned(),
    }
}
