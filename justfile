set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

default:
    just --list

help:
    just --list

list:
    just --list

doctor:
    cd bpk-lib; cargo xtask doctor

install-hooks:
    cd bpk-lib; cargo xtask install-hooks

traceability:
    cd bpk-lib; cargo xtask traceability

structural:
    cd bpk-lib; cargo xtask structural

inspect:
    cd bpk-lib; cargo xtask structural
    cd bpk-lib; cargo xtask boundary
    cd bpk-lib; cargo xtask architecture-ir
    cd bpk-lib; cargo xtask architecture-ir --check
    cd bpk-lib; cargo xtask ast-grep

layout:
    cd bpk-lib; cargo xtask layout

boundary:
    cd bpk-lib; cargo xtask boundary

stale-paths:
    cd bpk-lib; cargo xtask stale-paths

disk-audit:
    cd bpk-lib; cargo xtask disk-audit

clean-generated:
    cd bpk-lib; cargo xtask clean-generated

package-leak-scan:
    cd bpk-lib; cargo xtask package-leak-scan --allow-dirty

check:
    cd bpk-lib; cargo xtask check

test:
    cd bpk-lib; cargo xtask test

clippy:
    cd bpk-lib; cargo xtask clippy

fmt:
    cd bpk-lib; cargo xtask fmt

deny:
    cd bpk-lib; cargo xtask deny

bench-compile:
    cd bpk-lib; cargo xtask bench --compile

perf-gates:
    cd bpk-lib; cargo xtask perf-gates

loom:
    cd bpk-lib; cargo xtask loom

template-freshness:
    cd bpk-lib; cargo xtask template-freshness

staged-diff:
    cd bpk-lib; cargo xtask staged-diff

release-manifest:
    cd bpk-lib; cargo xtask release-manifest

# Keep mutation surfaces aligned with the compiled feature set:
# the blake3 hash helper is excluded from no-default runs, and the
# clock-only fallback helper is excluded from all-features runs.
mutants-smoke:
    cd bpk-lib; cargo xtask mutants smoke

mutants-full:
    cd bpk-lib; cargo xtask mutants full

ledger-list:
    cd bpk-lib; cargo xtask factory-ledger list

context:
    cd bpk-lib; cargo xtask context

ledger-run command *args:
    cd bpk-lib; cargo xtask factory-ledger run -- {{command}} {{args}}

ledger-run-gate gate command *args:
    cd bpk-lib; cargo xtask factory-ledger run --gate {{gate}} -- {{command}} {{args}}

ci-fast:
    cd bpk-lib; cargo xtask ci-fast

ci-windows:
    cd bpk-lib; cargo xtask ci-windows-surface

ci:
    cd bpk-lib; cargo xtask ci

verify:
    cd bpk-lib; cargo xtask preflight

# Whole-repo validation in one command: the Rust gates (preflight) AND the
# bpk-ts package set. This is the polyglot porch — no more "run `just verify`
# then remember the pnpm combo by hand" to clear both halves of the monorepo.
verify-all: verify verify-ts

# The bpk-ts gate surface. The real pnpm logic lives in the xtask engine
# (`cargo xtask verify-ts`) per the justfile-stays-thin contract; this recipe
# only forwards to it.
verify-ts:
    cd bpk-lib; cargo xtask verify-ts

seal:
    cd bpk-lib; cargo xtask check-version-pins
    cd bpk-lib; cargo xtask evidence-audit
    cd bpk-lib; cargo xtask release-status --strict --active
    cd bpk-lib; cargo xtask release-manifest --strict

ship mode="dry":
    just ship-{{mode}}

ship-dry:
    cd bpk-lib; cargo xtask release --dry-run

ship-real:
    cd bpk-lib; cargo xtask release

pre-commit:
    cd bpk-lib; cargo xtask pre-commit

# Report-only coverage. The same `cargo xtask cover` now also runs per-PR in CI
# as the non-blocking `coverage-baseline` job (advisory observation). The hard
# coverage floor (80) is `cover-check` below, which runs in preflight/verify-linux.
cover:
    cd bpk-lib; cargo xtask cover

cover-check:
    cd bpk-lib; cargo xtask cover --ci --threshold 80

cover-json:
    cd bpk-lib; cargo xtask cover --json

bench: bench-neutral

bench-neutral:
    cd bpk-lib; cargo xtask bench --surface neutral

bench-native:
    cd bpk-lib; cargo xtask bench --surface native

bench-save surface="neutral":
    cd bpk-lib; cargo xtask bench --surface {{surface}} --save

bench-compare surface="neutral":
    cd bpk-lib; cargo xtask bench --surface {{surface}} --compare

fuzz:
    cd bpk-lib; cargo xtask fuzz

fuzz-deep:
    cd bpk-lib; cargo xtask fuzz --deep

chaos:
    cd bpk-lib; cargo xtask chaos

chaos-deep:
    cd bpk-lib; cargo xtask chaos --deep

fuzz-chaos:
    cd bpk-lib; cargo xtask fuzz-chaos

stress:
    cd bpk-lib; cargo xtask stress

docs:
    cd bpk-lib; cargo xtask docs --open

doc: docs

cargo +args:
    cd bpk-lib; cargo {{args}}

pnpm +args:
    pnpm {{args}}

npm +args:
    npm {{args}}
