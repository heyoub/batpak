# Process boundary / supervisor evidence (Downstream Host)

**Owner layer:** above batpak (Downstream Host support spec).

**Disposition:** do not implement in batpak store or core runtime. batpak may
supply `PlatformEvidenceSummary` (`src/store/stats.rs`) as an input plane,
but lifecycle law for supervised processes is host-owned.

## Evidence subjects (planning surface)

Host/runtime records, not substrate physics:

- process start and observed readiness
- drain began / completed
- exit code and terminal state
- restart intent and policy snapshot handle (opaque id, not policy engine)
- sandbox profile identity (opaque digest / version handle)
- supervisor unit identity that emitted the observation

## Composition

- May embed or cross-reference batpak `PlatformEvidenceSummary` for the store
  path the host attached to the process.
- Must not require new store primitives unless batpak grows a generic,
  domain-neutral process runner (out of scope for the pre-Downstream arc).
