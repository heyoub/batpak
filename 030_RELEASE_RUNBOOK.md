# Release Runbook

Release work is split into two acts: a reviewable release-prep PR, then the
manual publish/tag/release steps from a clean `main`.

## Release-Prep PR

1. Start from a clean, current `main`.
2. Bump every workspace package version for the release. For a pre-1.0
   breaking change, bump the minor version, for example `0.6.0` -> `0.7.0`.
3. Keep internal dependency cross-references in sync. After editing, search
   for the previous version across manifests; this should be clean except for
   historical changelog entries:

   ```bash
   rg '<escaped-previous-version>' --glob Cargo.toml
   ```

4. Make every crate needed by the root crate publishable. The required publish
   chain for `batpak` itself is `batpak-macros-support`, `batpak-macros`,
   then `batpak`. `batpak-bench-support` is a publishable companion crate used
   as a root dev-dependency; publish it for version alignment, but the root
   crate does not need it indexed before `batpak` can publish.
5. Give each publishable internal crate crates.io metadata and a small
   crate-local `README.md`.
6. Finalize `CHANGELOG.md`: add the dated release section and restore an empty
   `[Unreleased]` section above it.
7. Run the release dry-run:

   ```bash
   cargo xtask release --dry-run
   ```

   The xtask dry-run runs the repo-owned CI surface, downstream consumer smoke,
   docs, and a patched `cargo publish --dry-run` for the root crate. The patch
   overrides are a dry-run aid only; the real publish still needs dependency
   crates indexed on crates.io.

8. Before publish, explicitly smoke the cross-crate payload registry fixture in
   both debug and release modes so `inventory` registration survives optimized
   linkage:

   ```bash
   cargo test --manifest-path bpk-lib/crates/core/fixtures/downstream/Cargo.toml
   cargo test --test event_payload_registry_downstream
   cargo test --release --manifest-path bpk-lib/crates/core/fixtures/kind-collision-composer/Cargo.toml
   ```

9. Inspect package contents before publishing:

   ```bash
   cargo package --list -p batpak-macros-support
   cargo package --list -p batpak-macros
   cargo package --list -p batpak-bench-support
   cargo package --list -p batpak
   ```

   Look for missing READMEs, accidental large fixtures, broken symlinks, and
   tarballs near the crates.io size limit.

## Benchmark Baseline

Capture release numbers from the merged release commit on stable hardware, not
from an unmerged PR head, devcontainer, or noisy VM:

```bash
cargo xtask bench --surface neutral --save baseline-v0.7.5
```

Record the hardware and OS next to the numbers: CPU model, RAM, disk type, OS,
kernel, and whether frequency scaling was pinned. Saved baselines are the
comparison point for the next release; dry-run bench compilation is not a
performance measurement.

## Publish

Publish from a clean `main`, after the release-prep PR is merged and CI is
green. Do not tag until crates.io publishes succeed.

```bash
git status

# Required dependency chain for the root crate.
cargo publish -p batpak-macros-support
cargo search batpak-macros-support

cargo publish -p batpak-macros
cargo search batpak-macros

cargo publish -p batpak
cargo search batpak

# Companion crate: version-aligned, but not required for root crate resolution.
cargo publish -p batpak-bench-support
cargo search batpak-bench-support
```

Wait for each crate in the required chain to appear in `cargo search` before
publishing the next one. Crates.io index propagation can lag for a few minutes;
publishing out of order usually fails with "no matching package found" for the
dependency version.

If a published version is wrong, yank it and publish a patch release. Crates.io
does not allow replacing the bytes for an existing version.

## Tag And GitHub Release

After crates.io indexing settles:

```bash
git tag -a v0.7.0 -m "Release 0.7.0"
git push origin v0.7.0
gh release create v0.7.0 --title "v0.7.0" --notes-file <notes-file>
```

Use release notes from `CHANGELOG.md` plus the benchmark appendix. Prefer an
annotated tag; avoid lightweight release tags.

## Post-Publish Checks

1. Build a fresh scratch consumer:

   ```bash
   mkdir batpak-smoke
   cd batpak-smoke
   cargo init --name batpak-smoke
   cargo add batpak@0.7.0
   printf 'use batpak::*; fn main() {}' > src/main.rs
   cargo build
   ```

2. Check `https://docs.rs/batpak/<version>` after docs.rs finishes building.
3. Check `https://crates.io/crates/batpak` for README rendering, license, and
   repository links.
4. Run a final local sanity check on fresh `main`:

   ```bash
   cargo xtask traceability
   cargo xtask structural
   ```

5. Record any release-specific notes in the active plan or project journal.
