set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

default:
    just --list

doctor:
    cargo xtask doctor

install-hooks:
    cargo xtask install-hooks

traceability:
    cargo xtask traceability

structural:
    cargo xtask structural

check:
    cargo xtask check

test:
    cargo xtask test

clippy:
    cargo xtask clippy

fmt:
    cargo xtask fmt

deny:
    cargo xtask deny

bench-compile:
    cargo xtask bench --compile

# Keep mutation surfaces aligned with the compiled feature set:
# the blake3 hash helper is excluded from no-default runs, and the
# clock-only fallback helper is excluded from all-features runs.
mutants-smoke:
    cargo xtask mutants smoke

mutants-full:
    cargo xtask mutants full

ci:
    cargo xtask ci

pre-commit:
    cargo xtask pre-commit

cover:
    cargo xtask cover

cover-check:
    cargo xtask cover --ci --threshold 80

cover-json:
    cargo xtask cover --json

bench: bench-neutral

bench-neutral:
    cargo xtask bench --surface neutral

bench-native:
    cargo xtask bench --surface native

bench-report surface="neutral":
    cargo xtask bench --surface {{surface}}

bench-save surface="neutral":
    cargo xtask bench --surface {{surface}} --save

bench-compare surface="neutral":
    cargo xtask bench --surface {{surface}} --compare

fuzz:
    cargo xtask fuzz

fuzz-deep:
    cargo xtask fuzz --deep

chaos:
    cargo xtask chaos

chaos-deep:
    cargo xtask chaos --deep

fuzz-chaos:
    cargo xtask fuzz-chaos

stress: fuzz chaos fuzz-chaos bench

doc:
    cargo xtask docs --open
