#!/usr/bin/env bash
# Onboard a fresh clone by installing repo hooks and pinned developer tools.
set -euo pipefail

cd "$(dirname "$0")/../bpk-lib"
cargo xtask setup --install-tools
