## What & why

<!--
What does this change do, and why? Link the issue/RFC if this touches the
public API, architecture, dependencies, or the invariant lattice — those are
RFC-first (see CONTRIBUTING.md → Project Direction).
-->

## Doctrine checklist

- [ ] No domain nouns added to the public API (batpak is substrate, not policy)
- [ ] Free functions / thin `Store` methods — no new manager object, trait, or `dyn` hierarchy
- [ ] Behavior changes ship a **falsifying** test (one that fails if the behavior regresses, not a happy-path smoke)
- [ ] No `#[allow(...)]` added to silence a lint; no `unwrap!`/`panic!`/`todo!`/`dbg!` in production code
- [ ] Oversized files split, not added to an allowlist
- [ ] Docs / examples / traceability updated if the public surface or behavior changed
- [ ] `just verify` run locally before the push that matters

## If AI-assisted

- [ ] I understand and can defend every line of this change; it is not a vibe-dump
