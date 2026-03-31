#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/../batpak"

quick=false
if [ "${1:-}" = "--quick" ]; then
  quick=true
fi

echo "=== batpak integrity verification ==="
echo

echo "--- Gate 1: doctor ---"
cargo run --manifest-path tools/integrity/Cargo.toml -- doctor --strict
echo

echo "--- Gate 2: traceability ---"
cargo run --manifest-path tools/integrity/Cargo.toml -- traceability-check
echo

echo "--- Gate 3: structural ---"
cargo run --manifest-path tools/integrity/Cargo.toml -- structural-check
echo

echo "--- Gate 4: cargo fmt --check ---"
cargo fmt --check
echo

echo "--- Gate 5: cargo clippy --all-features --all-targets -- -D warnings ---"
cargo clippy --all-features --all-targets -- -D warnings
echo

if [ "$quick" = true ]; then
  echo "=== QUICK GATES PASSED ==="
  exit 0
fi

echo "--- Gate 6: cargo deny check ---"
cargo deny check
echo

echo "--- Gate 7: cargo nextest run --profile ci --all-features ---"
cargo nextest run --profile ci --all-features
echo

echo "--- Gate 8: cargo test --doc --all-features ---"
cargo test --doc --all-features
echo

echo "--- Gate 9: cargo check --all-features ---"
cargo check --all-features
echo

echo "--- Gate 10: cargo check --no-default-features ---"
cargo check --no-default-features
echo

echo "--- Gate 11: cargo bench --no-run --all-features ---"
cargo bench --no-run --all-features
echo

echo "=== ALL GATES PASSED ==="
