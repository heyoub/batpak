## Summary

## Verification
- [ ] `just verify`, `just ci-fast`, or scoped verification passes locally; reason documented below if not
- [ ] `just inspect` passes, or change does not touch structural surfaces
- [ ] `just traceability` passes, or change does not touch traced surfaces
- [ ] Tests were added, strengthened, or verified as unnecessary

## Docs / Traceability
- [ ] Public API changes update README, factory docs, or examples as appropriate
- [ ] New public items, invariants, flows, or artifacts update `bpk-lib/traceability/`
- [ ] Decision history stays in `archive/decisions/`; live root docs stay factory-current
- [ ] CHANGELOG `[Unreleased]` is updated when user-visible behavior changes

## CI / branch protection
- [ ] Required checks updated if job names changed (`ci-fast-linux`, `verify-linux`, `ci-windows`, `mutants-smoke`, …)

## Risks
- Compatibility:
- Rollback:
- Follow-up:
