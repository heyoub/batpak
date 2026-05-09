# ADR-0015: dm-flakey Chaos Harness

## Status
Accepted (shipped in 0.7.0).

## Context
Phase 0 durable-frontier chaos tests proved writer-thread panic behavior inside
one process. They could not prove true torn-tail durability for
`SingleAppendWritten`, because a process panic leaves dirty page-cache bytes
available to the same host kernel. Reopen may recover a complete unsynced frame
from cache even though a real block-device failure would have lost it.

Phase 1B needs a harness that can make writes disappear below the filesystem
boundary. The harness should be fast enough for focused scenarios, should avoid
new Rust dependencies, and should stay out of ordinary per-PR CI unless
explicitly requested.

## Decision
Use Linux device-mapper as the block-layer chaos primitive. The first harness
surface is a private `crates/core/tests/chaos/` module with a `dm_flakey` wrapper that:

- creates a sparse backing file and loop device;
- exposes it through a device-mapper target;
- formats and mounts it as ext4 with synchronous writes;
- flips the active mapper table to an error target;
- tears down mount, mapper, loop device, and backing file with best-effort RAII.

The integration test target is compiled only on Linux with the
`dangerous-test-hooks` feature. The destructive smoke scenario requires
`BATPAK_RUN_CHAOS=1`, so feature-isolation can compile the harness without
attempting privileged block-device operations.

CI runs the smoke harness in a separate `chaos-nightly` job on a daily schedule,
on manual workflow dispatch, or when a pull request carries the `run-chaos`
label. It is not part of the default per-PR matrix.

## Alternatives Considered
- QEMU power-loss fixture: faithful, but much slower and large enough to become
  its own infrastructure project.
- FUSE shim: easier to run without elevated privileges, but it tests a userspace
  filesystem boundary rather than the kernel block path the store actually
  depends on.
- LD_PRELOAD write shim: simple and dependency-light, but too high-level to
  model page-cache and block-device failure semantics honestly.

## Consequences
The chaos proof is Linux-specific. That is acceptable because the durability
claim is kernel and filesystem specific; a cross-platform fake would be less
honest than a Linux-only hard proof.

The initial scaffold proves only that the harness can create, mount, flip, and
tear down a failing device. Batpak-specific torn-tail scenarios remain separate
Phase 1B stops so each durability claim can be reviewed independently.

The harness requires privileged device operations. Ordinary CI and local
feature-isolation runs compile it but skip destructive execution unless the
operator opts in with `BATPAK_RUN_CHAOS=1`.

## References
- [ADR-0014: Durable Frontier Observability](ADR-0014-durable-frontier.md)
- `traceability/invariants.yaml`: `INV-FRONTIER-FAULT-ORDINALS`,
  `INV-CHAOS-LINUX-ONLY`
- `crates/core/tests/chaos/dm_flakey.rs`
- `HARNESS_LEDGER.md`
