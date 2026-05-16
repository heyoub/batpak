set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

default:
    just --list

doctor:
    cd bpk-lib && cargo xtask doctor

install-hooks:
    cd bpk-lib && cargo xtask install-hooks

traceability:
    cd bpk-lib && cargo xtask traceability

structural:
    cd bpk-lib && cargo xtask structural

layout:
    cd bpk-lib && cargo xtask layout

boundary:
    cd bpk-lib && cargo xtask boundary

stale-paths:
    cd bpk-lib && cargo xtask stale-paths

disk-audit:
    cd bpk-lib && cargo xtask disk-audit

clean-generated:
    cd bpk-lib && cargo xtask clean-generated

package-leak-scan:
    cd bpk-lib && cargo xtask package-leak-scan --allow-dirty

check:
    cd bpk-lib && cargo xtask check

test:
    cd bpk-lib && cargo xtask test

clippy:
    cd bpk-lib && cargo xtask clippy

fmt:
    cd bpk-lib && cargo xtask fmt

deny:
    cd bpk-lib && cargo xtask deny

bench-compile:
    cd bpk-lib && cargo xtask bench --compile

template-freshness:
    cd bpk-lib && cargo xtask template-freshness

staged-diff:
    cd bpk-lib && cargo xtask staged-diff

release-manifest:
    cd bpk-lib && cargo xtask release-manifest

# Keep mutation surfaces aligned with the compiled feature set:
# the blake3 hash helper is excluded from no-default runs, and the
# clock-only fallback helper is excluded from all-features runs.
mutants-smoke:
    cd bpk-lib && cargo xtask mutants smoke

mutants-full:
    cd bpk-lib && cargo xtask mutants full

ci:
    cd bpk-lib && cargo xtask ci

pre-commit:
    cd bpk-lib && cargo xtask pre-commit

cover:
    cd bpk-lib && cargo xtask cover

cover-check:
    cd bpk-lib && cargo xtask cover --ci --threshold 80

cover-json:
    cd bpk-lib && cargo xtask cover --json

bench: bench-neutral

bench-neutral:
    cd bpk-lib && cargo xtask bench --surface neutral

bench-native:
    cd bpk-lib && cargo xtask bench --surface native

bench-save surface="neutral":
    cd bpk-lib && cargo xtask bench --surface {{surface}} --save

bench-compare surface="neutral":
    cd bpk-lib && cargo xtask bench --surface {{surface}} --compare

fuzz:
    cd bpk-lib && cargo xtask fuzz

fuzz-deep:
    cd bpk-lib && cargo xtask fuzz --deep

chaos:
    cd bpk-lib && cargo xtask chaos

chaos-deep:
    cd bpk-lib && cargo xtask chaos --deep

fuzz-chaos:
    cd bpk-lib && cargo xtask fuzz-chaos

stress:
    cd bpk-lib && cargo xtask stress

docs:
    cd bpk-lib && cargo xtask docs --open

doc: docs
