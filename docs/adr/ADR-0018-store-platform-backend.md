# ADR-0018: Store Platform Backend

## Status

Accepted.

## Context

batpak's store already touches target-sensitive machine behavior in a few
places: symlink leaf rejection, same-directory temp-file persistence,
parent-directory fsync, store-lock opening, segment creation and sync, canonical
clock helpers, and file-backed mmap. Those operations are narrow store-internal
machine contact, not generic business logic. They give the store the target
facts it needs for honest persistence and replay decisions.

Leaving those calls scattered makes it harder to see which code is observing
the target and which code is deciding store semantics.

## Decision

Create a private `src/store/platform/` backend room for target-sensitive
machine-contact helpers, evidence summaries, narrow admission tokens,
versioned profile records, and opt-in open-time reverify.

The governing rule is:

> Platform observes. Store admits. batpak guarantees.

`store::platform` may perform narrow target-sensitive operations and expose
mechanics and descriptive evidence to the store. It must not define durability,
replay, visibility, or admission semantics. Store, cold-start, segment, and
frontier code remain the owners of those guarantees.

## Consequences

- Machine-contact helpers have one internal home under `src/store/platform/`.
- Store diagnostics expose a platform evidence summary so operators can see
  which target-sensitive mechanics/posture were reported.
- Store lock, parent-dir sync, mmap-index, and sealed-segment mmap paths use
  internal admission tokens. The tokens admit target mechanics; store
  invariants still own semantic guarantees such as segment immutability.
- `StoreConfig::platform_profile_path` enables opt-in profile-verified open.
  Profile mismatch fails before mutable writer spawn or successful-open
  observability, but open may still create the data directory and lock file
  before reverify fails.
- `cargo xtask platform ...` owns operator profile probe/verify/bless/audit
  workflows. `build.rs` may validate an explicitly configured profile via
  `BATPAK_PLATFORM_PROFILE`, but it never probes live hardware.
- Profile fingerprints use non-cryptographic CRC32 for accidental drift
  detection only. Profile signing is explicitly not implemented.
- Structural checks prevent direct target-sensitive store calls from leaking
  outside `src/store/platform/`.
- Codegen, proc macros, profile signing, and public API design stay outside
  this store-internal boundary.
- Safety comments stay at semantic call sites when the proof belongs to store
  logic, such as sealed-segment mmap immutability.
- Evidence/admission work builds on this private room while the public surface
  remains `StoreConfig`, diagnostics, and documented errors.

## Non-goals

- Keep the boundary as a private store-internal module.
- No generic wrapper around all filesystem I/O.
- No live hardware probing in `build.rs`.
- No profile signing.

## Traceability

- `ART-STORE-PLATFORM`
- `INV-PLATFORM-EVIDENCE-NOT-MEANING`
- `FLOW-STORE-PLATFORM-CONTACT`
