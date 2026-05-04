## Summary

## Verification
- [ ] `cargo xtask ci` passes locally, or reason documented below
- [ ] `cargo xtask traceability` passes, or change does not touch traced surfaces
- [ ] `cargo xtask structural` passes, or change does not touch structural surfaces
- [ ] Tests were added, strengthened, or verified as unnecessary

## Docs / Traceability
- [ ] Public API changes update README, GUIDE, REFERENCE, or examples as appropriate
- [ ] New public items, invariants, flows, or artifacts update `traceability/`
- [ ] ADRs are added or referenced when behavior or architecture changes
- [ ] CHANGELOG `[Unreleased]` is updated when user-visible behavior changes

## Risks
- Compatibility:
- Rollback:
- Follow-up:
