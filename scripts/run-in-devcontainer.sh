#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
runtime="${OCI_RUNTIME:-docker}"
image_tag="${BATPAK_DEVCONTAINER_IMAGE:-batpak-devcontainer}"
skip_build="${BATPAK_DEVCONTAINER_SKIP_BUILD:-0}"

if [[ "${skip_build}" != "1" ]]; then
  "${runtime}" build -f "${repo_root}/.devcontainer/Dockerfile" -t "${image_tag}" "${repo_root}"
elif ! "${runtime}" image inspect "${image_tag}" >/dev/null 2>&1; then
  echo "BATPAK_DEVCONTAINER_SKIP_BUILD=1 was set but image '${image_tag}' is not available locally." >&2
  exit 1
fi

"${runtime}" run --rm \
  -e DEVCONTAINER=1 \
  -e CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}" \
  -e PROPTEST_CASES="${PROPTEST_CASES:-256}" \
  -v "${repo_root}:/workspace/batpak" \
  -w /workspace/batpak \
  "${image_tag}" \
  bash -c "$*"
