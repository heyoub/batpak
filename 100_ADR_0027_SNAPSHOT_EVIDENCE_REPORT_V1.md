# ADR-0027: Snapshot Evidence Report v1

## Status
Accepted.

## Context
R4.04 identified a proof-object gap in the snapshot lifecycle. `Store::snapshot`
already drains the writer, opens a private visibility fence, copies segment and
visibility artifacts, and cancels the fence, but returned only `()`. Operators
could inspect the destination directory after the fact, but batpak did not bind
the snapshot boundary, copied structural inputs, and fence cleanup into one
deterministic report.

## Decision
batpak adds `Store::snapshot_with_evidence` and
`bpk-lib/crates/core/src/store/snapshot_report.rs`. `Store::snapshot` remains as
a deprecated wrapper for one minor cut and drops the report.

### 1) v1 report body

`SnapshotReportBody` includes:

- `schema_version`
- `snapshot_id`
- `fence_token`
- `source_watermark`
- `copied_segment_ids_sorted`
- `copied_visibility_ranges_present`
- `copied_pending_compaction_marker_present`
- `destination_path_digest`
- deterministic sorted `findings`

### 2) honesty contract

v1 reports strongest truthful state from existing substrate facts:

- the private visibility-fence token that covered the copy
- the source segment watermark after writer drain
- copied segment ids sorted by numeric segment id
- copied optional sidecar presence for visibility ranges and pending compaction
- destination path bytes by digest, never raw path text
- `CopyByteHashUnavailable` when callers need per-file hashes that v1 does not
  record

### 3) deterministic identity

`SnapshotEvidenceReport` follows ADR-0019 family identity:

- deterministic body
- canonical `body_hash` through the active batpak hash backend
- metadata outside deterministic identity

The fence token participates in identity because it is the proof-bearing
snapshot boundary. Equal fresh-store scenarios with equal destination path bytes
produce equal body hashes; repeated snapshots against a reused destination may
legitimately differ because destination cleanup findings are evidence.

## Non-goals

This slice does not add:

- per-file byte hash tables
- snapshot artifact envelope signing
- cross-process snapshot scheduler semantics
- restore-proof expansion beyond existing backup/restore report bodies
- a policy statement that a snapshot is sufficient for a regulated backup

## Consequences

- snapshot mutation evidence is first-class instead of tracing-only
- `Store::compact` and `Store::snapshot_with_evidence` now both default toward
  returning structural evidence for lifecycle mutations
- snapshot reports can be persisted or wrapped in a generic canonical artifact
  envelope by callers that need attestation
- raw destination paths remain out of canonical bodies
- a reused destination is observable through `DestinationCleared`

## References

- `100_ADR_0019_CANONICAL_ENCODING_CONTRACT.md`
- `100_ADR_0024_PROJECTION_RUN_EVIDENCE_REPORT_V1.md`
- `080_EVIDENCE_REPORTS.md`
- `bpk-lib/crates/core/src/store/lifecycle.rs`
- `bpk-lib/crates/core/src/store/snapshot_report.rs`
