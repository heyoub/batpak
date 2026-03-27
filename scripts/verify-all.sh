#!/usr/bin/env bash
set -euo pipefail

# ============================================================================
# batpak verification harness
#
# Runs all quality gates in dependency order. Exit code 0 = all green.
# Optimized for Rust: fmt → clippy → test (all features) → test (no features)
#
# Usage:
#   ./scripts/verify-all.sh          # full suite
#   ./scripts/verify-all.sh --quick  # fmt + clippy only (pre-commit)
# ============================================================================

cd "$(dirname "$0")/../batpak"

QUICK=false
if [ "${1:-}" = "--quick" ]; then
  QUICK=true
fi

echo "=== batpak verification ==="
echo ""

# ── Gate 1: formatting ───────────────────────────────────────────────
echo "--- Gate 1: cargo fmt --check ---"
cargo fmt --check
echo "    PASS"
echo ""

# ── Gate 2: clippy ───────────────────────────────────────────────────
echo "--- Gate 2: cargo clippy --all-features -- -D warnings ---"
cargo clippy --all-features -- -D warnings 2>&1
echo "    PASS"
echo ""

if [ "$QUICK" = true ]; then
  echo "=== QUICK GATES PASSED (fmt + clippy) ==="
  exit 0
fi

# ── Gate 3: tests with all features ──────────────────────────────────
echo "--- Gate 3: cargo test --all-features ---"
cargo test --all-features 2>&1
echo "    PASS"
echo ""

# ── Gate 4: tests with no features ───────────────────────────────────
echo "--- Gate 4: cargo test --no-default-features ---"
cargo test --no-default-features 2>&1
echo "    PASS"
echo ""

# ── Gate 5: doc warnings (informational) ─────────────────────────────
echo "--- Gate 5: cargo doc --all-features --no-deps (informational) ---"
DOC_WARNINGS=$(cargo doc --all-features --no-deps 2>&1 | grep -c "warning" || true)
echo "    Doc warnings: $DOC_WARNINGS (pre-existing, not gated)"
echo ""

echo "=== ALL GATES PASSED ==="
