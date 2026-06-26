//! Launcher-binary resolution + content attestation for the Linux backend's `execute()`,
//! split out of `backend_impl.rs` to hold that file under the non-overridable file-size cap.
//! SAFE std (`std::fs` / `std::env`); nothing here is `unsafe`.

use super::launch;
use super::LinuxBackend;
use crate::contract::report::ObservedFact;

/// Record the BLAKE3 digest of the launcher binary observed AT THE RESOLVED PATH, BEFORE
/// spawn, as a provenance evidence fact.
///
/// HONEST SCOPE (the codex-review correction): this proves "the bytes at `path` when read
/// here", NOT "the exact bytes the kernel exec'd" — `run_launcher` later spawns by PATH
/// (`Command::new`), so a swap/symlink between this read and the exec is a TOCTOU window.
/// The fact wording reflects that. Closing the race (hash an OPENED fd and `fexecve` that
/// SAME fd) and digest PINNING (refuse on mismatch) are the follow-ons; this is provenance
/// EVIDENCE, not a gate, so a read failure is silently skipped (the launch still proceeds).
pub(super) fn attest_launcher(path: &std::path::Path, observed: &mut Vec<ObservedFact>) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let digest = batpak::event::hash::compute_hash(&bytes);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    observed.push(ObservedFact {
        kind: "launcher_identity".to_string(),
        detail: format!(
            "blake3={hex} observed_at_path={} (pre-spawn; not the exec'd-fd bytes — \
             fd-exec pinning is the follow-on)",
            path.display()
        ),
    });
}

/// Resolve the launcher binary path, failing closed if unresolvable. Resolution
/// order: the backend's INJECTED `launcher_path` (constructor injection) FIRST, then
/// the `BVISOR_LAUNCHER_BIN` env override, else the `bvisor-linux-launcher` binary
/// CO-LOCATED with the current executable (the documented default install layout). If
/// none resolves to an existing file ⇒ `Err` (the caller reports `Outcome::Unsupported`
/// — the workload NEVER runs unconfined). The resolved binary's CONTENT digest is then
/// attested by [`attest_launcher`]; digest-PINNING the exact bin (refuse on mismatch) is
/// the follow-on.
pub(super) fn resolve_launcher(backend: &LinuxBackend) -> Result<std::path::PathBuf, String> {
    // Injected launcher path (thread-safe constructor injection) takes precedence — it
    // is how the integration tests point at the compile-time launcher without the banned
    // process-env mutation. Confirm it exists so a bad inject still fails closed.
    if let Some(path) = &backend.launcher_path {
        if path.is_file() {
            return Ok(path.clone());
        }
        return Err(format!(
            "injected launcher path does not exist: {}",
            path.display()
        ));
    }
    // The override path is trusted as supplied (step-12 note); honor it even if a
    // stat would race, but still confirm it exists so we fail closed on a typo.
    if let Ok(p) = std::env::var(launch::ENV_LAUNCHER_BIN) {
        if !p.trim().is_empty() {
            let path = std::path::PathBuf::from(p);
            if path.is_file() {
                return Ok(path);
            }
            return Err(format!(
                "{} points to a non-existent launcher binary: {}",
                launch::ENV_LAUNCHER_BIN,
                path.display()
            ));
        }
    }
    // Default: the launcher next to the current executable.
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot locate the current executable to find the launcher: {e}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "current executable has no parent directory".to_string())?;
    let default = dir.join("bvisor-linux-launcher");
    if default.is_file() {
        return Ok(default);
    }
    Err(format!(
        "no launcher binary: {} unset and no bvisor-linux-launcher beside {}",
        launch::ENV_LAUNCHER_BIN,
        exe.display()
    ))
}
