# ADR-0009 Cold-Start Artifact Fixtures

These fixture stores pin historical fast-start artifact readers through the
real `Store::open` path. They are copied to a temporary directory before each
test so the checked-in bytes stay immutable.

## Provenance

- `checkpoint-v5/`
  - generated from commit `5283284` (`f6dbe15^`)
  - writer artifact versions: checkpoint `v5`, mmap `v4`
  - config: checkpoint enabled, mmap disabled
  - expected current reopen path: `OpenIndexPath::Checkpoint`
- `mmap-v4/`
  - generated from commit `5283284` (`f6dbe15^`)
  - writer artifact versions: checkpoint `v5`, mmap `v4`
  - config: checkpoint enabled, mmap enabled
  - expected current reopen path: `OpenIndexPath::Mmap`

Both fixtures contain one app event with a non-root `AppendPositionHint` and an
`app.audit` receipt extension. Current readers must preserve lane/depth and
hydrate receipt extensions from the authoritative `.fbat` frame because these
older optimization artifacts do not directly carry receipt-extension maps.

Checkpoint `v2`/`v3` and mmap `v1`/`v2`/`v3` compatibility remains covered by
focused synthetic decoder tests in `src/store/cold_start/{checkpoint,mmap}.rs`.
The local git history available to this repository does not contain the older
writer commits needed to produce real checked-in store directories for those
versions.

