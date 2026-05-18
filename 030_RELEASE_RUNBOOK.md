# Release Runbook

Release work is split into two acts: a reviewable release-prep PR, then the
manual publish/tag/release steps from a clean `main`.

## Release-Prep PR

1. Start from a clean, current `main`.
2. Bump every workspace package version for the release. For ordinary pre-1.0
   breaking changes, bump the minor version, for example `0.6.0` -> `0.7.0`.
   For the 0.7.6 public-surface correction cut, ADR-0026 permits the patch
   release shape and ADR-0031 defines the release proof posture for the whole
   publishable substrate family.
3. Keep internal dependency cross-references in sync. After editing, run the
   workspace-owned pin check:

   ```bash
   cargo xtask check-version-pins
   ```

4. Make every crate in the release chain publishable. For the 0.7.6 correction
   cut, ADR-0031 names the release train: `batpak`,
   `batpak-macros-support`, `batpak-macros`, `batpak-bench-support`,
   `syncbat-macros`, `syncbat`, and `netbat`.
5. Give each publishable internal crate crates.io metadata and a small
   crate-local `README.md`.
6. Finalize `CHANGELOG.md`: add the dated release section and restore an empty
   `[Unreleased]` section above it.
7. Verify that the checked-in public API snapshot matches the intended
   post-cleanup surface:

   ```bash
   cargo xtask public-api --strict --check-baseline
   ```

   Refresh with `cargo xtask public-api --strict --bless-baseline` only when
   the public-surface change is intentional and has a `CHANGELOG.md` migration
   entry.
8. Run the release dry-run:

   ```bash
   cargo xtask release --dry-run
   ```

   The xtask dry-run runs the repo-owned CI surface, downstream consumer smoke,
   docs, and patched `cargo publish --dry-run` checks for the release chain.
   Patch overrides cannot detect internal dependency version drift; the
   release-prep PR owns that through `cargo xtask check-version-pins`.

9. Before publish, explicitly smoke the cross-crate payload registry fixture in
   both debug and release modes so `inventory` registration survives optimized
   linkage:

   ```bash
   cargo test --manifest-path bpk-lib/crates/core/fixtures/downstream/Cargo.toml
   cargo test --test event_payload_registry_downstream
   cargo test --release --manifest-path bpk-lib/crates/core/fixtures/kind-collision-composer/Cargo.toml
   ```

10. Inspect package contents before publishing:

   ```bash
   cargo package --list -p batpak-macros-support
   cargo package --list -p batpak-macros
   cargo package --list -p batpak-bench-support
   cargo package --list -p syncbat-macros
   cargo package --list -p batpak
   cargo package --list -p syncbat
   cargo package --list -p netbat
   ```

   Look for missing READMEs, accidental large fixtures, broken symlinks, and
   tarballs near the crates.io size limit.

11. If canonical encoding dependencies or public report-body schemas changed,
    run the canonical patch-stability suite without `GOLDEN_UPDATE` and inspect
    the fixture diff intentionally:

   ```bash
   cargo test -p batpak --test canonical_patch_stability --all-features
   ```

   Refresh goldens only as a deliberate compatibility decision paired with
   ADR-0019 and `CHANGELOG.md` notes.

## Pre-Release-Prep Gate Order

Run this order once from the release-prep branch before asking for release
review:

```bash
cargo xtask preflight
cargo xtask evidence-audit
cargo xtask check-version-pins
cargo xtask package-leak-scan --strict-language
cargo xtask perf-gates
cargo xtask loom
cargo xtask mutants smoke
cargo xtask public-api --strict --check-baseline
cargo xtask semver-check
cargo xtask release-manifest --strict
cargo xtask release --dry-run
```

## Benchmark Baseline

Capture release numbers from the merged release commit on stable hardware, not
from an unmerged PR head, devcontainer, or noisy VM:

```bash
cargo xtask bench --surface neutral --save baseline-v<version>
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

cargo publish -p batpak-macros-support
cargo search batpak-macros-support

cargo publish -p batpak-macros
cargo search batpak-macros

cargo publish -p batpak-bench-support
cargo search batpak-bench-support

cargo publish -p syncbat-macros
cargo search syncbat-macros

cargo publish -p batpak
cargo search batpak

cargo publish -p syncbat
cargo search syncbat

cargo publish -p netbat
cargo search netbat
```

Wait for each crate in the required chain to appear in `cargo search` before
publishing the next one. Crates.io index propagation can lag for a few minutes;
publishing out of order usually fails with "no matching package found" for the
dependency version.

If a published version is wrong, yank it and publish a patch release. Crates.io
does not allow replacing the bytes for an existing version.

## Rollback / Hotfix

- Yank-only: yank in reverse publish order: `netbat`, `syncbat`, `batpak`,
  `syncbat-macros`, `batpak-bench-support`, `batpak-macros`,
  `batpak-macros-support`.
- Yank-and-patch: yank the incorrect version, prepare the next patch release,
  run the full Pre-Release-Prep Gate Order, then publish the chain.
- Tag correction: replace an unpublished local tag with `git tag -fa`. If any
  consumer may have pulled the tag, publish release notes that name the
  corrected tag and leave the original object reachable.
- docs.rs failure: rebuild docs locally first. Yank only when package bytes are
  wrong; otherwise publish a patch release with the documentation fix.

## Deployment Pattern

`batpak` store directories are single-owner resources. A release may support
forward-reading old artifacts, but that does not permit two binary versions to
open the same mutable `data_dir` concurrently. For low-downtime rollouts, route
traffic through one owning process, stop that owner, reopen with the new binary,
and resume. Readers that need continuity should use exported/tail-able events,
offline copied snapshots, or a product-owned replica rather than bypassing the
directory lock.

## Tag And GitHub Release

After crates.io indexing settles:

```bash
git tag -a v<version> -m "Release <version>"
git push origin v<version>
gh release create v<version> --title "v<version>" --notes-file <notes-file>
```

Use release notes from `CHANGELOG.md` plus the benchmark appendix. Prefer an
annotated tag; avoid lightweight release tags.

## Post-Publish Checks

1. Build a fresh scratch consumer:

   ```bash
   mkdir batpak-smoke
   cd batpak-smoke
   cargo init --name batpak-smoke
   cargo add batpak@<version>
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
