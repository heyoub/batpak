# ADR-0019: Canonical Encoding Compatibility Contract

## Status
Accepted.

## Context
batpak exposes `batpak::canonical` as a back-compatible alias for its
batpak-scoped named-field MessagePack helper used by receipts, signing cover
bytes, and deterministic fixture/report workflows.
Today, `bpk-lib/crates/core/src/encoding.rs` documents that this surface is stable for the current
crate version but does not claim a cross-version canonical-bytes guarantee.

Extraction work depends on a clear contract boundary before adding new evidence
report APIs. Without an explicit compatibility contract, report-body identity,
schema snapshots, and future artifact/registry work risk over-promising byte
stability.

## Decision
batpak adopts the following canonical encoding compatibility contract.

### 1) Existing canonical surface

- `batpak::canonical::{to_bytes, from_bytes}` remains the legacy public alias
  for batpak's deterministic named-field MessagePack surface.
- The encoding format remains named-field MessagePack in this phase.
- This ADR does not change on-wire format or introduce a second canonical
  encoder.
- The root crate pins the exact `rmp-serde` encoder version used for canonical
  bytes. Encoder upgrades are intentional compatibility work: update the pin,
  refresh golden fixtures, and review this ADR instead of accepting silent
  transitive byte-shape changes.
- This surface does not implement protocol-specific canonicalization such as
  JSON Canonicalization Scheme; callers that need another protocol's canonical
  bytes must compute them outside batpak core.

### 2) Compatibility class for current canonical bytes

- Current canonical bytes are **version-anchor-verifiable**.
- Verification and deterministic replay expectations are scoped to the batpak
  version line that produced the bytes.
- batpak does **not** promise cross-minor canonical-byte equality in this
  decision.

### 3) Contract for new public evidence report bodies

Any new public deterministic report API added after this ADR must:

- define an explicit report schema version in its public contract; and
- treat canonical body bytes as **patch-stable within that schema version**;
  and
- add or extend a golden fixture covering the report body or explain why an
  existing fixture already covers the same public byte boundary.

In other words, for two patch releases that keep the same report schema version,
equal logical report inputs must encode to equal canonical bytes.

Report `body_hash` values are derived from those canonical body bytes using the
active batpak hash backend. Default builds use BLAKE3. No-default-feature builds
use batpak's deterministic CRC32-backed fallback and therefore preserve
determinism without claiming cryptographic strength.

The compatibility mechanism is deliberately concrete: exact encoder pin plus
checked-in golden bytes for public deterministic report families. A semver range
such as `rmp-serde = "1"` or `~1.3` is not sufficient for these bytes because
downstream resolution could select an encoder patch that the fixture suite never
observed. A fully owned canonical MessagePack encoder remains a possible future
decision, but it is a format project, not a release-prep shortcut.

### 4) Non-promises in this phase

This decision intentionally does not promise:

- cross-minor canonical-byte equality for all existing payloads;
- automatic additive/breaking schema classification semantics;
- migration to dCBOR or any alternate canonical format;
- artifact-envelope or registry-row identity semantics.

Those are follow-on decisions after schema/fixture report surfaces are defined
and exercised.

## Consequences

- Canonical encoding remains boring and unchanged in this slice.
- `rmp-serde` updates become deliberate compatibility events instead of ambient
  dependency drift.
- New report APIs must carry explicit schema-version discipline instead of
  relying on implicit format stability.
- Schema snapshot and chain/subscriber evidence report work can proceed with a
  clear compatibility floor.
- Artifact envelope and attested registry work stays parked until a later ADR
  defines stronger cross-version identity guarantees.

## References

- `bpk-lib/crates/core/src/encoding.rs`
- `bpk-lib/crates/core/src/lib.rs` (`pub use crate::encoding as canonical`)
- `080_EVIDENCE_REPORTS.md`
