# Maintainer lane

The canonical repo workflow is:

```bash
cargo xtask doctor
cargo xtask ci
cargo xtask docs
```

Before pushing — full CI inside the canonical devcontainer.
Bit-equivalent to the GH `Integrity (ubuntu-devcontainer)` job:

```bash
# Before pushing — full CI inside the canonical devcontainer.
# Bit-equivalent to the GH Integrity (ubuntu-devcontainer) job.
cargo xtask preflight
```

Hardware-dependent perf gates — run on a stable machine, not shared CI:

```bash
# Hardware-dependent perf gates (run on a stable machine, not CI).
cargo xtask perf-gates
```

For performance work:

```bash
cargo xtask bench --surface neutral
cargo xtask bench --surface native --save
cargo xtask bench --surface neutral --compare
```
