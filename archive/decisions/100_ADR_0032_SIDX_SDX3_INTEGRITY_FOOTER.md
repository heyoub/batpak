# ADR-0032: SIDX SDX3 Integrity Footer

## Status

Accepted; 0.8.3 audit-remediation cut.

## Context

Sealed segments carry a SIDX footer that lets reopen reconstruct the in-memory
index without a full frame scan. Through 0.8.2 that footer used the `SDX2`
magic and carried no end-to-end integrity check over its own bytes. A footer
that was silently truncated, partially written, or bit-rotted on the storage
medium could still parse far enough to seed a wrong fast-path index, which is
exactly the class of cold-start corruption the substrate must refuse to trust.

ADR-0009 already fixes the artifact-upgrade posture for cold-start artifacts:
current readers forward-read known older artifacts, old optimization artifacts
may be ignored and rebuilt or fallback-scanned, and downgrade is not assumed
safe. The SIDX footer is one of those optimization artifacts, so it can be
upgraded additively as long as the fallback path stays correct.

## Decision

### Footer Format Bump

The SIDX footer magic is bumped `SDX2` -> `SDX3`. An `SDX3` footer carries a
CRC32 computed over `[string_table ++ entries]` (the concatenation of the
footer's string table bytes followed by its entry bytes). The CRC32 is verified
on cold start before the footer is trusted to seed the index.

### Trust and Fallback Posture

- `SDX3` footers whose CRC32 verifies are trusted for fast-path index
  reconstruction.
- Pre-0.8.3 `SDX2` footers are no longer trusted at all. They are treated as a
  stale optimization artifact and ignored.
- When a footer is absent, an old `SDX2` footer, or an `SDX3` footer whose
  CRC32 does not verify, reopen falls back to the CRC-verified frame-scan
  rebuild. The events themselves are the source of truth, so no data is lost.

### Upgrade Consequence

The first reopen of a store written before 0.8.3 is slower because every sealed
segment falls back to the frame-scan rebuild once. That cost is one-time per
sealed segment: the next seal/rotation writes an `SDX3` footer, and subsequent
reopens take the verified fast path again. This is the ADR-0009 contract in
practice: forward-read compatibility plus rebuild-on-distrust, not a silent
trust of an older artifact.

## Consequences

- A truncated, torn, or rotted SIDX footer can no longer seed a wrong index; it
  fails the CRC32 check and degrades to the authoritative frame scan.
- Operators upgrading from <= 0.8.2 should expect a one-time slower first reopen
  per sealed segment and otherwise no migration step.
- Downgrade still follows ADR-0009: reverting binaries means purging/rebuilding
  cold-start artifacts unless a specific downgrade path is proven; an older
  binary will simply ignore `SDX3` footers and frame-scan.

## References

- `100_ADR_0009_POSITION_HINTS_AND_ARTIFACT_UPGRADES.md`
- `100_ADR_0033_0_8_3_HARDENING_POSTURE.md`
- `CHANGELOG.md`
