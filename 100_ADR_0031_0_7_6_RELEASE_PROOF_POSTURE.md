# ADR-0031: 0.7.6 Release Proof Posture

## Status

Accepted for the 0.7.6 correction cut.

## Context

ADR-0026 defines the pre-1.0 public-surface correction strategy for `batpak`.
ADR-0028 and ADR-0029 extend that correction from the core substrate into the
sync runtime and network boundary. The 0.7.6 cut is therefore not a
single-crate packaging exercise. It is the first release where the public
substrate family is treated as one train:

```text
bp records. sb runs. nb exposes.
```

The release must prove that shape without burning local machine budget on
always-on expensive gates. Some proof artifacts are committed metadata and
fixtures; some gates are explicit release actions.

## Decision

The 0.7.6 release train ships every publishable batpak-family crate:

- `batpak`
- `batpak-macros-support`
- `batpak-macros`
- `batpak-bench-support`
- `syncbat-macros`
- `syncbat`
- `netbat`

There are no workspace-private holdbacks for `syncbat`, `netbat`, or
`syncbat-macros` in this cut. Their manifests, crate-local READMEs, internal
path-dependency pins, public API baselines, and package contents belong to the
same release proof as `batpak`.

The release proof posture is:

1. **Version pins are checked in.** Internal path dependencies use the same
   release version as the package they reference.
2. **Golden fixtures are checked in.** Syncbat and netbat wire shapes use
   core-style committed `.hex` files instead of inline string pins.
3. **Golden fixtures are traceability artifacts.** The fixture paths are named
   in `bpk-lib/traceability/artifacts.yaml` and linked to the runtime and
   boundary invariants they defend.
4. **Mutation lanes are checked in.** Syncbat runtime dispatch, syncbat durable
   register catalog, and netbat boundary protocol seams have named mutation
   lane metadata.
5. **Mutation execution is machine-budgeted.** Lane definitions are release
   source; executing mutation runs is a release gate, not a background loop.
6. **Property fuzzing follows the repo harness.** The release uses the
   existing proptest posture and committed regression seeds. It does not
   introduce a second fuzz substrate for receipt hashing in this cut.
7. **Streaming is specified before implementation.** ADR-0030 owns the
   `NETBAT/2 STREAM` contract shape, while `NETBAT/1` remains request/response
   until the batpak-side stream item vocabulary is present and tested.

## Non-Negotiables

- Do not publish a partial family where `batpak` ships but `syncbat` or
  `netbat` remains workspace-only.
- Do not encode wire stability only as inline test literals.
- Do not run expensive proof gates by default in agent edit loops.
- Do not treat unchecked mutation lanes as a release substitute for committed
  tests, public API baselines, and golden fixtures.
- Do not move `NETBAT/1` from request/response into accidental streaming.

## Release Gate Shape

The release-prep branch owns these proof classes:

| Proof class | Checked-in source | Release action |
| --- | --- | --- |
| Version alignment | `Cargo.toml`, `Cargo.lock` | `cargo xtask check-version-pins` |
| Public API | `traceability/public_api/*.txt` | `cargo xtask public-api --strict --check-baseline` |
| Wire stability | `.hex` golden fixtures | owning crate tests |
| Traceability | `traceability/*.yaml` | `cargo xtask structural` and `cargo xtask evidence-audit` |
| Architecture IR | workspace manifests and `traceability/*.yaml` | `cargo xtask architecture-ir` |
| Mutation scope | xtask lane metadata | explicit mutation lane run under thermal headroom |
| Package contents | manifests and crate READMEs | `cargo package --list -p ...` |
| Release proof bundle | runbook and xtask release manifest | `cargo xtask release-manifest --strict` |

Agent edit loops may update the checked-in source side of this table without
running the release-action side. The final release-prep gate executes the
actions once from a clean branch.

## Consequences

- `syncbat` and `netbat` are judged as release surfaces, not examples attached
  to `batpak`.
- The release has a static proof trail even before expensive gates are run.
- Expensive gates become named, intentional release actions instead of
  incremental laptop heat.
- The TypeScript/client-facing work can depend on a coherent substrate family:
  core records, syncbat runs, netbat exposes.

## References

- `030_RELEASE_RUNBOOK.md`
- `040_TESTING_DOCTRINE.md`
- `041_TESTING_LEDGER.md`
- `100_ADR_0019_CANONICAL_ENCODING_CONTRACT.md`
- `100_ADR_0026_PRE_1_0_PUBLIC_SURFACE_STRATEGY.md`
- `100_ADR_0028_SYNCBAT_RUNTIME_CONTRACT.md`
- `100_ADR_0029_NETBAT_BOUNDARY_CONTRACT.md`
- `100_ADR_0030_NETBAT_STREAMING_CONTRACT.md`
- `CHANGELOG.md`
- `bpk-lib/traceability/artifacts.yaml`
- `bpk-lib/traceability/requirements.yaml`
