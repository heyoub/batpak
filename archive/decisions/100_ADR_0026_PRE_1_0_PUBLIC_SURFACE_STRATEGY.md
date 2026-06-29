# ADR-0026: Pre-1.0 Public Surface Strategy

## Status

Accepted for the `0.7.6` correction cut.

## Context

`batpak` is still pre-1.0 and has no known downstream users that require a
strict patch-compatible migration promise. Several shipped public names are
useful proof of the implementation history but are not the shape the project
should ask future users to depend on.

Examples include:

- internal index helpers that became public through broad re-exports
- public mutable configuration fields that can bypass validation
- low-level physical witnesses exposed through default imports
- duplicate public words for the same operational concept
- compatibility aliases that keep old names alive after better names exist

Deferring every correction to a later minor line would make the version number
cleaner but would also preserve accidental public contracts longer than the
project needs.
For this repository, maintainability and an honest pre-1.0 shape matter more
than preserving a patch-compatible fiction for unused APIs.

## Decision

`0.7.6` is a pre-1.0 correction release.

The release may include public API breaks when all of the following are true:

1. the old surface advertises the wrong abstraction or leaks internal machinery
2. the replacement shape is documented before the code change lands
3. the changelog includes a migration note for the break
4. the tree is green after the atomic change

The release also includes the Canal correction from ADR-0011. Canal is an owed
delivery abstraction, not a documentation cleanup. It should land as a narrow
composition layer over existing cursor/subscription primitives, with the
cursor-guaranteed path remaining the typed-reactor default.

## Release Discipline

The correction cut is ordered:

1. vocabulary and strategy docs
2. advisory public API measurement
3. Canal delivery abstraction
4. public surface cleanup
5. ADR-0009 historical artifact fixtures
6. post-cleanup public API baseline

Public API measurement starts advisory so it can record the intentional
cleanup. It becomes release-blocking only after the cleaned surface is captured
as the new baseline.

Each cleanup commit should be concept-atomic. For example, making
`StoreConfig` fields private and updating the tests that used those fields
belong in the same commit. Intermediate states that knowingly fail tests or
structural checks are not acceptable.

## Consequences

- `0.7.6` may be larger than a normal patch release.
- Migration notes are required for every intentional public break.
- The post-`0.7.6` surface becomes the baseline for stricter semver/public API
  checks.
- The project does not promise strict patch compatibility until the corrected
  pre-1.0 public shape is captured.

## Non-Goals

- This ADR does not make `batpak` a userland product surface. Core remains the
  substrate.
- This ADR does not add an external context-profile spec's semantics to `batpak`.
- This ADR does not include deferred durability-waiter redesign or performance
  threshold calibration.
