#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
runtime="${OCI_RUNTIME:-docker}"
image_tag="${BATPAK_DEVCONTAINER_IMAGE:-batpak-devcontainer}"

"${runtime}" build -f "${repo_root}/.devcontainer/Dockerfile" -t "${image_tag}" "${repo_root}"
"${runtime}" run --rm \
  -e DEVCONTAINER=1 \
  -e CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}" \
  -e PROPTEST_CASES="${PROPTEST_CASES:-256}" \
  -v "${repo_root}:/workspace/batpak" \
  -w /workspace/batpak/batpak \
  "${image_tag}" \
  bash -lc "$*"
